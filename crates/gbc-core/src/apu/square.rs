//! The square channels, CH1 and CH2 (ADR 0021 section 5): one non-generic
//! implementation for both, and CH1's sweep (task 3).

use core::num::NonZeroU32;

use super::units::{Envelope, Length};

/// The duty patterns (NRx1 bits 6-7) as 8-bit masks, bit i = position i
/// (SameBoy `duties`; Pan Docs NRx1 waveforms): 12.5%, 25%, 50%, 75%.
/// SameBoy lists positions 0-7 left to right (`0,0,0,0,0,0,0,1` for 12.5%),
/// so as bit masks the ADR 0021 literals read reversed: position 7 is bit 7.
const DUTY: [u8; 4] = [0b1000_0000, 0b1000_0001, 0b1110_0001, 0b0111_1110];

/// The square channels' length counter range.
const LENGTH_MAX: u16 = 64;

/// A square channel's state. The register bytes are in `Memory::io`.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Square {
    /// Base clocks to the next duty step (>= 1 while on).
    pub(crate) countdown: u16,
    /// The 11-bit frequency (NRx3, NRx4 bits 0-2): the period used at the
    /// next reload.
    pub(crate) freq: u16,
    /// NRx1 bits 6-7.
    pub(crate) duty: u8,
    /// The duty position, 0-7. Only APU power off resets it (Pan Docs).
    pub(crate) pos: u8,
    /// The channel has stepped since it started: until then it outputs
    /// digital 0 (Pan Docs: "On first startup, the channel outputs a
    /// digital zero"; SameBoy marks `current_sample_index` with 0x80).
    pub(crate) started: bool,
    /// The NR52 bit.
    pub(crate) on: bool,
    pub(crate) len: Length,
    pub(crate) env: Envelope,
}

impl Square {
    pub(crate) const ZERO: Self = Self {
        countdown: 0,
        freq: 0,
        duty: 0,
        pos: 0,
        started: false,
        on: false,
        len: Length::ZERO,
        env: Envelope::ZERO,
    };

    /// Base clocks per duty step: `(2048 - f) * 4`, 4-8192.
    #[inline]
    fn period(&self) -> NonZeroU32 {
        let p = (2048 - u32::from(self.freq & 0x7FF)) * 4;
        NonZeroU32::new(p).unwrap_or(NonZeroU32::MIN)
    }

    /// The duty bit at the current position.
    #[inline]
    fn high(&self) -> bool {
        DUTY[usize::from(self.duty & 3)] >> self.pos & 1 != 0
    }

    /// The digital output, 0-15 (PCM12/PCM34): the volume while the duty
    /// bit is 1, 0 while off or before the first step.
    #[inline]
    pub(crate) fn digital(&self) -> u8 {
        if self.on && self.started && self.high() {
            self.env.volume
        } else {
            0
        }
    }

    /// The digital output `dt` clocks after the state (PCM12), from a
    /// copy: the state is unchanged. Out of line: one copy for both
    /// squares (code size).
    #[inline(never)]
    pub(crate) fn digital_at(&self, dt: u64) -> u8 {
        let mut sq = *self;
        sq.advance(dt);
        sq.digital()
    }

    /// A duty step: the position moves on and the first-sample zero ends.
    #[inline]
    fn tick(&mut self, period: NonZeroU32) {
        self.pos = (self.pos + 1) & 7;
        self.started = true;
        self.countdown = period.get() as u16;
    }

    /// Advances by `dt` clocks without integrating (ADR 0021 section 9),
    /// in closed form: positions only matter mod 8. Out of line: one copy
    /// for both squares (code size).
    #[inline(never)]
    pub(crate) fn advance(&mut self, dt: u64) {
        if !self.on {
            return;
        }
        let cd = u64::from(self.countdown);
        if dt < cd {
            self.countdown = (cd - dt) as u16;
            return;
        }
        let p = self.period();
        let rest = dt - cd;
        // Positions repeat every 8 periods; a sync span is normally under
        // 2^32 clocks (DIV-APU steps every 8192), so the u64 remainder is
        // only for long STOP stretches (`rem_long`). `8 * p <= 65,536`.
        let rest = u32::try_from(rest).unwrap_or_else(|_| super::rem_long(rest, 8 * p.get()));
        let steps = rest / p;
        self.tick(p);
        self.pos = (self.pos + (steps & 7) as u8) & 7;
        self.countdown = (p.get() - rest % p) as u16;
    }

