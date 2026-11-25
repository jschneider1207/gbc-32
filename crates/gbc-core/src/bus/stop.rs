//! STOP as the bus sees it: STOP mode (the frozen system counter and the
//! wake-up) and the CGB speed switch (KEY1; ADR 0020).

use super::Ctx;
use crate::Events;
use crate::cpu::Bus;
use crate::joypad::p1_lines;
use crate::schedule::{EventKind, NEVER};

// Names only the doc comments use.
#[cfg(doc)]
use crate::{cpu::StopKind, joypad::p1_interrupt, timer::Timer};

impl Ctx<'_, '_> {
    /// STOP's DIV reset: with nothing pending (STOP is two bytes), first
    /// the TIMA edge of AGE's later reset ([`Timer::stop_edge`], ADR 0020).
    ///
    /// UNPINNED: spsw-div-reset-point (c): the reset is at the end of
    /// STOP's fetch (SameBoy `enter_stop_mode`, CGB E); AGE (verified on
    /// CGB B/C) has it after the second byte. No ROM tells by itself; the
    /// change needs the human (ADR 0020; `docs/reference/speed-switch.md`).
    /// UNPINNED: spsw-reset-edges (a): the serial clock's edge at this
    /// reset, and the 4 kHz TIMA and frame-sequencer edges of a one-byte
    /// STOP's reset, are the plain DIV-reset rule; no ROM covers them.
    pub(super) fn stop_div_reset(&mut self) {
        if self.sys.ie & self.sys.iflags == 0 {
            let now = self.clock();
            self.sys.timer.stop_edge(now);
        }
        self.div_reset();
    }

    /// STOP mode begins, at the end of STOP's fetch and after its DIV
    /// reset (SameBoy `enter_stop_mode`; ADR 0020, STOP mode amendment).
    ///
    /// - The system counter stops ([`Timer::freeze`]), and with it
    ///   everything clocked from it: TIMA, the serial bit clock and the
    ///   APU's frame sequencer have no deadline until the wake-up
    ///   (SameBoy `timers_run` returns while `stopped`). Nothing may set
    ///   one of the three before [`Self::stop_wake`]: only CPU writes and
    ///   their own events do, and neither happens in STOP mode. The
    ///   serial port's lazy shifts follow the counter's value, not the
    ///   clock (`serial` module docs), so a running transfer holds with
    ///   it; STOP's DIV reset caught it up just before.
    /// - The PPU and the APU's channels run on (on a CGB; SameBoy
    ///   `GB_display_run`, "The PPU does not advance while in STOP mode
    ///   on the DMG", and `timers_run`, which still advances `apu_cycles`
    ///   on a CGB), so frames and samples keep their rate.
    /// - The screen goes black, unless the PPU is in mode 3 (Pan Docs:
    ///   "On CGB, leaving the LCD enabled when invoking STOP will result
    ///   in a black screen. Except if the LCD is in Mode 3, where it will
    ///   keep drawing the current screen"). SameBoy: the PPU loses
    ///   palette RAM unless the CPU's access to it was blocked at the
    ///   STOP (`cgb_palettes_ppu_blocked = !cgb_palettes_blocked`), and
    ///   draws black without it, in either CGB mode. Whole frames here
    ///   ([`Self::stop_frame`]).
    ///
    /// UNPINNED: stop-mode (a): all of STOP mode (this, [`Self::stop_enter`],
    /// [`Self::stop_wake`]) follows SameBoy's CGB path and Pan Docs; no
    /// ROM in the manifest enters plain STOP (review A1-01). Accepted as
    /// implemented by the human (2026-11-24). A hardware ROM reading DIV
    /// and TIMA around a STOP ended by a button would settle the counter.
    #[cold]
    #[inline(never)]
    pub(super) fn stop_freeze(&mut self) {
        let now = self.clock();
        self.sys.timer.freeze(now);
        for kind in [EventKind::Timer, EventKind::Serial, EventKind::DivApu] {
            self.sys.sched.set(kind, NEVER);
        }
        // Synced, for `drawing` (as `write_palette_data`); reschedules.
        self.sync_ppu_to(now);
        let speed = self.hot.speed();
        let p = &self.sys.ppu;
        self.sys.stop_black = p.lcd_enabled() && !p.drawing() && !p.palette_blocked(now, speed);
    }

