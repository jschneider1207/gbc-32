//! The panel in the loop as a measurement (plan Stage 2 M3, the
//! `display` feature; moved from src/bin/headless.rs, which only selects
//! it): a [`FrameSink`] that shows every completed frame and checks the
//! previous one by reading it back. The `headless` lines are those of a
//! run without it (the sink writes no line, `gbc_firmware::headless`
//! "Board hook"); its figures go on `stats display` lines after the run
//! (not compared; run-headless.sh checks them).
//!
//! Unlike the real-time [`Display`](super::Display), it waits: per
//! completed frame `n`, outside the core's cycle count:
//! 1. wait for frame `n - 1`'s transfer to finish, then read the window
//!    back and compare it with the buffer, which still holds `n - 1`
//!    ([`lcd_dma::readback`], ~90 ms);
//! 2. `scale_frame` frame `n` into the buffer (timed);
//! 3. the sentinel (M3 review T-2): the bitwise inverse of frame `n`
//!    written into the window by the CPU ([`lcd_dma::write_inverse`],
//!    ~12 ms, timed separately: self-test overhead, not part of the 5%
//!    budget), so every pixel the transfer of `n` fails to write reads
//!    back as a mismatch, even when frame `n` equals frame `n - 1`;
//! 4. queue frame `n` (`present`, timed: window commands and the MDMA
//!    registers);
//! 5. depending on [`Start`]: wait until TE has started it, start it at
//!    once, or return with it queued.
//!
//! So with `te-wait` and `now` the transfer of frame `n` (8.1 ms) runs
//! while the core emulates frame `n + 1`, for every frame (the contention
//! measurement); with `te` the overlap depends on where TE falls. After
//! the run the last transfer is read back too, then the control
//! ([`control_readback`](super::control_readback): exactly 2 mismatching
//! pixels).
//!
//! Panel set-up, the control, [`Range`] and the `first_bad` /
//! `first_error` fields are the real-time sink's (src/display.rs); the
//! image defines the TE interrupt (`EXTI15_10`, src/lib.rs table).
//!
//! [`FrameSink`]: gbc_firmware::headless::FrameSink

use core::fmt::{self, Write};
use core::hint::black_box;

use super::{BadPixel, Range, control_readback, panel, write_first_bad, write_first_error};
use crate::lcd_dma::{self, LcdDma};
use crate::{CPU_HZ, cycles};
use gbc_core::{FRAME_CLOCKS, Frame};
use gbc_firmware::headless::FrameSink;
use gbc_firmware::video::scale_frame;

/// What starts a queued transfer (`GBC_DISPLAY_START`, build.rs).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// No display: the sink returns at once (the panel is set up and
    /// black). The same code as the other modes, for a same-build
    /// baseline of the core's cycles.
    Off,
    /// TE, and the sink waits until it has started (ADR 0039's start,
    /// with the overlap made certain).
    TeWait,
    /// TE; the sink returns with the transfer queued.
    Te,
    /// At once, from the sink (`LcdDma::start_now`; may tear).
    Now,
}

impl Start {
    const NAMES: [(&str, Start); 4] = [
        ("off", Start::Off),
        ("te-wait", Start::TeWait),
        ("te", Start::Te),
        ("now", Start::Now),
    ];

    pub fn name(self) -> &'static str {
        match self {
            Start::Off => "off",
            Start::TeWait => "te-wait",
            Start::Te => "te",
            Start::Now => "now",
        }
    }
}

const fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

const fn code(s: &str) -> u8 {
    let mut i = 0;
    while i < Start::NAMES.len() {
        if same(s, Start::NAMES[i].0) {
            return i as u8;
        }
        i += 1;
    }
    panic!("GBC_DISPLAY_START: not off, te-wait, te or now");
}

/// The mode's index in [`Start::NAMES`]. Read through `black_box`, so
/// the compiler cannot specialise the code on it: images that differ
/// only in the mode have the same code (one constant differs).
const START_CODE: u8 = code(env!("GBC_DISPLAY_START"));

