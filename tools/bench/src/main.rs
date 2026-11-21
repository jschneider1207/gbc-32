//! Runs one ROM headless for a fixed number of emulated clocks.
//!
//! Usage: `bench <rom | synth:<name>> <emulated-seconds>` (synthetic ROMs:
//! synth.rs), or `bench --fingerprint` (fingerprint.rs). It is meant to run
//! under
//! `valgrind --tool=callgrind --toggle-collect=bench::run_emulation`
//! (scripts/check-bench.sh), so only the emulation loop is counted: process
//! startup, ROM loading and setup are not. No clocks or randomness: the
//! same ROM and duration always execute the same instructions.

use std::hint::black_box;
use std::path::Path;
use std::process::ExitCode;

use gbc_core::{
    CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH,
    WHITE, cart_ram_size,
};

mod fingerprint;
mod synth;

/// `bench --fingerprint [--seconds N] [--root DIR]`.
fn fingerprint_main(args: &[String]) -> ExitCode {
    let mut secs = 3u64;
    let mut root = "test-roms".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = it.next();
        match (a.as_str(), v) {
            // At least 1 s: a shorter run (0 runs nothing) would make
            // nearly any two builds look identical.
            ("--seconds", Some(v)) if v.parse::<u64>().is_ok_and(|s| s >= 1) => {
                secs = v.parse().unwrap_or(3);
            }
            ("--root", Some(v)) => root = v.clone(),
            _ => {
                eprintln!("usage: bench --fingerprint [--seconds N (>= 1)] [--root DIR]");
                return ExitCode::from(2);
            }
        }
    }
    fingerprint::run(Path::new(&root), secs)
}

/// The measured region: callgrind collects only while this function is on
/// the stack (`inline(never)` keeps it a real symbol).
#[inline(never)]
pub fn run_emulation(emu: &mut Emulator<'_>, target: u64, frame: &mut Frame) -> u64 {
    let mut frames = 0u64;
    while emu.clock() < target {
        let r = emu.run_until(target, frame, &mut []);
        if r.events.contains(Events::FRAME_READY) {
            frames += 1;
        }
        if r.events.contains(Events::CPU_LOCKED) {
            break;
        }
    }
    black_box(frames)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "--fingerprint") {
        return fingerprint_main(&args[2..]);
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
    let frames = run_emulation(&mut emu, target, &mut frame);
    println!("{path}: {} clocks, {frames} frames", emu.clock());
    ExitCode::SUCCESS
}
