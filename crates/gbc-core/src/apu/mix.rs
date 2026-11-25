//! The output pipeline: the window integrator, the NR50/NR51 mixer and
//! the decimation filter (ADR 0021 sections 8-9, ADR 0035).
//!
//! Two stages take the mixed signal from the clock rate to the output
//! rate. The first is the exact sum of the mixed signal over each
//! `WINDOW`-clock window (a box filter to 65,536 Hz): the channel outputs
//! are piecewise constant between syncs, so a window is a sum of
//! `level * duration` segments, integer only. The second is an 8-tap FIR
//! over the window sums that keeps every other result (`FIR`): one output
//! sample per two windows.

use super::{Apu, SAMPLE_PERIOD, StereoSample, WINDOW, WINDOW_LOG2};

/// The second stage: a low-pass over the last 8 window sums, evaluated
/// once per two windows (2:1 decimation, 65,536 to 32,768 Hz). The taps
/// are symmetric (linear phase), so half of them are stored: tap `i` and
/// tap `7 - i` are `FIR[i]`. A Kaiser-windowed sinc (beta 3) with its
/// -6 dB point at 16,384 Hz, scaled to a sum of 1,024 and rounded (the
/// rounded taps add up to exactly that): the DC gain is 1 after the
/// shift by `FIR_SHIFT`, and a constant input comes out exact.
/// Measured: `docs/reference/audio-aliasing.md`.
///
/// It delays the stream by 1.5 samples against the plain average it
/// replaced: an output sample is centred 256 clocks before the end of its
/// last window (the kernel covers 512 clocks), where the 128-clock
/// average was centred 64 before.
pub(crate) const FIR: [i32; 4] = [-22, -56, 131, 459];

/// `log2` of the sum of all 8 taps.
const FIR_SHIFT: u32 = 10;
const _: () = assert!(2 * (FIR[0] + FIR[1] + FIR[2] + FIR[3]) == 1 << FIR_SHIFT);

/// A window sum's largest magnitude: the mix's extreme (4 channels x 15 x
/// volume 8 = 480) over a whole window, 30,720. `Output::hist` holds the
/// sums as `i16`, and the scale needs no shift: a window sum is the
/// average level times 64, which is the sample scale.
const WINDOW_MAX: i32 = 480 * WINDOW as i32;
const _: () = assert!(WINDOW_MAX <= i16::MAX as i32 && WINDOW_LOG2 == 6);

/// The sum of the 8 taps' magnitudes: 1,336.
const FIR_ABS: i32 = 2 * (-FIR[0] - FIR[1] + FIR[2] + FIR[3]);
// The filter's sum cannot overflow `i32`: at most 1,336 x 30,720 =
// 41,041,920, whatever the inputs' signs (2% of the range).
const _: () = assert!(FIR_ABS as i64 * WINDOW_MAX as i64 <= (i32::MAX / 50) as i64);
// The result can leave `i16`, though: the negative taps overshoot, up to
// 30,720 x 1,336 / 1,024 = 40,080 for window sums that alternate between
// the extremes in step with the taps' signs. `fir` saturates.
const _: () = assert!((WINDOW_MAX * FIR_ABS) >> FIR_SHIFT > i16::MAX as i32);

/// `Output::half` when the next window sum is the stream's first: it then
/// fills the whole history. Even, as for any first window of a sample.
const PRIME: u8 = 2;

/// The output stream's position: kept across APU power cycles.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Output {
    /// The clock the output is accounted for up to: mixed, or listed in
    /// `Apu::spans`. The channels are exact at the start of the listed
    /// spans (`Spans::clocks` earlier), which is this clock whenever the
    /// list is empty.
    pub(crate) synced: u64,
    /// The current window's mixed sums so far (left, right), without the
    /// listed spans.
    pub(crate) acc: [i32; 2],
    /// The sums (left, right) of the 8 windows the next sample is made
    /// of, oldest first: the 6 before the sample's own two, then its
    /// first (once complete) and its second. Each is at most 30,720 in
    /// magnitude (`WINDOW_MAX`).
    pub(crate) hist: [[i16; 2]; 8],
    /// Clocks into the current window at `synced` (`< WINDOW`).
    pub(crate) pos: u8,
    /// 0 in the first window of an output sample, 1 in its second, or
    /// `PRIME`.
    pub(crate) half: u8,
    /// The last sync integrated. Once an advance-only stretch (empty
    /// buffer) ends, the window grid restarts at `synced` with `acc = 0`
    /// and no history (`PRIME`).
    pub(crate) live: bool,
}