fn start() -> Start {
    match black_box(START_CODE) {
        0 => Start::Off,
        1 => Start::TeWait,
        2 => Start::Te,
        _ => Start::Now,
    }
}

/// Any wait here is shorter: 50 ms (a TE period plus a transfer is
/// 25 ms).
const TIMEOUT: u32 = CPU_HZ / 20;

fn range(out: &mut dyn Write, name: &str, r: &Range) -> fmt::Result {
    write!(
        out,
        " {name}_min={} {name}_mean={} {name}_max={}",
        r.min,
        r.mean(),
        r.max
    )
}

fn us(c: u64) -> u64 {
    c * 1_000_000 / u64::from(CPU_HZ)
}

pub struct Measure {
    dma: LcdDma,
    start: Start,
    /// The frame whose transfer is queued or running and not yet read
    /// back.
    queued: Option<u32>,
    /// A wait timed out: the display stops (the channel's state is
    /// unknown, so nothing may touch the FMC again).
    dead: bool,
    frames: u32,
    transfers: u32,
    readbacks: u32,
    mismatches: u32,
    /// The first mismatching pixel.
    first_bad: Option<(u32, BadPixel)>,
    errors: u32,
    first_error: Option<(u32, lcd_dma::Error)>,
    start_timeouts: u32,
    /// Transfers already finished when the sink first looked (their
    /// `start_to_done` is then only an upper bound, so not in `mdma`).
    late: u32,
    scale: Range,
    setup: Range,
    mdma: Range,
    te_to_start: Range,
    /// The wait for the previous transfer (TE included).
    wait: Range,
    /// `te-wait`: from the end of `present` until TE started it.
    to_start: Range,
    readback: Range,
    /// The sentinel's CPU writes ([`lcd_dma::write_inverse`]).
    sentinel: Range,
}

/// Sets the panel up as the real-time sink does ([`panel`]: FMC at
/// `SPEC`, the Game Boy frame rate, all black) and the transfer (ADR
/// 0039), with the start mode the image was built with.
pub fn init() -> Measure {
    Measure {
        dma: panel(),
        start: start(),
        queued: None,
        dead: false,
        frames: 0,
        transfers: 0,
        readbacks: 0,
        mismatches: 0,
        first_bad: None,
        errors: 0,
        first_error: None,
        start_timeouts: 0,
        late: 0,
        scale: Range::new(),
        setup: Range::new(),
        mdma: Range::new(),
        te_to_start: Range::new(),
        wait: Range::new(),
        to_start: Range::new(),
        readback: Range::new(),
        sentinel: Range::new(),
    }
}

impl Measure {
    /// Waits for the queued transfer and reads it back.
    fn finish(&mut self) {
        let Some(n) = self.queued.take() else {
            return;
        };
        let t0 = cycles();
        let r = match self.dma.try_wait() {
            Some(r) => {
                if matches!(r, Ok(Some(_))) {
                    self.late += 1;
                }
                r
            }
            None => match self.dma.wait(TIMEOUT) {
                Ok(Some(t)) => {
                    self.mdma.add(t.start_to_done);
                    Ok(Some(t))
                }
                r => r,
            },
        };
        self.wait.add(cycles().wrapping_sub(t0));
        match r {
            Ok(Some(t)) => {
                self.transfers += 1;
                self.te_to_start.add(t.te_to_start);
            }
            Ok(None) => {}
            Err(e) => {
                self.errors += 1;
                self.first_error.get_or_insert((n, e));
                if matches!(e, lcd_dma::Error::Timeout { .. }) {
                    self.dead = true;
                    return;
                }
            }
        }
        let t0 = cycles();
        let (l, buf) = self.dma.parts();
        let rb = lcd_dma::readback(l, buf);
        self.readback.add(cycles().wrapping_sub(t0));
        self.readbacks += 1;
        self.mismatches += rb.mismatches;
        if let (None, Some(p)) = (self.first_bad, rb.first) {
            self.first_bad = Some((n, p));
        }
    }

