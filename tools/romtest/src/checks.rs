//! Check evaluation (acceptance/README.md, "Checks").

use std::io::Cursor;
use std::path::{Path, PathBuf};

use gbc_core::{Frame, Registers, SCREEN_HEIGHT, SCREEN_WIDTH};
use serde::Serialize;

use crate::manifest::{Check, CheckKind, expand_template};

const W: usize = SCREEN_WIDTH;
const H: usize = SCREEN_HEIGHT;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CheckStatus {
    Pass,
    Fail,
    Unsupported,
    NoReference,
    /// Not evaluated because the ROM never ran (load error or panic).
    Skipped,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckResult {
    pub kind: &'static str,
    /// The check as written in the manifest.
    pub spec: String,
    pub status: CheckStatus,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Screenshot checks: the expanded reference path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Screenshot checks: pixels that differ from the reference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mismatched_pixels: Option<u64>,
    /// Screenshot failures: the actual frame and the diff image, relative
    /// to `target/acceptance/`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual_png: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_png: Option<String>,
}

impl CheckResult {
    fn new(check: &Check, status: CheckStatus, detail: String) -> Self {
        Self {
            kind: check.kind().name(),
            spec: check.to_string(),
            status,
            detail,
            reference: None,
            mismatched_pixels: None,
            actual_png: None,
            diff_png: None,
        }
    }
}

/// The result for a check whose ROM never reached an exit (load error or
/// panic, so the ROM's status is `crash`): `no-reference` for a screenshot
/// whose PNG is missing (for information), otherwise `skipped`. `Err` is a
/// runner error (a bad expanded path).
pub fn not_run(check: &Check, roms_dir: &Path, rom: &str) -> Result<CheckResult, String> {
    if let Check::Screenshot(template) = check {
        let (dir, stem) = dir_and_stem(rom);
        let reference = expand_template(template, dir, stem)?;
        if !roms_dir.join(&reference).exists() {
            let mut r = CheckResult::new(
                check,
                CheckStatus::NoReference,
                format!("{reference} not found"),
            );
            r.reference = Some(reference);
            return Ok(r);
        }
    }
    Ok(CheckResult::new(check, CheckStatus::Skipped, String::new()))
}

/// An 8-bit RGB image, row-major.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgb {
    pub width: usize,
    pub height: usize,
    pub data: Vec<[u8; 3]>,
}

/// RGB555 to RGB888 with `(X << 3) | (X >> 2)` per channel
/// (acceptance/README.md, "Colors used for screenshot comparison"). Bits
/// 0-4 are red, 5-9 green, 10-14 blue (the CGB palette layout).
pub fn rgb555_to_888(p: u16) -> [u8; 3] {
    let x = |s: u16| {
        let v = ((p >> s) & 0x1F) as u8;
        (v << 3) | (v >> 2)
    };
    [x(0), x(5), x(10)]
}

pub fn frame_to_rgb(frame: &Frame) -> Rgb {
    Rgb {
        width: W,
        height: H,
        data: frame.iter().map(|&p| rgb555_to_888(p)).collect(),
    }
}

/// Decodes a PNG of any color type and bit depth to 8-bit RGB (alpha is
/// dropped).
pub fn decode_png(bytes: &[u8]) -> Result<Rgb, String> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let size = reader
        .output_buffer_size()
        .ok_or("image too large to decode")?;
    let mut buf = vec![0; size];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (width, height) = (info.width as usize, info.height as usize);
    let channels = match info.color_type {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        png::ColorType::Indexed => return Err("indexed PNG was not expanded".into()),
    };
    if info.bit_depth != png::BitDepth::Eight {
        return Err(format!("unexpected bit depth {:?}", info.bit_depth));
    }
    let mut data = Vec::with_capacity(width * height);
    for row in buf.chunks(info.line_size).take(height) {
        for px in row[..width * channels].chunks_exact(channels) {
            data.push(if channels <= 2 {
                [px[0]; 3]
            } else {
                [px[0], px[1], px[2]]
            });
        }
    }
    Ok(Rgb {
        width,
        height,
        data,
    })
}

pub fn encode_png(img: &Rgb) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width as u32, img.height as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(img.data.as_flattened())
        .map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

