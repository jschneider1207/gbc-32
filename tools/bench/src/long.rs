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
//! and (for workloads that leave the LCD on once their setup is done) one
//! frame per 70,224 clocks, every frame, from the first one on
//! ([`FrameCheck`]).
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
/// stays on from the first frame to the end of the run (then the frame
/// rate is checked, [`FrameCheck`]). The LCD may be off before the first
/// frame (setup with the LCD off, as `synth:dmg-raster` does).
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
    /// For a workload whose frames are checked: the clock after the call
    /// that reported the first `FRAME_READY`, and the largest deviation
    /// of a later frame from its due clock.
    frame_check: Option<(u64, u64)>,
}

/// How far after the frame boundary a run call that reports `FRAME_READY`
/// can end, in base clocks. The core does not report the boundary's
/// clock: the PPU raises the event when the M-cycle that reaches VBlank
/// (line 144, dot 5) ends, at most 3 clocks after that dot, and the event
/// ends the call after the current step (`Ctx::raise`, `Ctx::end_run`:
/// "the run call ends after the current instruction"), i.e. after at most
/// 5 more M-cycles of the longest step (a 6 M-cycle instruction, `CALL`;
/// an interrupt dispatch is 5; an idle or a VRAM DMA stall M-cycle is 1).
/// 3 + 5 * 4 = 23 in single speed, less in double speed.
const FRAME_JITTER: u64 = 23;

/// The frame check of a workload that keeps the LCD on: counted from the
/// first completed frame (the LCD may be off before it), frame `k` must
/// end its call at `first + k * FRAME_CLOCKS`, within [`FRAME_JITTER`]
/// either way (`first` itself is up to that late). Each frame is checked
/// as it arrives, against the first, so a lost, extra or delayed frame
/// fails at once and nothing can drift. At the end, the run must not have
/// gone past the next frame's due clock: frames stopping fails too.
struct FrameCheck {
    /// The clock after the call that reported the first frame.
    first: Option<u64>,
    /// The clock after the call that reported the latest frame.
    last: u64,
    frames: u64,
    max_dev: u64,
}

impl FrameCheck {
    const fn new() -> Self {
        Self {
            first: None,
            last: 0,
            frames: 0,
            max_dev: 0,
        }
    }

    /// The call that ended at `clock` reported `FRAME_READY`.
    fn frame(&mut self, clock: u64) -> Result<(), String> {
        let first = *self.first.get_or_insert(clock);
        let due = first + self.frames * u64::from(FRAME_CLOCKS);
        let dev = clock.abs_diff(due);
        if dev > FRAME_JITTER {
            return Err(format!(
                "frame {} at clock {clock}, due at {due} (frame 0 at {first}, \
                 previous at {}, {} clocks before; tolerance {FRAME_JITTER})",
                self.frames,
                self.last,
                clock - self.last
            ));
        }
        self.max_dev = self.max_dev.max(dev);
        self.last = clock;
        self.frames += 1;
        Ok(())
    }

    /// The run ended at `clock`.
    fn end(&self, clock: u64) -> Result<(u64, u64), String> {
        let Some(first) = self.first else {
            return Err(format!("no frame in {clock} clocks"));
        };
        let span = u64::from(FRAME_CLOCKS);
        // Implied by `frame`; stated as the end-of-run invariant.
        let want = 1 + (self.last + FRAME_JITTER - first) / span;
        if self.frames != want {
            return Err(format!(
                "{} frames from clock {first} to {}, expected {want}",
                self.frames, self.last
            ));
        }
        let next = first + self.frames * span;
        if clock > next + FRAME_JITTER {
            return Err(format!(
                "frames stopped: {} frames, the last at clock {}; the run \
                 ended at {clock}, past frame {} (due at {next})",
                self.frames, self.last, self.frames
            ));
        }
        Ok((first, self.max_dev))
    }
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
    let mut check = FrameCheck::new();
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
        if r.events.contains(Events::FRAME_READY) {
            frames += 1;
            if lcd_on {
                check.frame(emu.clock())?;
            }
        }
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
    let frame_check = if lcd_on {
        Some(check.end(clocks)?)
    } else {
        None
    };
    Ok(Outcome {
        clocks,
        frames,
        samples,
        wall,
        frame_check,
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
                "ok    {src}: {} clocks, {} frames{}, {} samples, {:.1} s",
                o.clocks,
                o.frames,
                o.frame_check.map_or(String::new(), |(first, dev)| format!(
                    " (from clock {first}, every {FRAME_CLOCKS} \u{b1}{dev})"
                )),
                o.samples,
                o.wall
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
