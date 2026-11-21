//! DIV/TIMA/TMA/TAC (Pan Docs: Timer and Divider Registers; "Timer
//! obscure behaviour").
//!
//! DIV is the top byte of a 16-bit counter that counts CPU clocks (4 per
//! M-cycle, also in double speed, where the M-cycle is shorter). TIMA
//! increments on a falling edge of one counter bit, selected by TAC and
//! ANDed with the TAC enable bit. The serial port's bit clock is another
//! bit of the same counter, so [`Timer::advance`] reports every bit that
//! fell.
//!
//! The bus runs one M-cycle as the CPU's access (a register read or write
//! here), then [`Timer::advance_reload`] (if [`Timer::reload_pending`]),
//! [`Timer::advance`], then [`Timer::edges`] for the bits that fell
//! ([`Timer::tick`] is that sequence). The pieces are separate so the
//! bus's per-M-cycle path touches only state it keeps in its run context
//! (ADR 0008, amendments).
//!
//! This is the eager model: one call per M-cycle. It is also the
//! reference the event-scheduled timer is checked against (ADR 0011,
//! "Verification"), so it can be driven standalone: per M-cycle, at most
//! one [`Timer::read`] or [`Timer::write`] (the access), then
//! [`Timer::tick`].
//!
//! # TIMA overflow (Pan Docs: "Timer overflow behavior"; ADR 0011)
//! TIMA overflows in the tick that ends some M-cycle. The next M-cycle is
//! **cycle A** ([`Reload::A`]): TIMA reads 00, the reload from TMA and the
//! interrupt have not happened yet, and they happen in the tick that ends
//! cycle A. The M-cycle after that is **cycle B** ([`Reload::B`]): TIMA
//! already holds TMA and IF is set. SameBoy models the same window as
//! `tima_reload_state` RELOADING/RELOADED (`advance_tima_state_machine`
//! in Core/timing.c; `GB_IO_TIMA`/`GB_IO_TMA` in `write_high_memory`,
//! Core/memory.c); Mooneye `tima_reload`, `tima_write_reloading` and
//! `tma_write_reloading` test it.
//! - A TIMA write in cycle A cancels both the reload and the interrupt.
//!   DIV and TAC writes do not.
//! - A TMA write in cycle A is what the reload loads.
//! - A TIMA write in cycle B is ignored; a TMA write in cycle B is also
//!   stored into TIMA.
//! - A DIV/TAC-write glitch increment that overflows happens during the
//!   access, so the tick that ends that same M-cycle reloads, and the next
//!   M-cycle is cycle B: TIMA = 00 is never observable.
//! - A glitch increment *during* cycle A (ADR 0011's uncovered corner)
//!   can't happen: the selected counter bit just fell, so it and every
//!   bit below it are 0 at cycle A's access, and neither a DIV nor a TAC
//!   write sees a 1 -> 0 edge on it. Increments that don't overflow leave
//!   the window alone anyway.

/// Where TIMA is in the overflow window (module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reload {
    /// No overflow in the last two M-cycles.
    Idle,
    /// Cycle A: TIMA overflowed in the previous tick and reads 00; the
    /// tick that ends this M-cycle loads TMA and requests the interrupt.
    A,
    /// Cycle B: the reload happened in the previous tick.
    B,
}

#[derive(Clone, Copy)]
pub(crate) struct Timer {
    /// The internal 16-bit counter; DIV is its high byte.
    pub(crate) counter: u16,
    pub(crate) tima: u8,
    pub(crate) tma: u8,
    /// TAC, low 3 bits.
    pub(crate) tac: u8,
    /// The counter bit TIMA counts falling edges of, or 0 if TAC disables
    /// the timer.
    tima_bit: u16,
    /// The overflow window. Not [`Reload::Idle`] for at most two M-cycles
    /// after an overflow; the bus's tick tests it on its slow path.
    reload: Reload,
}

/// Counter bit per TAC clock select: 4096, 262144, 65536, 16384 Hz.
const TAC_BITS: [u16; 4] = [1 << 9, 1 << 3, 1 << 5, 1 << 7];

/// The timer interrupt bit in IF.
pub(crate) const INT_TIMER: u8 = 0x04;

impl Timer {
    pub(crate) const fn new() -> Self {
        Self {
            counter: 0,
            tima: 0,
            tma: 0,
            tac: 0,
            tima_bit: 0,
            reload: Reload::Idle,
        }
    }

    /// The tick that ends one M-cycle, after its access. Returns the
    /// counter bits that fell (1 -> 0), for the serial clock, and ORs the
    /// timer interrupt into `iflags`. The bus inlines the same sequence.
    #[cfg(test)]
    pub(crate) fn tick(&mut self, iflags: &mut u8) -> u16 {
        if self.reload_pending() {
            *iflags |= self.advance_reload();
        }
        let fell = self.advance();
        self.edges(fell);
        fell
    }

