//! The PPU's side of the bus: syncing it to the clock, its deadline in the
//! event table and the writes to its registers and to palette RAM (`ppu`
//! module; ADR 0002).

use super::Ctx;
use crate::ppu::{self, View, edges};
use crate::schedule::EventKind;

// Names only the doc comments use.
#[cfg(doc)]
use crate::ppu::Ppu;

/// The index step of a BCPD/OCPD write in CGB mode: returns the palette RAM
/// byte the index register (BCPS/OCPS) selects in bits 0-5, then, if its
/// bit 7 (auto-increment) is set, increments bits 0-5, wrapping 3F to 00
/// and keeping bit 7 (Pan Docs: LCD Color Palettes, FF68 BCPS; SameBoy
/// `write_high_memory`, GB_IO_BGPD). It happens even when the PPU blocks
/// the write (Pan Docs: "even if the write fails due to CRAM being
/// inaccessible"; SameBoy `cgb_palettes_blocked`).
fn palette_index_step(index: &mut u8) -> usize {
    let i = usize::from(*index & 0x3F);
    if *index & 0x80 != 0 {
        *index = (*index & 0x80) | (index.wrapping_add(1) & 0x3F);
    }
    i
}

/// [`Ctx::run_ppu`]: the walk alone, without posting timed requests.
const PPU_WALK: u8 = 0;
/// [`Ctx::run_ppu`]: a sync.
const PPU_SYNC: u8 = 1;
/// [`Ctx::run_ppu`]: the sync the PPU deadline runs.
const PPU_DUE: u8 = 2;

impl Ctx<'_, '_> {
    /// The `Ppu` deadline: catches the PPU up to the current clock and
    /// reschedules.
    #[inline(never)]
    pub(super) fn sync_ppu(&mut self, now: u64) {
        self.run_ppu::<PPU_DUE>(now);
        let s = &mut *self.sys;
        let at = s.ppu.next_event(self.hot.speed());
        s.sched.rearm(EventKind::Ppu, at);
    }

    /// Catches the PPU up to clock `at` (the current clock, or one dot
    /// later for a register write timed there) and reschedules. A running
    /// OAM DMA is brought up to date first: the PPU may read OAM.
    pub(super) fn sync_ppu_to(&mut self, at: u64) {
        self.run_ppu_to_call(at);
        self.schedule_ppu();
    }

    /// [`Self::sync_ppu_to`] without the rescheduling. `HOW`:
    /// - `PPU_SYNC`: `Ppu::sync`;
    /// - `PPU_DUE`: the same at the PPU deadline, where an update point
    ///   is due (`Ppu::sync_due`);
    /// - `PPU_WALK`: the walk alone (`Ppu::walk_to`), for a caller that
    ///   isn't an IF observer; a timed interrupt request due by `at` then
    ///   waits for the next sync, which the PPU deadline (at or before the
    ///   request) or an IF observer's own check of that deadline runs.
    #[inline(always)]
    fn run_ppu<const HOW: u8>(&mut self, at: u64) {
        if self.hot.dma != 0 {
            self.dma_sync(self.clock());
        }
        let speed = self.hot.speed();
        let s = &mut *self.sys;
        let (ppu, iflags) = (&mut s.ppu, &mut s.iflags);
        let events = match HOW {
            PPU_WALK => ppu.walk_to(at, self.mem, self.frame, iflags, speed),
            PPU_DUE => ppu.sync_due(at, self.mem, self.frame, iflags, speed),
            _ => ppu.sync(at, self.mem, self.frame, iflags, speed),
        };
        self.raise(events);
        if self.sys.vdma.hblank {
            self.vdma_arm(at, true);
        }
    }

    /// [`Self::run_ppu`] ([`PPU_SYNC`]) as a call, for the callers other
    /// than the PPU deadline and `write_palette_data` (raster palette
    /// effects), which are hot enough to inline it.
    #[inline(never)]
    pub(super) fn run_ppu_to_call(&mut self, at: u64) {
        self.run_ppu::<PPU_SYNC>(at);
    }

    /// Sets the Ppu deadline and reschedules.
    pub(super) fn schedule_ppu(&mut self) {
        let s = &mut *self.sys;
        s.sched
            .set(EventKind::Ppu, s.ppu.next_event(self.hot.speed()));
        self.reschedule();
    }

    /// A speed switch (after `Hot::set_speed`): the walk's line-0 and
    /// line-153 points differ per speed (`ppu::edges::ly0_pulse`), so the PPU
    /// re-seeks its cursor and the deadline moves (to single speed they
    /// come earlier).
    #[cold]
    #[inline(never)]
    pub(super) fn ppu_speed_changed(&mut self) {
        self.sys.ppu.speed_changed(self.hot.speed());
        self.schedule_ppu();
    }

