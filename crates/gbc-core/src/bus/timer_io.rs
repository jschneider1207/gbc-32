//! The timer's and the serial port's side of the bus: their register
//! writes, DIV resets and deadlines in the event table (`timer` and
//! `serial` modules; ADR 0011).

use super::Ctx;
use crate::Events;
use crate::schedule::{EventKind, NEVER};
use crate::serial::INT_SERIAL;
use crate::timer::INT_TIMER;

// Names only the doc comments use.
#[cfg(doc)]
use crate::{serial::Serial, timer::Timer};

impl Ctx<'_, '_> {
    /// A serial byte completed (`done`): the serial interrupt.
    fn serial_done(&mut self, done: bool) {
        if done {
            self.sys.iflags |= INT_SERIAL;
            self.irq_attention();
            self.raise(Events::SERIAL_TX);
        }
    }

    /// Brings the serial port's shift register up to the clock `now`
    /// ([`Serial::catch_up`]): the shifts of a running transfer are no
    /// events, only its completion is (`serial` module docs, "Lazy
    /// shifts"). The completion's event runs before any access on its
    /// clock, so no catch-up but the event's completes a byte.
    pub(super) fn serial_catch_up(&mut self, now: u64) {
        let c = self.sys.timer.sc(now);
        let done = self.sys.serial.catch_up(c);
        self.serial_done(done);
    }

    /// The Serial deadline: the clock at which the running transfer
    /// completes (its eighth shift, the bit clock's 1 -> 0 edge that many
    /// periods on), else [`NEVER`]. It needs no catch-up
    /// ([`Serial::completion`] is the same before and after one), but a
    /// counter that runs (not in STOP mode).
    #[inline(always)]
    fn serial_deadline(&self) -> u64 {
        let s = &*self.sys;
        match s.serial.completion() {
            None => NEVER,
            Some(v) => s.timer.clock_at(v),
        }
    }

    /// Sets the Serial deadline ([`Self::serial_deadline`]) and
    /// reschedules: after a change to the transfer, the counter or its
    /// rate.
    // Out of line, measured (docs/perf.md, "Scheduler and attribute
    // pass"): inlined into the register-write paths it is +1,450 B and
    // slower. `Schedule::set` is inlined here, also measured.
    #[inline(never)]
    pub(super) fn schedule_serial(&mut self) {
        let at = self.serial_deadline();
        self.sys.sched.set_inline(EventKind::Serial, at);
        self.reschedule();
    }

    /// Sets the Timer deadline and reschedules: after a change to the
    /// timer, or to bit 2 of IF or IE.
    ///
    /// The Timer event's work for the rest of the machine is to set IF
    /// bit 2 at a reload. While the bit is set and IE bit 2 is clear, a
    /// reload has nothing to request and the dispatch can't clear the
    /// bit, so there is no deadline: the timer is caught up
    /// ([`Timer::catch_up`]) at its next access, and here, when one of
    /// the two bits changes (the reloads up to the clock requested the
    /// interrupt that was pending). With IE bit 2 set the deadline stays
    /// on, so the dispatch's acknowledge has nothing to do (ADR 0011,
    /// 2026-11-24 amendment).
    #[inline(never)] // See `schedule_serial`.
    pub(super) fn schedule_timer(&mut self) {
        let now = self.clock();
        let s = &mut *self.sys;
        s.timer.catch_up(now);
        let at = if s.iflags & !s.ie & INT_TIMER != 0 {
            NEVER
        } else {
            s.timer.deadline()
        };
        s.sched.set_inline(EventKind::Timer, at);
        self.reschedule();
    }

    /// Writes to SB, SC, DIV, TIMA, TMA and TAC (by the low 7 address
    /// bits): they need the system counter, and all but SB move the Timer
    /// or the Serial deadline. Out of line so the register logic stays
    /// off `write_mapped` (ADR 0011).
    #[inline(never)]
    pub(super) fn write_timer_io(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        let s = &mut *self.sys;
        match reg {
            // The shifts before the write took the old SB's bits. The
            // edges don't move: the deadline stays.
            0x01 => {
                let done = s.serial.write_sb(value, s.timer.sc(now));
                return self.serial_done(done);
            }
            0x02 => {
                let c = s.timer.sc(now);
                let done = s.serial.write_sc(value, s.cgb_mode, c);
                self.serial_done(done);
                self.schedule_serial();
                return;
            }
            0x04 => return self.div_reset(),
            0x05 => s.timer.write_tima(now, value),
            0x06 => s.timer.write_tma(now, value),
            _ => s.timer.write_tac(now, value),
        }
        self.schedule_timer();
    }

    /// A DIV write or STOP: every set counter bit falls (the TIMA glitch
    /// in [`Timer::write_div`], a serial flip-flop toggle in
    /// [`Serial::div_reset`], after the shifts up to the old counter
    /// value; Pan Docs FF04), then both deadlines move with the rebased
    /// counter.
    pub(super) fn div_reset(&mut self) {
        self.div_reset_as(false);
    }

    /// `stop`: the reset of a two-byte STOP ([`Timer::stop_div`]), after
    /// its second byte (AGE `gb_bus::execute_stop`; ADR 0020).
    ///
    /// UNPINNED: spsw-reset-edges (a): the serial clock's edge at STOP's
    /// reset is the plain DIV-reset rule (as in AGE), as are the 4 kHz
    /// TIMA and frame-sequencer edges of a one-byte STOP's reset; no ROM
    /// covers them.
    pub(super) fn div_reset_as(&mut self, stop: bool) {
        let now = self.clock();
        let old = if stop {
            self.sys.timer.stop_div(now)
        } else {
            self.sys.timer.write_div(now)
        };
        let new = self.sys.timer.sc(now);
        let done = self.sys.serial.div_reset(old, new);
        self.serial_done(done);
        self.apu_div_reset(old, stop);
        self.schedule_timer();
        self.schedule_serial();
    }

    /// A serial transfer completes at `now` (`EventKind::Serial`): the
    /// catch-up applies the shifts since the last access, the eighth
    /// among them. Once a transfer; `boundary` reschedules after the
    /// event loop.
    // Out of line, measured (docs/perf.md, "Scheduler and attribute
    // pass"): inlined into `boundary` it slows the PPU event's path.
    // Not `#[cold]`: a running transfer has one every eight bits.
    #[inline(never)]
    pub(super) fn serial_event(&mut self, now: u64) {
        self.serial_catch_up(now);
        debug_assert!(!self.sys.serial.running());
        // `NEVER`, the transfer being over.
        let at = self.serial_deadline();
        self.sys.sched.rearm(EventKind::Serial, at);
    }
}
