//! The audio-paced main loop (Stage 2 M4, "Audio-driven pacing"; the
//! firmware counterpart of desktop ADR 0024): the emulator's speed is
//! slaved to the DAC's sample clock. The loop runs the core one
//! `run_frame` call at a time **while the sample ring has room for a
//! whole call's samples** ([`MIN_AUDIO_SLOTS`] = 550, the most one call
//! can produce), pushes the call's samples through the [`Conditioner`]
//! into the ring, hands a completed frame to the [`FrameSink`], and
//! otherwise calls the board's [`Idle`] hook (`WFI` on the MCU; the DMA
//! refill interrupt, which pops the ring, wakes it). Since a push never
//! exceeds the room checked before the call and the consumer only frees
//! room, the producer can never overrun the ring.
//!
//! Scripted input is applied exactly as [`crate::headless`] does it (the
//! held set of frame `n` with `set_buttons` right before call `n`), so a
//! run's emulated state depends only on the ROM, the script and `N`,
//! never on when the consumer pops. [`run`] plays `N` calls (35,837 is
//! 10 minutes at 59.7275 Hz), drains the ring and prints the lines below.
//!
//! [`run_with`] (M5, the player loop) generalises the input to any
//! [`InputSource`] (a [`Script`], or [`crate::buttons::LiveInput`]: pins
//! and PC buttons), sampled once per frame and applied at the frame's start
//! clock, and adds a [`Service`] hook between frames for the board's UART
//! (input frames, save load and dump through [`crate::save`]). [`run`] is
//! `run_with` with the script and [`NoService`], so its lines are unchanged.
//!
//! # Line format (version 1)
//!
//! As in [`crate::headless`]: `\n` endings, decimal numbers, hashes as 16
//! lower-case hex digits of 64-bit FNV-1a. **Compared lines** start with
//! `paced `; everything in them is emulated state (equal on host and
//! board, at any consumer rate). Lines starting with `stats ` are
//! report-only.
//!
//! ```text
//! paced v1 rom_bytes=<len> rom_fnv=<hash> input_fnv=<hash> frames=<N>
//! paced end frames=<N> clock=<clock> ready=<k> fbs=<hash> feed_frames=<count> feed=<hash>
//! paced drain feed_frames=<count> feed=<hash>
//! stats paced core min=<c> mean=<c> max=<c> max_frame=<n> hz=<hz>
//! stats paced frontend push_mean=<c> push_max=<c> hash_mean=<c> hash_max=<c> sink_mean=<c> sink_max=<c>
//! stats paced idle calls=<n> drain_calls=<n>
//! stats paced ring min_fill=<f> max_fill=<f> underrun_events=<e> underrun_frames=<f> overrun_events=<e> overrun_frames=<f>
//! stats paced drain underrun_events=<e> underrun_frames=<f>
//! stats paced speed elapsed=<c> ppm=<+-n> steady_ppm=<+-n>       (only if hz != 0)
//! ```
//!
//! - Header: as `headless v1` (the ROM's length and FNV-1a, the script's
//!   [`Script::fnv`], `N`).
//! - `end` (printed when the `N`th call's samples are in the ring):
//!   `clock` is `Emulator::clock`; `ready` the number of calls that
//!   returned `FRAME_READY`; `fbs` is FNV-1a over those calls' frame
//!   hashes in order, each the `headless` `fb=` value ([`Fnv1a::frame`] of
//!   the whole frame buffer) folded as its 8 little-endian bytes (a chain
//!   rather than one stream over all pixels, so a mismatch can be located
//!   against the `headless` per-frame lines; calls without a frame, LCD
//!   off, add nothing); `feed_frames`/`feed` are the count and
//!   [`FeedHash`] of every frame pushed into the ring (producer
//!   side). With no overrun that is every core sample of the run, and
//!   `feed` equals [`dac_feed_hash`] of all of them in order.
//! - `drain` (printed once the consumer has popped every pushed frame):
//!   the [`FeedTap`]'s count and hash, i.e. what the consumer (on the
//!   board, the DMA ISR) actually handed to the DAC, underrun filler
//!   excluded. Compared: with no overrun it equals the `end` fields, at
//!   any underrun count, so a board/host difference here means frames were
//!   lost or reordered between the ring and the DMA buffer.
//! - `core`: [`CycleClock`] counts around each `run_frame` call alone.
//!   `frontend`: around the conditioning + hash + ring push (`push`, mean
//!   over all calls), the frame hash (`hash`) and the sink (`sink`), the
//!   last two over the `FRAME_READY` calls only (0 if none).
//! - `idle`: [`Idle::idle`] calls while producing, and while draining.
//! - `ring`: [`RingStats`] for the producing part: the counters' increase
//!   from the start of [`run`] to the `end` line, and the fill watermarks
//!   (reset at the start; `min_fill` is 4294967295 if nothing was popped).
//!   The acceptance's "0 underruns" is this line's `underrun_events`.
//! - `drain`: the counters' increase while draining. The last pop of the
//!   drain normally finds fewer frames than a DMA half and counts one
//!   underrun event here, by construction; it is not a playback fault.
//! - `speed`: `elapsed` is the [`CycleClock`] counts from the start of the
//!   run to the `end` line (summed over wraps: the clock is read at least
//!   once per call and per idle). `ppm` is the emulated speed's deviation
//!   from real time, `(clock / CLOCK_HZ) / (elapsed / hz) - 1` in parts
//!   per million, rounded (integer math). It includes the initial ring
//!   fill (up to [`RING_FRAMES`] samples ahead: at most +104 ppm over
//!   10 minutes); `steady_ppm` measures from the first idle call instead
//!   (the ring is full at both ends, so the bias is under one call's 550
//!   samples), and is `-` if the loop never idled. On the host the
//!   consumer is not real-time, so the figure is meaningless there.
//!
//! If the run cannot start, a `paced error <reason>` line follows the
//! header: the [`crate::headless`] reasons, or `not_fresh fill=<f>
//! tap_frames=<t>` when the ring or the tap already held frames.
//!
//! # Board use
//!
//! - `RING: FeedRing` and `TAP: FeedTap` are statics. The DMA ISR is
//!   `audio_dac::on_dma(|_, out| { TAP.pop_to(&RING, out, |_| {}); })`.
//! - Start the DAC (`AudioDac::start`) before [`run`]: its 250 ms ramp
//!   comes before the first pop, and the loop fills the ring within the
//!   first four calls. Started right before [`run`], the ramp counts in
//!   `elapsed` with nothing consumed (-417 ppm over 10 minutes), so the
//!   `paced` image waits until the ramp is four refills from its end
//!   (src/bin/paced.rs).
//! - [`Idle`] is `WFI`. With a DWT [`CycleClock`] the CPU clock must keep
//!   running in Sleep (H7: DBGMCU_CR DBGSLEEP_D1), or the counter stops
//!   during `WFI` and the speed figures count only the busy time
//!   (docs/reference/audio-dac.md). The loop hands [`Idle::idle`] its
//!   wait condition so the board can re-check it with interrupts masked
//!   right before `WFI` (`cpsid i`, check, `wfi`, `cpsie i`: WFI wakes on
//!   a pending interrupt even with PRIMASK set, Armv7-M B1.5.19); without
//!   that, a refill landing between the loop's check and the `WFI` would
//!   delay the loop by up to one DMA half (7.8 ms) or until any other
//!   interrupt (M4 review P-4).
//! - [`run`] drains itself: after the `end` line it calls [`Idle`] until
//!   the tap has popped every frame pushed, then prints `drain` and the
//!   stats. The DAC must still be playing (stop it after `run` returns);
//!   with a stopped consumer the drain never ends.
//!
//! On the host (`tools/headless --paced`), [`Idle`] pops one DMA half
//! (256 frames, the H753's `audio_dac::HALF`) through a tap, standing in
//! for the ISR.
//!
//! [`Conditioner`]: crate::audio::Conditioner
//! [`FeedHash`]: crate::audio::FeedHash
//! [`FeedTap`]: crate::audio::FeedTap
//! [`dac_feed_hash`]: crate::audio::dac_feed_hash
//! [`RING_FRAMES`]: crate::audio::RING_FRAMES

