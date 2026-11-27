//! The panel as the paced loop's [`FrameSink`] (through [`SinkRef`]):
//! the newest completed frame goes to the panel at each TE; never waits
//! (src/bin/paced.rs docs, "Wiring"; ADR 0039 amendment, M4 review P-7).
//!
//! [`FrameSink`]: gbc_firmware::headless::FrameSink
//!
//! Shared by the `paced` and `player` images (moved from src/bin/paced.rs).
//!
//! # Readback samples (`player-script` only)
//!
//! The scripted player checks its own panel path (plan Stage 2 B3: "the
//! display readback has 0 mismatches"): a full RAMRD readback takes
//! 90 ms, five frames, so it cannot follow every frame of a real-time
//! loop. Instead, by a fixed rule independent of the ROM: the first frame
//! shown at or after loop frame [`SAMPLE_FIRST`], and then the first one
//! at least [`SAMPLE_EVERY`] frames after the previous sample started, is
//! a **sample**. It is scaled into the buffer as usual, then, one step
//! per loop iteration and only while the loop is ahead of the DAC (the
//! idle hook with the ring full, [`Display::sample_step`]):
//!
//! 1. the sentinel ([`lcd_dma::write_inverse_rows`], [`SENTINEL_ROWS`]
//!    rows a step, 8 steps): the inverse of the frame written into the
//!    window by the CPU, so every pixel its transfer fails to write reads
//!    back wrong (as the headless `display` feature, M3 review T-2);
//! 2. the transfer queued for TE, as any frame, and waited for (polled);
//! 3. the readback ([`lcd_dma::readback_rows`], [`READBACK_ROWS`] rows a
//!    step, 54 steps) against the buffer, which still holds the frame.
//!
//! Meanwhile the newer frames wait in the stash as during a transfer
//! (the newest is shown once the sample ends; the others are skipped).
//! Frame and feed hashes and the core's cycles are untouched (the sink
//! only reads the frame); a step's cost (about 0.8 M cycles) is awake
//! time, so it counts in the player's `busy`/`fe` and over-budget figures.
//! After the run a sample still under way is finished, and a control
//! readback with one buffer word inverted must give exactly 2
//! mismatching pixels. Line: `stats display readback`.
//!
//! # The measurement sink (`display` feature)
//!
//! The `headless-display-<rom>` images' sink, `measure::Measure` (plan
//! Stage 2 M3), waits for and reads back every frame instead; it shares
//! the panel set-up, the control readback, [`Range`] and the
//! `first_bad` / `first_error` fields with this one.

#[cfg(feature = "display")]
pub mod measure;

use core::cell::RefCell;
use core::fmt::{self, Write};

use crate::lcd::{self, FrameRate, Lcd};
use crate::lcd_dma::{self, LcdDma};
use crate::{CPU_HZ, cycles};
use gbc_core::Frame;
use gbc_firmware::headless::FrameSink;
#[cfg(feature = "player-script")]
use gbc_firmware::video::OUT_H;
use gbc_firmware::video::scale_frame;

/// A transfer still pending or running this long after `present` is
/// abandoned (100 ms: TE comes every 16.7 ms and a transfer takes
/// 8.1 ms, so a healthy one is done within 25 ms).
pub const STUCK: u32 = CPU_HZ / 10;

/// Count, min, max and sum of a series of cycle counts (both sinks'
/// figures; `min` is printed by `measure` only).
#[derive(Clone, Copy, Default)]
pub struct Range {
    pub n: u32,
    pub min: u32,
    pub max: u32,
    pub sum: u64,
}

impl Range {
    pub const fn new() -> Range {
        Range {
            n: 0,
            min: 0,
            max: 0,
            sum: 0,
        }
    }
    pub fn add(&mut self, v: u32) {
        self.min = if self.n == 0 { v } else { self.min.min(v) };
        self.max = self.max.max(v);
        self.sum += u64::from(v);
        self.n += 1;
    }
    pub fn mean(&self) -> u64 {
        self.sum / u64::from(self.n.max(1))
    }
}

/// (x, y, got, want), as `lcd_dma::Readback::first`.
#[cfg(any(feature = "player-script", feature = "display"))]
type BadPixel = (u16, u16, [u8; 3], [u8; 3]);

