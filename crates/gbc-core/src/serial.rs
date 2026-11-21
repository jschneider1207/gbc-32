//! Serial port, SB/SC (Pan Docs: Serial Data Transfer (Link Cable)).
//!
//! Only transfers on the internal clock happen; no link partner is
//! emulated, so the bits shifted in are 1s (SB reads FF after a transfer,
//! as with no cable).
//!
//! # The bit clock (SameBoy `GB_serial_master_edge`, `GB_IO_SC` write)
//! The serial clock derives from the timer's system counter: a master
//! flip-flop toggles on every falling edge of counter bit 7 (bit 2 with
//! the CGB fast clock, SC bit 1), whether or not a transfer runs, and a
//! running internal-clock transfer shifts one bit each time the flip-flop
//! goes from 1 to 0 (8192 Hz; 262144 Hz fast). An SC write forces the
//! flip-flop to 0 (if it was 1, that is a 1 -> 0 edge, which shifts one
//! bit of the transfer running *before* the write), after clearing the bit
//! count. A DIV reset makes every set counter bit fall, so it toggles the
//! flip-flop if bit 7 (bit 2) was set.
//!
//! The counter is monotonic between DIV writes and rebases to a multiple
//! of 2^16 on one (`timer` module docs), so between those events the
//! flip-flop is a function of the counter: the number of bit-k falls
//! up to counter value `c` is `c >> (k+1)`, so the flip-flop is bit
//! `k+1` of the counter XOR a stored `phase`, which the SC write and the
//! DIV reset set. The next shift is the next counter value at which that
//! bit makes the flip-flop go 1 -> 0, the `EventKind::Serial` deadline
//! (ADR 0011). The eager reference for the differential test keeps the
//! flip-flop as a plain bool instead (`clock`, `write_sc_eager`).

#[cfg_attr(test, derive(Clone))]
pub(crate) struct Serial {
    /// The most recent byte sent out (reported with `Events::SERIAL_TX`).
    pub(crate) last_byte: u8,
    pub(crate) sb: u8,
    pub(crate) sc: u8,
    /// The counter bit whose falling edges toggle the flip-flop: bit 7, or
    /// bit 2 with the fast clock.
    clock_bit: u64,
    /// The flip-flop is counter bit `k+1` XOR this (0 or 1); see the
    /// module docs.
    phase: u64,
    /// Bits shifted so far in the running transfer.
    pub(crate) bits: u8,
    /// The bits shifted out so far.
    out: u8,
    /// The eager reference's flip-flop (differential test only).
    #[cfg(test)]
    ff: bool,
}

/// The serial interrupt bit in IF.
pub(crate) const INT_SERIAL: u8 = 0x08;

impl Serial {
    pub(crate) const fn new() -> Self {
        Self {
            last_byte: 0,
            sb: 0,
            sc: 0,
            clock_bit: 1 << 7,
            phase: 0,
            bits: 0,
            out: 0,
            #[cfg(test)]
            ff: false,
        }
    }

    /// A transfer on the internal clock is running (SC bits 7 and 0).
    #[inline]
    fn running(&self) -> bool {
        self.sc & 0x81 == 0x81
    }

    /// The counter bit that the flip-flop follows: one above `clock_bit`.
    #[inline]
    fn ff_bit(&self) -> u32 {
        self.clock_bit.trailing_zeros() + 1
    }

    /// The flip-flop at counter value `c`.
    #[inline]
    fn flip_flop(&self, c: u64) -> bool {
        (c >> self.ff_bit()) & 1 != self.phase
    }

    /// Stores SC and selects the bit clock. `cgb_mode` enables the fast
    /// clock bit (SameBoy: `serial_mask = cgb_mode && (value & 2) ? 4 : 0x80`).
    fn set_sc(&mut self, value: u8, cgb_mode: bool) {
        self.sc = value & if cgb_mode { 0x83 } else { 0x81 };
        self.clock_bit = if self.sc & 0x02 != 0 { 1 << 2 } else { 1 << 7 };
    }

    /// A write to SC at counter value `c`: the bit count clears, the
    /// flip-flop is forced to 0 (shifting one bit of a transfer that was
    /// running if it was 1; that bit counts towards the new transfer, as
    /// in SameBoy, so it can't complete a byte), then SC takes `value`.
    pub(crate) fn write_sc(&mut self, value: u8, cgb_mode: bool, c: u64) {
        self.bits = 0;
        if self.flip_flop(c) && self.running() {
            self.shift();
        }
        self.set_sc(value, cgb_mode);
        self.phase = (c >> self.ff_bit()) & 1;
    }

