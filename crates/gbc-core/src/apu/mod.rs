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
pub(crate) use square::SWEEP_LAG;
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

/// `x mod m` for a channel advance over 2^32 clocks or more
/// (`Square`/`Wave::advance`). Normal spans are far shorter (DIV-APU syncs
/// every 8,192 clocks, and every run call ends with a sync); only a long
/// STOP or a caller's distant run target gets here. Cold and out of line,
/// so the u64 division (`__aeabi_uldivmod`, ~50-100 cycles on Cortex-M7)
/// stays off the advance paths, which divide in u32.
#[cold]
#[inline(never)]
pub(crate) fn rem_long(x: u64, m: u32) -> u32 {
    (x % u64::from(m)) as u32
}

/// Checks that a channel advanced over spans of 2^32 clocks or more at
/// once (the u64 paths, `rem_long`, `noise::long_steps`) ends in the same
/// state as advanced in steps under 2^32 (the u32 paths).
#[cfg(test)]
fn check_long_advance<T: Copy + PartialEq + core::fmt::Debug>(
    ch: T,
    advance: impl Fn(&mut T, u64),
) {
    for dt in [
        1 << 32,
        3 << 32 | 12_345,
        5 << 32 | 777_777,
        7 << 32 | 3_000_001,
    ] {
        let mut one = ch;
        advance(&mut one, dt);
        let mut split = ch;
        let mut left = dt;
        while left != 0 {
            let step = left.min(1 << 31);
            advance(&mut split, step);
            left -= step;
        }
        assert_eq!(one, split, "{ch:?} dt={dt}");
    }
}

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
    /// The power-on skip (power on with the APU bit high), in DIV-APU edges
    /// left: 2, the next edge does nothing; 1, the next edge clocks the
    /// units without stepping, so `step` stays 1 through both (SameBoy
    /// `GB_SKIP_DIV_EVENT_SKIP`, then `_SKIPPED`); 0, inactive.
    pub(crate) skip: u8,
    /// 2 MHz ticks (2 base clocks) of the clock at power on, mod 4: bit 0
    /// is SameBoy's 1 MHz phase (`lf_div`) for the square trigger delay
    /// (ADR 0021 section 5, task 1(b)); both bits give the noise
    /// alignment (`Noise::trigger_delay`).
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
                skip: 0,
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
    /// (NR10 = 80: sweep off; NR11 = BF: duty 2; NR12 = F3) and keeps
    /// running at the boot chime's last note, f = 0x7C1 (252 clocks per
    /// step; the boot ROM's second NR13 write), until a game retriggers it.
    /// Its duty phase at hand-over is fitted to gambatte's hardware-verified
    /// `ch1_init_pos_*` ROMs (cgb04c), which freeze the position by
    /// retriggering at a varied clock: the step into position 1 (duty 2
    /// low) must fall in clocks 144-147 + 504 here; 145 puts the steps on
    /// the APU's 1 MHz grid (1 mod 4), where the boot ROM's own
    /// single-speed trigger leaves them (`Apu::trigger_delay`).
    /// The length counter stays 0 (provisional).
    pub(crate) fn post_boot(&mut self) {
        *self = Self::new();
        self.fs.power = true;
        let ch1 = &mut self.sq[0];
        ch1.on = true;
        ch1.duty = 2;
        ch1.freq = 0x7C1;
        ch1.pos = 6;
        ch1.high = true;
        ch1.countdown = 145;
        ch1.reload = 252;
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
    /// edge comes too soon and is skipped, and the one
    /// after it clocks step 1 again without stepping: `step` is 1 (odd: a
    /// length-enabling NRx4 write clocks length once more) from power on
    /// to the third edge (SameBoy NR52 power on: `skip_div_event`,
    /// `div_divider = 1`; `GB_apu_div_event`).
    pub(crate) fn power_on(&mut self, now: u64, apu_bit_high: bool) {
        self.fs = Fs {
            power: true,
            step: u8::from(apu_bit_high),
            skip: 2 * u8::from(apu_bit_high),
            phase: ((now >> 1) & 3) as u8,
        };
    }

    /// A DIV-APU falling edge (after a sync to its clock): the frame
    /// sequencer steps and clocks the units, except during the power-on
    /// skip (`Fs::skip`). Pan Docs "Audio details", DIV-APU; SameBoy
    /// `GB_apu_div_event`. A sweep step that ends the sweep timer comes
    /// `SWEEP_LAG` clocks later (`Sweep::step`).
    #[inline(never)]
    pub(crate) fn step(&mut self) {
        let fs = &mut self.fs;
        if fs.skip != 0 {
            fs.skip -= 1;
            if fs.skip != 0 {
                return;
            }
        } else {
            fs.step = (fs.step + 1) & 7;
        }
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
            self.sweep.step();
        }
    }

    /// A trigger at `dist` clocks before the next `DivApu` event falls in
    /// the frame-sequencer step before the envelopes' step 0, which then
    /// doesn't count for the new envelope: from 4 clocks before the edge
    /// into step 7 to 4 clocks before the one into step 0. gambatte
    /// `EnvelopeUnit::nr4Init` (GPL, behaviour only): one more period when
    /// `(cc + 2) & 0x7000 == 0`, 2 MHz units; hardware captures cgb04c
    /// `ch2_init_reset_env_counter_timing_13`/`_14` (4 clocks before the
    /// window: counts) vs `_15`/`_16` (inside: one period later). SameBoy
    /// agrees on the step: its envelope countdowns tick at step 7
    /// (`GB_apu_div_event`). While a sweep step is pending (`Sweep::due`,
    /// just after the edge into step 3 or 7) the event is that step, not
    /// the next edge.
    #[inline]
    fn env_late(&self, dist: u32) -> bool {
        // Steps 6 within 4 clocks of the edge, or 7 unless within 4 of the
        // edge into 0: the trigger moved 4 clocks on is in step 7.
        self.fs.step + u8::from(dist <= 4 && !self.sweep.due) == 7
    }

    /// SameBoy's 1 MHz phase `lf_div` at clock `now`: 1 at power on, then
    /// toggling every 2 base clocks (SameBoy `GB_apu_init`, `GB_apu_run`).
    /// The exact fit is deferred (ADR 0021 section 12).
    #[inline]
    fn lf_div(&self, now: u64) -> u8 {
        ((now >> 1) as u8 ^ self.fs.phase ^ 1) & 1
    }

    /// Base clocks from a trigger at `now` to a square channel's first duty
    /// step, beyond one period, for a channel that is off (`Square::trigger`;
    /// 4 fewer for one that is on). `docs/reference/apu.md`, "Square
    /// channel trigger countdown":
    /// - single speed: the step lands on the APU's own 1 MHz grid, the
    ///   first clock 1 mod 4 at least 5 clocks on (gambatte cgb04c
    ///   `speedchange*_ch1_duty0_pos6_to_pos7_timing*`: a trigger before a
    ///   speed switch, read back after it). The grid runs on through
    ///   switches, so after one to single speed (odd CPU grid) the delay
    ///   still ends on it.
    /// - double speed: gambatte's CGB-C linear rule, `6 + 2 * lf_div`
    ///   (`DutyUnit::nr4Change`: `3 + lf_div` 2 MHz ticks; cgb04c
    ///   `ch1_duty0_pos6_to_pos7_timing_ds_6`,
    ///   `speedchange*_ch1_duty0_pos6_to_pos7_timing*ds_2`). SameBoy's
    ///   `6 - lf_div` is the CGB-E result SameSuite `channel_{1,2}_align*`
    ///   expect (signed off by the human, 2026-11-23; m9-triage group 7).
    ///   While the frame sequencer runs 2 clocks late after a switch to
    ///   double speed with the APU on (`ds` bit 0, `Timer::apu_ds_delay`),
    ///   the 1 MHz phase is one 2 MHz tick late too: `lf_div` flips
    ///   (cgb04c `speedchange_ch1_nr4init_duty0_pos6_to_pos7_timing_1`/`_2`:
    ///   power on, switch, trigger; 6 clocks, not 8).
    #[inline]
    fn trigger_delay(&self, now: u64, ds: u8) -> u8 {
        if ds & 2 != 0 {
            6 + (((self.lf_div(now) ^ ds) & 1) << 1)
        } else {
            5 + ((now as u8).wrapping_neg() & 3)
        }
    }

    /// A write to a channel register FF10-FF25 (by the low 7 address bits)
    /// at clock `now`, after a sync, with the APU on and the next `DivApu`
    /// event at clock `edge`. `ds`: the M-cycle length in clocks (bit 1
    /// set: double speed; 0 is taken as single speed), plus 1 while
    /// `Timer::apu_ds_delay` is set. The byte is already stored in
    /// `Memory::io`.
    #[inline(never)]
    pub(crate) fn write(&mut self, reg: usize, value: u8, now: u64, ds: u8, edge: u64) {
        // Under 2^14 while powered (`DivApu` is an edge or the sweep lag).
        let dist = edge.wrapping_sub(now) as u32;
        // FF10-FF14 are CH1's NR10-NR14 and FF15-FF19 CH2's (FF15 unused)
        // NR21-NR24: one call site for both squares keeps a single inlined
        // copy. NR10 and FF15 (offset 0) are no-ops in `Square::write`.
        let i = reg.wrapping_sub(0x10);
        if i < 10 {
            let ch = usize::from(i >= 5);
            let (fs, delay) = (self.fs.step, self.trigger_delay(now, ds));
            self.sq[ch].write(i - 5 * ch, value, fs, delay);
        }
        // NR10, CH1's trigger after the channel's own, or CH4. One
        // unconditional tail call instead of a test on every exit (code
        // size). The noise alignment: 2 MHz ticks since power on, mod 4,
        // and double speed in bit 2.
        let al = ((now >> 1) as u8).wrapping_sub(self.fs.phase) & 3 | (ds & 2) << 1;
        self.write_rest(reg, value, dist, al);
    }

    /// The rest of a write to FF10-FF25 (`reg`), after `Square::write`,
    /// `dist` clocks before the next `DivApu` event: NR10, or CH1's
    /// trigger (NR14 bit 7) after the channel's own, go to the sweep;
    /// FF1F-FF23 to CH4 (NR41-NR44 at offsets 1-4, FF1F unused); nothing
    /// for the others. Then a CH1, CH2 or CH4 trigger in the step before
    /// the envelopes' step 0 (`Apu::env_late`) delays its envelope (here
    /// rather than in the channels' trigger paths: code size). Out of
    /// line, off `Apu::write`'s common path.
    ///
    /// A CH1 trigger up to 8 clocks before the edge into step 3 or 7, or
    /// before the sweep step it causes (`Sweep::due`), resets the sweep
    /// timer after that step: it doesn't count. gambatte
    /// `SweepUnit::nr4Init` (GPL, behaviour only): the timer starts from
    /// `(cc + 2 + 2 * cgb) >> 14`, 2 MHz units; hardware captures cgb04c
    /// `ch1_init_reset_sweep_counter_timing_7` (12 clocks before: counts)
    /// vs `_10` (8 before: doesn't). SameBoy reloads its countdown at the
    /// trigger with no window.
    #[inline(never)]
    fn write_rest(&mut self, reg: usize, value: u8, dist: u32, al: u8) {
        let fs = self.fs.step;
        match reg {
            0x10 => self.sweep.write(value, &mut self.sq[0]),
            0x1F..=0x23 => self.noise.write(reg - 0x1F, value, fs, al),
            _ => {}
        }
        if value & 0x80 == 0 {
            return;
        }
        let late = self.env_late(dist);
        let env = match reg {
            0x14 => {
                let ch1 = &mut self.sq[0];
                let skip = self.sweep.due && self.sweep.check == 0 || fs & 3 == 2 && dist <= 8;
                self.sweep.trigger(ch1, skip);
                &mut ch1.env
            }
            0x19 => &mut self.sq[1].env,
            0x23 => &mut self.noise.env,
            _ => return,
        };
        if late {
            env.delay();
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

    /// Integrates the consecutive spans `segs` (each at most one sample
    /// window): adds to `lv[c][i]` channel `c`'s analog level (-15..15
    /// while its DAC is on, else 0) times clocks over span `i`, for
    /// CH1-CH4. `io` holds wave RAM.
    #[inline]
    fn integrate(&mut self, segs: &[u8], io: &[u8; 0x80], lv: &mut [[i16; mix::BATCH]; 4]) {
        let [l1, l2, l3, l4] = lv;
        let [s1, s2] = &mut self.sq;
        s1.integrate(segs, l1);
        s2.integrate(segs, l2);
        self.wave.integrate(segs, io, l3);
        self.noise.integrate(segs, l4);
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

    impl Apu {
        /// A DIV-APU edge with its sweep step and overflow check at once
        /// (no lag).
        pub(crate) fn edge(&mut self) {
            self.step();
            while self.sweep.due {
                self.sweep.clock(&mut self.sq[0]);
            }
        }
    }

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
        mem.apu.write(0x16, 0x40, 0, 0, u64::MAX);
        mem.apu.write(0x17, 0xF0, 0, 0, u64::MAX);
        mem.apu.write(0x19, 0x84, 0, 0, u64::MAX);
        let first = u64::from(mem.apu.sq[1].countdown);
        assert_eq!(read_apu(&mem, 0, 0x76), 0x00, "not started");
        // Position 7 after 7 steps, with the state still at clock 0.
        let t = first + 6 * 4096 + 1;
        assert_eq!(read_apu(&mem, t, 0x76), 0xF0);
        assert_eq!(read_apu(&mem, t + 2 * 4096, 0x76), 0x00, "position 1");
        assert_eq!(mem.apu.out.synced, 0, "pure");
    }

    #[test]
    fn post_boot_ch1_runs_on_at_0x7c1_in_the_fitted_phase() {
        // gambatte `ch1_init_pos_*` (cgb04c): high (duty 2, position 0)
        // through clock 648, low (position 1) from 649 for 4 steps (the
        // ROMs allow 648-651; 649 is 1 mod 4, the trigger grid).
        let mut apu = Apu::new();
        apu.post_boot();
        assert_eq!(apu.sq[0].reload, 252, "the running period is f 0x7C1's");
        let mut ch1 = apu.sq[0];
        ch1.advance(648);
        assert_eq!((ch1.pos, ch1.high), (0, true));
        ch1.advance(1);
        assert_eq!((ch1.pos, ch1.high), (1, false));
        ch1.advance(4 * 252 - 1);
        assert_eq!((ch1.pos, ch1.high), (4, false));
        ch1.advance(1);
        assert_eq!((ch1.pos, ch1.high), (5, true));
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
        assert_eq!((apu.fs.step, apu.fs.skip), (1, 2));
        apu.edge();
        assert_eq!((apu.fs.step, apu.fs.skip), (1, 1));
        // The second edge clocks step 1 without stepping (SameBoy
        // `GB_SKIP_DIV_EVENT_SKIPPED`).
        apu.edge();
        assert_eq!((apu.fs.step, apu.fs.skip), (1, 0));
        apu.edge();
        assert_eq!(apu.fs.step, 2);
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.edge();
        assert_eq!(apu.fs.step, 1);
        for _ in 0..7 {
            apu.edge();
        }
        assert_eq!(apu.fs.step, 0, "wraps mod 8");
    }

    /// Power on with the DIV-APU bit high, then CH2 triggered with length 2
    /// and length on: step 1 is odd, so the trigger clocks length once
    /// (2 -> 1). The first edge is skipped; the second clocks length at
    /// step 1 again (1 -> 0) rather than stepping to 2, which has no length
    /// clock. Gambatte `ch2_init_reset_length_counter_timing_nr52_*` and
    /// `ch2_late_reset_nr52_*` (CGB); SameBoy `GB_apu_div_event`.
    #[test]
    fn the_second_edge_after_a_high_power_on_clocks_length_at_step_1() {
        let mut apu = Apu::new();
        apu.power_on(0, true);
        apu.write(0x17, 0xF0, 0, 0, u64::MAX); // DAC on
        apu.write(0x16, 0x3E, 0, 0, u64::MAX); // length 2
        apu.write(0x19, 0xC0, 0, 0, u64::MAX); // trigger, length on
        assert_eq!(apu.nr52() & 0x02, 0x02, "on after the trigger");
        apu.edge();
        assert_eq!(apu.nr52() & 0x02, 0x02, "first edge skipped");
        apu.edge();
        assert_eq!(apu.nr52() & 0x02, 0x00, "length 1 -> 0 at step 1");
    }

    #[test]
    fn single_speed_triggers_step_on_the_1mhz_grid() {
        // gambatte cgb04c `speedchange*_ch1_duty0_pos6_to_pos7_timing*`:
        // in single speed the first step after a trigger is the first clock
        // 1 mod 4 at least 5 clocks after one period (an off channel), 1
        // for an on channel; the CPU grid can be odd after a switch to
        // single speed. f = 0x7E0: 128 clocks per step.
        for now in 100..108u64 {
            let mut apu = Apu::new();
            apu.power_on(0, false);
            apu.write(0x12, 0xF0, now, 0, u64::MAX);
            apu.write(0x13, 0xE0, now, 0, u64::MAX);
            apu.write(0x14, 0x87, now, 0, u64::MAX);
            let first = now + u64::from(apu.sq[0].countdown);
            assert_eq!(first % 4, 1, "off, trigger at {now}");
            assert!((now + 128 + 5..now + 128 + 9).contains(&first), "{now}");
            apu.write(0x14, 0x87, now, 0, u64::MAX);
            let first = now + u64::from(apu.sq[0].countdown);
            assert_eq!(first % 4, 1, "on, trigger at {now}");
            assert!((now + 128 + 1..now + 128 + 5).contains(&first), "{now}");
        }
    }

    #[test]
    fn double_speed_triggers_follow_gambattes_linear_rule() {
        // gambatte `DutyUnit::nr4Change` (CGB C, cgb04c
        // `ch1_duty0_pos6_to_pos7_timing_ds_6`, `speedchange*_..._ds_2`):
        // the first step comes one period plus `6 + 2 * lf_div` clocks
        // after an off-channel trigger, 4 fewer for an on channel.
        // SameBoy's CGB-E `10 - 2 * lf_div` has the opposite sign. lf_div
        // is 1 at power on (clock 0) and toggles every 2 clocks. With the
        // delay after a switch to double speed (`ds` = 3), one tick later.
        for (now, extra) in [(100, 8), (101, 8), (102, 6), (103, 6), (104, 8)] {
            for (ds, extra) in [(2, extra), (3, 14 - extra)] {
                let mut apu = Apu::new();
                apu.power_on(0, false);
                apu.write(0x12, 0xF0, now, ds, u64::MAX);
                apu.write(0x14, 0x87, now, ds, u64::MAX);
                assert_eq!(apu.sq[0].countdown, 1024 + extra, "off, {now}, {ds}");
                apu.write(0x14, 0x87, now, ds, u64::MAX);
                assert_eq!(apu.sq[0].countdown, 1024 + extra - 4, "on, {now}, {ds}");
            }
        }
    }

    /// A powered APU at step 0 with CH1 (DAC on, volume 15, duty 50%)
    /// triggered at frequency `f` with NR10 = `nr10`, through `Apu::write`.
    fn ch1_playing(nr10: u8, f: u16) -> Apu {
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.write(0x10, nr10, 0, 0, u64::MAX);
        apu.write(0x11, 0x80, 0, 0, u64::MAX);
        apu.write(0x12, 0xF0, 0, 0, u64::MAX);
        apu.write(0x13, f as u8, 0, 0, u64::MAX);
        apu.write(0x14, 0x80 | (f >> 8) as u8, 0, 0, u64::MAX);
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
            apu.edge();
            seen[usize::from(apu.fs.step)] = apu.sq[0].freq;
        }
        assert_eq!(
            seen,
            [0x240, 0x100, 0x100, 0x180, 0x180, 0x180, 0x180, 0x240]
        );
        // A sweep step that overflows turns CH1 off: NR52 bit 0.
        let mut apu = ch1_playing(0x11, 1280);
        for _ in 0..3 {
            apu.edge();
        }
        assert_eq!(apu.nr52() & 1, 0);
    }

    /// The edge into step 3 or 7 leaves the sweep step pending
    /// (`Sweep::due`) for the bus to run `SWEEP_LAG` clocks later.
    #[test]
    fn the_edges_into_steps_3_and_7_leave_the_sweep_step_pending() {
        let mut apu = ch1_playing(0x11, 0x100);
        for want in [false, false, true] {
            apu.step();
            assert_eq!(apu.sweep.due, want, "step {}", apu.fs.step);
        }
        assert_eq!(apu.sq[0].freq, 0x100, "not yet");
        apu.sweep.clock(&mut apu.sq[0]);
        assert_eq!((apu.sweep.due, apu.sweep.check), (true, 1), "the check");
        assert_eq!(apu.sq[0].freq, 0x180);
        // Period 2: the first sweep step only counts down, at once.
        let mut apu = ch1_playing(0x21, 0x100);
        for _ in 0..3 {
            apu.step();
        }
        assert_eq!((apu.sweep.due, apu.sweep.timer), (false, 1));
        for _ in 0..4 {
            apu.step();
        }
        assert_eq!((apu.sweep.due, apu.sweep.timer), (true, 1));
    }

    /// The sweep timer after a CH1 trigger (NR10 = 20: period 2) `dist`
    /// clocks before the next `DivApu` event, at frame-sequencer `step`,
    /// with a sweep step pending (`due`) or not.
    fn sweep_timer_after_trigger(step: u8, dist: u64, due: bool) -> u8 {
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.fs.step = step;
        apu.sweep.due = due;
        for (reg, v) in [(0x10, 0x20), (0x12, 0xF0), (0x14, 0x87)] {
            apu.write(reg, v, 100, 0, 100 + dist);
        }
        apu.sweep.timer
    }

    #[test]
    fn a_ch1_trigger_up_to_8_clocks_before_a_sweep_step_skips_it() {
        // gambatte cgb04c `ch1_init_reset_sweep_counter_timing_7` (12
        // clocks before the edge into step 3: counts) and `_10` (8: not).
        assert_eq!(sweep_timer_after_trigger(2, 12, false), 2);
        assert_eq!(sweep_timer_after_trigger(2, 8, false), 3);
        assert_eq!(sweep_timer_after_trigger(6, 1, false), 3);
        assert_eq!(
            sweep_timer_after_trigger(1, 8, false),
            2,
            "no sweep step next"
        );
        assert_eq!(sweep_timer_after_trigger(3, 4, true), 3, "inside the lag");
        // Inside the overflow check after a step: the step is done.
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.fs.step = 3;
        (apu.sweep.due, apu.sweep.check) = (true, 3);
        for (reg, v) in [(0x10, 0x20), (0x12, 0xF0), (0x14, 0x87)] {
            apu.write(reg, v, 100, 0, 104);
        }
        assert_eq!(apu.sweep.timer, 2, "inside the check");
        assert_eq!(sweep_timer_after_trigger(3, 8000, false), 2);
    }

    /// The CH2 and CH4 envelope timers after a trigger (NRx2 = 09: volume
    /// 0, up, period 1) `dist` clocks before the next `DivApu` event, at
    /// frame-sequencer `step`, with a sweep step pending (`due`) or not.
    fn env_timers_after_trigger(step: u8, dist: u64, due: bool) -> (u8, u8) {
        let mut apu = Apu::new();
        apu.power_on(0, false);
        apu.fs.step = step;
        apu.sweep.due = due;
        for (reg, v) in [(0x17, 0x09), (0x19, 0x80), (0x21, 0x09), (0x23, 0x80)] {
            apu.write(reg, v, 100, 0, 100 + dist);
        }
        (apu.sq[1].env.timer, apu.noise.env.timer)
    }

    #[test]
    fn a_trigger_in_the_step_before_the_envelope_step_adds_a_period() {
        // gambatte cgb04c `ch2_init_reset_env_counter_timing_13`/`_14` (8
        // clocks before the edge into step 7: counts) and `_15`/`_16` (4:
        // one period later).
        assert_eq!(env_timers_after_trigger(6, 8, false), (1, 1));
        assert_eq!(env_timers_after_trigger(6, 4, false), (2, 2));
        assert_eq!(env_timers_after_trigger(7, 8000, false), (2, 2));
        assert_eq!(env_timers_after_trigger(7, 5, false), (2, 2));
        assert_eq!(
            env_timers_after_trigger(7, 4, false),
            (1, 1),
            "4 before step 0"
        );
        assert_eq!(
            env_timers_after_trigger(7, 2, true),
            (2, 2),
            "inside the sweep lag"
        );
        assert_eq!(env_timers_after_trigger(0, 4, false), (1, 1));
        assert_eq!(env_timers_after_trigger(5, 4, false), (1, 1));
    }

    #[test]
    fn nr10_writes_go_to_the_sweep_and_ff15_is_ignored() {
        let mut apu = ch1_playing(0x19, 0x400);
        let before = apu;
        apu.write(0x15, 0xFF, 0, 0, u64::MAX);
        assert_eq!(apu, before, "FF15 unused");
        apu.write(0x10, 0x11, 0, 0, u64::MAX); // clears negate after a negate calculation
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
        mem.apu.write(0x17, 0xA0, 0, 0, u64::MAX);
        mem.apu.write(0x19, 0x87, 0, 0, u64::MAX);
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

    /// A powered APU with CH4 (DAC on, volume 15, NR43 = 0: the first step
    /// 12 clocks on, then one every 8, bit 0 first 1 after 15 steps)
    /// triggered at clock 0.
    fn ch4_playing() -> Box<Memory> {
        let mut mem = Box::new(Memory::new());
        mem.apu.power_on(0, false);
        for (reg, v) in [(0x21, 0xF0), (0x22, 0x00), (0x23, 0x80)] {
            mem.io[reg] = v;
            mem.apu.write(reg, v, 0, 0, u64::MAX);
        }
        mem
    }

    #[test]
    fn nr52_bit_3_and_pcm34_high_nibble_follow_ch4() {
        let mut mem = ch4_playing();
        assert_eq!(mem.apu.nr52(), 0xF8);
        assert_eq!(read_apu(&mem, 12 + 14 * 8 - 1, 0x77), 0x00);
        assert_eq!(read_apu(&mem, 12 + 14 * 8, 0x77), 0xF0);
        assert_eq!(mem.apu.out.synced, 0, "pure");
        mem.apu.write(0x21, 0x00, 0, 0, u64::MAX);
        assert_eq!(mem.apu.nr52(), 0xF0, "DAC off");
        assert_eq!(read_apu(&mem, 15 * 8, 0x77), 0x00);
    }

    #[test]
    fn frame_sequencer_clocks_the_ch4_length() {
        let mut mem = ch4_playing();
        mem.apu.write(0x20, 0x3F, 0, 0, u64::MAX); // length 1
        mem.apu.write(0x23, 0x40, 0, 0, u64::MAX); // enable at step 0: no extra clock
        assert_eq!(mem.apu.nr52() & 8, 8);
        mem.apu.edge(); // step 1: length
        assert_eq!(mem.apu.nr52() & 8, 0);
    }

    #[test]
    fn power_off_clears_the_ch4_length() {
        let mut mem = ch4_playing();
        mem.apu.write(0x20, 0x3F, 0, 0, u64::MAX); // length 1
        mem.apu.power_off();
        mem.apu.power_on(0, false);
        mem.apu.write(0x21, 0xF0, 0, 0, u64::MAX);
        mem.apu.write(0x23, 0xC0, 0, 0, u64::MAX); // trigger, length on: 64 if cleared
        mem.apu.edge(); // step 1: length
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
        // Bit 0 is 0 for the first 124 clocks (+15), then 1 (-15) for 4.
        assert_eq!(buf[0].left, (124 - 4) * 15 * 8 / 2, "x 64 / 128");
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
        mem.apu.edge(); // step 1: length
        assert!(!mem.apu.wave.on);
    }
}

#[cfg(test)]
mod sync_tests {
    //! The sync invariant (ADR 0021 section 3; plan M6 task 7): the APU state
    //! after a span does not depend on how the span is split into syncs, nor
    //! on whether the syncs integrated (a buffer) or only advanced (an empty
    //! buffer, section 9); with buffers, the samples do not depend on the split
    //! either. A seeded script of register writes and frame-sequencer steps
    //! drives all four channels: CH1's sweep, the envelopes up and down, wave
    //! RAM and NR32 shifts, the LFSR in 7- and 15-bit modes, lengths expiring,
    //! retriggers and frequency changes at random clocks.

    extern crate std;
    use super::*;
    use std::{boxed::Box, vec, vec::Vec};

    /// xorshift64* (Vigna): a deterministic test PRNG (gbc-core takes no
    /// dependencies).
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
        }
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn byte(&mut self) -> u8 {
            (self.next() >> 40) as u8
        }
    }

    #[derive(Clone, Copy)]
    enum Ev {
        /// A write to FF10-FF3F by the low 7 address bits.
        Write(usize, u8),
        /// A DIV-APU falling edge.
        Step,
    }

    /// Base clocks between DIV-APU edges (512 Hz, single speed).
    const DIV_APU: u64 = 8192;
    const STEPS: u64 = 64;
    const END: u64 = STEPS * DIV_APU + 1000;

    /// The bus's write path with the APU on (bus.rs `write_apu`), after a sync.
    fn write(mem: &mut Memory, reg: usize, value: u8, now: u64) {
        match reg {
            0x1A..=0x1E | 0x30..=0x3F => write_wave(mem, reg, value),
            _ => {
                mem.io[reg] = value;
                mem.apu.write(reg, value, now, 0, u64::MAX);
            }
        }
    }

    /// The events of one seed, sorted by clock: all four channels set up and
    /// triggered at clock 0, a DIV-APU edge every 8,192 clocks, and random
    /// writes in between.
    fn script(seed: u64) -> Vec<(u64, Ev)> {
        let mut rng = Rng::new(seed);
        let mut evs = Vec::new();
        let mut at0 = |reg: usize, v: u8| evs.push((0, Ev::Write(reg, v)));
        at0(0x24, 0x77);
        at0(0x25, 0xFF);
        for i in 0..16 {
            at0(0x30 + i, rng.byte());
        }
        // CH1: sweep period 1, add, shift 6; duty 1, length 16; volume 15
        // down every 3; f = 0x500, length on.
        for (reg, v) in [(0x10, 0x16), (0x11, 0x70), (0x12, 0xF3), (0x13, 0x00)] {
            at0(reg, v);
        }
        at0(0x14, 0xC5);
        // CH2: duty 3; volume 2 up every 5; f = 0x7FD (12 clocks per step:
        // whole 96-clock waveforms inside one window).
        for (reg, v) in [(0x16, 0xC0), (0x17, 0x2D), (0x18, 0xFD), (0x19, 0x87)] {
            at0(reg, v);
        }
        // CH3: length 16, volume 100%, f = 0x610, length on.
        for (reg, v) in [(0x1A, 0x80), (0x1B, 0xF0), (0x1C, 0x20), (0x1D, 0x10)] {
            at0(reg, v);
        }
        at0(0x1E, 0xC6);
        // CH4: length 32; volume 10 down every 1; 7-bit, r = 0, s = 0.
        for (reg, v) in [(0x20, 0x20), (0x21, 0xA1), (0x22, 0x08), (0x23, 0xC0)] {
            at0(reg, v);
        }
        for k in 1..=STEPS {
            evs.push((k * DIV_APU, Ev::Step));
        }
        // Midway, CH4 switches to the 15-bit LFSR (r = 1, s = 3).
        let mid = STEPS / 2 * DIV_APU + 300;
        evs.push((mid, Ev::Write(0x22, 0x31)));
        evs.push((mid, Ev::Write(0x23, 0x80)));
        // And CH3 goes to f = 0x7FF (2 clocks per step, 64 per waveform).
        evs.push((mid + 7, Ev::Write(0x1D, 0xFF)));
        evs.push((mid + 7, Ev::Write(0x1E, 0x87)));
        for _ in 0..300 {
            let t = 1 + rng.below(END - 1);
            let mut v = rng.byte();
            let reg = match rng.below(6) {
                0 => 0x10 + rng.below(5) as usize,
                1 => 0x16 + rng.below(4) as usize,
                2 => {
                    let reg = 0x1A + rng.below(5) as usize;
                    if reg == 0x1A && rng.below(8) != 0 {
                        v |= 0x80; // CH3's DAC mostly on
                    }
                    reg
                }
                3 => {
                    let reg = 0x20 + rng.below(4) as usize;
                    if reg == 0x22 && rng.below(4) != 0 {
                        v &= 0x3F; // mostly short periods (s <= 3)
                    }
                    reg
                }
                4 => 0x24 + rng.below(2) as usize,
                _ => 0x30 + rng.below(16) as usize,
            };
            // Half the frequency writes go near f = 2047, where whole
            // waveform periods fit in a window (the closed-form sums).
            if rng.below(2) == 0 {
                match reg {
                    0x13 | 0x18 | 0x1D => v |= 0xF8,
                    0x14 | 0x19 | 0x1E => v |= 0x07,
                    _ => {}
                }
            }
            evs.push((t, Ev::Write(reg, v)));
        }
        evs.sort_by_key(|&(t, _)| t);
        evs
    }

    /// The register reads that depend on channel state, at `now` (pure).
    fn reads(mem: &Memory, now: u64) -> [u8; 19] {
        let mut r = [0; 19];
        r[0] = read_apu(mem, now, 0x26);
        r[1] = read_apu(mem, now, 0x76);
        r[2] = read_apu(mem, now, 0x77);
        for i in 0..16 {
            r[3 + i] = read_apu(mem, now, 0x30 + i);
        }
        r
    }

    #[derive(Clone, Copy, PartialEq)]
    enum Room {
        Always,
        Never,
        Random,
    }

    /// One run of `seed`'s script: the APU state at each event (before it
    /// applies) and at the end, and the samples. With `split`, each gap
    /// between events is cut at random extra sync points; at each, the reads
    /// from the stale state (advanced copies) must equal those after the sync.
    fn drive(seed: u64, split: bool, room: Room) -> (Vec<Apu>, Vec<StereoSample>) {
        let mut rng = Rng::new(seed ^ 0xA5A5);
        let mut mem = Box::new(Memory::new());
        mem.apu.power_on(0, false);
        let mut buf = vec![StereoSample::default(); (END / SAMPLE_PERIOD + 4) as usize];
        let mut len = 0;
        let mut states = Vec::new();
        let mut prev = 0;
        let mut sync = |mem: &mut Memory, now: u64, rng: &mut Rng| {
            let with = match room {
                Room::Always => true,
                Room::Never => false,
                Room::Random => rng.below(2) == 0,
            };
            let Memory { apu, io, .. } = mem;
            let audio: &mut [StereoSample] = if with { &mut buf } else { &mut [] };
            apu.sync(now, io, audio, &mut len);
        };
        let evs = script(seed);
        let ends = evs.iter().map(|&(t, ev)| (t, Some(ev)));
        for (t, ev) in ends.chain([(END, None)]) {
            if split && t > prev {
                let n = match rng.below(4) {
                    0 => 0,
                    1 => 1,
                    _ => rng.below(24),
                };
                let mut cuts: Vec<u64> = (0..n).map(|_| prev + rng.below(t - prev + 1)).collect();
                cuts.sort_unstable();
                for c in cuts {
                    let before = reads(&mem, c);
                    sync(&mut mem, c, &mut rng);
                    assert_eq!(before, reads(&mem, c), "seed {seed}: reads at {c}");
                }
            }
            sync(&mut mem, t, &mut rng);
            prev = t;
            states.push(mem.apu);
            match ev {
                Some(Ev::Write(reg, v)) => write(&mut mem, reg, v, t),
                Some(Ev::Step) => mem.apu.edge(),
                None => {}
            }
        }
        buf.truncate(len);
        (states, buf)
    }

    /// `a` and `b` agree at every event and at the end; with `whole`, in the
    /// output stream too.
    fn assert_same(seed: u64, a: &[Apu], b: &[Apu], whole: bool) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            let mut y = *y;
            if !whole {
                assert_eq!(x.out.synced, y.out.synced);
                y.out = x.out;
            }
            assert_eq!(*x, y, "seed {seed}, event {i}");
        }
    }

    #[test]
    fn random_split_points_change_neither_state_nor_samples() {
        for seed in 0..12 {
            let (one, s1) = drive(seed, false, Room::Always);
            let (split, s2) = drive(seed, true, Room::Always);
            assert_same(seed, &one, &split, true);
            assert_eq!(s1.len() as u64, END / SAMPLE_PERIOD);
            assert!(s1 == s2, "seed {seed}: samples differ");
            // The script keeps the mix audible and varied.
            assert!(s1.iter().any(|s| s.left != s1[0].left));
        }
    }

    #[test]
    fn an_empty_buffer_leaves_the_same_channel_state() {
        for seed in 0..12 {
            let (with, _) = drive(seed, false, Room::Always);
            let (without, s) = drive(seed, false, Room::Never);
            assert!(s.is_empty());
            assert_same(seed, &with, &without, false);
            let (mixed, _) = drive(seed, true, Room::Random);
            assert_same(seed, &with, &mixed, false);
        }
    }

    #[test]
    fn the_script_exercises_every_channel() {
        // Guards the tests above against a script that leaves channels idle.
        let (states, _) = drive(1, false, Room::Always);
        let ever = |f: &dyn Fn(&Apu) -> bool| states.iter().any(f);
        assert!(ever(&|a| a.sq[0].on && a.sweep.enabled));
        assert!(ever(&|a| a.sq[1].on && a.sq[1].env.volume > 2));
        assert!(ever(&|a| a.wave.on && a.wave.index > 0));
        assert!(ever(&|a| a.noise.on && a.noise.nr43 & 8 != 0));
        assert!(ever(&|a| a.noise.on && a.noise.nr43 & 8 == 0));
        // A length expiry (or DAC off) turned a playing channel off.
        let offs = states
            .windows(2)
            .any(|w| w[0].nr52() & !w[1].nr52() & 0xF != 0);
        assert!(offs);
    }
}
