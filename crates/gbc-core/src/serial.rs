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
//! bit makes the flip-flop go 1 -> 0. The eager reference for the
//! differential tests keeps the flip-flop as a plain bool instead
//! and shifts a bit at a time (`eager::EagerSerial`, test builds only).
//!
//! # Lazy shifts (ADR 0011, 2026-11-24 serial amendment)
//! Of a transfer's eight shifts only the last has an effect outside the
//! port (SC bit 7 clears, IF bit 3, `Events::SERIAL_TX`), so only it is
//! an event: the `EventKind::Serial` deadline is the clock of
//! [`Serial::completion`]. The seven before it change `sb`, `out` and
//! `bits` only, and those are a function of the counter: the shift edges
//! are the counter values `v ≡ phase << (k+1)` modulo `2^(k+2)`, so the
//! number of them in `(c0, c1]` is a difference of two shifts
//! (`edges`). The three fields are kept as of the counter value `synced`,
//! and [`Serial::catch_up`] applies the shifts since in one step. It runs
//! before everything that reads or changes them or moves the edges: an
//! SB read (on the fly, [`Serial::read_sb`]), an SB write, an SC write,
//! a DIV reset, and the completion itself. The state is keyed on the
//! counter value, not the clock, so what changes only the counter's rate
//! (a speed switch, STOP mode's freeze) changes no shift, only the
//! clock of the deadline.

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
    /// Bits shifted so far in the running transfer, as of `synced` (as
    /// `sb` and `out`).
    pub(crate) bits: u8,
    /// The bits shifted out so far.
    out: u8,
    /// The counter value `sb`, `out` and `bits` are up to date with
    /// (module docs, "Lazy shifts"). The eager reference doesn't use it.
    synced: u64,
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
            synced: 0,
        }
    }

    /// A transfer on the internal clock is running (SC bits 7 and 0).
    #[inline]
    pub(crate) fn running(&self) -> bool {
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

    /// The number of shift edges at counter values up to and including
    /// `c`, plus one: the edges are the values `v ≡ phase << (k+1)` modulo
    /// `2^(k+2)`, where counter bit `k+1` becomes `phase` (the flip-flop
    /// goes 1 -> 0). Only differences are used.
    #[inline]
    fn edges(&self, c: u64) -> u64 {
        let r = self.phase << self.ff_bit();
        let m = self.ff_bit() + 1;
        (c + (1 << m) - r) >> m
    }

    /// The shifts of the running transfer that `sb`, `out` and `bits`
    /// don't have yet at counter value `c`: the edges in `(synced, c]`,
    /// and no more than the transfer has left (the last one ends it).
    #[inline]
    fn due(&self, c: u64) -> u32 {
        if !self.running() {
            return 0;
        }
        debug_assert!(c >= self.synced);
        let n = self.edges(c).saturating_sub(self.edges(self.synced));
        n.min(u64::from(8 - self.bits)) as u32
    }

    /// Brings `sb`, `out` and `bits` up to counter value `c` (module
    /// docs, "Lazy shifts"). Returns true when that completes a byte
    /// (see [`Serial::shift_by`]): only when `c` is at or past
    /// [`Serial::completion`], which is the Serial event's catch-up, since
    /// the event runs before any access on its clock. Out of line: one
    /// copy for its callers (the 64-bit shifts are long on the MCU).
    #[inline(never)]
    pub(crate) fn catch_up(&mut self, c: u64) -> bool {
        let n = self.due(c);
        self.synced = c;
        n != 0 && self.shift_by(n)
    }

    /// The counter value at which the running transfer completes, or
    /// `None` when none runs: the first edge after `synced` (the next
    /// `v > synced` with `v ≡ phase << (k+1)` modulo `2^(k+2)`), then one
    /// period for each further bit. It doesn't depend on when the port
    /// was last caught up.
    pub(crate) fn completion(&self) -> Option<u64> {
        if !self.running() {
            return None;
        }
        let r = self.phase << self.ff_bit();
        let m = self.ff_bit() + 1;
        let first = (self.edges(self.synced) << m) + r;
        Some(first + (u64::from(7 - self.bits) << m))
    }

    /// SB as read at counter value `c`: with the shifts due by then,
    /// without changing the port (reads have no side effects, `peek`).
    #[inline]
    pub(crate) fn read_sb(&self, c: u64) -> u8 {
        let n = self.due(c);
        ((u32::from(self.sb) << n) | ((1 << n) - 1)) as u8
    }

    /// A write to SB at counter value `c`. The bits shifted out before
    /// it stay in `out`; the edges don't move. Returns
    /// [`Serial::catch_up`]'s result.
    pub(crate) fn write_sb(&mut self, value: u8, c: u64) -> bool {
        let done = self.catch_up(c);
        self.sb = value;
        done
    }

    /// A write to SC at counter value `c`: the bit count clears, the
    /// flip-flop is forced to 0 (shifting one bit of a transfer that was
    /// running if it was 1; that bit counts towards the new transfer, as
    /// in SameBoy, so it can't complete a byte), then SC takes `value`.
    /// Returns [`Serial::catch_up`]'s result for the transfer running
    /// before the write.
    pub(crate) fn write_sc(&mut self, value: u8, cgb_mode: bool, c: u64) -> bool {
        let done = self.catch_up(c);
        self.bits = 0;
        if self.flip_flop(c) && self.running() {
            self.shift_by(1);
        }
        self.set_sc(value, cgb_mode);
        self.phase = (c >> self.ff_bit()) & 1;
        done
    }

    /// A DIV reset (DIV write or STOP), the counter going from `old` to
    /// `new`: if the clock bit was set it falls, toggling the flip-flop,
    /// which shifts on 1 -> 0. Returns true when a byte completes (see
    /// [`Serial::shift_by`]).
    pub(crate) fn div_reset(&mut self, old: u64, new: u64) -> bool {
        let mut done = self.catch_up(old);
        let mut ff = self.flip_flop(old);
        if old & self.clock_bit != 0 {
            ff = !ff;
            done |= !ff && self.running() && self.shift_by(1);
        }
        self.phase = ((new >> self.ff_bit()) & 1) ^ u64::from(ff);
        self.synced = new;
        done
    }

    /// The bit count and the counter value of the next shift of the
    /// running transfer, as an access at counter value `c` finds them
    /// (tests).
    #[cfg(test)]
    pub(crate) fn progress(&self, c: u64) -> (u8, Option<u64>) {
        let mut s = self.clone();
        s.catch_up(c);
        let rest = u64::from(7 - s.bits) << (s.ff_bit() + 1);
        (s.bits, s.completion().map(|v| v - rest))
    }

    /// What the lazy port, once caught up, and the eager reference must
    /// agree on (the differential tests): SB, SC, the bit count, the
    /// bits sent and the last byte.
    #[cfg(test)]
    pub(crate) fn state(&self) -> [u8; 5] {
        [self.sb, self.sc, self.bits, self.out, self.last_byte]
    }

    /// SC as read. Nothing in it depends on the shifts not yet applied:
    /// bit 7 clears at the completion, which is an event.
    pub(crate) fn read_sc(&self, cgb_mode: bool) -> u8 {
        self.sc | if cgb_mode { 0x7C } else { 0x7E }
    }

    /// `n` bit clock edges of a running transfer (1 to the `8 - bits` it
    /// has left): SB shifts out from bit 7 and 1s shift in. Returns true
    /// when the byte completes: SC bit 7 is cleared, the caller requests
    /// the serial interrupt, and `last_byte` holds the byte sent.
    fn shift_by(&mut self, n: u32) -> bool {
        debug_assert!(n >= 1 && n <= u32::from(8 - self.bits));
        // `out` above `sb`: one 16-bit shift register.
        let v = (((u32::from(self.out) << 8) | u32::from(self.sb)) << n) | ((1 << n) - 1);
        self.sb = v as u8;
        self.out = (v >> 8) as u8;
        self.bits += n as u8;
        if self.bits < 8 {
            return false;
        }
        self.last_byte = self.out;
        self.sc &= 0x7F;
        self.bits = 0;
        true
    }
}

