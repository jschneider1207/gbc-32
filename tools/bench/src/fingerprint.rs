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
//!
//! `--audio-buffer N` passes an N-sample audio buffer to every call instead
//! of an empty one (as the MCU will). `AUDIO_BUFFER_FULL` then ends calls
//! early; a chunk is completed by further calls to the same target, and the
//! hash folds the merged chunk (summed clocks, events without
//! `AUDIO_BUFFER_FULL`), so it is defined exactly as without audio. The
//! hash must not change: the APU state, and so every register read, is the
//! same whether the syncs integrated or only advanced, however the calls
//! split the run (ADR 0021 section 3, the sync invariant; plan M6 task 7).
//! The samples themselves are not hashed, unless `--hash-samples` (needs
//! `--audio-buffer`) is given: then every sample the calls wrote is folded
//! into the hash too, and the synthetic ROMs (`synth:<name>`, the bench's
//! programs) are added to the corpus, so an exact APU optimization can be
//! shown sample-identical. Without the flag the lines are unchanged.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use gbc_core::{
    Buttons, CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT,
    SCREEN_WIDTH, StereoSample, WHITE, cart_ram_size,
};

/// A corpus entry: a directory (relative to the root) searched recursively
/// for `.gb`/`.gbc`, an optional file-name filter, the run length as a
/// divisor of `--seconds`, and whether the input pass runs it.
struct Suite {
    dir: &'static str,
    name_contains: Option<&'static str>,
    /// Only ROMs whose name lacks `cgb04c` and that have a sibling
    /// `<stem>_cgb04c.png` (the manifest's `gambatte-cgb-png` group).
    sibling_png: bool,
    div: u64,
    input_pass: bool,
}

const fn suite(dir: &'static str) -> Suite {
    Suite {
        dir,
        name_contains: None,
        sibling_png: false,
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
        sibling_png: false,
        div: 8,
        input_pass: false,
    },
    // As the manifest's `gambatte-cgb-png` group (ADR 0018): the 198 ROMs with
    // a sibling `_cgb04c.png` reference and no `cgb04c` in the name (scx_during_m3,
    // scy, bgtilemap, bgtiledata, window...). Same run length: the check reads
    // frame 15 (`run_frames = 15`), and 1/8 of 3 s is 22 frames.
    Suite {
        dir: "gambatte",
        name_contains: None,
        sibling_png: true,
        div: 8,
        input_pass: false,
    },
];

/// Fewer runs than this means test-roms/ is missing or partial (the pinned
/// release gives 334 ROMs outside gambatte, run twice, and 3,154 gambatte
/// `cgb04c` ROMs and 198 with a sibling PNG: 4,020 runs), and "identical" would mean little.
pub const MIN_RUNS: usize = 4_000;

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

/// The `gambatte-cgb-png` rule: no `cgb04c` in the name, and a sibling
/// `<stem>_cgb04c.png` exists.
fn has_cgb04c_png(p: &Path) -> bool {
    let named = p
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains("cgb04c"));
    !named
        && p.with_extension("")
            .with_file_name(format!(
                "{}_cgb04c.png",
                p.file_stem().and_then(|s| s.to_str()).unwrap_or("")
            ))
            .is_file()
}

fn collect(dir: &Path, s: &Suite, out: &mut Vec<PathBuf>) {
    let name_contains = s.name_contains;
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, s, out);
        } else if matches!(p.extension().and_then(|x| x.to_str()), Some("gb" | "gbc"))
            && name_contains.is_none_or(|s| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.contains(s))
            })
            && (!s.sibling_png || has_cgb04c_png(&p))
        {
            out.push(p);
        }
    }
}

/// How every run of a fingerprint pass is made.
#[derive(Clone, Copy, Default)]
pub struct Opts {
    /// The per-call audio buffer length (0: empty).
    pub audio: usize,
    /// Fold the samples into the hash (needs `audio`).
    pub hash_samples: bool,
    /// Power on with the clock at this value (`--clock-offset`, the
    /// `clock-offset` feature). Every clock that is hashed or passed in is
    /// taken relative to it, so the lines must equal those of a run from 0.
    pub clock_offset: u64,
}

/// Moves the power-on state to clock `offset` (not 0).
#[cfg(feature = "clock-offset")]
fn start_clock_at(emu: &mut Emulator<'_>, offset: u64) {
    emu.start_clock_at(offset);
}

/// Without the feature the core has no such hook; `fingerprint_main`
/// refuses `--clock-offset` before any run.
#[cfg(not(feature = "clock-offset"))]
fn start_clock_at(_: &mut Emulator<'_>, _: u64) {
    unreachable!("--clock-offset without the clock-offset feature");
}

/// Whether this build can honor `--clock-offset`.
pub const HAS_CLOCK_OFFSET: bool = cfg!(feature = "clock-offset");

