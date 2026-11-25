//! The memory bus: everything the CPU reaches ([`Sys`]), the per-run-call
//! context the CPU drives ([`Ctx`]), the memory map and the I/O registers.
//! Design: ADR 0008.
//!
//! Each CPU bus call is one M-cycle: the access happens at the start of the
//! cycle (it sees the state as of the current clock), then [`Ctx::tick`]
//! advances the clock by one M-cycle and runs scheduled events (the TIMA
//! reload, a serial byte's end, the PPU's next sync) when the clock reaches the
//! earliest deadline in `Sys::sched` (the event table, ADR 0011). Nothing
//! else runs per M-cycle: the timer and the serial bit clock are computed
//! from the clock when their registers are accessed ([`crate::timer`]),
//! and the PPU is otherwise caught up only when one of its registers is
//! written (ADR 0002).
//!
//! The per-M-cycle state ([`Hot`]: the clock countdown) is copied into the
//! [`Ctx`] for the duration of a run call, so the tick reaches it through
//! the one context pointer the CPU holds instead of through `Ctx::sys`
//! (ADR 0008, amendments).

use crate::apu::{self, StereoSample};
use crate::cart::Cart;
use crate::config::Config;
use crate::cpu::{Bus, INT_MASK, StopKind};
use crate::dma::{self, Conflict, OamDma, ReadClears};
use crate::hdma::{self, Refetch, VramDma};
use crate::joypad::{InputQueue, p1_interrupt, p1_lines};
use crate::memory::Memory;
use crate::ppu::{Frame, Ppu};
use crate::schedule::{EventKind, NEVER, Schedule};
use crate::serial::Serial;
use crate::speed::Speed;
use crate::timer::{INT_TIMER, Timer};
use crate::{Buttons, Emulator, Events, RunResult};

mod apu_io;
mod ppu_io;
mod reset;
mod timer_io;

/// Everything the bus reaches except the RAM arrays (in [`Memory`]) and the
/// CPU itself. Lives in `Hw`.
#[repr(C)]
pub(crate) struct Sys<'a> {
    // Fields the inlined CPU bus path reads (`read`, `write_mapped`, the
    // interrupt checks) come first, so their offsets stay small: Thumb-2
    // has 16-bit loads only for small offsets (ADR 0011 step 1 measured
    // +1 KB in `Cpu::run` with the event table at the front).
    pub(crate) cart: Cart<'a>,
    /// Offsets of the VRAM bank at 8000 and the WRAM bank at D000.
    vram_off: usize,
    wram_off: usize,
    /// IE (FFFF), all 8 bits.
    pub(crate) ie: u8,
    /// IF (FF0F): bits 0-4, nothing else (bits 5-7 don't exist and read 1,
    /// `read_io`). A request on its way to IF is not here (`irq::PendingIrq`).
    pub(crate) iflags: u8,
    /// The cartridge runs in CGB mode (header 0143 bit 7); otherwise DMG
    /// compatibility mode, where the CGB-only registers are locked (KEY0 =
    /// 04; Pan Docs: Power Up Sequence; SameBoy `cgb_mode`).
    pub(crate) cgb_mode: bool,
    /// VBK (bit 0) and SVBK (bits 0-2) as written.
    pub(crate) vbk: u8,
    pub(crate) svbk: u8,
    /// KEY1 bit 0 (speed switch armed).
    pub(crate) key1: u8,
    /// During the CGB speed-switch halt (`Ctx::speed_switch`), the clock
    /// at which its last M-cycle (the wake-up) starts. Stale otherwise.
    pub(crate) switch_halt_end: u64,
    /// P1/JOYP select bits (4-5) as written.
    pub(crate) joyp_select: u8,
    pub(crate) buttons: Buttons,
    pub(crate) config: Config,
    pub(crate) serial: Serial,
    pub(crate) ppu: Ppu,
    /// DIV/TIMA/TMA/TAC and the system counter (off the bus hot path).
    pub(crate) timer: Timer,
    /// The clock between run calls. During a run call the live
    /// copy is [`Ctx::hot`] (this one is stale until the `Ctx` is dropped),
    /// so code running inside a call must use that one.
    pub(crate) hot: Hot,
    /// The event table: when each kind of scheduled work must next run
    /// ([`Ctx::run_events`]).
    pub(crate) sched: Schedule,
    /// Scheduled button changes; the earliest is the `Input` deadline
    /// (ADR 0012).
    pub(crate) inputs: InputQueue,
    /// CGB VRAM DMA (FF51-FF55, ADR 0017).
    pub(crate) vdma: VramDma,
    /// OAM bytes cleared by conflicting CPU reads during an OAM DMA from
    /// VRAM, not yet applied to OAM ([`ReadClears`]). Empty outside a
    /// transfer and between run calls.
    pub(crate) dma_read_clears: ReadClears,
    /// STOP mode with the screen black: frames completed in it are black
    /// ([`Ctx::stop_frame`]). Cold, so last.
    pub(crate) stop_black: bool,
}

impl Sys<'_> {
    pub(crate) fn set_joypad(&mut self, buttons: Buttons, select: u8) {
        let old = p1_lines(self.buttons, self.joyp_select);
        self.buttons = buttons;
        self.joyp_select = select & 0x30;
        self.iflags |= p1_interrupt(old, p1_lines(self.buttons, self.joyp_select));
    }

    /// Sets the `Input` deadline for the earliest scheduled button change
    /// at the current `speed` (ADR 0012). The caller reschedules the
    /// countdown if it is running.
    pub(crate) fn schedule_inputs(&mut self, speed: Speed) {
        self.sched
            .set(EventKind::Input, self.inputs.deadline(speed));
    }

    /// Sets the `DivApu` deadline at clock `now`: the next falling edge of
    /// the system counter's DIV-APU bit while the APU is powered, else
    /// [`NEVER`] (ADR 0021 section 4). Called wherever the counter is
    /// rebased or the APU powers on, with the APU's frame-sequencer state
    /// `fs`. The caller reschedules.
    pub(crate) fn set_div_apu_deadline(&mut self, fs: apu::Fs, now: u64) {
        let at = if fs.power {
            let k = self.timer.div_apu_bit() + 1;
            // Bit 13 (double speed) with the delay armed: 2 clocks late.
            let late = (u64::from(fs.ds_delay) & u64::from(k - 13)) << 1;
            self.timer.clock_at(((self.timer.sc(now) >> k) + 1) << k) + late
        } else {
            NEVER
        };
        self.sched.set(EventKind::DivApu, at);
    }
}

/// OR masks for reads of FF10-FF2F: write-only bits (lengths, frequency
/// low bytes, trigger) and unused bits and registers read 1 (Pan Docs:
/// Audio Registers; SameBoy Core/apu.c `read_mask`; Mooneye
/// `misc/bits/unused_hwio-C` and `misc/boot_hwio-C`).
const SOUND_READ_MASK: [u8; 0x20] = [
    0x80, 0x3F, 0x00, 0xFF, 0xBF, // NR10-NR14
    0xFF, 0x3F, 0x00, 0xFF, 0xBF, // FF15 (unused), NR21-NR24
    0x7F, 0xFF, 0x9F, 0xFF, 0xBF, // NR30-NR34
    0xFF, 0xFF, 0x00, 0x00, 0xBF, // FF1F (unused), NR41-NR44
    0x00, 0x00, 0x70, // NR50-NR52
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, // FF27-FF2F (unused)
];

/// The state the bus touches on every M-cycle: the clock, as a countdown.
/// Kept by value in [`Ctx`] during a run call and in [`Sys::hot`] between
/// calls.
///
/// The clock is not counted directly. `t_minus` counts the M-cycles left
/// until `deadline` (the first M-cycle boundary at or after the earlier
/// of `Sys::sched.next()` and the run call's target), and the tick only
/// adds one and tests the sign; the u64 clock is `deadline + t_minus *
/// mcycle`, exact at every point (ADR 0008, amendments).
#[derive(Clone, Copy)]
#[repr(C)]
pub(crate) struct Hot {
    /// The T-minus count: minus the M-cycles until `deadline`, so zero or
    /// above when it has been reached. It counts up so that the tick's
    /// test is the sign of the sum (the flags of the increment itself).
    /// First in `Hot`, and `Hot` first in [`Ctx`] (both `repr(C)`): the
    /// tick increments the word the CPU's bus pointer points at.
    t_minus: i32,
    /// Base clocks per M-cycle: 4, or 2 in CGB double speed. This is
    /// the CPU speed ([`Hot::speed`]), kept as the number the clock is
    /// computed with rather than as a [`Speed`]: read through
    /// [`Hot::speed`] and [`Hot::mcycle`], written only by
    /// [`Hot::set_speed`] (the clock depends on it).
    mcycle: i32,
    /// The clock at which `t_minus` reaches 0: the first M-cycle boundary
    /// at or after the earliest due clock (or that clock capped,
    /// [`Hot::set_countdown`]).
    deadline: u64,
    /// FE00 while an OAM DMA transfer runs (from the FF46 write to its
    /// `Dma` deadline), else 0. CPU accesses dispatch on `addr | dma`, so
    /// during a transfer every address takes the E000-FFFF arm, the only
    /// one that tests the flag (`read_high`, `write_mapped`) and goes to
    /// the conflict path: one OR per access and no extra branch on the
    /// fast path (ADR 0011: conflicts in the access, not the tick).
    pub(crate) dma: u16,
}

impl Hot {
    pub(crate) const fn new() -> Self {
        Self {
            t_minus: 0,
            mcycle: 4,
            deadline: 0,
            dma: 0,
        }
    }

    /// Base clocks since the last load or reset.
    #[inline]
    pub(crate) fn clock(&self) -> u64 {
        let ahead = i64::from(self.t_minus) * i64::from(self.mcycle);
        self.deadline.wrapping_add_signed(ahead)
    }

    /// The CPU speed.
    #[inline]
    pub(crate) fn speed(&self) -> Speed {
        if self.mcycle == 2 {
            Speed::Double
        } else {
            Speed::Normal
        }
    }

    /// Base clocks per M-cycle, [`Speed::mcycle`] of [`Self::speed`]
    /// without the conversion.
    #[inline]
    pub(crate) fn mcycle(&self) -> u64 {
        self.mcycle as u64
    }

    /// log2 of [`Self::mcycle`]: [`Speed::mcycle_shift`] of
    /// [`Self::speed`], as one shift of the word the tick already has
    /// (4 -> 2, 2 -> 1) for the paths that run on every access
    /// (`set_countdown`, `dma_conflict`, `Ctx::dma_sync`: through
    /// `speed()` the last costs 4 Ir per call, +0.4% on
    /// `synth:dma-storm`, measured).
    #[inline]
    fn mcycle_shift(&self) -> u32 {
        self.mcycle as u32 >> 1
    }

    /// Sets the CPU speed, the clock staying what it is. The countdown
    /// is left at zero: the caller reschedules, or the next tick does.
    pub(crate) fn set_speed(&mut self, speed: Speed) {
        self.set_clock(self.clock());
        self.mcycle = speed.mcycle() as i32;
    }

    /// Sets the clock, with the countdown at zero (the caller
    /// reschedules, or the next tick does).
    #[inline]
    fn set_clock(&mut self, now: u64) {
        self.deadline = now;
        self.t_minus = 0;
    }

    /// Sets the countdown at clock `now` for what is due `ahead` clocks
    /// later: it reaches zero at the first M-cycle boundary at or after
    /// that. `ahead` is capped at `i32::MAX` clocks (reaching a capped
    /// deadline just reschedules), so `t_minus` always fits.
    #[inline(always)]
    fn set_countdown(&mut self, now: u64, ahead: u64) {
        // A shift by 2 or 1, not a division (no divide-by-zero panic
        // path on thumbv7em).
        let (m, s) = (self.mcycle as u32, self.mcycle_shift());
        let n = (ahead.min(i32::MAX as u64) as u32 + (m - 1)) >> s;
        self.t_minus = -(n as i32);
        self.deadline = now + (u64::from(n) << s);
    }

    /// This clock with [`Self::dma`] clear: reads through it are
    /// conflict-free (`peek`, the DMA's own reads).
    #[inline]
    pub(crate) fn without_dma(&self) -> Self {
        Self { dma: 0, ..*self }
    }
}

/// The context of one run call: what the CPU's bus reaches, plus the
/// caller's buffers and the events raised so far. The CPU gets one pointer
/// to it, so a lazy PPU/APU catch-up on any bus access reaches the frame
/// and audio buffers without threading them through every call (ADR 0008).
///
/// Built with [`Ctx::new`]; dropping it stores [`Ctx::hot`] back into
/// [`Sys::hot`].
#[repr(C)]
pub(crate) struct Ctx<'r, 'a> {
    /// The live per-M-cycle state, by value so the tick doesn't load
    /// `sys` (copied from and back to `Sys::hot`). First, so that the
    /// countdown is at offset 0 ([`Hot::t_minus`]).
    pub(crate) hot: Hot,
    pub(crate) mem: &'r mut Memory,
    pub(crate) sys: &'r mut Sys<'a>,
    pub(crate) frame: &'r mut Frame,
    /// The caller's audio buffer and how much of it is written.
    pub(crate) audio: &'r mut [StereoSample],
    pub(crate) audio_len: usize,
    pub(crate) events: Events,
    /// Events that end the run call.
    pub(crate) stop_on: Events,
    /// The run call ends once the clock reaches this (`u64::MAX`: no
    /// target, or already reached).
    target: u64,
    /// The run call ends after the current instruction ([`Bus::run_done`]).
    /// Set through [`Ctx::end_run`] while the CPU runs, so that it looks.
    pub(crate) done: bool,
    /// [`Bus::attention`]: the CPU checks before its next instruction
    /// (the end of the run, an interrupt, its own state). Raised by
    /// [`Ctx::end_run`], by whatever may have left `IE & IF` nonzero
    /// ([`Ctx::irq_attention`]: after the events of a boundary, a
    /// completed serial byte, and a write to IF, IE, P1 or a PPU
    /// register) and by the CPU ([`Bus::attend`]); lowered only by the
    /// CPU's check ([`Bus::settle`]). ADR 0008, 2026-11-24 amendment.
    ///
    /// Those are all the places IF bits are set while the CPU runs
    /// instructions. A PPU sync to the current clock sets none: a point
    /// that requests an interrupt has the PPU deadline at or before it
    /// (ADR 0033), so a boundary walked it. Only a sync ahead of the
    /// clock can (the IF, STAT and LYC writes, which raise this after
    /// it; the dispatch's acknowledge, a checked step).
    attention: bool,
}

impl Drop for Ctx<'_, '_> {
    fn drop(&mut self) {
        // Bring OAM up to the clock, so it is exact between run calls.
        if self.hot.dma != 0 {
            self.dma_sync(self.clock());
        }
        self.sys.hot = self.hot;
    }
}

impl<'r, 'a> Ctx<'r, 'a> {
    /// The context for a run call that ends when the clock reaches
    /// `target` or an event in `stop_on` is raised.
    pub(crate) fn new(
        mem: &'r mut Memory,
        sys: &'r mut Sys<'a>,
        frame: &'r mut Frame,
        audio: &'r mut [StereoSample],
        stop_on: Events,
        target: u64,
    ) -> Self {
        let mut ctx = Self {
            hot: sys.hot,
            mem,
            sys,
            frame,
            audio,
            audio_len: 0,
            events: Events::EMPTY,
            stop_on,
            target,
            done: false,
            attention: true,
        };
        ctx.start_audio();
        ctx.reschedule();
        ctx
    }
}

impl Ctx<'_, '_> {
    /// Base clocks since the last load or reset.
    #[inline]
    pub(crate) fn clock(&self) -> u64 {
        self.hot.clock()
    }

    /// The run call ends after the current instruction.
    #[inline]
    pub(crate) fn end_run(&mut self) {
        self.done = true;
        self.attention = true;
    }

    /// IE or IF may have changed: with an enabled interrupt requested,
    /// the CPU checks before its next instruction ([`Ctx::attention`]).
    /// It tests IME itself, so this is also raised with IME off.
    #[inline(always)]
    fn irq_attention(&mut self) {
        if self.sys.ie & self.sys.iflags != 0 {
            self.attention = true;
        }
    }

    /// Advances everything but the CPU by one M-cycle: only the clock
    /// countdown. Everything else is an event (ADR 0011).
    #[inline(always)]
    fn tick(&mut self) {
        self.hot.t_minus += 1;
        if self.hot.t_minus >= 0 {
            self.boundary();
        }
    }

    /// The clock jumps `clocks` ahead, off the M-cycle grid (the grid
    /// moves with it): what is due by then runs, and the countdown is set
    /// for the new grid.
    fn skip_clocks(&mut self, clocks: u64) {
        self.hot.set_clock(self.clock() + clocks);
        self.boundary();
    }

    /// An IE write (FFFF): bit 2 turns the Timer deadline on or off
    /// while the timer interrupt is requested ([`Self::schedule_timer`]).
    #[inline(never)]
    fn write_ie(&mut self, value: u8) {
        let old = self.sys.ie;
        self.sys.ie = value;
        if (old ^ value) & INT_TIMER != 0 {
            self.schedule_timer();
        }
        self.irq_attention();
    }

    /// STOP's DIV reset: with nothing pending (STOP is two bytes), first
    /// the TIMA edge of AGE's later reset ([`Timer::stop_edge`], ADR 0020).
    fn stop_div_reset(&mut self) {
        if self.sys.ie & self.sys.iflags == 0 {
            let now = self.clock();
            self.sys.timer.stop_edge(now);
        }
        self.div_reset();
    }

    /// STOP mode begins, at the end of STOP's fetch and after its DIV
    /// reset (SameBoy `enter_stop_mode`; ADR 0020, STOP mode amendment).
    ///
    /// - The system counter stops ([`Timer::freeze`]), and with it
    ///   everything clocked from it: TIMA, the serial bit clock and the
    ///   APU's frame sequencer have no deadline until the wake-up
    ///   (SameBoy `timers_run` returns while `stopped`). Nothing may set
    ///   one of the three before [`Self::stop_wake`]: only CPU writes and
    ///   their own events do, and neither happens in STOP mode. The
    ///   serial port's lazy shifts follow the counter's value, not the
    ///   clock (`serial` module docs), so a running transfer holds with
    ///   it; STOP's DIV reset caught it up just before.
    /// - The PPU and the APU's channels run on (on a CGB; SameBoy
    ///   `GB_display_run`, "The PPU does not advance while in STOP mode
    ///   on the DMG", and `timers_run`, which still advances `apu_cycles`
    ///   on a CGB), so frames and samples keep their rate.
    /// - The screen goes black, unless the PPU is in mode 3 (Pan Docs:
    ///   "On CGB, leaving the LCD enabled when invoking STOP will result
    ///   in a black screen. Except if the LCD is in Mode 3, where it will
    ///   keep drawing the current screen"). SameBoy: the PPU loses
    ///   palette RAM unless the CPU's access to it was blocked at the
    ///   STOP (`cgb_palettes_ppu_blocked = !cgb_palettes_blocked`), and
    ///   draws black without it, in either CGB mode. Whole frames here
    ///   ([`Self::stop_frame`]).
    #[cold]
    #[inline(never)]
    fn stop_freeze(&mut self) {
        let now = self.clock();
        self.sys.timer.freeze(now);
        for kind in [EventKind::Timer, EventKind::Serial, EventKind::DivApu] {
            self.sys.sched.set(kind, NEVER);
        }
        // Synced, for `drawing` (as `write_palette_data`); reschedules.
        self.sync_ppu_to(now);
        let speed = self.hot.speed();
        let p = &self.sys.ppu;
        self.sys.stop_black = p.lcd_enabled() && !p.drawing() && !p.palette_blocked(now, speed);
    }

