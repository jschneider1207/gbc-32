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
    /// Base clocks to the next LFSR step (>= 1 while on): the divisor
    /// counter increment on which its bit `s` rises. With shift 14 or 15
    /// the steps come every `2^(s+1)` increments but leave the LFSR alone.
    pub(crate) countdown: u32,
    /// The 15-bit LFSR (bit 15 always 0). Bit 0 is the output.
    pub(crate) lfsr: u16,
    /// SameBoy's free-running 14-bit divisor counter
    /// (`noise_channel.counter`) as it will be right after the increment
    /// at `countdown`. Between steps it is implied: `k` increments before
    /// that one, every `divisor()` clocks (docs/reference/apu.md, noise).
    pub(crate) counter: u16,
    /// At most this many increments before the one at `countdown`: after
    /// an NR43 write the first increment comes when the old divisor
    /// countdown runs out, up to 56 clocks, which may be longer than the
    /// new divisor. `u16::MAX` once a step has passed.
    pub(crate) pre: u16,
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
fn reduce(n: u32, narrow: bool) -> u32 {
    let (base, cycle) = lfsr_cycle(narrow);
    if n < base + cycle {
        n
    } else {
        base + (n - base) % cycle
    }
}

/// The LFSR's pre-period and cycle length (`reduce`).
#[inline]
fn lfsr_cycle(narrow: bool) -> (u32, u32) {
    if narrow { (8, 127) } else { (0, 32_767) }
}

/// `Noise::advance` over `rest >= 2^32` clocks past the countdown (a long
/// STOP or a caller's distant run target; see `apu::rem_long`): the step
/// count `rest / p + 1` and `rest % p`, in u64 (`__aeabi_uldivmod` on
/// thumbv7em), out of line and cold. The count is replaced by one with
/// the same effect that fits u32: congruent modulo 2^14 (`bump` keeps the
/// counter modulo 2^14, and the count stays nonzero) and, at or past the
/// pre-period plus one cycle, modulo the LFSR cycle and still past it
/// (`reduce`). The modulus `2^14 * cycle` (at most 536,854,528) keeps it
/// under 2^30.
#[cold]
#[inline(never)]
fn long_steps(rest: u64, p: u32, narrow: bool) -> (u32, u32) {
    let n = rest / u64::from(p) + 1;
    let (base, cycle) = lfsr_cycle(narrow);
    let past = u64::from(base + cycle);
    let n = if n < past {
        n
    } else {
        past + (n - past) % ((1 << 14) * u64::from(cycle))
    };
    (n as u32, (rest % u64::from(p)) as u32)
}

/// Set bits per byte value: a table, as Cortex-M7 has no popcount
/// instruction (LLVM's `count_ones` is a ~12-instruction SWAR sequence
/// there, as on baseline x86-64).
static POP8: [u8; 256] = {
    let mut t = [0; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = (i as u8).count_ones() as u8;
        i += 1;
    }
    t
};

/// The LFSR as its output sequence, for a batch of windows: many steps per
/// word operation without stepping the register (plan M6 task 8b).
///
/// Let `s(m)` be bit 0 after `m` steps from `x0`. The register's low `n`
/// bits (`n` = 15, or 7 in 7-bit mode) are `s(0..n)`, and each step writes
/// `!(b0 ^ b1)` at bit `n - 1` after the shift (and bit 14), so
/// `s(m + n) = !(s(m) ^ s(m + 1))` for all `m >= 0`. Applied to itself:
/// `s(m + 2n) = !(s(m) ^ s(m + 2))`, and again `s(m + 4n) = !(s(m) ^
/// s(m + 4))` (the complements cancel in pairs). With lag `d` and tap `t`
/// the `d - t` outputs after the `len` known ones are one word operation:
/// 24 per operation in 7-bit mode (lag 28), 28 in 15-bit mode (lag 30).
///
/// Bit `j` of `bits` is `s(m - 1 + j)` after `m` steps, for `j < len`:
/// bit 0 the output before the last step (unknown before the first), bit 1
/// the current one. The register is rebuilt from it: 15-bit mode, the next
/// 15 outputs; 7-bit mode, bits 0-6 the next 7 outputs and bits 7-14 the
/// last 8 feedback bits, `s(m - 1 .. m + 7)`, except for the original bits
/// 7-14 not yet overwritten in the first 8 steps: `corr` holds those
/// differences, shifted along.
struct Run {
    bits: u64,
    /// Known bits: at least `lag + 1` between steps.
    len: u32,
    lag: u32,
    tap: u32,
    corr: u32,
    narrow: bool,
}

