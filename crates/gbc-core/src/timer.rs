//! DIV/TIMA/TMA/TAC (Pan Docs: Timer and Divider Registers; "Timer
//! obscure behaviour").
//!
//! DIV is the top byte of a 16-bit counter that counts CPU clocks (4 per
//! M-cycle, also in double speed, where the M-cycle is shorter). TIMA
//! increments on a falling edge of one counter bit, selected by TAC and
//! ANDed with the TAC enable bit. The serial port's bit clock is another
//! bit of the same counter, so [`Timer::tick`] reports every bit that fell.
//!
//! M1 has the falling-edge model, including the DIV-write and TAC-write
//! glitches and the one-M-cycle delay before the TMA reload and interrupt
//! after an overflow. TODO(M2): writes to TIMA/TMA during the overflow and
//! reload cycles (Pan Docs: "Timer overflow behavior").

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
    /// TIMA overflowed in the last M-cycle: TMA is loaded and the interrupt
    /// requested in this one.
    reload: bool,
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
            reload: false,
        }
    }

    /// One M-cycle. Returns the counter bits that fell (1 -> 0), for the
    /// serial clock, and ORs the timer interrupt into `iflags`.
    #[inline(always)]
    pub(crate) fn tick(&mut self, iflags: &mut u8) -> u16 {
        if self.reload {
            self.reload = false;
            self.tima = self.tma;
            *iflags |= INT_TIMER;
        }
        let old = self.counter;
        let new = old.wrapping_add(4);
        self.counter = new;
        let fell = old & !new;
        if fell & self.tima_bit != 0 {
            self.increment();
        }
        fell
    }

    #[inline]
    fn increment(&mut self) {
        let (v, overflow) = self.tima.overflowing_add(1);
        self.tima = v;
        self.reload = overflow;
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

    /// Changing TAC can turn the timer's input from 1 to 0, which counts as
    /// a falling edge (Pan Docs: "Timer obscure behaviour").
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
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn overflow_reloads_one_cycle_later() {
        let mut t = Timer::new();
        let mut iflags = 0;
        t.tma = 0xAB;
        t.tima = 0xFF;
        t.write_tac(0x05);
        // The 4th M-cycle makes bit 3 fall.
        for _ in 0..4 {
            t.tick(&mut iflags);
        }
        assert_eq!((t.tima, iflags), (0x00, 0));
        t.tick(&mut iflags);
        assert_eq!((t.tima, iflags), (0xAB, INT_TIMER));
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
