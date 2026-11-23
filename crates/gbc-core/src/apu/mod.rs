//! Audio processing unit (ADR 0021).
//!
//! The APU catches up lazily (ADR 0002): nothing runs per M-cycle.
//! [`Apu::sync`] brings the state from `Output::synced` to a clock at the
//! points where it changes discontinuously (APU register writes, DIV-APU
//! frame-sequencer steps) and where the output needs it (the audio buffer
//! boundary, the end of each run call). Register reads stay pure
//! ([`read_apu`]).
//!
//! The register bytes stay in `Memory::io` (FF10-FF25 raw, wave RAM
//! FF30-FF3F); [`Apu`] (in `Memory::apu`, not `Hw`) holds only the internal
//! state. All-zero is a valid `Apu`: powered off.
//!
//! Task 1(a) is the framework: power, the frame sequencer and the output
//! pipeline. The channels plug into `Apu::write`, `Apu::integrate`,
//! `Apu::advance`, [`Apu::step`] and the NR52 active bits. Task 1(b): CH2;
//! task 3: CH1 with its sweep; task 4: CH3 (`write_wave`, `read_apu`);
//! task 5: CH4.

mod mix;
mod noise;
mod square;
mod units;
mod wave;

pub(crate) use mix::Output;

use crate::memory::Memory;
use noise::Noise;
use square::{Square, Sweep};
use wave::Wave;

/// One stereo output sample, written by the core into the caller's audio
/// buffer at [`AUDIO_SAMPLE_RATE`](crate::AUDIO_SAMPLE_RATE): the average
/// of the mixed signal over its
/// [`AUDIO_SAMPLE_CLOCKS`](crate::AUDIO_SAMPLE_CLOCKS)-clock window (a box
/// filter).
///
/// Scale: 64 per unit of mixed level, the sum of the panned channels'
/// analog levels (each -15..=15) times the side's NR50 volume plus 1. Full
/// scale is therefore ±30,720 (4 channels x 15 x 8 x 64), with no
/// clamping. DC is kept (a silent but powered DAC is a constant, not 0):
/// the DC-blocking high-pass filter and resampling to the device rate are
/// the front end's job.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StereoSample {
    pub left: i16,
    pub right: i16,
}

/// Output samples are averages over windows of `1 << SAMPLE_PERIOD_LOG2`
/// base clocks: 128, i.e. 32,768 Hz in both CPU speeds (ADR 0021 section
/// 8). The one knob for a later rate change.
pub(crate) const SAMPLE_PERIOD_LOG2: u32 = 7;

/// Base clocks per output sample window.
pub(crate) const SAMPLE_PERIOD: u64 = 1 << SAMPLE_PERIOD_LOG2;

/// The APU's internal state. The register bytes are in `Memory::io`.
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Apu {
    /// The output stream: kept across power cycles.
    pub(crate) out: Output,
    pub(crate) fs: Fs,
    /// CH1 and CH2.
    pub(crate) sq: [Square; 2],
    /// CH1's sweep.
    pub(crate) sweep: Sweep,
    /// CH3.
    pub(crate) wave: Wave,
    /// CH4.
    pub(crate) noise: Noise,
}

/// Power and the frame sequencer (ADR 0021 section 4).
#[repr(C)]
#[derive(Clone, Copy)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
pub(crate) struct Fs {
    /// NR52 bit 7.
    pub(crate) power: bool,
    /// Frame-sequencer steps since power on, mod 8 (SameBoy `div_divider`).
    pub(crate) step: u8,
    /// The next DIV-APU edge is skipped (power on with the APU bit high).
    pub(crate) skip: bool,
    /// SameBoy's 1 MHz phase (`lf_div`) at power on, from the clock: the
    /// square trigger delay (ADR 0021 section 5, task 1(b)).
    pub(crate) phase: u8,
}

impl Apu {
    /// Powered off, all zero (`Memory::new` is `const` all-zero).
    pub(crate) const fn new() -> Self {
        Self {
            out: Output::new(),
            fs: Fs {
                power: false,
                step: 0,
                skip: false,
                phase: 0,
            },
            sq: [Square::ZERO; 2],
            sweep: Sweep::ZERO,
            wave: Wave::ZERO,
            noise: Noise::ZERO,
        }
    }