    /// Writes to the PPU registers FF40-FF45 and FF47-FF4B. The PPU is
    /// synced to the write first (ADR 0002): the write lands between the
    /// update points before and after it, and moves the next deadline.
    /// Out of line, off `write_io`.
    #[inline(never)]
    pub(super) fn write_ppu(&mut self, reg: usize, value: u8) {
        let now = self.clock();
        let speed = self.hot.speed();
        match reg {
            0x40 => {
                self.run_ppu_to_call(now);
                // Double speed: the fetcher sees the write one dot earlier
                // (as SCX, SCY below; SameBoy `GB_CONFLICT_LCDC_CGB_DOUBLE`
                // writes at `pending_cycles - 2`, 8 MHz ticks).
                let at = now - u64::from(speed.extra());
                let s = &mut *self.sys;
                s.ppu.flush(at, self.mem, self.frame);
                s.ppu.hold_fetch(at, self.mem, reg, value, false);
                // Turning the LCD off outside mode 0 starts an HBlank
                // transfer block (SameBoy `GB_lcd_off`).
                if s.vdma.hblank && value & 0x80 == 0 && s.ppu.stat(now, speed) & 3 != 0 {
                    s.vdma.pending = true;
                    self.end_run();
                }
                let s = &mut *self.sys;
                if (value ^ s.ppu.lcdc) & 0x04 != 0 {
                    s.ppu.obj_size_write(now, speed, self.mem);
                    s.ppu.obj_size_fetch(at, value, self.mem);
                }
                let win = (value ^ s.ppu.lcdc) & 0x20 != 0;
                // The line's WY check, if past, saw the old LCDC.5 (as for
                // a WY write): clearing it doesn't undo a match.
                if s.ppu.lcdc & 0x20 != 0 {
                    s.ppu.wy_write(now, self.mem, speed);
                }
                s.ppu.write_lcdc(now, value, &mut s.iflags, speed);
                s.ppu.check_wy_at(now, self.mem, speed);
                if win {
                    s.ppu.window_write(at, self.mem, None);
                }
            }
            0x41 => {
                // CGB: in two phases one dot apart; for the first, the LYC
                // select (double speed: the mode-0 select) keeps its old
                // value (SameBoy `GB_CONFLICT_STAT_CGB`, `_DOUBLE`). When
                // the write sets that select, the selects it clears stay
                // set for the first phase too: switching from an active
                // source to an active LY=LYC keeps the line high, with no
                // interrupt (gambatte `miscmstatirq/lycstatwirq_trigger_*`
                // CGB `E0`, `statChangeTriggersStatIrqCgb`; behavior only,
                // GPL). SameBoy clears them at once, a one-dot low glitch.
                let keep = if speed.is_double() { 0x08 } else { 0x40 };
                let old = self.sys.ppu.stat_select();
                let mut first = (value & !keep) | (old & keep);
                if value & !old & keep != 0 {
                    first |= old;
                }
                self.run_ppu_to_call(now);
                let s = &mut *self.sys;
                s.ppu
                    .write_stat(now, first, &mut s.iflags, speed, View::StatPhase1);
                let at = now + edges::STAT_PHASE2.delay(speed);
                // Single speed, phase 2 on line 0's mode-2 pulse: mode 1
                // still holds the line there (gambatte's line 0 starts a dot
                // later, `Ppu::on_ly0_pulse`), so a write that keeps it up
                // requests nothing. The pulse itself runs first (a mode 2
                // the write selects is past it). CGB04C
                // `miscmstatirq/lycstatwirq_trigger_ly00_10_50_1` (`10 ->
                // 50`, LYC = 0: E0), `m2enable/late_enable_m1disable_ly0_2`.
                // (Synced to `now`, the dot before, for phase 1.)
                let pulse = self.sys.ppu.on_ly0_pulse(at, speed);
                let high = self.sys.ppu.stat_line_high();
                self.run_ppu_to_call(at);
                let s = &mut *self.sys;
                let old = s.iflags;
                s.ppu
                    .write_stat(at, value, &mut s.iflags, speed, View::StatPhase2);
                if pulse && high && s.ppu.stat_line_high() {
                    s.iflags &= old | !ppu::INT_STAT;
                }
            }
            0x44 => {} // LY is read-only.
            0x45 => {
                // Single speed: one dot into the M-cycle (SameBoy
                // `GB_CONFLICT_WRITE_CPU`); double speed: at the access
                // (`GB_CONFLICT_READ_OLD`).
                let at = now + edges::LYC_WRITE.delay(speed);
                self.run_ppu_to_call(at);
                let s = &mut *self.sys;
                if speed.is_double() {
                    s.ppu.write_lyc(at, value, &mut s.iflags, speed);
                } else {
                    // Single speed: the STAT interrupt the write requests
                    // (LY = LYC newly true with the LYC select set)
                    // reaches IF `edges::LYC_WRITE_IRQ` dots after the
                    // write lands, in the next M-cycle: the interrupt
                    // check or IF read at the end of the write's M-cycle
                    // doesn't see it, one an M-cycle later does.
                    let mut requested = 0;
                    s.ppu.write_lyc(at, value, &mut requested, speed);
                    if requested != 0 {
                        let due = at + edges::LYC_WRITE_IRQ;
                        s.ppu.request_irq(due, requested, &mut s.iflags);
                    }
                }
            }
            // SCY SCX BGP OBP0 OBP1 WY WX: the pixels already output in
            // mode 3 are drawn with the old value first. Where in the
            // M-cycle the write lands (SameBoy `cgb_conflict_map`,
            // single speed): SCY, SCX and WY at the access
            // (`GB_CONFLICT_READ_OLD`); the palettes one dot earlier on
            // CGB C (`GB_CONFLICT_PALETTE_CGB`, `pending_cycles - 1`;
            // Mealybug `m3_bgp_change`, `m3_obp0_change` CGB C
            // screenshots); WX one dot later (`GB_CONFLICT_WRITE_CPU`).
            // Double speed: SCX and SCY (and LCDC above) one dot before the
            // access (SameBoy `GB_CONFLICT_SCX_DMG_AND_CGB_DOUBLE`:
            // `pending_cycles - 2` 8 MHz ticks; SameBoy keeps SCY at the
            // access, but gambatte `scy_during_m3_*ds*` want the same dot
            // as SCX: docs/design/mid-line-ppu.md step D), the others at
            // the access (approximation).
            _ => {
                let at = match reg {
                    0x42 | 0x43 if speed.is_double() => now - 1,
                    _ if speed.is_double() => now,
                    0x47..=0x49 => now - 1,
                    0x4B => now + 1,
                    _ => now,
                };
                self.run_ppu_to_call(now.max(at));
                let s = &mut *self.sys;
                if matches!(reg, 0x42 | 0x43) {
                    s.ppu.scroll_write(at, self.mem, self.frame, reg, value);
                } else {
                    s.ppu.flush(at, self.mem, self.frame);
                }
                if reg == 0x4A {
                    s.ppu.wy_write(now, self.mem, speed);
                }
                self.mem.io[reg] = value;
                match reg {
                    0x47..=0x49 => s.ppu.write_palette(
                        reg,
                        value,
                        &s.config.dmg_palettes,
                        s.cgb_mode,
                        &mut self.mem.bg_pairs,
                    ),
                    // The WY condition and the window start in mode 3
                    // (`Ppu::wy_written`; WY lands at the access).
                    0x4A => s.ppu.wy_written(now, self.mem, speed),
                    0x4B => s.ppu.window_write(at, self.mem, Some(ppu::WX_ON_LEAD)),
                    _ => {}
                }
            }
        }
        // A write in mode 3 may have moved the line's mode-0 start (an SCX
        // write in the fine-scroll discard, a window start or cut:
        // `Ppu::end_moved`), after the sync above scheduled the HBlank
        // transfer's trigger from the old one: it moves too. Without
        // this the trigger stayed where it was until the next PPU sync,
        // and with none before it the block ran off the HBlank (the
        // state depended on where the PPU was synced:
        // `scripts/check-sync-invariance.sh`).
        if self.sys.vdma.hblank {
            self.vdma_arm(now, true);
        }
        // The selects, LYC or LCDC may move the next deadline.
        self.schedule_ppu();
        // An LCDC, STAT or LYC write may have requested the STAT
        // interrupt.
        self.irq_attention();
    }

