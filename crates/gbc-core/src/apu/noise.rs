//! The noise channel, CH4 (ADR 0021 section 5; Pan Docs "Audio Registers"
//! FF20-FF23 and "Audio details", noise channel; SameBoy `Core/apu.c`,
//! MIT). Timing conventions: docs/reference/apu.md.

use core::num::NonZeroU32;

use super::units::{Envelope, Length};

/// CH4's length counter range.
const LENGTH_MAX: u16 = 64;

/// The noise channel's state. The register bytes are in `Memory::io`.
/// All-zero is the power-off state.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Noise {
    /// Base clocks to the next LFSR step (>= 1 while on). With shift 14 or
    /// 15 the steps still come but leave the LFSR alone.
    pub(crate) countdown: u32,
    /// The 15-bit LFSR (bit 15 always 0). Bit 0 is the output.
    pub(crate) lfsr: u16,
    /// The last NR43 written: shift (bits 4-7), 7-bit mode (bit 3),
    /// divisor code `r` (bits 0-2). Read at each step, as in SameBoy.
    pub(crate) nr43: u8,
    /// The NR52 bit.
    pub(crate) on: bool,
    pub(crate) len: Length,
    pub(crate) env: Envelope,
}

/// `n` LFSR steps from `x` (15 bits) in 7-bit mode (`narrow`) or not, and
/// the number of steps after which bit 0 is 1.
///
/// A step writes `!(b0 ^ b1)` into bit 14, and in 7-bit mode also bit 6,
/// then shifts right (SameBoy `lfsr_feedback`/`shift_lfsr_with_feedback`,
/// masks 0x4000/0x4040, applied after the shift; Pan Docs' 16-bit picture
/// before the shift is the same machine). The first `k` steps read only
/// the original bits 0..=k as long as no fed-back bit has reached bit 1:
/// `k <= 14`, or `k <= 6` in 7-bit mode. So `k` steps are one word
/// operation: the feedback bits are `!(x ^ x >> 1)` masked to `k` bits,
/// step `m`'s bit lands at `15 - k + (m - 1)` (and `7 - k + (m - 1)` in
/// 7-bit mode), and bit 0 after step `m` is the original bit `m`. The
/// shortest period (8 clocks) makes 1024 steps per frame-sequencer span:
/// 74 word operations, or 171 in 7-bit mode.
#[inline]
fn lfsr_steps(mut x: u32, mut n: u32, narrow: bool) -> (u16, u32) {
    let chunk = if narrow { 6 } else { 14 };
    let mut ones = 0;
    while n != 0 {
        let k = n.min(chunk);
        let m = (1 << k) - 1;
        ones += (x >> 1 & m).count_ones();
        let f = !(x ^ x >> 1) & m;
        x = x >> k | f << (15 - k);
        if narrow {
            x = x & !(m << (7 - k)) | f << (7 - k);
        }
        n -= k;
    }
    (x as u16, ones)
}

/// `n` steps reduced to a short equivalent count. The 15-bit step is a
/// bijection whose cycles are 32,767 states long (x^15 + x^14 + 1 is
/// primitive) or the fixed point 0x7FFF, so `n mod 32767` is exact. In
/// 7-bit mode bit 7 is overwritten, so after 8 steps bits 7-14 are the
/// last 8 feedback bits and the state is a function of the 7-bit LFSR in
/// bits 0-6 (cycles of 127, or the fixed point 0x7F). Only long STOP
/// stretches reach the reduction.
#[inline]
fn reduce(n: u64, narrow: bool) -> u32 {
    let (base, cycle) = if narrow { (8, 127) } else { (0, 32_767) };
    if n < base + cycle {
        n as u32
    } else {
        (base + (n - base) % cycle) as u32
    }
}

impl Noise {
    pub(crate) const ZERO: Self = Self {
        countdown: 0,
        lfsr: 0,
        nr43: 0,
        on: false,
        len: Length::ZERO,
        env: Envelope::ZERO,
    };

