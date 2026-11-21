//! Serial port, SB/SC (Pan Docs: Serial Data Transfer (Link Cable)).
//!
//! Only transfers on the internal clock happen; no link partner is
//! emulated, so the bits shifted in are 1s (SB reads FF after a transfer,
//! as with no cable). Each bit is shifted on a falling edge of a bit of the
//! timer's internal counter (8192 Hz: bit 8; CGB fast clock, SC bit 1,
//! 262144 Hz: bit 3), like the hardware, which derives the serial clock
//! from the same divider (SameBoy `GB_serial_master_edge`).

pub(crate) struct Serial {
    /// The most recent byte sent out (reported with `Events::SERIAL_TX`).
    pub(crate) last_byte: u8,
    pub(crate) sb: u8,
    pub(crate) sc: u8,
    /// The counter bit whose falling edge shifts one bit, or 0 when no
    /// transfer is running.
    pub(crate) clock_bit: u16,
    /// Bits shifted so far in the running transfer.
    bits: u8,
    /// The bits shifted out so far.
    out: u8,
}

/// The serial interrupt bit in IF.
pub(crate) const INT_SERIAL: u8 = 0x08;

impl Serial {
    pub(crate) const fn new() -> Self {
        Self {
            last_byte: 0,
            sb: 0,
            sc: 0,
            clock_bit: 0,
            bits: 0,
            out: 0,
        }
    }

    /// A write to SC. `cgb_mode` enables the fast clock bit.
    pub(crate) fn write_sc(&mut self, value: u8, cgb_mode: bool) {
        self.sc = value & if cgb_mode { 0x83 } else { 0x81 };
        // A transfer runs while bit 7 is set; only the internal clock
        // (bit 0) drives it here.
        self.clock_bit = if value & 0x81 == 0x81 {
            if self.sc & 0x02 != 0 { 1 << 3 } else { 1 << 8 }
        } else {
            0
        };
        self.bits = 0;
    }

    pub(crate) fn read_sc(&self, cgb_mode: bool) -> u8 {
        self.sc | if cgb_mode { 0x7C } else { 0x7E }
    }

    /// Called when the timer counter bits in `fell` went from 1 to 0 and a
    /// transfer is running. Returns true when a byte completes: SC bit 7 is
    /// cleared, the caller requests the serial interrupt, and `last_byte`
    /// holds the byte sent.
    #[inline]
    pub(crate) fn clock(&mut self, fell: u16) -> bool {
        if fell & self.clock_bit == 0 {
            return false;
        }
        self.out = (self.out << 1) | (self.sb >> 7);
        self.sb = (self.sb << 1) | 1;
        self.bits += 1;
        if self.bits < 8 {
            return false;
        }
        self.last_byte = self.out;
        self.sc &= 0x7F;
        self.clock_bit = 0;
        self.bits = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_clock_transfer_sends_the_byte() {
        let mut s = Serial::new();
        s.sb = b'P';
        s.write_sc(0x81, false);
        for i in 0..8 {
            assert_eq!(s.clock(1 << 8), i == 7);
        }
        assert_eq!(s.last_byte, b'P');
        assert_eq!(s.sb, 0xFF);
        assert_eq!(s.read_sc(false), 0x7F);
        // Other counter bits don't clock it; nothing runs afterwards.
        assert!(!s.clock(0xFFFF));
    }

    #[test]
    fn external_clock_never_completes() {
        let mut s = Serial::new();
        s.write_sc(0x80, false);
        for _ in 0..100 {
            assert!(!s.clock(0xFFFF));
        }
        assert_eq!(s.read_sc(false), 0xFE);
    }
}
