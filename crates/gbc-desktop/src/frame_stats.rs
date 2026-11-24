//! `GBC_FRAME_STATS=1`: how evenly the window loop produces and presents
//! frames, printed to stderr every [`STATS_PERIOD`]. The window keeps an
//! `Option<FrameStats>` that is `None` unless the variable is set, so the
//! normal run pays one branch per event. Pure: callers pass the times in.

use std::time::{Duration, Instant};

/// How often a line is printed.
pub const STATS_PERIOD: Duration = Duration::from_secs(4);

/// `Some` when `GBC_FRAME_STATS=1`.
pub fn from_env(now: Instant) -> Option<FrameStats> {
    std::env::var_os("GBC_FRAME_STATS")
        .is_some_and(|v| v == "1")
        .then(|| FrameStats::new(now))
}

pub struct FrameStats {
    since: Instant,
    emulated: u64,
    presented: u64,
    /// Completed frames replaced by a newer one before any present.
    unpresented: u64,
    /// Wake-ups that ran 0, 1, 2 and 3 or more frames.
    per_wake: [u64; 4],
    /// A completed frame is waiting for a present.
    pending: bool,
    last_present: Option<Instant>,
    /// Intervals between presents in this period, for the percentiles.
    intervals: Vec<Duration>,
    late_sum: Duration,
    late_max: Duration,
    late_count: u64,
}

impl FrameStats {
    pub fn new(now: Instant) -> Self {
        FrameStats {
            since: now,
            emulated: 0,
            presented: 0,
            unpresented: 0,
            per_wake: [0; 4],
            pending: false,
            last_present: None,
            intervals: Vec::with_capacity(512),
            late_sum: Duration::ZERO,
            late_max: Duration::ZERO,
            late_count: 0,
        }
    }

    /// A timed wake-up asked for `requested` and ran at `actual`.
    pub fn woke(&mut self, requested: Instant, actual: Instant) {
        let late = actual.saturating_duration_since(requested);
        self.late_sum += late;
        self.late_max = self.late_max.max(late);
        self.late_count += 1;
    }

    /// One wake-up ran `n` frames.
    pub fn ran(&mut self, n: u32) {
        self.per_wake[(n as usize).min(3)] += 1;
    }

    /// One frame was emulated; `completed` if it produced a new picture.
    pub fn emulated(&mut self, completed: bool) {
        self.emulated += 1;
        if completed {
            if self.pending {
                self.unpresented += 1;
            }
            self.pending = true;
        }
    }

    /// The window presented at `now`.
    pub fn presented(&mut self, now: Instant) {
        self.presented += 1;
        self.pending = false;
        if let Some(prev) = self.last_present {
            self.intervals.push(now - prev);
        }
        self.last_present = Some(now);
    }

    /// The line to print if a period has passed (and then starts the next).
    pub fn report(&mut self, now: Instant) -> Option<String> {
        let span = now.duration_since(self.since);
        if span < STATS_PERIOD {
            return None;
        }
        let secs = span.as_secs_f64();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        self.intervals.sort_unstable();
        let pick = |q: f64| {
            let n = self.intervals.len();
            if n == 0 {
                return Duration::ZERO;
            }
            // Nearest rank.
            let i = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
            self.intervals[i]
        };
        let late_mean = if self.late_count == 0 {
            Duration::ZERO
        } else {
            self.late_sum / self.late_count as u32
        };
        let [w0, w1, w2, w3] = self.per_wake;
        let line = format!(
            "frames: emulated {:.2}/s presented {:.2}/s unpresented {} \
             per wake 0:{w0} 1:{w1} 2:{w2} 3+:{w3} \
             present interval p1 {:.1} p99 {:.1} max {:.1} ms \
             wake late mean {:.2} max {:.2} ms",
            self.emulated as f64 / secs,
            self.presented as f64 / secs,
            self.unpresented,
            ms(pick(0.01)),
            ms(pick(0.99)),
            ms(self.intervals.last().copied().unwrap_or_default()),
            ms(late_mean),
            ms(self.late_max),
        );
        let last_present = self.last_present;
        let pending = self.pending;
        let mut intervals = std::mem::take(&mut self.intervals);
        intervals.clear();
        *self = FrameStats {
            intervals,
            last_present,
            pending,
            ..FrameStats::new(now)
        };
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_before_the_period() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        assert!(s.report(t0 + STATS_PERIOD - ms(1)).is_none());
        assert!(s.report(t0 + STATS_PERIOD).is_some());
        assert!(s.report(t0 + STATS_PERIOD).is_none(), "a new period began");
    }

    #[test]
    fn counts_rates_and_frames_never_presented() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        // 8 frames: two per wake-up, only the second of each presented.
        for i in 0..4 {
            s.emulated(true);
            s.emulated(true);
            s.ran(2);
            s.presented(t0 + ms(20 * i));
        }
        // A frame with the LCD off completes nothing: not unpresented.
        s.emulated(false);
        s.ran(1);
        s.ran(0);
        s.ran(5);
        let line = s.report(t0 + ms(4000)).unwrap();
        assert!(line.contains("emulated 2.25/s"), "{line}");
        assert!(line.contains("presented 1.00/s"), "{line}");
        assert!(line.contains("unpresented 4 "), "{line}");
        assert!(line.contains("per wake 0:1 1:1 2:4 3+:1 "), "{line}");
    }

    #[test]
    fn a_pending_frame_presented_after_the_report_is_not_lost() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        s.emulated(true);
        s.report(t0 + STATS_PERIOD).unwrap();
        s.emulated(true);
        let line = s.report(t0 + STATS_PERIOD * 2).unwrap();
        assert!(line.contains("unpresented 1 "), "{line}");
    }

    #[test]
    fn present_interval_percentiles() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        // 100 intervals: 98 of 10 ms, one of 20, one of 40.
        let mut t = t0;
        s.presented(t);
        for i in 0..100 {
            t += match i {
                50 => ms(40),
                70 => ms(20),
                _ => ms(10),
            };
            s.presented(t);
        }
        let line = s.report(t0 + ms(5000)).unwrap();
        assert!(line.contains("p1 10.0 p99 20.0 max 40.0 ms"), "{line}");
        // The next period's first interval is measured from the last
        // present before the report.
        s.presented(t + ms(17));
        let line = s.report(t0 + ms(10_000)).unwrap();
        assert!(line.contains("max 17.0 ms"), "{line}");
    }

    #[test]
    fn wake_lateness_mean_and_max() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        s.woke(t0, t0 + ms(1));
        s.woke(t0, t0 + ms(5));
        // Early (woken by another event): counts as on time.
        s.woke(t0 + ms(10), t0);
        let line = s.report(t0 + STATS_PERIOD).unwrap();
        assert!(line.contains("wake late mean 2.00 max 5.00 ms"), "{line}");
    }
}
