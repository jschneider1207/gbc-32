//! Long run: `bench --long [--minutes N] [--root DIR]` (docs/perf.md, "Long
//! run").
//!
//! Runs a few workloads from power-on for N emulated minutes (default 30),
//! past 2^32 base clocks (17 min 4 s), where every 32-bit clock difference
//! in the core has wrapped at least once. Meant for the `checked` profile
//! (`scripts/check-sync-invariance.sh --long`): integer-overflow checks and
//! `debug_assert!`s on, so an arithmetic slip panics instead of wrapping.
//!
//! The property: no panic, the run reaches its end, and the two rates that
//! must hold over any length do: one audio sample per 128 clocks exactly,
//! and (for workloads that leave the LCD on) one frame per 70,224 clocks.
//! Wall time is reported; it is the tool reading the host clock, not the
//! core.
//!
//! The workloads are ones that keep working for the whole run (most test
//! ROMs finish within seconds and then spin): the synthetic programs
//! (timer and serial storm in double speed, OAM DMA, HALT with interrupts,
//! all four APU channels with raster writes, palette raster with HDMA, APU
//! register writes, DMG-mode raster), cgb-acid2 (halted, STAT interrupts
//! every frame), cpu_instrs, cgb_sound and the MBC3 RTC test (the RTC
//! counts 30 minutes).

use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use gbc_core::{
    AUDIO_SAMPLE_CLOCKS, CLOCK_HZ, Config, DmgPalettes, Emulator, Events, FRAME_CLOCKS, Frame,
    Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE, cart_ram_size,
};

use crate::{AUDIO_SLOTS, synth};

/// `synth:<name>` or a path under the test-ROM root, and whether the LCD
/// stays on for the whole run (then the frame rate is checked).
const WORKLOADS: &[(&str, bool)] = &[
    ("synth:event-storm", true),
    ("synth:dma-storm", true),
    ("synth:halt-irq", true),
    ("synth:worst-case", true),
    ("synth:cgb-raster", true),
    ("synth:apu-writes", true),
    ("synth:dmg-raster", true),
    // (It turns the LCD off while it sets up.)
    ("cgb-acid2/cgb-acid2.gbc", false),
    ("blargg/cpu_instrs/cpu_instrs.gb", false),
    ("blargg/cgb_sound/cgb_sound.gb", false),
    ("rtc3test/rtc3test.gb", false),
];

struct Outcome {
    clocks: u64,
    frames: u64,
    samples: u64,
    wall: f64,
}

/// Runs `src` for `end` clocks in frame-sized calls with an audio buffer,
/// as the MCU front end will. `Err`: the ROM did not load, or a rate
/// check failed.
fn run_one(src: &str, lcd_on: bool, root: &Path, end: u64) -> Result<Outcome, String> {
    let rom = match src.strip_prefix("synth:") {
        Some(name) => synth::build(name).ok_or("unknown synthetic ROM")?,
        None => std::fs::read(root.join(src)).map_err(|e| e.to_string())?,
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
    let mut frame: Box<Frame> = vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size");
    let mut audio = vec![StereoSample::default(); AUDIO_SLOTS];
    let (mut frames, mut samples) = (0u64, 0u64);
    let t0 = Instant::now();
    while emu.clock() < end {
        let before = emu.clock();
        let to = end.min(before + u64::from(FRAME_CLOCKS));
        let r = emu.run_until(to, &mut frame, &mut audio);
        if emu.clock() != before + r.clocks || r.clocks == 0 {
            return Err(format!(
                "clock {before} + {} clocks gave {}",
                r.clocks,
                emu.clock()
            ));
        }
        samples += r.audio_samples as u64;
        frames += u64::from(r.events.contains(Events::FRAME_READY));
    }
    let wall = t0.elapsed().as_secs_f64();
    let clocks = emu.clock();
    // The buffer never fills (`AUDIO_SLOTS`), so the window grid runs from
    // clock 0 unbroken: one sample per whole window.
    let want = clocks / u64::from(AUDIO_SAMPLE_CLOCKS);
    if samples != want {
        return Err(format!(
            "{samples} samples in {clocks} clocks, expected {want}"
        ));
    }
    // LCD on throughout: frames complete every `FRAME_CLOCKS` from the
    // post-boot position on, so the count is within one of the quotient.
    let want = clocks / u64::from(FRAME_CLOCKS);
    if lcd_on && frames.abs_diff(want) > 1 {
        return Err(format!(
            "{frames} frames in {clocks} clocks, expected {want}"
        ));
    }
    Ok(Outcome {
        clocks,
        frames,
        samples,
        wall,
    })
}

/// `bench --long [--minutes N] [--root DIR]`.
pub fn main(args: &[String]) -> ExitCode {
    let mut minutes = 30u64;
    let mut root = "test-roms".to_string();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match (a.as_str(), it.next()) {
            ("--minutes", Some(v)) if v.parse::<u64>().is_ok_and(|m| m >= 1) => {
                minutes = v.parse().unwrap_or(30);
            }
            ("--root", Some(v)) => root = v.clone(),
            _ => {
                eprintln!("usage: bench --long [--minutes N (>= 1)] [--root DIR]");
                return ExitCode::from(2);
            }
        }
    }
    let end = minutes * 60 * u64::from(CLOCK_HZ);
    let root = Path::new(&root);
    let t0 = Instant::now();
    // One thread per workload: a panic (an overflow check, a debug
    // assertion) is that thread's result, and the others still finish.
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = WORKLOADS
            .iter()
            .map(|&(src, lcd_on)| s.spawn(move || run_one(src, lcd_on, root, end)))
            .collect();
        handles.into_iter().map(|h| h.join()).collect()
    });
    let mut failed = false;
    for (&(src, _), r) in WORKLOADS.iter().zip(results) {
        match r {
            Ok(Ok(o)) => println!(
                "ok    {src}: {} clocks, {} frames, {} samples, {:.1} s",
                o.clocks, o.frames, o.samples, o.wall
            ),
            Ok(Err(e)) => {
                failed = true;
                println!("FAIL  {src}: {e}");
            }
            Err(_) => {
                failed = true;
                println!("PANIC {src} (message above)");
            }
        }
    }
    println!(
        "bench --long: {minutes} emulated minutes ({end} clocks{}) x {} workloads, {:.1} s wall",
        if end > 1 << 32 { ", past 2^32" } else { "" },
        WORKLOADS.len(),
        t0.elapsed().as_secs_f64()
    );
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
