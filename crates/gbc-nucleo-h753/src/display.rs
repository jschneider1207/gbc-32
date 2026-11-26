//! The panel as the paced loop's [`FrameSink`] (through [`SinkRef`]):
//! the newest completed frame goes to the panel at each TE; never waits
//! (src/bin/paced.rs docs, "Wiring"; ADR 0039 amendment, M4 review P-7).
//!
//! [`FrameSink`]: gbc_firmware::headless::FrameSink
//!
//! Shared by the `paced` and `player` images (moved from src/bin/paced.rs).

use core::cell::RefCell;
use core::fmt::{self, Write};

use crate::lcd::{self, FrameRate, Lcd};
use crate::lcd_dma::{self, LcdDma};
use crate::{CPU_HZ, cycles};
use gbc_core::Frame;
use gbc_firmware::headless::FrameSink;
use gbc_firmware::video::scale_frame;

/// A transfer still pending or running this long after `present` is
/// abandoned (100 ms: TE comes every 16.7 ms and a transfer takes
/// 8.1 ms, so a healthy one is done within 25 ms).
pub const STUCK: u32 = CPU_HZ / 10;

/// Count, max and sum of cycle counts.
#[derive(Clone, Copy, Default)]
struct Range {
    n: u32,
    max: u32,
    sum: u64,
}

impl Range {
    fn add(&mut self, v: u32) {
        self.n += 1;
        self.max = self.max.max(v);
        self.sum += u64::from(v);
    }
    fn mean(&self) -> u64 {
        self.sum / u64::from(self.n.max(1))
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
}

/// Sets the panel up (FMC at `SPEC`, the Game Boy frame rate, all
/// black) and the transfer (ADR 0039).
pub fn init(stash: &'static mut Frame) -> Display {
    let mut l = Lcd::init(lcd::SPEC);
    l.set_frame_rate(FrameRate::GAME_BOY);
    l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
    l.begin_pixels();
    l.fill(0, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
    Display {
        dma: LcdDma::new(l),
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
        scale: Range::default(),
        present: Range::default(),
        copy: Range::default(),
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
        if let Some((n, e)) = self.first_error {
            write!(out, " first_error=frame{n}:{e:?}")?;
        }
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