    /// The overflow window is open: [`Timer::advance_reload`] must run at
    /// the start of this M-cycle's tick.
    #[inline(always)]
    pub(crate) fn reload_pending(&self) -> bool {
        self.reload != Reload::Idle
    }

    /// Moves the overflow window on at the end of cycle A or B. The end of
    /// cycle A loads TMA into TIMA; returns the IF bit to request then (0
    /// otherwise).
    #[inline]
    pub(crate) fn advance_reload(&mut self) -> u8 {
        match self.reload {
            Reload::A => {
                self.reload = Reload::B;
                self.tima = self.tma;
                INT_TIMER
            }
            Reload::B | Reload::Idle => {
                self.reload = Reload::Idle;
                0
            }
        }
    }

    /// Advances the counter by one M-cycle (4 clocks) and returns the bits
    /// that fell.
    #[inline(always)]
    pub(crate) fn advance(&mut self) -> u16 {
        let old = self.counter;
        let new = old.wrapping_add(4);
        self.counter = new;
        old & !new
    }

    /// Increments TIMA if its selected bit is among the bits that `fell`.
    #[inline]
    pub(crate) fn edges(&mut self, fell: u16) {
        if fell & self.tima_bit != 0 {
            self.increment();
        }
    }

    /// The counter bit TIMA counts falling edges of (0 when disabled).
    #[inline]
    pub(crate) fn tima_bit(&self) -> u16 {
        self.tima_bit
    }

    /// One TIMA increment. An overflow leaves TIMA at 00 and opens the
    /// window; an increment that doesn't overflow leaves the window as it
    /// is (DIV/TAC glitches in cycle A don't cancel the reload).
    #[inline]
    fn increment(&mut self) {
        let (v, overflow) = self.tima.overflowing_add(1);
        self.tima = v;
        if overflow {
            self.reload = Reload::A;
        }
    }

    #[inline]
    pub(crate) fn div(&self) -> u8 {
        (self.counter >> 8) as u8
    }

    /// Writing DIV clears the whole counter. If the selected bit was 1,
    /// that is a falling edge and TIMA increments. Returns the bits that
    /// fell (they clock the serial port too).
    pub(crate) fn write_div(&mut self) -> u16 {
        let fell = self.counter;
        self.counter = 0;
        if fell & self.tima_bit != 0 {
            self.increment();
        }
        fell
    }

    /// Pan Docs "Timer overflow behavior": a TIMA write in cycle A cancels
    /// the reload and the interrupt; in cycle B it is ignored (TMA wins).
    #[inline]
    pub(crate) fn write_tima(&mut self, value: u8) {
        match self.reload {
            Reload::B => {}
            Reload::A => {
                self.tima = value;
                self.reload = Reload::Idle;
            }
            Reload::Idle => self.tima = value,
        }
    }

    /// The reload at the end of cycle A reads `tma` then, so a write in
    /// cycle A is used; in cycle B the write also reaches TIMA (Pan Docs
    /// "Timer overflow behavior"; SameBoy stores TMA writes into TIMA
    /// whenever `tima_reload_state` isn't RUNNING).
    #[inline]
    pub(crate) fn write_tma(&mut self, value: u8) {
        self.tma = value;
        if self.reload == Reload::B {
            self.tima = value;
        }
    }

    /// Changing TAC can turn the timer's input from 1 to 0, which counts as
    /// a falling edge (Pan Docs: "Timer obscure behaviour"). That includes
    /// disabling the timer: SameBoy ticks there; Pan Docs says CGBs don't
    /// (ADR 0011 open question, kept until a test decides).
    pub(crate) fn write_tac(&mut self, value: u8) {
        let was_high = self.counter & self.tima_bit != 0;
        self.tac = value & 7;
        self.tima_bit = if value & 4 != 0 {
            TAC_BITS[usize::from(value & 3)]
        } else {
            0
        };
        if was_high && self.counter & self.tima_bit == 0 {
            self.increment();
        }
    }

    /// Reads FF04-FF07 by the low address bits (standalone driving; the
    /// bus reads the fields).
    #[cfg(test)]
    pub(crate) fn read(&self, reg: u8) -> u8 {
        match reg & 3 {
            0 => self.div(),
            1 => self.tima,
            2 => self.tma,
            _ => 0xF8 | self.tac,
        }
    }

