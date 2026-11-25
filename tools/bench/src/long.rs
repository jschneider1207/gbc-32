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
//! and one frame per 70,224 clocks, every frame, from the first one on
//! (or, for a workload that turns the LCD off during its setup, from the
//! first one after its last LCD-off gap, which must come before its setup
//! deadline: [`Lcd`], [`FrameCheck`]).
//! Wall time is reported; it is the tool reading the host clock, not the
//! core.
//!
//! The workloads are ones that keep working for the whole run (most test
//! ROMs finish within seconds and then spin): the synthetic programs
//! (timer and serial storm in double speed, OAM DMA, HALT with interrupts,
//! all four APU channels with raster writes, palette raster with HDMA, APU
//! register writes, DMG-mode raster, OAM rewritten in every HBlank), cgb-acid2 (halted, STAT interrupts
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

/// How a workload's frames are checked ([`FrameCheck`]).
#[derive(Clone, Copy, Debug)]
enum Lcd {
    /// The LCD stays on from the first frame to the end of the run. It may
    /// be off before the first frame (setup with the LCD off, as
    /// `synth:dmg-raster` does), which must end its call by
    /// [`FIRST_FRAME_LIMIT`].
    OnFromStart,
    /// The workload turns the LCD off during its setup, so frames stop and
    /// later resume out of phase. A frame that ends its call more than
    /// [`FRAME_CLOCKS`] + [`OVERSHOOT`] clocks after the previous one (or
    /// after power-on: the first) is an LCD-off gap; at a gap the check
    /// restarts from that frame, which must end its call at or before
    /// `until` (inclusive; this replaces [`FIRST_FRAME_LIMIT`]). Every other
    /// frame, before the deadline too, and every frame after it, is checked
    /// as for [`Lcd::OnFromStart`]; a gap after `until`, or no frame by it,
    /// fails, and so does a run with a number of gaps other than `gaps`
    /// (the first frame is not counted: the core is deterministic, and the
    /// count is what was measured). `until` is the measured re-anchor clock
    /// + [`FRAME_CLOCKS`] / 2, so no further frame is due before it.
    SetupGaps { until: u64, gaps: u32 },
}

/// `synth:<name>` or a path under the test-ROM root, and how its frames
/// are checked.
///
/// The setup gaps and deadlines: measured over 30 minutes (2026-11-25,
/// `checked` profile; offsets after the last gap: cgb-acid2 0..0, cpu_instrs
/// -10..11, cgb_sound -4..16, rtc3test 0..0). `gaps` is the exact count of
/// LCD-off gaps, `until` the clock of the first frame after the last one
/// plus FRAME_CLOCKS / 2 (35,112), so no further frame is due before it:
/// cgb-acid2 no gap (it sets up with the LCD off from power-on), first
/// frame at 959,452, until 994,564; cpu_instrs one gap (278,224 clocks), at
/// 348,288, until 383,400; cgb_sound one (278,212), at 348,276, until
/// 383,388; rtc3test two (299,672, then 99,424), at 469,156, until 504,268.
const WORKLOADS: &[(&str, Lcd)] = &[
    ("synth:event-storm", Lcd::OnFromStart),
    ("synth:dma-storm", Lcd::OnFromStart),
    ("synth:halt-irq", Lcd::OnFromStart),
    ("synth:worst-case", Lcd::OnFromStart),
    ("synth:cgb-raster", Lcd::OnFromStart),
    ("synth:apu-writes", Lcd::OnFromStart),
    ("synth:dmg-raster", Lcd::OnFromStart),
    ("synth:oam-lines", Lcd::OnFromStart),
    (
        "cgb-acid2/cgb-acid2.gbc",
        Lcd::SetupGaps {
            until: 994_564,
            gaps: 0,
        },
    ),
    (
        "blargg/cpu_instrs/cpu_instrs.gb",
        Lcd::SetupGaps {
            until: 383_400,
            gaps: 1,
        },
    ),
    (
        "blargg/cgb_sound/cgb_sound.gb",
        Lcd::SetupGaps {
            until: 383_388,
            gaps: 1,
        },
    ),
    (
        "rtc3test/rtc3test.gb",
        Lcd::SetupGaps {
            until: 504_268,
            gaps: 2,
        },
    ),
];

struct Outcome {
    clocks: u64,
    frames: u64,
    samples: u64,
    wall: f64,
    /// The frame check's result ([`FrameCheck::end`]).
    frame_check: Checked,
}

