//! The wave channel, CH3 (ADR 0021 section 5; Pan Docs "Audio Registers"
//! FF1A-FF1E and FF30-FF3F, "Audio details" wave channel; SameBoy
//! `Core/apu.c`, MIT).
//!
//! The channel reads wave RAM (`Memory::io[0x30..0x40]`) one nibble per
//! step, high nibble first; the methods that step take `io` for it.

use core::num::NonZeroU32;

use super::units::Length;

/// CH3's length counter range.
const LENGTH_MAX: u16 = 256;

/// The right shift of each NR32 volume code (bits 5-6): mute, 100%, 50%,
/// 25% (Pan Docs NR32; SameBoy `{4, 0, 1, 2}`).
const SHIFT: [u8; 4] = [4, 0, 1, 2];

/// Wave RAM's first byte in `Memory::io`.
const RAM: usize = 0x30;

/// The wave channel's state. The register bytes and wave RAM are in
/// `Memory::io`. All-zero is the power-off state: DAC off, volume code 0
/// (mute, so the cached sum 0 is exact), sample buffer 0.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Wave {
    /// Base clocks to the next sample step (>= 1 while on).
    pub(crate) countdown: u16,
    /// The 11-bit frequency (NR33, NR34 bits 0-2): the period used at the
    /// next reload.
    pub(crate) freq: u16,
    pub(crate) len: Length,
    /// The waveform sum: the 32 wave RAM samples' digital outputs
    /// (`nibble >> shift`), summed. Recomputed at every CH3 register or wave RAM
    /// write (`resum`), so the closed-form integral of whole waveform
    /// periods is one multiplication.
    pub(crate) sum: u16,
    /// The sample index, 0-31: the nibble the buffer was read for.
    pub(crate) index: u8,
    /// The sample buffer: the wave RAM byte read at the last step. A
    /// trigger keeps it (Pan Docs: the buffer is not refilled; SameBoy
    /// `current_sample_byte`).
    pub(crate) buffer: u8,
    /// NR32 bits 5-6.
    pub(crate) vol: u8,
    /// NR30 bit 7.
    pub(crate) dac: bool,
    /// The NR52 bit.
    pub(crate) on: bool,
}

/// The nibble of `byte` that sample `index` plays: high for even indices
/// (Pan Docs: "the upper nibble is played first").
#[inline]
fn nibble(byte: u8, index: u8) -> u8 {
    if index & 1 == 0 {
        byte >> 4
    } else {
        byte & 0xF
    }
}

impl Wave {
    pub(crate) const ZERO: Self = Self {
        countdown: 0,
        freq: 0,
        len: Length::ZERO,
        sum: 0,
        index: 0,
        buffer: 0,
        vol: 0,
        dac: false,
        on: false,
    };

    /// Base clocks per sample step: `(2048 - f) * 2`, 2-4096 (Pan Docs:
    /// the wave channel runs at twice the square channels' step rate).
    #[inline]
    fn period(&self) -> NonZeroU32 {
        let p = (2048 - u32::from(self.freq & 0x7FF)) * 2;
        NonZeroU32::new(p).unwrap_or(NonZeroU32::MIN)
    }

    #[inline]
    fn shift(&self) -> u8 {
        SHIFT[usize::from(self.vol & 3)]
    }

    /// The digital output, 0-15 (PCM34 low nibble): the buffered nibble
    /// shifted by the NR32 code while on, else 0. Right after a trigger
    /// that is the old buffer's high nibble (index 0; SameBoy NR34).
    #[inline]
    pub(crate) fn digital(&self) -> u8 {
        if self.on {
            nibble(self.buffer, self.index) >> self.shift()
        } else {
            0
        }
    }

    /// The digital output `dt` clocks after the state (PCM34), from a copy.
    #[inline(never)]
    pub(crate) fn digital_at(&self, dt: u64, io: &[u8; 0x80]) -> u8 {
        let mut w = *self;
        w.advance(dt, io);
        w.digital()
    }

