//! `GBC_FRAME_STATS=1`: how evenly the window loop produces and presents
//! frames, printed to stderr every [`STATS_PERIOD`]. The window keeps an
//! `Option<FrameStats>` that is `None` unless the variable is set, so the
//! normal run pays one branch per event. Pure: callers pass the times in.
//! Besides pacing it keeps the longest main-thread step, present and
//! gamepad poll, and the save writes, so a long interval can be attributed.

use std::time::{Duration, Instant};

/// How often a line is printed.
pub const STATS_PERIOD: Duration = Duration::from_secs(4);

/// `Some` when `GBC_FRAME_STATS=1`.
pub fn from_env(now: Instant) -> Option<FrameStats> {
    std::env::var_os("GBC_FRAME_STATS")
        .is_some_and(|v| v == "1")
        .then(|| FrameStats::new(now))
}

/// Gaps between successive events, kept across periods so a period's
/// first gap is measured from the previous period's last event.
struct Gaps {
    last: Option<Instant>,
    gaps: Vec<Duration>,
}

impl Gaps {
    fn new() -> Self {
        Gaps {
            last: None,
            gaps: Vec::with_capacity(512),
        }
    }

    fn mark(&mut self, now: Instant) {
        if let Some(prev) = self.last {
            self.gaps.push(now.saturating_duration_since(prev));
        }
        self.last = Some(now);
    }

    /// p1, p99 and max (nearest rank) in ms, then forgets the gaps.
    fn take(&mut self) -> [f64; 3] {
        self.gaps.sort_unstable();
        let n = self.gaps.len();
        let pick = |q: f64| {
            if n == 0 {
                return 0.0;
            }
            let i = ((q * n as f64).ceil() as usize).clamp(1, n) - 1;
            self.gaps[i].as_secs_f64() * 1e3
        };
        let out = [pick(0.01), pick(0.99), pick(1.0)];
        self.gaps.clear();
        out
    }
}

pub struct FrameStats {
    since: Instant,
    emulated: u64,
    presented: u64,
    /// Completed frames replaced by a newer one before any present.
    unpresented: u64,
    /// Completed frames identical to the one before: the game itself did
    /// not redraw (it runs below 60 fps or lags, as on hardware).
    unchanged: u64,
    /// Frames that completed no picture (LCD off): the game blanked the
    /// screen, as on hardware. They lengthen the frame and present
    /// intervals without being a host stall.
    blank: u64,
    /// Wake-ups that ran 0, 1, 2 and 3 or more frames.
    per_wake: [u64; 4],
    /// A completed frame is waiting for a present.
    pending: bool,
    /// Between completed frames, and between presents.
    frame_gaps: Gaps,
    present_gaps: Gaps,
    late_sum: Duration,
    late_max: Duration,
    late_count: u64,
    /// Longest single stretch of main-thread work, by kind, so a long
    /// frame interval can be attributed: running the frames of one
    /// wake-up (autosave hand-off included), one blit + present, and one
    /// gamepad poll.
    step_max: Duration,
    /// Longest event-loop iteration: wake-up to `about_to_wait` done, all
    /// handlers in between (redraws, input, the stats print) included.
    busy_max: Duration,
    /// When the current iteration began.
    iter_start: Option<Instant>,
    present_max: Duration,
    pads_max: Duration,
    /// Save files written (by whichever thread writes them) and the
    /// longest write.
    saves: u64,
    save_max: Duration,
}

impl FrameStats {
    pub fn new(now: Instant) -> Self {
        FrameStats {
            since: now,
            emulated: 0,
            presented: 0,
            unpresented: 0,
            unchanged: 0,
            blank: 0,
            per_wake: [0; 4],
            pending: false,
            frame_gaps: Gaps::new(),
            present_gaps: Gaps::new(),
            late_sum: Duration::ZERO,
            late_max: Duration::ZERO,
            late_count: 0,
            step_max: Duration::ZERO,
            busy_max: Duration::ZERO,
            iter_start: None,
            present_max: Duration::ZERO,
            pads_max: Duration::ZERO,
            saves: 0,
            save_max: Duration::ZERO,
        }
    }

    /// An event-loop iteration began at `now`.
    pub fn iteration_began(&mut self, now: Instant) {
        self.iter_start = Some(now);
    }

    /// The iteration begun last ended at `now`.
    pub fn iteration_ended(&mut self, now: Instant) {
        if let Some(t) = self.iter_start.take() {
            self.busy_max = self.busy_max.max(now.saturating_duration_since(t));
        }
    }

    /// Running one wake-up's frames took `d`.
    pub fn step_took(&mut self, d: Duration) {
        self.step_max = self.step_max.max(d);
    }

    /// One blit and present took `d`.
    pub fn present_took(&mut self, d: Duration) {
        self.present_max = self.present_max.max(d);
    }

    /// One gamepad poll took `d`.
    pub fn pads_took(&mut self, d: Duration) {
        self.pads_max = self.pads_max.max(d);
    }