    /// A BCPD (`obj` false) or OCPD (`obj` true) write in CGB mode: steps
    /// the index; unless the PPU blocks palette RAM
    /// ([`Ppu::palette_blocked`]), stores the byte at BCPS/OCPS and
    /// re-resolves the color.
    /// Out of line: rare next to the other I/O writes (hi-color raster
    /// effects do it a few times a line).
    #[inline(never)]
    pub(super) fn write_palette_data(&mut self, obj: bool, value: u8) {
        let now = self.clock();
        // Palette RAM only: not an IF observer.
        self.run_ppu::<PPU_WALK>(now);
        let s = &mut *self.sys;
        let i = palette_index_step(&mut self.mem.io[if obj { 0x6A } else { 0x68 }]);
        // Synced to `now`: drawing (latched, so past the window start, and
        // before the mode-3 end) means blocked, and when not drawing there
        // are no pixels to flush with the old colors.
        if s.ppu.drawing() || s.ppu.palette_blocked(now, self.hot.speed()) {
            return;
        }
        if obj {
            self.mem.obj_pal[i] = value;
            s.ppu.write_ocpd(i, self.mem);
        } else {
            self.mem.bg_pal[i] = value;
            s.ppu.write_bcpd(i, self.mem);
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::bus::test_util::{idle_to, load, rom, setup, with_ctx, write};
    use crate::cpu::Bus;
    use crate::ppu::Frame;
    use crate::speed::Speed;
    use crate::{Emulator, Events, FRAME_CLOCKS};
    use std::boxed::Box;

    /// A DMG-mode emulator whose BG is tile 0 (color 3) in even map
    /// columns and tile 1 (color 0) in odd ones.
    fn striped_bg() -> &'static mut Emulator<'static> {
        striped_bg_with(0x00)
    }

    /// [`striped_bg`] with header byte 0x143 = `cgb_flag`.
    fn striped_bg_with(cgb_flag: u8) -> &'static mut Emulator<'static> {
        let emu = load(rom(0x8000, 0, 0, cgb_flag));
        emu.mem.vram[..16].fill(0xFF);
        for col in 0..32 {
            emu.mem.vram[0x1800 + col] = (col & 1) as u8;
        }
        emu
    }

    /// Runs `emu` to line position `t` of line `ly` in the first full
    /// frame (an M-cycle boundary: `t` is 3 mod 4), writes `value` to
    /// `reg` there, and runs past the line's end. Returns the line's
    /// pixels.
    fn write_in_mode3(emu: &mut Emulator, ly: usize, t: u64, reg: u16, value: u8) -> [u16; 160] {
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * ly as u64;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + t, &mut fb, &mut []);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        assert_eq!(ctx.clock(), line + t);
        ctx.write(reg, value);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        drop(ctx);
        fb.as_chunks::<160>().0[ly]
    }

    /// A BGP write in mode 3 changes the pixels from the one being output
    /// one dot before the access on (SameBoy `GB_CONFLICT_PALETTE_CGB`,
    /// CGB C); the ones before keep the old colors.
    #[test]
    fn mid_mode3_bgp_write_splits_the_line() {
        let emu = striped_bg();
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        // No SCX fine scroll, no objects: pixel x is output at t = 96 + x.
        // The write at t = 131 lands at 130: pixels 0-33 are out (34 at
        // the access itself; both in a color-3 tile).
        let row = write_in_mode3(emu, 5, 131, 0xFF47, 0x3C);
        for (x, &p) in row.iter().enumerate() {
            let color3 = (x / 8) % 2 == 0;
            // BGP FC: color 3 -> shade 3; 3C: color 3 -> shade 0, color 0
            // -> shade 0 either way.
            let want = match (color3, x < 34) {
                (true, true) => gray[3],
                _ => gray[0],
            };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// BCPD/OCPD writes store at the BCPS/OCPS index and, with bit 7 set,
    /// increment it with bits 0-5 wrapping (bit 7 kept, bit 6 reads 1);
    /// reads return the byte at the index without incrementing (Pan Docs:
    /// LCD Color Palettes; SameBoy `write_high_memory`/`read_high_memory`).
    #[test]
    fn cgb_palette_data_auto_increments_and_reads_back() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF68, 0xBE); // auto-increment, index 3E
        for v in [0x11, 0x22, 0x33] {
            write(emu, 0xFF69, v);
        }
        assert_eq!(emu.peek(0xFF68), 0xC1);
        assert_eq!((emu.mem.bg_pal[0x3E], emu.mem.bg_pal[0x3F]), (0x11, 0x22));
        assert_eq!(emu.mem.bg_pal[0], 0x33);
        // No auto-increment: writes and reads stay at the index.
        write(emu, 0xFF68, 0x3F);
        assert_eq!((emu.peek(0xFF69), emu.peek(0xFF69)), (0x22, 0x22));
        write(emu, 0xFF69, 0x44);
        assert_eq!((emu.peek(0xFF68), emu.peek(0xFF69)), (0x7F, 0x44));
        // OCPD: the same, on OBJ palette RAM.
        write(emu, 0xFF6A, 0xBF);
        write(emu, 0xFF6B, 0x55);
        write(emu, 0xFF6B, 0x66);
        assert_eq!(emu.peek(0xFF6A), 0xC1);
        write(emu, 0xFF6A, 0x3F);
        assert_eq!(emu.peek(0xFF6B), 0x55);
        write(emu, 0xFF6A, 0x00);
        assert_eq!(emu.peek(0xFF6B), 0x66);
        assert_eq!(emu.mem.bg_pal[0x3F], 0x44);
    }

    /// Mode 3 (CGB mode): a BCPD/OCPD read gives FF and a write is dropped
    /// but still steps the index; after mode 3 both work again (Pan Docs:
    /// LCD Color Palettes; SameBoy `cgb_palettes_blocked`).
    #[test]
    fn cgb_palette_data_is_blocked_in_mode3() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * 5;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 150, &mut fb, &mut []);
        emu.mem.bg_pal[0] = 0x5A;
        emu.mem.obj_pal[1] = 0xA5;
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write(0xFF68, 0x80);
        ctx.write(0xFF6A, 0x01);
        let reads = [ctx.read(0xFF69), ctx.read(0xFF6B)];
        ctx.write(0xFF69, 0x12);
        ctx.write(0xFF6B, 0x34);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        let after = [ctx.read(0xFF69), ctx.read(0xFF6B)];
        drop(ctx);
        assert_eq!(reads, [0xFF, 0xFF]);
        // Not stored; BCPS stepped (auto-increment), OCPS not.
        assert_eq!((emu.mem.bg_pal[0], emu.mem.obj_pal[1]), (0x5A, 0xA5));
        assert_eq!((emu.mem.io[0x68], emu.mem.io[0x6A]), (0x81, 0x01));
        // HBlank: readable again (BCPS now at byte 1).
        assert_eq!(after, [emu.mem.bg_pal[1], 0xA5]);
    }

    /// An OCPD write resolves its OBJ color right away (bit 15 is not a
    /// color bit; Pan Docs: LCD Color Palettes).
    #[test]
    fn ocpd_write_sets_the_obj_color() {
        let emu = load(rom(0x8000, 0, 0, 0x80));
        write(emu, 0xFF6A, 0x80 | 0x0E); // OBJ palette 1, color 3
        write(emu, 0xFF6B, 0x34);
        write(emu, 0xFF6B, 0x92);
        assert_eq!(emu.hw.sys.ppu.colors.obj[1][3], 0x1234);
    }

    /// Line 5 of the first full frame of `emu` (striped BG).
    fn line5(emu: &mut Emulator) -> [u16; 160] {
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * 5;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 400, &mut fb, &mut []);
        fb.as_chunks::<160>().0[5]
    }

    /// CGB mode: BG colors come from palette RAM (BG palette 0 here, no
    /// attributes yet); BGP has no effect; the other colors are the boot
    /// ROM's white.
    #[test]
    fn cgb_mode_bg_colors_come_from_palette_ram() {
        let emu = striped_bg_with(0x80);
        // Off clock 0 (the BGP write path looks one dot back).
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(100, &mut fb, &mut []);
        write(emu, 0xFF68, 0x86); // BG palette 0, color 3, auto-increment
        write(emu, 0xFF69, 0x20);
        write(emu, 0xFF69, 0x03);
        write(emu, 0xFF47, 0x00);
        let row = line5(emu);
        for (x, &p) in row.iter().enumerate() {
            let want = if (x / 8) % 2 == 0 { 0x0320 } else { 0x7FFF };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// DMG compatibility mode: BCPD writes are ignored, BGP picks the
    /// compatibility colors as before.
    #[test]
    fn dmg_compat_bg_colors_ignore_palette_ram() {
        let emu = striped_bg();
        write(emu, 0xFF68, 0x80);
        for _ in 0..8 {
            write(emu, 0xFF69, 0x1F);
        }
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        let row = line5(emu);
        for (x, &p) in row.iter().enumerate() {
            // Post-boot BGP = FC: color 3 -> shade 3, color 0 -> shade 0.
            let want = if (x / 8) % 2 == 0 { gray[3] } else { gray[0] };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// A WX write in mode 3 that brings the window on screen starts it if
    /// it lands (one dot after the access, `GB_CONFLICT_WRITE_CPU`) by the
    /// window stall's dot (`ppu::WX_ON_LEAD`), as one that moves it off
    /// screen removes it (gambatte `window/late_wx_ff_0f_{1,2}`, CGB04C).
    #[test]
    fn wx_write_starts_the_window_up_to_its_stall() {
        // WY = 0, LCDC.5 set, WX FF -> 0F on line 5: the window (map 9800
        // column 0, dark) at x = 8 over the BG's column 1 (light); its
        // stall would begin at dot 104.
        let run = |t: u64| {
            let emu = striped_bg();
            let Emulator { mem, hw } = &mut *emu;
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            ctx.write(0xFF4B, 0xFF);
            ctx.write(0xFF4A, 0);
            ctx.write(0xFF40, 0xB1);
            drop(ctx);
            write_in_mode3(emu, 5, t, 0xFF4B, 0x0F)
        };
        let row = run(103);
        assert_eq!(row[8], row[0], "lands at 104: the window at x = 8");
        let row = run(107);
        assert_ne!(row[8], row[0], "lands at 108: too late");
    }

    /// A SCX write in mode 3 (landing at the access, `GB_CONFLICT_READ_OLD`)
    /// moves the BG from the first tile the fetcher has not read the map
    /// entry of yet (`Ppu::hold_fetch`).
    #[test]
    fn mid_mode3_scx_write_splits_the_line() {
        let emu = striped_bg();
        let gray = crate::DmgPalettes::GRAYSCALE.bg;
        // The write at t = 151: pixels 0-54 are out with SCX = 0, tile 6
        // (pixels 48-55) is in the FIFO and tile 7 (56-63) was fetched with
        // SCX = 0 too.
        let row = write_in_mode3(emu, 7, 151, 0xFF43, 8);
        for (x, &p) in row.iter().enumerate() {
            let scx = if x < 64 { 0 } else { 8 };
            let want = if ((x + scx) / 8) % 2 == 0 {
                gray[3]
            } else {
                gray[0]
            };
            assert_eq!(p, want, "x = {x}");
        }
    }

    /// Double speed: an SCX write reaches the fetcher one dot before the
    /// access (SameBoy `GB_CONFLICT_SCX_DMG_AND_CGB_DOUBLE`; gambatte
    /// `scx_during_m3_ds_*`). The access at t = 145, one dot after tile 6's
    /// first pixel (x = 48) is output, lands at 144, before tile 7's map
    /// entry is read (`FETCH_SAMPLED`): tile 7 (56-63) has the new SCX.
    #[test]
    fn double_speed_scx_write_lands_a_dot_before_the_access() {
        let emu = striped_bg();
        let ly = 7;
        let line = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 456 * ly as u64;
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line + 143, &mut fb, &mut []);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        // 2-clock M-cycles (`write_ppu` keys double speed on them; the
        // switch's own timing is not under test).
        ctx.hot.set_speed(Speed::Double);
        ctx.idle();
        assert_eq!(ctx.clock(), line + 145);
        ctx.write(0xFF43, 8);
        while ctx.clock() < line + 400 {
            ctx.idle();
        }
        drop(ctx);
        let row = fb.as_chunks::<160>().0[ly];
        assert_ne!(row[47], row[48], "SCX = 0 up to tile 6");
        assert_eq!(row[56], row[48], "tile 7 with SCX = 8");
        assert_ne!(row[64], row[48], "tile 8 with SCX = 8");
    }

    /// A LYC write that makes LY = LYC with the LYC select set: in single
    /// speed the interrupt reaches IF one M-cycle after the write's
    /// (gambatte `cc + 5`, CGB04C `lycEnable/lyc_ff45_trigger_delay_2`),
    /// also when a walk point syncs the PPU in between; in double speed at
    /// once. An IF write in between doesn't cancel it.
    #[test]
    fn lyc_write_interrupt_reaches_if_one_mcycle_later_in_single_speed() {
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        // (M-cycle clocks, SCX, write at line 5 dot, IF write after it,
        // M-cycles to IF). Around the mode-3 end (a PPU deadline, 256 +
        // SCX % 8 here), with SCX 1-3 it falls inside the next M-cycle.
        let end = (240..270).flat_map(|t| (0..8).map(move |scx| (4, scx, t, false, 1)));
        let fixed = [
            (4, 0, 100, false, 1),
            (4, 0, 100, true, 1),
            (2, 0, 100, false, 0),
        ];
        for (m, scx, t, if_write, later) in fixed.into_iter().chain(end) {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line5 - 40, &mut fb, &mut []);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            let speed = if m == 2 { Speed::Double } else { Speed::Normal };
            ctx.hot.set_speed(speed);
            ctx.sys.ppu.speed_changed(speed);
            ctx.write(0xFF41, 0x40);
            ctx.write(0xFF43, scx);
            while ctx.clock() < line5 + t {
                ctx.idle();
            }
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF45, 5);
            if if_write {
                ctx.write_mapped(0xFF0F, 0);
            }
            for i in 0..3 {
                let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
                assert_eq!(stat, i >= later, "{m}, {scx}, {t}, {if_write}, {i}");
                ctx.idle();
            }
        }
    }

    /// Runs `f` right after a single-speed LYC write (its M-cycle done)
    /// that requests the STAT interrupt: line 5, LY = LYC newly true with
    /// the LYC select set, IF clear. The request is on its way to IF: not
    /// there at the end of the write's M-cycle, there one M-cycle later
    /// (`lyc_write_interrupt_reaches_if_one_mcycle_later_in_single_speed`).
    fn after_requesting_lyc_write(f: impl FnOnce(&mut Ctx)) {
        let line5 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 5 * 456;
        let emu = load(rom(0x8000, 0, 0, 0x80));
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let _ = emu.run_until(line5 - 40, &mut fb, &mut []);
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write(0xFF41, 0x40);
        while ctx.clock() < line5 + 100 {
            ctx.idle();
        }
        ctx.sys.iflags = 0;
        crate::cpu::Bus::write(&mut ctx, 0xFF45, 5);
        assert_eq!(ctx.sys.iflags & crate::ppu::INT_STAT, 0);
        f(&mut ctx);
    }

    /// Review A2-04 (1): a PPU register write in the M-cycle after the LYC
    /// write (`PUSH`, `CALL` or a dispatch with SP = FF46 writes FF45, then
    /// FF44) reschedules the PPU; the request still reaches IF on time
    /// (gambatte, behaviour only: `memevent_oneshot_statirq` is an event
    /// of its own, at `cc + 5` whatever is written next).
    #[test]
    fn lyc_write_request_survives_a_ppu_write_in_the_next_mcycle() {
        for reg in [0xFF44, 0xFF43, 0xFF4B] {
            after_requesting_lyc_write(|ctx| {
                crate::cpu::Bus::write(ctx, reg, 0);
                let stat = ctx.sys.iflags & crate::ppu::INT_STAT;
                assert_eq!(stat, crate::ppu::INT_STAT, "then {reg:04X}");
            });
        }
    }

    /// Review A2-04 (2): `Bus::set_interrupt_flags` (the trait's IF setter)
    /// while the request is on its way doesn't drop it: IF is written,
    /// the request is not in IF yet.
    #[test]
    fn lyc_write_request_survives_the_if_setter() {
        after_requesting_lyc_write(|ctx| {
            Bus::set_interrupt_flags(ctx, 0);
            ctx.idle();
            let stat = ctx.sys.iflags & crate::ppu::INT_STAT;
            assert_eq!(stat, crate::ppu::INT_STAT);
        });
    }

    /// Review A2-04 (3): IF holds only bits 0-4 while a request is on its
    /// way, so `IE & IF` is what is pending without a mask (IE has all 8
    /// bits), and FF0F reads E0 | IF.
    #[test]
    fn lyc_write_request_is_not_in_if_before_its_clock() {
        after_requesting_lyc_write(|ctx| {
            ctx.sys.ie = 0xFF;
            assert_eq!(ctx.sys.iflags, 0);
            assert_eq!(Bus::interrupt_enable(ctx) & Bus::interrupt_flags(ctx), 0);
            ctx.idle();
            assert_eq!(Bus::interrupt_flags(ctx), crate::ppu::INT_STAT);
            assert_eq!(Bus::read(ctx, 0xFF0F), 0xE0 | crate::ppu::INT_STAT);
        });
    }

    /// Review A2-04 (4): turning the LCD off while the request is on its
    /// way cancels it (gambatte, behaviour only: `LCD::lcdcChange`
    /// disables every LCD event when the LCD goes off,
    /// `memevent_oneshot_statirq` included); it doesn't come back when the
    /// LCD is turned on again.
    #[test]
    fn lcd_off_cancels_a_lyc_write_request() {
        after_requesting_lyc_write(|ctx| {
            crate::cpu::Bus::write(ctx, 0xFF40, 0x11);
            for _ in 0..8 {
                ctx.idle();
                assert_eq!(ctx.sys.iflags & crate::ppu::INT_STAT, 0, "LCD off");
            }
            crate::cpu::Bus::write(ctx, 0xFF40, 0x91);
            // Lines 0-3 of the new frame (LYC = 5): no STAT interrupt.
            for _ in 0..4 * 114 {
                ctx.idle();
                assert_eq!(ctx.sys.iflags & crate::ppu::INT_STAT, 0, "LCD on again");
            }
        });
    }

    /// Single speed, line 0's mode-2 pulse (walk point t = 4): its
    /// interrupt request reaches IF at t = 5 however the PPU was synced
    /// before, also by a sync on the point itself, which leaves the PPU
    /// deadline on the request's clock (gambatte's event is at our t = 5;
    /// CGB04C `lcd_offset/offset3_lyc99int_m2irq_count_1`: an IF read at
    /// 0:4 doesn't see it).
    #[test]
    fn line_0_pulse_request_reaches_if_a_dot_after_its_point_however_synced() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let line0 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 154 * 456;
        // Dots of line 0 the PPU is synced to before dot 5.
        for syncs in [&[][..], &[4], &[3, 4], &[1, 2, 3, 4]] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line0 - 500, &mut fb, &mut []);
            write(emu, 0xFF45, 0xFF);
            write(emu, 0xFF41, 0x20);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            while ctx.clock() < line0 - 1 {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), line0 - 1);
            ctx.sys.iflags = 0;
            for &t in syncs {
                ctx.sync_ppu_to(line0 + t);
                assert_eq!(ctx.sys.iflags & STAT, 0, "{syncs:?}: synced to dot {t}");
            }
            if syncs.last() == Some(&4) {
                assert_eq!(ctx.sys.sched.deadline(EventKind::Ppu), line0 + 5);
            }
            ctx.sync_ppu_to(line0 + 5);
            assert_eq!(ctx.sys.iflags & STAT, STAT, "{syncs:?}");
        }
    }

    /// Single speed, line 153's comparison against 0 (walk point t = 12):
    /// its interrupt request reaches IF at t = 13 however the PPU was
    /// synced before, also by a sync on the point itself, which leaves the
    /// PPU deadline on the request's clock (gambatte's LYC = 0 event is at
    /// our t = 13, `edges::L153_ZERO_IRQ`). An IF read or interrupt check
    /// at an M-cycle boundary on 153:12 so doesn't see it, and a dispatch
    /// that acknowledges another interrupt there leaves it to come.
    #[test]
    fn line_153_lyc_0_request_reaches_if_a_dot_after_its_point_however_synced() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let line153 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 153 * 456;
        // Dots of line 153 the PPU is synced to before dot 13.
        for syncs in [&[][..], &[12], &[11, 12], &[9, 10, 11, 12]] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line153 - 500, &mut fb, &mut []);
            write(emu, 0xFF45, 0);
            write(emu, 0xFF41, 0x40);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            while ctx.clock() < line153 - 1 {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), line153 - 1);
            ctx.sys.iflags = 0;
            for &t in syncs {
                ctx.sync_ppu_to(line153 + t);
                assert_eq!(ctx.sys.iflags & STAT, 0, "{syncs:?}: synced to dot {t}");
            }
            if syncs.last() == Some(&12) {
                assert!(ctx.sys.ppu.stat_line_high());
                assert_eq!(ctx.sys.sched.deadline(EventKind::Ppu), line153 + 13);
            }
            ctx.sync_ppu_to(line153 + 13);
            assert_eq!(ctx.sys.iflags & STAT, STAT, "{syncs:?}");
        }
    }

    /// Single speed, a STAT write whose phase 2 lands on line 0's mode-2
    /// pulse (access at t = 3) sees the mode-1 source still holding the
    /// line: `10 -> 50` with LYC = 0 requests nothing (CGB04C
    /// `miscmstatirq/lycstatwirq_trigger_ly00_10_50_1`); accessed at
    /// t = 7 (`_2`) it does. `10 -> 20` at t = 3: the pulse is past for
    /// the new mode-2 select (`m2enable/late_enable_m1disable_ly0_2`).
    #[test]
    fn stat_write_on_the_line_0_pulse_sees_mode_1() {
        let line0 = u64::from(FRAME_CLOCKS - crate::ppu::POST_BOOT_POS) + 154 * 456;
        for (value, t, irq) in [(0x50, 3, false), (0x50, 7, true), (0x20, 3, false)] {
            let emu = load(rom(0x8000, 0, 0, 0x80));
            let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
            let _ = emu.run_until(line0 - 200, &mut fb, &mut []);
            write(emu, 0xFF45, 0);
            write(emu, 0xFF41, 0x10);
            let Emulator { mem, hw } = &mut *emu;
            let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
            while ctx.clock() < line0 + t {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), line0 + t);
            ctx.sys.iflags = 0;
            crate::cpu::Bus::write(&mut ctx, 0xFF41, value);
            ctx.idle();
            let stat = ctx.sys.iflags & crate::ppu::INT_STAT != 0;
            assert_eq!(stat, irq, "{value:02X} at 0:{t}");
        }
    }

    /// CGB, single speed: a STAT write that switches the selected source
    /// to LY=LYC while both are active keeps the line high, so it requests
    /// no interrupt, although the LYC select lands one dot after the
    /// others (gambatte `miscmstatirq/lycstatwirq_trigger_m0_*_08_40`,
    /// CGB `E0`). The same write while mode 3 has the line low does.
    #[test]
    fn stat_write_switching_to_lyc_select_keeps_the_line_high() {
        // Frame position = clock - E + 7: line 2, t = 301 (mode 0) and
        // t = 101 (mode 3).
        for (m, want) in [
            ((2 * 456 + 301 - 7) / 4, 0),
            ((2 * 456 + 101 - 7) / 4, crate::ppu::INT_STAT),
        ] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF45, 2);
                ctx.write(0xFF40, 0x91);
                idle_to(ctx, e, m);
                ctx.write(0xFF41, 0x08);
                ctx.sys.iflags = 0;
                ctx.write(0xFF41, 0x40);
                ctx.idle();
                assert_eq!(ctx.sys.iflags & crate::ppu::INT_STAT, want, "access at {m}");
            });
        }
    }

    /// CGB, single speed: [`stat_irq_after_write_with`] with LYC = FF.
    fn stat_irq_after_write(sel: u8, pos: u64, value: u8, n: u64) -> u8 {
        stat_irq_after_write_with(0xFF, sel, pos, value, n, false)
    }

    /// CGB: with LYC = `lyc` and the LCD on (frame position = clock - E +
    /// 3), selects `sel`, then (in double speed: 2-clock M-cycles from
    /// here) a STAT write of `value` with its access at frame position
    /// `pos`; returns IF's STAT bit from the write to `n` M-cycles after
    /// it.
    fn stat_irq_after_write_with(
        lyc: u8,
        sel: u8,
        pos: u64,
        value: u8,
        n: u64,
        double: bool,
    ) -> u8 {
        irq_after_write(0, lyc, sel, pos, 0xFF41, value, n, double)
    }

    /// [`stat_irq_after_write_with`] with SCX = `scx` and the write to
    /// `reg` (STAT or LYC).
    #[allow(clippy::too_many_arguments)]
    fn irq_after_write(
        scx: u8,
        lyc: u8,
        sel: u8,
        pos: u64,
        reg: u16,
        value: u8,
        n: u64,
        double: bool,
    ) -> u8 {
        irq_after_write_on(scx, lyc, sel, pos, reg, value, n, double, 0)
    }

    /// [`irq_after_write`] with the CPU's M-cycle grid moved `skip` clocks
    /// against the frame (as a speed switch can; gambatte's `lcdoffset`
    /// ROMs): in double speed the default grid's accesses are at odd line
    /// positions, with `skip` = 1 at even ones.
    #[allow(clippy::too_many_arguments)]
    fn irq_after_write_on(
        scx: u8,
        lyc: u8,
        sel: u8,
        pos: u64,
        reg: u16,
        value: u8,
        n: u64,
        double: bool,
        skip: u64,
    ) -> u8 {
        let emu = setup();
        let mut stat = 0;
        with_ctx(emu, |ctx| {
            if scx != 0 {
                ctx.write(0xFF43, scx);
            }
            let e = ctx.clock();
            ctx.write(0xFF45, lyc);
            ctx.write(0xFF40, 0x91);
            ctx.write(0xFF41, sel);
            // The switch's own timing is not under test.
            ctx.hot
                .set_speed(if double { Speed::Double } else { Speed::Normal });
            if skip != 0 {
                ctx.skip_clocks(skip);
            }
            while ctx.clock() < e + pos - 3 {
                ctx.idle();
            }
            assert_eq!(ctx.clock(), e + pos - 3);
            ctx.sys.iflags = 0;
            ctx.write(reg, value);
            while ctx.clock() < e + pos - 3 + n * ctx.hot.mcycle() {
                ctx.idle();
            }
            stat = ctx.sys.iflags & crate::ppu::INT_STAT;
        });
        stat
    }

    /// Line 143's mode 0 ends for STAT writes at line 144 dot 3, and a
    /// write from there to VBlank (dot 5) no longer changes the selects
    /// that block the mode-1 interrupt (gambatte `m1/ly143_late_m0enable`,
    /// `m1irq_m0disable` `_1`/`_2`, CGB04C; `edges::MODE_HOLD_SS`).
    #[test]
    fn line_144_stat_writes_from_dot_3_keep_the_mode_part() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let (l143_455, l144_3) = (143 * 456 + 455, 144 * 456 + 3);
        // Enabling mode 0: on time, then too late.
        assert_eq!(stat_irq_after_write(0, l143_455, 0x08, 1), STAT);
        assert_eq!(stat_irq_after_write(0, l144_3, 0x08, 3), 0);
        // Clearing mode 0 with mode 1 selected: the mode-1 interrupt
        // follows, then stays blocked by the old select.
        assert_eq!(stat_irq_after_write(0x18, l143_455, 0x10, 3), STAT);
        assert_eq!(stat_irq_after_write(0x18, l144_3, 0x10, 3), 0);
        // Line 0 dot 3: a mode-1 enable is too late (`m1irq_late_enable`).
        let (l153_455, l0_3) = (153 * 456 + 455, 154 * 456 + 3);
        assert_eq!(stat_irq_after_write(0x40, l153_455, 0x10, 1), STAT);
        assert_eq!(stat_irq_after_write(0x40, l0_3, 0x10, 1), 0);
    }

    /// Line 144's mode-2 quirk interrupt is at dot 3, once, and with mode
    /// 2 selected the mode-1 interrupt at VBlank is blocked (gambatte
    /// `m1/m2m1irq_ifw_2`, `ly143_late_m2enable_2`, CGB04C;
    /// `edges::QUIRK2`).
    #[test]
    fn line_144_mode_2_quirk_once_at_dot_3() {
        const STAT: u8 = crate::ppu::INT_STAT;
        // Writes of the same selects: the line doesn't change. From line
        // 143 dot 455: the quirk at dot 3.
        assert_eq!(stat_irq_after_write(0x30, 143 * 456 + 455, 0x30, 2), STAT);
        // From dot 3, with the quirk already in: nothing at VBlank.
        assert_eq!(stat_irq_after_write(0x30, 144 * 456 + 3, 0x30, 3), 0);
        // Mode 2 enabled at dot 3 is too late for the quirk, and VBlank
        // doesn't repeat it.
        assert_eq!(stat_irq_after_write(0, 144 * 456 + 3, 0x20, 3), 0);
    }

    /// Single speed: a STAT write's first phase at the comparison-off dot
    /// (t = 3) sees LY=LYC off, so switching from LYC to mode 1 there
    /// requests the interrupt; a dot before it the LYC source still holds
    /// the line (gambatte `m1/m1irq_enable_after_lyc144_{1,2}`, CGB04C).
    #[test]
    fn stat_write_at_the_comparison_off_dot_sees_ly_lyc_off() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(144, 0x40, pos, 0x10, 2, false);
        assert_eq!(w(144 * 456 + 455), 0);
        assert_eq!(w(145 * 456 + 3), STAT);
    }

    /// Double speed: a STAT write's second phase, half a dot after the
    /// first, sees the comparison of that half dot: at line 144 t = 3.5
    /// LY=LYC (143) still holds the line, so switching LYC for mode 0
    /// keeps it high (gambatte `m1/lyc143_late_m0enable_lycdisable_ds_1`,
    /// CGB04C: no STAT interrupt).
    #[test]
    fn double_speed_stat_phase_2_sees_the_comparison_half_a_dot_in() {
        assert_eq!(
            stat_irq_after_write_with(143, 0x40, 144 * 456 + 3, 0x08, 2, true),
            0
        );
    }

    /// Single speed: before `t = 3` (the mode-2 interrupt) a STAT write
    /// sees the previous line's mode 0, not mode 2: dropping the LYC
    /// select at line 2 dot 0 with only mode 2 left lowers the line, and
    /// line 2's mode-2 interrupt follows (gambatte
    /// `m2enable/lyc1_m2irq_late_lycdisable_1`, CGB04C).
    #[test]
    fn stat_write_before_dot_3_sees_mode_0() {
        const STAT: u8 = crate::ppu::INT_STAT;
        assert_eq!(
            stat_irq_after_write_with(1, 0x60, 456 + 455, 0x20, 2, false),
            STAT
        );
    }

    /// Double speed, line 0: with mode 2 selected the mode-1 source lasts
    /// to the mode-2 pulse (t = 5), also for a write landing on the dot
    /// before it (t = 4: a STAT write's second phase from an access at
    /// t = 3 on the default grid, its first phase or a LYC write at t = 4
    /// on the grid of the `lcdoffset1` ROMs). So a write there that keeps
    /// modes 1 and 2 selected leaves the line up, and the pulse requests
    /// nothing (SameBoy `display.c`: mode 1 until the pulse; gambatte
    /// `mstat_irq.h` `doM2Event`, behaviour only: blocked by the mode-1
    /// select). With mode 2 not selected the source ends at t = 4
    /// (`lycEnable/lycwirq_trigger_ly00_stat50_ds_lcdoffset1_2`, CGB04C).
    #[test]
    fn double_speed_line_0_mode_1_holds_to_the_pulse_for_writes() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let l0 = 154 * 456;
        // The access at line 0 dot `t` (even: the grid moved one clock).
        let w = |lyc, sel, t: u64, reg, value| {
            irq_after_write_on(0, lyc, sel, l0 + t, reg, value, 4, true, (t + 1) % 2)
        };
        for t in 1..8 {
            // The same selects, the LYC select added (no match), mode 2
            // added to mode 1 before the pulse, and a LYC write.
            assert_eq!(w(0xFF, 0x30, t, 0xFF41, 0x30), 0, "30 -> 30 at {t}");
            assert_eq!(w(0xFF, 0x30, t, 0xFF41, 0x70), 0, "30 -> 70 at {t}");
            assert_eq!(w(0xFF, 0x30, t, 0xFF45, 0xFE), 0, "LYC at {t}");
            if t <= 3 {
                assert_eq!(w(0xFF, 0x10, t, 0xFF41, 0x30), 0, "10 -> 30 at {t}");
            }
        }
        // Mode 1 cleared before the pulse: the pulse's interrupt
        // (`m2enable/m2_late_m1disable_ly0_ds_1`, the access at t = 3);
        // after it, none (`_ds_2`, t = 5).
        assert_eq!(w(0xFF, 0x30, 3, 0xFF41, 0x20), STAT);
        assert_eq!(w(0xFF, 0x30, 5, 0xFF41, 0x20), 0);
        // Mode 2 not selected: the mode-1 source is over at t = 4, and a
        // LYC = 0 match selected there raises the line.
        assert_eq!(w(0, 0x10, 3, 0xFF41, 0x50), 0);
        assert_eq!(w(0, 0x10, 4, 0xFF41, 0x50), STAT);
    }

    /// Double speed: a STAT write's first phase enabling mode 2 at t = 5
    /// still requests the mode-2 interrupt, at t = 7 not (gambatte
    /// `m2enable/late_enable_ds_{1,2}`, CGB04C); on line 0 the same
    /// (`late_enable_ly0_ds_{1,2}`).
    #[test]
    fn double_speed_mode_2_enable_window() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(0xFF, 0, pos, 0x20, 1, true);
        assert_eq!(w(136 * 456 + 5), STAT);
        assert_eq!(w(136 * 456 + 7), 0);
        assert_eq!(w(154 * 456 + 5), STAT);
        assert_eq!(w(154 * 456 + 7), 0);
    }

    /// STAT writes just before the mode-0 interrupt (line 2, SCX 0: at
    /// t = 257; SCX 1: 258) no longer change the selects it sees: within
    /// 2 dots of the access in single speed, 1 in double speed
    /// (`edges::M0_STAT_LATCH`; gambatte `m0enable/disable_*`,
    /// `lycdisable_ff41_scx1_ds_*`, CGB04C).
    #[test]
    fn stat_writes_just_before_the_mode_0_interrupt_are_too_late() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let at = 2 * 456 + 255;
        // Single speed, clearing mode 0: 2 dots before, the interrupt
        // still comes; 3 dots before, not.
        let w = |scx| irq_after_write(scx, 0xFF, 0x08, at, 0xFF41, 0, 3, false);
        assert_eq!(w(0), STAT);
        assert_eq!(w(1), 0);
        // Clearing the LYC select while LY=LYC (2): 2 dots before, the
        // interrupt stays blocked; 3 dots before, unblocked.
        let w = |scx, at, double| irq_after_write(scx, 2, 0x48, at, 0xFF41, 0x08, 3, double);
        assert_eq!(w(0, at, false), 0);
        assert_eq!(w(1, at, false), STAT);
        // Double speed: 1 dot before, blocked; 2 dots before, not.
        assert_eq!(w(1, at + 2, true), 0);
        assert_eq!(w(0, at, true), STAT);
    }

    /// A LYC write ending LY=LYC just before the mode-0 interrupt (line 2)
    /// doesn't unblock it: within 6 dots of the access in single speed, 2
    /// in double speed (`edges::M0_LYC_LATCH`; gambatte
    /// `m0enable/lycdisable_ff45_*`, CGB04C).
    #[test]
    fn lyc_writes_just_before_the_mode_0_interrupt_still_block_it() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |scx, at, double| irq_after_write(scx, 2, 0x48, at, 0xFF45, 0xFF, 4, double);
        // Single speed: 6 dots before (SCX 0), 7 (SCX 1).
        assert_eq!(w(0, 2 * 456 + 251, false), 0);
        assert_eq!(w(1, 2 * 456 + 251, false), STAT);
        // Double speed: 2 dots before (SCX 0), 3 (SCX 1).
        assert_eq!(w(0, 2 * 456 + 255, true), 0);
        assert_eq!(w(1, 2 * 456 + 255, true), STAT);
    }

    /// Single speed: clearing the mode-1 select 1-2 dots before line
    /// 153's LY=LYC comparison doesn't unblock the LYC=153 interrupt
    /// (gambatte `lycEnable/lyc153_late_m1disable_{1,2}`, CGB04C).
    #[test]
    fn mode_1_clear_just_before_the_line_153_comparison_still_blocks() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let w = |pos| stat_irq_after_write_with(153, 0x50, pos, 0x40, 3, false);
        assert_eq!(w(152 * 456 + 455), STAT);
        assert_eq!(w(153 * 456 + 3), 0);
    }

    /// Single speed, line 153: a LYC write is compared with 153 up to dot
    /// 7, the held LY=LYC (152) dropped from dot 4 (view; access at 3), and
    /// not from dot 12 (gambatte `lycEnable/lyc153_late_ff45_enable_{3,5}`,
    /// CGB04C).
    #[test]
    fn line_153_lyc_write_windows() {
        const STAT: u8 = crate::ppu::INT_STAT;
        for (lyc, pos, want) in [(152, 153 * 456 + 3, STAT), (0xFF, 153 * 456 + 11, 0)] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF45, lyc);
                ctx.write(0xFF40, 0x91);
                ctx.write(0xFF41, 0x40);
                idle_to(ctx, e, (pos - 3) / 4);
                ctx.sys.iflags = 0;
                ctx.write(0xFF45, 153);
                ctx.idle();
                assert_eq!(ctx.sys.iflags & STAT, want, "access at {pos}");
            });
        }
    }

    /// LCD off with the coincidence bit kept set: a STAT write newly
    /// selecting LY=LYC requests the interrupt, one keeping it selected
    /// doesn't (gambatte `memory.cpp` FF41; `lycEnable/lcdoff_lycirqen_1`).
    #[test]
    fn lcd_off_lyc_select_with_the_kept_coincidence_bit() {
        const STAT: u8 = crate::ppu::INT_STAT;
        let emu = setup();
        with_ctx(emu, |ctx| {
            ctx.write(0xFF45, 0);
            ctx.write(0xFF40, 0x91);
            ctx.idle();
            ctx.write(0xFF40, 0x11);
            ctx.sys.iflags = 0;
            ctx.write(0xFF41, 0x40);
            assert_eq!(ctx.sys.iflags & STAT, STAT);
            ctx.sys.iflags = 0;
            ctx.write(0xFF41, 0x40);
            assert_eq!(ctx.sys.iflags & STAT, 0);
        });
    }
}
