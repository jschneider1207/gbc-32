//! The `gambatte = true` check (acceptance/README.md, "Checks"; ADR 0018).
//!
//! The rules are those of Gambatte's `test/testrunner.cpp`
//! (pokemon-speedrunning/gambatte-core @ d819bad196) for a CGB run on
//! CGB-CPU-04 ("cgb04c"). That file is GPL: it was read to learn the rules
//! only. Nothing here is copied from it; the hex-digit glyphs below were
//! re-derived from what the test ROMs draw (ADR 0018).

use gbc_core::Frame;

use crate::checks::{Rgb, W};
use crate::machine::AudioCapture;

/// What a ROM's file name asks of the CGB run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expect {
    /// `_out<hex>`: these digits (0-15 each), drawn left to right from the
    /// top-left corner of the screen.
    Hex(Vec<u8>),
    /// `_outaudio0` (false: silence) / `_outaudio1` (true: sound).
    Audio(bool),
}

/// The result tag that applies to a CGB run, chosen the way testrunner
/// chooses it from the extension-stripped name:
/// - a name with `dmg08_cgb04c_out` uses that (one result for both);
/// - otherwise a name with `dmg08_out` has a CGB result only if it also
///   has `cgb04c_out`, and then uses `cgb04c_out`;
/// - otherwise a name with `_out` uses `_out`.
///
/// The value follows the first occurrence of the tag.
fn cgb_tag(stem: &str) -> Option<&'static str> {
    if stem.contains("dmg08_cgb04c_out") {
        Some("dmg08_cgb04c_out")
    } else if stem.contains("dmg08_out") {
        stem.contains("cgb04c_out").then_some("cgb04c_out")
    } else if stem.contains("_out") {
        Some("_out")
    } else {
        None
    }
}

/// Parses the `_out` result for the CGB run from a ROM's file stem (the
/// file name without its extension; directories are not part of the
/// naming convention). `Ok(None)`: the name has no CGB result. `Err`: the
/// name has a CGB tag that names no result (testrunner would compare zero
/// digits and pass; we refuse instead), or more digits than fit on a line.
pub fn parse_out(stem: &str) -> Result<Option<Expect>, String> {
    let Some(tag) = cgb_tag(stem) else {
        return Ok(None);
    };
    let at = stem.find(tag).expect("cgb_tag found it") + tag.len();
    let rest = &stem[at..];
    if rest.starts_with("audio0") {
        return Ok(Some(Expect::Audio(false)));
    }
    if rest.starts_with("audio1") {
        return Ok(Some(Expect::Audio(true)));
    }
    let digits: Vec<u8> = rest
        .chars()
        .map_while(|c| c.to_digit(16))
        .map(|d| d as u8)
        .collect();
    if digits.is_empty() {
        return Err(format!(
            "gambatte: `{tag}` in {stem:?} is followed by neither hex digits nor audio0/1"
        ));
    }
    if digits.len() * 8 > W {
        return Err(format!(
            "gambatte: {stem:?} expects {} digits; at most {} fit on screen",
            digits.len(),
            W / 8
        ));
    }
    Ok(Some(Expect::Hex(digits)))
}

/// The reference PNG that applies to a CGB run of `{dir}/{stem}.gb[c]`,
/// in testrunner's order: `{stem}_dmg08_cgb04c.png` (shared by DMG and
/// CGB) if it exists, else `{stem}_cgb04c.png`. Both are relative paths
/// in the same directory as the ROM; the caller checks which exists.
pub fn png_candidates(dir: &str, stem: &str) -> [String; 2] {
    let p = |suffix: &str| {
        if dir.is_empty() {
            format!("{stem}{suffix}")
        } else {
            format!("{dir}/{stem}{suffix}")
        }
    };
    [p("_dmg08_cgb04c.png"), p("_cgb04c.png")]
}

/// Gambatte's CGB color formula (the howto in `test-roms/gambatte/`) for
/// an RGB555 pixel (bits 0-4 red, 5-9 green, 10-14 blue):
/// R' = (13R + 2G + B) / 2, G' = (3G + B) * 2, B' = (3R + 2G + 11B) / 2,
/// with integer division (a right shift). Every channel's maximum is 248,
/// so nothing needs clamping.
pub fn gambatte_rgb(p: u16) -> [u8; 3] {
    let r = u32::from(p & 0x1F);
    let g = u32::from((p >> 5) & 0x1F);
    let b = u32::from((p >> 10) & 0x1F);
    [
        ((r * 13 + g * 2 + b) >> 1) as u8,
        ((g * 3 + b) << 1) as u8,
        ((r * 3 + g * 2 + b * 11) >> 1) as u8,
    ]
}