impl Run {
    #[inline]
    fn new(x0: u16, narrow: bool) -> Self {
        let (n, lag, tap) = if narrow { (7, 28, 4) } else { (15, 30, 2) };
        let bits = u64::from(x0 & ((1 << n) - 1)) << 1;
        let mut run = Self {
            bits,
            len: n + 1,
            lag: n,
            tap: 1,
            corr: if narrow {
                (u32::from(x0) >> 7 ^ bits as u32) & 0xFF
            } else {
                0
            },
            narrow,
        };
        // Lags n and 2n until there are lag + 1 known bits.
        while run.len <= lag {
            if run.len > 2 * n {
                (run.lag, run.tap) = (2 * n, 2);
            }
            run.fill();
        }
        (run.lag, run.tap) = (lag, tap);
        run
    }

    /// One word operation: `lag - tap` more known bits (up to 64).
    #[inline]
    fn fill(&mut self) {
        let lo = self.len - self.lag;
        let k = self.lag - self.tap;
        let f = !(self.bits >> lo ^ self.bits >> (lo + self.tap)) & ((1 << k) - 1);
        self.bits |= f << self.len;
        self.len = (self.len + k).min(64);
    }

    /// The current output (bit 0 of the register).
    #[inline]
    fn out(&self) -> u32 {
        (self.bits >> 1) as u32 & 1
    }

    /// `n` (at most 16) steps; returns the number after which bit 0 is 1.
    #[inline]
    fn take(&mut self, n: u32) -> u32 {
        debug_assert!(n <= 16);
        // One operation adds at least 24 >= n bits: `len` stays > lag.
        if self.len <= n + self.lag {
            self.fill();
        }
        let v = (self.bits >> 2) as u32 & ((1 << n) - 1);
        let ones = POP8[usize::from(v as u8)] + POP8[usize::from((v >> 8) as u8)];
        self.bits >>= n;
        self.len -= n;
        self.corr >>= n;
        u32::from(ones)
    }

    /// The register after the steps taken.
    #[inline]
    fn lfsr(&self) -> u16 {
        let b = self.bits as u32;
        if self.narrow {
            (b >> 1 & 0x7F | ((b ^ self.corr) & 0xFF) << 7) as u16
        } else {
            (b >> 1 & 0x7FFF) as u16
        }
    }
}

impl Noise {
    pub(crate) const ZERO: Self = Self {
        countdown: 0,
        lfsr: 0,
        counter: 0,
        pre: 0,
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
        NonZeroU32::new(self.divisor() << ((self.nr43 >> 4) + 1)).unwrap_or(NonZeroU32::MIN)
    }

    /// Base clocks per divisor counter increment: SameBoy's `divisor`
    /// (`r * 4`, r = 0: 2) 2 MHz ticks, times 2.
    #[inline]
    fn divisor(&self) -> u32 {
        let r = u32::from(self.nr43 & 7);
        if r == 0 { 4 } else { 8 * r }
    }

    /// Moves `counter` on by `n` steps: `2^(s+1)` increments each (a
    /// multiple of 2^14, so no change, for s >= 13).
    #[inline]
    fn bump(&mut self, n: u32) {
        let inc = n << ((self.nr43 >> 4) + 1);
        self.counter = (u32::from(self.counter).wrapping_add(inc) & 0x3FFF) as u16;
        if n != 0 {
            self.pre = u16::MAX;
        }
    }

