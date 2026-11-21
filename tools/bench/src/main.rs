//! Runs one ROM headless for a fixed number of emulated clocks.
//!
//! Usage: `bench <rom> <emulated-seconds>`. It is meant to run under
//! `valgrind --tool=callgrind --toggle-collect=bench::run_emulation`
//! (scripts/check-bench.sh), so only the emulation loop is counted: process
//! startup, ROM loading and setup are not. No clocks or randomness: the
//! same ROM and duration always execute the same instructions.

use std::hint::black_box;
use std::process::ExitCode;

use gbc_core::{
    CLOCK_HZ, Config, DmgPalettes, Emulator, Events, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH,
    WHITE, cart_ram_size,
};

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
    let [_, path, secs] = &args[..] else {
        eprintln!("usage: bench <rom> <emulated-seconds>");
        return ExitCode::from(2);
    };
    let Ok(secs) = secs.parse::<u64>() else {
        eprintln!("bad seconds: {secs}");
        return ExitCode::from(2);
    };
    let rom = match std::fs::read(path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{path}: {e}");
            return ExitCode::from(2);
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