    /// STOP mode is entered, after STOP's second byte: OAM DMA pauses
    /// and HBlanks start no VRAM DMA blocks, as in HALT (SameBoy
    /// `GB_dma_run` returns while `stopped`; `GB_display_run` starts HDMA
    /// only with `!gb->stopped`). A block already requested waits for the
    /// wake-up. SameBoy decides both at the end of STOP's fetch; here the
    /// second byte's read comes first, so that it isn't a read with the
    /// transfer paused.
    #[cold]
    #[inline(never)]
    pub(crate) fn stop_enter(&mut self) {
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        if self.sys.vdma.hblank {
            let in_hblank = self.hdma_period(self.clock());
            let v = &mut self.sys.vdma;
            v.halted = true;
            v.wake_ok = !in_hblank;
        }
    }

    /// The wake-up check of a CPU in STOP mode: whether a selected P1
    /// line is low (Pan Docs: "STOP is terminated by one of the P10 to
    /// P13 lines going low"; SameBoy `GB_cpu_run`: `JOYP & 0xF != 0xF`,
    /// sampled every M-cycle). The lines were all high when STOP mode
    /// began ([`Bus::stop_begin`]) and the select bits can't change in it,
    /// so a low line is a button pressed in a selected group since: the
    /// joypad interrupt's condition ([`p1_interrupt`]).
    ///
    /// If so, STOP mode ends here (SameBoy `leave_stop_mode`): the counter
    /// runs again from the value it held, with the deadlines that follow
    /// it; OAM DMA resumes as after HALT; and in mode 0, after a STOP
    /// outside it, an HBlank transfer starts a block
    /// (`allow_hdma_on_wake`). The CPU then idles two M-cycles.
    #[cold]
    #[inline(never)]
    pub(crate) fn stop_wake(&mut self) -> bool {
        if p1_lines(self.sys.buttons, self.sys.joyp_select) == 0x0F {
            return false;
        }
        let now = self.clock();
        self.sys.stop_black = false;
        self.sys.timer.thaw(now);
        self.set_apu_deadline(now);
        self.schedule_timer();
        self.schedule_serial();
        self.halt_end();
        let _ = self.wake_stalls();
        true
    }

    /// After an idle in STOP mode: a frame completed in it is black
    /// ([`Self::stop_freeze`]). A simplification of SameBoy's per-pixel
    /// rule: the frame STOP mode begins in is black from its top, and the
    /// one it ends in is drawn normally from its top.
    #[inline]
    pub(crate) fn stop_frame(&mut self) {
        if self.sys.stop_black && self.events.contains(Events::FRAME_READY) {
            self.frame.fill(0);
        }
    }

    /// The end of a run call that started at clock `start`: the samples
    /// completed in it are written, a partial window carries over (ADR 0021
    /// section 3), and the context is dropped. Once per call.
    pub(crate) fn finish(mut self, start: u64) -> RunResult {
        self.apu_sync(self.clock());
        RunResult {
            events: self.events,
            clocks: self.clock() - start,
            audio_samples: self.audio_len,
        }
    }

    /// Records `events`, ending the run call after this instruction if
    /// one of them is a stop event.
    #[inline]
    pub(crate) fn raise(&mut self, events: Events) {
        self.events |= events;
        if events.intersects(self.stop_on) {
            self.end_run();
        }
    }

    /// Sets the countdown to the earlier of the earliest scheduled event
    /// and the target, capped at `i32::MAX` clocks ahead (reaching a
    /// capped deadline just reschedules). Called whenever a deadline
    /// changes while the countdown runs.
    fn reschedule(&mut self) {
        self.reschedule_at(self.clock());
    }

    /// [`Self::reschedule`], `now` being the current clock.
    #[inline(always)]
    fn reschedule_at(&mut self, now: u64) {
        let deadline = self.sys.sched.next().min(self.target);
        self.hot.set_countdown(now, deadline.saturating_sub(now));
    }

    /// Idle M-cycles until the countdown reaches zero (the first M-cycle
    /// boundary at or after the deadline): at least one.
    #[inline]
    fn mcycles_to_deadline(&self) -> u32 {
        // Test builds with eager sync: the skips go one M-cycle at a time.
        #[cfg(feature = "eager-sync")]
        return 1;
        #[cfg(not(feature = "eager-sync"))]
        {
            self.hot.t_minus.min(-1).unsigned_abs()
        }
    }

    /// The countdown reached zero: run what is due at the current clock
    /// and set the next deadline.
    #[inline(never)]
    fn boundary(&mut self) {
        let now = self.clock();
        if now >= self.sys.sched.next() {
            self.run_events(now);
        }
        if now >= self.target {
            self.target = u64::MAX;
            self.end_run();
        }
        // An event may have requested an interrupt.
        self.irq_attention();
        // No handler moves the clock.
        self.reschedule_at(now);
    }

    /// Runs every kind of scheduled work whose deadline is at or before
    /// `now`, in [`EventKind`] order, so events on the same clock run in a
    /// fixed order (ADR 0011). Each handler sets its next deadline. The
    /// caller checked that something is due (`now >= sched.next()`).
    fn run_events(&mut self, now: u64) {
        // One pass over the hot kinds in index order, each handler
        // storing its next deadline in place, then one fold for the
        // minimum (`Schedule::hot_min`, `settle`). The order is the one a
        // "first due kind, again after each handler" loop gives: the pass
        // starts over wherever a handler left an earlier kind (or its
        // own) due at `now`.
        //
        // `sched.next()` is stale inside the handlers; a `reschedule`
        // there is redone by `boundary`.
        loop {
            let s = &mut *self.sys;
            if s.sched.take_hot(EventKind::Timer, now) {
                // A reload's interrupt request. With IE bit 2 clear, the
                // reload itself and those that follow while IF bit 2
                // stays set are computed at the next access
                // (`schedule_timer`).
                s.iflags |= INT_TIMER;
                if s.ie & INT_TIMER != 0 {
                    debug_assert_eq!(s.timer.deadline(), now);
                    s.timer.reload(now);
                    let at = s.timer.deadline();
                    s.sched.rearm(EventKind::Timer, at);
                    if at <= now {
                        continue;
                    }
                }
            }
            if s.sched.take_hot(EventKind::Serial, now) {
                self.serial_event(now);
                let sched = &self.sys.sched;
                if sched.due(EventKind::Timer, now) || sched.due(EventKind::Serial, now) {
                    continue;
                }
            }
            if self.sys.sched.take_hot(EventKind::Ppu, now) {
                self.sync_ppu(now);
            }
            let hot = self.sys.sched.hot_min();
            if hot <= now {
                continue;
            }
            let Some(kind) = self.sys.sched.take_slow(now) else {
                self.sys.sched.settle(hot);
                break;
            };
            match kind {
                EventKind::Dma => {
                    if self.sys.ppu.dma.tail < dma::LEN {
                        self.dma_tail_end();
                    } else {
                        self.dma_sync(now);
                        self.hot.dma = 0;
                    }
                }
                EventKind::VramDma => self.vdma_hblank(),
                EventKind::Input => self.apply_inputs(now),
                EventKind::DivApu => self.div_apu_event(now),
                EventKind::AudioBuffer => self.audio_buffer_event(now),
                EventKind::Timer | EventKind::Serial | EventKind::Ppu => {
                    debug_assert!(false, "{kind:?} is a hot kind");
                }
            }
        }
    }

    /// Applies the scheduled button changes due at the M-cycle boundary
    /// `now`: those at a clock inside the M-cycle that starts at `now` or
    /// earlier (rounded down, ADR 0012), so accesses from `now` on see
    /// them. Then schedules the next one. Out of line, off `boundary`.
    #[cold]
    #[inline(never)]
    fn apply_inputs(&mut self, now: u64) {
        let speed = self.hot.speed();
        let s = &mut *self.sys;
        if let Some(buttons) = s.inputs.take_due(now + speed.mcycle() - 1) {
            s.set_joypad(buttons, s.joyp_select);
        }
        s.schedule_inputs(speed);
    }

    /// A CPU write at the current clock (no M-cycle; the caller ticks).
    /// During an OAM DMA transfer every address takes the E000-FFFF
    /// branch ([`Hot::dma`]), where the flag test sends it to the
    /// conflict path, off the RAM fast path.
    ///
    /// Out of line, one copy for the CPU's writes: inlined whole into
    /// `Cpu::run` it is +10.6 KB (over the `.text` gate), and with only
    /// the C000-DFFF store inlined at every write +3.3 KB for -0.8% on
    /// cpu_instrs and +0.7% on `synth:dma-storm` (measured; docs/perf.md
    /// "CPU write path").
    #[inline(never)]
    pub(crate) fn write_mapped(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        // The tests in the order of how often real code takes them: WRAM
        // (the stack), then FF00-FFFF, then the rest. FF00-FFFF is
        // dispatched here (`write_ff`), not through one more call.
        let key = addr | self.hot.dma;
        if key >> 13 == 6 {
            *wram_mut(self.mem, self.sys, a) = value;
        } else if key >= 0xE000 {
            if self.hot.dma != 0 {
                self.write_during_dma(addr, value)
            } else if addr >= 0xFF00 {
                // HRAM is stored here; I/O and IE are calls.
                self.write_ff(addr, value)
            } else if addr >= 0xFE00 {
                self.write_oam(addr, value)
            } else {
                *wram_mut(self.mem, self.sys, a) = value;
            }
        } else {
            match key >> 13 {
                4 => self.write_vram(addr, value),
                5 => self.sys.cart.write_ram(addr, value, self.hot.clock()),
                _ => self.sys.cart.write_rom(addr, value, self.hot.clock()),
            }
        }
    }

    /// [`Self::write_mapped`] below FE00 without the conflict path, for
    /// [`Self::write_during_dma`] (which handles FE00-FFFF itself).
    #[inline(always)]
    fn write_to(&mut self, addr: u16, value: u8) {
        debug_assert!(addr < 0xFE00);
        let a = usize::from(addr);
        match addr >> 13 {
            0..=3 => self.sys.cart.write_rom(addr, value, self.hot.clock()),
            4 => self.write_vram(addr, value),
            5 => self.sys.cart.write_ram(addr, value, self.hot.clock()),
            // C000-DFFF and its echo.
            _ => *wram_mut(self.mem, self.sys, a) = value,
        }
    }

    /// A CPU write to VRAM: dropped while the PPU blocks VRAM
    /// ([`Ppu::vram_blocked`]). Out of line: the check needs the PPU's
    /// position.
    #[inline(never)]
    fn write_vram(&mut self, addr: u16, value: u8) {
        if self.sys.ppu.vram_blocked(self.clock(), self.hot.speed()) {
            return;
        }
        let a = usize::from(addr);
        self.mem.vram[(self.sys.vram_off | (a & 0x1FFF)) & 0x3FFF] = value;
    }

    /// A CPU write to FE00-FEFF: dropped while the PPU blocks OAM
    /// ([`Ppu::oam_blocked`]), FEA0-FEFF included (Pan Docs: Memory Map,
    /// "FEA0-FEFF range").
    #[inline(never)]
    fn write_oam(&mut self, addr: u16, value: u8) {
        if self
            .sys
            .ppu
            .oam_blocked(self.clock(), self.hot.speed(), true)
        {
            return;
        }
        let i = oam_offset(addr);
        if let Some(b) = self.mem.oam.get_mut(i) {
            *b = value;
            self.sys.ppu.oam_written();
        } else {
            self.mem.extra_oam[extra_oam_index(i)] = value;
        }
    }

    /// FF00-FFFF: I/O, HRAM (stored here, in the caller) and IE. Also
    /// what a write there does during a transfer
    /// ([`Self::write_during_dma`]).
    #[inline(always)]
    fn write_ff(&mut self, addr: u16, value: u8) {
        let a = usize::from(addr);
        match addr {
            0xFF00..=0xFF7F => self.write_io(a & 0x7F, value),
            0xFF80..=0xFFFE => self.mem.hram[a - 0xFF80] = value,
            _ => self.write_ie(value),
        }
    }

    /// IF (FF0F): lands one T-cycle into the M-cycle (SameBoy
    /// `GB_CONFLICT_WRITE_CPU` in `cgb_conflict_map`, both speeds), so a
    /// PPU interrupt requested at the access dot (double speed: half a dot
    /// after it, a whole dot on our grid) is cleared by it, one later is
    /// not. CGB04C `m2int_m0irq/m2int_m0irq_scx3_ifw_{1,2,3,4,ds_1,ds_2}`
    /// (the mode-0 interrupt with SCX = 3 against an IF write one M-cycle
    /// apart). A request still on its way to IF at that dot
    /// (`Ppu::request_irq`) is not in IF yet, so the write doesn't clear
    /// it.
    #[inline(never)]
    fn write_if(&mut self, value: u8) {
        let at = self.clock() + 1;
        if self.sys.sched.deadline(EventKind::Ppu) <= at {
            self.sync_ppu_to(at);
        }
        let s = &mut *self.sys;
        let old = s.iflags;
        s.iflags = value & INT_MASK;
        if (old ^ value) & INT_TIMER != 0 {
            self.schedule_timer();
        }
        self.irq_attention();
    }

    /// [`Bus::push_acknowledge`] of a push onto IF or IE (and FF1F-FFEF,
    /// which the dispatch's one test also sends here).
    ///
    /// Onto IF itself (SP = FF10): the
    /// vector comes from IF as it was before the write, the write and the
    /// IF read being the same access (SameBoy `cycle_write_if` returns the
    /// old IF; gambatte cgb04c `irq_precedence/if_and_ie_0_vector_4`:
    /// SP = FF11 writes 0x80 to IF and the timer vector still runs). Out
    /// of line: in the inlined dispatch, the old-IF select cost ~1.5% Ir
    /// in every bench ROM (codegen of `Cpu::run`, measured).
    ///
    /// Onto IE (SP = 0000): `ie` is the dispatch's earlier sample, so the
    /// acknowledged bit can be the timer's with IE bit 2 now clear, which
    /// turns the Timer deadline back on ([`Self::schedule_timer`]; an
    /// acknowledge anywhere else finds it on).
    #[cold]
    #[inline(never)]
    fn push_acknowledge_reg(&mut self, addr: u16, value: u8, ie: u8) -> u8 {
        let old = self.sys.iflags;
        self.write_mapped(addr, value);
        let iflags = if addr == 0xFF0F { old } else { self.sys.iflags };
        let pending = ie & iflags;
        let bit = pending.isolate_lowest_one();
        // The bit is cleared as in `push_acknowledge`.
        const PPU: u8 = crate::ppu::INT_VBLANK | crate::ppu::INT_STAT;
        if bit & PPU != 0 {
            let mid = self.clock() + self.hot.mcycle() / 2;
            if self.sys.sched.deadline(EventKind::Ppu) <= mid {
                self.run_ppu_to_call(mid);
            }
            self.sys.iflags &= !bit;
        }
        self.idle();
        self.sys.iflags &= !(bit & !PPU);
        self.schedule_timer();
        pending
    }

    /// FF00-FF7F, by the low 7 address bits.
    fn write_io(&mut self, reg: usize, value: u8) {
        let s = &mut *self.sys;
        let cgb = s.cgb_mode;
        match reg {
            0x00 => {
                // Selecting a group with a button held requests the
                // joypad interrupt.
                s.set_joypad(s.buttons, value);
                self.irq_attention();
            }
            0x01 | 0x02 | 0x04..=0x07 => self.write_timer_io(reg, value),
            0x0F => self.write_if(value),
            0x10..=0x3F => self.write_apu(reg, value),
            0x40..=0x45 | 0x47..=0x4B => self.write_ppu(reg, value),
            0x46 => self.oam_dma(value),
            0x51..=0x54 if cgb => s.vdma.write_addr(reg - 0x50, value),
            0x55 if cgb => self.write_hdma5(value),
            0x4D if cgb => s.key1 = value & 0x01,
            0x4F if cgb => {
                s.vbk = value & 0x01;
                s.vram_off = usize::from(s.vbk) * 0x2000;
            }
            // CGB palettes. BCPS/OCPS (the index registers) are writable on
            // a CGB in either mode, BCPD/OCPD only in CGB mode (SameBoy
            // `write_high_memory`); blocked around mode 3
            // ([`Ppu::palette_blocked`]).
            0x68 | 0x6A => self.mem.io[reg] = value,
            0x69 | 0x6B if cgb => self.write_palette_data(reg == 0x6B, value),
            // OPRI: writable in CGB mode only (after the boot ROM), and only
            // stored: SameBoy applies a write to the object priority mode
            // only while the boot ROM runs or in KEY0's PGB mode (Core/
            // memory.c, GB_IO_OPRI; Pan Docs: OPRI), so after boot the
            // mode stays what reset chose (OAM index in CGB mode).
            0x6C if cgb => self.mem.io[reg] = value,
            0x70 if cgb => {
                s.svbk = value & 0x07;
                // Bank 0 selects bank 1 (Pan Docs: SVBK).
                s.wram_off = usize::from(s.svbk.max(1)) * 0x1000;
            }
            // Undocumented CGB registers (SameBoy Core/memory.c,
            // `write_high_memory`: GB_IO_PSWX/PSWY/PSW/PGB store the
            // value; the read masks are in `read_io`). RP (FF56, the
            // infrared port) too, in either mode (SameBoy: `GB_IO_RP`
            // stores on any CGB); its read is CGB mode only
            // (`read_io_other`). One arm with them: `write_io` is in the
            // ITCM set, and a separate arm cost it 40 B.
            0x56 | 0x72..=0x75 => self.mem.io[reg] = value,
            _ => {}
        }
    }

    /// FF46: starts an OAM DMA transfer from `value << 8` (Pan Docs: OAM
    /// DMA Transfer). The FF46 write's M-cycle and the next one are the
    /// start delay; byte `i` is transferred in the M-cycle `2 + i` after the
    /// write's. A write while a transfer is running restarts it: the old
    /// one stops, but keeps CPU OAM reads blocked through the new start
    /// delay unless it was in its last M-cycle (SameBoy `GB_IO_DMA`,
    /// `dma_restarting`; Mooneye `oam_dma_restart`).
    ///
    /// The PPU is brought up to the write first (its OAM reads so far saw
    /// the old state), and OAM is kept as it is now for the PPU's reads of
    /// bytes not written yet (`Memory::oam_dma_old`).
    #[inline(never)]
    fn oam_dma(&mut self, value: u8) {
        let now = self.clock();
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        self.dma_ppu_event(now);
        self.mem.io[0x46] = value;
        let m = self.hot.mcycle();
        let restart = self.hot.dma != 0 && {
            self.dma_sync(now);
            now + m < self.sys.ppu.dma.end
        };
        // The old transfer still moves a byte in the next M-cycle, if it
        // is past its own start delay by then, but already on the new
        // source's bus and address (gambatte: the FF46 write sets the new
        // source at once and the old position runs on until the new
        // start; cgb04c `oamdma_src8000_srcchange0000_busyinc`).
        let old = self.sys.ppu.dma.start;
        let start = now + 2 * m;
        let end = start + u64::from(dma::LEN) * m;
        let mut d = OamDma {
            base: u16::from(value) << 8,
            start,
            block_from: if restart { now } else { start },
            end,
            speed: self.hot.speed(),
            ppu_live: true,
            ..OamDma::new()
        };
        d.set_ppu_window();
        let mut deadline = end;
        if restart && now + m >= old {
            // Until the `Dma` deadline at the real `start`
            // (`dma_tail_end`), `start` is that of a transfer at byte
            // `tail` in the next M-cycle, so the CPU's conflict path
            // (`dma_conflict`) needs no test for it; `copied` keeps the
            // lazy copy from moving bytes meanwhile.
            let tail = ((now + m - old) >> d.speed.mcycle_shift()) as u8;
            d.tail = tail;
            d.start = now + m - u64::from(tail) * m;
            d.copied = tail + 1;
            deadline = start;
        }
        self.sys.ppu.dma = d;
        self.mem.oam_dma_old = self.mem.oam;
        self.hot.dma = 0xFE00;
        self.sys.sched.set(EventKind::Dma, deadline);
        self.reschedule();
    }

