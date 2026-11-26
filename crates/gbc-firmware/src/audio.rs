//! Audio out, board-independent (Stage 2 M4): the DC blocker ADR 0035
//! leaves to the platform, the `i16` to 12-bit DAC conversion, the sample
//! ring between the main loop (producer) and the DMA refill ISR
//! (consumer), and the hash of exactly what the DAC is fed.
//!
//! The data path is `StereoSample` (core, 32,768 Hz) -> [`DcBlocker`] per
//! channel -> [`dac_code`] -> [`pack`] (one `u32` per frame, the layout of
//! the DAC's `DAC_DHR12RD` register) -> [`AudioRing`] -> ISR -> DMA buffer.
//!
//! # Packing
//!
//! RM0433 DAC_DHR12RD: DACC1DHR in bits 11:0, DACC2DHR in bits 27:16, the
//! rest reserved (0). **Left is DAC channel 1 (OUT1, PA4), right is
//! channel 2 (OUT2, PA5).**
//!
//! # Scale
//!
//! One DAC step is 16 `i16` units: code = 2048 + y/16, so core full scale
//! (+-32,768) is the DAC's full swing and 0 is mid-scale 2048. The blocker's
//! output can reach +-65,535 (ADR 0035, "Headroom"; only after the mix sits
//! at one extreme and swings to the other), which saturates at the rails
//! (0 and 4095) rather than being halved: ordinary content (a full-range
//! square wave is +-30,720) has the full resolution.

use crate::fnv::Fnv1a;
use core::sync::atomic::{AtomicU32, Ordering};
use gbc_core::StereoSample;

/// The blocker's pole is `a = 1 - 2^-DC_SHIFT`.
pub const DC_SHIFT: u32 = 9;
/// Fractional bits of the blocker's state.
const FRAC: u32 = 8;

/// Cutoff of the DC blocker in millihertz: -3 dB at about
/// `fs * (1 - a) / (2 pi)` = 32,768 / 512 / 6.283 = 10.2 Hz (the exact
/// bilinear value is within 0.1% of that at this ratio). Time constant 512
/// samples = 15.6 ms.
pub const DC_CUTOFF_MILLIHZ: u32 = 10_186;

/// Mid-scale DAC code (zero signal).
pub const MID: u16 = 2048;
/// The largest DAC code.
pub const MAX_CODE: u16 = 4095;
/// A frame of silence: both channels at mid-scale.
pub const MID_FRAME: u32 = pack(MID, MID);

/// An integer one-pole high-pass, `y[n] = x[n] - x[n-1] + a y[n-1]`,
/// `a = 1 - 2^-9`, for one channel. Deterministic: integers only.
///
/// State: `ys`, `y` in Q8 (`FRAC` = 8 fractional bits), and the previous
/// input. Per step `ys += (x - x_prev) << 8; ys -= decay(ys)`, where
/// `decay` is `ys / 512` **rounded away from zero**. That is the error
/// feedback: the pole's multiplication is never truncated toward the
/// state, so any non-zero state shrinks by at least 1 each step and a
/// constant input reaches `y = 0` exactly (no limit cycle; plain `ys >> 9`
/// would stick at up to +-511 in Q8). The decay is monotone: `|ys|` falls
/// strictly while the input is constant.
///
/// Headroom: the exact filter's impulse response is `d[n] - (1-a) a^(n-1)`,
/// L1 norm 2, so `|y| <= 2 * 32,768 = 65,536` for any `i16` input (this
/// is the ADR 0035 bound), square waves included: `|ys| <= 2^24`, far
/// inside `i32`. Rounding adds at most 1 per step to a state that the
/// decay pulls back, so the bound holds up to a few units of Q8. The state
/// is clamped to +-2^26 anyway and the arithmetic saturates, so no input
/// can wrap or panic.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DcBlocker {
    prev: i32,
    ys: i32,
}

const STATE_LIMIT: i32 = 1 << 26;

impl DcBlocker {
    /// A settled blocker (output 0 for a zero input).
    pub const fn new() -> Self {
        Self { prev: 0, ys: 0 }
    }

    /// One sample in, the high-passed sample out, in `i16` units but
    /// `i32` (range about +-65,535).
    #[inline]
    pub fn step(&mut self, x: i16) -> i32 {
        let x = i32::from(x);
        let mask = (1i32 << DC_SHIFT) - 1;
        let ys = self
            .ys
            .saturating_add((x - self.prev) << FRAC)
            .clamp(-STATE_LIMIT, STATE_LIMIT);
        // Round the decay away from zero (see the type docs).
        let decay = if ys >= 0 {
            (ys + mask) >> DC_SHIFT
        } else {
            (ys - mask) >> DC_SHIFT
        };
        self.ys = ys - decay;
        self.prev = x;
        // The output is the sum before the pole's decay (`x - x_prev +
        // a y_prev`; the decay stored above is the `a y` of the next step).
        // Q8 to integer, round to nearest (half up).
        (ys + (1 << (FRAC - 1))) >> FRAC
    }
}

/// A blocker output (`i16` units) to a 12-bit DAC code: `2048 + y/16`,
/// rounded to nearest with ties up (`(y + 8) >> 4`, arithmetic shift),
/// saturated to 0..=4095. -32,768 gives 0, 0 gives 2048, 32,767 gives 4095.
#[inline]
pub fn dac_code(y: i32) -> u16 {
    let c = (y.saturating_add(8) >> 4) + i32::from(MID);
    c.clamp(0, i32::from(MAX_CODE)) as u16
}

