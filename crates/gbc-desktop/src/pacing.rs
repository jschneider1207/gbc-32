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
/// The floor is short because a frame runs at most this late: a 1 ms
/// floor made frames land up to 1 ms off their 16.74 ms slot.
const AUDIO_WAKE_MIN: Duration = Duration::from_micros(250);
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

/// Audio pacing: the buffered frames as if the device played them
/// continuously. Devices take a chunk per callback (WASAPI: 10 ms), so
/// the ring's fill falls in steps, and pacing on it alone runs frames only
/// on callback boundaries: 10 and 20 ms apart instead of every 16.7 ms
/// (measured on Windows, ADR 0024 amendment). The chunk the last callback
/// took, starting to play at `start` on the smoothed device clock
/// ([`callback_clock`]), counts as still buffered until it has had time
/// to play at `rate` by `now` (same epoch). Past that the device keeps
/// playing, so the estimate keeps falling (a late callback would
/// otherwise hold frames back), for at most another chunk. A callback
/// early against the smoothed clock (`now < start`) still counts the part
/// of the previous chunk not yet played, so the estimate doesn't drop.
pub fn smoothed_fill(
    ring: usize,
    chunk: usize,
    start: Duration,
    now: Duration,
    rate: u32,
) -> usize {
    let elapsed = now.as_nanos() as i128 - start.as_nanos() as i128;
    let played = elapsed * i128::from(rate) / 1_000_000_000;
    let chunk = chunk as i128;
    let played = played.clamp(-chunk, 2 * chunk);
    (ring as i128 + chunk - played).max(0) as usize
}

/// Callbacks drift from the device clock by a millisecond or so (WASAPI,
/// measured 9-11 ms apart for 10 ms chunks). When the chunk of a callback
/// at `at` starts playing on a smoothed clock: the previous start plus
/// its length, nudged 1/8 of the way toward `at`. A callback more than a
/// chunk off (the first one, a stall) restarts the clock at `at`.
pub fn callback_clock(prev: Option<(Duration, usize)>, at: Duration, rate: u32) -> Duration {
    let Some((start, frames)) = prev else {
        return at;
    };
    let len = Duration::from_nanos(frames as u64 * 1_000_000_000 / u64::from(rate.max(1)));
    let expected = start + len;
    let err = at.as_nanos() as i128 - expected.as_nanos() as i128;
    if err.unsigned_abs() > len.as_nanos() {
        return at;
    }
    let ns = expected.as_nanos() as i128 + err / 8;
    Duration::from_nanos(ns.max(0) as u64)
}

/// Bits of [`pack_callback`] that hold the frame count.
const CHUNK_BITS: u32 = 20;

/// A callback's start (since some epoch; µs, wraps after 2^44 µs = 203
/// days) and the frames it took (saturating at 2^20 - 1), in one atomic
/// word.
pub fn pack_callback(at: Duration, frames: usize) -> u64 {
    let us = (at.as_micros() as u64) & ((1 << (64 - CHUNK_BITS)) - 1);
    let frames = (frames as u64).min((1 << CHUNK_BITS) - 1);
    us << CHUNK_BITS | frames
}

/// The inverse of [`pack_callback`]: (µs, frames).
pub fn unpack_callback(packed: u64) -> (u64, usize) {
    (
        packed >> CHUNK_BITS,
        (packed & ((1 << CHUNK_BITS) - 1)) as usize,
    )
}

/// When to retry opening the audio device after it was lost (ADR 0024
/// amendment). Pure: the caller passes `now`. First retry `BASE` after the
/// loss, each failed retry doubles the wait up to `MAX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    next: Instant,
    wait: Duration,
}

impl RetryPolicy {
    pub const BASE: Duration = Duration::from_secs(2);
    pub const MAX: Duration = Duration::from_secs(16);

    /// The device was lost (or never opened) at `now`.
    pub fn new(now: Instant) -> Self {
        RetryPolicy {
            next: now + Self::BASE,
            wait: Self::BASE,
        }
    }

    /// Whether an attempt is due. Cheap: one comparison per wake-up.
    pub fn due(&self, now: Instant) -> bool {
        now >= self.next
    }

    /// An attempt at `now` failed: back off.
    pub fn failed(&mut self, now: Instant) {
        self.wait = (self.wait * 2).min(Self::MAX);
        self.next = now + self.wait;
    }
}