    /// A DIV reset (DIV write or STOP), the counter going from `old` to
    /// `new`: if the clock bit was set it falls, toggling the flip-flop,
    /// which shifts on 1 -> 0. Returns true when that completes a byte
    /// (see [`Serial::shift`]).
    pub(crate) fn div_reset(&mut self, old: u64, new: u64) -> bool {
        let mut ff = self.flip_flop(old);
        let mut done = false;
        if old & self.clock_bit != 0 {
            ff = !ff;
            done = !ff && self.running() && self.shift();
        }
        self.phase = ((new >> self.ff_bit()) & 1) ^ u64::from(ff);
        done
    }

    /// The counter value of the next shift after counter value `c`, or
    /// `None` when no transfer runs: the next `v > c` at which counter bit
    /// `k+1` becomes `phase` (the flip-flop goes 1 -> 0), i.e. the next
    /// `v ≡ phase << (k+1)` modulo `2^(k+2)`.
    pub(crate) fn next_shift(&self, c: u64) -> Option<u64> {
        if !self.running() {
            return None;
        }
        let r = self.phase << self.ff_bit();
        let m = self.ff_bit() + 1;
        Some((((c + (1 << m) - r) >> m) << m) + r)
    }

    pub(crate) fn read_sc(&self, cgb_mode: bool) -> u8 {
        self.sc | if cgb_mode { 0x7C } else { 0x7E }
    }

    /// The eager model's per-M-cycle hook, for the differential test's
    /// reference (`timer::reference`): `fell` holds the counter bits that
    /// went from 1 to 0 (an M-cycle's tick, or a DIV reset); a fall of the
    /// clock bit toggles the flip-flop, and 1 -> 0 shifts one bit
    /// (SameBoy `GB_serial_master_edge`).
    #[cfg(test)]
    pub(crate) fn clock(&mut self, fell: u64) -> bool {
        if fell & self.clock_bit == 0 {
            return false;
        }
        self.ff = !self.ff;
        !self.ff && self.running() && self.shift()
    }

    /// The eager model's SC write (SameBoy `GB_IO_SC`), with the
    /// flip-flop kept as a bool.
    #[cfg(test)]
    pub(crate) fn write_sc_eager(&mut self, value: u8, cgb_mode: bool) {
        self.bits = 0;
        if self.ff {
            self.ff = false;
            if self.running() {
                self.shift();
            }
        }
        self.set_sc(value, cgb_mode);
    }