    /// Writes NR43 (`trigger` false) or restarts the step phase (a trigger,
    /// `nr43` unchanged) and re-derives the next step from the divisor
    /// counter, which keeps running (SameBoy `GB_apu_write` NR43 and
    /// `GB_apu_run`: the LFSR steps when an increment raises counter bit
    /// `s`, read from NR43 at each increment). An NR43 write keeps the
    /// counter and the divisor countdown, unless that has just reloaded
    /// (the write lands on an increment), when the new divisor loads.
    /// The next step is then at the first increment that raises the new
    /// bit `s`: within the rest of the old divisor countdown (at most 56
    /// clocks) plus one new period. A trigger keeps the counter's bits
    /// above `s` and loads one full period, as before (SameBoy's
    /// alignment-dependent start is deferred, ADR 0021 section 12).
    #[inline(never)]
    fn rebase(&mut self, nr43: u8, trigger: bool) {
        let d = self.divisor();
        // `k` more increments come before the one at `countdown` (at most
        // `pre`), so the counter is now `counter - k - 1` and its next increment is `t`
        // clocks away (1..=d; 0 only in the power-off state).
        let k = (self.countdown.saturating_sub(1) / d).min(u32::from(self.pre));
        let mut t = self.countdown - k * d;
        let mut v = u32::from(self.counter).wrapping_sub(k + 1);
        self.nr43 = nr43;
        let s = u32::from(nr43 >> 4);
        if trigger {
            // Bit `s` has just risen: the next rise is a full period away.
            v = v & !((2 << s) - 1) | 1 << s;
            t = 0;
        }
        // SameBoy `countdown_reloaded` (an increment landed just now; not
        // the end of a long first gap): the new divisor loads (its
        // alignment term, `{2, 1, 4, 3}[alignment & 3]` ticks on CGB-C, is
        // not modelled).
        if t == 0 || t == d && k < u32::from(self.pre) {
            t = self.divisor();
        }
        // Increments after the next one up to the one where bit `s` rises:
        // the counter after it is `2^s` modulo `2^(s+1)`. Shift 14 and 15
        // never rise; any increment serves as the reference.
        let j = if s < 14 {
            (1u32 << s).wrapping_sub(v.wrapping_add(1)) & ((2 << s) - 1)
        } else {
            0
        };
        self.countdown = t + j * self.divisor();
        self.pre = j as u16;
        self.counter = (v.wrapping_add(1 + j) & 0x3FFF) as u16;
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
        let narrow = self.nr43 & 8 != 0;
        // u32 for spans under 2^32 clocks (all but long STOP stretches):
        // `p >= 8`, so `n <= 2^29 + 1`.
        debug_assert!(p >= 8);
        let (n, r) = match u32::try_from(rest) {
            Ok(rest) => (rest / p + 1, rest % p),
            Err(_) => long_steps(rest, p, narrow),
        };
        self.countdown = p - r;
        self.bump(n);
        self.clock(reduce(n, narrow));
    }

    /// Adds to `out[i]` the analog level times clocks over the `i`-th of
    /// the consecutive spans `segs` (each at most one sample window, so at
    /// most 17 steps) and advances over them: `15 - 2 * digital` while the
    /// DAC is on (Pan Docs DAC), nothing while it is off. A batch per call:
    /// NR43 and the volume are constant over a sync span.
    #[inline(never)]
    pub(crate) fn integrate(&mut self, segs: &[u8], out: &mut [i16]) {
        if !self.env.dac() {
            return;
        }
        if !self.on {
            return super::mix::constant(segs, out, 15);
        }
        let vol2 = 2 * i32::from(self.env.volume);
        if self.nr43 >> 4 >= 14 {
            // Frozen LFSR (`clock`): a constant level; the countdown still
            // runs.
            super::mix::constant(segs, out, 15 - vol2 * i32::from(self.lfsr & 1));
            let total = segs.iter().map(|&dt| u64::from(dt)).sum();
            return self.advance(total);
        }
        let p = self.period();
        let mut run = Run::new(self.lfsr, self.nr43 & 8 != 0);
        let mut cd = self.countdown;
        let mut steps = 0u32;
        for (o, &dt) in out.iter_mut().zip(segs) {
            let dt = u32::from(dt);
            // At most one window: at most 16 steps (p >= 8).
            debug_assert!(u64::from(dt) <= super::SAMPLE_PERIOD);
            // Opaque: stops LLVM's Cortex-M runtime unrolling (x4), which
            // would double the function's size (docs/perf.md, APU cost).
            let dt = core::hint::black_box(dt);
            // Clocks with bit 0 = 1: up to the first step at the old bit,
            // then `p` per step after which it is 1, minus the unelapsed
            // rest of the last step's period.
            let bit = run.out();
            let high = if dt < cd {
                cd -= dt;
                bit * dt
            } else {
                let rest = dt - cd;
                let left = p.get() - rest % p;
                let n = rest / p + 1;
                steps = steps.wrapping_add(n);
                let ones = run.take(n);
                let h = bit * cd + ones * p.get() - run.out() * left;
                cd = left;
                h
            };
            *o += (15 * dt as i32 - vol2 * high as i32) as i16;
        }
        self.countdown = cd;
        self.lfsr = run.lfsr();
        self.bump(steps);
    }