    /// Adds to `out[i]` the analog level times clocks over the `i`-th of
    /// the consecutive spans `segs` (each at most one sample window) and
    /// advances over them: `15 - 2 * digital` while the DAC is on (Pan Docs
    /// DAC: digital 0 -> +1, 15 -> -1), nothing while it is off. A batch
    /// per call: the period and duty are constant over a sync span. Each
    /// span in closed form: up to the first step at the current bit (0
    /// before the first step), then the `n` whole periods from the duty
    /// byte rotated to the first position (whole patterns by its
    /// popcount), then the final position's bit for the rest. Exact: the
    /// same state and integral as stepping
    /// (`integrate_matches_a_clock_by_clock_reference`). Out of line: one
    /// copy for both squares (code size).
    #[inline(never)]
    pub(crate) fn integrate(&mut self, segs: &[u8], out: &mut [i16]) {
        if !self.env.dac() {
            // The DAC off turns the channel off: nothing advances.
            return;
        }
        if !self.on {
            return super::mix::constant(segs, out, 15);
        }
        let spans = out.iter_mut().zip(segs);
        let vol2 = 2 * i32::from(self.env.volume);
        let p = self.period();
        let duty = DUTY[usize::from(self.duty & 3)];
        let whole = duty.count_ones();
        for (o, &dt) in spans {
            let dt = u32::from(dt);
            // Opaque: stops LLVM's Cortex-M runtime unrolling (x4), which
            // would double the function's size (docs/perf.md, APU cost).
            let dt = core::hint::black_box(dt);
            let cd = u32::from(self.countdown);
            let bit = u32::from(self.started) & u32::from(duty >> self.pos & 1);
            let high = if dt < cd {
                self.countdown = (cd - dt) as u16;
                bit * dt
            } else {
                let rest = dt - cd;
                let (n, r) = (rest / p, rest % p);
                let first = (self.pos + 1) & 7;
                let pattern = duty.rotate_right(u32::from(first));
                let m = (n & 7) as u8;
                let ones = (n >> 3) * whole + (pattern & ((1 << m) - 1)).count_ones();
                self.pos = (first + m) & 7;
                self.started = true;
                self.countdown = (p.get() - r) as u16;
                bit * cd + p.get() * ones + r * u32::from(pattern >> m & 1)
            };
            *o += (15 * dt as i32 - vol2 * high as i32) as i16;
        }
    }

    /// One span through `integrate` (tests).
    #[cfg(test)]
    pub(crate) fn integrate1(&mut self, dt: u32) -> i32 {
        let mut out = [0];
        self.integrate(&[u8::try_from(dt).unwrap()], &mut out);
        i32::from(out[0])
    }

    /// A write to NRx1-NRx4 (`off` 1-4) with the frame sequencer at `fs`
    /// and SameBoy's 1 MHz phase `lf_div` (0 or 1) at the write.
    #[inline]
    pub(crate) fn write(&mut self, off: usize, value: u8, fs: u8, lf_div: u8) {
        match off {
            1 => {
                self.duty = value >> 6;
                self.len.load(LENGTH_MAX, u16::from(value & 0x3F));
            }
            2 => {
                self.env.write(value, self.on);
                if !self.env.dac() {
                    self.on = false;
                }
            }
            3 => self.freq = self.freq & 0x700 | u16::from(value),
            4 => {
                self.freq = self.freq & 0xFF | u16::from(value & 7) << 8;
                if self.len.write_nrx4(LENGTH_MAX, value, fs) {
                    self.on = false;
                }
                if value & 0x80 != 0 {
                    self.trigger(lf_div);
                }
            }
            _ => {}
        }
    }

