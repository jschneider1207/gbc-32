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
    /// The clock the APU state is exact at.
    pub(crate) synced: u64,
    /// The current window's mixed sums so far (left, right).
    pub(crate) acc: [i32; 2],
    /// Clocks into the current window (`< SAMPLE_PERIOD`).
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
/// 16 spans: 256 B of level sums on the stack.
pub(crate) const BATCH: usize = 16;

/// Adds `level` times each span's clocks to `out`: a channel whose DAC is
/// on while the channel is off (Pan Docs DAC: digital 0 -> +1). One copy
/// for all channels (code size).
#[inline(never)]
pub(crate) fn constant(segs: &[u8], out: &mut [i32], level: i32) {
    for (o, &dt) in out.iter_mut().zip(segs) {
        // Opaque: no unrolling (code size).
        *o += level * i32::from(core::hint::black_box(dt));
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

/// The mixed sums of one span from the channel integrals.
#[inline]
pub(crate) fn mix(levels: [i32; 4], w: &[[i32; 4]; 2]) -> [i32; 2] {
    w.map(|side| (0..4).map(|ch| levels[ch] * side[ch]).sum())
}

/// A completed window's sum as a sample: the average level times 64, so
/// the extreme (4 channels x 15 x volume 8 = 480) maps to 30,720, within
/// `i16` without clamping.
#[inline]
pub(crate) fn sample(acc: i32) -> i16 {
    ((acc << 6) >> SAMPLE_PERIOD_LOG2) as i16
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
    /// 9). `io` is `Memory::io`: NR50/NR51 are constant over the span (a
    /// write to them syncs first).
    #[inline(never)]
    pub(crate) fn sync(
        &mut self,
        now: u64,
        io: &[u8; 0x80],
        audio: &mut [StereoSample],
        len: &mut usize,
    ) {
        let Some(mut dt) = now.checked_sub(self.out.synced) else {
            return;
        };
        self.out.synced = now;
        if audio.is_empty() {
            self.advance(dt, io);
            self.out.live = false;
            return;
        }
        self.start_output();
        let w = weights(io[0x24], io[0x25]);
        while dt != 0 {
            // A batch: the rest of the current window, then whole windows,
            // the last one perhaps partial.
            let mut segs = [0u8; BATCH];
            let mut k = 0;
            let mut pos = u64::from(self.out.pos);
            while k < BATCH && dt != 0 {
                let seg = (SAMPLE_PERIOD - pos).min(dt);
                segs[k] = seg as u8;
                dt -= seg;
                pos = 0;
                k += 1;
            }
            let mut lv = [[0; BATCH]; 4];
            self.integrate(&segs[..k], io, &mut lv);
            for (i, &seg) in segs[..k].iter().enumerate() {
                // Opaque: no unrolling (code size).
                let seg = core::hint::black_box(seg);
                let sums = mix([lv[0][i], lv[1][i], lv[2][i], lv[3][i]], &w);
                self.out.acc[0] += sums[0];
                self.out.acc[1] += sums[1];
                let pos = u64::from(self.out.pos) + u64::from(seg);
                if pos < SAMPLE_PERIOD {
                    self.out.pos = pos as u8;
                    continue;
                }
                self.out.pos = 0;
                let [l, r] = core::mem::take(&mut self.out.acc);
                if let Some(slot) = audio.get_mut(*len) {
                    *slot = StereoSample {
                        left: sample(l),
                        right: sample(r),
                    };
                    *len += 1;
                }
            }
        }
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