    /// The `Dma` deadline at a restarted transfer's real start (or
    /// anything that needs the real state before it): undoes the restart
    /// tail's stand-in `start` (see [`Self::oam_dma`]). Cold.
    #[cold]
    #[inline(never)]
    fn dma_tail_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        let m = d.speed.mcycle();
        d.start += (u64::from(d.tail) + 1) * m;
        d.copied = 0;
        d.tail = dma::LEN;
        self.sys.sched.set(EventKind::Dma, d.end);
    }

    /// Before the OAM DMA state changes at `now` (an FF46 write, a halt
    /// during a transfer): the PPU catches up, so its OAM reads up to now
    /// (OAM scan entries, object fetches) see the old state
    /// ([`OamDma::ppu_at`] only describes clocks after the last change).
    #[inline(never)]
    fn dma_ppu_event(&mut self, now: u64) {
        self.run_ppu_to_call(now);
        let s = &mut *self.sys;
        s.ppu.dma_event(now, self.mem, self.frame);
        self.schedule_ppu();
    }

    /// Copies the OAM DMA bytes due by the CPU access at `now` (an M-cycle
    /// boundary). Called before anything can observe OAM or change a
    /// source byte not yet copied: a CPU write during the transfer, a
    /// restart, the end of a run call and the `Dma` deadline.
    fn dma_sync(&mut self, now: u64) {
        let d = self.sys.ppu.dma;
        // Paused while halted: no progress past the pause.
        let due = d.due(now.min(d.paused), self.hot.mcycle_shift());
        for i in d.copied..due {
            let src = d.base + u16::from(i);
            // CGB: a source at E000-FFFF reads FF (SameBoy `GB_dma_run`).
            self.mem.oam[usize::from(i)] = if src >= 0xE000 {
                0xFF
            } else {
                read::<false>(self.mem, self.sys, &self.hot.without_dma(), src)
            };
        }
        if due > d.copied {
            self.sys.ppu.oam_written();
        }
        self.sys.ppu.dma.copied = d.copied.max(due);
        // Only a transfer from VRAM records any (`dma_read_clear`).
        if d.base & 0xE000 == 0x8000 {
            self.dma_apply_read_clears();
        }
    }

    /// Clears the OAM bytes recorded by [`dma_read_clear`] (all copied by
    /// now: each was due by its read) and empties the record.
    #[cold]
    #[inline(never)]
    fn dma_apply_read_clears(&mut self) {
        if self.sys.dma_read_clears.apply(&mut self.mem.oam) {
            self.sys.ppu.oam_written();
        }
    }

    /// A CPU write while an OAM DMA transfer runs: OAM writes are dropped
    /// for the whole transfer, start delay included (SameBoy
    /// `write_high_memory`), and the rest follows [`dma::conflict`] for
    /// CGB C. A write that conflicts with the DMA never reaches its
    /// target; what it does to OAM follows gambatte's cgb04c captures
    /// (ADR 0028). The transfer is synced first, because the write may
    /// change a source byte or its mapping.
    #[inline(never)]
    fn write_during_dma(&mut self, addr: u16, value: u8) {
        let now = self.clock();
        self.dma_sync(now);
        if addr >= 0xFE00 {
            if addr >= 0xFF00 {
                self.write_ff(addr, value);
            }
            return;
        }
        match dma_conflict(&self.sys.ppu.dma, &self.hot, now, addr) {
            (Conflict::None, _) => self.write_to(addr, value),
            (Conflict::Wram(a), _) => self.write_to(a, value),
            (Conflict::Open | Conflict::Dma | Conflict::DmaVram, i) => {
                // CGB C (gambatte cgb04c `oamdma_src*_busypush*`, ADR
                // 0028): the CPU's data replaces the OAM byte just
                // transferred, except that a VRAM source gets 00 there
                // and a WRAM source leaves it. SameBoy instead clears it
                // for any source below A000 and drops the rest.
                let base = self.sys.ppu.dma.base;
                if !(0xC000..0xE000).contains(&base) {
                    self.mem.oam[usize::from(i)] = if base & 0xE000 == 0x8000 { 0 } else { value };
                    self.sys.ppu.oam_written();
                }
            }
        }
    }

    /// HALT during a transfer: the DMA makes no progress while the CPU is
    /// halted (SameBoy `GB_dma_run` returns early while `halted`, and
    /// `GB_advance_cycles` overwrites `dma_cycles`, so the halted cycles
    /// are dropped, not banked). Called at the end of HALT's fetch
    /// M-cycle, whose own step still counts. OAM stays blocked (the
    /// transfer is still active) and the `Dma` deadline is cleared, so the
    /// halted skip doesn't stop at it. `docs/reference/oam-dma.md`.
    ///
    /// For the PPU (synced first: a halt changes what its OAM reads see):
    /// with bytes left, the transfer stays active and halted until the
    /// wake-up; halted in the start delay, it hasn't started for the PPU;
    /// with every byte written, its last step isn't held and the PPU sees
    /// it end on time (gambatte `oamdma_late_halt_stat_2`, cgb04c).
    #[inline(never)]
    fn dma_pause(&mut self) {
        let now = self.clock();
        if now >= self.sys.ppu.dma.end {
            return;
        }
        self.dma_ppu_event(now);
        self.dma_sync(now);
        let d = &mut self.sys.ppu.dma;
        d.paused = now;
        d.halt_from = now;
        d.halt_to = NEVER;
        d.halt_dest = d.copied;
        if now <= d.ppu_from {
            d.ppu_from = NEVER;
            d.ppu_to = NEVER;
        } else if d.copied < dma::LEN {
            d.ppu_to = NEVER;
        }
        self.sys.sched.set(EventKind::Dma, NEVER);
        self.reschedule();
    }

    /// The wake-up from HALT, at the start of the wake-up M-cycle: the
    /// transfer resumes there. SameBoy drops every halted M-cycle,
    /// including the wake-up one, then takes exactly one step on wake-up
    /// (`GB_cpu_run`: `dma_cycles = 4; GB_dma_run`): the same count, by the
    /// next CPU access, as resuming one M-cycle early. Everything still
    /// ahead moves out by the paused time. The PPU's OAM scan sees the CPU
    /// halted through the wake-up M-cycle (SameBoy clears `halted` after
    /// it); what it sees up to now doesn't change, so it needn't sync.
    #[inline(never)]
    fn dma_resume(&mut self) {
        let now = self.clock();
        let m = self.hot.mcycle();
        let d = &mut self.sys.ppu.dma;
        let shift = now - d.paused;
        if d.block_from > d.paused {
            d.block_from += shift;
        }
        d.start += shift;
        d.end += shift;
        d.paused = NEVER;
        d.halt_to = now + m;
        let held = d.ppu_to == NEVER;
        if d.ppu_from == NEVER {
            d.set_ppu_window();
        } else if held {
            d.ppu_to = d.end - dma::PPU_LEAD;
        }
        self.sys.sched.set(EventKind::Dma, d.end);
        self.reschedule();
    }

    /// A speed switch while the transfer is paused ([`Self::dma_pause`]):
    /// what is still ahead keeps its M-cycle count at the new speed
    /// `to`, around the pause (SameBoy counts the DMA in CPU cycles,
    /// `dma_cycles`). Unverified on hardware (`docs/reference/oam-dma.md`).
    #[cold]
    #[inline(never)]
    fn dma_retime(&mut self, to: Speed) {
        let d = &mut self.sys.ppu.dma;
        let p = d.paused;
        // Clocks from `p` (on the old M-cycle grid, as `p` is) scaled to
        // the new M-cycle length: halved to double speed, else doubled.
        let scale = |x: u64| if to.is_double() { x >> 1 } else { x << 1 };
        let at = |t: u64| {
            if t >= p {
                p + scale(t - p)
            } else {
                // Back to single speed within 640 clocks of reset, a
                // transfer started at double speed can have a rescaled
                // start before clock 0, which a `u64` can't hold. It
                // starts at the earliest clock the PPU's view can describe
                // (`PPU_LEAD`, `OamDma::set_ppu_window`/`ppu_at`) instead,
                // so what is left runs that many clocks late rather than
                // (wrapped) never being copied.
                p.saturating_sub(scale(p - t)).max(dma::PPU_LEAD)
            }
        };
        if d.block_from > p {
            d.block_from = at(d.block_from);
        }
        d.start = at(d.start);
        d.end = d.start + u64::from(dma::LEN) * to.mcycle();
        d.speed = to;
    }

    /// The CGB speed switch of a STOP ([`StopKind::Switch`]), after its second
    /// byte if read; returns whether the CPU waits ([`Bus::speed_switch`]).
    /// After SameBoy `stop()`
    /// and `GB_advance_cycles` (MIT; ADR 0020 has the derivation):
    ///
    /// - KEY1 bit 0 clears and the speed toggles.
    /// - Nothing pending: the CPU halts for 0x20008 T-cycles (0x8002
    ///   M-cycles; `speed_switch_halt_countdown`) by the timer, which keeps
    ///   running (Pan Docs: FF4D - KEY1). The rest of the machine is frozen
    ///   for its first 5 T-cycles (`speed_switch_freeze`), so it sees
    ///   0x8001 M-cycles at the new speed (exact to double speed, one clock
    ///   long to single speed, where SameBoy itself avoids "odd mode"): the
    ///   timer runs one M-cycle ahead, which cancels STOP's second byte,
    ///   whose M-cycle it doesn't count (`timers_run` returns while
    ///   `stopped`). An interrupt ends the halt early. OAM DMA is paused
    ///   through it (`GB_dma_run` returns while `halted`), as in HALT.
    /// - An interrupt pending: no halt; to double speed the next two
    ///   M-cycles take 6 clocks for the PPU (one more M-cycle here, which
    ///   the timer doesn't count).
    /// - IME off: the timer starts 4 T-cycles late (`enter_stop_mode`:
    ///   `div_cycles = -4`, the DIV reset held). On CGB B/C the same hold
    ///   applies with an interrupt pending and IME on (below; SameBoy,
    ///   modelling CGB E, has none there).
    ///
    /// # Order (ADR 0020, 2026-11-24 amendment)
    /// The speed is an input to every lazily evaluated component and to
    /// every deadline, so the switch goes in four steps, in this order:
    ///
    /// 1. Everything lazy is brought up to the switch clock with the
    ///    **old** speed's rules: the PPU, OAM DMA (`dma_pause`) and the
    ///    timer's reloads (`Timer::speed_switch` catches up before it
    ///    rebases). The APU runs on base clocks and has no rule that
    ///    depends on the speed. Nor has the serial port: its lazy shifts
    ///    follow the counter's value (`serial` module docs), which the
    ///    switch leaves alone, so a catch-up before the switch and one
    ///    after it apply the same shifts and it needs none here, only
    ///    its deadline (step 3). The scheduled inputs keep no lazy state
    ///    (deadlines only).
    /// 2. The speed changes (`Hot::set_speed`, the timer's counter rate, the
    ///    PPU's walk, a paused OAM DMA's M-cycle length).
    /// 3. Every deadline that depends on the speed is set again for the
    ///    **new** one: Ppu, DivApu, Input, Timer, Serial.
    /// 4. Only then may time pass and the event loop run (`boundary`):
    ///    the halt's 3 odd clocks to single speed.
    ///
    /// A deadline left from the old speed must never reach `boundary`: its
    /// handler would run at a clock the component isn't due at and re-arm
    /// from there (review A1-02: a fast-clock serial bit shifted twice).
    /// Lazy work left from before the switch must never run after step 2:
    /// it would run with the new speed's rules (review E8: the PPU's
    /// update points; `Ppu::sync` takes the speed of the sync that reaches
    /// a point, and only points that raise an interrupt or end a line or
    /// frame are deadlines).
    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        let now = self.clock();
        let to = self.hot.speed().toggled();
        let m = to.mcycle();
        // Step 1 (the timer's part is in `Timer::speed_switch` below).
        // The PPU first, as before any change to what it sees: its walk's
        // points up to `now` belong to the old speed (a pipeline start's
        // late-WY decision, line 0's and line 153's points).
        self.run_ppu_to_call(now);
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        let dma_paused = self.sys.ppu.dma.paused != NEVER;
        if dma_paused {
            // Unlike HALT, the transfer's last step (its end) is held for
            // the PPU even with every byte written: gambatte's STOP brings
            // the transfer only up to this M-cycle (HALT: one further),
            // and its cgb04c `oamdma_late_speedchange_stat_1` (the switch
            // in the last byte's M-cycle) misses the object the PPU finds
            // in `_2` (the switch in the end's M-cycle: no pause).
            self.sys.ppu.dma.ppu_to = NEVER;
        }
        // Step 2.
        self.sys.key1 = 0;
        if dma_paused {
            self.dma_retime(to);
        }
        self.hot.set_speed(to);
        // (Sets the Ppu deadline for the new walk too: step 3's.)
        self.ppu_speed_changed();
        // The held DIV reset (4 T-cycles): with IME off (SameBoy), and on
        // CGB B/C also with an interrupt pending. AGE `spsw-interrupts`
        // (pending, IME on, both directions): the first DIV increment and
        // the 16 kHz TIMA increment after the switch come one M-cycle
        // later on CGB B/C (`OFS_B`, verified on CPU CGB B and C) than on
        // CGB E, which SameBoy's model (no hold with IME on) matches.
        let mut freeze = if ime && !pending { 0 } else { 4 };
        if pending && to.is_double() {
            freeze += 4;
        }
        // To single speed the halt is one clock short of whole M-cycles
        // (below); the timer counts that clock, so it stays on the CPU's
        // M-cycle grid.
        let short = i64::from(!to.is_double() && !pending);
        // T-cycles to clocks at the new speed.
        self.sys.timer.speed_switch(now, freeze * m / 4, short != 0);
        self.mem.apu.fs.ds_delay ^= to.is_double();
        if !pending && self.sys.vdma.hblank {
            self.vdma_stop();
        }
        // Step 3. The DIV-APU bit changes (12 <-> 13) with the counter
        // speed, and a switch to double speed toggles the frame
        // sequencer's delay (`apu::Fs::ds_delay`, above). The M-cycle length
        // changes, and with it the boundary a scheduled button change
        // rounds down to (ADR 0012). The timer and the serial clock follow
        // the rebased counter, which already accounts for the grid moving
        // in step 4 (`short`), so nothing here has to be set again after
        // it. `schedule_serial` reschedules the countdown, for the DivApu
        // and Input deadlines too.
        self.set_apu_deadline(now);
        self.sys.schedule_inputs(to);
        self.schedule_timer();
        self.schedule_serial();
        if short != 0 {
            // Step 4. The halt's odd clocks (`switch_halt_end` below), idled
            // first (the CPU halts at `now`, so after `vdma_stop`): the
            // CPU's M-cycle grid moves by 3 (-1) clocks. What is due in
            // them runs at their end, the first boundary of the new grid.
            self.skip_clocks(3);
        }
        if !pending {
            // To single speed, Gambatte's PPU gains a clock at the switch
            // (`PPU::speedChange`: `p_.now -= isDoubleSpeed()`), and SameBoy
            // counts 0x20003 T-cycles at single speed after its 5-cycle
            // freeze: the PPU sees one clock less than 0x8001 M-cycles from
            // `now` to the next fetch (3 clocks, idled above, then 0x7FFF
            // M-cycles and the wake-up), so the CPU's M-cycle grid moves by
            // a clock against the PPU's, as on hardware (gambatte
            // `lcd_offset` tests, offsets 1 and 3).
            self.sys.switch_halt_end = now + 0x8000 * m - short as u64;
            // The halt is whole M-cycles from here (the odd clocks are
            // idled): `idle_speed_switch` ends exactly on its end.
            debug_assert!(
                (self.sys.switch_halt_end)
                    .wrapping_sub(self.clock())
                    .is_multiple_of(m)
            );
            return true;
        }
        if dma_paused {
            self.dma_resume();
        }
        // The CPU idles one M-cycle to double speed.
        to.is_double()
    }

    /// Whether the speed-switch halt has run its length.
    #[inline]
    pub(crate) fn speed_switch_over(&self) -> bool {
        self.clock() >= self.sys.switch_halt_end
    }

    /// As [`Bus::halt_end`], except (gambatte CGB C
    /// captures, ADR 0017 speed-switch amendment):
    /// - OAM DMA resumes one M-cycle later, at the end of the wake-up
    ///   M-cycle (`oamdma/oamdmasrcC0_speedchange_readC000`,
    ///   `dma/hdma_transition_speedchange_oamdma`: a WRAM read after the
    ///   switch sees the transfer one byte earlier).
    ///
    /// As after HALT, an HBlank in the wake-up M-cycle starts no HDMA
    /// block ([`Bus::wake_stalls`] clears `halted`).
    /// The wake-up's own block (HALT began outside the HBlank) is checked
    /// at the end of the wake-up M-cycle, as after HALT, also when the
    /// countdown ends it (gambatte `intevent_unhalt`;
    /// `dma/hdma_m3speedchange_late_m0wakeup_1`), and runs before the next
    /// instruction.
    #[inline]
    pub(crate) fn speed_switch_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        if d.paused != NEVER {
            d.paused -= self.hot.mcycle();
        }
        self.halt_end();
    }

    /// The halted skip ([`Bus::idle_halted`]), stopping at the halt's
    /// last M-cycle. Called while the halt isn't over
    /// ([`Self::speed_switch_over`]); always idles at least one M-cycle,
    /// so even a halt end off the M-cycle grid (`speed_switch` keeps it
    /// on the grid, and asserts so) is passed rather than never reached.
    #[inline]
    pub(crate) fn idle_speed_switch(&mut self) {
        let m = self.hot.mcycle() as u32;
        // The halt is 0x20000 clocks at most: the difference fits.
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        let left = self.sys.switch_halt_end.wrapping_sub(self.clock()) as u32 / m;
        let n = self.mcycles_to_deadline().min(left).max(1);
        self.hot.t_minus += n as i32;
        if self.hot.t_minus >= 0 {
            self.boundary();
        }
    }
}

/// CGB VRAM DMA (FF51-FF55; `hdma` module, ADR 0017).
impl Ctx<'_, '_> {
    /// Runs the CPU stall of the current transfer step (`VramDma::stall`
    /// idle M-cycles left), skipped a deadline at a time (ADR 0011), each
    /// deadline handled on the M-cycle it was before. Stops early at the
    /// first deadline that raises a stop event or reaches `limit` (the
    /// call's target; `u64::MAX` for a single step): the run call ends
    /// there and the next one resumes the stall. Without this, a GDMA
    /// stall (up to 4,100 clocks) crossing the `AudioBuffer` deadline
    /// would complete windows with no slot left, dropping them (ADR 0021,
    /// GDMA stall amendment).
    #[inline(never)]
    pub(crate) fn vdma_stall(&mut self, limit: u64) {
        let mut n = u32::from(self.sys.vdma.stall);
        while n != 0 {
            #[cfg(feature = "eager-sync")]
            self.eager_sync();
            let k = self.mcycles_to_deadline().min(n);
            self.hot.t_minus += k as i32;
            n -= k;
            if self.hot.t_minus >= 0 {
                // Left after this deadline's M-cycle, for `vdma_hblank`:
                // an HBlank trigger before the stall's end is lost.
                self.sys.vdma.stall = n as u16;
                self.boundary();
                if self.events.intersects(self.stop_on) || self.clock() >= limit {
                    break;
                }
            }
        }
        self.sys.vdma.stall = n as u16;
    }