    /// A trigger (NRx4 bit 7; the length reload is in `Length::write_nrx4`).
    /// The duty position is kept; the first step comes after SameBoy's
    /// `sample_countdown = (f ^ 0x7FF) * 2 + delay` 2 MHz ticks, with
    /// `delay = 6 - lf_div` for a channel that is off and `4 - lf_div` for
    /// one that is on. A countdown of n fires after n + 1 ticks (SameBoy
    /// `GB_apu_run`: the reload `(f ^ 0x7FF) * 2 + 1` gives the period
    /// `(2048 - f) * 2` ticks), so in base clocks: `(2048 - f) * 4 + 10 -
    /// 2 * lf_div` (off) or `+ 6 - 2 * lf_div` (on). See
    /// `docs/reference/apu.md`.
    fn trigger(&mut self, lf_div: u8) {
        let extra = (if self.on { 6 } else { 10 }) - 2 * u32::from(lf_div & 1);
        self.countdown = (self.period().get() + extra) as u16;
        if !self.on {
            self.started = false;
        }
        self.env.trigger();
        self.on = self.env.dac();
    }

    /// Frame-sequencer step `step` (after the increment): length on odd
    /// steps (256 Hz), the envelope on step 0 (64 Hz). Out of line: one
    /// copy for both squares (code size).
    #[inline(never)]
    pub(crate) fn frame_step(&mut self, step: u8) {
        if step & 1 == 1 {
            self.clock_length();
        }
        if step == 0 {
            self.clock_envelope();
        }
    }

    /// A frame-sequencer length step.
    #[inline]
    pub(crate) fn clock_length(&mut self) {
        if self.len.clock() {
            self.on = false;
        }
    }

    /// A frame-sequencer envelope step.
    #[inline]
    pub(crate) fn clock_envelope(&mut self) {
        if self.on {
            self.env.clock();
        }
    }
}

/// CH1's frequency sweep (NR10), classic model (ADR 0021 section 5; Pan
/// Docs "Audio Registers" FF10 and "Audio details", Pulse channel with
/// sweep; blargg `cgb_sound` 04-06).
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Sweep {
    /// The shadow frequency: copied from the channel on trigger, the base
    /// of every calculation.
    pub(crate) shadow: u16,
    /// NR10 bits 0-6: period (4-6), negate (3), shift (0-2).
    pub(crate) nr10: u8,
    /// Sweep steps left before the next calculation.
    pub(crate) timer: u8,
    /// The internal enabled flag: period or shift non-zero at the trigger.
    pub(crate) enabled: bool,
    /// A calculation in negate mode ran since the trigger.
    pub(crate) negate_used: bool,
}

impl Sweep {
    pub(crate) const ZERO: Self = Self {
        shadow: 0,
        nr10: 0,
        timer: 0,
        enabled: false,
        negate_used: false,
    };

    /// NR10 bits 4-6: steps between calculations (0: none).
    #[inline]
    fn period(&self) -> u8 {
        self.nr10 >> 4 & 7
    }

    /// The timer reload: a period of 0 counts as 8.
    #[inline]
    fn reload(&mut self) {
        self.timer = match self.period() {
            0 => 8,
            p => p,
        };
    }

    /// One calculation: `shadow +- (shadow >> shift)`. A result over 2047
    /// (the overflow check) turns the channel off. A subtraction is
    /// remembered for the negate quirk.
    #[inline]
    fn calc(&mut self, ch: &mut Square) -> u16 {
        let delta = self.shadow >> (self.nr10 & 7);
        let new = if self.nr10 & 8 != 0 {
            self.negate_used = true;
            self.shadow - delta
        } else {
            self.shadow + delta
        };
        if new > 2047 {
            ch.on = false;
        }
        new
    }

    /// An NR10 write. Negate quirk: clearing the negate bit after a
    /// negate-mode calculation since the trigger turns the channel off
    /// (Pan Docs NR10; SameBoy `GB_apu_write` NR10, always on CGB C and
    /// older).
    #[inline]
    pub(crate) fn write(&mut self, value: u8, ch: &mut Square) {
        self.nr10 = value & 0x7F;
        if self.negate_used && value & 8 == 0 {
            ch.on = false;
        }
    }