/// How far after the frame boundary (line 144, dot 5) a run call that
/// reports `FRAME_READY` can end, in base clocks: 31 (single speed). The
/// core does not report the boundary's clock. The PPU's VBlank deadline
/// runs at the first M-cycle end at or after the dot (`Ctx::tick`,
/// `Ctx::boundary`; a clock skip, `Ctx::skip_clocks`, also ends on an
/// M-cycle end): up to 3 clocks later (1 in double speed). Its event ends
/// the call (`Ctx::raise` -> `Ctx::end_run`) only where the CPU looks at
/// `Bus::run_done`: after an instruction (`Cpu::run`), or after a checked
/// step that is no instruction (`Cpu::checked_fetch`). The longest run of
/// M-cycles with no look after the one that crossed the dot is in
/// `Cpu::step_not_running`, `Mode::Stopped`: STOP mode's wake-up idles
/// two M-cycles, then `Cpu::attend` goes on, with IME off, to the opcode
/// fetch and `Cpu::execute` of a 6 M-cycle instruction (`CALL`), so the
/// dot in the first idle leaves 1 + 6 M-cycles to run: 3 + 7 * 4 = 31.
/// The others are shorter: HALT's and the speed-switch halt's single
/// wake-up idle, then an instruction, 3 + 6 * 4 = 27 (a dispatch, 5
/// M-cycles, instead: 23); an instruction or a dispatch alone, 3 + 5 * 4
/// = 23; a halted skip (`Bus::idle_halted`) or a VRAM DMA stall stops at
/// the event's M-cycle (`vdma_turn`). In double speed: 1 + 7 * 2 = 15.
const OVERSHOOT: i64 = 31;

/// The tolerance of the check before this one (`|offset| <= 23` around
/// the first frame, from a bound that missed the wake-up paths of
/// [`OVERSHOOT`]). Kept so that no run that check rejected passes. It is
/// tighter than the hardware model: by the derivation at [`OVERSHOOT`],
/// offsets of 24 to 31 are legitimate (a STOP wake-up's idles and then a
/// `CALL`, or HALT's wake-up and a `CALL`), yet a frame with one is
/// rejected here. No current workload reaches them (seen: -4 to 16).
/// Relaxing this cap to [`OVERSHOOT`] needs the human's sign-off
/// (AGENTS.md: a check may not be loosened without it).
const ANCHOR_TOLERANCE: i64 = 23;

/// The latest clock the first frame may end its call at: within three
/// frames of power-on. Setup with the LCD off is allowed, a core that
/// loses the first frames is not (`synth:dmg-raster`, the latest, at
/// 194,844).
const FIRST_FRAME_LIMIT: u64 = 3 * FRAME_CLOCKS as u64;

/// A passed frame check: the clock after the call that reported frame 0
/// (the first, or the first after the last LCD-off gap), the lowest and
/// highest offset of a frame from its due clock, and the number of
/// LCD-off gaps ([`Lcd::SetupGaps`]; 0 otherwise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Checked {
    first: u64,
    lo: i64,
    hi: i64,
    gaps: u32,
}

/// The frame check: counted from the first completed frame (the LCD may
/// be off before it, up to [`FIRST_FRAME_LIMIT`]), or, for
/// [`Lcd::SetupGaps`], from the first after the last LCD-off gap (up to
/// its deadline). Frame `k`'s offset is `clock - (first + k *
/// FRAME_CLOCKS)`, `first` being frame 0's clock (offset 0); every
/// frame's call ends between the boundary and [`OVERSHOOT`] after it, so
/// the offsets span at most [`OVERSHOOT`] (frame 0's own overshoot
/// cancels). Each frame is checked as it arrives, against the first, so
/// a lost, extra or delayed frame fails at once and nothing can drift. At
/// the end, the run must not have reached the next frame's boundary:
/// frames stopping fails too.
struct FrameCheck {
    lcd: Lcd,
    /// The LCD-off gaps so far ([`Lcd::SetupGaps`]).
    gaps: u32,
    /// The clock after the call that reported frame 0.
    first: Option<u64>,
    /// The clock after the call that reported the latest frame.
    last: u64,
    frames: u64,
    /// The lowest and highest offset so far.
    lo: i64,
    hi: i64,
}

impl FrameCheck {
    const fn new(lcd: Lcd) -> Self {
        Self {
            lcd,
            gaps: 0,
            first: None,
            last: 0,
            frames: 0,
            lo: 0,
            hi: 0,
        }
    }