    /// The state the CGB boot ROM leaves at clock 0: powered on, NR52 = F1
    /// (CH1 on, its DAC on, volume 0 and the envelope stopped: the boot
    /// chime has decayed by hand-over; ADR 0021 section 7, provisional),
    /// frame sequencer at step 0. CH1 matches the post-boot registers
    /// (NR10 = 80: sweep off; NR11 = BF: duty 2; NR12 = F3); its frequency,
    /// length and duty position are not observable through them and stay 0
    /// (provisional).
    pub(crate) fn post_boot(&mut self) {
        *self = Self::new();
        self.fs.power = true;
        let ch1 = &mut self.sq[0];
        ch1.on = true;
        ch1.started = true;
        ch1.duty = 2;
        ch1.env.nrx2 = 0xF3;
    }

    #[inline]
    pub(crate) fn powered(&self) -> bool {
        self.fs.power
    }

    /// NR52 as read: power, three unused bits as 1, the channel bits
    /// (Pan Docs: FF26 - NR52).
    pub(crate) fn nr52(&self) -> u8 {
        0x70 | u8::from(self.fs.power) << 7
            | u8::from(self.noise.on) << 3
            | u8::from(self.wave.on) << 2
            | u8::from(self.sq[1].on) << 1
            | u8::from(self.sq[0].on)
    }

    /// NR52 bit 7 from 1 to 0, after a sync: everything but the output
    /// stream becomes the zero state, which on CGB includes the length
    /// counters (Pan Docs: Audio Registers; blargg `cgb_sound` 08). The
    /// caller zeroes FF10-FF25; wave RAM is kept (SameBoy `GB_apu_write`).
    pub(crate) fn power_off(&mut self) {
        *self = Self {
            out: self.out,
            ..Self::new()
        };
    }

    /// NR52 bit 7 from 0 to 1 at clock `now`. The zero state already has
    /// the duty positions and the wave sample buffer at 0 (Pan Docs). If
    /// the counter's DIV-APU bit is 1 (`apu_bit_high`), the first falling
    /// edge comes too soon and is skipped (SameBoy NR52 power on:
    /// `skip_div_event`, `div_divider = 1`).
    pub(crate) fn power_on(&mut self, now: u64, apu_bit_high: bool) {
        self.fs = Fs {
            power: true,
            step: u8::from(apu_bit_high),
            skip: apu_bit_high,
            phase: ((now >> 1) & 1) as u8,
        };
    }

    /// A DIV-APU falling edge (after a sync to its clock): the frame
    /// sequencer steps, unless the edge is the one skipped after power on.
    /// Pan Docs "Audio details", DIV-APU; SameBoy `GB_apu_div_event`.
    #[inline(never)]
    pub(crate) fn step(&mut self) {
        let fs = &mut self.fs;
        if fs.skip {
            fs.skip = false;
            return;
        }
        fs.step = (fs.step + 1) & 7;
        let step = fs.step;
        // Length (odd steps, 256 Hz) before the sweep (SameBoy
        // `GB_apu_div_event`); the envelopes on step 0 (64 Hz).
        for sq in &mut self.sq {
            sq.frame_step(step);
        }
        self.noise.frame_step(step);
        if step & 1 == 1 {
            self.wave.clock_length();
        }
        if step & 3 == 3 {
            // The sweep, 128 Hz.
            self.sweep.clock(&mut self.sq[0]);
        }
    }

    /// SameBoy's 1 MHz phase `lf_div` at clock `now`: 1 at power on, then
    /// toggling every 2 base clocks (SameBoy `GB_apu_init`, `GB_apu_run`).
    /// The exact fit is deferred (ADR 0021 section 12).
    #[inline]
    fn lf_div(&self, now: u64) -> u8 {
        ((now >> 1) as u8 ^ self.fs.phase ^ 1) & 1
    }

    /// A write to a channel register FF10-FF25 (by the low 7 address bits)
    /// at clock `now`, after a sync, with the APU on. The byte is already
    /// stored in `Memory::io`.
    #[inline(never)]
    pub(crate) fn write(&mut self, reg: usize, value: u8, now: u64) {
        // FF10-FF14 are CH1's NR10-NR14 and FF15-FF19 CH2's (FF15 unused)
        // NR21-NR24: one call site for both squares keeps a single inlined
        // copy. NR10 and FF15 (offset 0) are no-ops in `Square::write`.
        let i = reg.wrapping_sub(0x10);
        if i < 10 {
            let ch = usize::from(i >= 5);
            let (fs, lf) = (self.fs.step, self.lf_div(now));
            self.sq[ch].write(i - 5 * ch, value, fs, lf);
        }
        // NR10, CH1's trigger after the channel's own, or CH4. One
        // unconditional tail call instead of a test on every exit (code
        // size).
        self.write_rest(reg, value);
    }