pub fn frame_to_gambatte_rgb(frame: &Frame) -> Rgb {
    Rgb {
        width: W,
        height: crate::checks::H,
        data: frame.iter().map(|&p| gambatte_rgb(p)).collect(),
    }
}

/// Ink (black) and paper (white) of the hex digits, as Gambatte's formula
/// renders RGB555 0x0000 and 0x7FFF.
pub const INK: [u8; 3] = [0, 0, 0];
pub const PAPER: [u8; 3] = [248, 248, 248];

/// The 16 hex digits, 8x8 each. One byte per row, top row first; bit 7 is
/// the leftmost pixel; a set bit is ink. Derived from the digits the test
/// ROMs draw (ADR 0018): the shape most often drawn in a digit's cell by
/// the core across the cgb04c ROMs, and for B (which only one ROM expects)
/// the font tile stored in the test ROMs; the two sources agree on every
/// other digit. Every glyph's top row and left column are paper.
pub const GLYPHS: [[u8; 8]; 16] = [
    [0x00, 0x7F, 0x41, 0x41, 0x41, 0x41, 0x41, 0x7F], // 0
    [0x00, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08, 0x08], // 1
    [0x00, 0x7F, 0x01, 0x01, 0x7F, 0x40, 0x40, 0x7F], // 2
    [0x00, 0x7F, 0x01, 0x01, 0x3F, 0x01, 0x01, 0x7F], // 3
    [0x00, 0x41, 0x41, 0x41, 0x7F, 0x01, 0x01, 0x01], // 4
    [0x00, 0x7F, 0x40, 0x40, 0x7E, 0x01, 0x01, 0x7E], // 5
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x41, 0x41, 0x7F], // 6
    [0x00, 0x7F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10], // 7
    [0x00, 0x3E, 0x41, 0x41, 0x3E, 0x41, 0x41, 0x3E], // 8
    [0x00, 0x7F, 0x41, 0x41, 0x7F, 0x01, 0x01, 0x7F], // 9
    [0x00, 0x08, 0x22, 0x41, 0x7F, 0x41, 0x41, 0x41], // A
    [0x00, 0x7E, 0x41, 0x41, 0x7E, 0x41, 0x41, 0x7E], // B
    [0x00, 0x3E, 0x41, 0x40, 0x40, 0x40, 0x41, 0x3E], // C
    [0x00, 0x7E, 0x41, 0x41, 0x41, 0x41, 0x41, 0x7E], // D
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x40, 0x40, 0x7F], // E
    [0x00, 0x7F, 0x40, 0x40, 0x7F, 0x40, 0x40, 0x40], // F
];

/// The image `digits` must produce: `frame` with the digit cells replaced
/// by the expected glyphs. Comparing it with the frame exactly checks the
/// cells and nothing else.
pub fn expected_image(actual: &Rgb, digits: &[u8]) -> Rgb {
    let mut img = actual.clone();
    for (i, &d) in digits.iter().enumerate() {
        for (y, row) in GLYPHS[usize::from(d)].iter().enumerate() {
            for x in 0..8 {
                let ink = row & (0x80 >> x) != 0;
                img.data[y * W + i * 8 + x] = if ink { INK } else { PAPER };
            }
        }
    }
    img
}

