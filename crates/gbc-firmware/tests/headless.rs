//! The headless loop on a tiny synthetic ROM (always available) and on
//! cpu_instrs from `test-roms/` (skipped when absent, unless
//! `GBC_REQUIRE_TEST_DATA=1`; `GBC_TEST_ROMS` overrides the directory, for
//! scratch worktrees).

use std::path::PathBuf;

use gbc_core::{Buttons, Frame, Memory, StereoSample, WHITE};
use gbc_firmware::CycleClock;
use gbc_firmware::fnv::{self, Fnv1a};
use gbc_firmware::headless::{self, Buffers, Error, MIN_AUDIO_SLOTS, Summary};
use gbc_firmware::input::{Press, Script};

/// A clock that counts its own reads, so the stats lines are
/// deterministic too here (they still aren't part of the comparison).
struct CountingClock(std::cell::Cell<u32>);

impl CycleClock for CountingClock {
    fn now(&self) -> u32 {
        let t = self.0.get();
        self.0.set(t.wrapping_add(1000));
        t
    }
    fn hz(&self) -> u32 {
        0
    }
}

/// A 32 KiB ROM-only cartridge that starts a square wave on channel 1 and
/// then, forever, reads the action buttons from P1 and writes the result
/// to BGP. Every tile is tile 0 (all zeros, colour 0), so the whole screen
/// takes BGP's colour 0 = P1 bits 0-1: pressing A changes the frame.
fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    // Entry point: nop; jp $0150.
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]);
    // Header: ROM only (0147 = 0), 32 KiB (0148 = 0), no RAM (0149 = 0).
    let code = [
        0x3e, 0x80, 0xe0, 0x26, // NR52 = $80: APU on
        0x3e, 0x77, 0xe0, 0x24, // NR50 = $77: full volume
        0x3e, 0xff, 0xe0, 0x25, // NR51 = $ff: every channel to both sides
        0x3e, 0x80, 0xe0, 0x11, // NR11 = $80: 50% duty
        0x3e, 0xf0, 0xe0, 0x12, // NR12 = $f0: volume 15, no envelope
        0x3e, 0x00, 0xe0, 0x13, // NR13 = 0
        0x3e, 0x87, 0xe0, 0x14, // NR14 = $87: trigger
        // loop:
        0x3e, 0x10, 0xe0, 0x00, // P1 = $10: select the action buttons
        0xf0, 0x00, 0xf0, 0x00, // read P1 twice (settle)
        0xe0, 0x47, // BGP = P1
        0x18, 0xf4, // jr loop (-12)
    ];
    rom[0x150..0x150 + code.len()].copy_from_slice(&code);
    rom
}

struct Bufs {
    mem: Box<Memory>,
    save: Vec<u8>,
    frame: Box<Frame>,
    audio: Vec<StereoSample>,
}

impl Bufs {
    fn new(save: usize, audio: usize) -> Self {
        Self {
            mem: Box::new(Memory::new()),
            save: vec![0xa5; save],
            frame: Box::new([0; 160 * 144]),
            audio: vec![StereoSample::default(); audio],
        }
    }
    fn lend(&mut self) -> Buffers<'_> {
        Buffers {
            mem: &mut self.mem,
            save_ram: &mut self.save,
            frame: &mut self.frame,
            audio: &mut self.audio,
        }
    }
}

fn run_with(
    rom: &[u8],
    presses: &[Press],
    frames: u32,
    bufs: &mut Bufs,
) -> (String, Result<Summary, Error>) {
    let mut out = String::new();
    let clock = CountingClock(std::cell::Cell::new(0));
    let r = headless::run(
        rom,
        Script::new(presses),
        frames,
        bufs.lend(),
        &clock,
        &mut out,
    );
    (out, r)
}

fn run(rom: &[u8], presses: &[Press], frames: u32) -> String {
    let (out, r) = run_with(rom, presses, frames, &mut Bufs::new(0, MIN_AUDIO_SLOTS));
    r.unwrap();
    out
}

/// The compared lines (`headless ...`).
fn compared(out: &str) -> Vec<&str> {
    out.lines().filter(|l| l.starts_with("headless ")).collect()
}