    /// The rest of a write to FF10-FF25 (`reg`), after `Square::write`:
    /// NR10, or CH1's trigger (NR14 bit 7) after the channel's own, go to
    /// the sweep; FF1F-FF23 to CH4 (NR41-NR44 at offsets 1-4, FF1F unused);
    /// nothing for the others. Out of line, off `Apu::write`'s common path.
    #[inline(never)]
    fn write_rest(&mut self, reg: usize, value: u8) {
        let ch1 = &mut self.sq[0];
        match reg {
            0x10 => self.sweep.write(value, ch1),
            0x14 if value & 0x80 != 0 => self.sweep.trigger(ch1),
            0x1F..=0x23 => self.noise.write(reg - 0x1F, value, self.fs.step),
            _ => {}
        }
    }

    /// Advances the channels by `dt` clocks without integrating (the
    /// empty-buffer path, ADR 0021 section 9).
    #[inline]
    fn advance(&mut self, dt: u64, io: &[u8; 0x80]) {
        for sq in &mut self.sq {
            sq.advance(dt);
        }
        self.wave.advance(dt, io);
        self.noise.advance(dt);
    }

    /// Advances the channels by `dt` clocks (at most one sample window) and
    /// returns each channel's analog level (-15..15 while its DAC is on,
    /// else 0) times clocks, summed over the span, for CH1-CH4. `io` holds
    /// wave RAM.
    #[inline]
    fn integrate(&mut self, dt: u32, io: &[u8; 0x80]) -> [i32; 4] {
        let mut levels = [0; 4];
        for (level, sq) in levels.iter_mut().zip(&mut self.sq) {
            *level = sq.integrate(dt);
        }
        levels[2] = self.wave.integrate(dt, io);
        levels[3] = self.noise.integrate(dt);
        levels
    }
}

/// FF26 (NR52), wave RAM (FF30-FF3F) and FF76/FF77 (PCM12/PCM34) at clock
/// `now`, by the low 7 address bits. Pure: a channel whose state a read
/// depends on is copied and the copy advanced to `now` (ADR 0021 section
/// 3). Out of line, off `read_high`.
#[inline(never)]
pub(crate) fn read_apu(mem: &Memory, now: u64, reg: usize) -> u8 {
    let dt = now.saturating_sub(mem.apu.out.synced);
    match reg {
        0x26 => mem.apu.nr52(),
        // PCM12/PCM34: each channel's digital output (0-15) if it is on,
        // else 0 (SameBoy `GB_IO_PCM12`/`PCM34`), at `now`: a copy of the
        // channel is advanced.
        0x76 => {
            let [ch1, ch2] = &mem.apu.sq;
            ch1.digital_at(dt) | ch2.digital_at(dt) << 4
        }
        0x77 => mem.apu.wave.digital_at(dt, &mem.io) | mem.apu.noise.digital_at(dt) << 4,
        // Wave RAM: while CH3 plays, the byte it reads at `now` (CGB).
        _ => mem.apu.wave.read_ram_at(dt, &mem.io, reg),
    }
}