/// The `_outaudio0/1` rule (ADR 0018, audio amendment): `sound` = false
/// (`audio0`) wants silence over the last frame, true (`audio1`) wants
/// sound. As in testrunner, silent means every sample of that frame equals
/// the first one (both sides; any constant level, so DC is silence).
/// `Ok(None)` passes, `Ok(Some(why))` fails, `Err` is a runner error.
pub fn audio_failure(
    sound: bool,
    capture: Option<&AudioCapture>,
) -> Result<Option<String>, String> {
    let Some(c) = capture else {
        return Ok(Some(
            "the run never reached its last frame, so there is no audio to judge".into(),
        ));
    };
    if c.full {
        return Err(format!(
            "runner error: the last frame's audio filled all {} slots, so samples may be \
             missing",
            c.samples.len()
        ));
    }
    if c.end < c.until {
        return Ok(Some(format!(
            "the run ended at clock {} before the last frame's audio was complete (clock {})",
            c.end, c.until
        )));
    }
    // The window [start, end) yields one sample per AUDIO_SAMPLE_CLOCKS, so
    // fewer than (end - start) / AUDIO_SAMPLE_CLOCKS means the capture lost
    // samples (floor(x) - floor(y) >= floor(x - y), so a correct capture
    // never falls under this). A too-short capture could fake silence.
    let min = (c.end.saturating_sub(c.start) / u64::from(gbc_core::AUDIO_SAMPLE_CLOCKS)) as usize;
    if c.samples.len() < min {
        return Err(format!(
            "runner error: the last frame's audio has {} samples, fewer than the {min} \
             that clocks {}..{} produce",
            c.samples.len(),
            c.start,
            c.end
        ));
    }
    let Some(&first) = c.samples.first() else {
        return Ok(Some("the last frame produced no audio samples".into()));
    };
    let differs = c.samples.iter().position(|&s| s != first);
    let n = c.samples.len();
    Ok(match (sound, differs) {
        (false, None) | (true, Some(_)) => None,
        (false, Some(i)) => Some(format!(
            "`_outaudio0` expects silence, but the last frame has sound: sample {i} of {n} \
             ({}, {}) differs from sample 0 ({}, {})",
            c.samples[i].left, c.samples[i].right, first.left, first.right
        )),
        (true, None) => Some(format!(
            "`_outaudio1` expects sound, but all {n} samples of the last frame are ({}, {})",
            first.left, first.right
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_out_names() {
        let hex = |d: &[u8]| Ok(Some(Expect::Hex(d.to_vec())));
        assert_eq!(parse_out("t_cgb04c_out1"), hex(&[1]));
        assert_eq!(parse_out("t_dmg08_cgb04c_out0B"), hex(&[0, 0xB]));
        assert_eq!(parse_out("t_dmg08_out2_cgb04c_out0"), hex(&[0]));
        assert_eq!(parse_out("t_cgb04c_out3_dmg08_out2"), hex(&[3]));
        assert_eq!(parse_out("t_out3_dmg08_out2"), Ok(None));
        assert_eq!(parse_out("t_dmg08_out2"), Ok(None));
        assert_eq!(parse_out("t_out3"), hex(&[3]));
        // Digits end at the first non-hex character.
        assert_eq!(parse_out("t_cgb04c_outfe_x"), hex(&[0xF, 0xE]));
        assert_eq!(parse_out("t_cgb04c_out1g2"), hex(&[1]));
        // The first `_out` wins, as in testrunner.
        assert_eq!(parse_out("a_out1_b_out2"), hex(&[1]));
        assert_eq!(
            parse_out("t_cgb04c_outaudio0"),
            Ok(Some(Expect::Audio(false)))
        );
        assert_eq!(
            parse_out("t_dmg08_cgb04c_outaudio1"),
            Ok(Some(Expect::Audio(true)))
        );
        // `audio2` is not an audio result; its leading `a` is a digit.
        assert_eq!(parse_out("t_outaudio2"), hex(&[0xA]));
        assert_eq!(parse_out("t"), Ok(None));
        assert!(parse_out("t_cgb04c_out").is_err());
        assert!(parse_out("t_cgb04c_out_1").is_err());
        assert!(parse_out(&format!("t_out{}", "0".repeat(20))).is_ok());
        assert!(parse_out(&format!("t_out{}", "0".repeat(21))).is_err());
    }

    #[test]
    fn png_candidates_in_order() {
        assert_eq!(
            png_candidates("a/b", "t"),
            ["a/b/t_dmg08_cgb04c.png", "a/b/t_cgb04c.png"]
        );
        assert_eq!(png_candidates("", "t")[1], "t_cgb04c.png");
    }

    #[test]
    fn gambatte_color_formula() {
        assert_eq!(gambatte_rgb(0x0000), [0, 0, 0]);
        assert_eq!(gambatte_rgb(0x7FFF), [248, 248, 248]);
        // Pure red, green, blue (R=31 / G=31 / B=31).
        assert_eq!(gambatte_rgb(0x001F), [201, 0, 46]);
        assert_eq!(gambatte_rgb(0x03E0), [31, 186, 31]);
        assert_eq!(gambatte_rgb(0x7C00), [15, 62, 170]);
        // R=1, G=2, B=3: (13+4+3)/2, (6+3)*2, (3+4+33)/2.
        assert_eq!(gambatte_rgb(1 | (2 << 5) | (3 << 10)), [10, 18, 20]);
        // Odd sums round down: R=1 -> 13/2 = 6 and 3/2 = 1.
        assert_eq!(gambatte_rgb(0x0001), [6, 0, 1]);
        // Bit 15 is not a color bit.
        assert_eq!(gambatte_rgb(0x8000), [0, 0, 0]);
        // No channel exceeds 248.
        for p in 0..0x8000u16 {
            assert!(gambatte_rgb(p).iter().all(|&c| c <= 248));
        }
    }

    #[test]
    fn glyphs_are_distinct_and_framed() {
        for (i, g) in GLYPHS.iter().enumerate() {
            assert_eq!(g[0], 0, "{i:X}: top row is paper");
            assert!(
                g.iter().all(|r| r & 0x80 == 0),
                "{i:X}: left column is paper"
            );
            assert!(g.iter().any(|&r| r != 0), "{i:X}: has ink");
            for (j, h) in GLYPHS.iter().enumerate().skip(i + 1) {
                assert_ne!(g, h, "{i:X} and {j:X}");
            }
        }
    }
}
