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
use crate::dma::{self, ReadClears};
use crate::hdma::{Refetch, VramDma};
use crate::joypad::{InputQueue, p1_interrupt, p1_lines};
use crate::memory::Memory;
use crate::ppu::{Frame, Ppu};
use crate::schedule::{EventKind, NEVER, Schedule};
use crate::serial::Serial;
use crate::speed::Speed;
use crate::timer::{INT_TIMER, Timer};
use crate::{Buttons, Emulator, Events, RunResult};

mod apu_io;
mod oam_dma;
mod ppu_io;
mod reset;
mod stop;
mod timer_io;
mod vram_dma;

use oam_dma::read_during_dma;

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