    /// HDMA's HBlank period ([`Ppu::hdma_period`]) at clock `at`: from this
    /// line's trigger ([`hdma::HBLANK_DELAY`] after the mode-0 STAT change)
    /// to [`hdma::HBLANK_UNTIL`] dots into the next line. Also with the
    /// LCD off (a transfer started then runs at once).
    fn hdma_period(&self, at: u64) -> bool {
        let p = &self.sys.ppu;
        !p.lcd_enabled() || p.hdma_period(at, hdma::HBLANK_DELAY, hdma::HBLANK_UNTIL)
    }

    /// A transfer step is due: the CPU stops after this instruction so
    /// the run loop can run it ([`Emulator::run`]).
    fn vdma_request(&mut self) {
        self.sys.vdma.pending = true;
        self.end_run();
    }

    /// FF55 write. An HBlank transfer that starts in HDMA's period as it
    /// is one M-cycle later (gambatte `enableHdma`: `isHdmaPeriod(cc +
    /// 4)`) does its first block now; otherwise it waits for the next
    /// trigger, this line's if still ahead (`dma/hdma_late_enable_*`: an
    /// enable at the HBlank's end starts none).
    #[inline(never)]
    fn write_hdma5(&mut self, value: u8) {
        let now = self.clock();
        if value & 0x80 != 0 {
            // Latch the current line, if due, before the transfer is
            // active.
            self.run_ppu_to_call(now);
            self.schedule_ppu();
        }
        let mode0 = self.hdma_period(now + self.hot.mcycle());
        self.sys.vdma.write_hdma5(value, mode0);
        if self.sys.vdma.pending {
            self.end_run();
        }
        if self.sys.vdma.hblank {
            self.vdma_arm(now, false);
        }
    }

    /// Schedules the HBlank trigger of the PPU's current visible line, once
    /// per line. `fire_late`: a trigger at or before `now` fires at the
    /// next boundary (the PPU sync that latched the line ran late by less
    /// than an M-cycle); otherwise it is treated as passed.
    ///
    /// Every PPU sync during an HBlank transfer comes here (about 3.4 per
    /// line), and all but the first of a line find it armed: that test is
    /// inline.
    #[inline(always)]
    fn vdma_arm(&mut self, now: u64, fire_late: bool) {
        let s = &*self.sys;
        match s.ppu.mode0_start() {
            Some(start) if start != s.vdma.line => self.vdma_arm_at(start, now, fire_late),
            _ => {}
        }
    }

    /// [`Self::vdma_arm`] for a line whose mode 0 starts at `start`, not
    /// armed yet.
    #[inline(never)]
    fn vdma_arm_at(&mut self, start: u64, now: u64, fire_late: bool) {
        let s = &mut *self.sys;
        s.vdma.line = start;
        let at = start + hdma::HBLANK_DELAY;
        if at > now || fire_late {
            s.sched.set(EventKind::VramDma, at);
            // `boundary` reschedules when called from an event handler.
            self.reschedule();
        }
    }

    /// The `VramDma` deadline: an HBlank starts a block, unless the CPU
    /// is halted (SameBoy: `!gb->halted`), the transfer was stopped, or a
    /// block's stall is still running: gambatte acknowledges the request
    /// at a transfer's end, so a trigger during it is lost (CGB C captures
    /// `dma/hdma_transition_{,ei_}halt_late_unhalt_scx1_{1,2}`).
    fn vdma_hblank(&mut self) {
        let v = &mut self.sys.vdma;
        if v.refetch == Refetch::Ran {
            v.refetch = Refetch::None;
            if self.hot.clock() < v.stall_end {
                return;
            }
        }
        if v.hblank && !v.halted && v.stall == 0 && self.sys.ppu.lcd_enabled() {
            self.vdma_request();
        }
    }

    /// HALT begins during an HBlank transfer: HBlanks start no blocks
    /// until the wake-up, and the wake-up may start one (`wake_ok`) if the
    /// HALT began outside the HBlank (SameBoy `allow_hdma_on_wake`: STAT
    /// mode at the HALT). The HBlank here is HDMA's period
    /// ([`Self::hdma_period`]), which starts at this line's trigger, not
    /// at the mode-0 STAT change: a HALT between the two counts as before
    /// it (gambatte `dma/hdma_late_m3halt_m0unhalt_scx{1_3,2_2}`, CGB C
    /// captures; `hdma_late_m0halt_*` bound the other side).
    #[cold]
    fn vdma_halt(&mut self) {
        let in_hblank = self.hdma_period(self.clock());
        let v = &mut self.sys.vdma;
        v.halted = true;
        v.wake_ok = !in_hblank;
        // A block requested in HALT's M-cycle (gambatte `Memory::halt`).
        if v.pending {
            v.refetch = Refetch::Requested;
        }
    }

    /// The speed-switch halt begins during an HBlank transfer (after the
    /// speed change: `mcycle` is the new speed's). As [`Self::vdma_halt`],
    /// and gambatte samples HDMA for a STOP at the start of its last
    /// M-cycle (`Memory::stop(cc - 4)`), so a block due then was triggered
    /// in STOP's first M-cycle (ADR 0017, speed-switch amendment; CGB C
    /// captures `dma/hdma_late_m3speedchange_*_2`,
    /// `hdma_transition_speedchange_*`):
    /// - Triggered in the last M-cycle: it falls into the halt, which
    ///   loses it (`hdma_late_m3speedchange_{ly_scx1_1,tima_scx1_ds_2}`).
    /// - Triggered in the first: STOP's second byte runs as the next
    ///   opcode (`refetch`; `_inc_` ROMs). To double speed the block runs
    ///   in the halt, which ends the transfer with FF55 bit 7 set and its
    ///   length unchanged (gambatte `Memory::dma` while `halted()`;
    ///   `_hdma5_`, `_hdmadst10_`, `read_hdmadst00`); to single speed it
    ///   runs at the wake-up, before that opcode (`ldaaimm_scx1_ds`).
    #[cold]
    #[inline(never)]
    fn vdma_stop(&mut self) {
        // The new speed (called after the switch); the margin below is
        // the other speed's M-cycle length.
        let speed = self.hot.speed();
        let trigger = self.sys.vdma.line.wrapping_add(hdma::HBLANK_DELAY);
        if trigger + speed.toggled().mcycle() > self.clock() {
            self.sys.vdma.pending = false;
        } else if self.sys.vdma.pending {
            if speed.is_double() {
                let blocks = self.sys.vdma.blocks;
                self.vram_dma(false);
                let v = &mut self.sys.vdma;
                v.stall = 0;
                v.blocks = blocks;
                v.hblank = false;
            }
            self.sys.vdma.refetch = Refetch::Requested;
        }
        self.vdma_halt();
    }

    /// The wake-up from a HALT that began outside the HBlank: in HDMA's
    /// period it starts a block (SameBoy `GB_cpu_run`,
    /// `allow_hdma_on_wake`; gambatte `isHdmaPeriod` at the unhalt).
    #[cold]
    fn vdma_wake(&mut self) {
        self.sys.vdma.wake_ok = false;
        if self.sys.vdma.hblank && self.hdma_period(self.clock()) {
            self.vdma_request();
        }
    }

    /// Runs the pending transfer step at an instruction fetch: all blocks
    /// of a GDMA, or one HDMA block, and sets the CPU stall for it
    /// ([`hdma::stall_mcycles`], run by [`Self::vdma_stall`]). The bytes
    /// are copied at once at the start, after the PPU has drawn everything
    /// before it.
    ///
    /// Sources: ROM, cartridge RAM and WRAM are read; VRAM and E000-FFFF
    /// read FF (SameBoy reads the CPU data bus there, which this core does
    /// not model).
    ///
    /// `running`: the CPU is not in the speed-switch halt, so an OAM DMA
    /// transfer runs alongside ([`Self::oam_dma_during_vdma`]).
    #[inline(never)]
    pub(crate) fn vram_dma(&mut self, running: bool) {
        let now = self.clock();
        self.run_ppu_to_call(now);
        if self.sys.ppu.drawing() {
            self.sys.ppu.flush(now, self.mem, self.frame);
        }
        // An OAM DMA's bytes due so far read their source before the
        // VRAM DMA writes VRAM.
        if self.hot.dma != 0 {
            self.dma_sync(now);
        }
        let bank = self.sys.vram_off;
        let mut v = self.sys.vdma;
        let dest0 = v.dest;
        v.pending = false;
        let mut blocks = 0;
        loop {
            // Both counters are 16-aligned (FF52/FF54 bits 0-3 are
            // ignored, and a block adds 16), so a block is one 16-byte
            // run of VRAM: `dest & 0x1FFF` never wraps inside it.
            let block = vdma_block(self.mem, self.sys, v.src);
            let (vram, _) = self.mem.vram.as_chunks_mut::<16>();
            vram[((bank | usize::from(v.dest & 0x1FF0)) >> 4) & 0x3FF] = block;
            v.src = v.src.wrapping_add(16);
            v.dest = v.dest.wrapping_add(16);
            blocks += 1;
            v.blocks = v.blocks.wrapping_sub(1);
            if v.blocks == 0 || v.dest == 0 {
                v.hblank = false;
                break;
            }
            // One block per HBlank, also for a block whose transfer was
            // stopped after its trigger (`VramDma::single`).
            if v.hblank || v.single {
                break;
            }
        }
        v.single = false;
        // At most 1 + 128 * 16 M-cycles: fits. One less for the block a
        // HALT's or speed-switch STOP's latched opcode waited for
        // (`VramDma::refetch`).
        let full = hdma::stall_mcycles(blocks, self.hot.speed());
        v.stall = full as u16;
        v.refetch = if v.refetch == Refetch::Requested {
            v.stall -= 1;
            v.stall_end = now + u64::from(full) * self.hot.mcycle();
            Refetch::Ran
        } else {
            Refetch::None
        };
        self.sys.vdma = v;
        if self.hot.dma != 0 && running {
            self.oam_dma_during_vdma(now, dest0, blocks);
        }
    }

    /// An OAM DMA transfer whose steps fall inside a VRAM DMA that started
    /// at `now` (`blocks` blocks to VRAM from `dest0`): each such step
    /// writes the VRAM DMA's byte of that moment, not its own, to OAM at
    /// the VRAM DMA's source address & FF (if below A0), and its own byte
    /// is never written. The VRAM DMA moves 2 bytes per M-cycle at single
    /// speed (the step after M-cycle `k` takes byte `2k - 1`), 1 at double
    /// speed (byte `k - 1`); its last M-cycle (no bytes) and the steps
    /// before `now` are normal. Source: gambatte cgb04c captures
    /// `dma/hdma_transition_oamdma_1`, `oamdma/oamdmasrcC000_hdmasrc0000`
    /// (gambatte `Memory::dma` does this; read for behaviour only).
    /// SameBoy skips the step's write too but writes nothing in its place.
    /// Not modelled: what the PPU reads of OAM meanwhile.
    #[cold]
    #[inline(never)]
    fn oam_dma_during_vdma(&mut self, now: u64, dest0: u16, blocks: u32) {
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        let d = self.sys.ppu.dma;
        // Halted (paused): no steps.
        if d.paused != NEVER {
            return;
        }
        let m = d.speed.mcycle();
        let per = (self.hot.mcycle() as u32 / 2).max(1);
        let len = blocks * 16;
        let bank = self.sys.vram_off;
        let src0 = self.sys.vdma.src.wrapping_sub(len as u16);
        let mut copied = d.copied;
        for k in 1..=len / per {
            let t = now + u64::from(k) * m;
            if t < d.start {
                continue;
            }
            let i = (t - d.start) / m;
            if i >= u64::from(dma::LEN) {
                break;
            }
            let b = (k * per - 1) as u16;
            let p = usize::from(src0.wrapping_add(b) & 0xFF);
            if p < usize::from(dma::LEN) {
                let a = usize::from(dest0.wrapping_add(b) & 0x1FFF);
                self.mem.oam[p] = self.mem.vram[(bank | a) & 0x3FFF];
            }
            copied = i as u8 + 1;
        }
        self.sys.ppu.dma.copied = copied;
        self.sys.ppu.oam_written();
    }
}

/// The `eager-sync` feature (test only, never in a product build;
/// docs/perf.md "Sync-placement invariance").
#[cfg(feature = "eager-sync")]
impl Ctx<'_, '_> {
    /// Brings every lazily synced component up to the current clock: the
    /// PPU (with a running OAM DMA's bytes first, as before every PPU
    /// sync), the APU, the timer's reloads, the serial port's shifts and
    /// the cartridge RTC. Called
    /// at the start of every M-cycle, before its access, also inside the
    /// skips (HALT, STOP mode, the speed-switch halt, a VRAM DMA stall),
    /// which then go one M-cycle at a time ([`Self::mcycles_to_deadline`]).
    ///
    /// It sets no deadline and raises nothing the lazy build would not:
    /// a component's state is meant to be a function of the clock and the
    /// writes, not of when it was last synced (ADR 0002, 0011, 0014, 0021
    /// section 3), so a build with this feature must behave identically
    /// (`scripts/check-sync-invariance.sh`). The PPU deadline is left
    /// alone on purpose (`run_ppu_to_call`, not `sync_ppu_to`): where the lazy
    /// build reschedules is part of what is being compared.
    #[inline(never)]
    fn eager_sync(&mut self) {
        let now = self.clock();
        // Not backwards: an access earlier in this M-cycle may have run the
        // PPU past `now` already (the dispatch's acknowledge runs it to
        // the M-cycle's middle, then idles: `Bus::push_acknowledge`), and
        // `Ppu::sync` takes its clock as the new sync point.
        if self.sys.ppu.synced() <= now {
            self.run_ppu_to_call(now);
        }
        self.apu_sync(now);
        self.sys.timer.catch_up(now);
        self.serial_catch_up(now);
        if self.sys.cart.has_rtc() {
            self.sys.cart.rtc_sync(now);
        }
    }
}

impl Bus for Ctx<'_, '_> {
    #[inline(always)]
    fn read(&mut self, addr: u16) -> u8 {
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        let v = read::<true>(self.mem, self.sys, &self.hot, addr);
        self.tick();
        v
    }

    #[inline(always)]
    fn write(&mut self, addr: u16, value: u8) {
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        self.write_mapped(addr, value);
        self.tick();
    }

    #[inline(always)]
    fn idle(&mut self) {
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        self.tick();
    }

    #[inline(always)]
    fn interrupt_enable(&self) -> u8 {
        self.sys.ie
    }

    #[inline(always)]
    fn interrupt_flags(&self) -> u8 {
        self.sys.iflags
    }

    /// The PPU's bits (VBlank, STAT): its edges fall on dots, inside the
    /// M-cycle, so the PPU runs to the half-way point (SameBoy
    /// `GB_cpu_run`: `pending_cycles - 2`, then IF is cleared; 1 dot in
    /// double speed, 2 in single), the bit is cleared, and an edge after
    /// that requests the interrupt again (a PPU deadline it passed
    /// reschedules at the M-cycle's end). CGB04C (`_1` not absorbed, `_2`
    /// absorbed): `m2int_m2irq/m2int_m2irq_late_retrigger_{1,ds_1}`,
    /// `irq_precedence/late_m0irq_retrigger_{scx1_1,ds_1}`,
    /// `m1/lycint143_m1irq_late_retrigger_ds_1`,
    /// `m1/lycint_vblankirq_late_retrigger_ds_1`. The timer and serial
    /// raise theirs at M-cycle boundaries here: after the M-cycle, as
    /// before (`tima/tc00_irq_late_retrigger_ds_2`,
    /// `serial/start_wait_trigger_int8_read_if_ds_2` absorb).
    /// Inlined into the dispatch: out of line, `synth:halt-irq` ran 0.5%
    /// more instructions and `Cpu::run` was 1.1 KB larger (measured).
    #[inline(always)]
    fn push_acknowledge(&mut self, addr: u16, value: u8, ie: u8) -> u8 {
        const PPU: u8 = crate::ppu::INT_VBLANK | crate::ppu::INT_STAT;
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        // IF and IE (and FF1F-FFEF: one test).
        if addr | 0x00F0 == 0xFFFF {
            return self.push_acknowledge_reg(addr, value, ie);
        }
        self.write_mapped(addr, value);
        let pending = ie & self.sys.iflags;
        let bit = pending.isolate_lowest_one();
        if bit & PPU != 0 {
            // Only if a PPU edge can fall there (its deadline).
            let mid = self.clock() + self.hot.mcycle() / 2;
            if self.sys.sched.deadline(EventKind::Ppu) <= mid {
                self.run_ppu_to_call(mid);
            }
            self.sys.iflags &= !bit;
        }
        self.idle();
        // The timer's bit: its deadline is on (IE bit 2 is set, the push
        // wasn't onto IE; `schedule_timer`).
        self.sys.iflags &= !(bit & !PPU);
        pending
    }

    #[inline(always)]
    fn set_interrupt_flags(&mut self, value: u8) {
        let old = self.sys.iflags;
        self.sys.iflags = value & INT_MASK;
        if (old ^ value) & INT_TIMER != 0 {
            self.schedule_timer();
        }
    }

    fn event(&mut self, event: Events) {
        self.raise(event);
    }

    #[inline(always)]
    fn run_done(&self) -> bool {
        self.done
    }

    #[inline(always)]
    fn attention(&self) -> bool {
        self.attention
    }

    #[inline(always)]
    fn attend(&mut self) {
        self.attention = true;
    }

    #[inline(always)]
    fn attend_ime(&mut self) {
        self.irq_attention();
    }

    /// The CPU's check found nothing but, with `ime`, a pending
    /// interrupt it dispatches after one more instruction (EI's delay
    /// just ended) or, on the first step of a call, the run's end
    /// ([`Emulator::run`] sets `done` for a single step).
    #[inline(always)]
    fn settle(&mut self, ime: bool) {
        self.attention = self.done || (ime && self.sys.ie & self.sys.iflags != 0);
    }

    /// The wake-up's HDMA block: SameBoy `GB_cpu_run` checks STAT for it
    /// after the wake-up M-cycle, and runs it after the next opcode fetch,
    /// before that instruction (ADR 0017, amendment). gambatte
    /// `dma/hdma_late_m0unhalt_{1,ds_1}`: a wake-up M-cycle that ends in
    /// the next line's mode 2 starts none.
    #[inline(always)]
    fn wake_stalls(&mut self) -> bool {
        // HBlanks start blocks again after the wake-up M-cycle, not in it
        // (SameBoy clears `halted` after it; gambatte CGB C captures
        // `dma/hdma_m0halt_late_m3unhalt_scx1_2`,
        // `hdma_m0speedchange_late_m3wakeup_scx2_2`).
        self.sys.vdma.halted = false;
        if self.sys.vdma.wake_ok {
            self.vdma_wake();
        }
        self.sys.vdma.pending
    }

