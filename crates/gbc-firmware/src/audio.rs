//! Audio out, board-independent (Stage 2 M4): the DC blocker ADR 0035
//! leaves to the platform, the sample ring between the main loop
//! (producer) and the DAC's refill ISR (consumer), the hash of exactly
//! what the output is fed, and the tee that copies it for the PC.
//!
//! The data path is `StereoSample` (core, 32,768 Hz) -> [`DcBlocker`] per
//! channel -> saturate to `i16` ([`sample`]) -> [`PcmFrame`] -> [`AudioRing`]
//! -> ISR -> the board's output. Nothing here knows an output's format
//! (ADR 0044): the board converts [`PcmFrame`]s when it fills its DMA
//! buffer (the H753's 12-bit DAC: `gbc_nucleo_h753::audio_dac`; a 16-bit
//! I2S codec takes them as they are), and its DMA geometry is the board's
//! too (the tee and the host stand-ins take the half size as a parameter).
//!
//! # Scale
//!
//! A [`PcmFrame`] is signed 16-bit PCM, the core's own scale: 0 is
//! silence, core full scale (+-32,768) is full scale. The blocker's output
//! can reach +-65,535 (ADR 0035, "Headroom"; only after the mix sits at
//! one extreme and swings to the other), which saturates at the `i16`
//! limits rather than being halved: ordinary content (a full-range square
//! wave is +-30,720) has the full resolution. The H753 DAC's code is
//! `2048 + n/16` rounded and clamped, exactly the code it had before the
//! frame went neutral (ADR 0044; test there).

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

/// One stereo output frame: signed 16-bit PCM, left and right (module
/// docs, "Scale"). Left is the board's first channel (the H753: DAC
/// channel 1, PA4).
///
/// Rings, the tap and the tee hold it as one `u32` ([`to_bits`]: left in
/// bits 15:0, right in 31:16, each two's complement), so a slot is one
/// atomic word and its four little-endian bytes are the interleaved
/// `s16le` stereo stream (what [`FeedHash`] hashes).
///
/// [`to_bits`]: Self::to_bits
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PcmFrame {
    pub left: i16,
    pub right: i16,
}

impl PcmFrame {
    /// Both channels at 0.
    pub const SILENCE: PcmFrame = PcmFrame { left: 0, right: 0 };

    pub const fn new(left: i16, right: i16) -> Self {
        Self { left, right }
    }

    /// The frame as one word: left in bits 15:0, right in 31:16.
    #[inline]
    pub const fn to_bits(self) -> u32 {
        self.left as u16 as u32 | (self.right as u16 as u32) << 16
    }

    /// The frame of a [`to_bits`](Self::to_bits) word.
    #[inline]
    pub const fn from_bits(w: u32) -> Self {
        Self {
            left: w as u16 as i16,
            right: (w >> 16) as u16 as i16,
        }
    }
}

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