fn is_hex(s: &str, n: usize) -> bool {
    s.len() == n && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[test]
fn synthetic_run_is_deterministic() {
    let rom = synthetic_rom();
    let presses = [Press {
        frame: 3,
        hold: 2,
        buttons: Buttons::A,
    }];
    let a = run(&rom, &presses, 30);
    let b = run(&rom, &presses, 30);
    assert_eq!(a, b);
    // Dirty buffers from the caller don't leak into the output.
    let mut dirty = Bufs::new(0, 1024);
    dirty.frame.fill(0x1234);
    dirty.audio.fill(StereoSample { left: 7, right: 7 });
    let (c, r) = run_with(&rom, &presses, 30, &mut dirty);
    r.unwrap();
    assert_eq!(compared(&a), compared(&c));
}

#[test]
fn line_format() {
    let rom = synthetic_rom();
    let out = run(&rom, &[], 5);
    let lines: Vec<&str> = out.lines().collect();
    assert!(out.ends_with('\n') && !out.contains('\r'), "{out:?}");
    assert_eq!(
        lines[0],
        format!(
            "headless v1 rom_bytes=32768 rom_fnv={:016x} input_fnv={:016x} frames=5",
            fnv::hash(&rom),
            fnv::OFFSET_BASIS
        )
    );
    let mut last_clock = 0u64;
    for n in 0..5 {
        let l = lines[1 + n];
        let rest = l
            .strip_prefix(&format!("headless frame={n} clock="))
            .unwrap_or_else(|| panic!("{l:?}"));
        let f: Vec<&str> = rest.split(' ').collect();
        assert_eq!(f.len(), 3, "{l:?}");
        let clock: u64 = f[0].parse().unwrap();
        assert!(clock > last_clock, "{l:?}");
        last_clock = clock;
        let ev = f[1].strip_prefix("ev=").unwrap();
        assert!(is_hex(ev, 4), "{l:?}");
        // The LCD is on: every call ends on a frame.
        assert_eq!(u16::from_str_radix(ev, 16).unwrap() & 1, 1, "{l:?}");
        assert!(is_hex(f[2].strip_prefix("fb=").unwrap(), 16), "{l:?}");
    }
    let audio = lines[6]
        .strip_prefix("headless audio frames=5 samples=")
        .unwrap_or_else(|| panic!("{:?}", lines[6]));
    let (samples, h) = audio.split_once(" fnv=").unwrap();
    // 128 base clocks a sample, one call per frame.
    let samples: u64 = samples.parse().unwrap();
    assert_eq!(samples, last_clock / 128, "{audio:?}");
    assert!(is_hex(h, 16));
    // Then only stats lines (the counting clock: 1000 per call).
    assert_eq!(
        &lines[7..],
        ["stats run_frame min=1000 mean=1000 max=1000 max_frame=0 hz=0"]
    );
}

#[test]
fn input_changes_exactly_the_pressed_frames() {
    let rom = synthetic_rom();
    let none = run(&rom, &[], 20);
    let a = run(
        &rom,
        &[Press {
            frame: 10,
            hold: 5,
            buttons: Buttons::A,
        }],
        20,
    );
    let (none, a) = (compared(&none), compared(&a));
    assert_ne!(none[0], a[0], "input_fnv");
    for n in 0..20 {
        let (x, y) = (none[1 + n], a[1 + n]);
        if (10..15).contains(&n) {
            assert_ne!(x, y, "frame {n}");
        } else {
            assert_eq!(x, y, "frame {n}");
        }
    }
    // The audio doesn't depend on the buttons in this ROM.
    assert_eq!(none[21], a[21]);
}

#[test]
fn hashes_match_the_buffers() {
    let rom = synthetic_rom();
    let mut bufs = Bufs::new(0, MIN_AUDIO_SLOTS);
    let (out, r) = run_with(&rom, &[], 1, &mut bufs);
    let s = r.unwrap();
    let mut h = Fnv1a::new();
    h.frame(&bufs.frame);
    assert_eq!(s.last_fb, h.get());
    assert!(out.contains(&format!(" fb={:016x}\n", h.get())), "{out}");
    // The square wave plays: the stream is not all zeros.
    let mut zeros = Fnv1a::new();
    zeros.samples(&vec![StereoSample::default(); s.samples as usize]);
    assert_ne!(s.audio_fnv, zeros.get());
    assert!(s.samples > 500, "{s:?}");
    // The frame is one colour (BGP colour 0), not the initial white.
    assert!(bufs.frame.iter().all(|&p| p == bufs.frame[0]));
    assert_ne!(bufs.frame[0], WHITE);
}

#[test]
fn errors_print_one_line_after_the_header() {
    let mut with_ram = synthetic_rom();
    with_ram[0x147] = 0x03; // MBC1+RAM+BATTERY
    with_ram[0x149] = 0x02; // 8 KiB
    for (rom, save, audio, want) in [
        (
            with_ram.clone(),
            0x1fff,
            MIN_AUDIO_SLOTS,
            "headless error save_ram need=8192 have=8191",
        ),
        (
            synthetic_rom(),
            0,
            MIN_AUDIO_SLOTS - 1,
            "headless error audio_buffer need=550 have=549",
        ),
        (
            vec![0; 16],
            0,
            MIN_AUDIO_SLOTS,
            "headless error rom_header RomTooSmall { len: 16 }",
        ),
    ] {
        let (out, r) = run_with(&rom, &[], 10, &mut Bufs::new(save, audio));
        assert!(r.is_err());
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "{out}");
        assert!(lines[0].starts_with("headless v1 "), "{out}");
        assert_eq!(lines[1], want);
    }
    // Enough RAM: it runs, from zeroed RAM whatever the buffer held.
    let (out, r) = run_with(&with_ram, &[], 2, &mut Bufs::new(0x2000, 1024));
    r.unwrap();
    assert_eq!(compared(&out).len(), 4, "{out}");
}

fn test_rom(rel: &str) -> Option<Vec<u8>> {
    let dir = std::env::var_os("GBC_TEST_ROMS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-roms"),
        PathBuf::from,
    );
    let path = dir.join(rel);
    match std::fs::read(&path) {
        Ok(r) => Some(r),
        Err(e) => {
            assert!(
                std::env::var("GBC_REQUIRE_TEST_DATA").as_deref() != Ok("1"),
                "{}: {e} (GBC_REQUIRE_TEST_DATA=1)",
                path.display()
            );
            eprintln!("SKIPPED: {}: {e}", path.display());
            None
        }
    }
}

#[test]
fn cpu_instrs_is_deterministic() {
    let Some(rom) = test_rom("blargg/cpu_instrs/cpu_instrs.gb") else {
        return;
    };
    let a = run(&rom, &[], 60);
    let b = run(&rom, &[], 60);
    assert_eq!(a, b);
    assert_eq!(compared(&a).len(), 62);
}