    /// STOP mode is entered, after STOP's second byte: OAM DMA pauses
    /// and HBlanks start no VRAM DMA blocks, as in HALT (SameBoy
    /// `GB_dma_run` returns while `stopped`; `GB_display_run` starts HDMA
    /// only with `!gb->stopped`). A block already requested waits for the
    /// wake-up. SameBoy decides both at the end of STOP's fetch; here the
    /// second byte's read comes first, so that it isn't a read with the
    /// transfer paused.
    #[cold]
    #[inline(never)]
    pub(crate) fn stop_enter(&mut self) {
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        if self.sys.vdma.hblank {
            let in_hblank = self.hdma_period(self.clock());
            let v = &mut self.sys.vdma;
            v.halted = true;
            v.wake_ok = !in_hblank;
        }
    }

    /// The wake-up check of a CPU in STOP mode: whether a selected P1
    /// line is low (Pan Docs: "STOP is terminated by one of the P10 to
    /// P13 lines going low"; SameBoy `GB_cpu_run`: `JOYP & 0xF != 0xF`,
    /// sampled every M-cycle). The lines were all high when STOP mode
    /// began ([`Bus::stop_begin`]) and the select bits can't change in it,
    /// so a low line is a button pressed in a selected group since: the
    /// joypad interrupt's condition ([`p1_interrupt`]).
    ///
    /// If so, STOP mode ends here (SameBoy `leave_stop_mode`): the counter
    /// runs again from the value it held, with the deadlines that follow
    /// it; OAM DMA resumes as after HALT; and in mode 0, after a STOP
    /// outside it, an HBlank transfer starts a block
    /// (`allow_hdma_on_wake`). The CPU then idles two M-cycles.
    #[cold]
    #[inline(never)]
    pub(crate) fn stop_wake(&mut self) -> bool {
        if p1_lines(self.sys.buttons, self.sys.joyp_select) == 0x0F {
            return false;
        }
        let now = self.clock();
        self.sys.stop_black = false;
        self.sys.timer.thaw(now);
        self.set_apu_deadline(now);
        self.schedule_timer();
        self.schedule_serial();
        self.halt_end();
        let _ = self.wake_stalls();
        true
    }

    /// After an idle in STOP mode: a frame completed in it is black
    /// ([`Self::stop_freeze`]). A simplification of SameBoy's per-pixel
    /// rule: the frame STOP mode begins in is black from its top, and the
    /// one it ends in is drawn normally from its top.
    ///
    /// UNPINNED: stop-mode-screen (a): whole black frames stand for
    /// SameBoy's per-pixel palette rule and Pan Docs' "black screen
    /// unless in mode 3"; no ROM or capture shows the screen in STOP mode.
    #[inline]
    pub(crate) fn stop_frame(&mut self) {
        if self.sys.stop_black && self.events.contains(Events::FRAME_READY) {
            self.frame.fill(0);
        }
    }

