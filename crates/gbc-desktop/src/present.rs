//! When to present a finished picture: at a safe phase of the display's
//! refresh (ADR 0024, amendment "presentation phase").
//!
//! The game runs at 59.73 Hz from the audio clock (ADR 0024) and the
//! display at its own rate (59.97 or 60 Hz), so the time a picture is
//! ready drifts slowly against the display's vertical blanks: by 0.07 ms
//! a frame, one full refresh every ~4 s (the "beat": one picture must
//! stay on screen for two refreshes). Presenting as soon as a picture is
//! ready puts the present, for a second or so around each beat, within a
//! millisecond or two of a vblank, where the jitter in when pictures are
//! ready (about +-1 ms: wake-ups, audio callbacks) flips presents between
//! two refreshes: a burst of pictures shown twice and never (measured:
//! 55-76 irregularities/min where the beat explains 15).
//!
//! [`Presenter`] instead starts each present inside a window of the
//! refresh interval chosen so that the present *returns* well clear of a
//! vblank ([`start_window`]): at once if the picture is ready inside the
//! window, else at the start of the next window (up to ~5 ms later). It
//! also starts at most one present per refresh interval (per `k`
//! intervals on a display running at `k` times the game rate), counted
//! from the interval the last present started in. That rule is what turns
//! the beat into a single repeat: a picture ready a little too late for
//! its interval (or pushed by jitter) takes the next one, and the
//! pictures after it follow one interval at a time, never back, until
//! the slow drift has used up the slack a refresh later. Without it,
//! jitter in when pictures are ready flips them across the window's edge
//! instead.

use std::time::{Duration, Instant};

use crate::pacing::FRAME_PERIOD;

/// A present should return at least this long after a vblank, so DWM
/// has surely composed it for the next one (it composes shortly after a
/// vblank) and its jitter can't move it back across the vblank.
pub const END_AFTER_VBLANK: Duration = Duration::from_millis(3);
/// ... and at least this long before the next vblank.
pub const END_BEFORE_VBLANK: Duration = Duration::from_millis(2);

/// The initial estimate of one present's duration (measured on Windows:
/// median 0.43 ms, p99 1.9 ms for the blit and softbuffer's GDI present at
/// 4x); refined by [`Presenter::presented`].
const PRESENT_GUESS: Duration = Duration::from_micros(500);
/// The longest a picture waits for its present (half a frame): see
/// [`schedule`]. Normally the wait is at most a refresh less the start
/// window, about 5 ms at 60 Hz.
pub const MAX_WAIT: Duration = Duration::from_nanos(FRAME_PERIOD.as_nanos() as u64 / 2);
/// Durations beyond this (a stall) don't move the estimate further.
const PRESENT_MAX: Duration = Duration::from_millis(3);

/// The display's refresh grid: a recent vertical blank and the period.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    pub vblank: Instant,
    pub period: Duration,
}

impl Grid {
    /// The vblank that starts the refresh interval containing `t`.
    pub fn slot_start(&self, t: Instant) -> Instant {
        let p = self.period.as_nanos().max(1) as i128;
        let n = signed_nanos(t, self.vblank).div_euclid(p);
        offset(self.vblank, n * p)
    }
}

/// `a - b` in nanoseconds, negative when `a` is earlier.
fn signed_nanos(a: Instant, b: Instant) -> i128 {
    if a >= b {
        (a - b).as_nanos() as i128
    } else {
        -((b - a).as_nanos() as i128)
    }
}

/// `t` moved by `ns` nanoseconds (`t` itself if that is unrepresentable).
fn offset(t: Instant, ns: i128) -> Instant {
    let d = Duration::from_nanos(ns.unsigned_abs() as u64);
    if ns >= 0 {
        t.checked_add(d)
    } else {
        t.checked_sub(d)
    }
    .unwrap_or(t)
}

/// Refreshes per emulated frame `k`, when the display's `period` is within
/// -0.5%..+3% of `k` times the game's rate for `k` = 1 or 2 (59.4-61.5 Hz,
/// 119-123 Hz). `None` elsewhere: then pictures are presented as soon as
/// they are ready, as before.
pub fn refreshes_per_frame(period: Duration) -> Option<u32> {
    let r = FRAME_PERIOD.as_secs_f64() / period.as_secs_f64();
    let k = r.round();
    ((1.0..=2.0).contains(&k) && (0.995..=1.03).contains(&(r / k))).then_some(k as u32)
}

