//! Live playback of the board's audio (`gbc-pc play --audio`, ADR 0041
//! "Amendment: live playback"): everything but the Win32 device, which is
//! [`crate::waveout`]. Host-tested in `tests/live.rs`.
//!
//! The play loop (one thread, a board frame every <= 10 ms) hands each
//! `Audio` message to [`Live::on_audio`]; the playback thread calls
//! [`Live::fill`] once per device buffer. Both go through one
//! `Mutex<Live>`, held for microseconds, so the receive loop never waits
//! on the sound card.
//!
//! - **[`Feed`]** turns messages into frames in the [`Ring`]: the real
//!   frames, then the last frame `held` more times (the DAC's underrun
//!   filler, so the stream runs at the DAC's real-time rate), and for a
//!   forward jump in `first` (a gap: the board dropped halves) the last
//!   frame for the missing count, up to [`MAX_GAP_FILL`]. A backward jump
//!   (board reset) restarts the count, no fill.
//! - **[`Playback`]** pulls frames through a [`Resampler`] (4-point cubic
//!   Hermite, any ratio) at `IN_RATE / out_rate * (1 + correction)`. The
//!   correction comes from a [`Controller`]: a PI controller on the ring
//!   fill, low-pass filtered (the fill is a sawtooth: two messages arrive
//!   per board video frame, plus USB jitter). Adaptive resampling, not
//!   dropping or repeating frames: a 0.3% ratio change is inaudible, a
//!   dropped frame is a click.
//! - **States:** `Prefill` until the ring holds [`Tuning::target_ms`]
//!   plus [`Tuning::device_ms`] (the device is started only then, and its
//!   first buffers take the device part), `Playing`, and after a PC-side
//!   underrun (the ring ran dry) `Rebuffer`: the last output frame is held
//!   (no click) until the ring is back at the target. On entering
//!   `Playing` the ring is trimmed to the target (the board's start-up
//!   burst would otherwise be latency); a fill above
//!   [`Tuning::skip_ms`] is trimmed the same way (counted as a skip).

use gbc_link::audio::{FRAME_BYTES, codes};

use crate::audio::to_i16;

/// The board's DAC rate (`docs/reference/audio-dac.md`).
pub const IN_RATE: f64 = 32_768.675;

/// The most frames one gap is filled with (125 ms): more is latency.
pub const MAX_GAP_FILL: u32 = 4096;

/// A stereo frame, left then right.
pub type Frame = [i16; 2];

/// A fixed-capacity FIFO of frames.
#[derive(Clone, Debug)]
pub struct Ring {
    buf: Box<[Frame]>,
    head: usize,
    len: usize,
}