use core::fmt::Write;

use gbc_core::{Buttons, CLOCK_HZ, Emulator, Events};

use crate::CycleClock;
use crate::audio::{AudioRing, Conditioner, FeedHash, FeedTap, PcmFrame, RingStats};
use crate::fnv::{self, Fnv1a};
use crate::headless::{self, Buffers, CycleStats, FrameSink, MIN_AUDIO_SLOTS};
use crate::input::{InputSource, Script};

/// The board's wait for the consumer (`WFI` on the MCU).
pub trait Idle {
    /// Called when the ring has no room for another call's samples, and
    /// while draining. Returns when the consumer may have popped (or at
    /// once: the loop just checks again). `waiting` is the loop's
    /// condition (true: still no room, or still draining); a board that
    /// sleeps checks it with interrupts masked right before sleeping, so
    /// a pop between the loop's check and the sleep cannot be missed
    /// (module docs, "Board use"; M4 review P-4).
    fn idle(&mut self, waiting: &dyn Fn() -> bool);
}

/// The board's per-frame hook for [`run_with`], called between frames: at
/// the top of every iteration, once the ring has room and right before the
/// frame's input is sampled (so for frame 0 before the first run call).
/// The player loop polls its UART here: PC input frames (into
/// [`crate::buttons::Shared`]), save loads ([`crate::save::reload`]) and
/// dump requests ([`crate::save::take`]).
pub trait Service {
    /// `frame` is the index of the run call about to happen. The hook must
    /// not change the buttons (the loop applies them from its
    /// [`InputSource`]); if it ejected and reloaded the cartridge it
    /// returns [`Serviced::Reloaded`], so the loop re-applies the held
    /// buttons the eject cleared and keeps its speed figures right across
    /// the clock restart.
    fn service(&mut self, frame: u32, emu: &mut Emulator<'_>) -> Serviced;

    /// Called at the end of every iteration, after call `frame`'s samples
    /// are in the ring and its frame (if any) went to the sink, with that
    /// call's [`CycleClock`] counts: the player's statistics. Changes
    /// nothing in the run (the default does nothing).
    #[inline]
    fn frame_done(&mut self, _frame: u32, _cycles: FrameCycles) {}

    /// Whether the run hashes what it produced: the frame hash (`fbs`,
    /// every completed frame) and the DAC feed hash (`feed`, every push).
    /// On (the default) for every run whose `paced` lines are compared;
    /// the live player, which discards its lines, turns it off (the frame
    /// hash alone is 2.3% of a frame on the board, docs/perf.md "Paced
    /// loop (M4)"). Off, [`Report::fbs`] and [`Report::feed`] stay the hash
    /// of nothing and [`FrameCycles::hash`] measures an empty interval;
    /// every other effect of the run is the same. A constant, so the
    /// branches compile out.
    const HASHES: bool = true;
}

/// One iteration's [`CycleClock`] counts (as the `stats paced core` and
/// `frontend` lines measure them), for [`Service::frame_done`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameCycles {
    /// Around the `run_frame` call alone.
    pub core: u32,
    /// Conditioning, feed hash and ring push.
    pub push: u32,
    /// The frame hash and the sink, 0 when the call completed no frame.
    pub hash: u32,
    pub sink: u32,
}