/// Two codes as one `DAC_DHR12RD` word: left (channel 1) in bits 11:0,
/// right (channel 2) in bits 27:16. Codes above 4095 are masked.
#[inline]
pub const fn pack(left: u16, right: u16) -> u32 {
    (left as u32 & 0xfff) | ((right as u32 & 0xfff) << 16)
}

/// The left code of a packed frame.
#[inline]
pub const fn left_code(frame: u32) -> u16 {
    (frame & 0xfff) as u16
}

/// The right code of a packed frame.
#[inline]
pub const fn right_code(frame: u32) -> u16 {
    ((frame >> 16) & 0xfff) as u16
}

/// Both channels' blockers: core samples to packed DAC frames. Owned by
/// the producer (the main loop).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Conditioner {
    left: DcBlocker,
    right: DcBlocker,
}

impl Conditioner {
    pub const fn new() -> Self {
        Self {
            left: DcBlocker::new(),
            right: DcBlocker::new(),
        }
    }

    /// One core sample to the packed word the DAC is fed.
    #[inline]
    pub fn frame(&mut self, s: StereoSample) -> u32 {
        let l = dac_code(self.left.step(s.left));
        let r = dac_code(self.right.step(s.right));
        pack(l, r)
    }
}

/// The hash of the DAC feed: FNV-1a (`fnv.rs`) over the packed `u32`
/// frames in order, each as its four little-endian bytes. The board folds
/// the real frames the ISR pops (the first `n` that [`AudioRing::pop_into`]
/// returns, never the underrun filler); the host folds the same stream with
/// [`dac_feed_hash`]. They agree when every produced frame was fed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedHash(Fnv1a);

impl FeedHash {
    pub const fn new() -> Self {
        Self(Fnv1a::new())
    }