impl Ring {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            buf: vec![[0; 2]; capacity].into_boxed_slice(),
            head: 0,
            len: 0,
        }
    }

    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// False (and nothing stored) when full.
    pub fn push(&mut self, f: Frame) -> bool {
        if self.len == self.buf.len() {
            return false;
        }
        let i = (self.head + self.len) % self.buf.len();
        self.buf[i] = f;
        self.len += 1;
        true
    }

    pub fn pop(&mut self) -> Option<Frame> {
        if self.len == 0 {
            return None;
        }
        let f = self.buf[self.head];
        self.head = (self.head + 1) % self.buf.len();
        self.len -= 1;
        Some(f)
    }

    /// Drops the `n` oldest frames (fewer if it holds fewer).
    pub fn drop_oldest(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % self.buf.len();
        self.len -= n;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedStats {
    pub messages: u64,
    /// Real frames received.
    pub frames: u64,
    /// Jumps in `first` (forward or backward).
    pub gaps: u32,
    /// Frames missing in forward jumps.
    pub missing: u64,
    /// Frames inserted for them.
    pub filled: u64,
    /// The board's underrun filler (`held`), inserted.
    pub held: u64,
    /// Frames lost because the ring was full.
    pub overflow: u64,
}

/// `Audio` messages into the ring (module docs).
#[derive(Clone, Debug, Default)]
pub struct Feed {
    next: Option<u32>,
    last: Frame,
    pub stats: FeedStats,
}

impl Feed {
    pub fn on_audio(&mut self, ring: &mut Ring, first: u32, held: u16, data: &[u8]) {
        let put = |ring: &mut Ring, f: Frame, s: &mut FeedStats| {
            if !ring.push(f) {
                s.overflow += 1;
            }
        };
        let s = &mut self.stats;
        s.messages += 1;
        if let Some(next) = self.next
            && first != next
        {
            s.gaps += 1;
            let ahead = first.wrapping_sub(next);
            if ahead < 1 << 31 {
                s.missing += u64::from(ahead);
                let fill = ahead.min(MAX_GAP_FILL);
                s.filled += u64::from(fill);
                for _ in 0..fill {
                    put(ring, self.last, s);
                }
            }
        }
        let n = (data.len() / FRAME_BYTES) as u32;
        for f in data.as_chunks::<FRAME_BYTES>().0 {
            let (l, r) = codes(*f);
            self.last = [to_i16(l), to_i16(r)];
            put(ring, self.last, s);
        }
        s.frames += u64::from(n);
        s.held += u64::from(held);
        for _ in 0..held {
            put(ring, self.last, s);
        }
        self.next = Some(first.wrapping_add(n));
    }
}

/// A 4-point cubic Hermite (Catmull-Rom) resampler for stereo frames.
/// `step` is input frames per output frame; at exactly 1.0 the output is
/// the input, three frames late (the history starts silent).
#[derive(Clone, Debug)]
pub struct Resampler {
    h: [[f64; 2]; 4],
    /// Position of the next output between `h[1]` (0) and `h[2]` (1).
    frac: f64,
}

impl Default for Resampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Resampler {
    pub fn new() -> Self {
        Self {
            h: [[0.0; 2]; 4],
            frac: 0.0,
        }
    }

    /// The next output frame, or `None` if `src` ran dry (call again once
    /// it has frames: nothing is lost).
    pub fn next(&mut self, step: f64, src: &mut dyn FnMut() -> Option<Frame>) -> Option<Frame> {
        while self.frac >= 1.0 {
            let f = src()?;
            self.h = [
                self.h[1],
                self.h[2],
                self.h[3],
                [f64::from(f[0]), f64::from(f[1])],
            ];
            self.frac -= 1.0;
        }
        let t = self.frac;
        let mut out = [0i16; 2];
        for (c, o) in out.iter_mut().enumerate() {
            let [p0, p1, p2, p3] = [self.h[0][c], self.h[1][c], self.h[2][c], self.h[3][c]];
            let y = p1
                + 0.5
                    * t
                    * (p2 - p0
                        + t * (2.0 * p0 - 5.0 * p1 + 4.0 * p2 - p3
                            + t * (3.0 * (p1 - p2) + p3 - p0)));
            *o = y.round().clamp(-32768.0, 32767.0) as i16;
        }
        self.frac += step;
        Some(out)
    }
}

/// The playback's knobs (defaults: the values the board run used).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Ring fill aimed at (and pre-filled to), in ms of input.
    pub target_ms: f64,
    /// The device's queue (`waveout`: 4 x 10 ms), pre-filled on top.
    pub device_ms: f64,
    /// A fill above this is trimmed back to the target.
    pub skip_ms: f64,
    /// Correction per unit of relative fill error.
    pub kp: f64,
    /// Integral time, s: the I term removes the fill offset a steady rate
    /// mismatch would leave with P alone.
    pub ti_s: f64,
    /// Time constant of the fill's low-pass filter, s.
    pub tau_s: f64,
    /// Largest correction either way.
    pub max_correction: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            target_ms: 50.0,
            device_ms: 40.0,
            skip_ms: 160.0,
            kp: 0.05,
            ti_s: 10.0,
            tau_s: 0.5,
            max_correction: 0.01,
        }
    }
}

/// The PI controller on the filtered ring fill (module docs). With the
/// defaults the P loop's time constant is `target / kp` = 1 s (damping
/// about 0.7 with the 0.5 s filter): a rate mismatch `d` (0.33%: the
/// board's clock vs real time, plus the sound card's own error) is taken
/// up within seconds at a fill error of `d / kp` of the target, which
/// the slow I term (10 s) then brings back to zero.
#[derive(Clone, Debug)]
pub struct Controller {
    target: f64,
    tau: f64,
    kp: f64,
    ki: f64,
    max: f64,
    ema: f64,
    integral: f64,
    correction: f64,
}

impl Controller {
    /// `target` in frames.
    pub fn new(target: f64, t: &Tuning) -> Self {
        Self {
            target,
            tau: t.tau_s,
            kp: t.kp,
            ki: t.kp / t.ti_s,
            max: t.max_correction,
            ema: target,
            integral: 0.0,
            correction: 0.0,
        }
    }

    /// One measurement `dt` seconds after the last; returns the correction.
    pub fn update(&mut self, fill: usize, dt: f64) -> f64 {
        self.ema += (fill as f64 - self.ema) * (dt / (self.tau + dt));
        let err = (self.ema - self.target) / self.target;
        // Anti-windup: the I term alone never exceeds the clamp.
        let lim = self.max / self.ki;
        self.integral = (self.integral + err * dt).clamp(-lim, lim);
        self.correction = (self.kp * err + self.ki * self.integral).clamp(-self.max, self.max);
        self.correction
    }

    pub fn ema(&self) -> f64 {
        self.ema
    }

