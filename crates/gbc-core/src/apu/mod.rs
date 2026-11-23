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
//! `Apu::advance`, [`Apu::step`] and the NR52 active bits. Task 1(b): CH2.

mod mix;
mod square;
mod units;

pub(crate) use mix::Output;

use crate::memory::Memory;
use square::Square;

/// One stereo output sample, written by the core into the caller's audio
/// buffer.
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
    /// CH1 and CH2. CH1 is decoded from task 3 on (sweep).
    pub(crate) sq: [Square; 2],
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
    /// NR52 bits 0, 2, 3: CH1, CH3 and CH4 until their tasks (3-5) give
    /// them a channel; CH2's bit is `sq[1].on`.
    pub(crate) active: u8,
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
                active: 0,
                phase: 0,
            },
            sq: [Square::ZERO; 2],
        }
    }

    /// The state the CGB boot ROM leaves at clock 0: powered on, NR52 = F1
    /// (CH1 on, its DAC on, volume 0 and the envelope stopped: the boot
    /// chime has decayed by hand-over; ADR 0021 section 7, provisional),
    /// frame sequencer at step 0.
    pub(crate) fn post_boot(&mut self) {
        *self = Self::new();
        self.fs.power = true;
        self.fs.active = 0x01;
    }

    #[inline]
    pub(crate) fn powered(&self) -> bool {
        self.fs.power
    }

    /// NR52 as read: power, three unused bits as 1, the channel bits
    /// (Pan Docs: FF26 - NR52).
    pub(crate) fn nr52(&self) -> u8 {
        0x70 | u8::from(self.fs.power) << 7 | self.fs.active | u8::from(self.sq[1].on) << 1
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
            active: 0,
            phase: ((now >> 1) & 1) as u8,
        };
    }

    /// A DIV-APU falling edge (after a sync to its clock): the frame
    /// sequencer steps, unless the edge is the one skipped after power on.
    /// Pan Docs "Audio details", DIV-APU; SameBoy `GB_apu_div_event`.
    pub(crate) fn step(&mut self) {
        let fs = &mut self.fs;
        if fs.skip {
            fs.skip = false;
            return;
        }
        fs.step = (fs.step + 1) & 7;
        let step = fs.step;
        if step & 1 == 1 {
            // The length counters, 256 Hz.
            self.sq[1].clock_length();
        }
        // `step & 3 == 3`: the sweep (128 Hz, task 3).
        if step == 0 {
            // The envelopes, 64 Hz.
            self.sq[1].clock_envelope();
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
    pub(crate) fn write(&mut self, reg: usize, value: u8, now: u64) {
        if let 0x16..=0x19 = reg {
            let (fs, lf) = (self.fs.step, self.lf_div(now));
            self.sq[1].write(reg - 0x15, value, fs, lf);
        }
    }

    /// Advances the channels by `dt` clocks without integrating (the
    /// empty-buffer path, ADR 0021 section 9).
    #[inline]
    fn advance(&mut self, dt: u64) {
        self.sq[1].advance(dt);
    }

    /// Advances the channels by `dt` clocks (at most one sample window) and
    /// returns each channel's analog level (-15..15 while its DAC is on,
    /// else 0) times clocks, summed over the span, for CH1-CH4. CH1, CH3
    /// and CH4 are silent (DAC off) until their tasks.
    #[inline]
    fn integrate(&mut self, dt: u32) -> [i32; 4] {
        [0, self.sq[1].integrate(dt), 0, 0]
    }
}

/// FF26 (NR52), wave RAM (FF30-FF3F) and FF76/FF77 (PCM12/PCM34) at clock
/// `now`, by the low 7 address bits. Pure: a channel whose state a read
/// depends on is copied and the copy advanced to `now` (ADR 0021 section
/// 3). Out of line, off `read_high`.
#[inline(never)]
pub(crate) fn read_apu(mem: &Memory, now: u64, reg: usize) -> u8 {
    match reg {
        0x26 => mem.apu.nr52(),
        // PCM12/PCM34: each channel's digital output (0-15) if it is on,
        // else 0 (SameBoy `GB_IO_PCM12`/`PCM34`), at `now`: a copy of the
        // channel is advanced. CH1, CH3, CH4: tasks 3-5.
        0x76 => {
            let mut ch2 = mem.apu.sq[1];
            ch2.advance(now.saturating_sub(mem.apu.out.synced));
            ch2.digital() << 4
        }
        0x77 => 0x00,
        // Wave RAM as stored. CH3 playing: the byte it reads (task 4).
        _ => mem.io[reg],
    }
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
}