    /// `count` save files were written, the longest taking `max`.
    pub fn saved(&mut self, count: u64, max: Duration) {
        self.saves += count;
        self.save_max = self.save_max.max(max);
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

    /// One frame was emulated at `now`; `completed` if it produced a new
    /// picture, `unchanged` if that picture equals the previous one.
    pub fn emulated(&mut self, completed: bool, unchanged: bool, now: Instant) {
        self.emulated += 1;
        self.blank += u64::from(!completed);
        if completed {
            self.unchanged += u64::from(unchanged);
            if self.pending {
                self.unpresented += 1;
            }
            self.pending = true;
            self.frame_gaps.mark(now);
        }
    }

    /// The window presented at `now`.
    pub fn presented(&mut self, now: Instant) {
        self.presented += 1;
        self.pending = false;
        self.present_gaps.mark(now);
    }

    /// The line to print if a period has passed (and then starts the next).
    pub fn report(&mut self, now: Instant) -> Option<String> {
        let span = now.duration_since(self.since);
        if span < STATS_PERIOD {
            return None;
        }
        let secs = span.as_secs_f64();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let late_mean = if self.late_count == 0 {
            Duration::ZERO
        } else {
            self.late_sum / self.late_count as u32
        };
        let [w0, w1, w2, w3] = self.per_wake;
        let [f1, f99, fmax] = self.frame_gaps.take();
        let [p1, p99, pmax] = self.present_gaps.take();
        let line = format!(
            "frames: emulated {:.2}/s presented {:.2}/s unpresented {} unchanged {} blank {} \
             per wake 0:{w0} 1:{w1} 2:{w2} 3+:{w3} \
             frame interval p1 {f1:.1} p99 {f99:.1} max {fmax:.1} ms \
             present interval p1 {p1:.1} p99 {p99:.1} max {pmax:.1} ms \
             wake late mean {:.2} max {:.2} ms \
             busy max {:.1} step max {:.1} present max {:.1} pads max {:.1} ms \
             saves {} max {:.1} ms",
            self.emulated as f64 / secs,
            self.presented as f64 / secs,
            self.unpresented,
            self.unchanged,
            self.blank,
            ms(late_mean),
            ms(self.late_max),
            ms(self.busy_max),
            ms(self.step_max),
            ms(self.present_max),
            ms(self.pads_max),
            self.saves,
            ms(self.save_max),
        );
        let frame_gaps = std::mem::replace(&mut self.frame_gaps, Gaps::new());
        let present_gaps = std::mem::replace(&mut self.present_gaps, Gaps::new());
        let pending = self.pending;
        let iter_start = self.iter_start;
        *self = FrameStats {
            frame_gaps,
            present_gaps,
            pending,
            iter_start,
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
            s.emulated(true, false, t0);
            s.emulated(true, false, t0);
            s.ran(2);
            s.presented(t0 + ms(20 * i));
        }
        // A frame with the LCD off completes nothing: not unpresented.
        s.emulated(false, false, t0);
        s.ran(1);
        s.ran(0);
        s.ran(5);
        // Two more, both repeating the picture before (game lag).
        s.emulated(true, true, t0);
        s.emulated(true, true, t0);
        s.emulated(false, true, t0);
        let line = s.report(t0 + ms(4000)).unwrap();
        assert!(line.contains("emulated 3.00/s"), "{line}");
        assert!(line.contains("presented 1.00/s"), "{line}");
        assert!(
            line.contains("unpresented 5 unchanged 2 blank 2 "),
            "{line}"
        );
        assert!(line.contains("per wake 0:1 1:1 2:4 3+:1 "), "{line}");
    }

    #[test]
    fn a_pending_frame_presented_after_the_report_is_not_lost() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        s.emulated(true, false, t0);
        s.report(t0 + STATS_PERIOD).unwrap();
        s.emulated(true, false, t0);
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
        assert!(
            line.contains("present interval p1 17.0 p99 17.0 max 17.0 ms"),
            "{line}"
        );
    }

    #[test]
    fn frame_interval_counts_completed_frames_only() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        s.emulated(true, false, t0);
        s.emulated(false, false, t0 + ms(5));
        s.emulated(true, false, t0 + ms(20));
        s.emulated(true, false, t0 + ms(30));
        let line = s.report(t0 + STATS_PERIOD).unwrap();
        assert!(
            line.contains("frame interval p1 10.0 p99 20.0 max 20.0 ms"),
            "{line}"
        );
        assert!(
            line.contains("present interval p1 0.0 p99 0.0 max 0.0 ms"),
            "{line}"
        );
        // Measured across the report from the last completed frame.
        s.emulated(true, false, t0 + ms(47));
        let line = s.report(t0 + STATS_PERIOD * 2).unwrap();
        assert!(
            line.contains("frame interval p1 17.0 p99 17.0 max 17.0 ms"),
            "{line}"
        );
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

    #[test]
    fn work_maxima_and_saves_attribute_a_stall() {
        let t0 = Instant::now();
        let mut s = FrameStats::new(t0);
        s.step_took(ms(3));
        s.step_took(ms(150));
        s.step_took(ms(2));
        s.present_took(ms(4));
        s.present_took(ms(1));
        s.pads_took(ms(7));
        s.saved(1, ms(90));
        s.saved(2, ms(40));
        s.saved(0, Duration::ZERO);
        s.iteration_began(t0);
        s.iteration_ended(t0 + ms(160));
        s.iteration_began(t0 + ms(200));
        s.iteration_ended(t0 + ms(210));
        s.iteration_ended(t0 + ms(900)); // no iteration open: ignored
        // An iteration open across the report counts in the next period.
        s.iteration_began(t0 + ms(3990));
        let line = s.report(t0 + STATS_PERIOD).unwrap();
        assert!(
            line.contains(
                "busy max 160.0 step max 150.0 present max 4.0 pads max 7.0 ms saves 3 max 90.0 ms"
            ),
            "{line}"
        );
        // Each period starts from zero.
        s.iteration_ended(t0 + ms(4010));
        s.step_took(ms(5));
        let line = s.report(t0 + STATS_PERIOD * 2).unwrap();
        assert!(
            line.contains(
                "busy max 20.0 step max 5.0 present max 0.0 pads max 0.0 ms saves 0 max 0.0 ms"
            ),
            "{line}"
        );
    }
}