    /// One bit clock edge of a running transfer. Returns true when a byte
    /// completes: SC bit 7 is cleared, the caller requests the serial
    /// interrupt, and `last_byte` holds the byte sent.
    pub(crate) fn shift(&mut self) -> bool {
        self.out = (self.out << 1) | (self.sb >> 7);
        self.sb = (self.sb << 1) | 1;
        self.bits += 1;
        if self.bits < 8 {
            return false;
        }
        self.last_byte = self.out;
        self.sc &= 0x7F;
        self.bits = 0;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_clock_transfer_sends_the_byte() {
        // Eager model: 16 falls of bit 7 (two per flip-flop cycle) send 8 bits.
        let mut s = Serial::new();
        s.sb = b'P';
        s.write_sc_eager(0x81, false);
        for i in 0..16 {
            assert_eq!(s.clock(1 << 7), i == 15);
        }
        assert_eq!(s.last_byte, b'P');
        assert_eq!(s.sb, 0xFF);
        assert_eq!(s.read_sc(false), 0x7F);
        // Other counter bits don't clock it; nothing runs afterwards.
        assert!(!s.clock(0xFF7F));
        assert!(!s.clock(0xFFFF) && !s.clock(0xFFFF));
    }

    #[test]
    fn external_clock_never_completes() {
        let mut s = Serial::new();
        s.write_sc_eager(0x80, false);
        for _ in 0..100 {
            assert!(!s.clock(0xFFFF));
        }
        assert_eq!(s.read_sc(false), 0xFE);
        s.write_sc(0x80, false, 0x1234);
        assert_eq!(s.next_shift(0x1234), None);
    }

    /// The first shift after an SC write is on the second fall of the
    /// clock bit after it (the write forces the flip-flop to 0), whatever
    /// the counter's phase; in both clock speeds.
    #[test]
    fn sc_write_resets_the_flip_flop() {
        for (sc, k, cgb) in [(0x81, 7, false), (0x81, 7, true), (0x83, 2, true)] {
            let p = 1u64 << k;
            for c in [0x1000, 0x1000 + p, 0x1000 + 2 * p + 1, 0x1000 + 3 * p + 1] {
                let mut s = Serial::new();
                s.write_sc(sc, cgb, c);
                // Bit k falls at multiples of 2p.
                let at = ((c >> (k + 1)) + 2) << (k + 1);
                assert_eq!(s.next_shift(c), Some(at), "sc={sc:02X} c={c:X}");
                // Then on every second fall.
                assert_eq!(s.next_shift(at), Some(at + 4 * p));
            }
        }
    }

    /// An SC write while the flip-flop is 1 is its 1 -> 0 edge: one bit of
    /// the running transfer shifts, and counts towards the new one.
    #[test]
    fn sc_write_with_flip_flop_set_shifts_once() {
        let mut s = Serial::new();
        s.sb = 0x00;
        s.write_sc(0x81, false, 0x1000);
        // The fall at 0x1100 sets the flip-flop.
        s.write_sc(0x81, false, 0x1140);
        assert_eq!((s.sb, s.bits), (0x01, 1));
        // Not running: the edge doesn't shift.
        let mut s = Serial::new();
        s.write_sc(0x01, false, 0x1000);
        s.write_sc(0x81, false, 0x1140);
        assert_eq!((s.sb, s.bits), (0x00, 0));
    }

    /// A DIV reset toggles the flip-flop only if the clock bit was set,
    /// which moves the next shift by one period; in both clock speeds.
    #[test]
    fn div_reset_toggles_the_flip_flop_if_the_clock_bit_was_set() {
        for (sc, k, cgb) in [(0x81, 7, false), (0x83, 2, true)] {
            let p = 1u64 << k;
            let new = 0x10000;
            // Flip-flop 0 at the reset; clock bit set: toggles to 1, so the
            // next fall shifts.
            let mut s = Serial::new();
            s.write_sc(sc, cgb, 0x1000);
            assert!(!s.div_reset(0x1000 + p, new));
            assert_eq!(s.bits, 0);
            assert_eq!(s.next_shift(new), Some(new + 2 * p), "sc={sc:02X}");
            // Clock bit clear: no toggle, two falls to the next shift.
            let mut s = Serial::new();
            s.write_sc(sc, cgb, 0x1000);
            assert!(!s.div_reset(0x1000 + p - 1, new));
            assert_eq!(s.next_shift(new), Some(new + 4 * p), "sc={sc:02X}");
            // Flip-flop 1 and the clock bit set: the reset is the 1 -> 0
            // edge and shifts.
            let mut s = Serial::new();
            s.write_sc(sc, cgb, 0x1000);
            assert!(!s.div_reset(0x1000 + 3 * p, new));
            assert_eq!(s.bits, 1, "sc={sc:02X}");
            assert_eq!(s.next_shift(new), Some(new + 4 * p), "sc={sc:02X}");
        }
    }

    /// The lazy flip-flop agrees with the eager one shift for shift.
    #[test]
    fn lazy_next_shift_matches_the_eager_flip_flop() {
        for (sc, cgb, start) in [
            (0x81, false, 0x1F3C),
            (0x83, true, 0x1F3C),
            (0x81, true, 0x2004),
        ] {
            let (mut lazy, mut eager) = (Serial::new(), Serial::new());
            lazy.write_sc(sc, cgb, start);
            eager.write_sc_eager(sc, cgb);
            let mut c = start;
            for _ in 0..8 {
                let next = lazy.next_shift(c).unwrap();
                // Tick the eager one an M-cycle (4 counter units) at a time.
                while c < next {
                    let before = eager.bits;
                    let fell = c & !(c + 4);
                    c += 4;
                    eager.clock(fell);
                    assert_eq!(eager.bits != before, c == next, "c={c:X}");
                }
                lazy.shift();
                assert_eq!((eager.bits, eager.sb), (lazy.bits, lazy.sb), "c={c:X}");
            }
        }
    }
}