/// When in a refresh interval of `period` a present taking `present` may
/// start, as offsets from the vblank: so that it returns at least
/// [`END_AFTER_VBLANK`] after the vblank and [`END_BEFORE_VBLANK`] before
/// the next. For 59.97 Hz and a 0.5 ms present: 2.5..14.2 ms.
pub fn start_window(period: Duration, present: Duration) -> (Duration, Duration) {
    let lo = END_AFTER_VBLANK.saturating_sub(present);
    let hi = period.saturating_sub(END_BEFORE_VBLANK + present).max(lo);
    (lo, hi)
}

/// When to start presenting a picture ready at `ready`: the earliest time
/// at or after it inside the start `window` of a refresh interval, and
/// not in an interval that starts less than `gap` intervals after
/// `last_slot` (the start of the interval the previous present started
/// in) — unless that would wait more than [`MAX_WAIT`]: then the earliest
/// time in a window, even if the previous picture is replaced before it
/// was shown. That happens only after a picture came early (the frame
/// after the LCD turns back on is short, a stalled loop catches up): one
/// picture dropped, instead of every picture for the next second or two
/// waiting most of a refresh.
pub fn schedule(
    ready: Instant,
    grid: Grid,
    last_slot: Option<Instant>,
    gap: u32,
    window: (Duration, Duration),
) -> Instant {
    let (lo, hi) = window;
    let in_window = |slot: Instant| {
        let at = ready.max(slot + lo);
        if at <= slot + hi {
            at
        } else {
            slot + grid.period + lo
        }
    };
    let natural = grid.slot_start(ready);
    let mut slot = natural;
    if let Some(last) = last_slot {
        // Half a period of tolerance: `last` came from an earlier grid
        // query, whose vblank times may differ by a few µs.
        let min = last + grid.period * gap - grid.period / 2;
        // Whole periods on this grid: `last` + period can be a few µs
        // before or after one of this grid's vblanks. `last` is in the past,
        // so this runs at most `gap` + 1 times.
        while slot < min {
            slot += grid.period;
        }
    }
    let at = in_window(slot);
    if at - ready > MAX_WAIT {
        in_window(natural)
    } else {
        at
    }
}

/// The present schedule for the window loop. Each completed picture is
/// handed to [`Presenter::picture`]; the loop presents when
/// [`Presenter::due`] (waking for [`Presenter::next`]) and reports it to
/// [`Presenter::presented`].
#[derive(Debug, Clone)]
pub struct Presenter {
    /// Smoothed duration of one present (blit included).
    took: Duration,
    /// Start of the refresh interval the last present started in.
    last_slot: Option<Instant>,
    /// When the next present should start.
    pending: Option<Instant>,
}

impl Default for Presenter {
    fn default() -> Self {
        Self::new()
    }
}

impl Presenter {
    pub fn new() -> Self {
        Presenter {
            took: PRESENT_GUESS,
            last_slot: None,
            pending: None,
        }
    }

    /// A new picture is ready at `now`, `grid` the display's refresh grid
    /// (`None` when unknown). `false` when phase presenting doesn't apply
    /// (no grid, or a refresh rate far from the game's): present it now.
    /// While a present is pending, the newest picture simply takes it.
    pub fn picture(&mut self, now: Instant, grid: Option<Grid>) -> bool {
        let Some((grid, gap)) = grid.and_then(|g| Some((g, refreshes_per_frame(g.period)?))) else {
            self.pending = None;
            return false;
        };
        if self.pending.is_none() {
            let window = start_window(grid.period, self.took);
            self.pending = Some(schedule(now, grid, self.last_slot, gap, window));
        }
        true
    }

    /// When the pending present should start.
    pub fn next(&self) -> Option<Instant> {
        self.pending
    }