/// What a [`Service`] call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub enum Serviced {
    /// Nothing the loop needs to know.
    Continue,
    /// The cartridge was ejected and loaded again (clock back to 0).
    Reloaded,
}

/// No per-frame work: what [`run`] uses.
pub struct NoService;

impl Service for NoService {
    #[inline]
    fn service(&mut self, _: u32, _: &mut Emulator<'_>) -> Serviced {
        Serviced::Continue
    }
}

/// The board's hooks for [`run`].
pub struct Hooks<'h, C: ?Sized, S: ?Sized, I: ?Sized> {
    /// For the `stats` lines only.
    pub clock: &'h C,
    pub sink: &'h mut S,
    pub idle: &'h mut I,
}

/// The ring the loop fills and the tap its consumer pops through. Both
/// must be fresh (nothing pushed or popped yet).
pub struct Feed<'f, const N: usize> {
    pub ring: &'f AudioRing<N>,
    pub tap: &'f FeedTap,
}

/// Why a run did not start, or stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// As in [`crate::headless`] (`Write`: the output sink failed).
    Run(headless::Error),
    /// The ring or the tap already held frames.
    NotFresh { fill: usize, tap_frames: u32 },
}

impl From<core::fmt::Error> for Error {
    fn from(_: core::fmt::Error) -> Self {
        Self::Run(headless::Error::Write)
    }
}