/// Counts differing pixels. Mismatched sizes count every pixel of the
/// larger image.
pub fn count_mismatches(actual: &Rgb, expected: &Rgb) -> u64 {
    if (actual.width, actual.height) != (expected.width, expected.height) {
        return (actual.data.len().max(expected.data.len())) as u64;
    }
    actual
        .data
        .iter()
        .zip(&expected.data)
        .filter(|(a, e)| a != e)
        .count() as u64
}

/// Diff image: matching pixels as a dimmed gray copy of the reference,
/// mismatches in bright red. Same size as `actual`.
pub fn diff_image(actual: &Rgb, expected: &Rgb) -> Rgb {
    let same_size = (actual.width, actual.height) == (expected.width, expected.height);
    let data = actual
        .data
        .iter()
        .enumerate()
        .map(|(i, a)| match expected.data.get(i).filter(|_| same_size) {
            Some(e) if e == a => {
                let y = (u16::from(e[0]) * 3 + u16::from(e[1]) * 6 + u16::from(e[2])) / 10;
                let v = (32 + y * 3 / 8) as u8;
                [v, v, v]
            }
            _ => [255, 0, 0],
        })
        .collect();
    Rgb {
        width: actual.width,
        height: actual.height,
        data,
    }
}

/// See [`Ctx::verify`].
pub type VerifyFn<'a> = dyn Fn(&str, &[u8]) -> Result<(), String> + 'a;

/// Everything a check needs about one ROM run.
pub struct Ctx<'a> {
    pub roms_dir: &'a Path,
    /// The ROM path relative to `roms_dir`.
    pub rom: &'a str,
    pub serial: &'a [u8],
    pub registers: Option<Registers>,
    pub final_frame: &'a Frame,
    /// Where to write failure images: `(actual, diff)` paths for this
    /// check, relative to the output root, or `None` to skip writing.
    pub failure_pngs: &'a dyn Fn(usize) -> Option<(PathBuf, String, String)>,
    /// Verifies bytes loaded from a path (relative to `roms_dir`) against
    /// the integrity list: `Err` (a runner error) if the path isn't listed
    /// or the bytes don't match its SHA-256. A check may only use loaded
    /// bytes that passed this.
    pub verify: &'a VerifyFn<'a>,
}

/// The ROM's directory and file stem, for `{dir}` and `{stem}`.
pub fn dir_and_stem(rom: &str) -> (&str, &str) {
    let (dir, file) = rom.rsplit_once('/').unwrap_or(("", rom));
    let stem = file.rsplit_once('.').map_or(file, |(s, _)| s);
    (dir, stem)
}

/// Evaluates one check. `Err` is a runner error (I/O, unreadable PNG).
pub fn evaluate(check: &Check, index: usize, ctx: &Ctx<'_>) -> Result<CheckResult, String> {
    Ok(match check {
        Check::Serial(text) => {
            let found = contains(ctx.serial, text.as_bytes());
            let detail = if found {
                String::new()
            } else {
                format!("serial output does not contain {text:?}")
            };
            CheckResult::new(check, status(found), detail)
        }
        Check::Fibonacci => {
            let ok = ctx.registers.is_some_and(is_fibonacci);
            let detail = match ctx.registers {
                Some(r) if !ok => format!(
                    "B={} C={} D={} E={} H={} L={} (want 3 5 8 13 21 34)",
                    r.b, r.c, r.d, r.e, r.h, r.l
                ),
                None => "no registers".into(),
                _ => String::new(),
            };
            CheckResult::new(check, status(ok), detail)
        }
        // Not implemented until M5 (docs/plan/stage-1.md).
        Check::Gambatte => CheckResult::new(
            check,
            CheckStatus::Unsupported,
            "gambatte check not implemented yet".into(),
        ),
        Check::Screenshot(template) => screenshot(check, template, index, ctx)?,
    })
}

fn status(ok: bool) -> CheckStatus {
    if ok {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    }
}

/// Mooneye's pass signature (acceptance/README.md: `fibonacci`).
pub fn is_fibonacci(r: Registers) -> bool {
    (r.b, r.c, r.d, r.e, r.h, r.l) == (3, 5, 8, 13, 21, 34)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    needle.is_empty() || haystack.windows(needle.len()).any(|w| w == needle)
}

