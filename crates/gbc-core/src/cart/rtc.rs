//! MBC3 real-time clock counting on emulated time (see
//! `docs/reference/mbc3-rtc.md`).
//!
//! The clock is caught up lazily: [`Cart`](super::Cart) records the clock
//! it last synced at, and advances the counters by the elapsed base clocks
//! only when something can observe or change them (a register write, the
//! latch, `rtc_state`/`set_rtc_state`, a reset). Reads return the latched
//! copy, which changes only on a latch, so they need no catch-up and the
//! CPU loop pays nothing for the RTC.

use super::{RtcState, RtcTime};
use crate::CLOCK_HZ;

/// `n` increments of a counter holding `v` that rolls over at `modulus`
/// (carrying into the next register) but is `width` wide: a value at or
/// above `modulus` (only reachable by a write) counts up to the top of the
/// width and wraps to 0 *without* carrying (rtc3test "Invalid rollovers",
/// "High minutes"/"High hours"). Returns the new value and the carries.
/// O(1) in `n`.
const fn count(v: u8, mut n: u64, modulus: u8, width: u8) -> (u8, u64) {
    let mut v = v as u64;
    if v >= modulus as u64 {
        let to_wrap = width as u64 - v;
        if n < to_wrap {
            return ((v + n) as u8, 0);
        }
        n -= to_wrap;
        v = 0;
    }
    // Split so `v + n` cannot overflow for any `n` (u64::MAX seconds).
    let m = modulus as u64;
    let rem = v + n % m;
    ((rem % m) as u8, n / m + rem / m)
}

impl RtcTime {
    /// Advances the running counters by `n` whole seconds (ignores
    /// `halted`; the caller checks it). Seconds and minutes roll over at
    /// 60, hours at 24, the 9-bit day counter at 512, which sets the
    /// sticky `day_carry` (Pan Docs: MBC3, "The Day Counter"; SameBoy
    /// `Core/timing.c` `GB_rtc_run`). Not a per-second loop: correct for
    /// any `n`.
    pub(super) fn add_seconds(&mut self, n: u64) {
        let (s, n) = count(self.seconds, n, 60, 64);
        let (m, n) = count(self.minutes, n, 60, 64);
        let (h, n) = count(self.hours, n, 24, 32);
        self.seconds = s;
        self.minutes = m;
        self.hours = h;
        let days = u64::from(self.days) + n;
        if days > 0x1FF {
            self.day_carry = true;
        }
        self.days = (days & 0x1FF) as u16;
    }
}

impl RtcState {
    /// Adds `seconds` of wall-clock time, with hardware rollover: seconds,
    /// minutes and hours carry at 60/60/24 (out-of-range register values
    /// wrap as in [`RtcTime`]), and the day counter overflowing past 511
    /// sets the sticky `day_carry`. A halted clock does not advance.
    /// `latched` and `subsecond_clocks` are untouched. Correct for any
    /// `seconds` (not a per-second loop).
    ///
    /// For syncing on load: `emu.load(..)`, then
    /// `emu.set_rtc_state(saved.advance_seconds(elapsed))`.
    #[must_use]
    pub fn advance_seconds(mut self, seconds: u64) -> Self {
        if !self.live.halted {
            self.live.add_seconds(seconds);
        }
        self
    }