/// The ` first_bad=` field (none without a mismatching pixel): frame `n`'s
/// first mismatching pixel.
#[cfg(any(feature = "player-script", feature = "display"))]
fn write_first_bad(out: &mut dyn Write, bad: Option<(u32, BadPixel)>) -> fmt::Result {
    if let Some((n, (x, y, got, want))) = bad {
        write!(
            out,
            " first_bad=frame{n}:({x},{y}) got={:02x}{:02x}{:02x} want={:02x}{:02x}{:02x}",
            got[0], got[1], got[2], want[0], want[1], want[2]
        )?;
    }
    Ok(())
}

/// The ` first_error=` field (none without a transfer error).
fn write_first_error(out: &mut dyn Write, e: Option<(u32, lcd_dma::Error)>) -> fmt::Result {
    if let Some((n, e)) = e {
        write!(out, " first_error=frame{n}:{e:?}")?;
    }
    Ok(())
}

/// The panel set up for a sink (FMC at `SPEC`, the Game Boy frame rate,
/// all black) and the transfer (ADR 0039).
fn panel() -> LcdDma {
    let mut l = Lcd::init(lcd::SPEC);
    l.set_frame_rate(FrameRate::GAME_BOY);
    l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
    l.begin_pixels();
    l.fill(0, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
    LcdDma::new(l)
}

/// The readback's control: with the last transfer done, the panel holds
/// the buffer, so one buffer word inverted (both its pixels, every bit)
/// must give exactly 2 mismatching pixels; the count it gave.
#[cfg(any(feature = "player-script", feature = "display"))]
fn control_readback(dma: &mut LcdDma) -> u32 {
    let (l, buf) = dma.parts();
    buf[100][60] ^= 0xFFFF_FFFF;
    let rb = lcd_dma::readback(l, buf);
    buf[100][60] ^= 0xFFFF_FFFF;
    rb.mismatches
}

/// The first readback sample: the first frame shown at or after this loop
/// frame (2 s in: most ROMs have drawn something by then).
#[cfg(feature = "player-script")]
pub const SAMPLE_FIRST: u32 = 120;
/// Samples start at least this many loop frames apart (5.0 s).
#[cfg(feature = "player-script")]
pub const SAMPLE_EVERY: u32 = 300;
/// Rows read back per step: 4 x 0.42 ms = 1.7 ms (54 steps a sample).
#[cfg(feature = "player-script")]
pub const READBACK_ROWS: usize = 4;
/// Sentinel rows written per step: 27 x 56 us = 1.5 ms (8 steps).
#[cfg(feature = "player-script")]
pub const SENTINEL_ROWS: usize = 27;
#[cfg(feature = "player-script")]
const _: () = assert!(OUT_H.is_multiple_of(READBACK_ROWS) && OUT_H.is_multiple_of(SENTINEL_ROWS));

/// Where a readback sample is (module docs, "Readback samples").
#[cfg(feature = "player-script")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// The sentinel, from this buffer row on.
    Sentinel(usize),
    /// The sample's transfer queued or running.
    Transfer,
    /// The readback, from this buffer row on.
    Readback(usize),
}

#[cfg(feature = "player-script")]
struct Sampler {
    phase: Phase,
    /// The loop frame from which the next sample may start.
    next_at: u32,
    /// The sample's loop frame.
    frame: u32,
    /// One step per loop iteration: set by each frame, cleared by a step.
    armed: bool,
    samples: u32,
    readbacks: u32,
    mismatches: u32,
    first_bad: Option<(u32, BadPixel)>,
    /// The cycles of each step that did work.
    step: Range,
}

#[cfg(feature = "player-script")]
impl Sampler {
    const fn new() -> Sampler {
        Sampler {
            phase: Phase::Idle,
            next_at: SAMPLE_FIRST,
            frame: 0,
            armed: false,
            samples: 0,
            readbacks: 0,
            mismatches: 0,
            first_bad: None,
            step: Range::new(),
        }
    }