fn screenshot(
    check: &Check,
    template: &str,
    index: usize,
    ctx: &Ctx<'_>,
) -> Result<CheckResult, String> {
    let (dir, stem) = dir_and_stem(ctx.rom);
    let reference = expand_template(template, dir, stem)?;
    let path = ctx.roms_dir.join(&reference);
    let mut result = match std::fs::read(&path) {
        // A listed PNG can't be missing here (the integrity check runs
        // first), so this is a reference that upstream doesn't provide.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckResult::new(
            check,
            CheckStatus::NoReference,
            format!("{reference} not found"),
        ),
        Err(e) => return Err(format!("reading {}: {e}", path.display())),
        Ok(bytes) => {
            (ctx.verify)(&reference, &bytes).map_err(|e| format!("reference {e}"))?;
            let expected =
                decode_png(&bytes).map_err(|e| format!("decoding {}: {e}", path.display()))?;
            let actual = frame_to_rgb(ctx.final_frame);
            let mismatched = count_mismatches(&actual, &expected);
            let mut r = if mismatched == 0 {
                CheckResult::new(check, CheckStatus::Pass, String::new())
            } else {
                let mut detail = format!("{mismatched} pixels differ");
                if (expected.width, expected.height) != (W, H) {
                    detail = format!(
                        "reference is {}x{}, not {W}x{H}",
                        expected.width, expected.height
                    );
                }
                let mut r = CheckResult::new(check, CheckStatus::Fail, detail);
                if let Some((root, actual_rel, diff_rel)) = (ctx.failure_pngs)(index) {
                    write_png(&root.join(&actual_rel), &actual)?;
                    write_png(&root.join(&diff_rel), &diff_image(&actual, &expected))?;
                    r.actual_png = Some(actual_rel);
                    r.diff_png = Some(diff_rel);
                }
                r
            };
            r.mismatched_pixels = Some(mismatched);
            r
        }
    };
    result.reference = Some(reference);
    Ok(result)
}

fn write_png(path: &Path, img: &Rgb) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
    }
    let bytes = encode_png(img)?;
    std::fs::write(path, bytes).map_err(|e| format!("writing {}: {e}", path.display()))
}