/// The eager reference for the differential tests (here and
/// `timer::differential`): a port that keeps the flip-flop as a plain
/// bool, is clocked every M-cycle and shifts a bit at a time. It wraps a
/// [`Serial`] for the registers, so the production struct has one layout
/// in every build (review D9).
#[cfg(test)]
pub(crate) mod eager {
    use super::Serial;

    #[derive(Clone)]
    pub(crate) struct EagerSerial {
        port: Serial,
        /// The flip-flop.
        pub(super) ff: bool,
    }

    impl core::ops::Deref for EagerSerial {
        type Target = Serial;
        fn deref(&self) -> &Serial {
            &self.port
        }
    }

    impl core::ops::DerefMut for EagerSerial {
        fn deref_mut(&mut self) -> &mut Serial {
            &mut self.port
        }
    }

    impl EagerSerial {
        pub(crate) const fn new() -> Self {
            Self {
                port: Serial::new(),
                ff: false,
            }
        }

        /// The per-M-cycle hook: `fell` holds the counter bits that went
        /// from 1 to 0 (an M-cycle's tick, or a DIV reset); a fall of the
        /// clock bit toggles the flip-flop, and 1 -> 0 shifts one bit
        /// (SameBoy `GB_serial_master_edge`).
        pub(crate) fn clock(&mut self, fell: u64) -> bool {
            if fell & self.port.clock_bit == 0 {
                return false;
            }
            self.ff = !self.ff;
            !self.ff && self.port.running() && self.shift()
        }

