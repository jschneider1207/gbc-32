//! Runs one ROM headless for a fixed number of emulated clocks.
//!
//! Wall-clock mode: `bench --wall` (wall.rs). Long run: `bench --long`
//! (long.rs). `bench --rustflags` prints the rustc flags it was built with
//! (build.rs; checked by scripts/check-bench.sh, docs/perf.md "Build flags").
//!
//! Usage: `bench <rom | synth:<name>> <emulated-seconds>` (synthetic ROMs:
//! synth.rs), or `bench --fingerprint` (fingerprint.rs). `bench --write-synth
//! <name> <out.gb>` writes a synthetic ROM to a file (the board's
//! `paced-<name>` images embed it, and the host's `headless --paced` runs
//! the same file). It is meant to run
//! under
//! `valgrind --tool=callgrind --toggle-collect=bench::run_emulation`
//! (scripts/check-bench.sh), so only the emulation loop is counted: process
//! startup, ROM loading and setup are not. No clocks or randomness: the
//! same ROM and duration always execute the same instructions.

use std::hint::black_box;
use std::path::Path;
use std::process::ExitCode;

use gbc_core::{
    CLOCK_HZ, Config, DmgPalettes, Emulator, Events, FRAME_CLOCKS, Frame, Memory, SCREEN_HEIGHT,
    SCREEN_WIDTH, StereoSample, WHITE, cart_ram_size,
};

mod fingerprint;
mod long;
mod synth;
mod wall;

/// Audio buffer slots per call: one frame is 548.6 samples (70,224 / 128
/// clocks), so a frame-sized call never fills it and `AUDIO_BUFFER_FULL`
/// never splits a call.
const AUDIO_SLOTS: usize = 1024;

/// `bench --fingerprint [--seconds N] [--root DIR] [--audio-buffer N
/// [--hash-samples]] [--clock-offset N] [--trace RUN]`.
fn fingerprint_main(args: &[String]) -> ExitCode {
    let mut secs = 3u64;
    let mut root = "test-roms".to_string();
    let mut opts = fingerprint::Opts::default();
    let mut trace = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--hash-samples" {
            opts.hash_samples = true;
            continue;
        }
        let v = it.next();
        match (a.as_str(), v) {
            // At least 1 s: a shorter run (0 runs nothing) would make
            // nearly any two builds look identical.
            ("--seconds", Some(v)) if v.parse::<u64>().is_ok_and(|s| s >= 1) => {
                secs = v.parse().unwrap_or(3);
            }
            ("--root", Some(v)) => root = v.clone(),
            // Per-call audio buffers of N samples (the hash must not change:
            // ADR 0021 section 3, fingerprint.rs).
            ("--audio-buffer", Some(v)) if v.parse::<usize>().is_ok_and(|n| n >= 1) => {
                opts.audio = v.parse().unwrap_or(1);
            }
            // Power on with the clock at N (a multiple of 4; needs the
            // `clock-offset` feature). The lines must not change
            // (docs/perf.md "Clock-offset invariance").
            ("--clock-offset", Some(v)) if v.parse::<u64>().is_ok_and(|n| n % 4 == 0) => {
                opts.clock_offset = v.parse().unwrap_or(0);
            }
            // One run only, a line per chunk (to locate a difference).
            ("--trace", Some(v)) => trace = Some(v.clone()),
            _ => {
                eprintln!(
                    "usage: bench --fingerprint [--seconds N (>= 1)] [--root DIR] \
                     [--audio-buffer N (>= 1) [--hash-samples]] \
                     [--clock-offset N (multiple of 4)] [--trace RUN]"
                );
                return ExitCode::from(2);
            }
        }
    }
    if opts.hash_samples && opts.audio == 0 {
        eprintln!("bench: --hash-samples needs --audio-buffer N");
        return ExitCode::from(2);
    }
    if opts.clock_offset != 0 && !fingerprint::HAS_CLOCK_OFFSET {
        eprintln!("bench: --clock-offset needs a build with --features clock-offset");
        return ExitCode::from(2);
    }
    match trace {
        Some(run) => fingerprint::trace(Path::new(&root), secs, opts, &run),
        None => fingerprint::run(Path::new(&root), secs, opts),
    }
}