impl From<headless::Error> for Error {
    fn from(e: headless::Error) -> Self {
        Self::Run(e)
    }
}

/// What a finished run printed, as values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Report {
    pub frames: u32,
    /// `Emulator::clock` at the end.
    pub clock: u64,
    /// Emulated clocks over the whole run: `clock` plus the clock before
    /// each [`Serviced::Reloaded`] (equal to `clock` without reloads). The
    /// `speed` line uses it.
    pub emulated: u64,
    /// Calls that returned `FRAME_READY`.
    pub ready: u32,
    /// The `fbs=` chain.
    pub fbs: u64,
    /// Frames pushed into the ring, and their [`FeedHash`].
    pub feed_frames: u64,
    pub feed: u64,
    /// The tap's count and hash after the drain.
    pub drained_frames: u32,
    pub drained_feed: u64,
    pub core: CycleStats,
    pub push: CycleStats,
    pub hash: CycleStats,
    pub sink: CycleStats,
    pub idle_calls: u64,
    pub drain_idle_calls: u64,
    /// The producing part (module docs, `ring`).
    pub ring: RingStats,
    /// Counter increases while draining (watermarks not used).
    pub drain: RingStats,
    /// Clock counts from the start to the `end` line.
    pub elapsed: u64,
    /// `(emulated clocks, clock counts)` from the first idle call to the
    /// `end` line, if the loop idled.
    pub steady: Option<(u64, u64)>,
    pub hz: u32,
}

/// Packed frames converted per ring push (a stack buffer of 256 B).
const CHUNK: usize = 64;

/// Runs `rom` for `frames` calls with `input`, paced by `feed.ring`, and
/// prints the lines (module docs) to `out`.
pub fn run<C, S, I, W, const N: usize>(
    rom: &[u8],
    input: Script<'_>,
    frames: u32,
    bufs: Buffers<'_>,
    feed: Feed<'_, N>,
    hooks: Hooks<'_, C, S, I>,
    out: &mut W,
) -> Result<Report, Error>
where
    C: CycleClock + ?Sized,
    S: FrameSink + ?Sized,
    I: Idle + ?Sized,
    W: Write + ?Sized,
{
    let mut input = input;
    run_with(
        rom,
        &mut input,
        frames,
        bufs,
        feed,
        hooks,
        &mut NoService,
        out,
    )
}

