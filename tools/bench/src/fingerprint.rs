//! Behavior fingerprint (docs/perf.md, ADR 0011 "Verification").
//!
//! Runs a fixed corpus of test ROMs for a fixed number of emulated clocks
//! and prints one hash per ROM. Two builds that print the same lines behave
//! identically on everything the hash covers, so a restructuring of the
//! core's timing can be checked against its parent commit
//! (`scripts/diff-fingerprint.sh`).
//!
//! Per ROM: the emulator is loaded as romtest does (the only model is CGB C,
//! ADR 0003, with the default palettes), then run in 456-clock `run_until`
//! chunks with `LD_B_B` and `SERIAL_TX` stops enabled. After each chunk the
//! hash folds the clock, the chunk's clocks and events, the byte that
//! `SERIAL_TX` reported, the CPU registers, and the side-effect-free `peek`
//! of every I/O register FF00-FF7F (joypad, serial, timer, IF, APU and wave
//! RAM, PPU, CGB registers) and IE. On `FRAME_READY` it folds the frame
//! buffer. At the end it folds, via `peek`, VRAM 8000-9FFF, WRAM C000-DFFF,
//! OAM with FEA0-FEFF, and HRAM, then the whole cartridge RAM and the MBC3
//! RTC state.
//!
//! Not covered: the VRAM bank and the WRAM banks that are not mapped at the
//! end, and CGB palette RAM. The public API has no side-effect-free access
//! to them (`peek` cannot switch banks without writing VBK, SVBK or BCPS).
//!
//! Every ROM runs once without input. Every ROM outside gambatte runs a
//! second time (`<path>#input`) with a fixed press/release sequence
//! scheduled through `set_buttons_at` at clocks inside chunks, so the
//! joypad and scheduled-input paths are fingerprinted too.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gbc_core::{
    Buttons, CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT,
    SCREEN_WIDTH, WHITE, cart_ram_size,
};

/// A corpus entry: a directory (relative to the root) searched recursively
/// for `.gb`/`.gbc`, an optional file-name filter, the run length as a
/// divisor of `--seconds`, and whether the input pass runs it.
struct Suite {
    dir: &'static str,
    name_contains: Option<&'static str>,
    div: u64,
    input_pass: bool,
}

const fn suite(dir: &'static str) -> Suite {
    Suite {
        dir,
        name_contains: None,
        div: 1,
        input_pass: true,
    }
}

const CORPUS: &[Suite] = &[
    suite("mooneye-test-suite/acceptance"),
    suite("mooneye-test-suite/misc"),
    suite("mooneye-test-suite/emulator-only"),
    suite("blargg"),
    suite("age-test-roms"),
    suite("same-suite"),
    suite("cgb-acid2"),
    suite("cgb-acid-hell"),
    suite("dmg-acid2"),
    suite("mealybug-tearoom-tests"),
    suite("rtc3test"),
    suite("mbc3-tester"),
    // As in the manifest (`gambatte/**/*cgb04c*`, acceptance/stage1.toml).
    // Its 3,154 ROMs finish within 15 frames, so they run for 1/8 of the
    // time (22 frames at the default 3 s) and without the input pass.
    Suite {
        dir: "gambatte",
        name_contains: Some("cgb04c"),
        div: 8,
        input_pass: false,
    },
];

/// Fewer runs than this means test-roms/ is missing or partial (the pinned
/// release gives 334 ROMs outside gambatte, run twice, and 3,154 gambatte
/// ROMs: 3,822 runs), and "identical" would mean little.
pub const MIN_RUNS: usize = 3_800;

const CHUNK_CLOCKS: u64 = 456;

/// The input pass: one change every `INPUT_PERIOD` clocks, offset by a
/// phase that walks through the chunk, so changes land mid-chunk at varying
/// M-cycle offsets.
const INPUT_PERIOD: u64 = CLOCK_HZ as u64 / 8;
const NONE: Buttons = Buttons::from_bits_truncate(0);
const INPUT_SEQ: [Buttons; 16] = [
    Buttons::A,
    NONE,
    Buttons::START,
    NONE,
    Buttons::DOWN,
    NONE,
    Buttons::A.union(Buttons::B),
    NONE,
    Buttons::RIGHT,
    NONE,
    Buttons::UP.union(Buttons::SELECT),
    NONE,
    Buttons::LEFT,
    NONE,
    Buttons::B,
    NONE,
];

/// The k-th scheduled input change: its clock and the buttons held after it.
fn input_change(k: u64) -> (u64, Buttons) {
    let clock = (k + 1) * INPUT_PERIOD + (k * 97) % CHUNK_CLOCKS;
    (clock, INPUT_SEQ[(k % 16) as usize])
}

