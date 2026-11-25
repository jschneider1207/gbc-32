//! The APU's side of the bus: its register writes, the DIV-APU and
//! audio-buffer events and the lazy sync (`apu` module; ADR 0021).

use super::Ctx;
use crate::Events;
use crate::apu;
use crate::memory::Memory;
use crate::schedule::{EventKind, NEVER};

impl Ctx<'_, '_> {
    /// The APU part of a DIV write or STOP (`old`: the counter before it):
    /// if the DIV-APU bit was 1, the reset is its falling edge and the
    /// frame sequencer steps now (SameBoy `GB_set_internal_div_counter`,
    /// `triggers & apu_bit`); then the deadline moves with the rebased
    /// counter. The caller reschedules.
    // Out of line: measured with the other bus helpers as one batch, not
    // alone (docs/perf.md, "Small-state and attribute pass", A14).
    #[inline(never)]
    pub(super) fn apu_div_reset(&mut self, old: u64) {
        if !self.mem.apu.powered() {
            return;
        }
        let now = self.clock();
        if old >> self.sys.timer.div_apu_bit() & 1 != 0 {
            self.apu_sync(now);
            self.mem.apu.step();
        }
        self.set_apu_deadline(now);
    }

    /// Sets the `DivApu` deadline at clock `now`, after a frame-sequencer
    /// step, a sweep event or a counter rebase: `SWEEP_LAG` clocks on while
    /// a sweep step or overflow-check tick is pending (`Sweep::due`; a
    /// rebase inside the lag, at most 2 clocks
    /// into it in double speed, moves it by that much), else the next
    /// DIV-APU edge.
    // Out of line: measured with the other bus helpers as one batch, not
    // alone (docs/perf.md, "Small-state and attribute pass", A14).
    #[inline(never)]
    pub(super) fn set_apu_deadline(&mut self, now: u64) {
        let apu = &self.mem.apu;
        if apu.sweep.due {
            self.sys.sched.set(EventKind::DivApu, now + apu::SWEEP_LAG);
        } else {
            self.sys.set_div_apu_deadline(apu.fs, now);
        }
    }

    /// Brings the APU up to `now`, writing completed samples into the
    /// caller's buffer (ADR 0021 section 3).
    #[inline(never)]
    pub(crate) fn apu_sync(&mut self, now: u64) {
        let Memory { apu, io, .. } = &mut *self.mem;
        apu.sync(now, io, self.audio, &mut self.audio_len);
    }

    /// The start of a run call: the APU is brought to the clock (advance
    /// only, if a caller left it behind), and with buffer room the
    /// `AudioBuffer` deadline is where the last slot's window ends. The
    /// caller reschedules. Once per call.
    pub(super) fn start_audio(&mut self) {
        let now = self.clock();
        let apu = &mut self.mem.apu;
        if apu.out.synced < now {
            apu.sync(now, &self.mem.io, &mut [], &mut 0);
        }
        let at = match self.audio.len() {
            0 => NEVER,
            room => self.mem.apu.buffer_deadline(room),
        };
        self.sys.sched.set(EventKind::AudioBuffer, at);
    }

    /// A DIV-APU falling edge (`EventKind::DivApu`, due at `now`), or the
    /// pending sweep event `SWEEP_LAG` clocks after the last one
    /// (`Sweep::due`: the step, or a tick of its overflow check).
    // Out of line: measured with the other bus helpers as one batch, not
    // alone (docs/perf.md, "Small-state and attribute pass", A14).
    #[inline(never)]
    pub(super) fn div_apu_event(&mut self, now: u64) {
        self.apu_sync(now);
        let apu = &mut self.mem.apu;
        if apu.sweep.due {
            apu.sweep.clock(&mut apu.sq[0]);
        } else {
            apu.step();
        }
        self.set_apu_deadline(now);
    }

    /// The last free audio slot's window ended (`EventKind::AudioBuffer`):
    /// write it and end the call. The deadline stays `NEVER`.
    // Out of line: measured with the other bus helpers as one batch, not
    // alone (docs/perf.md, "Small-state and attribute pass", A14).
    #[inline(never)]
    pub(super) fn audio_buffer_event(&mut self, now: u64) {
        self.apu_sync(now);
        self.raise(Events::AUDIO_BUFFER_FULL);
    }

