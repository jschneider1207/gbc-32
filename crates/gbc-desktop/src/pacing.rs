//! Frame pacing. [`Pacer`] is the wall-clock fallback: the window loop asks
//! it "how many frames should I run now?" ([`Pacer::due`])
//! and "when should I wake next?" ([`Pacer::next_deadline`]). Audio-driven
//! pacing ([`Pacing::Audio`], ADR 0024) answers both from the buffer fill.

use std::time::{Duration, Instant};

use gbc_core::{CLOCK_HZ, StereoSample};

use crate::audio::AudioOut;

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

/// Most frames one wake-up runs under audio pacing, so input and redraws
/// are still handled when the emulator can't keep up.
pub const AUDIO_MAX_FRAMES_PER_WAKE: u32 = 4;

/// Bounds on the audio-paced sleep: wake at least this often to top up.
const AUDIO_WAKE_MIN: Duration = Duration::from_millis(1);
const AUDIO_WAKE_MAX: Duration = Duration::from_millis(8);

/// Audio pacing: run another frame while the buffer is below its target,
/// up to the per-wake-up cap.
pub fn audio_should_run(fill: usize, target: usize, ran: u32) -> bool {
    ran < AUDIO_MAX_FRAMES_PER_WAKE && fill < target
}

/// Audio pacing: how long until `fill` device frames drain to `target` at
/// `rate`, within [`AUDIO_WAKE_MIN`]..=[`AUDIO_WAKE_MAX`]; zero when
/// already below target (the cap stopped the frame loop).
pub fn audio_wake_after(fill: usize, target: usize, rate: u32) -> Duration {
    if fill < target {
        return Duration::ZERO;
    }
    let drain = Duration::from_secs_f64((fill - target) as f64 / f64::from(rate.max(1)));
    drain.clamp(AUDIO_WAKE_MIN, AUDIO_WAKE_MAX)
}

/// How the window loop paces frames: by the audio buffer when a device is
/// open (ADR 0024), else by the wall clock, silently. Each wake-up calls
/// [`Pacing::begin`], runs frames while [`Pacing::should_run`] (feeding
/// each one's audio to [`Pacing::push_audio`]), then sleeps until
/// [`Pacing::next_wake`].
pub enum Pacing {
    Audio(Box<AudioOut>),
    Clock { pacer: Pacer, due: u32 },
}

impl Pacing {
    /// Audio pacing if the default device opens, else the clock with one
    /// warning. `GBC_NO_AUDIO=1` skips the device (silent, clock-paced).
    pub fn open(now: Instant) -> Self {
        if std::env::var_os("GBC_NO_AUDIO").is_some_and(|v| v == "1") {
            return Self::clock(now);
        }
        match AudioOut::open() {
            Ok(a) => Pacing::Audio(Box::new(a)),
            Err(e) => {
                eprintln!("warning: no audio ({e}); running silent on the wall clock");
                Self::clock(now)
            }
        }
    }

    pub fn clock(now: Instant) -> Self {
        Pacing::Clock {
            pacer: Pacer::new(now),
            due: 0,
        }
    }

    /// Restarts the clock schedule (the window just appeared).
    pub fn restart(&mut self, now: Instant) {
        if let Pacing::Clock { pacer, .. } = self {
            *pacer = Pacer::new(now);
        }
    }

    /// Starts a wake-up. A failed audio stream switches to the clock here.
    pub fn begin(&mut self, now: Instant) {
        if let Pacing::Audio(a) = self
            && let Some(e) = a.failure(now)
        {
            eprintln!("warning: audio stopped ({e}); running silent on the wall clock");
            *self = Self::clock(now);
        }
        if let Pacing::Clock { pacer, due } = self {
            *due = pacer.due(now);
        }
    }

    /// Whether to run frame number `ran` (from 0) of this wake-up.
    pub fn should_run(&self, ran: u32) -> bool {
        match self {
            Pacing::Audio(a) => audio_should_run(a.fill(), a.target(), ran),
            Pacing::Clock { due, .. } => ran < *due,
        }
    }

    /// One frame's samples (dropped under the clock).
    pub fn push_audio(&mut self, samples: &[StereoSample]) {
        if let Pacing::Audio(a) = self {
            a.push(samples);
        }
    }

    pub fn next_wake(&self, now: Instant) -> Instant {
        match self {
            Pacing::Audio(a) => now + audio_wake_after(a.fill(), a.target(), a.rate()),
            Pacing::Clock { pacer, .. } => pacer.next_deadline(),
        }
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

    #[test]
    fn audio_runs_below_target_and_waits_above() {
        assert!(audio_should_run(0, 2400, 0));
        assert!(audio_should_run(2399, 2400, 1));
        assert!(!audio_should_run(2400, 2400, 0), "at target: wait");
        assert!(!audio_should_run(3000, 2400, 0));
    }

    #[test]
    fn audio_runs_at_most_the_cap_per_wakeup() {
        assert!(audio_should_run(0, 2400, AUDIO_MAX_FRAMES_PER_WAKE - 1));
        assert!(!audio_should_run(0, 2400, AUDIO_MAX_FRAMES_PER_WAKE));
        // An empty buffer refills over several wake-ups, not one.
        let (mut fill, mut ran) = (0, 0);
        while audio_should_run(fill, 48_000, ran) {
            fill += 800;
            ran += 1;
        }
        assert_eq!(ran, AUDIO_MAX_FRAMES_PER_WAKE);
    }

    #[test]
    fn audio_wakes_when_the_fill_reaches_target() {
        assert_eq!(audio_wake_after(100, 2400, 48_000), Duration::ZERO);
        assert_eq!(
            audio_wake_after(2400 + 240, 2400, 48_000),
            Duration::from_millis(5)
        );
        assert_eq!(audio_wake_after(2400, 2400, 48_000), AUDIO_WAKE_MIN);
        assert_eq!(audio_wake_after(2400 * 3, 2400, 48_000), AUDIO_WAKE_MAX);
    }
}