/// The check kinds a selection keeps (`--only-check`).
pub fn selected(checks: &[Check], only: Option<CheckKind>) -> Vec<(usize, &Check)> {
    checks
        .iter()
        .enumerate()
        .filter(|(_, c)| only.is_none_or(|k| c.kind() == k))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::new_frame;

    fn no_pngs(_: usize) -> Option<(PathBuf, String, String)> {
        None
    }

    fn all_listed(_: &str, _: &[u8]) -> Result<(), String> {
        Ok(())
    }

    fn ctx<'a>(roms_dir: &'a Path, rom: &'a str, serial: &'a [u8], frame: &'a Frame) -> Ctx<'a> {
        Ctx {
            roms_dir,
            rom,
            serial,
            registers: Some(Registers::default()),
            final_frame: frame,
            failure_pngs: &no_pngs,
            verify: &all_listed,
        }
    }

    #[test]
    fn rgb555_expansion() {
        assert_eq!(rgb555_to_888(0x7FFF), [255, 255, 255]);
        assert_eq!(rgb555_to_888(0), [0, 0, 0]);
        assert_eq!(rgb555_to_888(0x001F), [255, 0, 0]);
        assert_eq!(rgb555_to_888(0x03E0), [0, 255, 0]);
        assert_eq!(rgb555_to_888(0x7C00), [0, 0, 255]);
        // X = 0b01100 -> 0b01100_011.
        assert_eq!(rgb555_to_888(12), [0x63, 0, 0]);
        for p in 0..0x8000u16 {
            assert_eq!(rgb555_to_888(p), gbc_core::rgb555_to_888(p));
        }
    }

    #[test]
    fn serial_check() {
        let dir = Path::new(".");
        let f = new_frame();
        let c = Check::Serial("Passed all tests".into());
        let r = evaluate(
            &c,
            0,
            &ctx(dir, "a.gb", b"cpu_instrs\n\nPassed all tests\n", &f),
        )
        .unwrap();
        assert_eq!(r.status, CheckStatus::Pass);
        let r = evaluate(&c, 0, &ctx(dir, "a.gb", b"Passed all test", &f)).unwrap();
        assert_eq!(r.status, CheckStatus::Fail);
        let r = evaluate(&c, 0, &ctx(dir, "a.gb", b"", &f)).unwrap();
        assert_eq!(r.status, CheckStatus::Fail);
    }

    #[test]
    fn fibonacci_check() {
        let dir = Path::new(".");
        let f = new_frame();
        let mut cx = ctx(dir, "a.gb", b"", &f);
        let good = Registers {
            b: 3,
            c: 5,
            d: 8,
            e: 13,
            h: 21,
            l: 34,
            ..Registers::default()
        };
        cx.registers = Some(good);
        assert_eq!(
            evaluate(&Check::Fibonacci, 0, &cx).unwrap().status,
            CheckStatus::Pass
        );
        cx.registers = Some(Registers { l: 35, ..good });
        let r = evaluate(&Check::Fibonacci, 0, &cx).unwrap();
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("L=35"));
        cx.registers = None;
        assert_eq!(
            evaluate(&Check::Fibonacci, 0, &cx).unwrap().status,
            CheckStatus::Fail
        );
    }

    #[test]
    fn gambatte_is_unsupported() {
        let f = new_frame();
        let r = evaluate(&Check::Gambatte, 0, &ctx(Path::new("."), "a.gbc", b"", &f)).unwrap();
        assert_eq!(r.status, CheckStatus::Unsupported);
    }

    fn write_ref(dir: &Path, rel: &str, img: &Rgb) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, encode_png(img).unwrap()).unwrap();
    }

    #[test]
    fn screenshot_check() {
        let dir = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();
        let mut frame = new_frame();
        frame[0] = 0x001F; // red pixel top-left
        let expected = frame_to_rgb(&frame);
        write_ref(dir.path(), "suite/sub/t-cgb.png", &expected);
        let c = Check::Screenshot("{dir}/{stem}-cgb.png".into());

        let r = evaluate(&c, 0, &ctx(dir.path(), "suite/sub/t.gb", b"", &frame)).unwrap();
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(r.mismatched_pixels, Some(0));
        assert_eq!(r.reference.as_deref(), Some("suite/sub/t-cgb.png"));

        // Two pixels off: fail, count, and write the failure images.
        let mut actual = frame.clone();
        actual[0] = 0x7FFF;
        actual[160 * 144 - 1] = 0;
        let root = out.path().to_path_buf();
        let pngs = move |i: usize| {
            Some((
                root.clone(),
                format!("x.{i}.actual.png"),
                format!("x.{i}.diff.png"),
            ))
        };
        let mut cx = ctx(dir.path(), "suite/sub/t.gb", b"", &actual);
        cx.failure_pngs = &pngs;
        let r = evaluate(&c, 3, &cx).unwrap();
        assert_eq!(r.status, CheckStatus::Fail);
        assert_eq!(r.mismatched_pixels, Some(2));
        let written =
            decode_png(&std::fs::read(out.path().join("x.3.actual.png")).unwrap()).unwrap();
        assert_eq!(written, frame_to_rgb(&actual));
        let diff = decode_png(&std::fs::read(out.path().join("x.3.diff.png")).unwrap()).unwrap();
        assert_eq!(diff.data[0], [255, 0, 0]);
        assert_eq!(diff.data[160 * 144 - 1], [255, 0, 0]);
        assert_eq!(diff.data.iter().filter(|&&p| p == [255, 0, 0]).count(), 2);

        // Missing reference.
        let r = evaluate(&c, 0, &ctx(dir.path(), "suite/sub/u.gb", b"", &frame)).unwrap();
        assert_eq!(r.status, CheckStatus::NoReference);

        // Wrong-size reference fails.
        write_ref(
            dir.path(),
            "small.png",
            &Rgb {
                width: 2,
                height: 2,
                data: vec![[255; 3]; 4],
            },
        );
        let c2 = Check::Screenshot("small.png".into());
        let r = evaluate(&c2, 0, &ctx(dir.path(), "suite/sub/t.gb", b"", &frame)).unwrap();
        assert_eq!(r.status, CheckStatus::Fail);
        assert!(r.detail.contains("2x2"));

        // A corrupt reference is a runner error.
        std::fs::write(dir.path().join("bad.png"), b"not a png").unwrap();
        let c3 = Check::Screenshot("bad.png".into());
        assert!(evaluate(&c3, 0, &ctx(dir.path(), "t.gb", b"", &frame)).is_err());

        // So is a reference that isn't in the integrity list.
        let mut cx = ctx(dir.path(), "suite/sub/t.gb", b"", &frame);
        let only_small = |p: &str, _: &[u8]| {
            if p == "small.png" {
                Ok(())
            } else {
                Err(format!("{p} is not in the test-roms integrity list"))
            }
        };
        cx.verify = &only_small;
        let e = evaluate(&c, 0, &cx).unwrap_err();
        assert!(e.contains("not in the test-roms integrity list"), "{e}");
    }

    #[test]
    fn screenshot_for_a_rom_at_the_root() {
        // `{dir}` is empty for a ROM directly in test-roms/: the reference
        // is `t.png`, not the absolute `/t.png`.
        let dir = tempfile::tempdir().unwrap();
        let frame = new_frame();
        write_ref(dir.path(), "t.png", &frame_to_rgb(&frame));
        let c = Check::Screenshot("{dir}/{stem}.png".into());
        let r = evaluate(&c, 0, &ctx(dir.path(), "t.gb", b"", &frame)).unwrap();
        assert_eq!(r.status, CheckStatus::Pass);
        assert_eq!(r.reference.as_deref(), Some("t.png"));
        let r = not_run(&c, dir.path(), "u.gb").unwrap();
        assert_eq!(r.reference.as_deref(), Some("u.png"));
        assert_eq!(r.status, CheckStatus::NoReference);
        // A path that escapes test-roms/ is a runner error.
        let c = Check::Screenshot("{dir}/../{stem}.png".into());
        assert!(evaluate(&c, 0, &ctx(dir.path(), "t.gb", b"", &frame)).is_err());
        assert!(not_run(&c, dir.path(), "a/t.gb").is_err());
    }

    #[test]
    fn decodes_gray_palette_and_rgba_pngs() {
        let encode = |color, depth, w: u32, data: &[u8], palette: Option<Vec<u8>>| {
            let mut out = Vec::new();
            let mut enc = png::Encoder::new(&mut out, w, 1);
            enc.set_color(color);
            enc.set_depth(depth);
            if let Some(p) = palette {
                enc.set_palette(p);
            }
            let mut wr = enc.write_header().unwrap();
            wr.write_image_data(data).unwrap();
            wr.finish().unwrap();
            out
        };
        let g = encode(
            png::ColorType::Grayscale,
            png::BitDepth::Eight,
            2,
            &[0, 200],
            None,
        );
        assert_eq!(decode_png(&g).unwrap().data, [[0; 3], [200; 3]]);
        let rgba = encode(
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            1,
            &[1, 2, 3, 0],
            None,
        );
        assert_eq!(decode_png(&rgba).unwrap().data, [[1, 2, 3]]);
        let pal = encode(
            png::ColorType::Indexed,
            png::BitDepth::Two,
            3,
            &[0b00_01_10_00],
            Some(vec![0, 0, 0, 0x7B, 0xFF, 0x31, 0, 0x63, 0xC6]),
        );
        assert_eq!(
            decode_png(&pal).unwrap().data,
            [[0, 0, 0], [0x7B, 0xFF, 0x31], [0, 0x63, 0xC6]]
        );
    }

    /// Encodes a `w`x`h` PNG; `setup` adjusts the encoder.
    fn encode_with(
        w: u32,
        h: u32,
        color: png::ColorType,
        depth: png::BitDepth,
        data: &[u8],
        setup: impl FnOnce(&mut png::Encoder<'_, &mut Vec<u8>>),
    ) -> Vec<u8> {
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(color);
        enc.set_depth(depth);
        setup(&mut enc);
        let mut wr = enc.write_header().unwrap();
        wr.write_image_data(data).unwrap();
        wr.finish().unwrap();
        out
    }

    #[test]
    fn decodes_16_bit_pngs() {
        // 16-bit samples are reduced to their high byte.
        let rgb16 = encode_with(
            2,
            1,
            png::ColorType::Rgb,
            png::BitDepth::Sixteen,
            &[
                0x12, 0x34, 0xAB, 0xCD, 0xFF, 0xFF, 0, 0, 0x80, 0x7F, 0x01, 0xFF,
            ],
            |_| {},
        );
        assert_eq!(
            decode_png(&rgb16).unwrap().data,
            [[0x12, 0xAB, 0xFF], [0, 0x80, 0x01]]
        );
        let gray16 = encode_with(
            1,
            2,
            png::ColorType::Grayscale,
            png::BitDepth::Sixteen,
            &[0x7B, 0x00, 0xFF, 0xFF],
            |_| {},
        );
        assert_eq!(decode_png(&gray16).unwrap().data, [[0x7B; 3], [0xFF; 3]]);
    }

    #[test]
    fn decodes_gray_alpha_pngs() {
        let ga = encode_with(
            2,
            1,
            png::ColorType::GrayscaleAlpha,
            png::BitDepth::Eight,
            &[0x63, 0x00, 0xC6, 0xFF],
            |_| {},
        );
        let img = decode_png(&ga).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.data, [[0x63; 3], [0xC6; 3]]);
    }

    #[test]
    fn decodes_indexed_pngs_with_transparency() {
        // tRNS makes EXPAND produce RGBA; alpha is dropped, colors kept.
        let pal = encode_with(
            2,
            2,
            png::ColorType::Indexed,
            png::BitDepth::Eight,
            &[0, 1, 2, 0],
            |e| {
                e.set_palette(vec![0xFF, 0xFF, 0xFF, 0x94, 0x39, 0x39, 0, 0, 0]);
                e.set_trns(vec![0x00, 0x80]);
            },
        );
        assert_eq!(
            decode_png(&pal).unwrap().data,
            [[0xFF; 3], [0x94, 0x39, 0x39], [0; 3], [0xFF; 3]]
        );
    }

    /// zlib stream with stored (uncompressed) deflate blocks.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let chunks: Vec<&[u8]> = data.chunks(0xFFFF).collect();
        for (i, c) in chunks.iter().enumerate() {
            out.push(u8::from(i + 1 == chunks.len()));
            let len = c.len() as u16;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.extend_from_slice(c);
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &x in data {
            a = (a + u32::from(x)) % 65521;
            b = (b + a) % 65521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    #[test]
    fn decodes_interlaced_pngs() {
        // The png encoder can't interlace, so build the Adam7 passes by
        // hand (filter type 0 on every row). A distinct color per pixel
        // catches any row or column mix-up in de-interlacing.
        let pixel = |x: usize, y: usize| [x as u8, y as u8, ((x * 7 + y) % 251) as u8];
        const PASSES: [(usize, usize, usize, usize); 7] = [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ];
        let mut raw = Vec::new();
        for (x0, y0, dx, dy) in PASSES {
            for y in (y0..H).step_by(dy) {
                raw.push(0); // filter: none
                for x in (x0..W).step_by(dx) {
                    raw.extend_from_slice(&pixel(x, y));
                }
            }
        }
        let mut info = png::Info::with_size(W as u32, H as u32);
        info.color_type = png::ColorType::Rgb;
        info.bit_depth = png::BitDepth::Eight;
        info.interlaced = true;
        let mut out = Vec::new();
        let mut wr = png::Encoder::with_info(&mut out, info)
            .unwrap()
            .write_header()
            .unwrap();
        wr.write_chunk(png::chunk::IDAT, &zlib_stored(&raw))
            .unwrap();
        wr.finish().unwrap();

        let interlaced = png::Decoder::new(Cursor::new(&out[..]))
            .read_info()
            .unwrap()
            .info()
            .interlaced;
        assert!(interlaced, "the test image must actually be interlaced");
        let img = decode_png(&out).unwrap();
        assert_eq!((img.width, img.height), (W, H));
        for y in 0..H {
            for x in 0..W {
                assert_eq!(img.data[y * W + x], pixel(x, y), "({x}, {y})");
            }
        }
    }

    #[test]
    fn dir_and_stem_split() {
        assert_eq!(dir_and_stem("a/b/c.gb"), ("a/b", "c"));
        assert_eq!(dir_and_stem("c.d.gbc"), ("", "c.d"));
    }

    #[test]
    fn only_check_selection() {
        let checks = [Check::Serial("x".into()), Check::Screenshot("y".into())];
        assert_eq!(selected(&checks, None).len(), 2);
        let s = selected(&checks, Some(CheckKind::Screenshot));
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].0, 1);
        assert!(selected(&checks, Some(CheckKind::Fibonacci)).is_empty());
    }
}