    /// A write to FF10-FF3F (by the low 7 address bits): the APU is synced
    /// to the access first, then the write applies (ADR 0021 section 3).
    /// While the APU is off, FF10-FF25 ignore writes on CGB, the NRx1
    /// lengths included (SameBoy `GB_apu_write`: only DMG accepts them);
    /// NR52 and wave RAM stay writable. Out of line, off `write_mapped`.
    #[inline(never)]
    pub(super) fn write_apu(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        let Memory { apu, io, .. } = &mut *self.mem;
        apu.sync_write(reg, value, now, io, self.audio, &mut self.audio_len);
        match reg {
            0x26 => self.write_nr52(value, now),
            0x10..=0x25 if !self.mem.apu.powered() => {}
            // CH3 and wave RAM (also while off).
            0x1A..=0x1E | 0x30..=0x3F => apu::write_wave(self.mem, reg, value),
            // Channel registers: stored, then decoded by the channels.
            0x10..=0x25 => {
                self.mem.io[reg] = value;
                let edge = self.sys.sched.deadline(EventKind::DivApu);
                let speed = self.hot.speed();
                self.mem.apu.write(reg, value, now, speed, edge);
            }
            // FF27-FF2F: unused, stored.
            _ => self.mem.io[reg] = value,
        }
    }

    /// NR52: only bit 7 (power) is writable (Pan Docs: FF26 - NR52; ADR
    /// 0021 section 7). Off zeroes FF10-FF25 and the APU state but the
    /// output stream; on starts the frame sequencer. Synced to `now`.
    fn write_nr52(&mut self, value: u8, now: u64) {
        let on = value & 0x80 != 0;
        if on == self.mem.apu.powered() {
            return;
        }
        if on {
            // The power-on skip tests the counter as of the end of the
            // write's M-cycle, 4 counter ticks on in either speed: a write
            // in the M-cycle at whose end bit 12 (13 in double speed) rises
            // counts as high (gambatte `ch2_late_reset_nr52_2*` and
            // `ch2_init_reset_length_counter_timing_nr52_*`, CGB; SameBoy
            // `cycle_write` advances the pending cycles, then writes).
            let sc = self.sys.timer.sc(now) + 4;
            let high = sc >> self.sys.timer.div_apu_bit() & 1 != 0;
            self.mem.apu.power_on(now, high);
        } else {
            self.mem.apu.power_off();
            self.mem.io[0x10..=0x25].fill(0);
        }
        // The first edge is the next one after the end of the write's
        // M-cycle, consistent with the skip test above: an edge at that
        // end falls while the APU is still off (SameBoy `cycle_write` runs
        // the pending cycles, then `GB_apu_init`). One M-cycle is
        // 4 counter ticks in either speed. Power-off ignores it.
        self.sys
            .set_div_apu_deadline(self.mem.apu.fs, now + self.hot.mcycle());
        self.reschedule();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::apu::StereoSample;
    use crate::bus::SOUND_READ_MASK;
    use crate::ppu::Frame;
    use crate::testkit::{load, rom, write};
    use crate::timer::Timer;
    use crate::{Emulator, FRAME_CLOCKS};
    use std::boxed::Box;
    use std::vec;

    // --- APU framework (ADR 0021; M6 task 1(a)) ---

    fn run_audio(emu: &mut Emulator, clocks: u64, audio: &mut [StereoSample]) -> crate::RunResult {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.run_cycles(clocks, &mut fb, audio)
    }

    /// The clock of the first DIV-APU edge after reset: counter bit 12
    /// falls when the counter reaches a multiple of 0x2000.
    fn first_div_apu_edge() -> u64 {
        let c0 = Timer::post_boot(true).sc(0);
        (((c0 >> 13) + 1) << 13) - c0
    }

    #[test]
    fn nr52_power_off_clears_the_registers_and_ignores_writes() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        assert_eq!(emu.peek(0xFF26), 0xF1, "post-boot");
        write(emu, 0xFF26, 0x8E); // only bit 7 is writable
        assert_eq!(emu.peek(0xFF26), 0xF1);
        write(emu, 0xFF30, 0x5A);
        write(emu, 0xFF26, 0x00);
        assert_eq!(emu.peek(0xFF26), 0x70);
        assert_eq!(
            emu.hw.sys.sched.deadline(EventKind::DivApu),
            NEVER,
            "no frame sequencer while off"
        );
        for reg in 0xFF10..=0xFF25u16 {
            let mask = SOUND_READ_MASK[usize::from(reg - 0xFF10)];
            assert_eq!(emu.peek(reg), mask, "{reg:#06x} cleared");
            write(emu, reg, 0xFF);
            assert_eq!(emu.peek(reg), mask, "{reg:#06x} ignores writes while off");
        }
        // Wave RAM is kept and stays writable.
        assert_eq!(emu.peek(0xFF30), 0x5A);
        write(emu, 0xFF31, 0xA5);
        assert_eq!(emu.peek(0xFF31), 0xA5);
        write(emu, 0xFF26, 0x80);
        assert_eq!(emu.peek(0xFF26), 0xF0);
        assert_eq!(emu.peek(0xFF12), 0x00, "still cleared after power on");
        write(emu, 0xFF12, 0xF3);
        assert_eq!(emu.peek(0xFF12), 0xF3);
    }