/// [`run`] with any [`InputSource`] and a per-frame [`Service`] hook (the
/// player loop, M5). With a [`Script`] and [`NoService`] it is [`run`]:
/// the same calls on the emulator in the same order, so the same lines.
#[allow(clippy::too_many_arguments)]
pub fn run_with<C, S, I, In, V, W, const N: usize>(
    rom: &[u8],
    input: &mut In,
    frames: u32,
    bufs: Buffers<'_>,
    feed: Feed<'_, N>,
    hooks: Hooks<'_, C, S, I>,
    service: &mut V,
    out: &mut W,
) -> Result<Report, Error>
where
    C: CycleClock + ?Sized,
    S: FrameSink + ?Sized,
    I: Idle + ?Sized,
    In: InputSource + ?Sized,
    V: Service + ?Sized,
    W: Write + ?Sized,
{
    // The pacing rule is only sound if the ring can hold a call's samples.
    const { assert!(N >= MIN_AUDIO_SLOTS) };
    let Feed { ring, tap } = feed;
    let Hooks { clock, sink, idle } = hooks;
    writeln!(
        out,
        "paced v1 rom_bytes={} rom_fnv={:016x} input_fnv={:016x} frames={frames}",
        rom.len(),
        fnv::hash(rom),
        input.fnv(),
    )?;
    let (fill, tap_frames) = (ring.fill(), tap.frames());
    if fill != 0 || tap_frames != 0 {
        writeln!(
            out,
            "paced error not_fresh fill={fill} tap_frames={tap_frames}"
        )?;
        return Err(Error::NotFresh { fill, tap_frames });
    }
    let (mut emu, frame, audio) = match headless::start(rom, bufs) {
        Ok(s) => s,
        Err(e) => {
            headless::error_line(out, "paced", e)?;
            return Err(e.into());
        }
    };
    // Never more than one call's worth of slots, so a call can't produce
    // more than the room the pacing rule checked (gbc-core
    // AUDIO_SAMPLE_RATE docs: 550 never fills, so no call ends early).
    let audio = &mut audio[..MIN_AUDIO_SLOTS];

    let s0 = ring.stats();
    ring.reset_watermarks();
    let mut time = Elapsed::new(clock.now());
    let mut cond = Conditioner::new();
    let mut feed_h = FeedHash::new();
    let mut feed_frames = 0u64;
    let mut fbs = Fnv1a::new();
    let mut ready = 0u32;
    let mut held = Buttons::EMPTY;
    let mut idle_calls = 0u64;
    let mut steady_from = None;
    // Emulated clocks before the last reload (the clock restarts at 0).
    let mut clock_base = 0u64;
    let (mut core, mut push, mut hash, mut sinkc) = (
        CycleStats::EMPTY,
        CycleStats::EMPTY,
        CycleStats::EMPTY,
        CycleStats::EMPTY,
    );
    let mut words = [PcmFrame::SILENCE; CHUNK];
    for n in 0..frames {
        while ring.free() < MIN_AUDIO_SLOTS {
            if steady_from.is_none() {
                steady_from = Some((clock_base + emu.clock(), time.total));
            }
            idle.idle(&|| ring.free() < MIN_AUDIO_SLOTS);
            idle_calls += 1;
            time.tick(clock.now());
        }
        let before = emu.clock();
        if service.service(n, &mut emu) == Serviced::Reloaded {
            clock_base += before;
            // `eject` released every button.
            held = Buttons::EMPTY;
        }
        // A frame-sampled source applies its set with `set_buttons`, which
        // between calls is `set_buttons_at` the frame's start clock
        // (gbc-core); a clock script schedules its changes at their own
        // clocks ([`InputSource::apply`]).
        input.apply(n, &mut emu, &mut held);
        let t0 = clock.now();
        let r = emu.run_frame(frame, audio);
        let t1 = clock.now();
        core.add(t1.wrapping_sub(t0), n);

        let got = audio.get(..r.audio_samples).unwrap_or(&[]);
        for chunk in got.chunks(CHUNK) {
            let words = &mut words[..chunk.len()];
            for (w, &s) in words.iter_mut().zip(chunk) {
                *w = cond.frame(s);
            }
            // Never short under the pacing rule; if it were, only what
            // went in is hashed (and the overrun is counted).
            let k = ring.push_frames(words);
            if V::HASHES {
                feed_h.fold_frames(&words[..k]);
            }
            feed_frames += k as u64;
        }
        let t2 = clock.now();
        push.add(t2.wrapping_sub(t1), n);

        let mut fc = FrameCycles {
            core: t1.wrapping_sub(t0),
            push: t2.wrapping_sub(t1),
            ..FrameCycles::default()
        };
        if r.events.contains(Events::FRAME_READY) {
            if V::HASHES {
                let mut fb = Fnv1a::new();
                fb.frame(frame);
                fbs.u64(fb.get());
            }
            ready += 1;
            let t3 = clock.now();
            sink.frame(n, frame);
            let t4 = clock.now();
            hash.add(t3.wrapping_sub(t2), n);
            sinkc.add(t4.wrapping_sub(t3), n);
            fc.hash = t3.wrapping_sub(t2);
            fc.sink = t4.wrapping_sub(t3);
            time.tick(t4);
        } else {
            time.tick(t2);
        }
        service.frame_done(n, fc);
    }
    let s1 = ring.stats();
    let elapsed = time.total;
    let emulated = clock_base + emu.clock();
    let steady = steady_from.map(|(c, e)| (emulated - c, elapsed - e));
    writeln!(
        out,
        "paced end frames={frames} clock={} ready={ready} fbs={:016x} feed_frames={feed_frames} feed={:016x}",
        emu.clock(),
        fbs.get(),
        feed_h.get(),
    )?;

    // Drain: wait until the consumer has popped every pushed frame.
    let mut drain_idle_calls = 0u64;
    let draining = || u64::from(tap.frames()) != feed_frames & u64::from(u32::MAX);
    while draining() {
        idle.idle(&draining);
        drain_idle_calls += 1;
    }
    let s2 = ring.stats();
    let (drained_frames, drained_feed) = (tap.frames(), tap.hash());
    writeln!(
        out,
        "paced drain feed_frames={drained_frames} feed={drained_feed:016x}"
    )?;

    let report = Report {
        frames,
        clock: emu.clock(),
        emulated,
        ready,
        fbs: fbs.get(),
        feed_frames,
        feed: feed_h.get(),
        drained_frames,
        drained_feed,
        core: core.finish(),
        push: push.finish(),
        hash: hash.finish(),
        sink: sinkc.finish(),
        idle_calls,
        drain_idle_calls,
        ring: RingStats {
            min_fill: s1.min_fill,
            max_fill: s1.max_fill,
            ..counters_since(&s0, &s1)
        },
        drain: counters_since(&s1, &s2),
        elapsed,
        steady,
        hz: clock.hz(),
    };
    stats_lines(out, &report)?;
    Ok(report)
}