    pub fn fold(&mut self, frames: &[u32]) {
        for &f in frames {
            self.0.u32(f);
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// The DAC-feed hash of a whole run's core samples (what the host
/// `headless` prints to compare with the board): a fresh [`Conditioner`],
/// every sample in order. Call it on the concatenation of all
/// `run_frame` outputs of the run.
pub fn dac_feed_hash(samples: &[StereoSample]) -> u64 {
    let mut c = Conditioner::new();
    let mut h = FeedHash::new();
    for &s in samples {
        h.fold(&[c.frame(s)]);
    }
    h.get()
}

/// The consumer side of the DAC feed: pops from an [`AudioRing`] and folds
/// the real frames (`out[..n]`, never the underrun filler) into a
/// [`FeedHash`], so the board can show that what the DMA buffer got is what
/// the main loop pushed. A `static` shared by `&'static` like the ring; the
/// board's DMA ISR calls [`pop_into`](Self::pop_into) instead of the
/// ring's:
///
/// ```ignore
/// gbc_nucleo_h753::audio_dac::on_dma(|_half, out| {
///     TAP.pop_into(&RING, out);
/// });
/// ```
///
/// Exactly one context (the consumer) may call `pop_into`. The hash lives
/// in two `AtomicU32`s (thumbv7em has no 64-bit atomics), written only by
/// the consumer, then the frame count with `Release`; a reader that sees
/// [`frames`](Self::frames) equal to every frame pushed (`Acquire`) sees
/// the hash of all of them, and it stops changing (later pops are empty;
/// an empty pop writes nothing). [`crate::paced`] reads it that way after
/// draining the ring. Folding 256 frames costs about 1,024 FNV steps per
/// DMA half (128 a second).
pub struct FeedTap {
    lo: AtomicU32,
    hi: AtomicU32,
    /// Real frames popped (wraps at 2^32, 36 hours at 32,768 Hz).
    frames: AtomicU32,
}

impl FeedTap {
    /// Nothing popped yet: the hash of no frames.
    pub const fn new() -> Self {
        let h = FeedHash::new().get();
        Self {
            lo: AtomicU32::new(h as u32),
            hi: AtomicU32::new((h >> 32) as u32),
            frames: AtomicU32::new(0),
        }
    }

    /// Consumer: [`AudioRing::pop_into`], then folds `out[..n]`. Returns
    /// `n`.
    pub fn pop_into<const N: usize>(&self, ring: &AudioRing<N>, out: &mut [u32]) -> usize {
        let n = ring.pop_into(out);
        if let Some(real) = out.get(..n).filter(|r| !r.is_empty()) {
            let state = u64::from(self.lo.load(Ordering::Relaxed))
                | u64::from(self.hi.load(Ordering::Relaxed)) << 32;
            let mut h = FeedHash(Fnv1a::resume(state));
            h.fold(real);
            let state = h.get();
            self.lo.store(state as u32, Ordering::Relaxed);
            self.hi.store((state >> 32) as u32, Ordering::Relaxed);
            let f = self.frames.load(Ordering::Relaxed);
            self.frames
                .store(f.wrapping_add(n as u32), Ordering::Release);
        }
        n
    }

    /// Real frames popped so far.
    pub fn frames(&self) -> u32 {
        self.frames.load(Ordering::Acquire)
    }

    /// The hash of every real frame popped so far (stable once the
    /// producer has stopped and the ring is empty; type docs).
    pub fn hash(&self) -> u64 {
        u64::from(self.lo.load(Ordering::Relaxed))
            | u64::from(self.hi.load(Ordering::Relaxed)) << 32
    }
}

impl Default for FeedTap {
    fn default() -> Self {
        Self::new()
    }
}

/// Ring capacity in frames: 2048 = 62.5 ms at 32,768 Hz, 8 KiB. A video
/// frame yields at most 550 samples, so the pacing rule "run while
/// `free() >= 550`" keeps the ring between about 1,500 and 2,048 frames
/// (3.7 frames of slack for main-loop jitter) and the audio latency under
/// 50 ms. A larger ring would only add latency.
pub const RING_FRAMES: usize = 2048;
/// The DMA half-buffer the ISR refills: 256 frames (7.8 ms, 1 KiB per
/// half), so 128 half-transfer interrupts a second and 8 halves in the ring.
pub const DMA_HALF_FRAMES: usize = 256;

/// The ring the board uses.
pub type FeedRing = AudioRing<RING_FRAMES>;

/// Counters and watermarks of an [`AudioRing`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RingStats {
    /// `pop_into` calls that found fewer frames than asked.
    pub underrun_events: u32,
    /// Frames short, summed over those calls (filled with the last frame).
    pub underrun_frames: u32,
    /// `push` calls that had to drop frames.
    pub overrun_events: u32,
    /// Frames dropped (the newest of a push; the blocker did not see them).
    pub overrun_frames: u32,
    /// Lowest occupancy right after a pop (0 after an underrun) since
    /// the last [`AudioRing::reset_watermarks`]; `u32::MAX` if no pop yet.
    pub min_fill: u32,
    /// Highest occupancy right after a push since the last reset.
    pub max_fill: u32,
}

/// A single-producer single-consumer ring of packed DAC frames, in a
/// `static`, shared by `&'static` between the main loop (producer) and an
/// ISR (consumer). `N` must be a power of two (checked at compile time).
///
/// Exactly one context may call the producer methods ([`push_frames`],
/// [`push_slice`], [`free`] is fine from either) and exactly one the
/// consumer method ([`pop_into`]); the type does not enforce it.
///
/// # Ordering
///
/// `head` (frames ever pushed) is written only by the producer, `tail`
/// (frames ever popped) only by the consumer, both free-running `u32`s
/// (wrapping; `N <= 2^31` keeps `head - tail` exact). The producer writes
/// its slots with `Relaxed` stores and then publishes with a `Release`
/// store of `head`; the consumer's `Acquire` load of `head` makes those
/// slot writes visible before it reads them. Symmetrically the consumer
/// reads its slots, then `Release`-stores `tail`, and the producer's
/// `Acquire` load of `tail` guarantees the reads are finished before the
/// slots are overwritten. Each side reads its own index `Relaxed`. This
/// holds for two cores and, when the consumer is an interrupt on the
/// same core, because the ISR sees program order of the interrupted code
/// and the compiler is barred from reordering across the atomics. A push
/// interrupted by the ISR just looks like a push not yet published.
///
/// Counters and watermarks use `fetch_add`/`fetch_min`/`fetch_max` (ldrex/
/// strex on thumbv7em), so they stay correct from either context and
/// [`reset_watermarks`] may race with them (it can lose at most one update).
///
/// [`push_frames`]: Self::push_frames
/// [`push_slice`]: Self::push_slice
/// [`free`]: Self::free
/// [`pop_into`]: Self::pop_into
/// [`reset_watermarks`]: Self::reset_watermarks
pub struct AudioRing<const N: usize> {
    slots: [AtomicU32; N],
    head: AtomicU32,
    tail: AtomicU32,
    /// The last frame handed out (consumer only), repeated on underrun.
    last: AtomicU32,
    underrun_events: AtomicU32,
    underrun_frames: AtomicU32,
    overrun_events: AtomicU32,
    overrun_frames: AtomicU32,
    min_fill: AtomicU32,
    max_fill: AtomicU32,
}

impl<const N: usize> AudioRing<N> {
    const MASK: u32 = (N - 1) as u32;

    /// An empty ring.
    pub const fn new() -> Self {
        const { assert!(N.is_power_of_two() && N <= 1 << 31) };
        Self {
            slots: [const { AtomicU32::new(0) }; N],
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            last: AtomicU32::new(MID_FRAME),
            underrun_events: AtomicU32::new(0),
            underrun_frames: AtomicU32::new(0),
            overrun_events: AtomicU32::new(0),
            overrun_frames: AtomicU32::new(0),
            min_fill: AtomicU32::new(u32::MAX),
            max_fill: AtomicU32::new(0),
        }
    }

    /// Frames currently queued (a snapshot; exact for the caller's own side).
    pub fn fill(&self) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let t = self.tail.load(Ordering::Acquire);
        h.wrapping_sub(t) as usize
    }

    /// Frames the producer can push now. Never less than the truth: the
    /// consumer only frees more.
    pub fn free(&self) -> usize {
        N - self.fill()
    }

    /// Producer: pushes `s` through `cond` (blocker and conversion).
    /// Pushes as many as fit and returns that count; the rest are dropped
    /// (an overrun, counted) without touching `cond`.
    pub fn push_slice(&self, cond: &mut Conditioner, s: &[StereoSample]) -> usize {
        self.push_with(s.len(), |i| s.get(i).map_or(MID_FRAME, |&x| cond.frame(x)))
    }

    /// Producer: pushes already converted frames (see [`push_slice`]).
    ///
    /// [`push_slice`]: Self::push_slice
    pub fn push_frames(&self, f: &[u32]) -> usize {
        self.push_with(f.len(), |i| f.get(i).copied().unwrap_or(MID_FRAME))
    }

    fn push_with(&self, want: usize, mut frame: impl FnMut(usize) -> u32) -> usize {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let fill = head.wrapping_sub(tail) as usize;
        let n = want.min(N - fill);
        for i in 0..n {
            let idx = head.wrapping_add(i as u32) & Self::MASK;
            if let Some(slot) = self.slots.get(idx as usize) {
                slot.store(frame(i), Ordering::Relaxed);
            }
        }
        let new_head = head.wrapping_add(n as u32);
        self.head.store(new_head, Ordering::Release);
        if n < want {
            self.overrun_events.fetch_add(1, Ordering::Relaxed);
            self.overrun_frames
                .fetch_add((want - n) as u32, Ordering::Relaxed);
        }
        self.max_fill
            .fetch_max((fill + n) as u32, Ordering::Relaxed);
        n
    }

    /// Consumer: fills `out` from the ring and returns the number of
    /// **real** frames (`out[..n]`). On a shortage the rest of `out` is
    /// filled with the last frame handed out (mid-scale if none yet): the
    /// DAC holds its level instead of stepping to mid-scale, the smallest
    /// click available. That counts one underrun event and the missing
    /// frames. Hash only `out[..n]` ([`FeedHash`]).
    pub fn pop_into(&self, out: &mut [u32]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail) as usize;
        let n = out.len().min(avail);
        let mut last = self.last.load(Ordering::Relaxed);
        for (i, o) in out.iter_mut().enumerate() {
            if i < n {
                let idx = tail.wrapping_add(i as u32) & Self::MASK;
                if let Some(slot) = self.slots.get(idx as usize) {
                    last = slot.load(Ordering::Relaxed);
                }
            }
            *o = last;
        }
        self.last.store(last, Ordering::Relaxed);
        self.tail
            .store(tail.wrapping_add(n as u32), Ordering::Release);
        if n < out.len() {
            self.underrun_events.fetch_add(1, Ordering::Relaxed);
            self.underrun_frames
                .fetch_add((out.len() - n) as u32, Ordering::Relaxed);
        }
        self.min_fill
            .fetch_min((avail - n) as u32, Ordering::Relaxed);
        n
    }