/// A write to NR30-NR34 (FF1A-FF1E) with the APU on, or to wave RAM
/// (FF30-FF3F, writable while off too), by the low 7 address bits, after a
/// sync. While CH3 is on, a wave RAM write lands in the byte being played
/// (CGB; SameBoy `GB_apu_write`). Every such write recomputes the cached
/// waveform sum (ADR 0021 sections 5, 8). Out of line, off `write_apu`.
#[inline(never)]
pub(crate) fn write_wave(mem: &mut Memory, reg: usize, value: u8) {
    let Memory { apu, io, .. } = mem;
    let wave = &mut apu.wave;
    match reg.checked_sub(0x30) {
        Some(i) => io[0x30 + wave.ram_offset(i)] = value,
        None => {
            io[reg & 0x7F] = value;
            wave.write(reg.wrapping_sub(0x1A), value, apu.fs.step, io);
        }
    }
    wave.resum(io);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apu_fits_in_128_bytes() {
        assert!(size_of::<Apu>() <= 128, "{}", size_of::<Apu>());
    }

    #[test]
    fn pcm12_reads_ch2_advanced_to_the_read_clock() {
        extern crate std;
        let mut mem = std::boxed::Box::new(Memory::new());
        mem.apu.post_boot();
        // Duty 25% (positions 0 and 7 high), volume 15, f = 0x400: 4096
        // clocks per step.
        mem.apu.write(0x16, 0x40, 0);
        mem.apu.write(0x17, 0xF0, 0);
        mem.apu.write(0x19, 0x84, 0);
        let first = u64::from(mem.apu.sq[1].countdown);
        assert_eq!(read_apu(&mem, 0, 0x76), 0x00, "not started");
        // Position 7 after 7 steps, with the state still at clock 0.
        let t = first + 6 * 4096 + 1;
        assert_eq!(read_apu(&mem, t, 0x76), 0xF0);
        assert_eq!(read_apu(&mem, t + 2 * 4096, 0x76), 0x00, "position 1");
        assert_eq!(mem.apu.out.synced, 0, "pure");
    }

    #[test]
    fn power_off_keeps_only_the_output_stream() {
        let mut apu = Apu::new();
        apu.post_boot();
        apu.out.synced = 1234;
        apu.fs.step = 5;
        apu.power_off();
        let mut want = Apu::new();
        want.out.synced = 1234;
        assert_eq!(apu, want);
        assert_eq!(apu.nr52(), 0x70);
    }

    #[test]
    fn frame_sequencer_skips_the_first_edge_after_a_high_power_on() {
        let mut apu = Apu::new();
        apu.power_on(0, true);
        assert_eq!((apu.fs.step, apu.fs.skip), (1, true));
        apu.step();
        assert_eq!((apu.fs.step, apu.fs.skip), (1, false));
        apu.step();
        assert_eq!(apu.fs.step, 2);
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.step();
        assert_eq!(apu.fs.step, 1);
        for _ in 0..7 {
            apu.step();
        }
        assert_eq!(apu.fs.step, 0, "wraps mod 8");
    }

    /// A powered APU at step 0 with CH1 (DAC on, volume 15, duty 50%)
    /// triggered at frequency `f` with NR10 = `nr10`, through `Apu::write`.
    fn ch1_playing(nr10: u8, f: u16) -> Apu {
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.write(0x10, nr10, 0);
        apu.write(0x11, 0x80, 0);
        apu.write(0x12, 0xF0, 0);
        apu.write(0x13, f as u8, 0);
        apu.write(0x14, 0x80 | (f >> 8) as u8, 0);
        apu
    }

    #[test]
    fn nr52_bit_0_follows_ch1() {
        let mut apu = Apu::new();
        apu.post_boot();
        assert_eq!(apu.nr52(), 0xF1);
        // Sweep overflow on the trigger turns CH1 off.
        let apu = ch1_playing(0x01, 0x7FF);
        assert_eq!(apu.nr52() & 1, 0);
        let apu = ch1_playing(0x00, 0x7FF);
        assert_eq!(apu.nr52() & 1, 1);
    }

    #[test]
    fn the_sweep_runs_on_frame_sequencer_steps_3_and_7() {
        let mut apu = ch1_playing(0x11, 0x100);
        let mut seen = [0; 8];
        for _ in 0..8 {
            apu.step();
            seen[usize::from(apu.fs.step)] = apu.sq[0].freq;
        }
        assert_eq!(
            seen,
            [0x240, 0x100, 0x100, 0x180, 0x180, 0x180, 0x180, 0x240]
        );
        // A sweep step that overflows turns CH1 off: NR52 bit 0.
        let mut apu = ch1_playing(0x11, 1280);
        for _ in 0..3 {
            apu.step();
        }
        assert_eq!(apu.nr52() & 1, 0);
    }

    #[test]
    fn nr10_writes_go_to_the_sweep_and_ff15_is_ignored() {
        let mut apu = ch1_playing(0x19, 0x400);
        let before = apu;
        apu.write(0x15, 0xFF, 0);
        assert_eq!(apu, before, "FF15 unused");
        apu.write(0x10, 0x11, 0); // clears negate after a negate calculation
        assert!(!apu.sq[0].on);
    }

    #[test]
    fn pcm12_low_nibble_is_ch1() {
        extern crate std;
        let mut mem = std::boxed::Box::new(Memory::new());
        mem.apu = ch1_playing(0x00, 0x400); // 4096 clocks per step
        let first = u64::from(mem.apu.sq[0].countdown);
        assert_eq!(read_apu(&mem, 0, 0x76), 0x00, "not started");
        // Duty 50%: positions 0 and 5-7 high; position 1 after the first step.
        assert_eq!(read_apu(&mem, first, 0x76), 0x00);
        assert_eq!(read_apu(&mem, first + 4 * 4096, 0x76), 0x0F, "position 5");
        // CH2 in the high nibble at the same time.
        mem.apu.write(0x17, 0xA0, 0);
        mem.apu.write(0x19, 0x87, 0);
        let t = first + 4 * 4096;
        assert_eq!(read_apu(&mem, t, 0x76) & 0x0F, 0x0F);
    }

    #[test]
    fn ch1_is_audible_in_the_mix() {
        // f = 0: 8192 clocks per step, so whole windows sit at one level.
        let mut apu = ch1_playing(0x00, 0x000);
        let mut io = [0u8; 0x80];
        io[0x24] = 0x77;
        io[0x25] = 0x11; // CH1 only, both sides
        let mut buf = [StereoSample::default(); 600];
        let mut len = 0;
        apu.sync(600 * SAMPLE_PERIOD, &io, &mut buf, &mut len);
        assert_eq!(len, 600);
        // Digital 15 -> analog -15, times volume 8, times 64.
        let low = -15 * 8 * 64;
        assert!(buf.iter().any(|s| s.left == low && s.right == low));
        assert_eq!(buf[0].left, 15 * 8 * 64, "first-sample zero: +15");
    }
}