    /// Whether a present is pending and due at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.pending.is_some_and(|t| now >= t)
    }

    /// A present (of the newest picture) ran from `start` to `end`.
    pub fn presented(&mut self, start: Instant, end: Instant, grid: Option<Grid>) {
        self.pending = None;
        let d = end.saturating_duration_since(start).min(PRESENT_MAX);
        // A 1/16 step toward each new measurement: one slow present (a
        // stall, capped at PRESENT_MAX) moves the window by under 0.2 ms.
        self.took = if d >= self.took {
            self.took + (d - self.took) / 16
        } else {
            self.took - (self.took - d) / 16
        };
        self.last_slot = grid.map(|g| g.slot_start(start));
    }

    /// The current estimate of one present's duration.
    pub fn present_estimate(&self) -> Duration {
        self.took
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: fn(u64) -> Duration = Duration::from_micros;

    /// 59.973 Hz, the display measured in ADR 0024's amendment.
    const P_59_973: Duration = Duration::from_nanos(16_674_170);

    fn grid(t0: Instant) -> Grid {
        Grid {
            vblank: t0 + Duration::from_secs(1),
            period: P_59_973,
        }
    }

    #[test]
    fn slot_start_is_the_vblank_at_or_before() {
        let t0 = Instant::now();
        let g = grid(t0);
        assert_eq!(g.slot_start(g.vblank), g.vblank);
        assert_eq!(g.slot_start(g.vblank + US(5)), g.vblank);
        assert_eq!(
            g.slot_start(g.vblank + P_59_973 * 3 + US(1)),
            g.vblank + P_59_973 * 3
        );
        // Before the known vblank (DWM's can be after the query).
        assert_eq!(g.slot_start(g.vblank - US(1)), g.vblank - P_59_973);
        assert_eq!(
            g.slot_start(g.vblank - P_59_973 * 2),
            g.vblank - P_59_973 * 2
        );
    }

    #[test]
    fn rates_near_the_game_rate_or_twice_it_are_phase_presented() {
        let hz = |f: f64| Duration::from_secs_f64(1.0 / f);
        assert_eq!(refreshes_per_frame(hz(59.973)), Some(1));
        assert_eq!(refreshes_per_frame(hz(60.0)), Some(1));
        assert_eq!(refreshes_per_frame(hz(59.94)), Some(1));
        assert_eq!(refreshes_per_frame(hz(120.0)), Some(2));
        assert_eq!(refreshes_per_frame(hz(75.0)), None);
        assert_eq!(refreshes_per_frame(hz(50.0)), None);
        assert_eq!(refreshes_per_frame(hz(144.0)), None);
        assert_eq!(refreshes_per_frame(hz(240.0)), None);
        assert_eq!(refreshes_per_frame(hz(59.0)), None, "display much slower");
        assert_eq!(refreshes_per_frame(Duration::ZERO), None);
    }

    #[test]
    fn the_window_keeps_the_present_end_clear_of_vblanks() {
        let (lo, hi) = start_window(P_59_973, US(2_000));
        assert_eq!(lo, US(1_000));
        assert_eq!(hi, P_59_973 - US(4_000));
        // A slow present starts right at the vblank; an absurd one still
        // gets a (one-point) window.
        assert_eq!(start_window(P_59_973, US(5_000)).0, Duration::ZERO);
        let (lo, hi) = start_window(P_59_973, US(20_000));
        assert_eq!((lo, hi), (Duration::ZERO, Duration::ZERO));
    }

    #[test]
    fn a_picture_waits_for_the_window_and_one_present_per_interval() {
        let t0 = Instant::now();
        let g = grid(t0);
        let w = (US(1_000), US(12_000));
        let v = g.vblank;
        let p = P_59_973;
        let s = |ready, last, gap| schedule(ready, g, last, gap, w);
        // Inside the window: now.
        assert_eq!(s(v + US(5_000), None, 1), v + US(5_000));
        // Before it: at its start.
        assert_eq!(s(v + US(300), None, 1), v + US(1_000));
        // After it: the next interval's start.
        assert_eq!(s(v + US(12_500), None, 1), v + p + US(1_000));
        // This interval already had a present: the next one (7.7 ms on).
        assert_eq!(s(v + US(10_000), Some(v), 1), v + p + US(1_000));
        // ... unless that is more than MAX_WAIT away (12.7 ms): now,
        // replacing the previous picture.
        assert_eq!(s(v + US(5_000), Some(v), 1), v + US(5_000));
        // The previous interval had it: this one is free.
        assert_eq!(s(v + US(5_000), Some(v - p), 1), v + US(5_000));
        // `last` from a grid a few µs off still counts as that interval,
        // either way (measured on Windows: grids from two queries differ
        // by a few µs; the second case once let two presents share an
        // interval).
        assert_eq!(s(v + US(10_000), Some(v + US(3)), 1), v + p + US(1_000));
        assert_eq!(s(v + US(11_000), Some(v - US(3)), 1), v + p + US(1_000));
        assert_eq!(s(v + US(5_000), Some(v - p - US(3)), 1), v + US(5_000));
        // 120 Hz, two refreshes per frame: not in the interval after the
        // last present's either.
        let p2 = Duration::from_secs_f64(1.0 / 120.0);
        let g2 = Grid { period: p2, ..g };
        let w2 = (US(500), US(3_500));
        let s2 = |ready, last| schedule(ready, g2, last, 2, w2);
        assert_eq!(s2(v + US(1_000), Some(v - p2 - US(3))), v + p2 + US(500));
        assert_eq!(s2(v + US(1_000), Some(v - p2 * 2)), v + US(1_000));
    }

    #[test]
    fn the_presenter_keeps_one_pending_present_and_falls_back_without_a_grid() {
        let t0 = Instant::now();
        let g = grid(t0);
        let v = g.vblank;
        let mut p = Presenter::new();
        assert!(!p.picture(v, None), "no grid: present now");
        assert_eq!(p.next(), None);
        let far = Grid {
            period: Duration::from_secs_f64(1.0 / 75.0),
            ..g
        };
        assert!(!p.picture(v, Some(far)), "75 Hz: present now");
        assert!(p.picture(v + US(14_500), Some(g)));
        // Window 2.5..14.2 ms for the initial 0.5 ms present estimate.
        let at = v + P_59_973 + US(2_500);
        assert_eq!(p.next(), Some(at));
        assert!(!p.due(at - US(1)));
        assert!(p.due(at));
        // A newer picture before the present: it takes the same present.
        assert!(p.picture(v + P_59_973 + US(500), Some(g)));
        assert_eq!(p.next(), Some(at));
        p.presented(at, at + US(2_000), Some(g));
        assert_eq!(p.next(), None);
        // The next picture, ready in the same interval, waits a refresh.
        assert!(p.picture(at + US(9_000), Some(g)));
        let lo = start_window(P_59_973, p.present_estimate()).0;
        assert_eq!(p.next(), Some(v + P_59_973 * 2 + lo));
    }

    #[test]
    fn the_present_estimate_follows_measurements() {
        let t = Instant::now();
        let mut p = Presenter::new();
        for _ in 0..100 {
            p.presented(t, t + US(3_000), None);
        }
        assert!(
            p.present_estimate().abs_diff(US(3_000)) < US(20),
            "{:?}",
            p.present_estimate()
        );
        for _ in 0..100 {
            p.presented(t, t + Duration::from_millis(500), None);
        }
        assert!(p.present_estimate() <= PRESENT_MAX);
    }

    /// A pseudo-random sequence in `0..=max` µs.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self, max: u64) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            (self.0 >> 33) % (max + 1)
        }
    }

    struct Run {
        /// Refreshes each picture stayed on screen (all but the last).
        on_screen: Vec<u64>,
        /// Presents that returned within 0.5 ms of a vblank.
        near_vblank: usize,
        /// Longest wait from ready to present start.
        max_delay: Duration,
        mean_delay: Duration,
    }

    /// `ready_jitter` µs late, with a 4 ms late one every 7 s and one 12.7 ms
    /// early every 10 s, as after the LCD turns back on) presented on a
    /// `ready_jitter` µs late, with a 4 ms late one every 7 s) presented on a
    /// display of `period`; each present takes 2 ms +- `present_jitter` µs
    /// and lands on the first vblank after it returns, as the frame log
    /// estimates it. `phase` = with [`Presenter`], else at once. Each grid
    /// query sees the vblank up to 5 µs off (measured: queries differ by a
    /// few µs).
    fn simulate(phase: bool, period: Duration, ready_jitter: u64, present_jitter: u64) -> Run {
        let t0 = Instant::now();
        let g = Grid {
            vblank: t0 + US(3_000),
            period,
        };
        let mut rng = Lcg(7);
        let mut p = Presenter::new();
        let n = (60.0 / FRAME_PERIOD.as_secs_f64()) as u32;
        let mut landed = Vec::new();
        let (mut near, mut max_delay, mut sum) = (0, Duration::ZERO, Duration::ZERO);
        let mut busy_until = t0;
        for i in 0..n {
            let late = if i % 420 == 419 {
                4_000
            } else {
                rng.next(ready_jitter)
            };
            let early = if i % 600 == 300 { 12_700 } else { 0 };
            let ready = t0 + Duration::from_millis(20) + FRAME_PERIOD * i + US(late) - US(early);
            let start = if phase {
                let q = Grid {
                    vblank: g.vblank + US(rng.next(10)) - US(5),
                    ..g
                };
                assert!(p.picture(ready, Some(q)));
                p.next().unwrap().max(busy_until)
            } else {
                ready.max(busy_until)
            };
            let took = US(2_000 - present_jitter + rng.next(2 * present_jitter));
            let end = start + took;
            busy_until = end;
            if phase {
                let q = Grid {
                    vblank: g.vblank + US(rng.next(10)) - US(5),
                    ..g
                };
                p.presented(start, end, Some(q));
            }
            let delay = start - ready;
            max_delay = max_delay.max(delay);
            sum += delay;
            let since = signed_nanos(end, g.vblank);
            let p_ns = period.as_nanos() as i128;
            let phase_ns = since.rem_euclid(p_ns);
            if phase_ns < 500_000 || p_ns - phase_ns < 500_000 {
                near += 1;
            }
            landed.push(since.div_euclid(p_ns) + 1);
        }
        Run {
            on_screen: landed.windows(2).map(|w| (w[1] - w[0]) as u64).collect(),
            near_vblank: near,
            max_delay,
            mean_delay: sum / n,
        }
    }

    fn count(v: &[u64], k: u64) -> usize {
        v.iter().filter(|&&x| x == k).count()
    }

    #[test]
    fn phase_presenting_leaves_only_the_beat() {
        // Early pictures in `simulate`.
        const EARLY: usize = 6;
        // 59.973 Hz: the beat is (59.973 - 59.7275) * 60 = 14.7 repeats
        // per minute.
        let beat = 60.0 * (1.0 / P_59_973.as_secs_f64() - 1.0 / FRAME_PERIOD.as_secs_f64());
        for (rj, pj) in [(0, 0), (1_000, 1_000), (2_000, 1_000)] {
            let r = simulate(true, P_59_973, rj, pj);
            let (zero, two, more) = (
                count(&r.on_screen, 0),
                count(&r.on_screen, 2),
                r.on_screen.iter().filter(|&&x| x > 2).count(),
            );
            // At most one picture dropped and one repeated per early one.
            assert!(zero <= EARLY, "jitter {rj}/{pj}: {zero} dropped");
            assert_eq!(more, 0, "jitter {rj}/{pj}: tripled");
            assert!(
                (two as f64) <= beat + EARLY as f64 + 1.5,
                "jitter {rj}/{pj}: {two} repeats, beat {beat:.1}"
            );
            assert_eq!(r.near_vblank, 0, "jitter {rj}/{pj}");
            assert!(r.max_delay < Duration::from_millis(9), "{:?}", r.max_delay);
            assert!(
                r.mean_delay < Duration::from_millis(2),
                "{:?}",
                r.mean_delay
            );
        }
        // 120 Hz: two refreshes each, plus the beat.
        let r = simulate(true, Duration::from_secs_f64(1.0 / 120.0), 1_000, 1_000);
        let odd = r.on_screen.iter().filter(|&&x| x != 2 && x != 3).count();
        // An early picture: 0 then 4, or 1 then 3.
        assert!(odd <= 2 * EARLY, "{odd} outside 2-3");
        let beat = 60.0 * (120.0 - 2.0 / FRAME_PERIOD.as_secs_f64());
        let three = count(&r.on_screen, 3);
        assert!(
            three as f64 <= beat + EARLY as f64 + 1.5,
            "{three} vs {beat:.1}"
        );
    }

    #[test]
    fn presenting_at_once_shows_the_bursts_it_replaces() {
        // The measured problem, reproduced: same jitter, presents as soon
        // as ready.
        let r = simulate(false, P_59_973, 1_000, 1_000);
        let irregular = count(&r.on_screen, 0) + count(&r.on_screen, 2);
        assert!(irregular > 40, "{irregular}");
        assert!(r.near_vblank > 0);
    }
}