    /// One span through `integrate` (tests).
    #[cfg(test)]
    pub(crate) fn integrate1(&mut self, dt: u32) -> i32 {
        let mut out = [0];
        self.integrate(&[u8::try_from(dt).unwrap()], &mut out);
        i32::from(out[0])
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
            // The new shift and divisor apply from the running counter,
            // within one new period (`rebase`).
            3 => self.rebase(value, false),
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
                    self.rebase(self.nr43, true);
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

    fn xorshift(seed: &mut u32) -> u32 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 17;
        *seed ^= *seed << 5;
        *seed
    }

    /// The output-sequence stepper against single steps: random registers,
    /// random runs of 0-16 steps, both modes; the count and the rebuilt
    /// register after every run (including fewer than 8 steps in 7-bit
    /// mode, and the 15-bit all-ones fixed point).
    #[test]
    fn output_runs_match_single_steps() {
        let mut seed = 0x9E37_79B9_u32;
        for case in 0..3000 {
            let x0 = match case {
                0 => 0x7FFF,
                1 => 0,
                _ => (xorshift(&mut seed) & 0x7FFF) as u16,
            };
            for narrow in [false, true] {
                let mut run = Run::new(x0, narrow);
                let mut x = x0;
                for _ in 0..40 {
                    let n = xorshift(&mut seed) % 17;
                    let (want, ones) = naive(x, n, narrow);
                    assert_eq!(run.take(n), ones, "x0={x0:#x} narrow={narrow}");
                    x = want;
                    assert_eq!(run.lfsr(), x, "x0={x0:#x} narrow={narrow}");
                    assert_eq!(run.out(), u32::from(x & 1));
                }
            }
        }
    }