    /// SameBoy `stop()`: a button held on a selected P1 line ends STOP at
    /// once, with no DIV reset and no speed switch (Pan Docs: "Using the
    /// STOP instruction"). Otherwise STOP resets DIV (Pan Docs: FF04).
    fn stop_begin(&mut self) -> StopKind {
        if p1_lines(self.sys.buttons, self.sys.joyp_select) != 0x0F {
            return StopKind::Joypad;
        }
        self.stop_div_reset();
        if self.sys.cgb_mode && self.sys.key1 & 0x01 != 0 {
            StopKind::Switch
        } else {
            self.stop_freeze();
            StopKind::Stop
        }
    }

    fn stop_enter(&mut self) {
        Ctx::stop_enter(self);
    }

    /// The skip is the halted one: in STOP mode only a deadline (a
    /// scheduled button change) or the run's end can change anything the
    /// CPU waits for.
    fn stop_halt(&mut self) -> bool {
        if self.stop_wake() {
            return true;
        }
        self.idle_halted();
        self.stop_frame();
        false
    }

    fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        Ctx::speed_switch(self, ime, pending)
    }

    fn speed_switch_halt(&mut self) -> bool {
        if self.sys.ie & self.sys.iflags != 0 || self.speed_switch_over() {
            self.speed_switch_end();
            true
        } else {
            self.idle_speed_switch();
            false
        }
    }

    /// The halted skip (ADR 0011): nothing but a deadline can change IE &
    /// IF, the run's end or a stopped CPU's wake-up
    /// ([`Self::stop_wake`]), so consume the whole
    /// countdown in one go and handle the deadline, on the same M-cycle
    /// as idling one M-cycle at a time would.
    fn idle_halted(&mut self) {
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        // To zero, or one M-cycle past a countdown already there.
        self.hot.t_minus += self.mcycles_to_deadline() as i32;
        self.boundary();
    }

    /// The CPU's read, without the M-cycle's tick. Cold: only for a
    /// latched opcode.
    #[cold]
    #[inline(never)]
    fn fetch_peek(&mut self, addr: u16) -> u8 {
        read::<true>(self.mem, self.sys, &self.hot, addr)
    }

    /// `VramDma::refetch` is `Requested`: kept while the block is still
    /// `pending` (it runs at the wake-up), cleared otherwise (a switch to
    /// double speed ran it at once).
    fn hdma_refetch(&mut self) -> bool {
        let v = &mut self.sys.vdma;
        // The common case first: nothing to clear (perf: every HALT).
        if v.refetch == Refetch::None {
            return false;
        }
        let r = v.refetch == Refetch::Requested;
        if !v.pending {
            v.refetch = Refetch::None;
        }
        r
    }

    fn halt_begin(&mut self) {
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        if self.sys.vdma.hblank {
            self.vdma_halt();
        }
    }

    fn halt_end(&mut self) {
        if self.sys.ppu.dma.paused != NEVER {
            self.dma_resume();
        }
    }
}

/// A read as the CPU sees it at the current clock, through shared
/// references: the one read side effect (a conflicting read during an OAM
/// DMA from VRAM) is recorded in a `Cell` ([`dma_read_clear`]), so the CPU
/// and `peek` keep sharing one `read_high` (ADR 0028 amendment). Shared by
/// the CPU bus and
/// [`Emulator::peek`]. `hot` is the live clock: [`Ctx::hot`]
/// during a run call, [`Sys::hot`] otherwise. While an OAM DMA transfer
/// runs, every address goes to `read_high` and the conflict path
/// ([`Hot::dma`]); `peek` and the DMA's own reads clear it
/// ([`Hot::without_dma`]). Echo RAM is out of line in `read_high`, so
/// the inlined dispatch has no compare beyond the region.
///
/// `CPU` selects the CPU's view, where the PPU blocks VRAM and OAM
/// ([`read_vram`], [`read_oam`]); `peek` and the DMA's own reads use the
/// raw memory (SameBoy: DMA reads ignore `vram_read_blocked`).
#[inline(always)]
pub(crate) fn read<const CPU: bool>(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    let a = usize::from(addr);
    // ROM, then C000-FFFF (WRAM here, the rest in `read_high`) before
    // VRAM and cartridge RAM: the order of how often code reads them. As
    // a `match` on the region, a WRAM read came after the VRAM and
    // cartridge RAM compares (cpu_instrs and cgb_sound, which run from
    // WRAM, -5.9% and -5.1% Ir for +770 B; docs/perf.md "CPU read
    // path").
    let key = addr | hot.dma;
    if key < 0x8000 {
        sys.cart.read_rom(addr)
    } else if key >= 0xC000 {
        if key < 0xE000 {
            wram(mem, sys, a)
        } else {
            read_high(mem, sys, hot, addr)
        }
    } else if key >= 0xA000 {
        sys.cart.read_ram(addr)
    } else if CPU {
        read_vram(mem, sys, hot, addr)
    } else {
        mem.vram[(sys.vram_off | (a & 0x1FFF)) & 0x3FFF]
    }
}

/// The conflict for a CPU access to `addr` (below FE00) at `now` during
/// the transfer `d`, and the index of the byte being transferred. No
/// conflict in the start delay. (The restart tail, [`OamDma::tail`], is
/// in `d` as a transfer that started earlier: nothing here for it, as
/// any test here cost every read during a transfer up to 3% Ir on
/// `synth:dma-storm`, measured.)
fn dma_conflict(d: &OamDma, hot: &Hot, now: u64, addr: u16) -> (Conflict, u8) {
    if now < d.start {
        return (Conflict::None, 0);
    }
    // A shift, not a u64 division (a libcall on thumbv7em).
    let i = ((now - d.start) >> hot.mcycle_shift()) as u8;
    (dma::conflict(d.base, d.base + u16::from(i), addr), i)
}

/// A CPU read while an OAM DMA transfer runs (`Hot::dma`): OAM reads FF
/// (Pan Docs: OAM DMA bus conflicts; SameBoy `read_high_memory`), HRAM and
/// I/O are unaffected, and the rest follows [`dma::conflict`]. Side-effect
/// free like [`read`], except a conflicting read with a VRAM source
/// ([`dma_read_clear`]).
#[inline(never)]
fn read_during_dma(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    let now = hot.clock();
    let plain = hot.without_dma();
    if addr >= 0xFE00 {
        if addr < 0xFF00 && now >= sys.ppu.dma.block_from {
            return 0xFF;
        }
        if addr == 0xFF41 {
            return sys.ppu.stat_during_dma(now, hot.speed());
        }
        return read_high(mem, sys, &plain, addr);
    }
    let addr = match dma_conflict(&sys.ppu.dma, hot, now, addr) {
        (Conflict::None, _) => addr,
        (Conflict::Open, _) => return 0xFF,
        (Conflict::Wram(a), _) => a,
        (Conflict::DmaVram, i) => {
            dma_read_clear(sys, i);
            sys.ppu.dma.base + u16::from(i)
        }
        (Conflict::Dma, i) => sys.ppu.dma.base + u16::from(i),
    };
    read::<true>(mem, sys, &plain, addr)
}

/// A CPU read on the VRAM bus in the M-cycle of byte `i` of a transfer
/// from VRAM (`Conflict::DmaVram`): besides returning the byte the DMA reads,
/// on CGB C it clears OAM byte `i` (gambatte cgb04c
/// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, which gambatte
/// `Memory::nontrivial_read` reproduces; ADR 0028 amendment). Recorded
/// here and applied by the next `Ctx::dma_sync`, which copies byte `i`
/// first (it is due by now), so the lazy copy can't overwrite the 00.
/// Inline: a call here would cost every read during a transfer saved
/// registers (measured on `synth:dma-storm`).
#[inline(always)]
fn dma_read_clear(sys: &Sys, i: u8) {
    sys.dma_read_clears.record(i);
}

/// One VRAM DMA block's 16 source bytes from `src` (16-aligned, so the
/// block never crosses a region or bank boundary): ROM, cartridge RAM and
/// WRAM are read as [`read`] reads them, VRAM and E000-FFFF read FF. A
/// region that is one run of memory (the ROM bank window, plain cartridge
/// RAM, a WRAM bank) is copied without the per-byte dispatch.
#[inline(always)]
fn vdma_block(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
    debug_assert!(src & 0xF == 0);
    match src >> 13 {
        0..=3 => sys.cart.rom_block(src),
        5 => sys.cart.ram_block(src),
        6 => *wram_block(mem, sys, usize::from(src)),
        // VRAM and E000-FFFF: FF (see `Ctx::vram_dma`).
        _ => [0xFF; 16],
    }
}

/// 16 bytes of C000-DFFF from a 16-aligned `a`, as [`wram`] reads them.
#[inline(always)]
fn wram_block<'m>(mem: &'m Memory, sys: &Sys, a: usize) -> &'m [u8; 16] {
    let off = if a & 0x1000 == 0 {
        a & 0x0FF0
    } else {
        sys.wram_off | (a & 0x0FF0)
    };
    let (blocks, _) = mem.wram.as_chunks::<16>();
    &blocks[(off >> 4) & 0x7FF]
}

/// C000-DFFF and its echo: bank 0, then the SVBK bank.
#[inline(always)]
fn wram(mem: &Memory, sys: &Sys, a: usize) -> u8 {
    if a & 0x1000 == 0 {
        mem.wram[a & 0x0FFF]
    } else {
        mem.wram[(sys.wram_off | (a & 0x0FFF)) & 0x7FFF]
    }
}

#[inline(always)]
fn wram_mut<'m>(mem: &'m mut Memory, sys: &Sys, a: usize) -> &'m mut u8 {
    if a & 0x1000 == 0 {
        &mut mem.wram[a & 0x0FFF]
    } else {
        &mut mem.wram[(sys.wram_off | (a & 0x0FFF)) & 0x7FFF]
    }
}

/// The offset of `addr` in FE00-FEFF (the caller's address is in it):
/// below 0xA0 an index into OAM, from there on FEA0-FEFF
/// ([`extra_oam_index`]).
#[inline]
fn oam_offset(addr: u16) -> usize {
    debug_assert!(addr >> 8 == 0xFE);
    usize::from(addr & 0xFF)
}

/// FEA0-FEFF on CGB revision C, by the offset `i` in FE00-FEFF
/// ([`oam_offset`], 0xA0 or more): RAM with address bits 3-4 ignored
/// (SameBoy `GB_read_oam`/`write_oam`: `addr &= ~0x18`). TODO(M3): reads
/// 0xFF while the PPU blocks OAM (Pan Docs: "FEA0-FEFF range").
#[inline]
fn extra_oam_index(i: usize) -> usize {
    (i - 0xA0) & !0x18
}

/// The palette RAM byte BCPD (`reg` 0x69) or OCPD (0x6B) reads, at the
/// BCPS/OCPS index.
fn palette_byte(mem: &Memory, reg: usize) -> u8 {
    let ram = if reg == 0x6B {
        &mem.obj_pal
    } else {
        &mem.bg_pal
    };
    ram[usize::from(mem.io[reg - 1] & 0x3F)]
}

/// The rare I/O reads (`read_io`'s `_` arm), `reg` the low 7 address bits;
/// FF unless listed.
// Out of line, measured (docs/perf.md, "Scheduler and attribute pass"):
// as arms of `read_io` these cost +4.00% Ir on `synth:dma-storm` and
// +1.07% on cpu_instrs (`read_high` then needs the PPU position).
#[inline(never)]
fn read_io_other(mem: &Memory, sys: &Sys, hot: &Hot, reg: usize) -> u8 {
    let cgb = sys.cgb_mode;
    match reg {
        // BCPD/OCPD (CGB mode) read the palette RAM byte at the index,
        // without incrementing it (SameBoy `read_high_memory`; Pan Docs:
        // FF69), and FF while the PPU blocks palette RAM
        // ([`Ppu::palette_blocked`]; Pan Docs: LCD Color Palettes).
        0x69 | 0x6B if cgb && !sys.ppu.palette_blocked(hot.clock(), hot.speed()) => {
            palette_byte(mem, reg)
        }
        // FF74 (GB_IO_PSW): R/W in CGB mode only (see `read_io`).
        0x74 if cgb => mem.io[reg],
        // RP (CGB mode only; Pan Docs: "FF56 - RP"): bits 0, 6 and 7 as
        // written, bits 2-5 read 1 (SameBoy `read_high_memory`,
        // `GB_IO_RP`: `| 0x2E`, and `| 0x10` on every revision but CGB E),
        // and bit 1 reads 1 (no signal) unless reading is enabled (bits
        // 6-7 = 3) and light arrives. No link to another port is
        // modelled, so the only light is this port's own LED (bit 0;
        // SameBoy: "You will read your own IR LED if it's on"). Not
        // modelled: the receiver adapting to a steady light (Pan Docs).
        0x56 if cgb => {
            let rp = mem.io[reg] & 0xC1;
            (0x3E | rp) & !(u8::from(rp == 0xC1) << 1)
        }
        _ => 0xFF,
    }
}

/// A CPU read of VRAM: FF while the PPU blocks it ([`Ppu::vram_blocked`]).
/// Out of line: the check needs the PPU's position.
#[inline(never)]
fn read_vram(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if sys.ppu.vram_blocked(hot.clock(), hot.speed()) {
        return 0xFF;
    }
    mem.vram[(sys.vram_off | (usize::from(addr) & 0x1FFF)) & 0x3FFF]
}

/// A CPU read of FE00-FEFF: FF while the PPU blocks OAM
/// ([`Ppu::oam_blocked`]), FEA0-FEFF included.
#[inline(never)]
fn read_oam(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if sys.ppu.oam_blocked(hot.clock(), hot.speed(), false) {
        return 0xFF;
    }
    raw_oam(mem, addr)
}

/// FE00-FEFF as stored.
#[inline]
fn raw_oam(mem: &Memory, addr: u16) -> u8 {
    let i = oam_offset(addr);
    match mem.oam.get(i) {
        Some(&b) => b,
        None => mem.extra_oam[extra_oam_index(i)],
    }
}

/// E000-FFFF, and every address during an OAM DMA transfer (CPU only:
/// `peek` reads FE00-FEFF itself).
fn read_high(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    if hot.dma != 0 {
        return read_during_dma(mem, sys, hot, addr);
    }
    let a = usize::from(addr);
    match addr {
        // E000-FDFF: echo of C000-DDFF (Pan Docs: Memory Map).
        0..=0xFDFF => wram(mem, sys, a),
        0xFE00..=0xFEFF => read_oam(mem, sys, hot, addr),
        0xFF00..=0xFF7F => read_io(mem, sys, hot, a & 0x7F),
        0xFF80..=0xFFFE => mem.hram[a - 0xFF80],
        _ => sys.ie,
    }
}

/// SB at clock `now`, with the shifts of a running transfer up to it
/// ([`Serial::read_sb`]). Out of line, so the counter arithmetic stays
/// off `read_high` (as `Timer::read`).
#[inline(never)]
fn read_sb(sys: &Sys, now: u64) -> u8 {
    sys.serial.read_sb(sys.timer.sc(now))
}

/// FF00-FF7F by the low 7 address bits. Unused bits read 1; registers
/// that don't exist (or are CGB-only, in DMG compatibility mode) read FF.
fn read_io(mem: &Memory, sys: &Sys, hot: &Hot, reg: usize) -> u8 {
    let cgb = sys.cgb_mode;
    match reg {
        // P1/JOYP (Pan Docs: Joypad Input).
        0x00 => 0xC0 | sys.joyp_select | p1_lines(sys.buttons, sys.joyp_select),
        0x01 => read_sb(sys, hot.clock()),
        0x02 => sys.serial.read_sc(cgb),
        0x04..=0x07 => sys.timer.read(hot.clock(), reg),
        0x0F => 0xE0 | sys.iflags,
        // NR52, wave RAM and PCM12/PCM34 depend on the APU state.
        0x26 | 0x30..=0x3F | 0x76 | 0x77 => apu::read_apu(mem, hot.clock(), reg),
        // Sound: write-only and unused bits read 1 (`SOUND_READ_MASK`;
        // power off zeroes the stored bytes).
        0x10..=0x2F => mem.io[reg] | SOUND_READ_MASK[(reg - 0x10) & 0x1F],
        0x40 => sys.ppu.lcdc,
        0x41 => sys.ppu.stat(hot.clock(), hot.speed()),
        0x44 => sys.ppu.ly(hot.clock(), hot.speed()),
        0x45 => sys.ppu.lyc,
        0x42 | 0x43 | 0x46..=0x4B => mem.io[reg],
        // FF55 (HDMA5): FF in DMG compatibility mode, where no transfer
        // can start. FF51-FF54 are write-only (the `_` arm: FF).
        0x55 => sys.vdma.read_hdma5(),
        0x4D if cgb => 0x7E | sys.key1 | hot.speed().key1(),
        // VBK reads FE | bank on a CGB in either mode (SameBoy
        // `read_high_memory`: only `GB_is_cgb`; Mooneye `misc/boot_hwio-C`
        // reads FE in DMG compatibility mode). Writes need CGB mode.
        0x4F => 0xFE | sys.vbk,
        // BCPS/OCPS: bit 6 reads 1, in either mode (SameBoy
        // `read_high_memory`; Mooneye `misc/bits/unused_hwio-C`).
        0x68 | 0x6A => 0x40 | mem.io[reg],
        // BCPD/OCPD: `read_io_other`.
        0x70 if cgb => 0xF8 | sys.svbk,
        // Undocumented CGB registers, as in SameBoy (Core/memory.c,
        // `read_high_memory`, the I/O part of `GB_read_memory`): OPRI
        // reads `| 0xFE` on CGB in either mode; FF72 and FF73
        // (GB_IO_PSWX/PSWY) are fully R/W on CGB; FF74 (GB_IO_PSW) is R/W
        // in CGB mode only and reads FF otherwise; FF75 (GB_IO_PGB) has
        // only bits 4-6 R/W and reads `| 0x8F`. The model is always a CGB
        // (ADR 0003), so "on CGB" needs no check.
        0x6C => 0xFE | mem.io[reg],
        0x72 | 0x73 => mem.io[reg],
        // FF74: `read_io_other`.
        0x75 => 0x8F | mem.io[reg],
        // FF76/FF77 PCM12/PCM34 (read only, on CGB in either mode; Mooneye
        // `misc/bits/unused_hwio-C` reads 00 with the APU silent): above.
        _ => read_io_other(mem, sys, hot, reg),
    }
}

impl Emulator<'_> {
    /// Reads a byte as the CPU would see it at `addr` at the current clock,
    /// without side effects and ignoring PPU access blocking. Debug access
    /// for tools and tests; not used by emulation. Reads past the end of a
    /// short ROM return 0xFF, as if it were padded to a full bank
    /// ([`MIN_ROM_SIZE`](crate::MIN_ROM_SIZE)).
    pub fn peek(&self, addr: u16) -> u8 {
        let sys = &self.hw.sys;
        // Conflict-free: the memory as it is, OAM included, during a DMA,
        // and VRAM/OAM whatever the PPU mode.
        if (0xFE00..0xFF00).contains(&addr) {
            return raw_oam(self.mem, addr);
        }
        // Palette RAM whatever the PPU mode, too.
        if matches!(addr, 0xFF69 | 0xFF6B) && sys.cgb_mode {
            return palette_byte(self.mem, usize::from(addr & 0x7F));
        }
        read::<false>(self.mem, sys, &sys.hot.without_dma(), addr)
    }
}

