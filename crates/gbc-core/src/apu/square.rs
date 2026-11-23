//! The square channels, CH1 and CH2 (ADR 0021 section 5): one non-generic
//! implementation for both. CH1's sweep comes with task 3.

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

    /// A duty step: the position moves on and the first-sample zero ends.
    #[inline]
    fn tick(&mut self, period: NonZeroU32) {
        self.pos = (self.pos + 1) & 7;
        self.started = true;
        self.countdown = period.get() as u16;
    }

    /// Advances by `dt` clocks without integrating (ADR 0021 section 9),
    /// in closed form: positions only matter mod 8.
    #[inline]
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
        // only for long STOP stretches.
        let rest = u32::try_from(rest).unwrap_or((rest % (8 * u64::from(p.get()))) as u32);
        let steps = rest / p;
        self.tick(p);
        self.pos = (self.pos + (steps & 7) as u8) & 7;
        self.countdown = (p.get() - rest % p) as u16;
    }

    /// Advances by `dt` clocks (<= one sample window) and returns the
    /// analog level times clocks: `15 - 2 * digital` while the DAC is on
    /// (Pan Docs DAC: digital 0 -> +1, 15 -> -1), 0 while it is off.
    #[inline]
    pub(crate) fn integrate(&mut self, dt: u32) -> i32 {
        if !self.env.dac() {
            // The DAC off turns the channel off: nothing advances.
            return 0;
        }
        let dt_i = dt as i32;
        if !self.on {
            return 15 * dt_i;
        }
        let hi = 15 - 2 * i32::from(self.env.volume);
        let p = self.period();
        let mut dt = dt;
        let mut sum = 0;
        // Up to the first step (the only stretch that can be the
        // first-sample zero).
        let cd = u32::from(self.countdown);
        let level = if self.started && self.high() { hi } else { 15 };
        if dt < cd {
            self.countdown = (cd - dt) as u16;
            return level * dt_i;
        }
        sum += level * cd as i32;
        dt -= cd;
        self.tick(p);
        // Whole waveform periods: 8 steps contain the whole pattern.
        let wave = 8 * p.get();
        let full = dt / NonZeroU32::new(wave).unwrap_or(NonZeroU32::MIN);
        if full != 0 {
            let ones = DUTY[usize::from(self.duty & 3)].count_ones() as i32;
            sum += full as i32 * p.get() as i32 * (ones * hi + (8 - ones) * 15);
            dt -= full * wave;
        }
        // At most 8 more steps.
        loop {
            let cd = u32::from(self.countdown);
            let level = if self.high() { hi } else { 15 };
            if dt < cd {
                self.countdown = (cd - dt) as u16;
                return sum + level * dt as i32;
            }
            sum += level * cd as i32;
            dt -= cd;
            self.tick(p);
        }
    }

    /// A write to NRx1-NRx4 (`off` 1-4) with the frame sequencer at `fs`
    /// and SameBoy's 1 MHz phase `lf_div` (0 or 1) at the write.
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
        assert_eq!(a.integrate(100), 15 * 100, "digital 0 -> +15");
        // Retriggering an active channel keeps the output and uses the
        // short delay; lf_div 1 subtracts 2 clocks.
        sq.started = true;
        sq.write(4, 0x87, 0, 1);
        assert!(sq.started);
        assert_eq!(sq.countdown, 1024 + 6 - 2);
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
            int.integrate(dt);
            left -= dt;
        }
        assert_eq!(int, one);
    }

    #[test]
    fn the_dac_off_turns_the_channel_off() {
        let mut sq = playing(2, 15, 2000);
        assert!(sq.on);
        sq.write(2, 0x08, 0, 0); // volume 0, up: DAC still on
        assert!(sq.on);
        sq.write(2, 0x07, 0, 0); // bits 3-7 all 0: DAC off
        assert!(!sq.on);
        assert_eq!(sq.integrate(128), 0);
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
}