/// FNV-1a, 64 bit.
struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }
    fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    fn bytes(&mut self, bs: &[u8]) {
        for &b in bs {
            self.byte(b);
        }
    }
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }
    /// Folds a frame four pixels per step (FNV over 64-bit words: weaker
    /// mixing than per byte, but any single pixel change still changes it).
    fn frame(&mut self, f: &Frame) {
        for px in f.as_chunks::<4>().0 {
            let w = u64::from(px[0])
                | u64::from(px[1]) << 16
                | u64::from(px[2]) << 32
                | u64::from(px[3]) << 48;
            self.0 = (self.0 ^ w).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn peek_range(&mut self, emu: &Emulator<'_>, range: std::ops::RangeInclusive<u16>) {
        for a in range {
            self.byte(emu.peek(a));
        }
    }
}

fn collect(dir: &Path, name_contains: Option<&str>, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, name_contains, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("gb" | "gbc"))
            && name_contains.is_none_or(|s| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(s))
            })
        {
            out.push(p);
        }
    }
}

/// `Err` if the ROM fails to read or load.
fn fingerprint(path: &Path, end: u64, input: bool) -> Result<u64, String> {
    let rom = std::fs::read(path).map_err(|e| e.to_string())?;
    let ram_size = cart_ram_size(&rom).map_err(|e| format!("{e:?}"))?;
    let mut cart_ram = vec![0u8; ram_size];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    emu.load(&rom, &mut cart_ram, config)
        .map_err(|e| e.to_string())?;
    emu.set_stop_events(Events::LD_B_B | Events::SERIAL_TX);
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");

    let mut h = Fnv::new();
    let mut next_input = 0u64;
    while emu.clock() < end {
        let target = emu.clock() + CHUNK_CLOCKS;
        if input {
            // Queue every change due before the chunk ends; whether the
            // core accepted it is part of the hash.
            while input_change(next_input).0 < target {
                let (clock, buttons) = input_change(next_input);
                h.byte(u8::from(emu.set_buttons_at(clock, buttons)));
                next_input += 1;
            }
        }
        let r = emu.run_until(target, &mut frame, &mut []);
        h.u64(emu.clock());
        h.u64(r.clocks);
        h.bytes(&r.events.bits().to_le_bytes());
        if r.events.contains(Events::SERIAL_TX) {
            h.byte(emu.serial_byte());
        }
        if r.events.contains(Events::FRAME_READY) {
            h.frame(&frame);
        }
        let g = emu.registers();
        h.bytes(&[g.a, g.f, g.b, g.c, g.d, g.e, g.h, g.l]);
        h.bytes(&g.sp.to_le_bytes());
        h.bytes(&g.pc.to_le_bytes());
        h.peek_range(&emu, 0xFF00..=0xFF7F);
        h.byte(emu.peek(0xFFFF));
    }
    h.peek_range(&emu, 0x8000..=0x9FFF);
    h.peek_range(&emu, 0xC000..=0xDFFF);
    h.peek_range(&emu, 0xFE00..=0xFEFF);
    h.peek_range(&emu, 0xFF80..=0xFFFE);
    h.bytes(emu.cart_ram());
    h.bytes(format!("{:?}", emu.rtc_state()).as_bytes());
    Ok(h.0)
}

struct Run {
    path: PathBuf,
    end: u64,
    input: bool,
}

pub fn run(root: &Path, secs: u64) -> std::process::ExitCode {
    let mut runs = Vec::new();
    for s in CORPUS {
        let mut roms = Vec::new();
        collect(&root.join(s.dir), s.name_contains, &mut roms);
        let end = secs * u64::from(CLOCK_HZ) / s.div;
        for p in roms {
            if s.input_pass {
                runs.push(Run {
                    path: p.clone(),
                    end,
                    input: true,
                });
            }
            runs.push(Run {
                path: p,
                end,
                input: false,
            });
        }
    }
    runs.sort_by(|a, b| (&a.path, a.input).cmp(&(&b.path, b.input)));
    if runs.len() < MIN_RUNS {
        eprintln!(
            "bench: only {} fingerprint runs under {} (expected at least {MIN_RUNS}): \
             test-roms is missing or partial (run scripts/fetch-test-roms.sh or pass --root)",
            runs.len(),
            root.display()
        );
        return std::process::ExitCode::from(2);
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<u64, String>>>> = Mutex::new(vec![None; runs.len()]);
    let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(r) = runs.get(i) else { break };
                    let h = fingerprint(&r.path, r.end, r.input);
                    results.lock().expect("lock")[i] = Some(h);
                }
            });
        }
    });
    let results = results.into_inner().expect("lock");
    for (r, h) in runs.iter().zip(results) {
        let rel = r.path.strip_prefix(root).unwrap_or(&r.path).display();
        let tag = if r.input { "#input" } else { "" };
        match h {
            Some(Ok(h)) => println!("{h:016x} {rel}{tag}"),
            _ => println!("LOAD-ERROR {rel}{tag}"),
        }
    }
    std::process::ExitCode::SUCCESS
}