/// The `clock-offset` feature (test only, never in a product build;
/// docs/perf.md "Clock-offset invariance").
#[cfg(feature = "clock-offset")]
impl Emulator<'_> {
    /// Test hook, not product API: moves the power-on state from clock 0
    /// to `clock`, so a run starts with [`Emulator::clock`] `== clock` and
    /// behaves exactly as a run from 0 does, `clock` later. Everything
    /// that holds a current position moves with it: the clock itself,
    /// every deadline, the timer's counter origin, the PPU's frame origin
    /// and sync clock, the APU's sync clock and power-on phase, and the
    /// RTC's sync clock. State that records a past event that has not
    /// happened yet (an OAM DMA, a speed switch, an HDMA trigger) stays at
    /// its power-on value, as on a machine that reached `clock` without
    /// one: that is what a wrapped compare must get right after 17
    /// minutes (2^32 clocks).
    ///
    /// Call it right after [`Emulator::load`] or [`Emulator::reset`],
    /// before any run call, `set_buttons_at` or `set_rtc_state`. `clock`
    /// must be a multiple of 4 (the M-cycle; the APU's 1 MHz trigger grid
    /// is the clock mod 4). Absolute clocks passed in afterwards
    /// (`run_until` targets, `set_buttons_at`) are on the moved clock.
    ///
    /// # Panics
    /// If the clock is not 0, or `clock` is not a multiple of 4.
    pub fn start_clock_at(&mut self, clock: u64) {
        let sys = &mut self.hw.sys;
        assert!(
            sys.hot.clock() == 0 && sys.hot.t_minus == 0,
            "not at power on"
        );
        assert!(clock.is_multiple_of(4), "not a multiple of 4");
        sys.hot.deadline = clock;
        sys.sched.shift_clock(clock);
        sys.timer.shift_clock(clock);
        sys.ppu.shift_clock(clock);
        self.mem.apu.shift_clock(clock);
        let rtc = sys.cart.rtc_at(0);
        sys.cart.set_rtc(rtc, clock);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::test_util::{load, rom, write};
    use super::*;
    use crate::{FRAME_CLOCKS, Frame};
    use std::boxed::Box;

    /// `Hot` keeps the speed as a number (the tick's `mcycle` word):
    /// its accessors agree with `Speed`'s conversions, and a speed
    /// change keeps the clock.
    #[test]
    fn hot_speed_forms_agree_with_speed() {
        let mut hot = Hot::new();
        assert_eq!(hot.speed(), Speed::Normal);
        hot.set_countdown(1000, 40);
        hot.t_minus += 3;
        for speed in [Speed::Double, Speed::Normal, Speed::Double] {
            let now = hot.clock();
            hot.set_speed(speed);
            assert_eq!(hot.speed(), speed);
            assert_eq!(hot.mcycle(), speed.mcycle());
            assert_eq!(hot.mcycle_shift(), speed.mcycle_shift());
            assert_eq!(hot.clock(), now, "{speed:?}");
            hot.set_countdown(now, 9);
            hot.t_minus += 1;
            assert_eq!(hot.clock(), now + speed.mcycle());
        }
    }

    /// The per-byte VRAM DMA source read before the block fast path: the
    /// reference for `vdma_block`.
    fn vdma_block_ref(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
        let hot = Hot::new();
        core::array::from_fn(|i| {
            let a = src.wrapping_add(i as u16);
            if a < 0x8000 || matches!(a & 0xE000, 0xA000 | 0xC000) {
                read::<false>(mem, sys, &hot, a)
            } else {
                0xFF
            }
        })
    }

    /// The VRAM DMA block fast path (`vdma_block`) reads what the per-byte
    /// path (`vdma_block_ref`) reads, for every 16-aligned source, in
    /// each source region and mapper state: ROM banks (one window ending
    /// mid-block), cartridge RAM disabled / banked / an RTC register,
    /// WRAM banks via SVBK (0 maps bank 1), VRAM and E000-FFFF.
    #[test]
    fn vdma_block_fast_path_matches_per_byte_reads() {
        // MBC3+TIMER+RAM+BATTERY, 32 KiB RAM; 0x8008 bytes, so ROM bank 2
        // is an 8-byte window.
        let r = rom(0x8008, 0x10, 0x03, 0x80);
        for (i, b) in r.iter_mut().enumerate().skip(0x150) {
            *b = (i * 7 + (i >> 8)) as u8;
        }
        let emu = load(r);
        for (i, b) in emu.mem.wram.iter_mut().enumerate() {
            *b = (i * 5 + (i >> 12) * 33) as u8;
        }
        for (i, b) in emu.mem.vram.iter_mut().enumerate() {
            *b = i as u8;
        }
        write(emu, 0x0000, 0x0A); // RAM and RTC enabled
        for i in 0..0x8000u32 {
            write(emu, 0x4000, (i >> 13) as u8);
            write(
                emu,
                0xA000 | (i & 0x1FFF) as u16,
                (i * 3 + (i >> 13) * 71) as u8,
            );
        }
        let check = |emu: &Emulator, what: &str| {
            for src in (0..=0xFFF0u16).step_by(16) {
                assert_eq!(
                    vdma_block(emu.mem, &emu.hw.sys, src),
                    vdma_block_ref(emu.mem, &emu.hw.sys, src),
                    "{what}: {src:04X}"
                );
            }
        };
        for (what, writes) in [
            (
                "ROM bank 1, RAM bank 0, SVBK 0",
                &[(0x2000, 1), (0x4000, 0), (0xFF70, 0)][..],
            ),
            (
                "ROM bank 2 (partial), RAM bank 2, SVBK 3",
                &[(0x2000, 2), (0x4000, 2), (0xFF70, 3)],
            ),
            (
                "RAM bank 3, SVBK 7",
                &[(0x2000, 1), (0x4000, 3), (0xFF70, 7)],
            ),
            ("RTC seconds", &[(0x4000, 0x08)]),
            ("RAM disabled", &[(0x4000, 1), (0x0000, 0x00)]),
        ] {
            for &(a, v) in writes {
                write(emu, a, v);
            }
            check(emu, what);
        }
        // The block path sees the WRAM bank: bank 7 at D000.
        assert_eq!(
            vdma_block(emu.mem, &emu.hw.sys, 0xD010)[0],
            emu.mem.wram[0x7010]
        );
    }

    #[test]
    fn clock_countdown_is_capped_and_exact() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF40, 0x11); // LCD off: no event scheduled
        write(emu, 0xFF26, 0x00); // APU off: no frame-sequencer events
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let start = emu.clock();
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // Nothing due: the countdown is capped to `i32::MAX` clocks, in
        // M-cycles (rounded up).
        const CAPPED: i32 = -(1 << 29);
        assert_eq!((ctx.hot.t_minus, ctx.clock()), (CAPPED, start));
        // Reaching a (capped) deadline mid-M-cycle just reschedules, and
        // the clock stays exact across it.
        ctx.hot.set_countdown(start, 6);
        assert_eq!((ctx.hot.t_minus, ctx.clock()), (-2, start));
        ctx.idle();
        assert_eq!((ctx.hot.t_minus, ctx.clock()), (-1, start + 4));
        ctx.idle();
        assert_eq!((ctx.hot.t_minus, ctx.clock()), (CAPPED, start + 8));
        assert!(!ctx.done);
        drop(ctx);
        assert_eq!(emu.clock(), start + 8);

        // A target ends the call at the first M-cycle boundary at or after
        // it.
        let now = emu.clock();
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, now + 13);
        for _ in 0..3 {
            ctx.idle();
        }
        assert!(!ctx.done);
        ctx.idle();
        assert!(ctx.done);
        assert_eq!(ctx.clock(), now + 16);
        assert_eq!(ctx.hot.t_minus, CAPPED);
    }

    #[test]
    #[cfg_attr(feature = "eager-sync", ignore = "the skip is one M-cycle there")]
    fn idle_halted_consumes_the_whole_countdown() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF40, 0x11); // LCD off: no event scheduled
        write(emu, 0xFF26, 0x00); // APU off: no frame-sequencer events
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // (M-cycle, budget before) -> clocks idled: up to the first
        // M-cycle boundary at or after the deadline, at least one M-cycle,
        // then the deadline is handled (the capped countdown is renewed).
        for (mcycle, budget, idled) in [
            (4, 6, 8),
            (4, 8, 8),
            (4, 1, 4),
            (4, 0, 4),
            (4, -3, 4),
            (2, 7, 8),
            (4, i32::MAX, 1 << 31),
            (2, i32::MAX, 1 << 31),
        ] {
            let start = ctx.clock();
            ctx.hot.set_speed(if mcycle == 2 {
                Speed::Double
            } else {
                Speed::Normal
            });
            if budget >= 0 {
                ctx.hot.set_countdown(start, budget as u64);
            } else {
                // A deadline passed by less than an M-cycle: the
                // countdown is at zero (the tick handles it next).
                ctx.hot.set_clock(start);
            }
            ctx.idle_halted();
            assert_eq!(ctx.clock() - start, idled, "{mcycle} {budget}");
            // Renewed: capped at `i32::MAX` clocks, in M-cycles.
            let capped = (i32::MAX as u32).div_ceil(mcycle as u32) as i32;
            assert_eq!(ctx.hot.t_minus, -capped, "{mcycle} {budget}");
        }
    }

    #[test]
    fn echo_ram_and_wram_banks() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xC123, 0x11);
        assert_eq!(emu.peek(0xE123), 0x11);
        write(emu, 0xFDFF, 0x22);
        assert_eq!(emu.peek(0xDDFF), 0x22);
        // CGB mode: SVBK selects the bank at D000; 0 means 1.
        write(emu, 0xD000, 0x01);
        write(emu, 0xFF70, 2);
        assert_eq!(emu.peek(0xFF70), 0xFA);
        assert_eq!(emu.peek(0xD000), 0x00);
        write(emu, 0xD000, 0x02);
        assert_eq!(emu.peek(0xF000), 0x02);
        write(emu, 0xFF70, 0);
        assert_eq!(emu.peek(0xD000), 0x01);
        // VBK.
        write(emu, 0x8000, 0xA0);
        write(emu, 0xFF4F, 1);
        assert_eq!(emu.peek(0xFF4F), 0xFF);
        assert_eq!(emu.peek(0x8000), 0x00);
        write(emu, 0xFF4F, 0);
        assert_eq!(emu.peek(0x8000), 0xA0);
    }

    #[test]
    fn cgb_registers_are_locked_in_dmg_compatibility_mode() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        write(emu, 0xD000, 0x01);
        write(emu, 0xFF70, 2);
        write(emu, 0xFF4F, 1);
        write(emu, 0xFF4D, 1);
        for reg in [0xFF4D, 0xFF70] {
            assert_eq!(emu.peek(reg), 0xFF, "{reg:#06x}");
        }
        // VBK is readable (bank 0) but not writable.
        assert_eq!(emu.peek(0xFF4F), 0xFE);
        assert_eq!(emu.peek(0xD000), 0x01);
        // BCPS/OCPS are readable and writable, BCPD/OCPD are not.
        write(emu, 0xFF68, 0x05);
        write(emu, 0xFF69, 0x12);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF69)), (0x45, 0xFF));
    }

    /// Interrupt dispatch, last M-cycle: the STAT bit is cleared half-way
    /// into it (`Bus::acknowledge`), so a STAT edge up to there is
    /// absorbed and one after it requests the interrupt again (gambatte
    /// `m2int_m2irq/m2int_m2irq_late_retrigger_{1,ds_1}`), in both speeds.
    #[test]
    fn dispatch_acknowledges_ppu_interrupts_half_way_into_its_last_mcycle() {
        // The mode-2 interrupt (STAT bit 5) of line 5 (t = 3) and of the
        // next frame's line 0 (t = 4, double speed 5: `edges::ly0_pulse`): on
        // the CPU's grid (M-cycles from clock 0 mod 4), the first is at an
        // M-cycle boundary and the second one clock past it.
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        let line0 = line5 + 149 * 456;
        // (M-cycle clocks, edge, last M-cycle start before it, absorbed)
        for (m, edge, before, absorbed) in [
            (4, line5 + 3, 4, false),
            (4, line0 + 4, 1, true),
            (2, line5 + 3, 2, false),
            // `lyc153int_m2irq/lyc153int_m2irq_late_retrigger_ds_1`: cleared
            // at 0:4, before the edge; `_ds_2`: at 0:6, after it.
            (2, line0 + 5, 2, false),
            (2, line0 + 5, 0, true),
        ] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let start = edge - before;
            let _ = emu.run_until(start - 40, &mut fb, &mut []);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.write(0xFF41, 0x20);
            let speed = if m == 2 { Speed::Double } else { Speed::Normal };
            ctx.hot.set_speed(speed);
            ctx.sys.ppu.speed_changed(speed);
            while ctx.clock() < start {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), start);
            ctx.sys.iflags = crate::ppu::INT_STAT;
            let _ = crate::cpu::Bus::push_acknowledge(&mut ctx, 0xC000, 0, crate::ppu::INT_STAT);
            let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
            assert_eq!(
                stat, !absorbed,
                "M-cycle {m}, edge {edge}, last from - {before}"
            );
        }
    }

    /// An IF write lands one dot after its access (SameBoy
    /// `GB_CONFLICT_WRITE_CPU`): it clears a mode-0 interrupt requested
    /// up to that dot, not one after it (CGB04C
    /// `m2int_m0irq/m2int_m0irq_scx3_ifw_*`). Single speed, the requests
    /// of line 153's LYC=0 point and line 0's mode-2 pulse (one dot past
    /// an M-cycle boundary here) reach IF a dot later: an IF write
    /// accessed the dot before them doesn't clear them
    /// (`ly0/lycint152_lyc0irq_ifw_1`,
    /// `lyc153int_m2irq/lyc153int_m2irq_ifw_1`).
    #[test]
    fn if_write_lands_one_dot_after_its_access() {
        let line1 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456;
        let fresh = |scx: u8, stat: u8, lyc: u8| {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line1 - 40, &mut fb, &mut []);
            write(emu, 0xFF43, scx);
            write(emu, 0xFF45, lyc);
            write(emu, 0xFF41, stat);
            emu
        };
        // The mode-0 interrupt's dot against the M-cycle grid, per SCX:
        // cleared exactly when it is one dot past the write's access.
        let mut seen = [false; 2];
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        // A fresh emulator's context at M-cycle boundary `at`.
        macro_rules! ctx_at {
            ($ctx:ident, $emu:expr, $at:expr) => {
                let emu = $emu;
                let Emulator { mem, hw } = &mut *emu;
                let mut $ctx =
                    Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
                while $ctx.clock() < $at {
                    $ctx.idle();
                }
            };
        }
        for scx in 0..8 {
            let b = {
                ctx_at!(ctx, fresh(scx, 0x08, 0xFF), line1 + 200);
                ctx.sys.iflags = 0;
                let mut b = ctx.clock();
                while ctx.sys.iflags & crate::ppu::INT_STAT == 0 {
                    b = ctx.clock();
                    ctx.idle();
                }
                b
            };
            // The interrupt's dot: in (b, b + 4].
            let e = {
                ctx_at!(ctx, fresh(scx, 0x08, 0xFF), b);
                ctx.sys.iflags = 0;
                let mut e = b + 1;
                ctx.run_ppu_to_call(e);
                while ctx.sys.iflags & crate::ppu::INT_STAT == 0 {
                    e += 1;
                    ctx.run_ppu_to_call(e);
                }
                e
            };
            // An IF write accessed at `b`.
            ctx_at!(ctx, fresh(scx, 0x08, 0xFF), b);
            crate::cpu::Bus::write(&mut ctx, 0xFF0F, 0);
            let cleared = ctx.sys.iflags & crate::ppu::INT_STAT == 0;
            assert_eq!(cleared, e == b + 1, "SCX {scx}: access {b}, interrupt {e}");
            seen[usize::from(cleared)] = true;
        }
        assert_eq!(seen, [true, true]);
        // (STAT, LYC, interrupt dot): line 153's LYC=0 at 12, line 0's
        // mode-2 pulse at 4; the IF write accessed a dot before it.
        let line0 = line1 + 153 * 456;
        for (stat, lyc, edge) in [(0x40, 0, line0 - 456 + 12), (0x20, 0xFF, line0 + 4)] {
            ctx_at!(ctx, fresh(0, stat, lyc), edge - 1);
            assert_eq!(ctx.clock(), edge - 1);
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF0F, 0);
            assert_ne!(ctx.sys.iflags & crate::ppu::INT_STAT, 0, "STAT {stat:02X}");
        }
    }

    /// A dispatch's low-byte push onto IF (SP = FF10) chooses the vector
    /// from IF as it was before that write (SameBoy `cycle_write_if`;
    /// gambatte cgb04c `irq_precedence/if_and_ie_0_vector_4`). A push
    /// elsewhere sees IF as it is after the write.
    #[test]
    fn dispatch_push_onto_if_chooses_from_the_old_if() {
        for (addr, pending) in [(0xFF0F, INT_TIMER), (0xC000, 0)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.sys.iflags = INT_TIMER;
            if addr != 0xFF0F {
                // The same IF change, by another access.
                ctx.write(0xFF0F, 0x80);
            }
            let got = crate::cpu::Bus::push_acknowledge(&mut ctx, addr, 0x80, 0x1F);
            assert_eq!(got, pending, "push to {addr:04X}");
            assert_eq!(ctx.sys.iflags, 0, "push to {addr:04X}");
        }
    }

    /// CPU accesses to VRAM in mode 3 and to OAM in modes 2-3 are blocked;
    /// `peek` and the OAM DMA's reads are not.
    #[test]
    fn ppu_blocks_cpu_vram_and_oam_access() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.mem.vram[0] = 0x12;
        emu.mem.oam[0] = 0x34;
        // Line 5 of the first full frame, 120 dots in: mode 3.
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        let _ = emu.run_until(line5 + 120, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF41) & 3, 3);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0xFF);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE00), 0xFF);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFEA0), 0xFF);
        ctx.write_mapped(0x8000, 0x56);
        ctx.write_mapped(0xFE00, 0x78);
        assert_eq!((ctx.mem.vram[0], ctx.mem.oam[0]), (0x12, 0x34));
        // An OAM DMA from VRAM reads it anyway.
        assert_eq!(read::<false>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0x12);
        drop(ctx);
        assert_eq!((emu.peek(0x8000), emu.peek(0xFE00)), (0x12, 0x34));
        // HBlank of the same line: free.
        let _ = emu.run_until(line5 + 300, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF41) & 3, 0);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0x8000), 0x12);
        ctx.write_mapped(0x8000, 0x56);
        ctx.write_mapped(0xFE00, 0x78);
        assert_eq!((ctx.mem.vram[0], ctx.mem.oam[0]), (0x56, 0x78));
    }

    #[test]
    fn sound_registers_read_their_unused_bits_as_1() {
        let emu = load(rom(0x8000, 0, 0, 0x00));
        for (reg, ones) in [
            (0xFF10, 0x80),
            (0xFF15, 0xFF),
            (0xFF1C, 0x9F),
            (0xFF26, 0x70),
        ] {
            write(emu, reg, 0x00);
            assert_eq!(emu.peek(reg), ones, "{reg:#06x}");
        }
        write(emu, 0xFF30, 0x00);
        assert_eq!(emu.peek(0xFF30), 0x00);
        assert_eq!((emu.peek(0xFF76), emu.peek(0xFF77)), (0x00, 0x00));
    }

    #[test]
    fn unusable_area_is_cgb_c_ram_without_address_bits_3_and_4() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFEA0, 0x11);
        write(emu, 0xFEC5, 0x22);
        write(emu, 0xFEE7, 0x33);
        for (addr, v) in [
            (0xFEA8, 0x11),
            (0xFEB0, 0x11),
            (0xFEB8, 0x11),
            (0xFEDD, 0x22),
            (0xFEFF, 0x33),
            (0xFEA1, 0x00),
        ] {
            assert_eq!(emu.peek(addr), v, "{addr:#06x}");
        }
    }

    #[test]
    fn joypad_reads_the_selected_group() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        emu.set_buttons(Buttons::A | Buttons::DOWN);
        assert_eq!(emu.peek(0xFF00), 0xC0 | 0x06); // both groups selected
        write(emu, 0xFF00, 0x10); // buttons
        assert_eq!(emu.peek(0xFF00), 0xD0 | 0x0E);
        write(emu, 0xFF00, 0x20); // d-pad
        assert_eq!(emu.peek(0xFF00), 0xE0 | 0x07);
        write(emu, 0xFF00, 0x30); // neither
        assert_eq!(emu.peek(0xFF00), 0xFF);
    }

    #[test]
    fn mbc1_rom_banking() {
        // 1 MiB: 64 banks, each starting with its own number.
        let r = rom(0x10_0000, 0x01, 0, 0);
        for bank in 1..64 {
            r[bank * 0x4000] = bank as u8;
        }
        r[0x104] = 1; // not a multicart: no logo copy in bank 0x10
        let emu = load(r);
        assert_eq!(emu.peek(0x4000), 1);
        write(emu, 0x2000, 0x00); // 0 selects 1
        assert_eq!(emu.peek(0x4000), 1);
        write(emu, 0x2000, 0x05);
        assert_eq!(emu.peek(0x4000), 5);
        write(emu, 0x2000, 0xE7); // 5 bits: 0x07
        assert_eq!(emu.peek(0x4000), 7);
        write(emu, 0x4000, 0x01); // BANK2 = 1: bank 0x27
        assert_eq!(emu.peek(0x4000), 0x27);
        assert_eq!(emu.peek(0x0000), 0x00); // mode 0: bank 0 at 0000
        write(emu, 0x6000, 0x01); // mode 1: bank 0x20 at 0000
        assert_eq!(emu.peek(0x0000), 0x20);
        // 0x20 selects 0x21 only through the 5-bit zero check: 0x20 itself
        // can't be mapped at 4000.
        write(emu, 0x2000, 0x00);
        assert_eq!(emu.peek(0x4000), 0x21);
    }

    #[test]
    fn mbc1_bank_numbers_wrap_to_the_rom_size() {
        // 256 KiB: 16 banks; bank 0x15 mirrors bank 5.
        let r = rom(0x4_0000, 0x01, 0, 0);
        for bank in 1..16 {
            r[bank * 0x4000] = bank as u8;
        }
        let emu = load(r);
        write(emu, 0x2000, 0x15);
        assert_eq!(emu.peek(0x4000), 5);
        write(emu, 0x4000, 0x03);
        assert_eq!(emu.peek(0x4000), 5);
    }

    #[test]
    fn mbc1_ram_enable_and_banks() {
        let emu = load(rom(0x8000, 0x03, 0x03, 0)); // 32 KiB RAM
        write(emu, 0xA000, 0x55);
        assert_eq!(emu.peek(0xA000), 0xFF, "disabled");
        write(emu, 0x0000, 0x0A);
        write(emu, 0xA000, 0x55);
        assert_eq!(emu.peek(0xA000), 0x55);
        write(emu, 0x4000, 0x02); // BANK2 = 2, but mode 0: still bank 0
        assert_eq!(emu.peek(0xA000), 0x55);
        write(emu, 0x6000, 0x01); // mode 1: RAM bank 2
        assert_eq!(emu.peek(0xA000), 0x00);
        write(emu, 0xA001, 0x66);
        assert_eq!(emu.cart_ram()[2 * 0x2000 + 1], 0x66);
        write(emu, 0x0000, 0x00);
        assert_eq!(emu.peek(0xA001), 0xFF);
    }

    #[test]
    fn joypad_interrupt_on_a_line_going_low() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let joyp_if = |emu: &mut Emulator| {
            let v = emu.peek(0xFF0F) & 0x10 != 0;
            write(emu, 0xFF0F, 0);
            v
        };
        write(emu, 0xFF0F, 0);
        write(emu, 0xFF00, 0x10); // action buttons selected
        emu.set_buttons(Buttons::A);
        assert!(joyp_if(emu), "A pressed");
        emu.set_buttons(Buttons::A | Buttons::B);
        assert!(joyp_if(emu), "B pressed while A held");
        emu.set_buttons(Buttons::A);
        assert!(!joyp_if(emu), "release: line goes high");
        emu.set_buttons(Buttons::A | Buttons::UP);
        assert!(!joyp_if(emu), "d-pad not selected");
        // Selecting the d-pad exposes the held Up: P12 goes low.
        write(emu, 0xFF00, 0x20);
        assert!(joyp_if(emu), "select exposes Up");
        // Deselecting raises the lines; selecting both groups lowers P10
        // (A) and P12 (Up) again.
        write(emu, 0xFF00, 0x30);
        assert!(!joyp_if(emu));
        write(emu, 0xFF00, 0x00);
        assert!(joyp_if(emu));
        // Same lines low as before: no edge.
        write(emu, 0xFF00, 0x00);
        assert!(!joyp_if(emu));
        // Pressing Right while A already holds P10 low: no edge.
        emu.set_buttons(Buttons::A | Buttons::UP | Buttons::RIGHT);
        assert!(!joyp_if(emu));
    }

    #[test]
    fn rp_infrared_port_reads_back_its_writable_bits() {
        // FF56 (Pan Docs: "FF56 - RP"; SameBoy `read_high_memory`,
        // `GB_IO_RP`): bits 0 (LED), 6 and 7 (read enable) read back, bits
        // 2-5 read 1, and bit 1 reads 1 (no signal) unless reading is
        // enabled (bits 6-7 = 3) and light arrives. There is no peer, so
        // the only light is the port's own LED.
        let emu = load(rom(0x8000, 0, 0, 0x80));
        assert_eq!(emu.peek(0xFF56), 0x3E, "after boot");
        for (w, r) in [
            (0xFF, 0xFD),
            (0x00, 0x3E),
            (0xC0, 0xFE),
            (0x01, 0x3F),
            (0x80, 0xBE),
            (0x41, 0x7F),
            (0x3E, 0x3E),
            (0xC1, 0xFD),
        ] {
            write(emu, 0xFF56, w);
            assert_eq!(emu.peek(0xFF56), r, "after writing {w:#04x}");
        }
        // CGB mode only: FF in DMG compatibility mode (SameBoy: `if
        // (!gb->cgb_mode) return 0xFF`).
        let emu = load(rom(0x8000, 0, 0, 0x00));
        assert_eq!(emu.peek(0xFF56), 0xFF);
        write(emu, 0xFF56, 0x00);
        assert_eq!(emu.peek(0xFF56), 0xFF);
    }

    #[test]
    fn undocumented_cgb_registers() {
        // CGB mode (SameBoy Core/memory.c read_high_memory).
        let emu = load(rom(0x8000, 0, 0, 0x80));
        for (reg, v) in [
            (0xFF6C, 0xFE),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x} after boot");
        }
        for reg in [0xFF6C, 0xFF72, 0xFF73, 0xFF74, 0xFF75] {
            write(emu, reg, 0xFF);
        }
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0xFF),
            (0xFF73, 0xFF),
            (0xFF74, 0xFF),
            (0xFF75, 0xFF),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
        for reg in [0xFF6C, 0xFF72, 0xFF73, 0xFF74, 0xFF75] {
            write(emu, reg, 0x00);
        }
        for (reg, v) in [
            (0xFF6C, 0xFE),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
        write(emu, 0xFF72, 0x5A);
        write(emu, 0xFF73, 0xA5);
        write(emu, 0xFF75, 0x50);
        assert_eq!(
            (emu.peek(0xFF72), emu.peek(0xFF73), emu.peek(0xFF75)),
            (0x5A, 0xA5, 0xDF)
        );

        // DMG compatibility mode: the values Mooneye boot_hwio-C expects;
        // OPRI and FF74 are locked, FF72/FF73/FF75 still work.
        let emu = load(rom(0x8000, 0, 0, 0x00));
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0),
            (0xFF73, 0),
            (0xFF74, 0xFF),
            (0xFF75, 0x8F),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x} after boot");
        }
        write(emu, 0xFF6C, 0x00);
        write(emu, 0xFF72, 0x12);
        write(emu, 0xFF74, 0x34);
        write(emu, 0xFF75, 0x70);
        for (reg, v) in [
            (0xFF6C, 0xFF),
            (0xFF72, 0x12),
            (0xFF74, 0xFF),
            (0xFF75, 0xFF),
        ] {
            assert_eq!(emu.peek(reg), v, "{reg:#06x}");
        }
    }
}