/// The counters' increase from `a` to `b` (watermarks zero).
fn counters_since(a: &RingStats, b: &RingStats) -> RingStats {
    RingStats {
        underrun_events: b.underrun_events.wrapping_sub(a.underrun_events),
        underrun_frames: b.underrun_frames.wrapping_sub(a.underrun_frames),
        overrun_events: b.overrun_events.wrapping_sub(a.overrun_events),
        overrun_frames: b.overrun_frames.wrapping_sub(a.overrun_frames),
        min_fill: 0,
        max_fill: 0,
    }
}

/// A `u32` wrapping clock summed into a `u64` (exact while reads are less
/// than one wrap apart: 9.1 s at 472 MHz, 4.3 s for host nanoseconds).
struct Elapsed {
    last: u32,
    total: u64,
}

impl Elapsed {
    fn new(now: u32) -> Self {
        Self {
            last: now,
            total: 0,
        }
    }

    fn tick(&mut self, now: u32) {
        self.total += u64::from(now.wrapping_sub(self.last));
        self.last = now;
    }
}

impl CycleStats {
    const EMPTY: Self = Self {
        min: u32::MAX,
        max: 0,
        max_frame: 0,
        sum: 0,
    };

    fn add(&mut self, dt: u32, n: u32) {
        self.min = self.min.min(dt);
        if dt > self.max {
            self.max = dt;
            self.max_frame = n;
        }
        self.sum += u64::from(dt);
    }