    #[test]
    fn div_apu_steps_on_each_falling_edge_of_counter_bit_12() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), first);
        let step_at = |emu: &mut Emulator, t: u64| {
            let now = emu.clock();
            let _ = run_audio(emu, t - now, &mut []);
            emu.mem.apu.fs.step
        };
        assert_eq!(step_at(emu, first - 8), 0);
        assert_eq!(step_at(emu, first + 4), 1);
        assert_eq!(step_at(emu, first + 8192 - 8), 1);
        assert_eq!(step_at(emu, first + 8192 + 4), 2);
        assert_eq!(
            emu.hw.sys.sched.deadline(EventKind::DivApu),
            first + 2 * 8192
        );
    }

    /// The sweep step comes `SWEEP_LAG` (4) clocks after the edge into
    /// step 3: an access at the edge's clock still sees CH1 on (gambatte
    /// cgb04c `ch1_init_reset_sweep_counter_timing_nr52_3`), the `DivApu`
    /// event runs the step 4 clocks on and its overflow check 4 more on
    /// (shift 1), then waits for the next edge.
    #[test]
    fn the_sweep_steps_4_clocks_after_the_edge_into_step_3() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        let e3 = first + 2 * 8192;
        let _ = run_audio(emu, first + 8192 + 0x100, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 2);
        // Period 1, shift 1, f = 1280: the first step stores 1920 and its
        // overflow check (2880) turns CH1 off.
        for (reg, v) in [
            (0xFF12, 0xF0),
            (0xFF10, 0x11),
            (0xFF13, 0x00),
            (0xFF14, 0x85),
        ] {
            write(emu, reg, v);
        }
        assert_eq!(emu.peek(0xFF26) & 1, 1);
        let now = emu.clock();
        let _ = run_audio(emu, e3 - now, &mut []);
        assert_eq!(emu.clock(), e3);
        assert_eq!(emu.mem.apu.fs.step, 3);
        assert_eq!(emu.peek(0xFF26) & 1, 1, "on at the edge");
        assert_eq!(emu.mem.apu.sq[0].freq, 1280);
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 4);
        let _ = run_audio(emu, 4, &mut []);
        assert_eq!(emu.mem.apu.sq[0].freq, 1920);
        // The overflow check is 1 MHz tick (shift 1) after the step.
        assert_eq!(emu.peek(0xFF26) & 1, 1, "on until the check");
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 8);
        let _ = run_audio(emu, 4, &mut []);
        assert_eq!(emu.peek(0xFF26) & 1, 0, "off 8 clocks on");
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), e3 + 8192);
    }

    #[test]
    fn a_div_write_with_bit_12_set_steps_the_frame_sequencer_early() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        let _ = run_audio(emu, first + 0x800, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 1);
        // Bit 12 is 0: no edge; the next one is 0x2000 after the reset.
        write(emu, 0xFF04, 0);
        assert_eq!(emu.mem.apu.fs.step, 1);
        let reset = emu.clock();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), reset + 0x2000);
        // 0x1100 later bit 12 is 1: the reset is its falling edge.
        let _ = run_audio(emu, 0x1100, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 1);
        write(emu, 0xFF04, 0);
        assert_eq!(emu.mem.apu.fs.step, 2);
        let reset = emu.clock();
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), reset + 0x2000);
        let _ = run_audio(emu, 0x2000 + 4, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 3);
    }

    /// In double speed the edge is counter bit 13's fall, and each switch
    /// to double speed with the APU on toggles a 1 M-cycle (2 clock) delay:
    /// the 1st and 3rd switches delay the frame sequencer, the 2nd doesn't,
    /// single speed never does, and NR52 power on/off clears it (AGE
    /// `spsw-ch2-lc-delay`: `TEST_DS`, `TEST_DS_NS_DS`, `TEST_DS_OFF_ON`).
    #[test]
    fn div_apu_follows_counter_bit_13_after_a_switch_to_double_speed() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // The deadline less the bare edge of the current DIV-APU bit.
        let late = |ctx: &Ctx| {
            let t = &ctx.sys.timer;
            let k = t.div_apu_bit() + 1;
            let edge = t.clock_at(((t.sc(ctx.clock()) >> k) + 1) << k);
            ctx.sys.sched.deadline(EventKind::DivApu) - edge
        };
        let mut lates = [0; 6];
        for l in &mut lates {
            assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
            *l = late(&ctx);
        }
        assert_eq!(ctx.sys.timer.div_apu_bit(), 12);
        assert_eq!(lates, [2, 0, 0, 0, 2, 0], "DS, NS, DS, NS, DS, NS");
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert_eq!(late(&ctx), 0, "4th switch to double speed");
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
        assert_eq!(late(&ctx), 2, "5th switch to double speed");
        let now = ctx.clock();
        ctx.write_nr52(0x00, now);
        ctx.write_nr52(0x80, now);
        assert_eq!(late(&ctx), 0, "power cycle in double speed");
    }

    #[test]
    fn power_on_with_bit_12_set_skips_the_first_edge() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let first = first_div_apu_edge();
        write(emu, 0xFF26, 0x00);
        let _ = run_audio(emu, first + 0x1100, &mut []);
        write(emu, 0xFF26, 0x80);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 2));
        assert_eq!(emu.hw.sys.sched.deadline(EventKind::DivApu), first + 0x2000);
        let _ = run_audio(emu, 0x1000, &mut []);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 1));
        let _ = run_audio(emu, 0x2000, &mut []);
        assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (1, 0));
        let _ = run_audio(emu, 0x2000, &mut []);
        assert_eq!(emu.mem.apu.fs.step, 2);
    }

    /// The power-on skip samples the counter at the end of the NR52 write's
    /// M-cycle: a write in the M-cycle at whose end bit 12 rises (counter
    /// 0x2FFC-0x3000) skips; one M-cycle earlier doesn't. Gambatte
    /// `ch2_late_reset_nr52_1*` / `_2*` (CGB) differ by exactly that.
    #[test]
    fn power_on_in_the_m_cycle_where_bit_12_rises_skips() {
        for (sc, skip) in [(0x2FF8, 0), (0x2FFC, 2)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            write(emu, 0xFF26, 0x00);
            let _ = run_audio(emu, sc - Timer::post_boot(true).sc(0), &mut []);
            assert_eq!(emu.hw.sys.timer.sc(emu.clock()), sc);
            write(emu, 0xFF26, 0x80);
            assert_eq!(emu.mem.apu.fs.skip, skip, "{sc:#x}");
        }
    }

    /// An edge at the end of the NR52 power-on write's M-cycle (counter
    /// 0x3FFC -> 0x4000, bit 12 or 13 falling) happens while the APU is
    /// still off: no skip, and the first step comes one DIV-APU period
    /// later, not at once. Normal and double speed.
    #[test]
    fn power_on_in_the_m_cycle_where_the_div_apu_bit_falls_waits_a_period() {
        for double in [false, true] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            if double {
                let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
                let Emulator { mem, hw } = &mut *emu;
                let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
                assert!(crate::cpu::Bus::speed_switch(&mut ctx, false, false));
            }
            write(emu, 0xFF26, 0x00);
            let k = emu.hw.sys.timer.div_apu_bit() + 1;
            let period = 1u64 << k;
            let sc0 = emu.hw.sys.timer.sc(emu.clock());
            let sc = (((sc0 >> k) + 2) << k) - 4;
            let _ = run_audio(emu, emu.hw.sys.timer.clock_at(sc) - emu.clock(), &mut []);
            assert_eq!(emu.hw.sys.timer.sc(emu.clock()), sc, "double {double}");
            write(emu, 0xFF26, 0x80);
            assert_eq!((emu.mem.apu.fs.step, emu.mem.apu.fs.skip), (0, 0));
            let first = emu.hw.sys.timer.clock_at(sc + 4 + period);
            assert_eq!(
                emu.hw.sys.sched.deadline(EventKind::DivApu),
                first,
                "double {double}"
            );
            let _ = run_audio(emu, first - 32 - emu.clock(), &mut []);
            assert!(emu.clock() < first);
            assert_eq!(emu.mem.apu.fs.step, 0, "double {double}");
            let _ = run_audio(emu, 64, &mut []);
            assert_eq!(emu.mem.apu.fs.step, 1, "double {double}");
        }
    }

    #[test]
    fn one_sample_per_128_clocks_across_run_calls() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut audio = vec![StereoSample { left: 1, right: 1 }; 1000];
        let mut total = 0;
        for clocks in [100, 200, 28, 4, 1000, 40_000, 12] {
            let start = emu.clock();
            let r = run_audio(emu, clocks, &mut audio);
            let end = emu.clock();
            assert_eq!(
                r.audio_samples as u64,
                end / 128 - start / 128,
                "{start}..{end}"
            );
            // Post-boot only CH1 is on: DAC on (NR12 = F3) at volume 0,
            // panned to both sides (NR51 = F3), NR50 = 77: a constant
            // +15 x 8 x 64.
            let dc = StereoSample {
                left: 7680,
                right: 7680,
            };
            assert!(audio[..r.audio_samples].iter().all(|s| *s == dc));
            assert_eq!(emu.mem.apu.out.synced, end, "synced at the end of the call");
            total += r.audio_samples as u64;
        }
        assert_eq!(total, emu.clock() / 128);
        assert_eq!(crate::AUDIO_SAMPLE_RATE, 32_768);
        assert_eq!(crate::CLOCK_HZ / crate::AUDIO_SAMPLE_CLOCKS, 32_768);
    }

    #[test]
    fn a_full_audio_buffer_ends_the_call() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut audio = vec![StereoSample::default(); 10];
        let r = run_audio(emu, u64::from(FRAME_CLOCKS), &mut audio);
        assert!(r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert_eq!(r.audio_samples, 10);
        assert!((1280..1280 + 128).contains(&r.clocks), "{}", r.clocks);
        // The next call continues the window grid.
        let mut audio = vec![StereoSample::default(); 100];
        let r = run_audio(emu, 1000, &mut audio);
        assert!(!r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert_eq!(r.audio_samples as u64, emu.clock() / 128 - 10);
    }

    #[test]
    fn an_empty_audio_buffer_only_advances_and_restarts_the_grid() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let r = run_audio(emu, 1000, &mut []);
        assert_eq!(r.audio_samples, 0);
        assert!(!r.events.contains(Events::AUDIO_BUFFER_FULL));
        assert!(!emu.mem.apu.out.live);
        assert_eq!(emu.mem.apu.out.synced, emu.clock());
        // 100 clocks into a window, then an empty-buffer stretch: the next
        // call with room restarts the grid at its start.
        let mut audio = vec![StereoSample::default(); 10];
        assert_eq!(run_audio(emu, 100, &mut audio).audio_samples, 0);
        let _ = run_audio(emu, 400, &mut []);
        assert_eq!(run_audio(emu, 124, &mut audio).audio_samples, 0);
        assert_eq!(run_audio(emu, 4, &mut audio).audio_samples, 1);
    }

    // --- CH2 (ADR 0021 section 5; M6 task 1(b)) ---

    #[test]
    fn ch2_length_expiry_clears_nr52_bit_1() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF0); // DAC on
        write(emu, 0xFF16, 0x3E); // length 2
        write(emu, 0xFF19, 0xC0); // trigger, length on (fs 0: no extra clock)
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02);
        // The first DIV-APU edge is fs step 1: one length clock.
        let _ = run_audio(emu, first_div_apu_edge() + 100, &mut []);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02, "2 -> 1");
        // Step 3 is the next length clock.
        let _ = run_audio(emu, 2 * 8192, &mut []);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x00, "1 -> 0: off");
    }

    #[test]
    fn ch2_envelope_steps_at_fs_step_0() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF1); // volume 15, down, period 1
        write(emu, 0xFF19, 0x80);
        // Steps 1-7 don't clock the envelope; step 0 (the 8th) does.
        let _ = run_audio(emu, first_div_apu_edge() + 6 * 8192 + 100, &mut []);
        assert_eq!(emu.mem.apu.sq[1].env.volume, 15);
        let _ = run_audio(emu, 8192, &mut []);
        assert_eq!(emu.mem.apu.sq[1].env.volume, 14);
    }

    #[test]
    fn ch2_dac_off_clears_nr52_bit_1() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF17, 0xF0);
        write(emu, 0xFF19, 0x80);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x02);
        write(emu, 0xFF17, 0x07);
        assert_eq!(emu.peek(0xFF26) & 0x02, 0x00);
    }

    #[test]
    fn pcm12_high_nibble_is_ch2s_digital_output() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF16, 0x40); // duty 25%: positions 0 and 7 high
        write(emu, 0xFF17, 0xF0); // volume 15
        write(emu, 0xFF18, 0x00);
        write(emu, 0xFF19, 0x84); // f = 0x400: 4096 clocks per step
        let _ = run_audio(emu, 2000, &mut []);
        assert_eq!(emu.peek(0xFF76), 0x00, "position 0, before the first step");
        let _ = run_audio(emu, 4106 + 6 * 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0xF0, "position 7");
        // A pure read: the same at the same clock.
        assert_eq!(emu.peek(0xFF76), 0xF0);
        let _ = run_audio(emu, 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0xF0, "position 0, started");
        let _ = run_audio(emu, 4096, &mut []);
        assert_eq!(emu.peek(0xFF76), 0x00, "position 1");
    }

    /// CH2 at volume 15, duty 12.5%, frequency `f`, on both sides at
    /// volume 8, after `warm` windows; then `n` samples.
    fn ch2_tone(f: u16, warm: usize, n: usize) -> std::vec::Vec<StereoSample> {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF24, 0x77);
        write(emu, 0xFF25, 0x22);
        write(emu, 0xFF16, 0x00);
        write(emu, 0xFF17, 0xF0);
        write(emu, 0xFF18, f as u8);
        write(emu, 0xFF19, 0x80 | (f >> 8) as u8);
        let mut audio = vec![StereoSample::default(); warm + n];
        let r = run_audio(emu, 128 * (warm + n) as u64, &mut audio);
        assert_eq!(r.audio_samples, warm + n);
        audio.split_off(warm)
    }

    #[test]
    fn a_known_ch2_tone_averages_through_the_mixer() {
        // One duty high (digital 15 -> analog -15) and seven low (+15) per
        // period: average 11.25, times volume 8, times 64 = 5760.
        let want = StereoSample {
            left: 5760,
            right: 5760,
        };
        // f = 2044: one duty period per 128-clock window, every window.
        assert!(ch2_tone(2044, 2, 20).iter().all(|&s| s == want));
        // f = 2047: four whole periods per window (the closed-form path).
        assert!(ch2_tone(2047, 2, 20).iter().all(|&s| s == want));
        // f = 2016: one step per window; eight windows are one period.
        let tone = ch2_tone(2016, 10, 32);
        assert!(tone.iter().any(|&s| s != want));
        for period in tone.chunks(8) {
            let sum: i32 = period.iter().map(|s| i32::from(s.left)).sum();
            assert_eq!(sum, 8 * 5760);
        }
    }

    /// What a [`split_run`] ends with.
    #[derive(Debug, PartialEq, Eq)]
    struct SplitEnd {
        samples: std::vec::Vec<StereoSample>,
        clock: u64,
        regs: crate::Registers,
        out: crate::apu::Output,
        vram_sum: u64,
    }

    /// Runs `program` (at 0100, CGB mode, CH2 sounding on both sides) to
    /// clock `end`, in run calls of at most `chunk` clocks with `k`-slot
    /// audio buffers. Also returns whether a call ended inside a VRAM DMA
    /// stall.
    fn split_run(program: &[u8], end: u64, chunk: u64, k: usize) -> (SplitEnd, bool) {
        let rom = rom(0x8000, 0, 0, 0x80);
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        let emu = load(rom);
        for (reg, v) in [
            (0xFF24, 0x77),
            (0xFF25, 0x22),
            (0xFF16, 0x80), // duty 50%
            (0xFF17, 0xF0),
            (0xFF18, 0x00),
            (0xFF19, 0x87),
        ] {
            write(emu, reg, v);
        }
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let mut buf = vec![StereoSample::default(); k];
        let mut samples = std::vec::Vec::new();
        let mut mid_stall = false;
        while emu.clock() < end {
            let t = emu.clock().saturating_add(chunk).min(end);
            let r = emu.run_until(t, &mut fb, &mut buf);
            samples.extend_from_slice(&buf[..r.audio_samples]);
            mid_stall |= emu.hw.sys.vdma.stall != 0;
        }
        let vram_sum = emu.mem.vram.iter().enumerate();
        let vram_sum = vram_sum.map(|(i, &b)| (i as u64 + 1) * u64::from(b)).sum();
        let end = SplitEnd {
            samples,
            clock: emu.clock(),
            regs: emu.registers(),
            out: emu.mem.apu.out,
            vram_sum,
        };
        (end, mid_stall)
    }

    /// A loop of 128-block GDMAs (each a 4,100-clock CPU stall) that
    /// changes the CH2 frequency between them.
    const GDMA_LOOP: [u8; 0x18] = [
        0x3E, 0xC0, 0xE0, 0x51, // LD A,C0; LDH (51),A
        0xAF, 0xE0, 0x52, // XOR A; LDH (52),A
        0x3E, 0x80, 0xE0, 0x53, // LD A,80; LDH (53),A
        0xAF, 0xE0, 0x54, // XOR A; LDH (54),A
        0x3E, 0x7F, 0xE0, 0x55, // LD A,7F; LDH (55),A: GDMA, 128 blocks
        0x04, 0x78, 0xE0, 0x18, // INC B; LD A,B; LDH (18),A
        0x18, 0xE8, // JR 0100
    ];

    #[test]
    fn a_gdma_stall_across_the_buffer_deadline_drops_no_sample() {
        let end = 3 * u64::from(FRAME_CLOCKS) + 77;
        let (want, _) = split_run(&GDMA_LOOP, end, u64::MAX, 10_000);
        assert!(want.samples.len() as u64 >= end / 128 - 1);
        for k in [1, 7, 33, 549] {
            let (got, mid_stall) = split_run(&GDMA_LOOP, end, u64::MAX, k);
            assert_eq!(got, want, "k = {k}");
            assert!(mid_stall || k == 549, "k = {k}: no FULL inside a stall");
        }
        // Targets inside stalls too.
        let (got, mid_stall) = split_run(&GDMA_LOOP, end, 1_001, 7);
        assert_eq!(got, want);
        assert!(mid_stall);
    }

    #[test]
    fn samples_and_state_are_independent_of_call_and_buffer_splits() {
        // INC B; LD A,B; LDH (18),A; LD C,20; DEC C; JR NZ,-3; JR 0100.
        let program = [
            0x04, 0x78, 0xE0, 0x18, 0x0E, 0x20, 0x0D, 0x20, 0xFD, 0x18, 0xF5,
        ];
        let end = 2 * u64::from(FRAME_CLOCKS) + 333;
        let (want, _) = split_run(&program, end, u64::MAX, 10_000);
        for k in [1, 7, 33, 549] {
            for chunk in [u64::MAX, 997, 128 * 549] {
                let (got, _) = split_run(&program, end, chunk, k);
                assert_eq!(got, want, "k = {k}, chunk = {chunk}");
            }
        }
        // A run of n clocks never completes more than n / 128 + 2.
        let (got, _) = split_run(&program, end, 255, 4);
        assert_eq!(got, want, "4 slots suffice for 255-clock calls");
    }
}