    /// A snapshot of the counters and watermarks.
    pub fn stats(&self) -> RingStats {
        RingStats {
            underrun_events: self.underrun_events.load(Ordering::Relaxed),
            underrun_frames: self.underrun_frames.load(Ordering::Relaxed),
            overrun_events: self.overrun_events.load(Ordering::Relaxed),
            overrun_frames: self.overrun_frames.load(Ordering::Relaxed),
            min_fill: self.min_fill.load(Ordering::Relaxed),
            max_fill: self.max_fill.load(Ordering::Relaxed),
        }
    }

    /// Restarts the fill watermarks (the counters keep counting).
    pub fn reset_watermarks(&self) {
        self.min_fill.store(u32::MAX, Ordering::Relaxed);
        self.max_fill.store(0, Ordering::Relaxed);
    }
}

impl<const N: usize> Default for AudioRing<N> {
    fn default() -> Self {
        Self::new()
    }
}

/// Halves an [`AudioTee`] holds by default: 4 = 31 ms of audio, 4 KiB
/// (what the player's AXI SRAM has room for). Enough when the consumer
/// runs every video frame (16.7 ms, 2.1 halves).
pub const TEE_HALVES: usize = 4;

/// One half taken from an [`AudioTee`]: its real frames are `out[..real]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TeeHalf {
    /// Index of the first real frame among every real frame offered since
    /// [`AudioTee::new`] (wrapping), on or off.
    pub first: u32,
    /// Real frames (popped from the ring).
    pub real: u16,
    /// Underrun filler frames the DAC played after them.
    pub held: u16,
}

/// What an [`AudioTee`] counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TeeStats {
    /// Real frames offered since boot (on or off).
    pub seen: u32,
    /// Halves queued.
    pub queued: u32,
    /// Halves dropped because the queue was full (the consumer was slow).
    pub dropped_halves: u32,
    /// Real frames in those halves.
    pub dropped_frames: u32,
}

