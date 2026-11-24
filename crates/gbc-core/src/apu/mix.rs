//! The output pipeline: the window integrator and the NR50/NR51 mixer
//! (ADR 0021 sections 8-9).
//!
//! Each output sample is the exact average of the mixed signal over its
//! `SAMPLE_PERIOD`-clock window (a box filter). The channel outputs are
//! piecewise constant between syncs, so a window is a sum of
//! `level * duration` segments, integer only.

use super::{Apu, SAMPLE_PERIOD, SAMPLE_PERIOD_LOG2, StereoSample};

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
    /// Clocks into the current window at `synced` (`< SAMPLE_PERIOD`).
    pub(crate) pos: u8,
    /// The last sync integrated. Once an advance-only stretch (empty
    /// buffer) ends, the window grid restarts at `synced` with `acc = 0`.
    pub(crate) live: bool,
}

impl Output {
    pub(crate) const fn new() -> Self {
        Self {
            synced: 0,
            acc: [0; 2],
            pos: 0,
            live: false,
        }
    }
}

/// The spans integrated per batch: the channels are called once per batch,
/// with their per-span setup (period, duty, wave RAM words) done once.
/// 16 spans: 128 B of level sums on `Apu::flush`'s stack, cleared per
/// batch (9 x86 instructions; with the lanes in `Apu` instead the clear
/// would go, for under 0.1% of any bench). `i16`: a lane is one channel's
/// level times clocks over one span, at most 15 x `SAMPLE_PERIOD` = 1,920
/// in magnitude.
pub(crate) const BATCH: usize = 16;
const _: () = assert!(15 * SAMPLE_PERIOD <= i16::MAX as u64);
// `Spans::ends` and `Spans::set` have a bit per span.
const _: () = assert!(BATCH <= 16);
// `Spans::clocks` counts a full list.
const _: () = assert!(BATCH as u64 * SAMPLE_PERIOD <= u16::MAX as u64);

/// The spans listed for the next batch: the stretch of the output from
/// the last clock mixed to `Output::synced`, cut at every sample window's
/// end and wherever NR50 or NR51 was written. Listing is all a mixer
/// write needs: the channels' waveforms do not depend on the mixer, so
/// they integrate the whole list in one go later (`Apu::flush`), and the
/// weights are applied per span there. Empty between run calls, and
/// whenever the caller passes no buffer.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug))]
pub(crate) struct Spans {
    /// Each listed span's clocks (1 to `SAMPLE_PERIOD`).
    segs: [u8; BATCH],
    /// NR50 and NR51 from span `i` on, where `set` has bit `i` (and for
    /// span 0).
    vol: [[u8; 2]; BATCH],
    /// Bit `i`: NR50 or NR51 was written just before span `i`.
    set: u16,
    /// Bit `i`: span `i` ends its sample window.
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

/// A completed window's sum as a sample: the average level times 64, so
/// the extreme (4 channels x 15 x volume 8 = 480) maps to 30,720, within
/// `i16` without clamping.
#[inline]
pub(crate) fn sample(acc: i32) -> i16 {
    // `(acc << 6) >> SAMPLE_PERIOD_LOG2` as one shift: |acc| <= 480 *
    // SAMPLE_PERIOD, far from overflowing either way.
    if SAMPLE_PERIOD_LOG2 >= 6 {
        (acc >> (SAMPLE_PERIOD_LOG2 - 6)) as i16
    } else {
        (acc << (6 - SAMPLE_PERIOD_LOG2)) as i16
    }
}

impl Apu {
    /// Brings the state from `out.synced` to `now`. With a buffer
    /// (`audio` non-empty: the run call started with room), the span is
    /// integrated and one sample is written at `audio[*len]` per completed
    /// window while slots remain. A window completing with none left would
    /// be dropped, but none does: `AUDIO_BUFFER_FULL` ends the call within
    /// one window of the last slot's end (the rest of an instruction, or
    /// at once inside a VRAM DMA stall: `Ctx::vdma_stall`).
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
                sp.segs = [SAMPLE_PERIOD as u8; BATCH];
                sp.vol[0] = [io[0x24], io[0x25]];
            }
            // As many whole windows as are left and fit, or the rest of
            // the current window, or of the time.
            let (n, seg) = if pos == 0 && dt >= SAMPLE_PERIOD {
                let n = (dt >> SAMPLE_PERIOD_LOG2).min((BATCH - k) as u64);
                (n as usize, SAMPLE_PERIOD)
            } else {
                let seg = dt.min(SAMPLE_PERIOD - pos);
                if let Some(s) = sp.segs.get_mut(k) {
                    *s = seg as u8;
                }
                (1, seg)
            };
            dt -= n as u64 * seg;
            pos = (pos + seg) % SAMPLE_PERIOD;
            if pos == 0 {
                sp.ends |= (((1u32 << n) - 1) << k) as u16;
            }
            sp.len = (k + n) as u8;
            sp.clocks += (n as u64 * seg) as u16;
        }
        self.out.pos = pos as u8;
    }

    /// Integrates the listed spans in every channel and mixes them into
    /// the output, each with the NR50/NR51 weights in force over it: a
    /// sample is written at `audio[*len]` for every span that ends its
    /// window. The list is empty afterwards and the channels are at
    /// `out.synced`.
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
        // Into the windows: a sample where a span ends one.
        let [mut acc_l, mut acc_r] = self.out.acc;
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
            if let Some(slot) = audio.get_mut(n) {
                *slot = StereoSample {
                    left: sample(acc_l),
                    right: sample(acc_r),
                };
                n += 1;
            }
            (acc_l, acc_r) = (0, 0);
        }
        self.out.acc = [acc_l, acc_r];
        *len = n;
        self.spans.len = 0;
        self.spans.clocks = 0;
        self.spans.set = 0;
        self.spans.ends = 0;
    }

    /// Starts integrating: after an advance-only stretch, the window grid
    /// restarts at `synced` with nothing accumulated.
    #[inline]
    fn start_output(&mut self) {
        if !self.out.live {
            self.out.live = true;
            self.out.pos = 0;
            self.out.acc = [0; 2];
        }
    }

    /// At the start of a run call with `room` free slots (> 0), synced to
    /// the call's start: the clock where the last slot's window ends (the
    /// `AudioBuffer` deadline).
    pub(crate) fn buffer_deadline(&mut self, room: usize) -> u64 {
        self.start_output();
        let first = SAMPLE_PERIOD - u64::from(self.out.pos);
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
    fn a_window_sum_maps_to_the_average_level_times_64() {
        let p = SAMPLE_PERIOD as i32;
        assert_eq!(sample(480 * p), 30_720);
        assert_eq!(sample(-480 * p), -30_720);
        assert_eq!(sample(p), 64);
        assert_eq!(sample(0), 0);
    }
}