#[cfg(test)]
mod dma_tests {
    //! OAM DMA timing, blocking and CGB C conflicts (`docs/reference/oam-dma.md`).
    extern crate std;
    use super::test_util::{idle_to, setup, setup_with, with_ctx};
    use super::*;
    use crate::Frame;
    use std::boxed::Box;

    #[test]
    fn start_delay_blocking_and_end() {
        let emu = setup();
        let mut t0 = 0;
        with_ctx(emu, |ctx| {
            t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // M-cycle 0
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + 162 * 4);
            // M-cycle 1, the start delay: OAM is readable and untouched.
            assert_eq!(ctx.read(0xFE00), 0x55);
            // M-cycle 2: byte 0 transferred, OAM blocked; HRAM is not.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.read(0xFEA5), 0xFF);
            ctx.write(0xFF80, 0x12);
            assert_eq!(ctx.read(0xFF80), 0x12);
            // OAM writes are dropped.
            ctx.write(0xFE30, 0x99);
            ctx.write(0xFEA0, 0x99);
            idle_to(ctx, t0, 10);
        });
        // Between run calls OAM is synced to the clock: bytes 0-8 are in
        // (byte 8 counts from the access at M-cycle 10), the rest are not.
        // `peek` sees OAM as it is, without FF or conflicts.
        assert_eq!(emu.peek(0xFE00), 0xA5);
        assert_eq!(emu.peek(0xFE08), 8 ^ 0xA5);
        assert_eq!(emu.peek(0xFE09), 0x55);
        assert_eq!(emu.peek(0xC000), emu.mem.wram[0]);
        assert_eq!(emu.peek(0xC105), 5 ^ 0xA5);
        with_ctx(emu, |ctx| {
            idle_to(ctx, t0, 161);
            // The last transfer M-cycle: still blocked.
            assert_eq!(ctx.read(0xFE9F), 0xFF);
            // M-cycle 162: done.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFE9F), 0x9F ^ 0xA5);
            assert_eq!(ctx.read(0xFEA0), 0);
            ctx.write(0xFE30, 0x99);
            assert_eq!(ctx.read(0xFE30), 0x99);
        });
        for (i, &b) in emu.mem.oam.iter().enumerate() {
            if i != 0x30 {
                assert_eq!(b, i as u8 ^ 0xA5, "{i:#x}");
            }
        }
    }

    #[test]
    fn restart_keeps_oam_blocked_through_the_new_delay() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0xC2); // M-cycle 20
            // The new start delay: the old transfer still blocks OAM.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + (20 + 162) * 4);
            // The new transfer from C200.
            idle_to(ctx, t0, 20 + 162);
            assert_eq!(ctx.read(0xFE00), 0xA5);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// The M-cycle after a restarting FF46 write: the old transfer moves
    /// its next byte from the new source, so a CPU access on the new
    /// source's bus conflicts with that byte (gambatte cgb04c
    /// `oamdma_src8000_srcchange0000_busyinc`).
    #[test]
    fn restart_moves_the_old_byte_from_the_new_source() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0x40); // M-cycle 20: restart from ROM 4000
            // M-cycle 21: the old transfer's byte 19, from 4013.
            assert_eq!(ctx.read(0x0123), 19 ^ 0x3C);
            // M-cycle 22: the new transfer's byte 0.
            assert_eq!(ctx.read(0x0123), 0x3C);
            // The new transfer copies all 160 bytes from 4000 as usual.
            idle_to(ctx, t0, 190);
            assert_eq!(ctx.hot.dma, 0);
            for (i, &b) in ctx.mem.oam.iter().enumerate() {
                assert_eq!(b, i as u8 ^ 0x3C, "{i:#x}");
            }
            // Restarted in its own start delay: byte 0 in the next M-cycle.
            idle_to(ctx, t0, 200);
            ctx.write(0xFF46, 0xC1); // M-cycle 200
            ctx.write(0xFF46, 0x40); // 201
            assert_eq!(ctx.read(0x0123), 0x3C); // 202: byte 0 from 4000
            assert_eq!(ctx.read(0x0123), 0x3C); // 203: the new byte 0
        });
        // A fresh start conflicts with nothing in its start delay.
        let emu = setup();
        with_ctx(emu, |ctx| {
            ctx.write(0xFF46, 0x40);
            assert_eq!(ctx.read(0x0123), 0);
            assert_eq!(ctx.read(0x0123), 0x3C);
        });
    }

    #[test]
    fn restart_in_the_last_mcycle_is_a_fresh_start() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.write(0xFF46, 0xC1); // the old transfer's last M-cycle
            assert_eq!(ctx.read(0xFE10), 0x10 ^ 0xA5);
            assert_eq!(ctx.read(0xFE10), 0xFF);
        });
    }

    /// OAM DMA steps inside a VRAM DMA write the VRAM DMA's byte of that
    /// moment to OAM at its source address & FF, not their own (gambatte
    /// cgb04c `dma/hdma_transition_oamdma_1`).
    #[test]
    fn oam_dma_steps_during_a_vram_dma_take_its_bytes() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // bytes i ^ A5
            for (reg, v) in [(0xFF51, 0x40), (0xFF52, 0), (0xFF53, 0), (0xFF54, 0)] {
                ctx.write(reg, v);
            }
            ctx.write(0xFF55, 0x00); // a 1-block GDMA from ROM 4000
            idle_to(ctx, t0, 10);
            // At M-cycle 10 (bytes 0-8 due): the block's 8 M-cycles hold
            // the steps of bytes 9-16, which take block bytes 1, 3, .. 15.
            ctx.vram_dma(true);
            idle_to(ctx, t0, 170);
        });
        for (p, &b) in emu.mem.oam.iter().enumerate() {
            let want = match p {
                1..=15 if p % 2 == 1 => p as u8 ^ 0x3C,
                10 | 12 | 14 | 16 => 0x55,
                _ => p as u8 ^ 0xA5,
            };
            assert_eq!(b, want, "{p:#x}");
        }
    }

    #[test]
    fn wram_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            // The start delay: no conflict.
            assert_eq!(ctx.read(0xC000), ctx.mem.wram[0]);
            idle_to(ctx, t0, 7);
            // WRAM reads see the DMA's byte (M-cycle 7: byte 5, C105); ROM
            // and VRAM are free.
            assert_eq!(ctx.read(0xD000), 5 ^ 0xA5);
            assert_eq!(ctx.read(0x4001), 1 ^ 0x3C);
            // The address the DMA reads next is not a conflict.
            assert_eq!(ctx.read(0xC108), 8 ^ 0xA5); // M-cycle 9: byte 7
            // WRAM writes are dropped.
            let before = ctx.mem.wram[0x10];
            ctx.write(0xC010, 0x77);
            assert_eq!(ctx.mem.wram[0x10], before);
            // A VRAM write is free.
            ctx.write(0x8000, 0x66);
            assert_eq!(ctx.mem.vram[0], 0x66);
        });
    }

    #[test]
    fn rom_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0x40);
            idle_to(ctx, t0, 4);
            // External bus: the DMA's byte (M-cycle 4: byte 2, 4002).
            assert_eq!(ctx.read(0x0150), 2 ^ 0x3C);
            // WRAM: redirected to the DMA's bank half (bit 12 clear).
            ctx.mem.wram[0x123] = 0x42;
            assert_eq!(ctx.read(0xD123), 0x42);
            ctx.write(0xD124, 0x43); // M-cycle 6, goes to C124
            assert_eq!(ctx.mem.wram[0x124], 0x43);
            // An external-bus write is dropped, and its data replaces the
            // OAM byte just transferred (byte 5; CGB C, ADR 0028).
            ctx.write(0x2000, 0x05); // M-cycle 7
            assert_eq!(ctx.mem.oam[5], 0x05);
            assert_eq!(ctx.mem.oam[4], 4 ^ 0x3C);
            assert_eq!(ctx.sys.cart.read_rom(0x4000), 0x3C); // bank unchanged
            // VRAM is free.
            ctx.mem.vram[5] = 0x31;
            assert_eq!(ctx.read(0x8005), 0x31);
        });
    }

    #[test]
    fn high_source_reads_ff_and_opens_the_external_bus() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xFE);
            idle_to(ctx, t0, 3);
            assert_eq!(ctx.read(0x0150), 0xFF);
            ctx.mem.wram[0x1456] = 0x24;
            assert_eq!(ctx.read(0xC456), 0x24); // FE00: bit 12 set, D456
            idle_to(ctx, t0, 162);
        });
        assert!(emu.mem.oam.iter().all(|&b| b == 0xFF));
    }

    /// Without HALT (STOP, lockup: not paused, `docs/reference/oam-dma.md`)
    /// the idle skip stops at the `Dma` deadline like any other.
    #[test]
    fn unhalted_skip_stops_at_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            assert_eq!(ctx.hot.dma, 0xFE00);
            // Other deadlines may come first (in CGB mode the first
            // DIV-APU edge is 352 clocks after power on, ADR 0022): skip
            // until the transfer ends; no skip may pass its deadline.
            // Each skip advances the clock, so a few dozen iterations reach
            // the deadline; the cap turns a stuck skip into a failure.
            let mut skips = 0;
            while ctx.hot.dma != 0 {
                skips += 1;
                assert!(
                    skips <= 1000,
                    "idle skip made no progress: dma still {:#06X} after {skips} skips, clock {} (deadline {})",
                    ctx.hot.dma,
                    ctx.clock(),
                    t0 + 162 * 4
                );
                ctx.idle_halted();
                assert!(ctx.clock() <= t0 + 162 * 4);
            }
            assert_eq!(ctx.clock(), t0 + 162 * 4);
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// HALT in M-cycle 10 of a transfer, 50 halted M-cycles, wake-up. The
    /// transfer makes no progress while halted, OAM stays blocked, and the
    /// end moves out by the paused time (SameBoy `GB_dma_run`).
    #[test]
    fn halt_pauses_the_transfer() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            ctx.read(0x0100); // HALT's fetch: its step counts
            ctx.halt_begin();
            let f = ctx.clock();
            assert_eq!(ctx.sys.ppu.dma.copied, 10);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), NEVER);
            for _ in 0..50 {
                ctx.idle();
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.sys.ppu.dma.copied, 10);
                assert_eq!(ctx.hot.dma, 0xFE00);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE00), 0xFF);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE9F), 0xFF);
            }
            assert_eq!(ctx.mem.oam[10], 0x55);
            // The wake-up M-cycle: pushed out by 50 M-cycles.
            ctx.halt_end();
            assert_eq!(ctx.clock(), f + 50 * 4);
            let end = t0 + (162 + 50) * 4;
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), end);
            ctx.idle();
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 11);
            while ctx.clock() < end - 4 {
                ctx.idle();
            }
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.idle();
            assert_eq!((ctx.clock(), ctx.hot.dma), (end, 0));
        });
        assert!((0..0xA0).all(|i| emu.mem.oam[i] == i as u8 ^ 0xA5));
    }

    /// STOP mode pauses the transfer like HALT (SameBoy `GB_dma_run`
    /// returns while `stopped`; `leave_stop_mode` takes one step).
    #[test]
    fn stop_mode_pauses_the_transfer() {
        // P1 = 10 (action buttons); IF = 0; LD A,C1; LDH (46),A; STOP 00;
        // LD B,B.
        let emu = setup_with(&[
            0x3E, 0x10, 0xE0, 0x00, 0xAF, 0xE0, 0x0F, 0x3E, 0xC1, 0xE0, 0x46, 0x10, 0x00, 0x40,
        ]);
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        for _ in 0..7 {
            let _ = emu.step(&mut fb, &mut []);
        }
        assert_eq!(emu.hw.cpu.registers().pc, 0x10D);
        // A byte per M-cycle from the one after the FF46 write's: STOP's
        // fetch and its second byte.
        let copied = emu.hw.sys.ppu.dma.copied;
        assert_eq!(copied, 2);
        let _ = emu.run_cycles(10_000, &mut fb, &mut []);
        assert_eq!(emu.hw.sys.ppu.dma.copied, copied);
        assert_eq!(emu.hw.sys.hot.dma, 0xFE00);
        assert_eq!(emu.mem.oam[usize::from(copied)], 0x55);
        emu.set_buttons(Buttons::A);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(1000, &mut fb, &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        // The two wake-up M-cycles and LD B,B's fetch: three more bytes.
        assert_eq!(emu.hw.sys.ppu.dma.copied, copied + 3);
        let _ = emu.run_cycles(4 * 160, &mut fb, &mut []);
        assert_eq!(emu.hw.sys.hot.dma, 0);
        assert!((0..0xA0).all(|i| emu.mem.oam[i] == i as u8 ^ 0xA5));
    }

    /// A CPU write on the bus the transfer reads, in the M-cycle of byte
    /// 5: it never reaches its target, and OAM byte 5 gets the CPU's data
    /// (ROM/SRAM and E000-FFFF sources), 00 (VRAM source) or keeps the
    /// DMA's byte (WRAM source). gambatte cgb04c `oamdma_src*_busypush*`,
    /// ADR 0028.
    #[test]
    fn conflicting_write_lands_in_oam_by_source() {
        // (FF46 value, CPU write address, OAM byte 5 afterwards).
        let cases = [
            (0x40, 0xA123, 0x9A),
            (0x40, 0x7FFF, 0x9A),
            (0x80, 0x9123, 0x00),
            (0xC1, 0xD123, 5 ^ 0xA5),
            (0xFE, 0x2000, 0x9A),
            (0xFE, 0xB000, 0x9A),
        ];
        for (src, addr, want) in cases {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                ctx.write(addr, 0x9A);
                ctx.dma_sync(ctx.clock());
            });
            assert_eq!(emu.mem.oam[5], want, "source {src:02X}, write {addr:04X}");
            match addr {
                0x9123 => assert_eq!(emu.mem.vram[0x1123], 0),
                0xD123 => assert_eq!(emu.mem.wram[0x1123], 0),
                _ => {}
            }
        }
    }

    /// A CPU read on the VRAM bus during a transfer from VRAM returns the
    /// byte being transferred and clears that OAM byte (gambatte cgb04c
    /// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, ADR 0028 amendment):
    /// byte 5 (not yet copied when read), byte 6 (copied by a sync at the
    /// same clock, before the read) and byte 40 (a second word of the
    /// record), all applied by the next sync. A WRAM source's conflicting
    /// read leaves OAM alone.
    #[test]
    fn conflicting_vram_read_clears_the_oam_byte() {
        for (src, addr) in [(0x80u8, 0x9123), (0xC1, 0xD123)] {
            let emu = setup();
            for (i, b) in emu.mem.vram[..0xA0].iter_mut().enumerate() {
                *b = i as u8 ^ 0x69;
            }
            let byte = |i: usize| {
                if src == 0x80 {
                    i as u8 ^ 0x69
                } else {
                    i as u8 ^ 0xA5
                }
            };
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                assert_eq!(ctx.read(addr), byte(5));
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.read(addr), byte(6));
                idle_to(ctx, t0, 2 + 40);
                assert_eq!(ctx.read(addr), byte(40));
                ctx.idle();
                ctx.dma_sync(ctx.clock());
            });
            let cleared = |i: usize| src == 0x80 && matches!(i, 5 | 6 | 40);
            for (i, &b) in emu.mem.oam[..=41].iter().enumerate() {
                let want = if cleared(i) { 0 } else { byte(i) };
                assert_eq!(b, want, "source {src:02X}, byte {i}");
            }
            assert!(emu.hw.sys.dma_read_clears.is_empty());
        }
    }

    /// The halted skip passes the (cleared) `Dma` deadline: a paused
    /// transfer is no deadline (ADR 0011).
    #[test]
    #[cfg_attr(feature = "eager-sync", ignore = "the skip is one M-cycle there")]
    fn halted_skip_passes_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // APU off: no frame-sequencer events.
            ctx.write(0xFF26, 0x00);
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 3);
            ctx.halt_begin();
            ctx.idle_halted();
            assert!(ctx.clock() > t0 + 1_000_000);
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 2);
        });
    }

    /// The CPU's hooks: `LD A,C1; LDH (46),A; HALT`, 40 halted M-cycles, an
    /// IRQ with IME off, then `LD A,(D000)`, which conflicts with the WRAM
    /// source and reads the byte being transferred. SameBoy: the HALT's
    /// fetch M-cycle steps (byte 0), the halted ones including the wake-up
    /// M-cycle don't, the wake-up takes one step (byte 1), and the three
    /// M-cycles before the read step (bytes 2-4).
    #[test]
    fn wake_up_takes_one_dma_step() {
        let emu = setup_with(&[0x3E, 0xC1, 0xE0, 0x46, 0x76, 0xFA, 0x00, 0xD0, 0x18, 0xFE]);
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        let cpu = &mut hw.cpu;
        cpu.set_ime(false);
        ctx.sys.ie = 0x01;
        ctx.sys.iflags = 0;
        cpu.step(&mut ctx);
        cpu.step(&mut ctx);
        let w = ctx.clock() - 4;
        cpu.step(&mut ctx);
        assert!(cpu.halted());
        assert_eq!(ctx.clock(), w + 8);
        for _ in 0..40 {
            ctx.idle();
        }
        ctx.sys.iflags = 0x01;
        cpu.step(&mut ctx); // wake-up M-cycle, LD A,(D000)
        assert!(!cpu.halted());
        assert_eq!(cpu.registers().a, 4 ^ 0xA5);
        assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), w + (162 + 40) * 4);
    }

    /// HALT with bytes left: the PPU sees the transfer halted until the
    /// end of the wake-up M-cycle (SameBoy clears `halted` after it), then
    /// running again, moved out by the halted time.
    #[test]
    fn halt_holds_the_ppu_window_while_bytes_are_left() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            let p = ctx.clock();
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(p), (Running, 9));
            assert_eq!(d.ppu_at(p + 1), (Halted, 9));
            assert_eq!(d.ppu_at(p + 5000), (Halted, 9));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 4), (Halted, 9));
            assert_eq!(d.ppu_at(r + 5), (Running, 10));
            assert_eq!(d.ppu_to, d.end - 2);
            assert_eq!(d.ppu_at(d.end - 2), (Running, 160));
            assert_eq!(d.ppu_at(d.end - 1), (Off, 160));
        });
    }

    /// The speed-switch halt after the last byte, before the end: unlike
    /// HALT, the transfer's last step is held for the PPU through the
    /// halt (gambatte cgb04c `oamdma/oamdma_late_speedchange_stat_1`).
    #[test]
    fn speed_switch_after_the_last_byte_holds_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            assert!(ctx.speed_switch(true, false));
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.copied, 160);
            assert_eq!(d.ppu_at(t0 + 647), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 5000), (Halted, 160));
        });
    }

    /// A switch to double speed with the APU on delays the APU by one
    /// 2 MHz tick (`apu::Fs::ds_delay`): a CH1 trigger in double speed
    /// then takes the other of its two delays than the power-on phase
    /// gives (gambatte cgb04c
    /// `speedchange_ch1_nr4init_duty0_pos6_to_pos7_timing_1`/`_2`). A
    /// power cycle in double speed clears the delay.
    #[test]
    fn a_switch_to_double_speed_with_the_apu_on_flips_the_trigger_phase() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // CH1 DAC on, f = 0x700 (1024 clocks per step), triggered off
            // at two consecutive M-cycles: the clocks to the first step
            // against gambatte's linear rule with the power-on 1 MHz phase
            // `lf_div` (`Apu::trigger_delay`), flipped by `flip`.
            let check = |ctx: &mut Ctx, flip: u64| {
                ctx.write(0xFF12, 0xF0);
                for _ in 0..2 {
                    let t = ctx.clock();
                    ctx.write(0xFF14, 0x87);
                    let apu = &ctx.mem.apu;
                    let first = apu.out.synced + u64::from(apu.sq[0].countdown);
                    let lf_div = ((t >> 1) ^ u64::from(apu.fs.phase) ^ 1) & 1;
                    assert_eq!(first - t, 1024 + 6 + 2 * (lf_div ^ flip), "at {t}");
                    // Off again (DAC off, on) for the next trigger.
                    ctx.write(0xFF12, 0x00);
                    ctx.write(0xFF12, 0xF0);
                }
            };
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(ctx.speed_switch(true, false));
            while !ctx.speed_switch_over() {
                ctx.idle_speed_switch();
            }
            ctx.speed_switch_end();
            assert!(ctx.mem.apu.fs.ds_delay);
            check(ctx, 1);
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(!ctx.mem.apu.fs.ds_delay);
            check(ctx, 0);
        });
    }

    /// HALT after the last byte: the transfer's last step isn't held for
    /// the PPU, which sees it end on time (gambatte
    /// `oamdma/oamdma_late_halt_stat_2`, cgb04c).
    #[test]
    fn halt_after_the_last_byte_does_not_hold_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 646), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 647), (Off, 160));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            assert_eq!(ctx.sys.ppu.dma.ppu_at(r + 1), (Off, 160));
        });
    }

    /// HALT in the start delay: the transfer hasn't started for the PPU,
    /// and starts only after the wake-up.
    #[test]
    fn halt_in_the_start_delay_delays_the_ppu_window() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 7), (Off, 0));
            assert_eq!(d.ppu_at(t0 + 5000), (Off, 0));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 2), (Off, 0));
            assert_eq!(d.ppu_at(r + 5), (Running, 1));
        });
    }

    /// STAT reads mode 0 instead of 2 while a transfer runs (SameBoy
    /// `GB_STAT_update`); modes 3 and 0 read as usual, and mode 2 again
    /// once the transfer is over.
    #[test]
    fn stat_reads_mode_2_as_0_during_oam_dma() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // LCD on at E: frame position = clock - E + 7, so the access at
            // E + 4k is at position 4k + 7.
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, 100);
            ctx.write(0xFF46, 0xC1); // Line 0 (the short first line), HBlank.
            idle_to(ctx, e, 118); // Line 1, t = 23: mode 2.
            assert_eq!(ctx.read(0xFF41) & 3, 0);
            idle_to(ctx, e, 137); // Line 1, t = 99: mode 3.
            assert_eq!(ctx.read(0xFF41) & 3, 3);
            idle_to(ctx, e, 346); // Line 3, t = 23: mode 2, transfer over.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFF41) & 3, 2);
        });
    }

    /// An HBlank trigger while a block's CPU stall runs is lost; one at the
    /// stall's end starts the next block (gambatte acknowledges the
    /// request at a transfer's end; ADR 0017, trigger amendment).
    #[test]
    fn an_hblank_trigger_during_a_block_stall_is_lost() {
        for (at, kept) in [(8, false), (12, true)] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                ctx.write(0xFF40, 0x91);
                ctx.sys.vdma.hblank = true;
                ctx.sys.vdma.stall = 3;
                let now = ctx.clock();
                ctx.sys.sched.set(EventKind::VramDma, now + at);
                ctx.reschedule();
                ctx.vdma_stall(u64::MAX);
                assert_eq!(ctx.clock(), now + 12);
                assert_eq!(ctx.sys.vdma.stall, 0);
                assert_eq!(ctx.sys.vdma.pending, kept, "trigger at +{at}");
            });
        }
    }

    /// The lost-trigger window after a block that a HALT's latched opcode
    /// waited for (`Refetch::Ran`) ends at that block's full stall, however
    /// long the flag then stays set: nothing clears it until the next
    /// trigger or HALT, which can be more than 2^31 clocks (8.5 minutes)
    /// away (review finding A1-04: a 32-bit end made such a trigger look
    /// as if it came before the end, and its block was dropped).
    #[test]
    fn a_stale_ran_flag_loses_no_trigger_however_old() {
        for (later, kept) in [
            (0, false),
            (1 << 31 | 64, true),
            (1 << 32, true),
            (3 << 31, true),
        ] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                ctx.write(0xFF26, 0x00); // APU off: no frame-sequencer events
                // As `vram_dma` leaves it after such a block (1 + 8
                // M-cycles) at this clock.
                ctx.sys.vdma.refetch = Refetch::Ran;
                ctx.sys.vdma.stall_end = ctx.clock() + 36;
                // `later` clocks pass (LCD off, nothing else scheduled).
                ctx.hot.deadline += later;
                ctx.write(0xFF40, 0x91);
                ctx.sys.vdma.hblank = true;
                let now = ctx.clock();
                ctx.sys.sched.set(EventKind::VramDma, now + 4);
                ctx.reschedule();
                ctx.idle();
                ctx.idle();
                assert_eq!(ctx.clock(), now + 8);
                assert_eq!(ctx.sys.vdma.pending, kept, "{later} clocks later");
                assert_eq!(ctx.sys.vdma.refetch, Refetch::None);
            });
        }
    }

    /// A register write in mode 3 that moves the line's mode-0 start (here
    /// SCX's fine scroll, which lengthens mode 3) moves the HBlank
    /// transfer's trigger with it: the trigger was scheduled from the
    /// line's timing as it was before the write, and nothing else syncs
    /// the PPU before it (found by `check-sync-invariance.sh` on
    /// `synth:worst-case`: the block ran up to 5 clocks before mode 0).
    #[test]
    fn a_mode_3_write_that_moves_mode_0_moves_the_hblank_trigger() {
        let mut moved = [0; 2];
        // Accesses from line 2's mode 2 to the end of its mode 3, SCX
        // going from 7 to 6 (in the discard: a shorter mode 3, or 7 dots
        // longer once the discard has counted past 6) or to 4.
        for (m, scx) in ((2 * 456 + 60) / 4..(2 * 456 + 260) / 4).flat_map(|m| [(m, 6), (m, 4)]) {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF43, 0x07);
                ctx.write(0xFF40, 0x91);
                idle_to(ctx, e, (2 * 456 + 20) / 4);
                ctx.write(0xFF55, 0x87);
                assert!(ctx.sys.vdma.hblank && !ctx.sys.vdma.pending);
                idle_to(ctx, e, m);
                // The line's timing as the write's own sync sees it.
                ctx.run_ppu_to_call(ctx.clock());
                let before = ctx.sys.ppu.mode0_start();
                ctx.write(0xFF43, scx);
                let start = ctx.sys.ppu.mode0_start().unwrap();
                let trigger = start + hdma::HBLANK_DELAY;
                if trigger <= ctx.clock() {
                    // Line 2 isn't latched yet, or its trigger has passed.
                    return;
                }
                let before = before.unwrap();
                moved[0] += u32::from(start < before);
                moved[1] += u32::from(start > before);
                assert_eq!(
                    ctx.sys.sched.deadline(EventKind::VramDma),
                    trigger,
                    "access at {m}, SCX {scx}"
                );
                // The block is requested in the M-cycle that reaches it.
                while !ctx.sys.vdma.pending {
                    ctx.idle();
                }
                assert!(
                    (trigger..trigger + 4).contains(&ctx.clock()),
                    "access at {m}, SCX {scx}"
                );
            });
        }
        assert!(moved[0] > 0 && moved[1] > 0, "mode 0 moved {moved:?}");
    }

    /// CGB B/C read STAT mode 0 at line 0 dot 3 in single speed, but an
    /// HBlank-HDMA start in that M-cycle must not see HBlank: no block
    /// starts now (review finding; SameBoy keeps mode 1 there).
    #[test]
    fn hblank_hdma_write_at_line_0_dot_3_does_not_start_a_block() {
        // Frame position = clock - E + 7; line 0 begins at 154 * 456.
        let m = (154 * 456 + 3 - 7) / 4;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            assert_eq!(ctx.read(0xFF41) & 3, 0, "STAT read keeps the quirk");
        });
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            ctx.write(0xFF55, 0x80);
            assert!(!ctx.sys.vdma.pending, "no immediate block");
            assert!(ctx.sys.vdma.hblank);
        });
    }
}