/// A copy of the DAC feed for the PC (ADR 0041): the DMA ISR, right after
/// it refilled a half ([`FeedTap::pop_into`]), offers that half here; the
/// main loop takes whole halves and sends them as `Message::Audio`. A
/// single-producer (the ISR) single-consumer (the main loop) queue of `H`
/// halves (a power of two), with the [`AudioRing`] ordering rules: the
/// producer writes a slot, then `Release`-stores `head`; the consumer
/// reads it after an `Acquire` load of `head`, then `Release`-stores
/// `tail`.
///
/// Off (the default) an offer only counts its real frames (two atomic
/// accesses). On, it copies the real frames, unless the queue is full:
/// then the half is **dropped** and counted ([`TeeStats::dropped_halves`]),
/// so a slow consumer can never stall the DAC; the PC sees the gap in
/// [`TeeHalf::first`]. The filler of an underrun is not copied, only
/// counted in [`TeeHalf::held`].
///
/// Every field is an integer atomic, so a tee in memory that is not
/// zeroed at boot (a board's NOLOAD section) is sound; [`AudioTee::reset`]
/// then sets it up before the ISR runs.
pub struct AudioTee<const H: usize = TEE_HALVES> {
    /// 0 off, 1 on.
    on: AtomicU32,
    slots: [[AtomicU32; DMA_HALF_FRAMES]; H],
    /// Per slot: real frames in bits 15:0, held frames in 31:16.
    counts: [AtomicU32; H],
    firsts: [AtomicU32; H],
    head: AtomicU32,
    tail: AtomicU32,
    seen: AtomicU32,
    dropped_halves: AtomicU32,
    dropped_frames: AtomicU32,
    queued: AtomicU32,
}

impl<const H: usize> AudioTee<H> {
    /// Off and empty.
    pub const fn new() -> Self {
        const { assert!(H.is_power_of_two() && H <= 1 << 16) };
        Self {
            on: AtomicU32::new(0),
            slots: [const { [const { AtomicU32::new(0) }; DMA_HALF_FRAMES] }; H],
            counts: [const { AtomicU32::new(0) }; H],
            firsts: [const { AtomicU32::new(0) }; H],
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            seen: AtomicU32::new(0),
            dropped_halves: AtomicU32::new(0),
            dropped_frames: AtomicU32::new(0),
            queued: AtomicU32::new(0),
        }
    }