impl Output {
    pub(crate) const fn new() -> Self {
        Self {
            synced: 0,
            acc: [0; 2],
            hist: [[0; 2]; 8],
            pos: 0,
            half: PRIME,
            live: false,
        }
    }
}

/// The spans integrated per batch: the channels are called once per batch,
/// with their per-span setup (period, duty, wave RAM words) done once.
/// 16 spans: 128 B of level sums on `Apu::flush`'s stack, cleared per
/// batch (9 x86 instructions; with the lanes in `Apu` instead the clear
/// would go, for under 0.1% of any bench). `i16`: a lane is one channel's
/// level times clocks over one span, at most 15 x `WINDOW` = 960 in
/// magnitude.
pub(crate) const BATCH: usize = 16;
const _: () = assert!(15 * WINDOW <= i16::MAX as u64);
// `Spans::ends` and `Spans::set` have a bit per span.
const _: () = assert!(BATCH <= 16);
// `Spans::clocks` counts a full list.
const _: () = assert!(BATCH as u64 * WINDOW <= u16::MAX as u64);

/// The spans listed for the next batch: the stretch of the output from
/// the last clock mixed to `Output::synced`, cut at every window's end
/// and wherever NR50 or NR51 was written. Listing is all a mixer
/// write needs: the channels' waveforms do not depend on the mixer, so
/// they integrate the whole list in one go later (`Apu::flush`), and the
/// weights are applied per span there. Empty between run calls, and
/// whenever the caller passes no buffer.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct Spans {
    /// Each listed span's clocks (1 to `WINDOW`).
    segs: [u8; BATCH],
    /// NR50 and NR51 from span `i` on, where `set` has bit `i` (and for
    /// span 0).
    vol: [[u8; 2]; BATCH],
    /// Bit `i`: NR50 or NR51 was written just before span `i`.
    set: u16,
    /// Bit `i`: span `i` ends its window.
    ends: u16,
    /// Spans listed, at most `BATCH`.
    len: u8,
    /// Their clocks in all: how far the channels are behind
    /// `Output::synced`.
    clocks: u16,
}

impl Spans {
    pub(crate) const fn new() -> Self {
        Self {
            segs: [0; BATCH],
            vol: [[0; 2]; BATCH],
            set: 0,
            ends: 0,
            len: 0,
            clocks: 0,
        }
    }

    /// The clocks listed: how far the channels are behind `Output::synced`.
    #[inline]
    pub(crate) fn clocks(&self) -> u64 {
        u64::from(self.clocks)
    }
}

/// Only the listed spans count: the entries past `len` are left over from
/// earlier batches.
#[cfg(test)]
impl PartialEq for Spans {
    fn eq(&self, other: &Self) -> bool {
        let n = usize::from(self.len);
        let key = |s: &Self| (s.len, s.clocks, s.set, s.ends);
        key(self) == key(other)
            && self.segs[..n] == other.segs[..n]
            && (0..n)
                .filter(|&i| i == 0 || self.set >> i & 1 != 0)
                .all(|i| self.vol[i] == other.vol[i])
    }
}

#[cfg(test)]
impl Eq for Spans {}

/// Adds `level` times each span's clocks to `out`: a channel whose DAC is
/// on while the channel is off (Pan Docs DAC: digital 0 -> +1). Out of
/// line: one copy for all channels, off their playing paths.
#[inline(never)]
pub(crate) fn constant(segs: &[u8], out: &mut [i16], level: i32) {
    for (o, &dt) in out.iter_mut().zip(segs) {
        *o += (level * i32::from(dt)) as i16;
    }
}

/// The mixer's weights per side (left, right) and channel: the volume plus
/// 1 for the channels panned to that side (NR51: bits 0-3 CH1-CH4 right,
/// 4-7 left; NR50: bits 0-2 right, 4-6 left; VIN ignored, no cartridge
/// audio), else 0. Pan Docs: Audio Registers, FF24/FF25.
#[inline]
pub(crate) fn weights(nr50: u8, nr51: u8) -> [[i32; 4]; 2] {
    let side = |pan: u8, vol: u8| {
        let vol = i32::from(vol & 7) + 1;
        core::array::from_fn(|ch| i32::from(pan >> ch & 1) * vol)
    };
    [side(nr51 >> 4, nr50 >> 4), side(nr51, nr50)]
}