/// `Err` if the ROM fails to read or load. `trace`: print one line per
/// chunk (`--trace`), to find where two builds or offsets part.
fn fingerprint(path: &Path, end: u64, input: bool, opts: Opts, trace: bool) -> Result<u64, String> {
    let Opts {
        audio,
        hash_samples,
        clock_offset: base,
    } = opts;
    let rom = match path.to_str().and_then(|p| p.strip_prefix("synth:")) {
        Some(name) => crate::synth::build(name).ok_or("unknown synthetic ROM")?,
        None => std::fs::read(path).map_err(|e| e.to_string())?,
    };
    let ram_size = cart_ram_size(&rom).map_err(|e| format!("{e:?}"))?;
    let mut cart_ram = vec![0u8; ram_size];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    emu.load(&rom, &mut cart_ram, config)
        .map_err(|e| e.to_string())?;
    if base != 0 {
        start_clock_at(&mut emu, base);
    }
    let stop = Events::LD_B_B | Events::SERIAL_TX;
    emu.set_stop_events(stop);
    // The stops a call would have had without audio.
    let stop = (stop | Events::ALWAYS_STOP) - Events::AUDIO_BUFFER_FULL;
    let mut samples = vec![StereoSample::default(); audio];
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");

    let mut h = Fnv::new();
    let mut next_input = 0u64;
    while emu.clock() - base < end {
        let target = emu.clock() + CHUNK_CLOCKS;
        if input {
            // Queue every change due before the chunk ends; whether the
            // core accepted it is part of the hash.
            while input_change(next_input).0 + base < target {
                let (clock, buttons) = input_change(next_input);
                h.byte(u8::from(emu.set_buttons_at(clock + base, buttons)));
                next_input += 1;
            }
        }
        let (mut clocks, mut events) = (0, Events::EMPTY);
        loop {
            let r = emu.run_until(target, &mut frame, &mut samples);
            clocks += r.clocks;
            if hash_samples {
                for s in &samples[..r.audio_samples] {
                    h.bytes(&s.left.to_le_bytes());
                    h.bytes(&s.right.to_le_bytes());
                }
            }
            let ev = r.events - Events::AUDIO_BUFFER_FULL;
            events |= ev;
            if ev.intersects(stop)
                || emu.clock() >= target
                || !r.events.contains(Events::AUDIO_BUFFER_FULL)
            {
                break;
            }
        }
        h.u64(emu.clock() - base);
        h.u64(clocks);
        h.bytes(&events.bits().to_le_bytes());
        if events.contains(Events::SERIAL_TX) {
            h.byte(emu.serial_byte());
        }
        if events.contains(Events::FRAME_READY) {
            h.frame(&frame);
        }
        let g = emu.registers();
        h.bytes(&[g.a, g.f, g.b, g.c, g.d, g.e, g.h, g.l]);
        h.bytes(&g.sp.to_le_bytes());
        h.bytes(&g.pc.to_le_bytes());
        h.peek_range(&emu, 0xFF00..=0xFF7F);
        h.byte(emu.peek(0xFFFF));
        if trace {
            // The hash so far, the clock after the chunk (less the offset),
            // the events, then what usually parts first.
            println!(
                "{:016x} {:>10} ev={:04x} pc={:04x} sp={:04x} af={:02x}{:02x} bc={:02x}{:02x} \
                 de={:02x}{:02x} hl={:02x}{:02x} div={:02x} tima={:02x} if={:02x} ie={:02x} \
                 lcdc={:02x} stat={:02x} ly={:02x} nr52={:02x} pcm={:02x}{:02x} hdma5={:02x}",
                h.0,
                emu.clock() - base,
                events.bits(),
                g.pc,
                g.sp,
                g.a,
                g.f,
                g.b,
                g.c,
                g.d,
                g.e,
                g.h,
                g.l,
                emu.peek(0xFF04),
                emu.peek(0xFF05),
                emu.peek(0xFF0F),
                emu.peek(0xFFFF),
                emu.peek(0xFF40),
                emu.peek(0xFF41),
                emu.peek(0xFF44),
                emu.peek(0xFF26),
                emu.peek(0xFF76),
                emu.peek(0xFF77),
                emu.peek(0xFF55),
            );
        }
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

/// `--trace <run>`: one run of the corpus (a path under `root` or
/// `synth:<name>`, with `#input` for the input pass), a line per chunk.
/// The run length is that of the corpus entry the path belongs to.
pub fn trace(root: &Path, secs: u64, opts: Opts, run: &str) -> std::process::ExitCode {
    let (rel, input) = match run.strip_suffix("#input") {
        Some(p) => (p, true),
        None => (run, false),
    };
    let (path, div) = if rel.starts_with("synth:") {
        (PathBuf::from(rel), 1)
    } else {
        let div = if rel.starts_with("gambatte") { 8 } else { 1 };
        (root.join(rel), div)
    };
    let end = secs * u64::from(CLOCK_HZ) / div;
    match fingerprint(&path, end, input, opts, true) {
        Ok(h) => {
            println!("{h:016x} {run}");
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("bench: {run}: {e}");
            std::process::ExitCode::from(2)
        }
    }
}

pub fn run(root: &Path, secs: u64, opts: Opts) -> std::process::ExitCode {
    let mut runs = Vec::new();
    if opts.hash_samples {
        // The worst case one channel at a time too (`@<mask>`), and with
        // the APU off (`@0`: no APU writes at all).
        let solo = [
            "worst-case@0",
            "worst-case@1",
            "worst-case@2",
            "worst-case@4",
            "worst-case@8",
        ];
        for name in crate::synth::NAMES.iter().chain(&solo) {
            runs.push(Run {
                path: PathBuf::from(format!("synth:{name}")),
                end: secs * u64::from(CLOCK_HZ),
                input: false,
            });
        }
    }
    for s in CORPUS {
        let mut roms = Vec::new();
        collect(&root.join(s.dir), s, &mut roms);
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
                    let h = fingerprint(&r.path, r.end, r.input, opts, false);
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