    /// Starts or stops copying (either context). Halves already queued
    /// stay until [`AudioTee::clear`].
    pub fn set_on(&self, on: bool) {
        self.on.store(u32::from(on), Ordering::Relaxed);
    }

    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed) != 0
    }

    /// Back to [`AudioTee::new`]'s state (off, empty, counters 0; the
    /// slots are left as they are). Call it before the producer runs.
    pub fn reset(&self) {
        for a in [
            &self.on,
            &self.head,
            &self.tail,
            &self.seen,
            &self.dropped_halves,
            &self.dropped_frames,
            &self.queued,
        ] {
            a.store(0, Ordering::Relaxed);
        }
    }

    /// Producer: `half` is the DMA half just refilled, `real` the real
    /// frames at its start (the rest is underrun filler). At most
    /// [`DMA_HALF_FRAMES`] frames are kept.
    pub fn offer(&self, half: &[u32], real: usize) {
        let real = real.min(half.len()).min(DMA_HALF_FRAMES);
        let first = self.seen.load(Ordering::Relaxed);
        self.seen
            .store(first.wrapping_add(real as u32), Ordering::Relaxed);
        if !self.is_on() {
            return;
        }
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) as usize >= H {
            self.dropped_halves.fetch_add(1, Ordering::Relaxed);
            self.dropped_frames
                .fetch_add(real as u32, Ordering::Relaxed);
            return;
        }
        let i = head as usize & (H - 1);
        if let (Some(slot), Some(src)) = (self.slots.get(i), half.get(..real)) {
            for (d, &s) in slot.iter().zip(src) {
                d.store(s, Ordering::Relaxed);
            }
        }
        let held = (half.len() - real).min(0xFFFF) as u32;
        if let (Some(c), Some(f)) = (self.counts.get(i), self.firsts.get(i)) {
            c.store(real as u32 | held << 16, Ordering::Relaxed);
            f.store(first, Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        self.queued.fetch_add(1, Ordering::Relaxed);
    }

    /// Consumer: the oldest queued half into `out` (its real frames at
    /// `out[..real]`), or `None` if none is queued.
    pub fn pop_into(&self, out: &mut [u32; DMA_HALF_FRAMES]) -> Option<TeeHalf> {
        let mut it = out.iter_mut();
        self.pop_with(|w| {
            if let Some(o) = it.next() {
                *o = w;
            }
        })
    }

    /// Consumer: the oldest queued half, its real frames handed to `each`
    /// in order (no copy buffer needed), or `None` if none is queued.
    pub fn pop_with(&self, mut each: impl FnMut(u32)) -> Option<TeeHalf> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let i = tail as usize & (H - 1);
        let c = self.counts.get(i)?.load(Ordering::Relaxed);
        let first = self.firsts.get(i)?.load(Ordering::Relaxed);
        let real = (c & 0xFFFF) as usize;
        if let Some(slot) = self.slots.get(i) {
            for s in slot.iter().take(real) {
                each(s.load(Ordering::Relaxed));
            }
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Some(TeeHalf {
            first,
            real: real as u16,
            held: (c >> 16) as u16,
        })
    }

    /// Consumer: drops every queued half.
    pub fn clear(&self) {
        let head = self.head.load(Ordering::Acquire);
        self.tail.store(head, Ordering::Release);
    }

    /// Halves waiting (a snapshot).
    pub fn len(&self) -> usize {
        let h = self.head.load(Ordering::Acquire);
        let t = self.tail.load(Ordering::Acquire);
        h.wrapping_sub(t) as usize
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> TeeStats {
        TeeStats {
            seen: self.seen.load(Ordering::Relaxed),
            queued: self.queued.load(Ordering::Relaxed),
            dropped_halves: self.dropped_halves.load(Ordering::Relaxed),
            dropped_frames: self.dropped_frames.load(Ordering::Relaxed),
        }
    }
}

impl<const H: usize> Default for AudioTee<H> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::collections::VecDeque;
    use std::vec;
    use std::vec::Vec;

    fn ss(l: i16, r: i16) -> StereoSample {
        StereoSample { left: l, right: r }
    }

    /// Runs `x` into a fresh blocker, `n` samples, returns the outputs.
    fn run(x: i16, n: usize) -> Vec<i32> {
        let mut b = DcBlocker::new();
        (0..n).map(|_| b.step(x)).collect()
    }

    #[test]
    fn step_response_decays_monotonically_to_zero() {
        let y = run(20_000, 20_000);
        assert_eq!(y[0], 20_000);
        for w in y.windows(2) {
            assert!(w[1] <= w[0], "not monotone: {} then {}", w[0], w[1]);
            assert!(w[1] >= 0);
        }
        assert_eq!(*y.last().unwrap(), 0, "constant input must decay to 0");
    }

    #[test]
    fn step_response_follows_a_to_the_n() {
        let y = run(20_000, 2_000);
        let a = 1.0 - 1.0 / 512.0f64;
        for &n in &[1usize, 10, 100, 511, 1000, 1500] {
            let want = 20_000.0 * a.powi(n as i32);
            assert!(
                (f64::from(y[n]) - want).abs() < 2.0,
                "n={n}: {} vs {want}",
                y[n]
            );
        }
    }

    #[test]
    fn negative_step_decays_to_exactly_zero_too() {
        let y = run(-32_768, 20_000);
        assert_eq!(y[0], -32_768);
        for w in y.windows(2) {
            assert!(w[1] >= w[0]);
        }
        assert_eq!(*y.last().unwrap(), 0);
    }

    #[test]
    fn small_constants_do_not_stick() {
        // Plain `ys >> 9` truncation sticks for small inputs.
        for x in [1i16, -1, 3, -3, 100, -100] {
            assert_eq!(*run(x, 20_000).last().unwrap(), 0, "x={x}");
        }
    }

    #[test]
    fn full_scale_square_waves_stay_bounded() {
        for half in [1usize, 2, 7, 64, 512, 5_000] {
            let mut b = DcBlocker::new();
            let mut max = 0i32;
            for i in 0..100_000usize {
                let x = if (i / half) % 2 == 0 { 32_767 } else { -32_768 };
                let y = b.step(x);
                max = max.max(y.abs());
                assert!(b.ys.abs() < 1 << 25);
            }
            assert!(max <= 65_536, "half={half}: |y|={max}");
            if half >= 5_000 {
                // Slow square: settled between edges, so an edge reaches
                // nearly twice full scale.
                assert!(max > 60_000, "half={half}: |y|={max}");
            }
        }
    }

    #[test]
    fn extreme_steps_do_not_overflow() {
        let mut b = DcBlocker::new();
        for i in 0..10_000 {
            let x = if i % 3 == 0 { i16::MIN } else { i16::MAX };
            let y = b.step(x);
            assert!(y.abs() <= 65_536);
        }
    }

    #[test]
    fn silence_is_exactly_mid_scale() {
        let mut c = Conditioner::new();
        assert_eq!(c.frame(ss(0, 0)), MID_FRAME);
        // A burst then silence settles back to exactly mid-scale.
        for i in 0..1_000 {
            c.frame(ss(((i * 37) % 20_000) as i16, 25_000));
        }
        let mut last = 0;
        for _ in 0..30_000 {
            last = c.frame(ss(0, 0));
        }
        assert_eq!(last, MID_FRAME);
        // And a constant (DC) input, the idle-DAC case of ADR 0035.
        let mut c = Conditioner::new();
        for _ in 0..30_000 {
            last = c.frame(ss(30_720, -30_720));
        }
        assert_eq!(last, MID_FRAME);
    }

    #[test]
    fn conversion_endpoints_and_rounding() {
        assert_eq!(dac_code(-32_768), 0);
        assert_eq!(dac_code(32_767), 4095);
        assert_eq!(dac_code(0), 2048);
        assert_eq!(dac_code(-65_535), 0);
        assert_eq!(dac_code(65_535), 4095);
        assert_eq!(dac_code(i32::MAX), 4095);
        assert_eq!(dac_code(i32::MIN), 0);
        // Ties round up: 8/16 = 0.5 -> 1, 7/16 -> 0, -8/16 -> 0, -9/16 -> -1.
        assert_eq!(dac_code(8), 2049);
        assert_eq!(dac_code(7), 2048);
        assert_eq!(dac_code(-8), 2048);
        assert_eq!(dac_code(-9), 2047);
        // Monotone over the whole range.
        let mut prev = 0;
        for y in -70_000..=70_000 {
            let c = dac_code(y);
            assert!(c >= prev);
            prev = c;
        }
    }

    #[test]
    fn packing_puts_left_in_the_low_half() {
        let f = pack(0x123, 0xabc);
        assert_eq!(f, 0x0abc_0123);
        assert_eq!(left_code(f), 0x123);
        assert_eq!(right_code(f), 0xabc);
        assert_eq!(pack(0xffff, 0xffff), 0x0fff_0fff);
        let mut c = Conditioner::new();
        let f = c.frame(ss(-32_768, 32_767));
        assert_eq!((left_code(f), right_code(f)), (0, 4095));
    }

    #[test]
    fn feed_hash_matches_manual_fnv_of_frames() {
        let s: Vec<StereoSample> = (0..500)
            .map(|i| ss((i * 91) as i16, (i * -57) as i16))
            .collect();
        let mut c = Conditioner::new();
        let mut bytes = Vec::new();
        for &x in &s {
            bytes.extend_from_slice(&c.frame(x).to_le_bytes());
        }
        assert_eq!(dac_feed_hash(&s), crate::fnv::hash(&bytes));
        // Order matters.
        let mut r = s.clone();
        r.reverse();
        assert_ne!(dac_feed_hash(&r), dac_feed_hash(&s));
    }

    #[test]
    fn ring_is_fifo_and_wraps() {
        let r = AudioRing::<8>::new();
        let mut next = 0u32;
        let mut want = 0u32;
        for round in 0..50 {
            let k = 1 + round % 7;
            let v: Vec<u32> = (0..k)
                .map(|_| {
                    next += 1;
                    next
                })
                .collect();
            assert_eq!(r.push_frames(&v), k as usize);
            let mut out = vec![0u32; k as usize];
            assert_eq!(r.pop_into(&mut out), k as usize);
            for o in out {
                want += 1;
                assert_eq!(o, want);
            }
        }
        assert_eq!(r.stats().underrun_events, 0);
        assert_eq!(r.fill(), 0);
        assert_eq!(r.free(), 8);
    }

    #[test]
    fn ring_index_wraparound_of_u32() {
        // Push/pop enough to wrap the free-running u32 indices is too slow
        // to do directly; start them near the wrap instead.
        let r = AudioRing::<4>::new();
        r.head.store(u32::MAX - 1, Ordering::Relaxed);
        r.tail.store(u32::MAX - 1, Ordering::Relaxed);
        assert_eq!(r.push_frames(&[1, 2, 3, 4]), 4);
        assert_eq!(r.fill(), 4);
        let mut out = [0u32; 4];
        assert_eq!(r.pop_into(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(r.fill(), 0);
    }

    #[test]
    fn underrun_holds_last_frame_and_counts() {
        let r = AudioRing::<8>::new();
        let mut out = [0u32; 4];
        // Nothing ever pushed: mid-scale.
        assert_eq!(r.pop_into(&mut out), 0);
        assert_eq!(out, [MID_FRAME; 4]);
        assert_eq!(r.push_frames(&[10, 11]), 2);
        assert_eq!(r.pop_into(&mut out), 2);
        assert_eq!(out, [10, 11, 11, 11]);
        let s = r.stats();
        assert_eq!((s.underrun_events, s.underrun_frames), (2, 4 + 2));
        // Keeps holding.
        assert_eq!(r.pop_into(&mut out[..2]), 0);
        assert_eq!(out[..2], [11, 11]);
        let s = r.stats();
        assert_eq!((s.underrun_events, s.underrun_frames), (3, 8));
    }

    #[test]
    fn overrun_drops_the_newest_and_counts() {
        let r = AudioRing::<4>::new();
        assert_eq!(r.push_frames(&[1, 2, 3]), 3);
        assert_eq!(r.push_frames(&[4, 5, 6]), 1);
        assert_eq!(r.push_frames(&[7]), 0);
        let s = r.stats();
        assert_eq!((s.overrun_events, s.overrun_frames), (2, 3));
        let mut out = [0u32; 4];
        assert_eq!(r.pop_into(&mut out), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        // Dropped samples never reach the blocker.
        let r = AudioRing::<2>::new();
        let mut c = Conditioner::new();
        let mut c2 = c;
        assert_eq!(r.push_slice(&mut c, &[ss(1000, 0); 5]), 2);
        c2.frame(ss(1000, 0));
        c2.frame(ss(1000, 0));
        assert_eq!(c, c2);
    }

    #[test]
    fn fill_watermarks() {
        let r = AudioRing::<16>::new();
        assert_eq!(r.stats().min_fill, u32::MAX);
        r.push_frames(&[0; 10]);
        r.push_frames(&[0; 3]);
        assert_eq!(r.stats().max_fill, 13);
        let mut out = [0u32; 8];
        r.pop_into(&mut out);
        assert_eq!(r.stats().min_fill, 5);
        r.pop_into(&mut out);
        assert_eq!(r.stats().min_fill, 0);
        r.reset_watermarks();
        let s = r.stats();
        assert_eq!((s.min_fill, s.max_fill), (u32::MAX, 0));
        assert_eq!(s.underrun_events, 1, "counters survive a reset");
    }

    #[test]
    fn push_slice_equals_dac_feed_hash() {
        let s: Vec<StereoSample> = (0..3000)
            .map(|i| ss(((i * 131) % 60_000 - 30_000) as i16, (i * 17) as i16))
            .collect();
        let r = FeedRing::new();
        let mut c = Conditioner::new();
        let mut h = FeedHash::new();
        let mut out = [0u32; DMA_HALF_FRAMES];
        for chunk in s.chunks(550) {
            assert_eq!(r.push_slice(&mut c, chunk), chunk.len());
            while r.fill() >= DMA_HALF_FRAMES {
                let n = r.pop_into(&mut out);
                h.fold(&out[..n]);
            }
        }
        let rest = r.fill();
        let n = r.pop_into(&mut out[..rest]);
        h.fold(&out[..n]);
        assert_eq!(h.get(), dac_feed_hash(&s));
    }

    /// The tap hashes exactly the real frames, across underruns (the
    /// filler is not hashed) and odd pop sizes.
    #[test]
    fn feed_tap_hashes_real_frames_only() {
        let s: Vec<StereoSample> = (0..2000)
            .map(|i| ss(((i * 77) % 50_000 - 25_000) as i16, (i * 29) as i16))
            .collect();
        let r = FeedRing::new();
        let tap = FeedTap::new();
        assert_eq!(tap.hash(), FeedHash::new().get());
        let mut c = Conditioner::new();
        let mut out = [0u32; DMA_HALF_FRAMES];
        // Pop more than is there each time: every pop but the first
        // underruns.
        let mut pops = 0;
        for chunk in s.chunks(100) {
            r.push_slice(&mut c, chunk);
            tap.pop_into(&r, &mut out[..150]);
            pops += 1;
        }
        assert!(r.stats().underrun_events >= pops - 1);
        assert_eq!(tap.pop_into(&r, &mut out), 0, "drained");
        assert_eq!(tap.frames(), 2000);
        assert_eq!(tap.hash(), dac_feed_hash(&s));
    }

    #[test]
    fn random_interleaving_matches_a_vecdeque_model() {
        for seed in 1..=20u64 {
            let r = AudioRing::<16>::new();
            let mut model: VecDeque<u32> = VecDeque::new();
            let mut last = MID_FRAME;
            let (mut ue, mut uf, mut oe, mut of) = (0u32, 0u32, 0u32, 0u32);
            let mut rng = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let mut next = 0u32;
            for _ in 0..5_000 {
                rng ^= rng << 13;
                rng ^= rng >> 7;
                rng ^= rng << 17;
                let k = (rng >> 8) as usize % 12;
                if rng & 1 == 0 {
                    let v: Vec<u32> = (0..k)
                        .map(|_| {
                            next += 1;
                            next
                        })
                        .collect();
                    let n = r.push_frames(&v);
                    let fit = k.min(16 - model.len());
                    assert_eq!(n, fit);
                    model.extend(&v[..fit]);
                    if fit < k {
                        oe += 1;
                        of += (k - fit) as u32;
                    }
                } else {
                    let mut out = vec![0u32; k];
                    let n = r.pop_into(&mut out);
                    let real = k.min(model.len());
                    assert_eq!(n, real);
                    let mut want = Vec::new();
                    for i in 0..k {
                        if i < real {
                            last = model.pop_front().unwrap();
                        }
                        want.push(last);
                    }
                    assert_eq!(out, want);
                    if real < k {
                        ue += 1;
                        uf += (k - real) as u32;
                    }
                }
                assert_eq!(r.fill(), model.len());
                assert_eq!(r.free(), 16 - model.len());
            }
            let s = r.stats();
            assert_eq!(
                (
                    s.underrun_events,
                    s.underrun_frames,
                    s.overrun_events,
                    s.overrun_frames
                ),
                (ue, uf, oe, of)
            );
        }
    }

    #[test]
    fn tee_copies_real_frames_counts_and_drops() {
        let tee: AudioTee<4> = AudioTee::new();
        let half: [u32; DMA_HALF_FRAMES] = core::array::from_fn(|i| i as u32 * 7);
        let mut out = [0u32; DMA_HALF_FRAMES];
        // Off: counted, not copied.
        tee.offer(&half, 256);
        assert!(tee.is_empty());
        assert_eq!(tee.stats().seen, 256);
        tee.set_on(true);
        tee.offer(&half, 256);
        tee.offer(&half, 200); // an underrun: 56 filler frames
        let a = tee.pop_into(&mut out).unwrap();
        assert_eq!(
            a,
            TeeHalf {
                first: 256,
                real: 256,
                held: 0
            }
        );
        assert_eq!(out, half);
        out = [0; DMA_HALF_FRAMES];
        let b = tee.pop_into(&mut out).unwrap();
        assert_eq!(
            b,
            TeeHalf {
                first: 512,
                real: 200,
                held: 56
            }
        );
        assert_eq!(out[..200], half[..200]);
        assert_eq!(out[200], 0, "filler not copied");
        assert_eq!(tee.pop_into(&mut out), None);
        // Full: the fifth half is dropped and leaves a gap in `first`.
        for _ in 0..5 {
            tee.offer(&half, 256);
        }
        let s = tee.stats();
        assert_eq!((s.dropped_halves, s.dropped_frames, s.queued), (1, 256, 6));
        let firsts: [u32; 4] = core::array::from_fn(|_| tee.pop_into(&mut out).unwrap().first);
        assert_eq!(firsts, [712, 968, 1224, 1480]);
        tee.offer(&half, 256);
        assert_eq!(
            tee.pop_into(&mut out).unwrap().first,
            1992,
            "gap of the dropped half"
        );
        tee.offer(&half, 256);
        tee.clear();
        assert!(tee.is_empty());
        tee.set_on(false);
        tee.offer(&half, 10);
        assert!(tee.is_empty());
        assert_eq!(tee.stats().seen, 2514);
        tee.set_on(true);
        tee.offer(&half, 10);
        tee.reset();
        assert!(!tee.is_on() && tee.is_empty());
        assert_eq!(tee.stats(), TeeStats::default());
    }
}