    /// The CGB speed switch of a STOP ([`StopKind::Switch`]), after its second
    /// byte if read; returns whether the CPU waits ([`Bus::speed_switch`]).
    /// After SameBoy `stop()`
    /// and `GB_advance_cycles` (MIT; ADR 0020 has the derivation):
    ///
    /// - KEY1 bit 0 clears and the speed toggles.
    /// - Nothing pending: the CPU halts for 0x20008 T-cycles (0x8002
    ///   M-cycles; `speed_switch_halt_countdown`) by the timer, which keeps
    ///   running (Pan Docs: FF4D - KEY1). The rest of the machine is frozen
    ///   for its first 5 T-cycles (`speed_switch_freeze`), so it sees
    ///   0x8001 M-cycles at the new speed (exact to double speed, one clock
    ///   long to single speed, where SameBoy itself avoids "odd mode"): the
    ///   timer runs one M-cycle ahead, which cancels STOP's second byte,
    ///   whose M-cycle it doesn't count (`timers_run` returns while
    ///   `stopped`). An interrupt ends the halt early. OAM DMA is paused
    ///   through it (`GB_dma_run` returns while `halted`), as in HALT.
    /// - An interrupt pending: no halt; to double speed the next two
    ///   M-cycles take 6 clocks for the PPU (one more M-cycle here, which
    ///   the timer doesn't count).
    /// - IME off: the timer starts 4 T-cycles late (`enter_stop_mode`:
    ///   `div_cycles = -4`, the DIV reset held). On CGB B/C the same hold
    ///   applies with an interrupt pending and IME on (below; SameBoy,
    ///   modelling CGB E, has none there).
    ///
    /// # Order (ADR 0020, 2026-11-24 amendment)
    /// The speed is an input to every lazily evaluated component and to
    /// every deadline, so the switch goes in four steps, in this order:
    ///
    /// 1. Everything lazy is brought up to the switch clock with the
    ///    **old** speed's rules: the PPU, OAM DMA (`dma_pause`) and the
    ///    timer's reloads (`Timer::speed_switch` catches up before it
    ///    rebases). The APU runs on base clocks and has no rule that
    ///    depends on the speed. Nor has the serial port: its lazy shifts
    ///    follow the counter's value (`serial` module docs), which the
    ///    switch leaves alone, so a catch-up before the switch and one
    ///    after it apply the same shifts and it needs none here, only
    ///    its deadline (step 3). The scheduled inputs keep no lazy state
    ///    (deadlines only).
    /// 2. The speed changes (`Hot::set_speed`, the timer's counter rate, the
    ///    PPU's walk, a paused OAM DMA's M-cycle length).
    /// 3. Every deadline that depends on the speed is set again for the
    ///    **new** one: Ppu, DivApu, Input, Timer, Serial.
    /// 4. Only then may time pass and the event loop run (`boundary`):
    ///    the halt's 3 odd clocks to single speed.
    ///
    /// A deadline left from the old speed must never reach `boundary`: its
    /// handler would run at a clock the component isn't due at and re-arm
    /// from there (review A1-02: a fast-clock serial bit shifted twice).
    /// Lazy work left from before the switch must never run after step 2:
    /// it would run with the new speed's rules (review E8: the PPU's
    /// update points; `Ppu::sync` takes the speed of the sync that reaches
    /// a point, and only points that raise an interrupt or end a line or
    /// frame are deadlines).
    pub(super) fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
        let now = self.clock();
        let to = self.hot.speed().toggled();
        let m = to.mcycle();
        // Step 1 (the timer's part is in `Timer::speed_switch` below).
        // The PPU first, as before any change to what it sees: its walk's
        // points up to `now` belong to the old speed (a pipeline start's
        // late-WY decision, line 0's and line 153's points).
        self.run_ppu_to_call(now);
        if self.hot.dma != 0 {
            self.dma_pause();
        }
        let dma_paused = self.sys.ppu.dma.paused != NEVER;
        if dma_paused {
            // Unlike HALT, the transfer's last step (its end) is held for
            // the PPU even with every byte written: gambatte's STOP brings
            // the transfer only up to this M-cycle (HALT: one further),
            // and its cgb04c `oamdma_late_speedchange_stat_1` (the switch
            // in the last byte's M-cycle) misses the object the PPU finds
            // in `_2` (the switch in the end's M-cycle: no pause).
            self.sys.ppu.dma.ppu_to = NEVER;
        }
        // Step 2.
        self.sys.key1 = 0;
        if dma_paused {
            self.dma_retime(to);
        }
        self.hot.set_speed(to);
        // (Sets the Ppu deadline for the new walk too: step 3's.)
        self.ppu_speed_changed();
        // The held DIV reset (4 T-cycles): with IME off (SameBoy), and on
        // CGB B/C also with an interrupt pending. AGE `spsw-interrupts`
        // (pending, IME on, both directions): the first DIV increment and
        // the 16 kHz TIMA increment after the switch come one M-cycle
        // later on CGB B/C (`OFS_B`, verified on CPU CGB B and C) than on
        // CGB E, which SameBoy's model (no hold with IME on) matches.
        // UNPINNED: spsw-ime-on-timer (c): IME on with nothing pending has
        // no hold (SameBoy, CGB E, "I don't trust this if"); AGE's model
        // leaves the timer 4 T-cycles later. No ROM decides it: `ei`, a
        // two-byte switch with IE = 0, then a DIV or 262 kHz TIMA read
        // would (`docs/reference/speed-switch.md`; with the human).
        let mut freeze = if ime && !pending { 0 } else { 4 };
        if pending && to.is_double() {
            freeze += 4;
        }
        // To single speed the halt is one clock short of whole M-cycles
        // (below); the timer counts that clock, so it stays on the CPU's
        // M-cycle grid.
        let short = i64::from(!to.is_double() && !pending);
        // T-cycles to clocks at the new speed.
        self.sys.timer.speed_switch(now, freeze * m / 4, short != 0);
        self.mem.apu.fs.ds_delay ^= to.is_double();
        if !pending && self.sys.vdma.hblank {
            self.vdma_stop();
        }
        // Step 3. The DIV-APU bit changes (12 <-> 13) with the counter
        // speed, and a switch to double speed toggles the frame
        // sequencer's delay (`apu::Fs::ds_delay`, above). The M-cycle length
        // changes, and with it the boundary a scheduled button change
        // rounds down to (ADR 0012). The timer and the serial clock follow
        // the rebased counter, which already accounts for the grid moving
        // in step 4 (`short`), so nothing here has to be set again after
        // it. `schedule_serial` reschedules the countdown, for the DivApu
        // and Input deadlines too.
        self.set_apu_deadline(now);
        self.sys.schedule_inputs(to);
        self.schedule_timer();
        self.schedule_serial();
        if short != 0 {
            // Step 4. The halt's odd clocks (`switch_halt_end` below), idled
            // first (the CPU halts at `now`, so after `vdma_stop`): the
            // CPU's M-cycle grid moves by 3 (-1) clocks. What is due in
            // them runs at their end, the first boundary of the new grid.
            self.skip_clocks(3);
        }
        if !pending {
            // To single speed, Gambatte's PPU gains a clock at the switch
            // (`PPU::speedChange`: `p_.now -= isDoubleSpeed()`), and SameBoy
            // counts 0x20003 T-cycles at single speed after its 5-cycle
            // freeze: the PPU sees one clock less than 0x8001 M-cycles from
            // `now` to the next fetch (3 clocks, idled above, then 0x7FFF
            // M-cycles and the wake-up), so the CPU's M-cycle grid moves by
            // a clock against the PPU's, as on hardware (gambatte
            // `lcd_offset` tests, offsets 1 and 3).
            self.sys.switch_halt_end = now + 0x8000 * m - short as u64;
            // The halt is whole M-cycles from here (the odd clocks are
            // idled): `idle_speed_switch` ends exactly on its end.
            debug_assert!(
                (self.sys.switch_halt_end)
                    .wrapping_sub(self.clock())
                    .is_multiple_of(m)
            );
            return true;
        }
        if dma_paused {
            self.dma_resume();
        }
        // The CPU idles one M-cycle to double speed.
        to.is_double()
    }

    /// Whether the speed-switch halt has run its length.
    #[inline]
    pub(crate) fn speed_switch_over(&self) -> bool {
        self.clock() >= self.sys.switch_halt_end
    }

    /// As [`Bus::halt_end`], except (gambatte CGB C
    /// captures, ADR 0017 speed-switch amendment):
    /// - OAM DMA resumes one M-cycle later, at the end of the wake-up
    ///   M-cycle (`oamdma/oamdmasrcC0_speedchange_readC000`,
    ///   `dma/hdma_transition_speedchange_oamdma`: a WRAM read after the
    ///   switch sees the transfer one byte earlier).
    ///
    /// As after HALT, an HBlank in the wake-up M-cycle starts no HDMA
    /// block ([`Bus::wake_stalls`] clears `halted`).
    /// The wake-up's own block (HALT began outside the HBlank) is checked
    /// at the end of the wake-up M-cycle, as after HALT, also when the
    /// countdown ends it (gambatte `intevent_unhalt`;
    /// `dma/hdma_m3speedchange_late_m0wakeup_1`), and runs before the next
    /// instruction.
    #[inline]
    pub(crate) fn speed_switch_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        if d.paused != NEVER {
            d.paused -= self.hot.mcycle();
        }
        self.halt_end();
    }

    /// The halted skip ([`Bus::idle_halted`]), stopping at the halt's
    /// last M-cycle. Called while the halt isn't over
    /// ([`Self::speed_switch_over`]); always idles at least one M-cycle,
    /// so even a halt end off the M-cycle grid (`speed_switch` keeps it
    /// on the grid, and asserts so) is passed rather than never reached.
    #[inline]
    pub(crate) fn idle_speed_switch(&mut self) {
        let m = self.hot.mcycle() as u32;
        // The halt is 0x20000 clocks at most: the difference fits.
        #[cfg(feature = "eager-sync")]
        self.eager_sync();
        let left = self.sys.switch_halt_end.wrapping_sub(self.clock()) as u32 / m;
        let n = self.mcycles_to_deadline().min(left).max(1);
        self.hot.t_minus += n as i32;
        if self.hot.t_minus >= 0 {
            self.boundary();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::bus::test_util::{setup, with_ctx};

    /// A switch to double speed with the APU on delays the APU by one
    /// 2 MHz tick (`apu::Fs::ds_delay`): a CH1 trigger in double speed
    /// then takes the other of its two delays than the power-on phase
    /// gives (gambatte cgb04c
    /// `speedchange_ch1_nr4init_duty0_pos6_to_pos7_timing_1`/`_2`). A
    /// power cycle in double speed clears the delay.
    #[test]
    fn a_switch_to_double_speed_with_the_apu_on_flips_the_trigger_phase() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // CH1 DAC on, f = 0x700 (1024 clocks per step), triggered off
            // at two consecutive M-cycles: the clocks to the first step
            // against gambatte's linear rule with the power-on 1 MHz phase
            // `lf_div` (`Apu::trigger_delay`), flipped by `flip`.
            let check = |ctx: &mut Ctx, flip: u64| {
                ctx.write(0xFF12, 0xF0);
                for _ in 0..2 {
                    let t = ctx.clock();
                    ctx.write(0xFF14, 0x87);
                    let apu = &ctx.mem.apu;
                    let first = apu.out.synced + u64::from(apu.sq[0].countdown);
                    let lf_div = ((t >> 1) ^ u64::from(apu.fs.phase) ^ 1) & 1;
                    assert_eq!(first - t, 1024 + 6 + 2 * (lf_div ^ flip), "at {t}");
                    // Off again (DAC off, on) for the next trigger.
                    ctx.write(0xFF12, 0x00);
                    ctx.write(0xFF12, 0xF0);
                }
            };
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(ctx.speed_switch(true, false));
            while !ctx.speed_switch_over() {
                ctx.idle_speed_switch();
            }
            ctx.speed_switch_end();
            assert!(ctx.mem.apu.fs.ds_delay);
            check(ctx, 1);
            ctx.write(0xFF26, 0x00);
            ctx.write(0xFF26, 0x80);
            assert!(!ctx.mem.apu.fs.ds_delay);
            check(ctx, 0);
        });
    }
}