    /// Advances the live clock by `clocks` base clocks of emulated time.
    /// The RTC's 32,768 Hz crystal does not follow CGB double speed, so a
    /// second is always [`CLOCK_HZ`] base clocks. A halted clock keeps its
    /// sub-second count (rtc3test "RTC off/400": the next tick comes the
    /// remaining time after restarting).
    pub(super) fn advance(&mut self, clocks: u64) {
        if self.live.halted {
            return;
        }
        let total = u64::from(self.subsecond_clocks) + clocks;
        self.subsecond_clocks = (total % u64::from(CLOCK_HZ)) as u32;
        self.live.add_seconds(total / u64::from(CLOCK_HZ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HZ: u64 = CLOCK_HZ as u64;

    fn time(days: u16, hours: u8, minutes: u8, seconds: u8) -> RtcTime {
        RtcTime {
            seconds,
            minutes,
            hours,
            days,
            ..RtcTime::default()
        }
    }

    #[test]
    fn carry_chain() {
        let mut t = time(255, 23, 59, 59);
        t.add_seconds(1);
        assert_eq!(t, time(256, 0, 0, 0));
        let mut t = time(3, 10, 58, 30);
        t.add_seconds(95);
        assert_eq!(t, time(3, 11, 0, 5));
    }

    #[test]
    fn day_overflow_sets_sticky_carry() {
        let mut t = time(511, 23, 59, 59);
        t.add_seconds(1);
        assert_eq!(t.days, 0);
        assert!(t.day_carry);
        // Sticky: further counting (or another overflow) leaves it set.
        t.add_seconds(86_400 * 600);
        assert!(t.day_carry);
        assert_eq!(t.days, 600 - 512);
    }

    #[test]
    fn out_of_range_values_wrap_without_carry() {
        // rtc3test "Invalid value tick": 28:63:60 + 1 s = 28:63:61.
        let mut t = time(7, 28, 63, 60);
        t.add_seconds(1);
        assert_eq!(t, time(7, 28, 63, 61));
        // "Invalid rollovers": 63 -> 0 with no carry, at each level.
        let mut t = time(7, 5, 10, 63);
        t.add_seconds(1);
        assert_eq!(t, time(7, 5, 10, 0));
        let mut t = time(7, 5, 63, 59);
        t.add_seconds(1);
        assert_eq!(t, time(7, 5, 0, 0));
        let mut t = time(7, 31, 59, 59);
        t.add_seconds(1);
        assert_eq!(t, time(7, 0, 0, 0));
        // "High minutes"/"High hours": 60-63 and 24-31 still count up.
        let mut t = time(7, 30, 61, 59);
        t.add_seconds(1);
        assert_eq!(t, time(7, 30, 62, 0));
        // Past the wrap counting is normal: 62, 63 -> 0 (no carry), then
        // 58 more seconds.
        let mut t = time(7, 5, 10, 62);
        t.add_seconds(60);
        assert_eq!(t, time(7, 5, 10, 58));
    }

    #[test]
    fn advance_seconds_plain_and_untouched_fields() {
        let s = RtcState {
            live: time(3, 10, 58, 30),
            latched: time(1, 2, 3, 4),
            subsecond_clocks: 77,
        };
        let a = s.advance_seconds(95);
        assert_eq!(a.live, time(3, 11, 0, 5));
        assert_eq!(a.latched, s.latched);
        assert_eq!(a.subsecond_clocks, 77);
    }

    #[test]
    fn advance_seconds_carries_through_days() {
        let s = RtcState {
            live: time(255, 23, 59, 59),
            ..RtcState::default()
        };
        assert_eq!(s.advance_seconds(1).live, time(256, 0, 0, 0));
        assert_eq!(s.advance_seconds(86_400 + 1).live, time(257, 0, 0, 0));
    }

    #[test]
    fn advance_seconds_day_overflow_sets_carry() {
        let s = RtcState {
            live: time(511, 23, 59, 59),
            ..RtcState::default()
        };
        let a = s.advance_seconds(1).live;
        assert_eq!(a.days, 0);
        assert!(a.day_carry);
        assert!(!s.advance_seconds(0).live.day_carry);
    }

    #[test]
    fn advance_seconds_halted_is_noop() {
        let mut s = RtcState {
            live: time(5, 5, 5, 5),
            subsecond_clocks: 9,
            ..RtcState::default()
        };
        s.live.halted = true;
        assert_eq!(s.advance_seconds(1_000_000), s);
    }

    #[test]
    fn advance_seconds_out_of_range_wraps_like_internal_path() {
        // 62 s + 2 -> 64 wraps to 0 with no minute carry.
        let s = RtcState {
            live: time(7, 5, 10, 62),
            ..RtcState::default()
        };
        assert_eq!(s.advance_seconds(2).live, time(7, 5, 10, 0));
        let mut t = s;
        t.advance(2 * HZ);
        assert_eq!(s.advance_seconds(2).live, t.live);
    }

    #[test]
    fn advance_seconds_huge_input_does_not_overflow() {
        for start in [time(0, 0, 0, 0), time(511, 23, 59, 59), time(7, 31, 63, 63)] {
            let s = RtcState {
                live: start,
                ..RtcState::default()
            };
            let a = s.advance_seconds(u64::MAX);
            assert!(a.live.days <= 0x1FF);
        }
        // 2^64-1 s mod 60 / mod 86400 checked against u128 arithmetic.
        let a = RtcState::default().advance_seconds(u64::MAX).live;
        let n = u128::from(u64::MAX);
        assert_eq!(u128::from(a.seconds), n % 60);
        assert_eq!(u128::from(a.minutes), n / 60 % 60);
        assert_eq!(u128::from(a.hours), n / 3600 % 24);
        assert_eq!(u128::from(a.days), n / 86_400 % 512);
        assert!(a.day_carry);
    }

    #[test]
    fn halt_stops_counting_and_keeps_subsecond() {
        let mut s = RtcState::default();
        s.advance(HZ / 2);
        s.live.halted = true;
        s.advance(HZ * 100);
        assert_eq!(s.live.seconds, 0);
        assert_eq!(u64::from(s.subsecond_clocks), HZ / 2);
        s.live.halted = false;
        s.advance(HZ / 2);
        assert_eq!(s.live.seconds, 1);
        assert_eq!(s.subsecond_clocks, 0);
    }

    /// Independent one-second reference, written from the rtc3test
    /// descriptions rather than from `count`.
    fn tick(t: &mut RtcTime) {
        t.seconds = (t.seconds + 1) & 0x3F;
        if t.seconds != 60 {
            return;
        }
        t.seconds = 0;
        t.minutes = (t.minutes + 1) & 0x3F;
        if t.minutes != 60 {
            return;
        }
        t.minutes = 0;
        t.hours = (t.hours + 1) & 0x1F;
        if t.hours != 24 {
            return;
        }
        t.hours = 0;
        t.days = (t.days + 1) & 0x1FF;
        if t.days == 0 {
            t.day_carry = true;
        }
    }

    #[test]
    fn large_gap_matches_stepwise_counting() {
        // ~3 years in one call, from out-of-range registers, against a
        // per-second reference.
        let start = time(500, 26, 61, 62);
        let gap = 86_400 * 1000 + 12_345;
        let mut big = RtcState {
            live: start,
            ..RtcState::default()
        };
        big.subsecond_clocks = 7;
        big.advance(gap * HZ + 3);
        let mut step = start;
        for _ in 0..gap {
            tick(&mut step);
        }
        assert_eq!(big.live, step);
        assert_eq!(big.subsecond_clocks, 10);
        assert!(big.live.day_carry);
    }
}
