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

/// The sum of the 8 nibbles of `w` (<= 120): nibbles into bytes (<= 30
/// each), bytes by one multiplication.
#[inline]
fn nibble_sum(w: u32) -> u32 {
    const LOW: u32 = 0x0F0F_0F0F;
    let t = (w & LOW) + (w >> 4 & LOW);
    t.wrapping_mul(0x0101_0101) >> 24
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

    /// Wave RAM as the digital outputs in play order: four 32-bit words of
    /// 8 nibbles, sample `8j` in word `j`'s top nibble, each nibble shifted
    /// by the NR32 code in place (`(w >> s) & mask` keeps its own bits).
    /// 32-bit words: 64-bit shifts are long on thumbv7em.
    #[inline]
    fn words(&self, io: &[u8; 0x80]) -> [u32; 4] {
        let s = self.shift();
        let mask = u32::from(0xF_u8 >> s) * 0x1111_1111;
        core::array::from_fn(|j| {
            let at = RAM + 4 * j;
            let bytes = io[at..at + 4].try_into().unwrap_or([0; 4]);
            u32::from_be_bytes(bytes) >> s & mask
        })
    }

    /// Recomputes the cached waveform sum from wave RAM and NR32 (SWAR
    /// instead of a 32-nibble loop).
    pub(crate) fn resum(&mut self, io: &[u8; 0x80]) {
        self.sum = self.words(io).into_iter().map(nibble_sum).sum::<u32>() as u16;
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
        // STOP stretches only, as in `Square::advance`). `32 * p <= 131,072`.
        let rest = u32::try_from(rest).unwrap_or_else(|_| super::rem_long(rest, 32 * p.get()));
        let steps = rest / p;
        self.index = (self.index + (steps & 31) as u8) & 31;
        self.tick(p, io);
        self.countdown = (p.get() - rest % p) as u16;
    }

    /// Adds to `out[i]` the analog level times clocks over the `i`-th of
    /// the consecutive spans `segs` (each at most one sample window) and
    /// advances over them: `15 - 2 * digital` while the DAC is on (Pan
    /// Docs DAC), nothing while it is off. A batch per call: the period,
    /// NR32 and wave RAM are constant over a sync span. Each span in closed
    /// form: after the first step the buffer always holds the byte at the
    /// index, so the `n` whole sample periods are a run of wave RAM
    /// nibbles, a difference of two prefix sums from the SWAR words and the
    /// cached sum, and the last partial period is the nibble at the final
    /// index. Exact: the same state and integral as stepping
    /// (`integrate_matches_a_clock_by_clock_reference_and_advance`).
    #[inline(never)]
    pub(crate) fn integrate(&mut self, segs: &[u8], io: &[u8; 0x80], out: &mut [i16]) {
        if !self.dac {
            return;
        }
        if !self.on {
            return super::mix::constant(segs, out, 15);
        }
        let spans = out.iter_mut().zip(segs);
        let p = self.period();
        let shift = self.shift();
        let w = self.words(io);
        let (s0, s1, s2) = (nibble_sum(w[0]), nibble_sum(w[1]), nibble_sum(w[2]));
        let before = [0, s0, s0 + s1, s0 + s1 + s2];
        let sum = u32::from(self.sum);
        // The sum of the samples `0..k` of the endless waveform: whole
        // waveforms, whole words, then the word's top `k % 8` nibbles.
        let prefix = |k: u32| {
            let j = (k >> 3 & 3) as usize;
            (k >> 5) * sum + before[j] + nibble_sum(w[j] & !(u32::MAX >> (4 * (k & 7))))
        };
        for (o, &dt) in spans {
            let dt = u32::from(dt);
            let cd = u32::from(self.countdown);
            // Up to the first step: the buffer (after a trigger, the old one).
            let now = u32::from(nibble(self.buffer, self.index) >> shift);
            let high = if dt < cd {
                self.countdown = (cd - dt) as u16;
                now * dt
            } else {
                let rest = dt - cd;
                let (n, r) = (rest / p, rest % p);
                // Samples `first..first + n`, a whole period each.
                let first = u32::from(self.index + 1) & 31;
                let to = first + n;
                let end = to & 31;
                let last = w[(end >> 3 & 3) as usize] >> (28 - 4 * (end & 7)) & 15;
                self.index = end as u8;
                self.buffer = io[RAM + (end >> 1) as usize];
                self.countdown = (p.get() - r) as u16;
                now * cd + p.get() * (prefix(to) - prefix(first)) + r * last
            };
            *o += (15 * dt as i32 - 2 * high as i32) as i16;
        }
    }

    /// One span through `integrate` (tests).
    #[cfg(test)]
    pub(crate) fn integrate1(&mut self, dt: u32, io: &[u8; 0x80]) -> i32 {
        let mut out = [0];
        self.integrate(&[u8::try_from(dt).unwrap()], io, &mut out);
        i32::from(out[0])
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
    fn advance_over_2_pow_32_clocks_matches_shorter_spans() {
        let io = io_ramp();
        for f in [0, 1000, 1999, 2047] {
            crate::apu::check_long_advance(playing(&io, 1, f), |c, dt| c.advance(dt, &io));
        }
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
        assert_eq!(w.integrate1(100, &io), 0, "DAC off: no level");
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
                for dt in [
                    1, 128, 37, 128, 2, 3, 64, 65, 63, 128, 91, 128, 127, 128, 128,
                ] {
                    assert_eq!(
                        a.integrate1(dt, &io),
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