    /// Base clocks per LFSR step: `(r == 0 ? 8 : 16 * r) << s` (Pan Docs:
    /// 262,144 / (r * 2^s) Hz with r = 0 as 0.5; SameBoy: a counter
    /// clocked every `4r` (r = 0: 2) 2 MHz ticks, the LFSR stepping on each
    /// rising edge of its bit `s`). At most 112 << 15, so u32.
    #[inline]
    fn period(&self) -> NonZeroU32 {
        let r = u32::from(self.nr43 & 7);
        let base = if r == 0 { 8 } else { 16 * r };
        NonZeroU32::new(base << (self.nr43 >> 4)).unwrap_or(NonZeroU32::MIN)
    }

    /// `n` steps of the LFSR; returns the number of steps after which bit
    /// 0 is 1. Shift 14 and 15 never clock it: SameBoy's divisor counter is
    /// 14 bits (`& 0x3FFF`), so those bits never rise; bit 0 just stays.
    #[inline(never)]
    fn clock(&mut self, n: u32) -> u32 {
        if self.nr43 >> 4 >= 14 {
            return n * u32::from(self.lfsr & 1);
        }
        let (x, ones) = lfsr_steps(u32::from(self.lfsr), n, self.nr43 & 8 != 0);
        self.lfsr = x;
        ones
    }

    /// The digital output, 0-15 (PCM34 high nibble): the volume while bit
    /// 0 of the LFSR is 1 and the channel is on (SameBoy
    /// `current_lfsr_sample ? current_volume : 0`).
    #[inline]
    pub(crate) fn digital(&self) -> u8 {
        if self.on && self.lfsr & 1 != 0 {
            self.env.volume
        } else {
            0
        }
    }

    /// The digital output `dt` clocks after the state (PCM34), from a copy.
    #[inline(never)]
    pub(crate) fn digital_at(&self, dt: u64) -> u8 {
        let mut ch = *self;
        ch.advance(dt);
        ch.digital()
    }

    /// Advances by `dt` clocks without integrating (ADR 0021 section 9).
    /// The LFSR takes the same steps as in `integrate`, so its state does
    /// not depend on whether a buffer was passed.
    #[inline(never)]
    pub(crate) fn advance(&mut self, dt: u64) {
        if !self.on {
            return;
        }
        let cd = u64::from(self.countdown);
        if dt < cd {
            self.countdown = (cd - dt) as u32;
            return;
        }
        let p = self.period().get();
        let rest = dt - cd;
        self.countdown = p - (rest % u64::from(p)) as u32;
        self.clock(reduce(rest / u64::from(p) + 1, self.nr43 & 8 != 0));
    }

    /// Adds to `out[i]` the analog level times clocks over the `i`-th of
    /// the consecutive spans `segs` (each at most one sample window, so at
    /// most 17 steps) and advances over them: `15 - 2 * digital` while the
    /// DAC is on (Pan Docs DAC), nothing while it is off. A batch per call:
    /// NR43 and the volume are constant over a sync span.
    #[inline(never)]
    pub(crate) fn integrate(&mut self, segs: &[u8], out: &mut [i32]) {
        if !self.env.dac() {
            return;
        }
        if !self.on {
            return super::mix::constant(segs, out, 15);
        }
        let spans = out.iter_mut().zip(segs);
        let vol2 = 2 * i32::from(self.env.volume);
        let p = self.period();
        for (o, &dt) in spans {
            let dt = u32::from(dt);
            // Opaque: stops LLVM's Cortex-M runtime unrolling (x4), which
            // would double the function's size (docs/perf.md, APU cost).
            let dt = core::hint::black_box(dt);
            // Clocks with bit 0 = 1: up to the first step at the old bit,
            // then `p` per step after which it is 1, minus the unelapsed
            // rest of the last step's period.
            let bit = u32::from(self.lfsr & 1);
            let cd = self.countdown;
            let high = if dt < cd {
                self.countdown = cd - dt;
                bit * dt
            } else {
                let rest = dt - cd;
                let left = p.get() - rest % p;
                let ones = self.clock(rest / p + 1);
                self.countdown = left;
                bit * cd + ones * p.get() - u32::from(self.lfsr & 1) * left
            };
            *o += 15 * dt as i32 - vol2 * high as i32;
        }
    }