    /// The call that ended at `clock` reported `FRAME_READY`.
    fn frame(&mut self, clock: u64) -> Result<(), String> {
        if let Lcd::SetupGaps { until, .. } = self.lcd {
            // A frame later than any in-phase one can be (the previous
            // one's call ended at or after its boundary, this one's ends
            // at most OVERSHOOT after its own): frames stopped, the LCD
            // was off. The first frame counts as one, from clock 0.
            let gap = clock - self.last > u64::from(FRAME_CLOCKS) + OVERSHOOT as u64;
            if self.first.is_none() || gap {
                if clock > until {
                    let after = match self.first {
                        Some(_) => format!(
                            "{} clocks after the previous at {}",
                            clock - self.last,
                            self.last
                        ),
                        None => "with no frame before it".to_string(),
                    };
                    return Err(format!(
                        "frame at clock {clock}, {after}: an LCD-off gap (or the \
                         first frame) after the setup deadline {until} ({} gaps \
                         before)",
                        self.gaps
                    ));
                }
                if self.first.is_some() {
                    self.gaps += 1;
                }
                // Restart the check from this frame.
                (self.first, self.frames, self.lo, self.hi) = (Some(clock), 0, 0, 0);
            }
        }
        let first = match self.first {
            Some(f) => f,
            None if clock > FIRST_FRAME_LIMIT => {
                return Err(format!(
                    "first frame at clock {clock}, later than {FIRST_FRAME_LIMIT} \
                     (three frames)"
                ));
            }
            None => *self.first.insert(clock),
        };
        let due = first + self.frames * u64::from(FRAME_CLOCKS);
        // Both below 2^63 for any run length this tool takes.
        let off = clock as i64 - due as i64;
        let (lo, hi) = (self.lo.min(off), self.hi.max(off));
        if hi - lo > OVERSHOOT || off.abs() > ANCHOR_TOLERANCE {
            return Err(format!(
                "frame {} at clock {clock}, due at {due}: offset {off}, offsets \
                 now {lo}..{hi} (spread at most {OVERSHOOT}, each within \
                 \u{b1}{ANCHOR_TOLERANCE}; frame 0 at {first}, previous at {}, {} \
                 clocks before)",
                self.frames,
                self.last,
                clock - self.last
            ));
        }
        (self.lo, self.hi) = (lo, hi);
        self.last = clock;
        self.frames += 1;
        Ok(())
    }

    /// The run ended at `clock`.
    fn end(&self, clock: u64) -> Result<Checked, String> {
        let Some(first) = self.first else {
            return Err(format!("no frame in {clock} clocks"));
        };
        if let Lcd::SetupGaps { until, gaps } = self.lcd {
            // `frame` restarts only at or before `until`; stated as the
            // end-of-run invariant.
            if first > until {
                return Err(format!(
                    "frame 0 at clock {first}, after the setup deadline {until}"
                ));
            }
            // The core is deterministic: a dropped or shifted frame in the
            // setup (or an extra LCD-off gap) shows as a different count.
            if self.gaps != gaps {
                return Err(format!(
                    "{} LCD-off gaps, expected {gaps} (frame 0 at clock {first})",
                    self.gaps
                ));
            }
        }
        let span = u64::from(FRAME_CLOCKS);
        // Implied by `frame` (offsets within one `OVERSHOOT` of 0); stated
        // as the end-of-run invariant.
        let want = 1 + (self.last + OVERSHOOT as u64 - first) / span;
        if self.frames != want {
            return Err(format!(
                "{} frames from clock {first} to {}, expected {want}",
                self.frames, self.last
            ));
        }
        // Frame n's boundary is at or before `next` (frame 0's call ended
        // at or after its own). Had the run reached it, the M-cycle
        // ending there (the run ends on an M-cycle end) would have raised
        // frame n: a run that ends at or past `next` lost it.
        let next = first + self.frames * span;
        if clock >= next {
            return Err(format!(
                "frames stopped: {} frames, the last at clock {}; the run \
                 ended at {clock}, past frame {} (due by {next})",
                self.frames, self.last, self.frames
            ));
        }
        Ok(Checked {
            first,
            lo: self.lo,
            hi: self.hi,
            gaps: self.gaps,
        })
    }
}

