//! The CPU speed: normal, or CGB double speed (KEY1, FF4D; Pan Docs "CGB
//! Registers", "FF4D - KEY1").
//!
//! Every conversion from the speed to a number lives here: the M-cycle
//! length in base clocks, its log2, the system counter's rate, KEY1's
//! bit 7. The one place that keeps a number instead is `bus::Hot`, whose
//! `mcycle` word the tick reads on every instruction; it converts to and
//! from a `Speed` at its own edge (`Hot::speed`, `Hot::set_speed`).

/// The CPU speed. The discriminant is log2 of the speed-up, which is
/// what every conversion below is computed from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Speed {
    /// 4 base clocks per M-cycle (1,048,576 M-cycles per second).
    Normal = 0,
    /// CGB double speed: 2 base clocks per M-cycle.
    Double = 1,
}

impl Speed {
    #[inline]
    pub(crate) const fn is_double(self) -> bool {
        matches!(self, Self::Double)
    }

    /// The other speed (a KEY1 speed switch).
    #[inline]
    pub(crate) const fn toggled(self) -> Self {
        match self {
            Self::Normal => Self::Double,
            Self::Double => Self::Normal,
        }
    }

    /// Base clocks per M-cycle: 4, or 2 in double speed.
    #[inline]
    pub(crate) const fn mcycle(self) -> u64 {
        4 >> self as u32
    }

    /// log2 of [`Self::mcycle`]: clocks to M-cycles is a shift by this,
    /// not a u64 division (a libcall on thumbv7em).
    #[inline]
    pub(crate) const fn mcycle_shift(self) -> u32 {
        2 - self as u32
    }

    /// log2 of the system counter's ticks per base clock: the counter
    /// (DIV's 16 bits) counts T-cycles, 1 per base clock, or 2 in double
    /// speed.
    #[inline]
    pub(crate) const fn counter_shift(self) -> u32 {
        self as u32
    }

    /// KEY1 bit 7, the current-speed flag.
    #[inline]
    pub(crate) const fn key1(self) -> u8 {
        (self as u8) << 7
    }

    /// 1 in double speed, else 0: for the edges that fall one unit
    /// later (or earlier) in double speed.
    #[inline]
    pub(crate) const fn extra(self) -> u32 {
        self as u32
    }

    /// The entry of a `[normal, double]` pair for this speed.
    #[inline]
    pub(crate) const fn pick<T: Copy>(self, pair: [T; 2]) -> T {
        pair[self as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::Speed::{Double, Normal};

    #[test]
    fn conversions_agree_for_both_speeds() {
        for (s, mcycle, key1) in [(Normal, 4, 0x00), (Double, 2, 0x80)] {
            assert_eq!(s.mcycle(), mcycle);
            assert_eq!(1 << s.mcycle_shift(), mcycle);
            assert_eq!(s.mcycle() << s.counter_shift(), 4, "4 T-cycles per M-cycle");
            assert_eq!(s.key1(), key1);
            assert_eq!(s.toggled().toggled(), s);
            assert_ne!(s.toggled(), s);
            assert_eq!(s.is_double(), s == Double);
            assert_eq!(s.extra(), u32::from(s.is_double()));
            assert_eq!(s.pick(["n", "d"]), if s.is_double() { "d" } else { "n" });
        }
    }
}