        /// The SC write (SameBoy `GB_IO_SC`), with the flip-flop kept as
        /// a bool.
        pub(crate) fn write_sc_eager(&mut self, value: u8, cgb_mode: bool) {
            self.port.bits = 0;
            if self.ff {
                self.ff = false;
                if self.port.running() {
                    self.shift();
                }
            }
            self.port.set_sc(value, cgb_mode);
        }

        /// One bit clock edge of a running transfer. Returns true when a
        /// byte completes: SC bit 7 is cleared, the serial interrupt is
        /// requested, and `last_byte` holds the byte sent.
        fn shift(&mut self) -> bool {
            let p = &mut self.port;
            p.out = (p.out << 1) | (p.sb >> 7);
            p.sb = (p.sb << 1) | 1;
            p.bits += 1;
            if p.bits < 8 {
                return false;
            }
            p.last_byte = p.out;
            p.sc &= 0x7F;
            p.bits = 0;
            true
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::eager::EagerSerial;
    use super::*;
    use crate::testkit::Rng;

    #[test]
    fn internal_clock_transfer_sends_the_byte() {
        // Eager model: 16 falls of bit 7 (two per flip-flop cycle) send 8 bits.
        let mut s = EagerSerial::new();
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
        let mut s = EagerSerial::new();
        s.write_sc_eager(0x80, false);
        for _ in 0..100 {
            assert!(!s.clock(0xFFFF));
        }
        assert_eq!(s.read_sc(false), 0xFE);
        s.write_sc(0x80, false, 0x1234);
        assert_eq!(s.completion(), None);
        assert_eq!(s.read_sb(0x1234 + 0x10000), 0);
    }

    /// The counter value of the running transfer's next shift.
    fn next_shift(s: &Serial) -> u64 {
        s.progress(s.synced).1.unwrap()
    }

    /// The first shift after an SC write is on the second fall of the
    /// clock bit after it (the write forces the flip-flop to 0), whatever
    /// the counter's phase; in both clock speeds. Then one on every
    /// second fall, the eighth completing the byte.
    #[test]
    fn sc_write_resets_the_flip_flop() {
        for (sc, k, cgb) in [(0x81, 7, false), (0x81, 7, true), (0x83, 2, true)] {
            let p = 1u64 << k;
            for c in [0x1000, 0x1000 + p, 0x1000 + 2 * p + 1, 0x1000 + 3 * p + 1] {
                let mut s = Serial::new();
                s.sb = 0x00;
                s.write_sc(sc, cgb, c);
                // Bit k falls at multiples of 2p.
                let at = ((c >> (k + 1)) + 2) << (k + 1);
                assert_eq!(next_shift(&s), at, "sc={sc:02X} c={c:X}");
                assert_eq!(s.completion(), Some(at + 28 * p));
                // SB as read: a 1 in for each shift so far.
                for (n, sb) in [0x01, 0x03, 0x07, 0x0F, 0x1F, 0x3F, 0x7F]
                    .iter()
                    .enumerate()
                {
                    let v = at + 4 * p * n as u64;
                    assert_eq!(s.read_sb(v - 1), sb >> 1, "sc={sc:02X} c={c:X} n={n}");
                    assert_eq!(s.read_sb(v), *sb, "sc={sc:02X} c={c:X} n={n}");
                }
                // Not past the completion's edge, wherever it is caught
                // up from.
                assert!(!s.clone().catch_up(at + 28 * p - 1));
                assert!(s.clone().catch_up(at + 28 * p));
                assert!(!s.catch_up(at + 9 * p));
                assert_eq!((s.sb, s.bits), (0x07, 3));
                assert_eq!(s.completion(), Some(at + 28 * p));
                assert!(s.catch_up(at + 1000 * p));
                assert_eq!((s.sb, s.bits, s.last_byte), (0xFF, 0, 0x00));
                assert_eq!(s.read_sc(cgb) & 0x80, 0);
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
            assert_eq!(next_shift(&s), new + 2 * p, "sc={sc:02X}");
            // Clock bit clear: no toggle, two falls to the next shift.
            let mut s = Serial::new();
            s.write_sc(sc, cgb, 0x1000);
            assert!(!s.div_reset(0x1000 + p - 1, new));
            assert_eq!(next_shift(&s), new + 4 * p, "sc={sc:02X}");
            // Flip-flop 1 and the clock bit set: the reset is the 1 -> 0
            // edge and shifts.
            let mut s = Serial::new();
            s.write_sc(sc, cgb, 0x1000);
            assert!(!s.div_reset(0x1000 + 3 * p, new));
            assert_eq!(s.bits, 1, "sc={sc:02X}");
            assert_eq!(next_shift(&s), new + 4 * p, "sc={sc:02X}");
        }
    }

    /// How often the differential test reached the cases it is for, so
    /// it can't silently stop covering them.
    #[derive(Default, Debug)]
    struct Coverage {
        /// Bytes completed on the normal and on the fast clock.
        done: [u32; 2],
        /// Observations with one, and with two or more, shifts not yet
        /// applied to the lazy port.
        read_pending: [u32; 2],
        /// SB and SC writes with shifts not yet applied.
        sb_write_pending: u32,
        sc_write_pending: u32,
        /// SC writes with the flip-flop at 1 in a running transfer (the
        /// write shifts a bit).
        sc_write_shift: u32,
        /// DIV resets with shifts not yet applied, those that shifted a
        /// bit, and those whose bit completed the byte.
        div_pending: u32,
        div_shift: u32,
        div_done: u32,
        /// Speed switches and STOP modes in a running transfer.
        switch_running: u32,
        stop_running: u32,
        /// Observations of a transfer waiting for an external clock.
        external: u32,
        /// Gaps of more than a whole transfer with no observation.
        long_gap: u32,
    }

    /// The lazy port and the eager per-bit reference on one system
    /// counter, driven as the bus drives the lazy one.
    struct Pair<'a> {
        lazy: Serial,
        eager: EagerSerial,
        /// The system counter.
        c: u64,
        cgb: bool,
        /// The counter value of the Serial event, as the bus keeps its
        /// deadline: the lazy port's completion, asked for where the bus
        /// asks (after an SC write, a DIV reset, a speed switch and
        /// STOP mode's end), and nowhere else.
        deadline: Option<u64>,
        cov: &'a mut Coverage,
    }

    impl Pair<'_> {
        /// The counter advances by `d` (an M-cycle: 4). The eager port
        /// sees the bits that fell; the lazy one only its event.
        fn advance(&mut self, d: u64) {
            let old = self.c;
            self.c += d;
            let done = self.eager.clock(old & !self.c);
            let due = self.deadline == Some(self.c);
            if due {
                let fast = usize::from(self.lazy.sc & 0x02 != 0);
                assert!(self.lazy.catch_up(self.c), "c={:X}", self.c);
                self.deadline = self.lazy.completion();
                assert_eq!(self.deadline, None);
                self.cov.done[fast] += 1;
            }
            assert_eq!(done, due, "completion at c={:X}", self.c);
        }

        /// Shifts the lazy port hasn't applied at the current counter.
        fn pending(&self) -> u32 {
            self.lazy.due(self.c)
        }

        /// Every observation: SB and SC as read; the port's whole state
        /// as an access now would find it; the deadline, which nothing
        /// but the bus's own requests may move.
        fn check(&mut self, at: &str) {
            let (l, e, c) = (&self.lazy, &self.eager, self.c);
            let at = std::format!("{at} at c={c:X}");
            match self.pending() {
                0 => {}
                1 => self.cov.read_pending[0] += 1,
                _ => self.cov.read_pending[1] += 1,
            }
            self.cov.external += u32::from(l.sc & 0x81 == 0x80);
            assert_eq!(l.read_sb(c), e.sb, "SB, {at}");
            assert_eq!(l.read_sc(self.cgb), e.read_sc(self.cgb), "SC, {at}");
            assert_eq!(l.completion(), self.deadline, "deadline, {at}");
            let mut s = l.clone();
            assert!(!s.catch_up(c), "{at}");
            assert_eq!(
                (s.sb, s.sc, s.bits, s.out, s.last_byte, s.flip_flop(c)),
                (e.sb, e.sc, e.bits, e.out, e.last_byte, e.ff),
                "state, {at}"
            );
            assert_eq!(s.completion(), self.deadline, "caught-up deadline, {at}");
        }

        fn write_sb(&mut self, value: u8) {
            self.cov.sb_write_pending += u32::from(self.pending() != 0);
            assert!(!self.lazy.write_sb(value, self.c));
            self.eager.sb = value;
        }

        fn write_sc(&mut self, value: u8) {
            let l = &self.lazy;
            self.cov.sc_write_pending += u32::from(self.pending() != 0);
            self.cov.sc_write_shift += u32::from(self.eager.ff && l.running());
            assert!(!self.lazy.write_sc(value, self.cgb, self.c));
            self.eager.write_sc_eager(value, self.cgb);
            self.deadline = self.lazy.completion();
        }

        /// A DIV write (or STOP's reset): every set counter bit falls
        /// and the counter rebases (`Timer::write_div`).
        fn div_reset(&mut self) {
            let old = self.c;
            self.cov.div_pending += u32::from(self.pending() != 0);
            self.c = (old | 0xFFFF) + 1;
            let bits = self.eager.bits;
            let done = self.eager.clock(old);
            self.cov.div_shift += u32::from(done || self.eager.bits != bits);
            self.cov.div_done += u32::from(done);
            assert_eq!(self.lazy.div_reset(old, self.c), done, "c={old:X}");
            self.deadline = self.lazy.completion();
        }

        /// The bus's `schedule_serial`.
        fn reschedule(&mut self) {
            self.deadline = self.lazy.completion();
        }
    }