    /// Writes FF04-FF07 by the low address bits, as the bus does. Returns
    /// the counter bits that fell (a DIV write), for the serial clock.
    #[cfg(test)]
    pub(crate) fn write(&mut self, reg: u8, value: u8) -> u16 {
        match reg & 3 {
            0 => return self.write_div(),
            1 => self.write_tima(value),
            2 => self.write_tma(value),
            _ => self.write_tac(value),
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMA: u8 = 1;
    const TMA: u8 = 2;

    #[test]
    fn tima_counts_at_the_selected_rate() {
        let mut t = Timer::new();
        let mut iflags = 0;
        t.write_tac(0x05); // 262144 Hz: every 16 clocks = 4 M-cycles
        for _ in 0..40 {
            t.tick(&mut iflags);
        }
        assert_eq!(t.tima, 10);
        t.write_tac(0x01); // disabled
        for _ in 0..40 {
            t.tick(&mut iflags);
        }
        assert_eq!(t.tima, 10);
    }

    /// TIMA = FF, TMA = AB, 262144 Hz, ticked to cycle A: the 4th tick
    /// makes bit 3 fall and overflows TIMA.
    fn in_cycle_a(iflags: &mut u8) -> Timer {
        let mut t = Timer::new();
        t.write_tac(0x05);
        t.write(TMA, 0xAB);
        t.write(TIMA, 0xFF);
        for _ in 0..4 {
            t.tick(iflags);
        }
        t
    }

    #[test]
    fn cycle_a_reads_zero_then_cycle_b_reads_tma_with_if_set() {
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        assert_eq!((t.read(TIMA), iflags), (0x00, 0));
        t.tick(&mut iflags);
        assert_eq!((t.read(TIMA), iflags), (0xAB, INT_TIMER));
    }

    #[test]
    fn tima_write_in_cycle_a_cancels_reload_and_interrupt() {
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        t.write(TIMA, 0x42);
        for _ in 0..3 {
            t.tick(&mut iflags);
        }
        assert_eq!((t.read(TIMA), iflags), (0x42, 0));
    }

    #[test]
    fn tima_write_in_cycle_b_is_ignored() {
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        t.tick(&mut iflags);
        t.write(TIMA, 0x42);
        assert_eq!(t.read(TIMA), 0xAB);
        t.tick(&mut iflags);
        // The window is closed again: writes land.
        t.write(TIMA, 0x42);
        assert_eq!((t.read(TIMA), iflags), (0x42, INT_TIMER));
    }

    #[test]
    fn tma_write_in_cycle_b_also_reaches_tima() {
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        t.tick(&mut iflags);
        t.write(TMA, 0x77);
        assert_eq!((t.read(TIMA), t.read(TMA)), (0x77, 0x77));
        t.tick(&mut iflags);
        // Outside the window a TMA write just stores.
        t.write(TMA, 0x11);
        assert_eq!((t.read(TIMA), t.read(TMA)), (0x77, 0x11));
    }

    #[test]
    fn tma_write_in_cycle_a_is_what_the_reload_loads() {
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        t.write(TMA, 0x77);
        assert_eq!(t.read(TIMA), 0x00);
        t.tick(&mut iflags);
        assert_eq!((t.read(TIMA), iflags), (0x77, INT_TIMER));
    }

    #[test]
    fn div_and_tac_writes_in_cycle_a_keep_the_reload() {
        for (reg, value) in [(0, 0), (3, 0x06), (3, 0x01)] {
            let mut iflags = 0;
            let mut t = in_cycle_a(&mut iflags);
            t.write(reg, value);
            t.tick(&mut iflags);
            assert_eq!(
                (t.read(TIMA), iflags),
                (0xAB, INT_TIMER),
                "{reg} {value:02x}"
            );
        }
    }

    #[test]
    fn glitch_overflow_reloads_at_the_end_of_its_own_cycle() {
        for (reg, value) in [(0, 0), (3, 0x04)] {
            let mut iflags = 0;
            let mut t = Timer::new();
            t.write_tac(0x05);
            t.write(TMA, 0xAB);
            t.write(TIMA, 0xFF);
            t.tick(&mut iflags);
            t.tick(&mut iflags); // counter = 8: bit 3 set
            // DIV reset / switch to bit 9: bit 3's 1 -> 0 edge overflows.
            t.write(reg, value);
            assert_eq!((t.read(TIMA), iflags), (0x00, 0), "{reg}");
            t.tick(&mut iflags);
            assert_eq!((t.read(TIMA), iflags), (0xAB, INT_TIMER), "{reg}");
            // The next M-cycle is cycle B.
            t.write(TIMA, 0x42);
            assert_eq!(t.read(TIMA), 0xAB, "{reg}");
        }
    }

    #[test]
    fn tma_ff_at_the_fast_rate_overflows_again() {
        // TMA = FF: every 4th M-cycle overflows, each with its own window.
        let mut iflags = 0;
        let mut t = in_cycle_a(&mut iflags);
        t.write(TMA, 0xFF);
        let mut irqs = 0;
        for _ in 0..16 {
            iflags = 0;
            t.tick(&mut iflags);
            irqs += u32::from(iflags != 0);
        }
        assert_eq!(irqs, 4);
    }

    #[test]
    fn div_write_glitch_increments_tima() {
        let mut t = Timer::new();
        let mut iflags = 0;
        t.write_tac(0x05);
        t.tick(&mut iflags);
        t.tick(&mut iflags); // counter = 8: bit 3 set
        assert_eq!(t.tima, 0);
        t.write_div();
        assert_eq!((t.tima, t.div()), (1, 0));
    }
}