    /// The wave RAM offset (0-15) a CPU access to offset `i` reaches. While
    /// CH3 is on, CGB redirects every access to the byte being played,
    /// `index / 2`, at any time (SameBoy `GB_apu_read`/`GB_apu_write` on
    /// CGB-E and older; blargg `cgb_sound` 09, 12).
    #[inline]
    pub(crate) fn ram_offset(&self, i: usize) -> usize {
        (if self.on {
            usize::from(self.index >> 1)
        } else {
            i
        }) & 15
    }

    /// Wave RAM offset `i` as read `dt` clocks after the state, from a copy
    /// (pure, ADR 0021 section 3).
    #[inline(never)]
    pub(crate) fn read_ram_at(&self, dt: u64, io: &[u8; 0x80], i: usize) -> u8 {
        let mut w = *self;
        w.advance(dt, io);
        io[RAM + w.ram_offset(i)]
    }

    /// Recomputes the cached waveform sum from wave RAM and NR32: wave RAM
    /// as two 64-bit words of 16 nibbles, each nibble shifted in place
    /// (`(w >> s) & mask` keeps its own bits only), the nibbles summed
    /// into bytes (<= 30 each) and the bytes by one multiplication (<= 240).
    /// SWAR instead of a 32-nibble loop: 208 B instead of 380 on thumbv7em.
    #[inline(never)]
    pub(crate) fn resum(&mut self, io: &[u8; 0x80]) {
        const NIBBLES: u64 = 0x1111_1111_1111_1111;
        const LOW: u64 = 0x0F0F_0F0F_0F0F_0F0F;
        let s = self.shift();
        let mask = u64::from(0xF_u8 >> s) * NIBBLES;
        let word = |at: usize| {
            let bytes = io[at..at + 8].try_into().unwrap_or([0; 8]);
            let y = u64::from_le_bytes(bytes) >> s & mask;
            let t = (y & LOW) + (y >> 4 & LOW);
            (t.wrapping_mul(0x0101_0101_0101_0101) >> 56) as u16
        };
        self.sum = word(RAM) + word(RAM + 8);
    }

    /// A sample step: the index moves on and the buffer loads its byte.
    #[inline]
    fn tick(&mut self, period: NonZeroU32, io: &[u8; 0x80]) {
        self.index = (self.index + 1) & 31;
        self.buffer = io[RAM + usize::from(self.index >> 1)];
        self.countdown = period.get() as u16;
    }

    /// Advances by `dt` clocks without integrating (ADR 0021 section 9),
    /// in closed form: the index only matters mod 32, and the buffer is
    /// the byte at the final index (wave RAM is constant over a sync span:
    /// its writes sync first).
    #[inline(never)]
    pub(crate) fn advance(&mut self, dt: u64, io: &[u8; 0x80]) {
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
        // Indices repeat every 32 periods (the u64 remainder is for long
        // STOP stretches only, as in `Square::advance`).
        let rest = u32::try_from(rest).unwrap_or((rest % (32 * u64::from(p.get()))) as u32);
        let steps = rest / p;
        self.index = (self.index + (steps & 31) as u8) & 31;
        self.tick(p, io);
        self.countdown = (p.get() - rest % p) as u16;
    }

    /// Advances by `dt` clocks (<= one sample window) and returns the
    /// analog level times clocks: `15 - 2 * digital` while the DAC is on,
    /// 0 while it is off (Pan Docs DAC). Whole 32-step waveform periods
    /// use the cached sum, so at most 31 steps iterate.
    #[inline(never)]
    pub(crate) fn integrate(&mut self, dt: u32, io: &[u8; 0x80]) -> i32 {
        if !self.dac {
            return 0;
        }
        let level = |w: &Self| 15 - 2 * i32::from(w.digital());
        if !self.on {
            return 15 * dt as i32;
        }
        let p = self.period();
        let mut dt = dt;
        let mut sum = 0;
        let cd = u32::from(self.countdown);
        if dt >= cd {
            sum += level(self) * cd as i32;
            dt -= cd;
            self.tick(p, io);
            let wave = 32 * p.get();
            let full = dt / NonZeroU32::new(wave).unwrap_or(NonZeroU32::MIN);
            if full != 0 {
                let per = 32 * 15 - 2 * i32::from(self.sum);
                sum += full as i32 * p.get() as i32 * per;
                dt -= full * wave;
            }
        }
        loop {
            let cd = u32::from(self.countdown);
            if dt < cd {
                self.countdown = (cd - dt) as u16;
                return sum + level(self) * dt as i32;
            }
            sum += level(self) * cd as i32;
            dt -= cd;
            self.tick(p, io);
        }
    }