#[cfg(test)]
mod noise_tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;

    /// A powered APU with CH4 (DAC on, volume 15, NR43 = 0: a step every
    /// 8 clocks, bit 0 first 1 after 15 steps) triggered at clock 0.
    fn ch4_playing() -> Box<Memory> {
        let mut mem = Box::new(Memory::new());
        mem.apu.power_on(0, false);
        for (reg, v) in [(0x21, 0xF0), (0x22, 0x00), (0x23, 0x80)] {
            mem.io[reg] = v;
            mem.apu.write(reg, v, 0);
        }
        mem
    }

    #[test]
    fn nr52_bit_3_and_pcm34_high_nibble_follow_ch4() {
        let mut mem = ch4_playing();
        assert_eq!(mem.apu.nr52(), 0xF8);
        assert_eq!(read_apu(&mem, 15 * 8 - 1, 0x77), 0x00);
        assert_eq!(read_apu(&mem, 15 * 8, 0x77), 0xF0);
        assert_eq!(mem.apu.out.synced, 0, "pure");
        mem.apu.write(0x21, 0x00, 0);
        assert_eq!(mem.apu.nr52(), 0xF0, "DAC off");
        assert_eq!(read_apu(&mem, 15 * 8, 0x77), 0x00);
    }

    #[test]
    fn frame_sequencer_clocks_the_ch4_length() {
        let mut mem = ch4_playing();
        mem.apu.write(0x20, 0x3F, 0); // length 1
        mem.apu.write(0x23, 0x40, 0); // enable at step 0: no extra clock
        assert_eq!(mem.apu.nr52() & 8, 8);
        mem.apu.step(); // step 1: length
        assert_eq!(mem.apu.nr52() & 8, 0);
    }

    #[test]
    fn power_off_clears_the_ch4_length() {
        let mut mem = ch4_playing();
        mem.apu.write(0x20, 0x3F, 0); // length 1
        mem.apu.power_off();
        mem.apu.power_on(0, false);
        mem.apu.write(0x21, 0xF0, 0);
        mem.apu.write(0x23, 0xC0, 0); // trigger, length on: 64 if cleared
        mem.apu.step(); // step 1: length
        assert_eq!(mem.apu.nr52() & 8, 8);
    }

    #[test]
    fn ch4_is_audible_in_the_mix_and_needs_no_buffer_to_advance() {
        let mut mem = ch4_playing();
        let mut io = mem.io;
        io[0x24] = 0x77;
        io[0x25] = 0x88; // CH4 only, both sides
        let mut quiet = mem.apu;
        let mut buf = [StereoSample::default(); 64];
        let mut len = 0;
        mem.apu.sync(64 * SAMPLE_PERIOD, &io, &mut buf, &mut len);
        quiet.sync(64 * SAMPLE_PERIOD, &io, &mut [], &mut 0);
        assert_eq!(mem.apu.noise, quiet.noise, "same state without a buffer");
        // Bit 0 is 0 for the first 120 clocks (+15), then 1 (-15) for 8.
        assert_eq!(buf[0].left, (120 - 8) * 15 * 8 / 2, "x 64 / 128");
        assert!(buf.iter().any(|s| s.right < 15 * 8 * 64));
    }
}