    /// Starts a sample with frame `n` if one is due: the frame scaled
    /// into the buffer (no transfer may be queued), not yet queued.
    fn begin(&mut self, n: u32, dma: &mut LcdDma, scale: &mut Range, frame: &Frame) -> bool {
        if self.phase != Phase::Idle || n < self.next_at {
            return false;
        }
        let t0 = cycles();
        scale_frame(frame, dma.buffer());
        scale.add(cycles().wrapping_sub(t0));
        self.phase = Phase::Sentinel(0);
        self.next_at = n.saturating_add(SAMPLE_EVERY);
        self.frame = n;
        self.samples += 1;
        true
    }
}

/// [`Display::counters`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    pub presented: u32,
    pub skipped: u32,
    pub errors: u32,
    pub stuck: u32,
}

/// The panel filled with one RGB565 colour (FMC at `SPEC`), for a status
/// screen shown without a running game (the player with no ROM image).
pub fn status_screen(rgb565: u16) {
    let mut l = Lcd::init(lcd::SPEC);
    l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
    l.begin_pixels();
    l.fill(rgb565, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
}

pub struct Display {
    dma: LcdDma,
    /// A copy of the newest frame, taken while a transfer was running
    /// (MDMA reading the output buffer), to be shown once it is done:
    /// the core overwrites its own frame from the next frame on.
    stash: &'static mut Frame,
    /// The frame in `stash`, while it waits to be shown.
    held: Option<u32>,
    /// When the transfer now pending or running was queued.
    queued_at: Option<u32>,
    presented: u32,
    skipped: u32,
    replaced: u32,
    deferred: u32,
    transfers: u32,
    errors: u32,
    stuck: u32,
    first_error: Option<(u32, lcd_dma::Error)>,
    scale: Range,
    present: Range,
    copy: Range,
    #[cfg(feature = "player-script")]
    sampler: Sampler,
}

/// Sets the panel up (FMC at `SPEC`, the Game Boy frame rate, all
/// black) and the transfer (ADR 0039).
pub fn init(stash: &'static mut Frame) -> Display {
    Display {
        dma: panel(),
        stash,
        held: None,
        queued_at: None,
        presented: 0,
        skipped: 0,
        replaced: 0,
        deferred: 0,
        transfers: 0,
        errors: 0,
        stuck: 0,
        first_error: None,
        scale: Range::new(),
        present: Range::new(),
        copy: Range::new(),
        #[cfg(feature = "player-script")]
        sampler: Sampler::new(),
    }
}

impl Display {
    /// Notes how a finished transfer ended (`n`: the frame that found
    /// it finished).
    fn outcome(&mut self, n: u32, r: Result<Option<lcd_dma::Transfer>, lcd_dma::Error>) {
        match r {
            Ok(Some(_)) => self.transfers += 1,
            Ok(None) => {}
            Err(e) => {
                self.errors += 1;
                self.first_error.get_or_insert((n, e));
            }
        }
    }

    /// Whether the output buffer may be written now: no transfer
    /// queued, or the queued one finished (its outcome noted), or
    /// stuck (abandoned), or still pending and taken back (the frame
    /// it held becomes a skipped one: `replaced`). False while MDMA is
    /// reading the buffer.
    fn writable(&mut self, n: u32) -> bool {
        #[cfg(feature = "player-script")]
        if self.sampler.phase != Phase::Idle {
            // The buffer holds the sample until its readback is done.
            return false;
        }
        let Some(at) = self.queued_at else {
            return true;
        };
        match self.dma.try_wait() {
            Some(r) => self.outcome(n, r),
            None if cycles().wrapping_sub(at) > STUCK => {
                // Abandoned (`wait` with no time left aborts the
                // channel and reports the timeout).
                let r = self.dma.wait(0);
                self.stuck += 1;
                self.outcome(n, r);
            }
            None if self.dma.retract() => {
                self.presented -= 1;
                self.skipped += 1;
                self.replaced += 1;
            }
            None => return false,
        }
        self.queued_at = None;
        true
    }

    /// Converts `frame` into the output buffer and queues it for the
    /// next TE (`writable` must have said so).
    fn show(dma: &mut LcdDma, scale: &mut Range, present: &mut Range, frame: &Frame) -> u32 {
        let t0 = cycles();
        // No transfer is pending or running: `buffer` does not wait.
        scale_frame(frame, dma.buffer());
        let t1 = cycles();
        dma.present();
        let t2 = cycles();
        scale.add(t1.wrapping_sub(t0));
        present.add(t2.wrapping_sub(t1));
        t2
    }

    /// From the idle hook: a held frame goes out as soon as the
    /// transfer that kept it waiting has finished, so it is queued
    /// before the next TE (a transfer ends 8.1 ms after TE; the loop
    /// wakes at least every DMA half, 7.8 ms).
    pub fn service(&mut self) {
        let Some(n) = self.held else {
            return;
        };
        if !self.writable(n) {
            return;
        }
        self.held = None;
        #[cfg(feature = "player-script")]
        if self
            .sampler
            .begin(n, &mut self.dma, &mut self.scale, self.stash)
        {
            self.presented += 1;
            self.deferred += 1;
            return;
        }
        let t2 = Self::show(
            &mut self.dma,
            &mut self.scale,
            &mut self.present,
            self.stash,
        );
        self.presented += 1;
        self.deferred += 1;
        self.queued_at = Some(t2);
    }

    /// After the run: a held frame shown, the last transfer waited
    /// for (at most [`STUCK`] each), then the `stats display paced`
    /// line.
    pub fn report(&mut self, out: &mut dyn Write) -> fmt::Result {
        for _ in 0..2 {
            #[cfg(feature = "player-script")]
            self.finish_sample();
            if self.queued_at.is_some() {
                let r = self.dma.wait(STUCK);
                if matches!(r, Err(lcd_dma::Error::Timeout { .. })) {
                    self.stuck += 1;
                }
                self.outcome(u32::MAX, r);
                self.queued_at = None;
            }
            self.service();
        }
        #[cfg(feature = "player-script")]
        self.finish_sample();
        let te = lcd_dma::te_stats();
        write!(
            out,
            "stats display paced presented={} skipped={} transfers={} transfer_errors={} \
             stuck={} aborts={} te_edges={} scale_mean={} scale_max={} present_mean={} \
             present_max={} replaced={} deferred={} copy_mean={} copy_max={}",
            self.presented,
            self.skipped,
            self.transfers,
            self.errors,
            self.stuck,
            self.dma.aborts(),
            te.edges,
            self.scale.mean(),
            self.scale.max,
            self.present.mean(),
            self.present.max,
            self.replaced,
            self.deferred,
            self.copy.mean(),
            self.copy.max,
        )?;
        write_first_error(out, self.first_error)?;
        writeln!(out)?;
        #[cfg(feature = "player-script")]
        self.readback_report(out)?;
        Ok(())
    }

    /// From the idle hook while the loop is ahead (the ring full): one
    /// step of a readback sample, at most one per loop iteration (module
    /// docs, "Readback samples").
    #[cfg(feature = "player-script")]
    pub fn sample_step(&mut self) {
        if !self.sampler.armed || self.sampler.phase == Phase::Idle {
            return;
        }
        let t0 = cycles();
        // A finished transfer costs no step: the readback's first step
        // follows at once.
        let worked = self.step_once() || self.step_once();
        if worked {
            self.sampler.armed = false;
            self.sampler.step.add(cycles().wrapping_sub(t0));
        }
    }

    /// Advances the sample by one step; whether it did work (false while
    /// its transfer is still pending or running, or with no sample).
    #[cfg(feature = "player-script")]
    fn step_once(&mut self) -> bool {
        match self.sampler.phase {
            Phase::Idle => false,
            Phase::Sentinel(y) => {
                let (l, buf) = self.dma.parts();
                lcd_dma::write_inverse_rows(l, buf, y, y + SENTINEL_ROWS);
                let y = y + SENTINEL_ROWS;
                self.sampler.phase = if y >= OUT_H {
                    self.dma.present();
                    self.queued_at = Some(cycles());
                    Phase::Transfer
                } else {
                    Phase::Sentinel(y)
                };
                true
            }
            Phase::Transfer => {
                let n = self.sampler.frame;
                if let Some(at) = self.queued_at {
                    match self.dma.try_wait() {
                        Some(r) => self.outcome(n, r),
                        None if cycles().wrapping_sub(at) > STUCK => {
                            let r = self.dma.wait(0);
                            self.stuck += 1;
                            self.outcome(n, r);
                        }
                        None => return false,
                    }
                    self.queued_at = None;
                }
                self.sampler.phase = Phase::Readback(0);
                false
            }
            Phase::Readback(y) => {
                let (l, buf) = self.dma.parts();
                let rb = lcd_dma::readback_rows(l, buf, y, y + READBACK_ROWS);
                let s = &mut self.sampler;
                s.mismatches += rb.mismatches;
                if let (None, Some(p)) = (s.first_bad, rb.first) {
                    s.first_bad = Some((s.frame, p));
                }
                let y = y + READBACK_ROWS;
                s.phase = if y >= OUT_H {
                    s.readbacks += 1;
                    Phase::Idle
                } else {
                    Phase::Readback(y)
                };
                true
            }
        }
    }

    /// After the run: a sample under way finished, its transfer waited
    /// for (at most [`STUCK`]).
    #[cfg(feature = "player-script")]
    fn finish_sample(&mut self) {
        while self.sampler.phase != Phase::Idle {
            if self.sampler.phase == Phase::Transfer && self.queued_at.is_some() {
                let r = self.dma.wait(STUCK);
                if matches!(r, Err(lcd_dma::Error::Timeout { .. })) {
                    self.stuck += 1;
                }
                self.outcome(self.sampler.frame, r);
                self.queued_at = None;
            }
            self.step_once();
        }
    }

    /// The `stats display readback` line, after the control
    /// ([`control_readback`]; the last frame's transfer is done,
    /// `report`), as the measurement sink's.
    #[cfg(feature = "player-script")]
    fn readback_report(&mut self, out: &mut dyn Write) -> fmt::Result {
        let s = &self.sampler;
        let control = if s.readbacks > 0 && self.queued_at.is_none() {
            Some(control_readback(&mut self.dma))
        } else {
            None
        };
        let s = &self.sampler;
        write!(
            out,
            "stats display readback samples={} readbacks={} mismatches={} control_mismatches=",
            s.samples, s.readbacks, s.mismatches,
        )?;
        match control {
            Some(c) => write!(out, "{c}")?,
            None => write!(out, "none")?,
        }
        write!(
            out,
            " first={SAMPLE_FIRST} every={SAMPLE_EVERY} readback_rows={READBACK_ROWS} \
             sentinel_rows={SENTINEL_ROWS} steps={} step_mean={} step_max={}",
            s.step.n,
            s.step.mean(),
            s.step.max,
        )?;
        write_first_bad(out, s.first_bad)?;
        writeln!(out)
    }

    /// The running counts (the `stats display paced` fields).
    pub fn counters(&self) -> Counters {
        Counters {
            presented: self.presented,
            skipped: self.skipped,
            errors: self.errors,
            stuck: self.stuck,
        }
    }

    fn frame(&mut self, n: u32, frame: &Frame) {
        #[cfg(feature = "player-script")]
        {
            self.sampler.armed = true;
        }
        if !self.writable(n) {
            // MDMA is reading the buffer: keep a copy, shown once the
            // transfer is done (`service`, or the next frame instead).
            let t0 = cycles();
            self.stash.copy_from_slice(frame);
            self.copy.add(cycles().wrapping_sub(t0));
            if self.held.replace(n).is_some() {
                self.skipped += 1;
            }
            return;
        }
        if self.held.take().is_some() {
            // Superseded before it could be shown.
            self.skipped += 1;
        }
        #[cfg(feature = "player-script")]
        if self.sampler.begin(n, &mut self.dma, &mut self.scale, frame) {
            self.presented += 1;
            return;
        }
        let t2 = Self::show(&mut self.dma, &mut self.scale, &mut self.present, frame);
        self.presented += 1;
        self.queued_at = Some(t2);
    }
}

/// The [`Display`] as the loop's sink; the idle hook services the same
/// one ([`Display::service`]).
pub struct SinkRef<'d>(pub &'d RefCell<Display>);

impl FrameSink for SinkRef<'_> {
    fn frame(&mut self, n: u32, frame: &Frame) {
        self.0.borrow_mut().frame(n, frame);
    }
}