    /// A write to NR30-NR34 (`off` 0-4) with the frame sequencer at `fs`.
    /// The byte is already stored in `io`.
    #[inline]
    pub(crate) fn write(&mut self, off: usize, value: u8, fs: u8, io: &[u8; 0x80]) {
        match off {
            0 => {
                self.dac = value & 0x80 != 0;
                if !self.dac {
                    self.on = false;
                }
            }
            1 => self.len.load(LENGTH_MAX, u16::from(value)),
            2 => self.vol = value >> 5 & 3,
            3 => self.freq = self.freq & 0x700 | u16::from(value),
            4 => {
                self.freq = self.freq & 0xFF | u16::from(value & 7) << 8;
                if self.len.write_nrx4(LENGTH_MAX, value, fs) {
                    self.on = false;
                }
                if value & 0x80 != 0 {
                    self.trigger(io);
                }
            }
            _ => {}
        }
    }

    /// A trigger (NR34 bit 7; the length reload is in `Length::write_nrx4`).
    /// The index restarts at 0 and the sample buffer is kept (SameBoy:
    /// "we don't change the sample just yet! This was verified on
    /// hardware"), except that a retrigger one 2 MHz tick before a step
    /// (SameBoy `sample_countdown == 0`, our countdown <= 2) loads wave
    /// RAM byte 0. No wave RAM corruption: that is DMG only (SameBoy
    /// `!GB_is_cgb`; blargg `cgb_sound` 10). The first step comes after
    /// SameBoy's `(f ^ 0x7FF) + 3` ticks, n firing after n + 1: `(2051 -
    /// f) * 2` base clocks, with no `lf_div` term (docs/reference/apu.md).
    fn trigger(&mut self, io: &[u8; 0x80]) {
        if self.on && self.countdown <= 2 {
            self.buffer = io[RAM];
        }
        self.index = 0;
        self.countdown = (self.period().get() + 6) as u16;
        self.on = self.dac;
    }