#[cfg(test)]
mod wave_tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;

    /// A powered APU with wave RAM 0x01, 0x23, ... and CH3 (DAC on,
    /// volume 100%) triggered at f = 0x700 (512 clocks per step, first
    /// step at 518), through `write_wave`.
    fn ch3_playing() -> Box<Memory> {
        let mut mem = Box::new(Memory::new());
        mem.apu.power_on(0, false);
        for i in 0..16 {
            write_wave(
                &mut mem,
                0x30 + i,
                (2 * i as u8) << 4 | (2 * i as u8 + 1) & 0xF,
            );
        }
        for (reg, v) in [(0x1A, 0x80), (0x1C, 0x20), (0x1D, 0x00), (0x1E, 0x87)] {
            write_wave(&mut mem, reg, v);
        }
        mem
    }

    #[test]
    fn nr52_bit_2_and_pcm34_low_nibble_follow_ch3() {
        let mem = ch3_playing();
        assert_eq!(mem.apu.nr52() & 4, 4);
        assert_eq!(read_apu(&mem, 0, 0x77), 0x00, "old buffer 0");
        assert_eq!(read_apu(&mem, 518, 0x77), 0x01);
        assert_eq!(read_apu(&mem, 518 + 2 * 512, 0x77), 0x03, "index 3");
        assert_eq!(mem.apu.out.synced, 0, "pure");
        let mut mem = mem;
        write_wave(&mut mem, 0x1A, 0x00);
        assert_eq!(mem.apu.nr52() & 4, 0, "DAC off");
        assert_eq!(read_apu(&mem, 518, 0x77), 0x00);
    }

    #[test]
    fn wave_ram_writes_while_on_reach_the_playing_byte_and_resum() {
        let mut mem = ch3_playing();
        let io = mem.io;
        mem.apu.sync(518 + 512, &io, &mut [], &mut 0); // index 2: byte 1
        assert_eq!(read_apu(&mem, 518 + 512, 0x3F), 0x23);
        let sum = mem.apu.wave.sum;
        write_wave(&mut mem, 0x3F, 0xFF);
        assert_eq!((mem.io[0x31], mem.io[0x3F]), (0xFF, 0xEF));
        assert_eq!(mem.apu.wave.sum, sum - 2 - 3 + 30, "resummed");
        write_wave(&mut mem, 0x1C, 0x40); // 50%
        // Nibbles 0-15 twice with 2, 3 replaced by 15, 15, each halved.
        assert_eq!(mem.apu.wave.sum, 112 - 2 + 14, "resummed on NR32");
        // A retrigger while on never corrupts wave RAM on CGB (blargg 10).
        for t in [518 + 512 - 2, 518 + 512 + 1, 518 + 3 * 512 - 2] {
            let io = mem.io;
            mem.apu.sync(t, &io, &mut [], &mut 0);
            write_wave(&mut mem, 0x1E, 0x87);
            assert_eq!(mem.io[0x30..0x40], io[0x30..0x40], "retrigger at {t}");
        }
    }

    #[test]
    fn ch3_is_audible_in_the_mix_and_its_length_runs_on_odd_steps() {
        let mut mem = ch3_playing();
        let mut io = mem.io;
        io[0x24] = 0x77;
        io[0x25] = 0x44; // CH3 only, both sides
        let mut buf = [StereoSample::default(); 64];
        let mut len = 0;
        mem.apu.sync(64 * SAMPLE_PERIOD, &io, &mut buf, &mut len);
        // Until 518: old buffer 0 -> analog +15; then nibble 1 -> +13.
        assert_eq!(buf[0].left, 15 * 8 * 64);
        assert_eq!(buf[5].right, 13 * 8 * 64);
        write_wave(&mut mem, 0x1B, 0xFF); // length 1
        write_wave(&mut mem, 0x1E, 0x47);
        mem.apu.step(); // step 1: length
        assert!(!mem.apu.wave.on);
    }
}