    /// After the run: the last transfer read back, the control, and
    /// the `stats display` lines. `run_frame`: the core's (mean, max)
    /// cycles from the run's summary.
    pub fn report(&mut self, out: &mut dyn Write, run_frame: (u64, u32)) -> fmt::Result {
        let mut control = None;
        if self.start != Start::Off && !self.dead {
            self.finish();
            if !self.dead && self.readbacks > 0 {
                // The GRAM holds the last frame.
                control = Some(control_readback(&mut self.dma));
            }
        }
        let te = lcd_dma::te_stats();
        write!(
            out,
            "stats display start={} frames={} transfers={} readbacks={} mismatches={} \
             control_mismatches=",
            self.start.name(),
            self.frames,
            self.transfers,
            self.readbacks,
            self.mismatches,
        )?;
        match control {
            Some(c) => write!(out, "{c}")?,
            None => write!(out, "none")?,
        }
        write!(
            out,
            " transfer_errors={} start_timeouts={} dead={} late={} te_edges={} \
             te_min={} te_max={}",
            self.errors,
            self.start_timeouts,
            u8::from(self.dead),
            self.late,
            te.edges,
            te.min,
            te.max,
        )?;
        write_first_bad(out, self.first_bad)?;
        write_first_error(out, self.first_error)?;
        writeln!(out)?;

        // The CPU's share: conversion plus queueing, against 5% of a
        // real-time frame at CPU_HZ (plan M3 acceptance).
        let frame = u64::from(CPU_HZ) * u64::from(FRAME_CLOCKS) / u64::from(gbc_core::CLOCK_HZ);
        let budget = frame / 20;
        let worst = u64::from(self.scale.max) + u64::from(self.setup.max);
        write!(out, "stats display cpu")?;
        range(out, "scale", &self.scale)?;
        range(out, "setup", &self.setup)?;
        writeln!(
            out,
            " worst_sum={worst} budget={budget} ok={}",
            u8::from(self.scale.n > 0 && worst <= budget)
        )?;

        write!(out, "stats display dma")?;
        range(out, "mdma", &self.mdma)?;
        write!(out, " mdma_max_us={}", us(u64::from(self.mdma.max)))?;
        range(out, "te_to_start", &self.te_to_start)?;
        writeln!(out)?;

        write!(out, "stats display waits")?;
        range(out, "wait", &self.wait)?;
        range(out, "to_start", &self.to_start)?;
        range(out, "readback", &self.readback)?;
        range(out, "sentinel", &self.sentinel)?;
        writeln!(out)?;

        writeln!(
            out,
            "stats display core start={} run_frame_mean={} run_frame_max={}",
            self.start.name(),
            run_frame.0,
            run_frame.1
        )
    }
}

impl FrameSink for Measure {
    fn frame(&mut self, n: u32, frame: &Frame) {
        if self.start == Start::Off || self.dead {
            return;
        }
        self.frames += 1;
        self.finish();
        if self.dead {
            return;
        }
        let t0 = cycles();
        scale_frame(frame, self.dma.buffer());
        let t1 = cycles();
        // The sentinel (T-2): `finish` left no transfer running.
        let (l, buf) = self.dma.parts();
        lcd_dma::write_inverse(l, buf);
        let ts = cycles();
        self.dma.present();
        if self.start == Start::Now {
            self.dma.start_now();
        }
        let t2 = cycles();
        self.scale.add(t1.wrapping_sub(t0));
        self.sentinel.add(ts.wrapping_sub(t1));
        self.setup.add(t2.wrapping_sub(ts));
        self.queued = Some(n);
        if self.start == Start::TeWait {
            if !self.dma.wait_started(TIMEOUT) {
                self.start_timeouts += 1;
            }
            self.to_start.add(cycles().wrapping_sub(t2));
        }
    }
}
