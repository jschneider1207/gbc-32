//! Interrupt requests that reach IF at a later clock.
//!
//! Most requests are made at the clock they are computed for (a walk point
//! of the PPU, a timer reload) and go straight into IF. A few are known
//! before they reach IF: "IF bits B are set at clock T". [`PendingIrq`]
//! holds those as explicit state. Its owner folds [`PendingIrq::next`]
//! into its scheduler deadline and calls [`PendingIrq::post`] from its
//! sync, so IF at any clock is a function of that clock and the writes
//! before it, never of when the owner was last synced. IF itself only
//! ever holds real IF bits.
//!
//! A request stays cancellable until it is posted ([`PendingIrq::clear`]);
//! once in IF it is an ordinary IF bit (an IF write or the dispatch's
//! acknowledge clears it).

use crate::schedule::NEVER;

/// Up to two timed requests, earliest first. Two is what the rules that
/// use it can have waiting at once (the PPU: a LYC write's request and
/// line 0's mode-2 pulse).
#[derive(Clone, Copy)]
pub(crate) struct PendingIrq {
    at: [u64; 2],
    bits: [u8; 2],
}

impl PendingIrq {
    pub(crate) const NONE: Self = Self {
        at: [NEVER; 2],
        bits: [0; 2],
    };

    /// When the earliest request reaches IF, or [`NEVER`].
    #[inline(always)]
    pub(crate) const fn next(&self) -> u64 {
        self.at[0]
    }

    /// IF bits `bits` are set at clock `at`. The owner then reschedules
    /// itself ([`Self::next`]). With both slots taken (no rule today can
    /// do that) the earliest request is posted to `iflags` at once.
    pub(crate) fn request(&mut self, at: u64, bits: u8, iflags: &mut u8) {
        debug_assert!(at != NEVER && bits != 0);
        if self.at[1] != NEVER {
            debug_assert!(false, "a third timed interrupt request");
            self.pop(iflags);
        }
        if at == self.at[0] {
            self.bits[0] |= bits;
        } else if at < self.at[0] {
            self.at[1] = self.at[0];
            self.bits[1] = self.bits[0];
            self.at[0] = at;
            self.bits[0] = bits;
        } else {
            self.at[1] = at;
            self.bits[1] = bits;
        }
    }

    /// Sets in `iflags` every request that has reached IF by clock `now`.
    /// The caller tests [`Self::next`] first; out of line, off its sync.
    #[cold]
    #[inline(never)]
    pub(crate) fn post(&mut self, now: u64, iflags: &mut u8) {
        while self.at[0] <= now {
            self.pop(iflags);
        }
    }

    fn pop(&mut self, iflags: &mut u8) {
        *iflags |= self.bits[0];
        self.at[0] = self.at[1];
        self.bits[0] = self.bits[1];
        self.at[1] = NEVER;
        self.bits[1] = 0;
    }

    /// Drops every waiting request: none of them reaches IF.
    pub(crate) fn clear(&mut self) {
        *self = Self::NONE;
    }

    /// Nothing is waiting.
    #[cfg(any(test, feature = "clock-offset"))]
    pub(crate) fn is_empty(&self) -> bool {
        self.at[0] == NEVER
    }
}

#[cfg(test)]
mod tests {
    use super::{NEVER, PendingIrq};

    #[test]
    fn requests_post_at_their_clock_in_order() {
        let mut p = PendingIrq::NONE;
        let mut iflags = 0;
        assert_eq!(p.next(), NEVER);
        p.request(108, 0x02, &mut iflags);
        p.request(105, 0x01, &mut iflags);
        assert_eq!((p.next(), iflags), (105, 0));
        p.post(104, &mut iflags);
        assert_eq!((p.next(), iflags), (105, 0));
        p.post(105, &mut iflags);
        assert_eq!((p.next(), iflags), (108, 0x01));
        iflags = 0;
        p.post(200, &mut iflags);
        assert_eq!((p.next(), iflags), (NEVER, 0x02));
        assert!(p.is_empty());
    }

    #[test]
    fn same_clock_requests_merge_and_clear_drops_them() {
        let mut p = PendingIrq::NONE;
        let mut iflags = 0;
        p.request(10, 0x02, &mut iflags);
        p.request(10, 0x01, &mut iflags);
        p.request(12, 0x04, &mut iflags);
        p.post(10, &mut iflags);
        assert_eq!((p.next(), iflags), (12, 0x03));
        p.clear();
        p.post(u64::MAX - 1, &mut iflags);
        assert_eq!((p.next(), iflags), (NEVER, 0x03));
    }
}