    /// A frame-sequencer length step (odd steps, 256 Hz).
    #[inline]
    pub(crate) fn clock_length(&mut self) {
        if self.len.clock() {
            self.on = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wave RAM bytes 0x01, 0x23, ..., 0xEF (nibbles 0-15, then again).
    fn io_ramp() -> [u8; 0x80] {
        let mut io = [0; 0x80];
        for i in 0..16 {
            io[RAM + i] = ((2 * i as u8) & 0xF) << 4 | (2 * i as u8 + 1) & 0xF;
        }
        io
    }

    /// CH3 with its DAC on, volume code `vol`, triggered at `f`.
    fn playing(io: &[u8; 0x80], vol: u8, f: u16) -> Wave {
        let mut w = Wave::ZERO;
        w.write(0, 0x80, 0, io);
        w.write(2, vol << 5, 0, io);
        w.resum(io);
        w.write(3, f as u8, 0, io);
        w.write(4, 0x80 | (f >> 8) as u8, 0, io);
        w
    }

    #[test]
    fn trigger_delay_is_2051_minus_f_times_2_then_steps_every_2048_minus_f_times_2() {
        let io = io_ramp();
        let w = playing(&io, 1, 0x700); // period (2048 - 0x700) * 2 = 512
        assert_eq!(w.countdown, 512 + 6);
        let at = |dt| {
            let mut c = w;
            c.advance(dt, &io);
            (c.index, c.buffer)
        };
        assert_eq!(at(517), (0, 0), "before the first step");
        assert_eq!(at(518), (1, 0x01), "index 1: byte 0's low nibble");
        assert_eq!(at(518 + 511), (1, 0x01));
        assert_eq!(at(518 + 512), (2, 0x23));
        // 32 steps later: the same index.
        assert_eq!(at(518 + 512 + 32 * 512), (2, 0x23));
    }

    #[test]
    fn trigger_keeps_the_sample_buffer_and_never_writes_wave_ram() {
        let io = io_ramp();
        let mut w = playing(&io, 1, 0x700);
        w.advance(518 + 5 * 512, &io); // index 6: byte 3 = 0x67, high nibble
        assert_eq!((w.index, w.digital()), (6, 6));
        w.advance(100, &io);
        let before = io;
        w.write(4, 0x87, 0, &io);
        assert_eq!((w.index, w.buffer), (0, 0x67));
        assert_eq!(w.digital(), 6, "old buffer, high nibble (index 0)");
        assert_eq!(io, before);
        // A retrigger one tick (2 clocks) before a step loads byte 0.
        w.advance(u64::from(w.countdown) - 2, &io);
        w.write(4, 0x87, 0, &io);
        assert_eq!(w.buffer, 0x01);
    }

    #[test]
    fn nr32_shifts_the_nibble() {
        let mut io = [0; 0x80];
        io[RAM] = 0xFF;
        for (vol, want) in [(0, 0), (1, 15), (2, 7), (3, 3)] {
            let mut w = playing(&io, vol, 0x7FF);
            w.advance(u64::from(w.countdown), &io);
            assert_eq!(w.digital(), want, "code {vol}");
        }
    }

    #[test]
    fn length_counts_256_and_the_dac_gates_the_channel() {
        let io = io_ramp();
        let mut w = playing(&io, 1, 0);
        w.write(1, 0, 0, &io);
        w.write(4, 0x40, 0, &io); // enable length
        for _ in 0..255 {
            w.clock_length();
        }
        assert!(w.on);
        w.clock_length();
        assert!(!w.on, "256 steps");
        // A trigger with the DAC off leaves the channel off; a DAC-off
        // write turns it off.
        let mut w = playing(&io, 1, 0);
        w.write(0, 0x00, 0, &io);
        assert!(!w.on);
        w.write(4, 0x80, 0, &io);
        assert!(!w.on);
        assert_eq!(w.integrate(100, &io), 0, "DAC off: no level");
    }

    #[test]
    fn ram_access_while_on_reaches_the_byte_being_played() {
        let io = io_ramp();
        let w = playing(&io, 1, 0x700);
        assert_eq!(w.read_ram_at(0, &io, 5), 0x01, "index 0: byte 0");
        assert_eq!(w.read_ram_at(518 + 512, &io, 5), 0x23, "index 2: byte 1");
        assert_eq!(w.ram_offset(9), 0);
        assert_eq!(Wave::ZERO.read_ram_at(0, &io, 5), 0xAB, "off: as addressed");
    }

    /// A step-by-step reference integral: one clock at a time.
    fn slow(w: &mut Wave, dt: u32, io: &[u8; 0x80]) -> i32 {
        (0..dt)
            .map(|_| {
                let l = 15 - 2 * i32::from(w.digital());
                w.advance(1, io);
                l
            })
            .sum()
    }

    #[test]
    fn integrate_matches_a_clock_by_clock_reference_and_advance() {
        let mut io = io_ramp();
        io[RAM + 7] = 0xF3;
        for f in [2047, 2046, 2041, 2000, 0x700] {
            for vol in 0..4 {
                let mut a = playing(&io, vol, f);
                let (mut b, mut c) = (a, a);
                for dt in [1, 128, 37, 128, 128, 128, 91, 128, 128, 128, 128] {
                    assert_eq!(
                        a.integrate(dt, &io),
                        slow(&mut b, dt, &io),
                        "f {f} vol {vol}"
                    );
                    c.advance(u64::from(dt), &io);
                    assert_eq!(a, b);
                    assert_eq!(a, c);
                }
            }
        }
    }

    #[test]
    fn resum_sums_the_shifted_nibbles() {
        let io = io_ramp(); // nibbles 0-15 twice: 240
        let mut w = Wave::ZERO;
        for (vol, want) in [
            (0, 0),
            (1, 240),
            (2, 2 * (0..16).map(|n| n >> 1).sum::<u16>()),
        ] {
            w.vol = vol;
            w.resum(&io);
            assert_eq!(w.sum, want);
        }
    }
}