/// Runs `src` for `end` clocks in frame-sized calls with an audio buffer,
/// as the MCU front end will. `Err`: the ROM did not load, or a rate
/// check failed.
fn run_one(src: &str, lcd: Lcd, root: &Path, end: u64) -> Result<Outcome, String> {
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
    let mut check = FrameCheck::new(lcd);
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
            check.frame(emu.clock())?;
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
    let frame_check = check.end(clocks)?;
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
            .map(|&(src, lcd)| s.spawn(move || run_one(src, lcd, root, end)))
            .collect();
        handles.into_iter().map(|h| h.join()).collect()
    });
    let mut failed = false;
    for (&(src, _), r) in WORKLOADS.iter().zip(results) {
        match r {
            Ok(Ok(o)) => println!(
                "ok    {src}: {} clocks, {} frames (from clock {}, every {FRAME_CLOCKS}, \
                 offsets {}..{}{}), {} samples, {:.1} s",
                o.clocks,
                o.frames,
                o.frame_check.first,
                o.frame_check.lo,
                o.frame_check.hi,
                match o.frame_check.gaps {
                    0 => String::new(),
                    n => format!(", after {n} LCD-off gaps"),
                },
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

#[cfg(test)]
mod tests {
    use super::*;

    const F: u64 = FRAME_CLOCKS as u64;

    /// `r` failed with a message that contains `needle`.
    #[track_caller]
    fn fails(r: Result<Checked, String>, needle: &str) {
        match r {
            Err(e) => assert!(e.contains(needle), "{e:?} lacks {needle:?}"),
            Ok(c) => panic!("passed: {c:?}"),
        }
    }

    /// Feeds `clocks` as frame calls, then ends the run at `end`.
    fn run(lcd: Lcd, clocks: &[u64], end: u64) -> Result<Checked, String> {
        let mut c = FrameCheck::new(lcd);
        for &k in clocks {
            c.frame(k)?;
        }
        c.end(end)
    }

    /// `n` in-phase frames from `first`, each `off(k)` from its due clock.
    fn frames(first: u64, n: u64, off: impl Fn(u64) -> u64) -> Vec<u64> {
        (0..n).map(|k| first + k * F + off(k)).collect()
    }

    #[test]
    fn lcd_on_from_start_passes_in_phase_and_fails_a_gap() {
        let ok = frames(70_060, 10, |k| if k % 3 == 0 { 0 } else { 16 });
        let r = run(Lcd::OnFromStart, &ok, 70_060 + 10 * F - 1).unwrap();
        assert_eq!((r.first, r.lo, r.hi, r.gaps), (70_060, 0, 16, 0));
        // One frame lost (an LCD-off gap): no setup gaps for this kind.
        let mut gap = ok.clone();
        gap.remove(5);
        fails(
            run(Lcd::OnFromStart, &gap, 70_060 + 10 * F - 1),
            "offset 70224",
        );
        // A first frame past FIRST_FRAME_LIMIT.
        let late = frames(FIRST_FRAME_LIMIT + 4, 3, |_| 0);
        fails(
            run(Lcd::OnFromStart, &late, FIRST_FRAME_LIMIT + 3 * F),
            "later than",
        );
    }

    #[test]
    fn setup_gaps_restart_before_the_deadline() {
        let lcd = Lcd::SetupGaps {
            until: 450_000,
            gaps: 1,
        };
        // The first frame may be past FIRST_FRAME_LIMIT; one gap, then in
        // phase from the frame after it.
        let mut seq = vec![70_064];
        seq.extend(frames(348_288, 20, |k| if k % 2 == 0 { 0 } else { 21 }));
        let end = 348_288 + 20 * F - 1;
        let r = run(lcd, &seq, end).unwrap();
        assert_eq!((r.first, r.lo, r.hi, r.gaps), (348_288, 0, 21, 1));
        let r = run(
            Lcd::SetupGaps {
                until: 1_200_000,
                gaps: 0,
            },
            &[959_452],
            959_452 + 1,
        )
        .unwrap();
        assert_eq!((r.first, r.gaps), (959_452, 0));
    }

    #[test]
    fn setup_gaps_fail_after_the_deadline() {
        let until = 450_000;
        let lcd = Lcd::SetupGaps { until, gaps: 1 };
        // The last re-anchor past the deadline.
        let mut seq = vec![70_064];
        seq.extend(frames(until + 4, 5, |_| 0));
        fails(run(lcd, &seq, until + 4 + 5 * F - 1), "an LCD-off gap");
        // The first frame past it.
        fails(
            run(lcd, &frames(until + 4, 5, |_| 0), until + 4 + 5 * F - 1),
            "an LCD-off gap",
        );
        // A gap long after it.
        let mut seq = frames(348_288, 20, |_| 0);
        seq.remove(15);
        fails(run(lcd, &seq, 348_288 + 20 * F - 1), "an LCD-off gap");
        // No frame at all.
        fails(run(lcd, &[], 10 * F), "no frame in");
    }

    #[test]
    fn setup_gaps_keep_the_standard_rules_between_gaps() {
        let lcd = Lcd::SetupGaps {
            until: 450_000,
            gaps: 0,
        };
        let anchor = 348_288;
        // A frame 24 late (past ANCHOR_TOLERANCE) is no gap: it fails,
        // before the deadline too.
        for at in [1, 15] {
            let seq = frames(anchor, 20, |k| if k >= at { 24 } else { 0 });
            fails(run(lcd, &seq, anchor + 20 * F + 23), "offset 24");
        }
        // Offsets -12 and 20: each within ANCHOR_TOLERANCE, spread 32,
        // past OVERSHOOT.
        let mut seq = frames(anchor, 20, |k| if k == 10 { 20 } else { 0 });
        seq[12] -= 12;
        fails(run(lcd, &seq, anchor + 20 * F - 1), "offsets now -12..20");
        // An extra frame between two.
        let mut seq = frames(anchor, 20, |_| 0);
        seq.insert(10, seq[9] + F / 2);
        fails(run(lcd, &seq, anchor + 20 * F - 1), "offset -35112");
        // Frames that stop at the end.
        let seq = frames(anchor, 20, |_| 0);
        fails(run(lcd, &seq, anchor + 20 * F), "frames stopped");
    }

    #[test]
    fn setup_deadline_is_inclusive() {
        let until = 450_000;
        let lcd = Lcd::SetupGaps { until, gaps: 0 };
        // The first frame at `until` passes; one clock later fails.
        let r = run(lcd, &[until], until + 1).unwrap();
        assert_eq!(r.first, until);
        fails(run(lcd, &[until + 1], until + 2), "setup deadline");
        // The same for a re-anchor after a gap.
        let lcd = Lcd::SetupGaps { until, gaps: 1 };
        let at = |a: u64| {
            let mut seq = vec![70_064];
            seq.extend(frames(a, 3, |_| 0));
            run(lcd, &seq, a + 3 * F - 1)
        };
        assert_eq!(at(until).unwrap().first, until);
        fails(at(until + 1), "setup deadline");
    }

    #[test]
    fn gap_threshold_is_the_overshoot_bound() {
        let lcd = Lcd::SetupGaps {
            until: 450_000,
            gaps: 0,
        };
        let anchor = 348_288;
        // Frame 1 8 early, frame 2 at offset 16: 24 after frame 1, past
        // F + 23 yet at most F + OVERSHOOT. Offsets -8..16 are within
        // tolerance and spread: checked, not an LCD-off gap.
        let seq = [anchor, anchor + F - 8, anchor + 2 * F + 16];
        let r = run(lcd, &seq, anchor + 3 * F - 1).unwrap();
        assert_eq!((r.first, r.lo, r.hi, r.gaps), (anchor, -8, 16, 0));
        // F + OVERSHOOT is the largest still no gap; one more is.
        let seq = [anchor, anchor + F - 15, anchor + 2 * F + 16];
        let r = run(lcd, &seq, anchor + 3 * F - 1).unwrap();
        assert_eq!((r.first, r.lo, r.hi, r.gaps), (anchor, -15, 16, 0));
        let lcd = Lcd::SetupGaps {
            until: 600_000,
            gaps: 1,
        };
        let seq = [anchor, anchor + F - 16, anchor + 2 * F + 16];
        let r = run(lcd, &seq, anchor + 3 * F - 1).unwrap();
        assert_eq!((r.first, r.gaps), (anchor + 2 * F + 16, 1));
    }

    #[test]
    fn setup_gap_count_must_match() {
        let anchor = 348_288;
        let mut seq = vec![70_064];
        seq.extend(frames(anchor, 5, |_| 0));
        let end = anchor + 5 * F - 1;
        let with = |gaps| {
            run(
                Lcd::SetupGaps {
                    until: 450_000,
                    gaps,
                },
                &seq,
                end,
            )
        };
        assert_eq!(with(1).unwrap().gaps, 1);
        fails(with(0), "1 LCD-off gaps, expected 0");
        fails(with(2), "1 LCD-off gaps, expected 2");
        // A message for a first frame past the deadline names no previous.
        fails(
            run(
                Lcd::SetupGaps {
                    until: 100,
                    gaps: 0,
                },
                &[200],
                201,
            ),
            "no frame before it",
        );
    }
}
