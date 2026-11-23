//! Wall-clock frame pacing. This is the seam the audio step replaces: the
//! window loop asks "how many frames should I run now?" ([`Pacer::due`])
//! and "when should I wake next?" ([`Pacer::next_deadline`]). Audio-driven
//! pacing provides the same two answers from the audio buffer fill.

use std::time::{Duration, Instant};

use gbc_core::CLOCK_HZ;

/// Base clocks in one frame (154 lines x 456 dots).
const FRAME_CLOCKS: u64 = 70_224;

/// One frame at the exact hardware rate: 4,194,304 / 70,224 = 59.7275 Hz.
pub const FRAME_PERIOD: Duration =
    Duration::from_nanos(FRAME_CLOCKS * 1_000_000_000 / CLOCK_HZ as u64);

/// Most frames run in one wake-up when late; any further debt is dropped.
pub const MAX_CATCH_UP: u32 = 3;

pub struct Pacer {
    next: Instant,
    period: Duration,
}

impl Pacer {
    pub fn new(start: Instant) -> Self {
        Self::with_period(start, FRAME_PERIOD)
    }

    pub fn with_period(start: Instant, period: Duration) -> Self {
        Pacer {
            next: start,
            period,
        }
    }

    /// Frames to run at `now`: 0 if early, otherwise the frames owed, at
    /// most [`MAX_CATCH_UP`]. Past that the remaining debt is dropped and
    /// the schedule restarts from `now`.
    pub fn due(&mut self, now: Instant) -> u32 {
        if now < self.next {
            return 0;
        }
        let late = now - self.next;
        let owed = (late.as_nanos() / self.period.as_nanos().max(1)) as u64 + 1;
        if owed > u64::from(MAX_CATCH_UP) {
            self.next = now + self.period;
            MAX_CATCH_UP
        } else {
            self.next += self.period * owed as u32;
            owed as u32
        }
    }

    /// When the next frame is due.
    pub fn next_deadline(&self) -> Instant {
        self.next
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: Duration = Duration::from_millis(10);

    #[test]
    fn frame_period_is_the_hardware_rate() {
        let hz = 1e9 / FRAME_PERIOD.as_nanos() as f64;
        assert!((hz - 59.7275).abs() < 0.001, "{hz}");
    }

    #[test]
    fn early_runs_nothing_on_time_runs_one() {
        let t0 = Instant::now();
        let mut p = Pacer::with_period(t0, P);
        assert_eq!(p.due(t0), 1);
        assert_eq!(p.next_deadline(), t0 + P);
        assert_eq!(p.due(t0 + P / 2), 0);
        assert_eq!(p.due(t0 + P), 1);
    }

    #[test]
    fn a_late_wakeup_catches_up_exactly() {
        let t0 = Instant::now();
        let mut p = Pacer::with_period(t0, P);
        assert_eq!(p.due(t0 + P * 2 + P / 2), 3);
        assert_eq!(p.next_deadline(), t0 + P * 3);
        assert_eq!(p.due(t0 + P * 3), 1);
    }

    #[test]
    fn the_cap_is_three_frames() {
        let t0 = Instant::now();
        let mut p = Pacer::with_period(t0, P);
        assert_eq!(p.due(t0 + P * 2 + P / 2), 3, "3 owed, all run");
        let mut p = Pacer::with_period(t0, P);
        assert_eq!(p.due(t0 + P * 3 + P / 2), 3, "4 owed, 3 run");
    }

    #[test]
    fn debt_beyond_the_cap_is_dropped() {
        let t0 = Instant::now();
        let mut p = Pacer::with_period(t0, P);
        let now = t0 + P * 50 + P / 2;
        assert_eq!(p.due(now), 3);
        assert_eq!(p.next_deadline(), now + P);
        assert_eq!(p.due(now), 0, "no remaining debt");
        assert_eq!(p.due(now + P), 1);
    }
}