    fn run_sequence(rng: &mut Rng, cov: &mut Coverage) {
        let cgb = rng.below(4) != 0;
        let mut p = Pair {
            lazy: Serial::new(),
            eager: EagerSerial::new(),
            // Both flip-flops 0 (counter bits 8 and 3 clear).
            c: (rng.below(1 << 20) << 9) + 0x10000,
            cgb,
            deadline: None,
            cov,
        };
        // SC values: mostly a transfer on the internal clock.
        let sc = |rng: &mut Rng| {
            let v = [0x81, 0x83, 0x81, 0x83, 0x80, 0x01, 0x00, 0x82][rng.below(8) as usize];
            v | (rng.byte() & 0x7C)
        };
        for _ in 0..rng.below(300) {
            p.advance(4);
        }
        p.check("start");
        for _ in 0..60 {
            match rng.below(24) {
                // M-cycles, observed one by one.
                0..=4 => {
                    for _ in 0..rng.below(40) {
                        p.advance(4);
                        p.check("tick");
                    }
                }
                // A gap with no observation: a few bits, or more than a
                // whole transfer (1,024 M-cycles and the wait for the
                // first edge on the normal clock).
                5..=9 => {
                    let n = match rng.below(4) {
                        0 => 1200 + rng.below(3000),
                        1 => rng.below(40),
                        _ => rng.below(1100),
                    };
                    p.cov.long_gap += u32::from(n >= 1200 && p.lazy.running());
                    for _ in 0..n {
                        p.advance(4);
                    }
                }
                10..=12 => p.write_sb(rng.byte()),
                13..=16 => p.write_sc(sc(rng)),
                17 | 18 => p.div_reset(),
                // A speed switch changes the counter's rate, which is no
                // edge. To single speed the counter gains a clock the CPU
                // doesn't see (`Timer::speed_switch`, `short`): observed
                // off the M-cycle grid, then 3 more to the next boundary.
                19 | 20 => {
                    p.cov.switch_running += u32::from(p.lazy.running());
                    // No catch-up, only the deadline (`Ctx::speed_switch`).
                    p.reschedule();
                    if rng.below(2) == 0 {
                        p.advance(1);
                        p.check("switch");
                        p.advance(3);
                    }
                }
                // STOP mode: the DIV reset, then the counter holds its
                // value for any length of time; the wake-up sets the
                // deadline again.
                21 => {
                    p.div_reset();
                    p.cov.stop_running += u32::from(p.lazy.running());
                    p.check("stopped");
                    p.reschedule();
                }
                // A catch-up nothing needed (the `eager-sync` feature).
                _ => assert!(!p.lazy.catch_up(p.c)),
            }
            p.check("op");
        }
    }

    /// The lazy port agrees with the eager per-bit reference at every
    /// observation, and completes on the same counter value, under random
    /// interleavings of SB/SC reads and writes mid-transfer, DIV resets,
    /// speed switches, STOP mode and idle gaps, on both bit clocks and on
    /// the external one.
    #[test]
    fn lazy_port_matches_the_eager_per_bit_reference() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let mut cov = Coverage::default();
        for _ in 0..8000 {
            run_sequence(&mut rng, &mut cov);
        }
        let c = &cov;
        let floors = [
            c.done[0],
            c.done[1],
            c.read_pending[0],
            c.read_pending[1],
            c.sb_write_pending,
            c.sc_write_pending,
            c.sc_write_shift,
            c.div_pending,
            c.div_shift,
            c.div_done,
            c.switch_running,
            c.stop_running,
            c.external,
            c.long_gap,
        ];
        assert!(floors.iter().all(|&n| n >= 50), "{cov:?}");
    }
}