    /// One span through `integrate` (tests).
    #[cfg(test)]
    pub(crate) fn integrate1(&mut self, dt: u32) -> i32 {
        let mut out = [0];
        self.integrate(&[u8::try_from(dt).unwrap()], &mut out);
        out[0]
    }

    /// A write to NR41-NR44 (`off` 1-4; others are ignored) with the frame
    /// sequencer at `fs`.
    #[inline(never)]
    pub(crate) fn write(&mut self, off: usize, value: u8, fs: u8) {
        match off {
            1 => self.len.load(LENGTH_MAX, u16::from(value & 0x3F)),
            2 => {
                self.env.write(value, self.on);
                if !self.env.dac() {
                    self.on = false;
                }
            }
            // Takes effect at the next step (SameBoy reads NR43 per
            // counter step; its mid-period phase quirks are deferred,
            // ADR 0021 section 12).
            3 => self.nr43 = value,
            4 => {
                if self.len.write_nrx4(LENGTH_MAX, value, fs) {
                    self.on = false;
                }
                if value & 0x80 != 0 {
                    // SameBoy NR44: LFSR 0 (output 0 until the first
                    // step), the envelope reloads. The first step after
                    // one period (SameBoy's alignment-dependent start is
                    // deferred, ADR 0021 section 12).
                    self.lfsr = 0;
                    self.countdown = self.period().get();
                    self.env.trigger();
                    self.on = self.env.dac();
                }
            }
            _ => {}
        }
    }