    /// `min` is 0 rather than `u32::MAX` if nothing was added.
    fn finish(mut self) -> Self {
        if self.min == u32::MAX && self.sum == 0 {
            self.min = 0;
        }
        self
    }
}

/// `(clocks / CLOCK_HZ) / (cycles / hz) - 1` in ppm, rounded; `None` if
/// either rate is unknown.
pub fn speed_ppm(clocks: u64, cycles: u64, hz: u32) -> Option<i64> {
    if cycles == 0 || hz == 0 {
        return None;
    }
    let num = u128::from(clocks) * u128::from(hz) * 1_000_000;
    let den = u128::from(CLOCK_HZ) * u128::from(cycles);
    let q = (num + den / 2) / den;
    Some(i64::try_from(q).unwrap_or(i64::MAX) - 1_000_000)
}

fn stats_lines<W: Write + ?Sized>(out: &mut W, r: &Report) -> Result<(), Error> {
    let mean = |c: &CycleStats, n: u32| c.sum / u64::from(n.max(1));
    writeln!(
        out,
        "stats paced core min={} mean={} max={} max_frame={} hz={}",
        r.core.min,
        mean(&r.core, r.frames),
        r.core.max,
        r.core.max_frame,
        r.hz
    )?;
    writeln!(
        out,
        "stats paced frontend push_mean={} push_max={} hash_mean={} hash_max={} sink_mean={} sink_max={}",
        mean(&r.push, r.frames),
        r.push.max,
        mean(&r.hash, r.ready),
        r.hash.max,
        mean(&r.sink, r.ready),
        r.sink.max,
    )?;
    writeln!(
        out,
        "stats paced idle calls={} drain_calls={}",
        r.idle_calls, r.drain_idle_calls
    )?;
    let g = &r.ring;
    writeln!(
        out,
        "stats paced ring min_fill={} max_fill={} underrun_events={} underrun_frames={} overrun_events={} overrun_frames={}",
        g.min_fill,
        g.max_fill,
        g.underrun_events,
        g.underrun_frames,
        g.overrun_events,
        g.overrun_frames
    )?;
    writeln!(
        out,
        "stats paced drain underrun_events={} underrun_frames={}",
        r.drain.underrun_events, r.drain.underrun_frames
    )?;
    if let Some(ppm) = speed_ppm(r.emulated, r.elapsed, r.hz) {
        write!(
            out,
            "stats paced speed elapsed={} ppm={ppm:+} steady_ppm=",
            r.elapsed
        )?;
        match r.steady.and_then(|(c, e)| speed_ppm(c, e, r.hz)) {
            Some(s) => writeln!(out, "{s:+}")?,
            None => writeln!(out, "-")?,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_ppm_integer_math() {
        // Exactly real time: one second of clocks in one second of cycles.
        assert_eq!(
            speed_ppm(u64::from(CLOCK_HZ), 472_000_000, 472_000_000),
            Some(0)
        );
        // 0.1% fast and slow.
        assert_eq!(
            speed_ppm(u64::from(CLOCK_HZ) * 1001, 472_000_000_000, 472_000_000),
            Some(1000)
        );
        assert_eq!(
            speed_ppm(u64::from(CLOCK_HZ) * 999, 472_000_000_000, 472_000_000),
            Some(-1000)
        );
        // 10 minutes at 472 MHz doesn't overflow.
        let clocks = 35_837u64 * 70_224;
        let cycles = clocks * 472_000_000 / u64::from(CLOCK_HZ);
        assert_eq!(speed_ppm(clocks, cycles, 472_000_000), Some(0));
        assert_eq!(speed_ppm(1, 0, 1), None);
        assert_eq!(speed_ppm(1, 1, 0), None);
    }

    #[test]
    fn elapsed_sums_across_wraps() {
        let mut e = Elapsed::new(u32::MAX - 10);
        e.tick(5);
        e.tick(u32::MAX);
        e.tick(1);
        assert_eq!(e.total, 16 + (u64::from(u32::MAX) - 5) + 2);
    }
}