/// A blocker output (`i16` units, `i32`) as an output sample: saturated
/// to `i16` (module docs, "Scale").
#[inline]
pub fn sample(y: i32) -> i16 {
    y.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Both channels of a [`PcmFrame::to_bits`] word as 12-bit offset-binary
/// codes, each in its own 16-bit lane: left in bits 11:0, right in 27:16,
/// the rest 0. Per channel `code = 2048 + n/16`, rounded to nearest with
/// ties up and clamped to 0..=4095: -32,768 gives 0, 0 gives 2048, 32,744
/// and above give 4095. For a 12-bit output (the H753's DAC writes this
/// word to `DAC_DHR12RD` unchanged, whose channels sit in the same lanes;
/// ADR 0044). Together with [`sample`] it is bit-identical to the code the
/// DAC got before the frame went neutral (`clamp((y + 8) >> 4) + 2048`
/// over the blocker output `y`; test `u12_lanes_equal_the_old_dac_code`).
///
/// Both lanes at once, without a branch (it runs in the DMA ISR, 256
/// frames per refill): flipping bit 15 of each lane gives `u = n + 32,768`
/// (0..=65,535); `(u + 8) >> 4` is `(u >> 4) + bit 3 of u` (0..=4,096, so
/// no carry leaves a lane), and 4,096 (bit 12 set) becomes 4,095.
#[inline(always)]
pub const fn u12_lanes(pcm: u32) -> u32 {
    let u = pcm ^ 0x8000_8000;
    let t = ((u >> 4) & 0x0fff_0fff) + ((u >> 3) & 0x0001_0001);
    t - ((t >> 12) & 0x0001_0001)
}

/// What an output takes for a [`PcmFrame::to_bits`] word: a zero-sized
/// type whose [`map`](Self::map) the pop applies as it writes each word
/// (monomorphised; no dispatch). Lets the board's DMA buffer get its own
/// format in the same pass that reads the ring, one store per frame, with
/// the tap and the tee still seeing [`PcmFrame`] words ([`Popped`]).
pub trait FrameMap {
    fn map(pcm: u32) -> u32;
}

/// The frame words unchanged (the host stand-ins; a 16-bit codec).
pub struct Pcm;
impl FrameMap for Pcm {
    #[inline(always)]
    fn map(pcm: u32) -> u32 {
        pcm
    }
}

/// [`u12_lanes`]: the H753's `DAC_DHR12RD` (ADR 0044).
pub struct U12Lanes;
impl FrameMap for U12Lanes {
    #[inline(always)]
    fn map(pcm: u32) -> u32 {
        u12_lanes(pcm)
    }
}

/// An output half to pop into, its format `M`. Write-only: it gives the
/// words back to no one, so the only way to fill it is a pop
/// ([`AudioRing::pop_to`], [`FeedTap::pop_to`]), which writes every word
/// exactly once (the real frames, then the held last frame), already
/// mapped. The board makes one over its DMA half and hands it to the
/// image's source, so the source cannot skip the conversion.
pub struct HalfOut<'a, M: FrameMap> {
    words: &'a mut [u32],
    _map: core::marker::PhantomData<M>,
}

impl<'a, M: FrameMap> HalfOut<'a, M> {
    pub fn new(words: &'a mut [u32]) -> Self {
        Self {
            words,
            _map: core::marker::PhantomData,
        }
    }

    /// Frames in the half.
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }
}

/// The frames one pop handed out, as the pop's `inspect` callback sees
/// them (before the ring frees their slots): the real frames, in order,
/// read from the ring as [`PcmFrame::to_bits`] words (never the output's
/// mapped words: an output buffer may be non-cacheable, and the hash and
/// the tee are board-independent), and how many filler frames followed.
pub struct Popped<'a> {
    /// The real frames, in up to two runs (the ring wraps).
    a: &'a [AtomicU32],
    b: &'a [AtomicU32],
    held: usize,
}

impl Popped<'_> {
    /// Real frames.
    pub fn len(&self) -> usize {
        self.a.len() + self.b.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Underrun filler frames after them (the last frame repeated).
    pub fn held(&self) -> usize {
        self.held
    }

    /// Calls `f` with each real frame's word, in order.
    #[inline]
    pub fn for_each(&self, mut f: impl FnMut(u32)) {
        for s in self.a.iter().chain(self.b) {
            f(s.load(Ordering::Relaxed));
        }
    }
}

/// Both channels' blockers: core samples to output frames. Owned by the
/// producer (the main loop).
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

    /// One core sample to the frame the output is fed.
    #[inline]
    pub fn frame(&mut self, s: StereoSample) -> PcmFrame {
        PcmFrame {
            left: sample(self.left.step(s.left)),
            right: sample(self.right.step(s.right)),
        }
    }
}

/// The hash of the output feed: FNV-1a (`fnv.rs`) over the frames in
/// order, each as the four little-endian bytes of its
/// [`PcmFrame::to_bits`] word (left `s16le`, then right: the interleaved
/// PCM stream). The board folds the real frames the ISR pops (the first
/// `n` that [`AudioRing::pop_into`] returns, never the underrun filler);
/// the host folds the same stream with [`dac_feed_hash`]. They agree when
/// every produced frame was fed. Board-independent since ADR 0044 (before,
/// it hashed the H753's `DAC_DHR12RD` words).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FeedHash(Fnv1a);

impl FeedHash {
    pub const fn new() -> Self {
        Self(Fnv1a::new())
    }

    /// Folds frames given as [`PcmFrame::to_bits`] words (what
    /// [`AudioRing::pop_into`] writes).
    pub fn fold(&mut self, words: &[u32]) {
        for &w in words {
            self.0.u32(w);
        }
    }

    /// Folds the real frames of a pop.
    pub fn fold_popped(&mut self, p: &Popped<'_>) {
        p.for_each(|w| self.0.u32(w));
    }