    /// Frame-sequencer step `step` (after the increment): length on odd
    /// steps (256 Hz), the envelope on step 0 (64 Hz).
    #[inline(never)]
    pub(crate) fn frame_step(&mut self, step: u8) {
        if step & 1 == 1 && self.len.clock() {
            self.on = false;
        }
        if step == 0 && self.on {
            self.env.clock();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One step at a time, straight from the description (SameBoy
    /// `shift_lfsr_with_feedback`).
    fn naive(mut x: u16, n: u32, narrow: bool) -> (u16, u32) {
        let mut ones = 0;
        for _ in 0..n {
            let fb = !(x ^ x >> 1) & 1;
            let mask = if narrow { 0x4040 } else { 0x4000 };
            x >>= 1;
            if fb != 0 {
                x |= mask;
            } else {
                x &= !mask;
            }
            ones += u32::from(x & 1);
        }
        (x, ones)
    }

    #[test]
    fn word_steps_match_single_steps() {
        let mut seed = 0x1234_5678_u32;
        for _ in 0..2000 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let x = (seed & 0x7FFF) as u16;
            let n = seed >> 16 & 63;
            for narrow in [false, true] {
                assert_eq!(
                    lfsr_steps(u32::from(x), n, narrow),
                    naive(x, n, narrow),
                    "x={x:#x} n={n} narrow={narrow}"
                );
            }
        }
    }

    #[test]
    fn known_sequence_from_zero() {
        // From 0 the feedback is 1: bit 14 fills first (15-bit), bits 14
        // and 6 (7-bit); bit 0 first becomes 1 after 15 / 7 steps.
        assert_eq!(naive(0, 1, false).0, 0x4000);
        assert_eq!(lfsr_steps(0, 1, true).0, 0x4040);
        assert_eq!(lfsr_steps(0, 14, false).1, 0);
        assert_eq!(lfsr_steps(0, 15, false).1, 1);
        assert_eq!(lfsr_steps(0, 6, true).1, 0);
        assert_eq!(lfsr_steps(0, 7, true).1, 1);
    }

    #[test]
    fn long_spans_reduce_exactly() {
        for narrow in [false, true] {
            let cycle = if narrow { 127 } else { 32_767 };
            for n in [0, 5, 8, 9, 200, cycle + 3, cycle + 20, 3 * cycle + 9] {
                let short = reduce(n, narrow);
                assert!(short < 8 + cycle as u32);
                for x in [0u16, 0x1234, 0x7FFF, 0x5A5A] {
                    let full = naive(x, n as u32, narrow).0;
                    assert_eq!(naive(x, short, narrow).0, full, "n={n} x={x:#x}");
                }
            }
        }
    }

    /// CH4 with its DAC on at volume 15, NR43 = `nr43`, triggered.
    fn playing(nr43: u8) -> Noise {
        let mut ch = Noise::ZERO;
        ch.write(2, 0xF0, 0);
        ch.write(3, nr43, 0);
        ch.write(4, 0x80, 0);
        ch
    }

    #[test]
    fn period_is_r_times_16_shifted_with_r_0_as_8() {
        for (nr43, p) in [
            (0x00, 8),
            (0x01, 16),
            (0x07, 112),
            (0x30, 64),
            (0xD7, 112 << 13),
        ] {
            assert_eq!(playing(nr43).countdown, p, "nr43={nr43:#x}");
        }
        // 15 steps from 0 to the first 1 at r = 1, s = 1: 32 clocks each.
        let ch = playing(0x11);
        assert_eq!(ch.digital_at(15 * 32 - 1), 0);
        assert_eq!(ch.digital_at(15 * 32), 15);
        // A trigger clears the LFSR.
        let mut ch = ch;
        ch.lfsr = 0x1235;
        ch.write(4, 0x80, 0);
        assert_eq!(ch.lfsr, 0);
    }

    #[test]
    fn shift_14_and_15_freeze_the_lfsr() {
        for nr43 in [0xE0, 0xF0, 0xE8] {
            let mut ch = playing(nr43);
            ch.lfsr = 0x1235;
            ch.advance(1 << 24);
            assert_eq!(ch.lfsr, 0x1235, "nr43={nr43:#x}");
            assert_eq!(ch.integrate1(128), 15 * 128 - 2 * 15 * 128);
        }
        let mut ch = playing(0xD0);
        ch.advance(1 << 24);
        assert_ne!(ch.lfsr, 0, "shift 13 clocks");
    }

    #[test]
    fn integrate_and_advance_agree_and_sum_high_time() {
        for nr43 in [0x00, 0x08, 0x01, 0x21, 0x4F] {
            let mut a = playing(nr43);
            let mut b = a;
            let mut want = 0;
            let mut got = 0;
            for t in 0..3000u32 {
                // Clock by clock: level 15 - 2 * digital.
                want += 15 - 2 * i32::from(b.digital());
                b.advance(1);
                if t % 100 == 99 {
                    got += a.integrate1(100);
                    assert_eq!(a, b, "nr43={nr43:#x} t={t}");
                }
            }
            assert_eq!(got, want, "nr43={nr43:#x}");
        }
    }

    #[test]
    fn dac_and_length_turn_the_channel_off() {
        let mut ch = playing(0x00);
        assert!(ch.on);
        ch.write(2, 0x08, 0); // DAC on (bit 3), volume 0
        assert!(ch.on);
        ch.write(2, 0x00, 0);
        assert!(!ch.on);
        assert_eq!(ch.integrate1(10), 0, "DAC off: 0");
        let mut ch = playing(0x00);
        ch.write(1, 0x3F, 0); // length 1
        ch.write(4, 0x40, 0); // enable, no trigger
        ch.frame_step(1);
        assert!(!ch.on);
        assert_eq!(ch.integrate1(10), 150, "DAC on, channel off: +15");
    }

    #[test]
    fn the_envelope_runs_on_step_0() {
        let mut ch = Noise::ZERO;
        ch.write(2, 0xF1, 0); // 15, down, period 1
        ch.write(4, 0x80, 0);
        ch.frame_step(7);
        assert_eq!(ch.env.volume, 15);
        ch.frame_step(0);
        assert_eq!(ch.env.volume, 14);
    }
}