/// How the window loop paces frames: by the audio buffer when a device is
/// open (ADR 0024), else by the wall clock, silently. Each wake-up calls
/// [`Pacing::begin`], runs frames while [`Pacing::should_run`] (feeding
/// each one's audio to [`Pacing::push_audio`]), then sleeps until
/// [`Pacing::next_wake`].
pub enum Pacing {
    Audio(Box<AudioOut>),
    /// `retry` is `Some` when audio was wanted and should be reopened.
    Clock {
        pacer: Pacer,
        due: u32,
        retry: Option<RetryPolicy>,
    },
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
                Self::clock_retrying(now)
            }
        }
    }

    /// Silent and permanent (no retries).
    pub fn clock(now: Instant) -> Self {
        Pacing::Clock {
            pacer: Pacer::new(now),
            due: 0,
            retry: None,
        }
    }

    /// Silent until the default device opens again.
    fn clock_retrying(now: Instant) -> Self {
        Pacing::Clock {
            pacer: Pacer::new(now),
            due: 0,
            retry: Some(RetryPolicy::new(now)),
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
            *self = Self::clock_retrying(now);
        }
        if let Pacing::Clock {
            retry: Some(policy),
            ..
        } = self
            && policy.due(now)
        {
            // A fresh AudioOut: empty ring, new resampler and DRC state,
            // so nothing stale is played.
            match AudioOut::open() {
                Ok(a) => {
                    eprintln!("audio: output device back; resuming audio pacing");
                    *self = Pacing::Audio(Box::new(a));
                }
                Err(_) => policy.failed(now),
            }
        }
        if let Pacing::Clock { pacer, due, .. } = self {
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

    /// The fill audio pacing decides on, in ms (`None` under the clock).
    pub fn fill_ms(&self) -> Option<f64> {
        match self {
            Pacing::Audio(a) => Some(a.fill() as f64 * 1e3 / f64::from(a.rate().max(1))),
            Pacing::Clock { .. } => None,
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
    fn retry_waits_the_base_interval_before_the_first_attempt() {
        let t0 = Instant::now();
        let r = RetryPolicy::new(t0);
        assert!(!r.due(t0));
        assert!(!r.due(t0 + RetryPolicy::BASE - Duration::from_millis(1)));
        assert!(r.due(t0 + RetryPolicy::BASE));
    }

    #[test]
    fn retry_backs_off_and_caps() {
        let mut now = Instant::now();
        let mut r = RetryPolicy::new(now);
        let mut waits = Vec::new();
        for _ in 0..6 {
            now += RetryPolicy::MAX * 2;
            assert!(r.due(now));
            r.failed(now);
            assert!(!r.due(now), "never immediately after a failure");
            let mut w = Duration::ZERO;
            while !r.due(now + w) {
                w += Duration::from_millis(500);
            }
            waits.push(w);
        }
        let s = Duration::from_secs;
        assert_eq!(waits, [s(4), s(8), s(16), s(16), s(16), s(16)]);
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
    fn smoothed_fill_drains_the_last_chunk_at_the_device_rate() {
        let ms = Duration::from_millis;
        let t = ms(100);
        let f = |ring, chunk, now| smoothed_fill(ring, chunk, t, now, 48_000);
        // 480 frames that started playing 0, 5 and 10 ms ago at 48 kHz.
        assert_eq!(f(2000, 480, t), 2480);
        assert_eq!(f(2000, 480, t + ms(5)), 2240);
        assert_eq!(f(2000, 480, t + ms(10)), 2000);
        // A late callback: the device kept playing, for up to a chunk.
        assert_eq!(f(2000, 480, t + ms(12)), 1904);
        assert_eq!(f(2000, 480, t + ms(30)), 1520);
        assert_eq!(f(100, 480, t + ms(30)), 0);
        // An early one: the previous chunk is still playing.
        assert_eq!(f(2000, 480, t - ms(1)), 2528);
        assert_eq!(f(2000, 480, t - ms(30)), 2960);
        assert_eq!(f(2000, 0, t + ms(5)), 2000);
    }

    #[test]
    fn callback_clock_tracks_the_period_and_damps_jitter() {
        let us = Duration::from_micros;
        assert_eq!(callback_clock(None, us(5_000), 48_000), us(5_000));
        let prev = Some((us(5_000), 480));
        assert_eq!(callback_clock(prev, us(15_000), 48_000), us(15_000));
        // 800 µs late: an eighth of it (100 µs) gets through.
        assert_eq!(callback_clock(prev, us(15_800), 48_000), us(15_100));
        assert_eq!(callback_clock(prev, us(14_200), 48_000), us(14_900));
        // More than a chunk off: restart.
        assert_eq!(callback_clock(prev, us(40_000), 48_000), us(40_000));
        assert_eq!(callback_clock(prev, us(4_000), 48_000), us(4_000));
    }

    #[test]
    fn callback_packing_round_trips() {
        let at = Duration::from_micros(123_456_789);
        assert_eq!(unpack_callback(pack_callback(at, 480)), (123_456_789, 480));
        assert_eq!(unpack_callback(pack_callback(at, 0)), (123_456_789, 0));
        assert_eq!(
            unpack_callback(pack_callback(at, usize::MAX)),
            (123_456_789, (1 << CHUNK_BITS) - 1),
            "frames saturate, the time is kept"
        );
        assert_eq!(unpack_callback(0), (0, 0), "no callback yet: no chunk");
    }

    /// A device taking 480 frames every 10 ms at 48 kHz and the window
    /// loop's audio pacing (wake-ups exactly on time): the intervals
    /// between emulated frames, after the ring has filled.
    fn simulate(smooth: bool, jitter_us: u64) -> Vec<f64> {
        const RATE: u32 = 48_000;
        const CHUNK: usize = 480;
        const CB_US: u64 = 10_000;
        let mut lcg = 1u64;
        let mut clock = None;
        let frame_out = f64::from(RATE) * FRAME_PERIOD.as_secs_f64();
        let target = 2400;
        let (mut ring, mut produced) = (0usize, 0.0f64);
        // Each callback runs up to `jitter_us` after its 10 ms slot.
        let mut late = || {
            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (lcg >> 33) % (jitter_us + 1)
        };
        let mut slot = CB_US / 2;
        let mut next_cb = slot + late();
        let (mut last_cb, mut wake) = (None::<u64>, 0u64);
        let mut frames = Vec::new();
        while wake < 3_000_000 {
            if next_cb <= wake {
                assert!(
                    ring >= CHUNK || next_cb < 500_000,
                    "underrun at {next_cb} µs"
                );
                ring -= ring.min(CHUNK);
                let start = callback_clock(clock, Duration::from_micros(next_cb), RATE);
                clock = Some((start, CHUNK));
                last_cb = Some(start.as_micros() as u64);
                slot += CB_US;
                next_cb = slot + late();
                continue;
            }
            let now = wake;
            let fill = |ring: usize| match (smooth, last_cb) {
                (true, Some(at)) => smoothed_fill(
                    ring,
                    CHUNK,
                    Duration::from_micros(at),
                    Duration::from_micros(now),
                    RATE,
                ),
                _ => ring,
            };
            let mut ran = 0;
            while audio_should_run(fill(ring), target, ran) {
                let before = produced as usize;
                produced += frame_out;
                ring += produced as usize - before;
                frames.push(now);
                ran += 1;
            }
            wake = now + (audio_wake_after(fill(ring), target, RATE).as_micros() as u64);
        }
        frames
            .windows(2)
            .filter(|w| w[0] > 1_000_000)
            .map(|w| (w[1] - w[0]) as f64 / 1000.0)
            .collect()
    }

    #[test]
    fn audio_pacing_runs_frames_evenly_despite_chunky_callbacks() {
        let frame_ms = FRAME_PERIOD.as_secs_f64() * 1000.0;
        // Callbacks on time, or up to 2 ms late (as measured on Windows).
        for jitter in [0, 2_000] {
            let gaps = simulate(true, jitter);
            assert!(gaps.len() > 100);
            for g in &gaps {
                assert!(
                    (g - frame_ms).abs() < 0.5,
                    "jitter {jitter} µs: frame gap {g} ms: {gaps:?}"
                );
            }
        }
        // Without smoothing frames run on callback boundaries (the bug).
        let raw = simulate(false, 0);
        assert!(raw.iter().any(|g| (g - frame_ms).abs() > 5.0), "{raw:?}");
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