/// The mixed sums of one span from the channel integrals. An explicit sum:
/// `w.map(..)` compiled to out-of-line `try_map`/`Drain` calls.
#[inline]
pub(crate) fn mix(levels: [i32; 4], w: &[[i32; 4]; 2]) -> [i32; 2] {
    let [a, b, c, d] = levels;
    let side = |s: &[i32; 4]| a * s[0] + b * s[1] + c * s[2] + d * s[3];
    [side(&w[0]), side(&w[1])]
}

/// One side (0 left, 1 right) of an output sample: the `FIR` over the 8
/// window sums in `hist`, oldest first. A window sum is the
/// average level times 64 and the taps add up to `1 << FIR_SHIFT`, so a
/// steady extreme (480) maps to 30,720, exactly. The sum fits `i32` and
/// the result is saturated to `i16` (see the bounds at `FIR`): only a mix
/// that swings over more than 81% of the full scale (32,767 / 40,080)
/// can get there, e.g. all four channels at master volume 7 stepping
/// from one extreme to the other at once (30,720 + 7.6% of 61,440).
#[inline]
pub(crate) fn fir(hist: &[[i16; 2]; 8], side: usize) -> i16 {
    let x = |i: usize| i32::from(hist[i][side]);
    let sum = FIR[0] * (x(0) + x(7))
        + FIR[1] * (x(1) + x(6))
        + FIR[2] * (x(2) + x(5))
        + FIR[3] * (x(3) + x(4));
    (sum >> FIR_SHIFT).clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

impl Apu {
    /// Brings the state from `out.synced` to `now`. With a buffer
    /// (`audio` non-empty: the run call started with room), the span is
    /// integrated and one sample is written at `audio[*len]` per two
    /// completed windows while slots remain. A sample completing with
    /// none left would be dropped, but none does: `AUDIO_BUFFER_FULL` ends
    /// the call within one window of the last slot's end (the rest of an
    /// instruction, or at once inside a VRAM DMA stall:
    /// `Ctx::vdma_stall`).
    /// With an empty buffer, the channels only advance (ADR 0021 section
    /// 9). `io` is `Memory::io`, for wave RAM and for NR50/NR51 (their
    /// writes come through `sync_write`).
    #[inline(never)]
    pub(crate) fn sync(
        &mut self,
        now: u64,
        io: &[u8; 0x80],
        audio: &mut [StereoSample],
        len: &mut usize,
    ) {
        self.list(now, io, audio, len);
        self.flush(io, audio, len);
    }

    /// The sync before a write of `value` to FF10-FF3F (`reg`, the low 7
    /// address bits) at clock `now`; the caller then stores the byte and
    /// applies the write. A channel register, wave RAM or NR52 changes
    /// what the channels do from here on, so they are brought to `now`
    /// (`sync`). NR50 and NR51 only change the mixer's weights: the spans
    /// up to `now` are listed with the old ones and the new ones are
    /// noted for the spans that follow (`Spans`). The samples are the
    /// same either way: a window's sum is linear in its spans.
    #[inline(never)]
    pub(crate) fn sync_write(
        &mut self,
        reg: usize,
        value: u8,
        now: u64,
        io: &[u8; 0x80],
        audio: &mut [StereoSample],
        len: &mut usize,
    ) {
        if !matches!(reg, 0x24 | 0x25) || !self.fs.power {
            return self.sync(now, io, audio, len);
        }
        self.list(now, io, audio, len);
        if usize::from(self.spans.len) == BATCH {
            self.flush(io, audio, len);
        }
        // With no span listed, the next one reads the registers itself.
        let k = usize::from(self.spans.len);
        if let (1.., Some(vol)) = (k, self.spans.vol.get_mut(k)) {
            *vol = [io[0x24], io[0x25]];
            vol[reg & 1] = value;
            self.spans.set |= 1 << k;
        }
    }

    /// Moves `out.synced` to `now`. With a buffer, the clocks in between
    /// are listed as spans (`Spans`), a full list being mixed on the way
    /// (`flush`); the channels stay where the list starts. With an empty
    /// buffer the channels advance and nothing is listed.
    #[inline]
    fn list(&mut self, now: u64, io: &[u8; 0x80], audio: &mut [StereoSample], len: &mut usize) {
        let Some(mut dt) = now.checked_sub(self.out.synced) else {
            return;
        };
        self.out.synced = now;
        if audio.is_empty() {
            // Nothing is listed here (a run call has a buffer or none, and
            // ends with a sync); if something were, the channels would
            // first have to catch up with it.
            self.flush(io, audio, len);
            self.advance(dt, io);
            self.out.live = false;
            return;
        }
        self.start_output();
        let mut pos = u64::from(self.out.pos);
        while dt != 0 {
            if usize::from(self.spans.len) == BATCH {
                self.flush(io, audio, len);
            }
            let sp = &mut self.spans;
            let k = usize::from(sp.len);
            if k == 0 {
                // A new list: whole windows until a span says otherwise.
                sp.segs = [WINDOW as u8; BATCH];
                sp.vol[0] = [io[0x24], io[0x25]];
            }
            // As many whole windows as are left and fit, or the rest of
            // the current window, or of the time.
            let (n, seg) = if pos == 0 && dt >= WINDOW {
                let n = (dt >> WINDOW_LOG2).min((BATCH - k) as u64);
                (n as usize, WINDOW)
            } else {
                let seg = dt.min(WINDOW - pos);
                if let Some(s) = sp.segs.get_mut(k) {
                    *s = seg as u8;
                }
                (1, seg)
            };
            dt -= n as u64 * seg;
            pos = (pos + seg) % WINDOW;
            if pos == 0 {
                sp.ends |= (((1u32 << n) - 1) << k) as u16;
            }
            sp.len = (k + n) as u8;
            sp.clocks += (n as u64 * seg) as u16;
        }
        self.out.pos = pos as u8;
    }

    /// Integrates the listed spans in every channel and mixes them into
    /// the output, each with the NR50/NR51 weights in force over it. A
    /// span that ends its window puts the window's sum in the history,
    /// and every second one writes a sample at `audio[*len]` (`fir`). The
    /// history is state like `acc`: the samples do not depend on where
    /// the batches, the syncs or the caller's buffers end. The list is
    /// empty afterwards and the channels are at `out.synced`.
    #[inline]
    fn flush(&mut self, io: &[u8; 0x80], audio: &mut [StereoSample], len: &mut usize) {
        let sp = self.spans;
        let k = usize::from(sp.len).min(BATCH);
        if k == 0 {
            return;
        }
        let mut lv = [[0; BATCH]; 4];
        self.integrate(&sp.segs[..k], io, &mut lv);
        // The mixed sums of each span, in runs under the same weights:
        // from span 0 or a mixer write to the next write.
        let mut mixed = [[0; 2]; BATCH];
        let mut from = 0;
        while from < k {
            let w = weights(sp.vol[from][0], sp.vol[from][1]);
            let later = u32::from(sp.set) >> (from + 1);
            let to = (from + 1 + later.trailing_zeros() as usize).min(k);
            for i in from..to {
                mixed[i] = mix(core::array::from_fn(|c| i32::from(lv[c][i])), &w);
            }
            from = to;
        }
        // Into the windows: where a span ends one, its sum joins the
        // history, and the second window of a sample filters it.
        let out = &mut self.out;
        let [mut acc_l, mut acc_r] = out.acc;
        let mut half = out.half;
        let mut n = *len;
        let mut ends = sp.ends;
        for m in &mixed[..k] {
            acc_l += m[0];
            acc_r += m[1];
            let end = ends & 1 != 0;
            ends >>= 1;
            if !end {
                continue;
            }
            // `as i16`: at most `WINDOW_MAX`.
            let sum = [acc_l as i16, acc_r as i16];
            (acc_l, acc_r) = (0, 0);
            if half != 1 {
                if half == PRIME {
                    // The stream's first window: as if the signal had
                    // always been at this level, so a steady level is
                    // steady from the first sample on (romtest judges
                    // silence by "all samples equal", ADR 0021 section
                    // 10).
                    out.hist = [sum; 8];
                }
                out.hist[6] = sum;
                half = 1;
                continue;
            }
            out.hist[7] = sum;
            half = 0;
            if let Some(slot) = audio.get_mut(n) {
                *slot = StereoSample {
                    left: fir(&out.hist, 0),
                    right: fir(&out.hist, 1),
                };
                n += 1;
            }
            // The next sample's 6 earlier windows.
            out.hist.copy_within(2.., 0);
        }
        out.acc = [acc_l, acc_r];
        out.half = half;
        *len = n;
        self.spans.len = 0;
        self.spans.clocks = 0;
        self.spans.set = 0;
        self.spans.ends = 0;
    }

    /// Starts integrating: after an advance-only stretch, the window grid
    /// restarts at `synced` with nothing accumulated and no history.
    #[inline]
    fn start_output(&mut self) {
        if !self.out.live {
            self.out.live = true;
            self.out.pos = 0;
            self.out.acc = [0; 2];
            self.out.half = PRIME;
        }
    }

    /// At the start of a run call with `room` free slots (> 0), synced to
    /// the call's start: the clock where the last slot's second window
    /// ends (the `AudioBuffer` deadline).
    pub(crate) fn buffer_deadline(&mut self, room: usize) -> u64 {
        self.start_output();
        // Into the current sample: its first window may be done.
        let pos = u64::from(self.out.half & 1) * WINDOW + u64::from(self.out.pos);
        let first = SAMPLE_PERIOD - pos;
        let rest = (room as u64 - 1).saturating_mul(SAMPLE_PERIOD);
        self.out.synced.saturating_add(first).saturating_add(rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixer_pans_and_scales_each_side() {
        let levels = [1, 2, 4, 8];
        // NR51: CH2 + CH4 left, CH1 + CH3 right; NR50: left 7 (x8), right 0 (x1).
        assert_eq!(mix(levels, &weights(0x70, 0xA5)), [(2 + 8) * 8, 1 + 4]);
        assert_eq!(mix(levels, &weights(0x03, 0x1F)), [1, 15 * 4]);
        // VIN bits (3, 7) don't scale.
        assert_eq!(mix(levels, &weights(0x88, 0xFF)), [15, 15]);
        assert_eq!(mix(levels, &weights(0x77, 0x00)), [0, 0]);
    }

    #[test]
    fn steady_window_sums_map_to_the_average_level_times_64() {
        let p = WINDOW as i16;
        for (level, want) in [(480, 30_720), (-480, -30_720), (1, 64), (0, 0)] {
            let hist = [[level * p, -level * p]; 8];
            assert_eq!(fir(&hist, 0), want);
            assert_eq!(fir(&hist, 1), -want);
        }
    }

    /// The taps as the filter applies them, oldest window first.
    const TAPS: [i32; 8] = [-22, -56, 131, 459, 459, 131, -56, -22];

    #[test]
    fn the_filter_weighs_each_window_by_its_tap() {
        // One window at 1,024 and the rest at 0: its tap.
        for (i, tap) in TAPS.iter().enumerate() {
            let mut hist = [[0i16; 2]; 8];
            hist[i] = [1024, -2048];
            assert_eq!(fir(&hist, 0), *tap as i16, "window {i}");
            assert_eq!(fir(&hist, 1), -2 * *tap as i16, "window {i}");
        }
    }

    #[test]
    fn the_filter_saturates_where_the_overshoot_leaves_i16() {
        let m = WINDOW_MAX as i16;
        // A full-scale step, three windows after it: the two negative
        // taps still see the old level. 30,720 + 61,440 x 78 / 1,024.
        let mut hist = [[m, -m]; 8];
        hist[0] = [-m, m];
        hist[1] = [-m, m];
        assert_eq!(30_720 + 61_440 * 78 / 1024, 35_400);
        assert_eq!(fir(&hist, 0), i16::MAX);
        assert_eq!(fir(&hist, 1), i16::MIN);
        // Three channels' extreme (360 of 480) never does, even in step
        // with the taps' signs: 23,040 x 1,336 / 1,024 = 30,060.
        let m = m / 4 * 3;
        let hist: [[i16; 2]; 8] =
            core::array::from_fn(|i| if TAPS[i] < 0 { [-m, m] } else { [m, -m] });
        assert_eq!(fir(&hist, 0), 30_060);
        assert_eq!(fir(&hist, 1), -30_060);
    }
}