/// Helpers shared by the unit tests of the `bus` modules. Inline, as test
/// code that needs `std` must be (`tests/hard_rules.rs`).
#[cfg(test)]
mod test_util {
    extern crate std;
    use super::Ctx;
    use crate::cpu::Bus;
    use crate::ppu::Frame;
    use crate::{Config, Emulator, Events, cart_ram_size};
    use std::boxed::Box;
    use std::vec;

    /// An emulator with `rom` loaded and a RAM buffer of the header's size.
    pub(super) fn load(rom: &'static [u8]) -> &'static mut Emulator<'static> {
        let ram = vec![0u8; cart_ram_size(rom).unwrap()].leak();
        let emu = crate::tests::fresh();
        emu.load(rom, ram, Config::default()).unwrap();
        emu
    }

    pub(super) fn rom(len: usize, cart_type: u8, ram_code: u8, cgb_flag: u8) -> &'static mut [u8] {
        let rom = vec![0u8; len].leak();
        rom[0x143] = cgb_flag;
        rom[0x147] = cart_type;
        rom[0x149] = ram_code;
        rom
    }

    /// A CPU write through the bus (no M-cycle).
    pub(super) fn write(emu: &mut Emulator, addr: u16, value: u8) {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write_mapped(addr, value);
    }

    /// A CGB-mode emulator with the LCD off (no PPU deadline), OAM filled
    /// with 55, C100+i = C200+i = i ^ A5 and ROM 4000+i = i ^ 3C.
    pub(super) fn setup() -> &'static mut Emulator<'static> {
        setup_with(&[])
    }

    /// [`setup`] with `program` at 0100 (the entry point).
    pub(super) fn setup_with(program: &[u8]) -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x143] = 0x80;
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        for i in 0..0xA0 {
            rom[0x4000 + i] = i as u8 ^ 0x3C;
        }
        let emu = crate::tests::fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        emu.mem.oam.fill(0x55);
        for i in 0..0x200 {
            emu.mem.wram[0x100 + i] = i as u8 ^ 0xA5;
        }
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write_mapped(0xFF40, 0x11);
        drop(ctx);
        emu
    }

    /// Runs `f` with a run-call context on `emu`.
    pub(super) fn with_ctx(emu: &mut Emulator, f: impl FnOnce(&mut Ctx)) {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        f(&mut ctx);
    }

    /// Idles until the next access is at `t0 + n` M-cycles.
    pub(super) fn idle_to(ctx: &mut Ctx, t0: u64, n: u64) {
        while ctx.clock() < t0 + 4 * n {
            ctx.idle();
        }
        assert_eq!(ctx.clock(), t0 + 4 * n);
    }
}