    pub fn correction(&self) -> f64 {
        self.correction
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Prefill,
    Playing,
    Rebuffer,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlayStats {
    pub state: State,
    /// Device buffers filled while playing or rebuffering.
    pub buffers: u64,
    /// The ring fill at the last buffer, frames.
    pub fill: usize,
    /// Lowest and highest fill since [`Playback::take_window`].
    pub fill_min: usize,
    pub fill_max: usize,
    /// The controller's filtered fill, frames.
    pub fill_ema: f64,
    /// The ratio correction in use (0.001 = 0.1% faster).
    pub correction: f64,
    /// PC-side underruns: the ring ran dry while playing.
    pub underruns: u32,
    /// Output frames held during them (until the ring was refilled).
    pub underrun_frames: u64,
    /// Trims of a ring above `skip_ms`, and the frames dropped by them
    /// (the start trim is counted in `start_trimmed`).
    pub skips: u32,
    pub skipped_frames: u64,
    pub start_trimmed: u64,
}

/// One device buffer at a time, from the ring (module docs).
#[derive(Clone, Debug)]
pub struct Playback {
    out_rate: f64,
    base_step: f64,
    target: usize,
    prefill: usize,
    skip: usize,
    res: Resampler,
    ctl: Controller,
    last: Frame,
    stats: PlayStats,
}

impl Playback {
    pub fn new(out_rate: u32, t: &Tuning) -> Self {
        let target = (IN_RATE * t.target_ms / 1000.0).round() as usize;
        Self {
            out_rate: f64::from(out_rate),
            base_step: IN_RATE / f64::from(out_rate),
            target,
            prefill: target + (IN_RATE * t.device_ms / 1000.0).round() as usize,
            skip: (IN_RATE * t.skip_ms / 1000.0).round() as usize,
            res: Resampler::new(),
            ctl: Controller::new(target as f64, t),
            last: [0; 2],
            stats: PlayStats {
                state: State::Prefill,
                buffers: 0,
                fill: 0,
                fill_min: usize::MAX,
                fill_max: 0,
                fill_ema: target as f64,
                correction: 0.0,
                underruns: 0,
                underrun_frames: 0,
                skips: 0,
                skipped_frames: 0,
                start_trimmed: 0,
            },
        }
    }

    /// The target fill, frames.
    pub fn target(&self) -> usize {
        self.target
    }

    /// The fill the device starts at (target plus the device queue).
    pub fn prefill(&self) -> usize {
        self.prefill
    }

    /// Pre-filled: the device may start.
    pub fn ready(&self, ring: &Ring) -> bool {
        self.stats.state != State::Prefill || ring.len() >= self.prefill
    }

    pub fn stats(&self) -> PlayStats {
        self.stats
    }

    /// The stats, then a fresh min/max window.
    pub fn take_window(&mut self) -> PlayStats {
        let s = self.stats;
        self.stats.fill_min = usize::MAX;
        self.stats.fill_max = 0;
        s
    }

    /// Fills one device buffer (interleaved stereo).
    pub fn fill(&mut self, ring: &mut Ring, out: &mut [i16]) {
        let frames = out.len() / 2;
        let s = &mut self.stats;
        if s.state != State::Prefill {
            s.buffers += 1;
        }
        let fill = ring.len();
        s.fill = fill;
        s.fill_min = s.fill_min.min(fill);
        s.fill_max = s.fill_max.max(fill);
        let mut start = 0;
        if s.state != State::Playing {
            let resume = if s.state == State::Prefill {
                self.prefill
            } else {
                self.target
            };
            if fill < resume {
                if s.state == State::Rebuffer {
                    s.underrun_frames += frames as u64;
                }
                for o in out.as_chunks_mut::<2>().0 {
                    *o = self.last;
                }
                return;
            }
            s.start_trimmed += (fill - resume) as u64;
            ring.drop_oldest(fill - resume);
            s.state = State::Playing;
        } else if fill > self.skip {
            s.skips += 1;
            s.skipped_frames += (fill - self.target) as u64;
            ring.drop_oldest(fill - self.target);
        }
        let corr = self.ctl.update(ring.len(), frames as f64 / self.out_rate);
        s.correction = corr;
        s.fill_ema = self.ctl.ema();
        let step = self.base_step * (1.0 + corr);
        let mut pop = || ring.pop();
        for o in out.as_chunks_mut::<2>().0 {
            match self.res.next(step, &mut pop) {
                Some(f) => {
                    self.last = f;
                    *o = f;
                    start += 1;
                }
                None => break,
            }
        }
        if start < frames {
            s.underruns += 1;
            s.underrun_frames += (frames - start) as u64;
            s.state = State::Rebuffer;
            for o in &mut out.as_chunks_mut::<2>().0[start..] {
                *o = self.last;
            }
        }
    }
}

/// The state both threads share (behind one mutex).
#[derive(Clone, Debug)]
pub struct Live {
    pub ring: Ring,
    pub feed: Feed,
    pub play: Playback,
}

impl Live {
    /// A ring of 500 ms.
    pub fn new(out_rate: u32, t: &Tuning) -> Self {
        Self {
            ring: Ring::new((IN_RATE / 2.0) as usize),
            feed: Feed::default(),
            play: Playback::new(out_rate, t),
        }
    }

    pub fn on_audio(&mut self, first: u32, held: u16, data: &[u8]) {
        self.feed.on_audio(&mut self.ring, first, held, data);
    }

    pub fn ready(&self) -> bool {
        self.play.ready(&self.ring)
    }

    pub fn fill(&mut self, out: &mut [i16]) {
        self.play.fill(&mut self.ring, out);
    }

    /// Frames as milliseconds of input.
    pub fn ms(frames: f64) -> f64 {
        frames * 1000.0 / IN_RATE
    }
}