    /// Batches of random spans (at most one window each) against clock by
    /// clock, in both modes and with the shift-14/15 freeze.
    #[test]
    fn batches_match_clock_by_clock() {
        let mut seed = 0x0BAD_5EED_u32;
        for nr43 in [0x00, 0x08, 0x01, 0x09, 0x10, 0x18, 0x27, 0xE8, 0xF0] {
            let mut a = playing(nr43);
            a.lfsr = (xorshift(&mut seed) & 0x7FFF) as u16;
            let mut b = a;
            for _ in 0..50 {
                let k = 1 + xorshift(&mut seed) as usize % 16;
                let mut segs = [0u8; 16];
                let mut got = [0i16; 16];
                for s in &mut segs[..k] {
                    *s = (xorshift(&mut seed) % 129) as u8;
                }
                a.integrate(&segs[..k], &mut got[..k]);
                for (i, &dt) in segs[..k].iter().enumerate() {
                    let mut want = 0;
                    for _ in 0..dt {
                        want += 15 - 2 * i32::from(b.digital());
                        b.advance(1);
                    }
                    assert_eq!(i32::from(got[i]), want, "nr43={nr43:#x}");
                }
                assert_eq!(a, b, "nr43={nr43:#x}");
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
                assert!(short < 8 + cycle);
                for x in [0u16, 0x1234, 0x7FFF, 0x5A5A] {
                    let full = naive(x, n, narrow).0;
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

    /// The review scenario: NR43 = F7 freezes the LFSR, then 0x11 without
    /// a retrigger. The LFSR must step within the old divisor countdown
    /// (r = 7: 56 clocks) plus the new period (32), not when the old step
    /// countdown (up to 112 << 15 clocks) runs out.
    #[test]
    fn nr43_write_applies_within_one_new_period() {
        for frozen in [0xF7, 0xE7, 0xD7] {
            let mut ch = playing(0x11);
            ch.advance(5000);
            ch.write(3, frozen, 0);
            ch.advance(12_345);
            let x = ch.lfsr;
            assert_ne!(x, 0x7FFF);
            ch.write(3, 0x11, 0);
            assert!(ch.countdown <= 56 + 32, "{frozen:#x}: {}", ch.countdown);
            ch.advance(56 + 32);
            assert_ne!(ch.lfsr, x, "{frozen:#x}: no step within 88 clocks");
        }
        // A shift lowered mid-run (period 1024 -> 8) and a divisor lowered
        // with it (r 7, s 4: 1792 clocks -> r 7, s 0: 112).
        let mut ch = playing(0x70);
        ch.advance(100);
        ch.write(3, 0x00, 0);
        assert!(ch.countdown <= 4 + 8, "{}", ch.countdown);
        let mut ch = playing(0x47);
        ch.advance(3);
        ch.write(3, 0x07, 0);
        assert!(ch.countdown <= 56 + 112, "{}", ch.countdown);
    }

    /// SameBoy's counter, clock by clock: a 14-bit counter incremented
    /// every `divisor` clocks, the LFSR stepping when bit `s` (NR43 at
    /// that increment) rises; an NR43 write keeps the counter and the
    /// divisor countdown unless that has just reloaded (`GB_apu_run`,
    /// `GB_apu_write` NR43, without the glitch steps and alignment term).
    struct Counter {
        counter: u16,
        /// Clocks to the next increment.
        cd: u32,
        lfsr: u16,
        nr43: u8,
        /// The last clock incremented (SameBoy `countdown_reloaded`).
        reloaded: bool,
    }

    impl Counter {
        fn tick(&mut self) {
            self.cd -= 1;
            self.reloaded = self.cd == 0;
            if self.cd == 0 {
                self.cd = divisor(self.nr43);
                let s = self.nr43 >> 4;
                let old = u32::from(self.counter) >> s & 1;
                self.counter = (self.counter + 1) & 0x3FFF;
                if old == 0 && u32::from(self.counter) >> s & 1 == 1 {
                    self.lfsr = naive(self.lfsr, 1, self.nr43 & 8 != 0).0;
                }
            }
        }

        fn write(&mut self, nr43: u8) {
            if self.reloaded {
                self.cd = divisor(nr43);
            }
            self.nr43 = nr43;
        }
    }

    fn divisor(nr43: u8) -> u32 {
        match nr43 & 7 {
            0 => 4,
            r => 8 * u32::from(r),
        }
    }

    /// Random NR43 writes (shift, divisor and width) at random times
    /// against the clock-by-clock counter, through `advance` and
    /// `integrate`.
    #[test]
    fn nr43_writes_follow_the_free_running_counter() {
        let mut seed = 0x5EED_0043_u32;
        for case in 0..300 {
            let mut ch = playing((xorshift(&mut seed) & 0x3F) as u8);
            ch.advance(u64::from(xorshift(&mut seed) % 5000));
            // The reference starts from the same counter and phase.
            let d = divisor(ch.nr43);
            let k = ((ch.countdown - 1) / d).min(u32::from(ch.pre));
            let mut r = Counter {
                counter: ch.counter.wrapping_sub(k as u16 + 1) & 0x3FFF,
                cd: ch.countdown - k * d,
                lfsr: ch.lfsr,
                nr43: ch.nr43,
                reloaded: ch.countdown - k * d == d && k < u32::from(ch.pre),
            };
            for _ in 0..30 {
                // Mostly small shifts (audible steps), some frozen.
                let x = xorshift(&mut seed);
                let s = if x & 0x300 == 0 {
                    15 - (x >> 4 & 3)
                } else {
                    x >> 4 & 7
                };
                let nr43 = (s << 4 | x & 0xF) as u8;
                ch.write(3, nr43, 0);
                r.write(nr43);
                // Writes are at least 2 clocks apart (an M-cycle at double speed).
                let dt = 2 + xorshift(&mut seed) % 600;
                if case % 2 == 0 {
                    ch.advance(u64::from(dt));
                } else {
                    let mut left = dt;
                    while left != 0 {
                        let seg = left.min(128);
                        ch.integrate1(seg);
                        left -= seg;
                    }
                }
                for _ in 0..dt {
                    r.tick();
                }
                assert_eq!(ch.lfsr, r.lfsr, "case {case} nr43={nr43:#x}");
            }
        }
    }

    #[test]
    fn advance_over_2_pow_32_clocks_matches_shorter_spans() {
        // p = 8 (2^29 steps per piece), shift 13 (~7,000 steps in all:
        // under one 15-bit cycle, over 7-bit ones), frozen shift 15; 15-
        // and 7-bit.
        for nr43 in [0x00, 0x08, 0x10, 0x47, 0xD7, 0xDF, 0xF7] {
            crate::apu::check_long_advance(playing(nr43), |c, dt| c.advance(dt));
        }
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