    /// A trigger of `ch` (after its own trigger): the frequency is copied
    /// to the shadow, the timer reloads, the sweep is enabled if the
    /// period or the shift is non-zero, and with a non-zero shift one
    /// calculation runs for its overflow check only.
    #[inline]
    pub(crate) fn trigger(&mut self, ch: &mut Square) {
        self.shadow = ch.freq & 0x7FF;
        self.reload();
        let shift = self.nr10 & 7;
        self.enabled = self.period() != 0 || shift != 0;
        self.negate_used = false;
        if shift != 0 {
            self.calc(ch);
        }
    }

    /// A frame-sequencer sweep step (128 Hz). At the timer's end it
    /// reloads and, if enabled with a non-zero period, calculates; a
    /// result within range with a non-zero shift is written back to the
    /// shadow and the channel frequency, and a second calculation runs for
    /// its overflow check only.
    #[inline(never)]
    pub(crate) fn clock(&mut self, ch: &mut Square) {
        self.timer = self.timer.wrapping_sub(1);
        if self.timer != 0 {
            return;
        }
        self.reload();
        if !self.enabled || self.period() == 0 {
            return;
        }
        let new = self.calc(ch);
        if new <= 2047 && self.nr10 & 7 != 0 {
            self.shadow = new;
            ch.freq = new;
            self.calc(ch);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A playing square: DAC on, triggered at volume `vol` with duty `duty`
    /// and frequency `f`, then run to its first step.
    fn playing(duty: u8, vol: u8, f: u16) -> Square {
        let mut sq = Square::ZERO;
        sq.write(1, duty << 6, 0, 0);
        sq.write(2, vol << 4, 0, 0);
        sq.write(3, f as u8, 0, 0);
        sq.write(4, 0x80 | (f >> 8) as u8, 0, 0);
        let cd = u64::from(sq.countdown);
        sq.advance(cd);
        sq
    }

    #[test]
    fn duty_waveforms_follow_the_patterns() {
        // f = 2047: one step per 4 clocks.
        for (duty, want) in [
            (0, [0, 0, 0, 0, 0, 0, 0, 1]),
            (1, [1, 0, 0, 0, 0, 0, 0, 1]),
            (2, [1, 0, 0, 0, 0, 1, 1, 1]),
            (3, [0, 1, 1, 1, 1, 1, 1, 0]),
        ] {
            let mut sq = playing(duty, 9, 2047);
            // The first step has moved the position to 1.
            let mut got = [0; 8];
            for i in 0..8 {
                got[(i + 1) % 8] = sq.digital() / 9;
                sq.advance(4);
            }
            assert_eq!(got, want, "duty {duty}");
        }
    }

    #[test]
    fn a_started_channel_outputs_zero_until_its_first_step() {
        let mut sq = Square::ZERO;
        sq.pos = 7; // duty 0: high at position 7
        sq.write(2, 0xF0, 0, 0);
        sq.write(4, 0x87, 0, 0); // f = 0x700, period 1024
        assert!(sq.on);
        assert_eq!(sq.digital(), 0, "first-sample zero");
        assert_eq!(sq.countdown, 1024 + 10);
        let mut a = sq;
        assert_eq!(a.integrate1(100), 15 * 100, "digital 0 -> +15");
        // Retriggering an active channel keeps the output and uses the
        // short delay; lf_div 1 subtracts 2 clocks.
        sq.started = true;
        sq.write(4, 0x87, 0, 1);
        assert!(sq.started);
        assert_eq!(sq.countdown, 1024 + 6 - 2);
    }

    #[test]
    fn advance_over_2_pow_32_clocks_matches_shorter_spans() {
        for f in [0, 1000, 1999, 2047] {
            crate::apu::check_long_advance(playing(2, 15, f), |c, dt| c.advance(dt));
        }
    }

    #[test]
    fn advance_and_integrate_agree_and_split_freely() {
        let base = playing(2, 15, 1900);
        let mut one = base;
        one.advance(10_000);
        let mut split = base;
        for dt in [1, 127, 128, 3000, 6744] {
            split.advance(dt);
        }
        assert_eq!(split, one);
        let mut int = base;
        let mut left = 10_000u32;
        while left != 0 {
            let dt = left.min(128);
            int.integrate1(dt);
            left -= dt;
        }
        assert_eq!(int, one);
    }

    #[test]
    fn integrate_matches_a_clock_by_clock_reference() {
        for duty in 0..4 {
            for f in [2047, 2046, 2043, 2017, 1900, 0x700] {
                // Started and right after a trigger (first-sample zero).
                let mut fresh = playing(duty, 9, f);
                fresh.write(4, 0x80 | (f >> 8) as u8, 0, 0);
                for start in [playing(duty, 9, f), fresh] {
                    let (mut a, mut b) = (start, start);
                    for dt in [1, 128, 37, 2, 3, 4, 5, 31, 32, 33, 128, 91, 127, 128, 128] {
                        let want: i32 = (0..dt)
                            .map(|_| {
                                let l = 15 - 2 * i32::from(b.digital());
                                b.advance(1);
                                l
                            })
                            .sum();
                        assert_eq!(a.integrate1(dt), want, "duty {duty} f {f}");
                        assert_eq!(a, b, "duty {duty} f {f}");
                    }
                }
            }
        }
    }

    #[test]
    fn the_dac_off_turns_the_channel_off() {
        let mut sq = playing(2, 15, 2000);
        assert!(sq.on);
        sq.write(2, 0x08, 0, 0); // volume 0, up: DAC still on
        assert!(sq.on);
        sq.write(2, 0x07, 0, 0); // bits 3-7 all 0: DAC off
        assert!(!sq.on);
        assert_eq!(sq.integrate1(128), 0);
        // A trigger with the DAC off leaves it off, but reloads length.
        sq.write(4, 0x80, 0, 0);
        assert!(!sq.on);
        assert_eq!(sq.len.counter, 64);
    }

    #[test]
    fn length_expiry_turns_the_channel_off() {
        let mut sq = Square::ZERO;
        sq.write(1, 0x3E, 0, 0); // length 2
        sq.write(2, 0xF0, 0, 0);
        sq.write(4, 0xC0, 0, 0);
        assert!(sq.on);
        sq.clock_length();
        assert!(sq.on);
        sq.clock_length();
        assert!(!sq.on);
    }

    #[test]
    fn extra_length_clocking_on_nrx4_can_turn_the_channel_off() {
        let mut sq = Square::ZERO;
        sq.write(1, 0x3F, 0, 0); // length 1
        sq.write(2, 0xF0, 0, 0);
        sq.write(4, 0x80, 0, 0); // trigger, length disabled
        assert!(sq.on);
        sq.write(4, 0x40, 1, 0); // enable with fs odd: 1 -> 0
        assert!(!sq.on);
    }

    /// CH1 with its DAC on and NR10 = `nr10`, triggered at frequency `f`
    /// (the channel's trigger, then the sweep's, as `Apu::write` does).
    fn ch1(nr10: u8, f: u16) -> (Sweep, Square) {
        let mut sw = Sweep::ZERO;
        let mut sq = Square::ZERO;
        sq.write(2, 0xF0, 0, 0);
        sw.write(nr10, &mut sq);
        sq.write(3, f as u8, 0, 0);
        sq.write(4, 0x80 | (f >> 8) as u8, 0, 0);
        sw.trigger(&mut sq);
        (sw, sq)
    }

    #[test]
    fn sweep_trigger_copies_the_frequency_and_sets_the_enabled_flag() {
        let (sw, sq) = ch1(0x21, 0x123); // period 2, shift 1
        assert!(sq.on);
        assert_eq!((sw.shadow, sw.timer, sw.enabled), (0x123, 2, true));
        // Period 0 and shift 0: disabled, the timer loads 8.
        let (sw, _) = ch1(0x00, 0x123);
        assert_eq!((sw.timer, sw.enabled), (8, false));
        // Shift alone enables it.
        assert!(ch1(0x01, 0x100).0.enabled);
    }

    #[test]
    fn sweep_steps_add_or_subtract_and_write_back() {
        let (mut sw, mut sq) = ch1(0x11, 0x400); // period 1, up, shift 1
        sw.clock(&mut sq);
        assert_eq!((sq.freq, sw.shadow), (0x600, 0x600));
        let (mut sw, mut sq) = ch1(0x19, 0x400); // down
        sw.clock(&mut sq);
        assert_eq!((sq.freq, sw.shadow), (0x200, 0x200));
        assert!(sq.on);
    }

    #[test]
    fn sweep_period_0_counts_as_8() {
        let (mut sw, mut sq) = ch1(0x01, 0x100); // period 0, shift 1
        sw.write(0x11, &mut sq); // period 1 now; the timer still runs 8
        for _ in 0..7 {
            sw.clock(&mut sq);
        }
        assert_eq!(sq.freq, 0x100);
        sw.clock(&mut sq);
        assert_eq!(sq.freq, 0x180);
    }

    #[test]
    fn a_disabled_sweep_never_calculates() {
        let (mut sw, mut sq) = ch1(0x00, 0x700);
        sw.write(0x11, &mut sq); // period 1, shift 1, but not enabled
        for _ in 0..16 {
            sw.clock(&mut sq);
        }
        assert_eq!(sq.freq, 0x700);
        assert!(sq.on);
    }

    #[test]
    fn the_second_calculation_checks_overflow_only() {
        // 1280 + 640 = 1920 is written back; 1920 + 960 overflows: off.
        let (mut sw, mut sq) = ch1(0x11, 1280);
        sw.clock(&mut sq);
        assert_eq!((sq.freq, sw.shadow), (1920, 1920));
        assert!(!sq.on);
    }

    #[test]
    fn shift_0_checks_overflow_without_writing_back() {
        // Period 1, shift 0: 1023 + 1023 = 2046 fits, but isn't stored.
        let (mut sw, mut sq) = ch1(0x10, 1023);
        sw.clock(&mut sq);
        assert_eq!((sq.freq, sw.shadow), (1023, 1023));
        assert!(sq.on);
        // 1024 + 1024 = 2048 overflows: off.
        let (mut sw, mut sq) = ch1(0x10, 1024);
        sw.clock(&mut sq);
        assert!(!sq.on);
    }

    #[test]
    fn overflow_on_trigger_needs_a_non_zero_shift() {
        // Shift 1: 2047 + 1023 overflows at the trigger.
        let (_, sq) = ch1(0x01, 0x7FF);
        assert!(!sq.on);
        // Shift 0: no calculation at the trigger.
        let (_, sq) = ch1(0x70, 0x7FF);
        assert!(sq.on);
    }

    #[test]
    fn clearing_negate_after_a_negate_calculation_turns_the_channel_off() {
        // The trigger's calculation (shift 1) counts.
        let (mut sw, mut sq) = ch1(0x19, 0x400);
        sw.write(0x11, &mut sq);
        assert!(!sq.on);
        // No calculation yet (shift 0 at the trigger): stays on.
        let (mut sw, mut sq) = ch1(0x18, 0x400);
        sw.write(0x10, &mut sq);
        assert!(sq.on);
        // A sweep step's calculation in negate mode counts.
        sw.write(0x18, &mut sq);
        sw.clock(&mut sq);
        assert!(sq.on);
        sw.write(0x10, &mut sq);
        assert!(!sq.on);
        // A retrigger forgets it.
        let (mut sw, mut sq) = ch1(0x19, 0x400);
        sw.write(0x10, &mut sq); // off
        sq.write(4, 0x84, 0, 0);
        sw.trigger(&mut sq); // shift 0 now: no calculation
        sw.write(0x00, &mut sq);
        assert!(sq.on);
    }
}