    /// Folds frames.
    pub fn fold_frames(&mut self, frames: &[PcmFrame]) {
        for &f in frames {
            self.0.u32(f.to_bits());
        }
    }

    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// The feed hash of a whole run's core samples (what the host `headless`
/// prints to compare with the board): a fresh [`Conditioner`], every
/// sample in order. Call it on the concatenation of all `run_frame`
/// outputs of the run.
pub fn dac_feed_hash(samples: &[StereoSample]) -> u64 {
    let mut c = Conditioner::new();
    let mut h = FeedHash::new();
    for &s in samples {
        h.fold_frames(&[c.frame(s)]);
    }
    h.get()
}

/// The consumer side of the output feed: pops from an [`AudioRing`] and folds
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

    /// Consumer: [`AudioRing::pop_to`], folding the real frames (as
    /// [`PcmFrame`] words, whatever `M` writes to `out`) into the hash
    /// before `inspect` sees them. Returns the real frame count.
    #[inline]
    pub fn pop_to<const N: usize, M: FrameMap>(
        &self,
        ring: &AudioRing<N>,
        out: HalfOut<'_, M>,
        inspect: impl FnOnce(&Popped<'_>),
    ) -> usize {
        let n = ring.pop_to(out, |p| {
            if !p.is_empty() {
                let state = u64::from(self.lo.load(Ordering::Relaxed))
                    | u64::from(self.hi.load(Ordering::Relaxed)) << 32;
                let mut h = FeedHash(Fnv1a::resume(state));
                h.fold_popped(p);
                let state = h.get();
                self.lo.store(state as u32, Ordering::Relaxed);
                self.hi.store((state >> 32) as u32, Ordering::Relaxed);
            }
            inspect(p);
        });
        self.count(n);
        n
    }

    /// Consumer: [`AudioRing::pop_to`] and the frame count of
    /// [`pop_to`](Self::pop_to), without the fold: for a feed nobody
    /// compares (the live player; [`crate::paced::Service::HASHES`]). The
    /// hash stays where it was. One consumer uses one of the two families.
    #[inline]
    pub fn pop_unhashed_to<const N: usize, M: FrameMap>(
        &self,
        ring: &AudioRing<N>,
        out: HalfOut<'_, M>,
        inspect: impl FnOnce(&Popped<'_>),
    ) -> usize {
        let n = ring.pop_to(out, inspect);
        self.count(n);
        n
    }

    /// [`pop_to`](Self::pop_to) into plain [`PcmFrame`] words (the host
    /// stand-ins for the ISR): `out[..n]` are the real frames.
    pub fn pop_into<const N: usize>(&self, ring: &AudioRing<N>, out: &mut [u32]) -> usize {
        self.pop_to(ring, HalfOut::<Pcm>::new(out), |_| {})
    }

    /// [`pop_unhashed_to`](Self::pop_unhashed_to) into plain words.
    pub fn pop_unhashed<const N: usize>(&self, ring: &AudioRing<N>, out: &mut [u32]) -> usize {
        self.pop_unhashed_to(ring, HalfOut::<Pcm>::new(out), |_| {})
    }

    /// Publishes `n` more real frames (after the hash, `Release`).
    fn count(&self, n: usize) {
        if n != 0 {
            let f = self.frames.load(Ordering::Relaxed);
            self.frames
                .store(f.wrapping_add(n as u32), Ordering::Release);
        }
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

/// A single-producer single-consumer ring of [`PcmFrame`]s, in a
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
            last: AtomicU32::new(PcmFrame::SILENCE.to_bits()),
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
        self.push_with(s.len(), |i| {
            s.get(i).map_or(0, |&x| cond.frame(x).to_bits())
        })
    }

    /// Producer: pushes already converted frames (see [`push_slice`]).
    ///
    /// [`push_slice`]: Self::push_slice
    pub fn push_frames(&self, f: &[PcmFrame]) -> usize {
        self.push_with(f.len(), |i| f.get(i).map_or(0, |x| x.to_bits()))
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

    /// Consumer: fills `out` with frames from the ring, as
    /// [`PcmFrame::to_bits`] words ([`pop_to`](Self::pop_to) converts them
    /// for a board's DMA buffer), and returns the number of **real** frames
    /// (`out[..n]`). On a shortage the rest of `out` is filled with the
    /// last frame handed out ([`PcmFrame::SILENCE`] if none yet): the
    /// output holds its level instead of stepping to silence, the smallest
    /// click available. That counts one underrun event and the missing
    /// frames. Hash only `out[..n]` ([`FeedHash`]).
    pub fn pop_into(&self, out: &mut [u32]) -> usize {
        self.pop_to(HalfOut::<Pcm>::new(out), |_| {})
    }

    /// Consumer: [`pop_into`](Self::pop_into), writing each word through
    /// `M` ([`FrameMap`]) as it goes, so the output gets exactly one store
    /// per frame and is never read: the real frames, then `M::map` of the
    /// last frame in every remaining slot. `inspect` sees the real frames
    /// as [`PcmFrame`] words, read from the ring before their slots are
    /// freed (the feed hash, the tee). Returns their count.
    #[inline]
    pub fn pop_to<M: FrameMap>(
        &self,
        out: HalfOut<'_, M>,
        inspect: impl FnOnce(&Popped<'_>),
    ) -> usize {
        let out = out.words;
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let avail = head.wrapping_sub(tail) as usize;
        let n = out.len().min(avail);
        // The real frames: slots start.. up to the end of the array, then
        // from 0 (n <= avail <= N, so at most one wrap).
        let start = (tail & Self::MASK) as usize;
        let first = n.min(N - start);
        let a = self.slots.get(start..start + first).unwrap_or(&[]);
        let b = self.slots.get(..n - first).unwrap_or(&[]);
        // Copy n, then fill the rest: no per-slot `i < n` test (P5).
        let (oa, rest) = out.split_at_mut(a.len().min(out.len()));
        let (ob, held) = rest.split_at_mut(b.len().min(rest.len()));
        map_run::<M>(oa, a);
        map_run::<M>(ob, b);
        let last = match b.last().or(a.last()) {
            Some(s) => s.load(Ordering::Relaxed),
            None => self.last.load(Ordering::Relaxed),
        };
        held.fill(M::map(last));
        inspect(&Popped {
            a,
            b,
            held: held.len(),
        });
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

/// `out[i] = M::map(src[i])` over the shorter of the two. Out of line: one
/// unrolled copy serves both runs of a pop (the ISR's code size).
#[inline(never)]
fn map_run<M: FrameMap>(out: &mut [u32], src: &[AtomicU32]) {
    for (o, s) in out.iter_mut().zip(src) {
        *o = M::map(s.load(Ordering::Relaxed));
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
/// it refilled a half ([`FeedTap::pop_to`]), offers that half here
/// ([`offer_popped`](Self::offer_popped), from the pop's `inspect`); the
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
pub struct AudioTee<const F: usize, const H: usize = TEE_HALVES> {
    /// 0 off, 1 on.
    on: AtomicU32,
    slots: [[AtomicU32; F]; H],
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

impl<const F: usize, const H: usize> AudioTee<F, H> {
    /// Off and empty.
    pub const fn new() -> Self {
        const { assert!(H.is_power_of_two() && H <= 1 << 16) };
        Self {
            on: AtomicU32::new(0),
            slots: [const { [const { AtomicU32::new(0) }; F] }; H],
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

    /// Producer: `half` is the DMA half just refilled, as
    /// [`PcmFrame::to_bits`] words (before the board converts them),
    /// `real` the real frames at its start (the rest is underrun filler).
    /// At most `F` (the board's DMA half) frames are kept.
    pub fn offer(&self, half: &[u32], real: usize) {
        let src = half.get(..real.min(half.len())).unwrap_or(&[]);
        self.offer_with(real.min(half.len()), half.len(), |slot| {
            for (d, &s) in slot.iter().zip(src) {
                d.store(s, Ordering::Relaxed);
            }
        });
    }

    /// Producer: [`offer`](Self::offer) of the half one pop just filled,
    /// its real frames read from the ring (`inspect` of
    /// [`FeedTap::pop_to`]): the board's DMA half, in its own format, is
    /// not read back.
    pub fn offer_popped(&self, p: &Popped<'_>) {
        self.offer_with(p.len(), p.len() + p.held(), |slot| {
            let mut d = slot.iter();
            p.for_each(|w| {
                if let Some(d) = d.next() {
                    d.store(w, Ordering::Relaxed);
                }
            });
        });
    }

    /// `real` real frames of a half of `len`; `copy` stores the real
    /// frames (at most `F`) into the slot it is given.
    #[inline]
    fn offer_with(&self, real: usize, len: usize, copy: impl FnOnce(&[AtomicU32; F])) {
        let real = real.min(F);
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
        if let Some(slot) = self.slots.get(i) {
            copy(slot);
        }
        let held = (len - real).min(0xFFFF) as u32;
        if let (Some(c), Some(f)) = (self.counts.get(i), self.firsts.get(i)) {
            c.store(real as u32 | held << 16, Ordering::Relaxed);
            f.store(first, Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(1), Ordering::Release);
        self.queued.fetch_add(1, Ordering::Relaxed);
    }

    /// Consumer: the oldest queued half into `out` (its real frames at
    /// `out[..real]`), or `None` if none is queued.
    pub fn pop_into(&self, out: &mut [PcmFrame; F]) -> Option<TeeHalf> {
        let mut it = out.iter_mut();
        self.pop_with(|f| {
            if let Some(o) = it.next() {
                *o = f;
            }
        })
    }

    /// Consumer: the oldest queued half, its real frames handed to `each`
    /// in order (no copy buffer needed), or `None` if none is queued.
    pub fn pop_with(&self, mut each: impl FnMut(PcmFrame)) -> Option<TeeHalf> {
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
                each(PcmFrame::from_bits(s.load(Ordering::Relaxed)));
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

impl<const F: usize, const H: usize> Default for AudioTee<F, H> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The H753's DMA half, as the host stand-ins use it.
    const HALF: usize = 256;

    /// Test frames from their bit patterns (the ring tests use the words
    /// as tokens).
    fn fr(w: &[u32]) -> Vec<PcmFrame> {
        w.iter().map(|&w| PcmFrame::from_bits(w)).collect()
    }

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
    fn silence_is_exactly_zero() {
        let mut c = Conditioner::new();
        assert_eq!(c.frame(ss(0, 0)), PcmFrame::SILENCE);
        // A burst then silence settles back to exactly zero.
        for i in 0..1_000 {
            c.frame(ss(((i * 37) % 20_000) as i16, 25_000));
        }
        let mut last = PcmFrame::new(1, 1);
        for _ in 0..30_000 {
            last = c.frame(ss(0, 0));
        }
        assert_eq!(last, PcmFrame::SILENCE);
        // And a constant (DC) input, the idle-DAC case of ADR 0035.
        let mut c = Conditioner::new();
        for _ in 0..30_000 {
            last = c.frame(ss(30_720, -30_720));
        }
        assert_eq!(last, PcmFrame::SILENCE);
    }

    /// End to end (conditioner, neutral frame, the ISR's conversion):
    /// silence, after a burst or a constant DC input, is exactly the DAC's
    /// mid-scale code on both channels.
    #[test]
    fn silence_is_exactly_mid_scale() {
        const MID: u32 = 2048 | 2048 << 16;
        let word = |f: PcmFrame| u12_lanes(f.to_bits());
        let mut c = Conditioner::new();
        assert_eq!(word(c.frame(ss(0, 0))), MID);
        for i in 0..1_000 {
            c.frame(ss(((i * 37) % 20_000) as i16, 25_000));
        }
        let mut last = PcmFrame::new(1, 1);
        for _ in 0..30_000 {
            last = c.frame(ss(0, 0));
        }
        assert_eq!(word(last), MID);
        let mut c = Conditioner::new();
        for _ in 0..30_000 {
            last = c.frame(ss(30_720, -30_720));
        }
        assert_eq!(word(last), MID);
    }

    /// The DAC code of a blocker output `y` through the neutral frame
    /// (`sample`, then the ISR's `u12_lanes`): endpoints, saturation,
    /// round-half-up ties and monotonicity, as before ADR 0044.
    #[test]
    fn conversion_endpoints_and_rounding() {
        let code = |y: i32| u12_lanes(PcmFrame::new(sample(y), 0).to_bits()) & 0xfff;
        assert_eq!(code(-32_768), 0);
        assert_eq!(code(32_767), 4095);
        assert_eq!(code(0), 2048);
        assert_eq!(code(-65_535), 0);
        assert_eq!(code(65_535), 4095);
        assert_eq!(code(i32::MAX), 4095);
        assert_eq!(code(i32::MIN), 0);
        // Ties round up: 8/16 = 0.5 -> 1, 7/16 -> 0, -8/16 -> 0, -9/16 -> -1.
        assert_eq!(code(8), 2049);
        assert_eq!(code(7), 2048);
        assert_eq!(code(-8), 2048);
        assert_eq!(code(-9), 2047);
        let mut prev = 0;
        for y in -70_000..=70_000 {
            let c = code(y);
            assert!(c >= prev, "not monotone at y = {y}");
            prev = c;
        }
    }

    #[test]
    fn sample_saturates_to_i16() {
        assert_eq!(sample(0), 0);
        assert_eq!(sample(-32_768), -32_768);
        assert_eq!(sample(32_767), 32_767);
        assert_eq!(sample(32_768), 32_767);
        assert_eq!(sample(-32_769), -32_768);
        assert_eq!(sample(65_535), 32_767);
        assert_eq!(sample(-65_535), -32_768);
        assert_eq!(sample(i32::MAX), 32_767);
        assert_eq!(sample(i32::MIN), -32_768);
        for y in -70_000..=70_000 {
            let n = i32::from(sample(y));
            assert_eq!(n, y.clamp(-32_768, 32_767));
        }
    }

    /// The DAC code before ADR 0044 (`dac_code` of the blocker output `y`,
    /// as it was), copied unchanged: the reference [`u12_lanes`] must
    /// match.
    fn old_dac_code(y: i32) -> u16 {
        let c = (y.saturating_add(8) >> 4) + 2048;
        c.clamp(0, 4095) as u16
    }

    /// The old `DAC_DHR12RD` word (`pack(dac_code(l), dac_code(r))`).
    fn old_word(l: i32, r: i32) -> u32 {
        u32::from(old_dac_code(l)) & 0xfff | (u32::from(old_dac_code(r)) & 0xfff) << 16
    }

    /// Requirement of ADR 0044: the H753's DAC words are bit-identical to
    /// before for every blocker output (its whole range +-65,535, with
    /// margin, and the `i32` extremes the saturation guards), on either
    /// channel, through the neutral frame (`sample`, `to_bits`) and the
    /// conversion the ISR does.
    #[test]
    fn u12_lanes_equal_the_old_dac_code() {
        let new = |l: i32, r: i32| u12_lanes(PcmFrame::new(sample(l), sample(r)).to_bits());
        let ys = (-70_000..=70_000).chain([i32::MIN, i32::MIN + 7, i32::MAX - 8, i32::MAX]);
        for y in ys {
            assert_eq!(
                new(y, -y.saturating_add(1)),
                old_word(y, -y.saturating_add(1)),
                "y = {y}"
            );
            assert_eq!(new(0, y), old_word(0, y), "right y = {y}");
        }
        // Every i16 on one lane against a spread of values on the other
        // (no lane leaks into its neighbour).
        let others = [
            i16::MIN,
            -32_761,
            -32_760,
            -9,
            -8,
            -1,
            0,
            7,
            8,
            32_759,
            32_760,
            i16::MAX,
        ];
        for n in i16::MIN..=i16::MAX {
            for &o in &others {
                let (a, b) = (i32::from(n), i32::from(o));
                assert_eq!(new(a, b), old_word(a, b), "({n}, {o})");
                assert_eq!(new(b, a), old_word(b, a), "({o}, {n})");
            }
        }
        // The fixed points of the docs.
        assert_eq!(u12_lanes(PcmFrame::new(-32_768, 0).to_bits()), 2048 << 16);
        assert_eq!(
            u12_lanes(PcmFrame::new(32_744, 32_743).to_bits()),
            4095 | 4094 << 16
        );
        assert_eq!(u12_lanes(PcmFrame::SILENCE.to_bits()), 2048 | 2048 << 16);
    }

    #[test]
    fn frame_bits_put_left_in_the_low_half() {
        let f = PcmFrame::new(0x1234, -2);
        assert_eq!(f.to_bits(), 0xfffe_1234);
        assert_eq!(PcmFrame::from_bits(0xfffe_1234), f);
        assert_eq!(PcmFrame::new(-32_768, 32_767).to_bits(), 0x7fff_8000);
        assert_eq!(PcmFrame::SILENCE.to_bits(), 0);
        for w in [
            0,
            1,
            0x8000,
            0xffff,
            0x1_0000,
            0x8000_0000,
            u32::MAX,
            0x7fff_8001,
        ] {
            assert_eq!(PcmFrame::from_bits(w).to_bits(), w);
        }
        let mut c = Conditioner::new();
        assert_eq!(c.frame(ss(-32_768, 32_767)), PcmFrame::new(-32_768, 32_767));
    }

    #[test]
    fn feed_hash_matches_manual_fnv_of_s16le_frames() {
        let s: Vec<StereoSample> = (0..500)
            .map(|i| ss((i * 91) as i16, (i * -57) as i16))
            .collect();
        let mut c = Conditioner::new();
        let mut bytes = Vec::new();
        for &x in &s {
            let f = c.frame(x);
            bytes.extend_from_slice(&f.left.to_le_bytes());
            bytes.extend_from_slice(&f.right.to_le_bytes());
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
            assert_eq!(r.push_frames(&fr(&v)), k as usize);
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
        assert_eq!(r.push_frames(&fr(&[1, 2, 3, 4])), 4);
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
        assert_eq!(out, [0; 4]);
        assert_eq!(r.push_frames(&fr(&[10, 11])), 2);
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

    /// Frame words whose 12-bit codes differ from their own bits, and
    /// whose codes all differ from each other.
    fn pcm(k: i16) -> PcmFrame {
        PcmFrame::new(k.wrapping_mul(997), k.wrapping_mul(-1_201))
    }

    #[test]
    fn pop_to_maps_real_frames_and_the_held_fill_across_the_wrap() {
        let r = AudioRing::<8>::new();
        // Tail at 6: the next pop wraps after 2 slots.
        let six: Vec<PcmFrame> = (1..=6).map(pcm).collect();
        assert_eq!(r.push_frames(&six), 6);
        assert_eq!(r.pop_into(&mut [0; 6]), 6);
        let five: Vec<PcmFrame> = (7..=11).map(pcm).collect();
        assert_eq!(r.push_frames(&five), 5);
        let mut out = [0xdead_beef_u32; 8];
        let mut seen = Vec::new();
        let mut held = usize::MAX;
        let n = r.pop_to(HalfOut::<U12Lanes>::new(&mut out), |p| {
            p.for_each(|w| seen.push(w));
            held = p.held();
            assert_eq!(p.len(), 5);
        });
        assert_eq!(n, 5);
        let want: Vec<u32> = five.iter().map(|f| u12_lanes(f.to_bits())).collect();
        assert_eq!(out[..5], want[..], "real frames, mapped");
        assert_eq!(out[5..], [u12_lanes(pcm(11).to_bits()); 3], "held, mapped");
        assert_eq!(
            seen,
            five.iter().map(|f| f.to_bits()).collect::<Vec<_>>(),
            "inspect sees PcmFrame words"
        );
        assert_eq!(held, 3);
        // Empty: every slot is the mapped last frame, inspect sees none.
        let mut out = [0u32; 4];
        assert_eq!(
            r.pop_to(HalfOut::<U12Lanes>::new(&mut out), |p| assert!(
                p.is_empty()
            )),
            0
        );
        assert_eq!(out, [u12_lanes(pcm(11).to_bits()); 4]);
        // Nothing ever pushed: the mapped silence.
        let r = AudioRing::<8>::new();
        assert_eq!(r.pop_to(HalfOut::<U12Lanes>::new(&mut out), |_| {}), 0);
        assert_eq!(out, [u12_lanes(0); 4]);
        assert_eq!(u12_lanes(0), 2048 | 2048 << 16);
    }

    #[test]
    fn tap_pop_to_hashes_and_tees_pcm_words_not_the_output() {
        const F: usize = 8;
        let frames: Vec<PcmFrame> = (1..=13).map(pcm).collect();
        // Reference: plain words through pop_into and offer.
        let (r0, t0) = (AudioRing::<16>::new(), FeedTap::new());
        let tee0: AudioTee<F, 4> = AudioTee::new();
        tee0.set_on(true);
        // Fused: U12 words out, hash and tee from inspect.
        let (r1, t1) = (AudioRing::<16>::new(), FeedTap::new());
        let tee1: AudioTee<F, 4> = AudioTee::new();
        tee1.set_on(true);
        let mut plain = [0u32; F];
        let mut dac = [0u32; F];
        for chunk in [&frames[..5], &frames[5..13], &[][..]] {
            r0.push_frames(chunk);
            r1.push_frames(chunk);
            let n0 = t0.pop_into(&r0, &mut plain);
            tee0.offer(&plain, n0);
            let n1 = t1.pop_to(&r1, HalfOut::<U12Lanes>::new(&mut dac), |p| {
                tee1.offer_popped(p)
            });
            assert_eq!(n0, n1);
            assert_eq!(dac, plain.map(u12_lanes));
            assert_eq!((t0.hash(), t0.frames()), (t1.hash(), t1.frames()));
        }
        let mut want = FeedHash::new();
        want.fold_frames(&frames);
        assert_eq!(t1.hash(), want.get());
        assert_eq!(tee0.stats(), tee1.stats());
        let (mut o0, mut o1) = ([PcmFrame::SILENCE; F], [PcmFrame::SILENCE; F]);
        for _ in 0..3 {
            assert_eq!(tee0.pop_into(&mut o0), tee1.pop_into(&mut o1));
            assert_eq!(o0, o1);
        }
        // Unhashed: counts, no fold.
        let (r2, t2) = (AudioRing::<16>::new(), FeedTap::new());
        r2.push_frames(&frames[..3]);
        let n = t2.pop_unhashed_to(&r2, HalfOut::<U12Lanes>::new(&mut dac), |p| {
            assert_eq!((p.len(), p.held()), (3, F - 3))
        });
        assert_eq!((n, t2.frames(), t2.hash()), (3, 3, FeedTap::new().hash()));
        assert_eq!(dac[3..], [u12_lanes(pcm(3).to_bits()); F - 3]);
    }

    #[test]
    fn overrun_drops_the_newest_and_counts() {
        let r = AudioRing::<4>::new();
        assert_eq!(r.push_frames(&fr(&[1, 2, 3])), 3);
        assert_eq!(r.push_frames(&fr(&[4, 5, 6])), 1);
        assert_eq!(r.push_frames(&fr(&[7])), 0);
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
        r.push_frames(&fr(&[0; 10]));
        r.push_frames(&fr(&[0; 3]));
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
        let mut out = [0u32; HALF];
        for chunk in s.chunks(550) {
            assert_eq!(r.push_slice(&mut c, chunk), chunk.len());
            while r.fill() >= HALF {
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
        let mut out = [0u32; HALF];
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

    /// `pop_unhashed` pops and counts as `pop_into` does (same frames out,
    /// same count) and leaves the hash at the hash of nothing.
    #[test]
    fn feed_tap_unhashed_counts_but_leaves_the_hash() {
        let s: Vec<StereoSample> = (0..2000)
            .map(|i| ss(((i * 77) % 50_000 - 25_000) as i16, (i * 29) as i16))
            .collect();
        let (r, q) = (FeedRing::new(), FeedRing::new());
        let (hashed, unhashed) = (FeedTap::new(), FeedTap::new());
        let (mut c, mut d) = (Conditioner::new(), Conditioner::new());
        let (mut a, mut b) = ([0u32; HALF], [0u32; HALF]);
        for chunk in s.chunks(100) {
            r.push_slice(&mut c, chunk);
            q.push_slice(&mut d, chunk);
            let n = hashed.pop_into(&r, &mut a[..150]);
            assert_eq!(unhashed.pop_unhashed(&q, &mut b[..150]), n);
            assert_eq!(a, b);
        }
        assert_eq!((hashed.frames(), unhashed.frames()), (2000, 2000));
        assert_eq!(hashed.hash(), dac_feed_hash(&s));
        assert_eq!(unhashed.hash(), FeedHash::new().get());
    }

    #[test]
    fn random_interleaving_matches_a_vecdeque_model() {
        for seed in 1..=20u64 {
            let r = AudioRing::<16>::new();
            let mut model: VecDeque<u32> = VecDeque::new();
            let mut last = 0;
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
                    let n = r.push_frames(&fr(&v));
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
        let tee: AudioTee<HALF, 4> = AudioTee::new();
        let half: [u32; HALF] = core::array::from_fn(|i| i as u32 * 7);
        let mut out = [PcmFrame::SILENCE; HALF];
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
        assert_eq!(out.map(PcmFrame::to_bits), half);
        out = [PcmFrame::SILENCE; HALF];
        let b = tee.pop_into(&mut out).unwrap();
        assert_eq!(
            b,
            TeeHalf {
                first: 512,
                real: 200,
                held: 56
            }
        );
        assert_eq!(out.map(PcmFrame::to_bits)[..200], half[..200]);
        assert_eq!(out[200], PcmFrame::SILENCE, "filler not copied");
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