/// The measured region: callgrind collects only while this function is on
/// the stack (`inline(never)` keeps it a real symbol).
///
/// Calls are made as the MCU front end will make them: at most one frame
/// ([`FRAME_CLOCKS`]) per call, each with an [`AUDIO_SLOTS`]-sample audio
/// buffer whose samples are consumed (`black_box`) after the call.
#[inline(never)]
pub fn run_emulation(
    emu: &mut Emulator<'_>,
    target: u64,
    frame: &mut Frame,
    audio: &mut [StereoSample],
) -> u64 {
    let mut frames = 0u64;
    while emu.clock() < target {
        let to = target.min(emu.clock() + u64::from(FRAME_CLOCKS));
        let r = emu.run_until(to, frame, audio);
        black_box(&audio[..r.audio_samples]);
        if r.events.contains(Events::FRAME_READY) {
            frames += 1;
        }
        if r.events.contains(Events::CPU_LOCKED) {
            break;
        }
    }
    black_box(frames)
}

/// `bench --write-synth <name> <out.gb>`.
fn write_synth(args: &[String]) -> ExitCode {
    let [name, out] = args else {
        eprintln!("usage: bench --write-synth <name> <out.gb>");
        return ExitCode::from(2);
    };
    let Some(rom) = synth::build(name) else {
        eprintln!(
            "unknown synthetic ROM {name} (have: {})",
            synth::NAMES.join(", ")
        );
        return ExitCode::from(2);
    };
    match std::fs::write(out, rom) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{out}: {e}");
            ExitCode::from(1)
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--wall") {
        return wall::main(&args[2..]);
    }
    if args.get(1).is_some_and(|a| a == "--long") {
        return long::main(&args[2..]);
    }
    if args.get(1).is_some_and(|a| a == "--rustflags") {
        println!("{}", env!("BENCH_RUSTFLAGS"));
        return ExitCode::SUCCESS;
    }
    if args.get(1).is_some_and(|a| a == "--fingerprint") {
        return fingerprint_main(&args[2..]);
    }
    if args.get(1).is_some_and(|a| a == "--write-synth") {
        return write_synth(&args[2..]);
    }
    let [_, path, secs] = &args[..] else {
        eprintln!("usage: bench <rom | synth:<name>> <emulated-seconds>");
        return ExitCode::from(2);
    };
    let Ok(secs) = secs.parse::<u64>() else {
        eprintln!("bad seconds: {secs}");
        return ExitCode::from(2);
    };
    let rom = if let Some(name) = path.strip_prefix("synth:") {
        match synth::build(name) {
            Some(r) => r,
            None => {
                eprintln!(
                    "unknown synthetic ROM {name} (have: {})",
                    synth::NAMES.join(", ")
                );
                return ExitCode::from(2);
            }
        }
    } else {
        match std::fs::read(path) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{path}: {e}");
                return ExitCode::from(2);
            }
        }
    };
    let ram_size = match cart_ram_size(&rom) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{path}: {e:?}");
            return ExitCode::from(2);
        }
    };
    let mut cart_ram = vec![0u8; ram_size];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: DmgPalettes::default(),
    };
    if let Err(e) = emu.load(&rom, &mut cart_ram, config) {
        eprintln!("{path}: {e}");
        return ExitCode::from(2);
    }
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");
    let target = secs * u64::from(CLOCK_HZ);
    let mut audio = vec![StereoSample::default(); AUDIO_SLOTS];
    let frames = run_emulation(&mut emu, target, &mut frame, &mut audio);
    println!("{path}: {} clocks, {frames} frames", emu.clock());
    ExitCode::SUCCESS
}
