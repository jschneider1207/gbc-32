//! CGB VRAM DMA (FF51-FF55) as the bus drives it: arming, the HBlank
//! trigger, the CPU stall and the block copy (`hdma` module; ADR 0017).

use super::{Ctx, Sys};
use crate::dma;
use crate::hdma::{self, Refetch};
use crate::memory::Memory;
use crate::schedule::{EventKind, NEVER};

// Names only the doc comments use.
#[cfg(doc)]
use {
    super::{read, wram},
    crate::{Emulator, ppu::Ppu},
};

/// CGB VRAM DMA (FF51-FF55; `hdma` module, ADR 0017).
impl Ctx<'_, '_> {
    /// Runs the CPU stall of the current transfer step (`VramDma::stall`
    /// idle M-cycles left), skipped a deadline at a time (ADR 0011), each
    /// deadline handled on the M-cycle it was before. Stops early at the
    /// first deadline that raises a stop event or reaches `limit` (the
    /// call's target; `u64::MAX` for a single step): the run call ends
    /// there and the next one resumes the stall. Without this, a GDMA
    /// stall (up to 4,100 clocks) crossing the `AudioBuffer` deadline
    /// would complete windows with no slot left, dropping them (ADR 0021,
    /// GDMA stall amendment).
    #[inline(never)]
    pub(crate) fn vdma_stall(&mut self, limit: u64) {
        let mut n = u32::from(self.sys.vdma.stall);
        while n != 0 {
            #[cfg(feature = "eager-sync")]
            self.eager_sync();
            let k = self.mcycles_to_deadline().min(n);
            self.hot.t_minus += k as i32;
            n -= k;
            if self.hot.t_minus >= 0 {
                // Left after this deadline's M-cycle, for `vdma_hblank`:
                // an HBlank trigger before the stall's end is lost.
                self.sys.vdma.stall = n as u16;
                self.boundary();
                if self.events.intersects(self.stop_on) || self.clock() >= limit {
                    break;
                }
            }
        }
        self.sys.vdma.stall = n as u16;
    }

    /// HDMA's HBlank period ([`Ppu::hdma_period`]) at clock `at`: from this
    /// line's trigger ([`hdma::HBLANK_DELAY`] after the mode-0 STAT change)
    /// to [`hdma::HBLANK_UNTIL`] dots into the next line. Also with the
    /// LCD off (a transfer started then runs at once).
    pub(super) fn hdma_period(&self, at: u64) -> bool {
        let p = &self.sys.ppu;
        !p.lcd_enabled() || p.hdma_period(at, hdma::HBLANK_DELAY, hdma::HBLANK_UNTIL)
    }

    /// A transfer step is due: the CPU stops after this instruction so
    /// the run loop can run it ([`Emulator::run`]).
    fn vdma_request(&mut self) {
        self.sys.vdma.pending = true;
        self.end_run();
    }

    /// FF55 write. An HBlank transfer that starts in HDMA's period as it
    /// is one M-cycle later (gambatte `enableHdma`: `isHdmaPeriod(cc +
    /// 4)`) does its first block now; otherwise it waits for the next
    /// trigger, this line's if still ahead (`dma/hdma_late_enable_*`: an
    /// enable at the HBlank's end starts none).
    // Out of line: measured with the other bus helpers as one batch, not
    // alone (docs/perf.md, "Small-state and attribute pass", A14).
    #[inline(never)]
    pub(super) fn write_hdma5(&mut self, value: u8) {
        let now = self.clock();
        if value & 0x80 != 0 {
            // Latch the current line, if due, before the transfer is
            // active.
            self.run_ppu_to_call(now);
            self.schedule_ppu();
        }
        let mode0 = self.hdma_period(now + self.hot.mcycle());
        self.sys.vdma.write_hdma5(value, mode0);
        if self.sys.vdma.pending {
            self.end_run();
        }
        if self.sys.vdma.hblank {
            self.vdma_arm(now, false);
        }
    }

    /// Schedules the HBlank trigger of the PPU's current visible line, once
    /// per line. `fire_late`: a trigger at or before `now` fires at the
    /// next boundary (the PPU sync that latched the line ran late by less
    /// than an M-cycle); otherwise it is treated as passed.
    ///
    /// Every PPU sync during an HBlank transfer comes here (about 3.4 per
    /// line), and all but the first of a line find it armed: that test is
    /// inline.
    #[inline(always)]
    pub(super) fn vdma_arm(&mut self, now: u64, fire_late: bool) {
        let s = &*self.sys;
        match s.ppu.mode0_start() {
            Some(start) if start != s.vdma.line => self.vdma_arm_at(start, now, fire_late),
            _ => {}
        }
    }

    /// [`Self::vdma_arm`] for a line whose mode 0 starts at `start`, not
    /// armed yet.
    #[inline(never)]
    fn vdma_arm_at(&mut self, start: u64, now: u64, fire_late: bool) {
        let s = &mut *self.sys;
        s.vdma.line = start;
        let at = start + hdma::HBLANK_DELAY;
        if at > now || fire_late {
            s.sched.set(EventKind::VramDma, at);
            // `boundary` reschedules when called from an event handler.
            self.reschedule();
        }
    }

    /// The `VramDma` deadline: an HBlank starts a block, unless the CPU
    /// is halted (SameBoy: `!gb->halted`), the transfer was stopped, or a
    /// block's stall is still running: gambatte acknowledges the request
    /// at a transfer's end, so a trigger during it is lost (CGB C captures
    /// `dma/hdma_transition_{,ei_}halt_late_unhalt_scx1_{1,2}`).
    pub(super) fn vdma_hblank(&mut self) {
        let v = &mut self.sys.vdma;
        if v.refetch == Refetch::Ran {
            v.refetch = Refetch::None;
            if self.hot.clock() < v.stall_end {
                return;
            }
        }
        if v.hblank && !v.halted && v.stall == 0 && self.sys.ppu.lcd_enabled() {
            self.vdma_request();
        }
    }

    /// HALT begins during an HBlank transfer: HBlanks start no blocks
    /// until the wake-up, and the wake-up may start one (`wake_ok`) if the
    /// HALT began outside the HBlank (SameBoy `allow_hdma_on_wake`: STAT
    /// mode at the HALT). The HBlank here is HDMA's period
    /// ([`Self::hdma_period`]), which starts at this line's trigger, not
    /// at the mode-0 STAT change: a HALT between the two counts as before
    /// it (gambatte `dma/hdma_late_m3halt_m0unhalt_scx{1_3,2_2}`, CGB C
    /// captures; `hdma_late_m0halt_*` bound the other side).
    #[cold]
    pub(super) fn vdma_halt(&mut self) {
        let in_hblank = self.hdma_period(self.clock());
        let v = &mut self.sys.vdma;
        v.halted = true;
        v.wake_ok = !in_hblank;
        // A block requested in HALT's M-cycle (gambatte `Memory::halt`).
        if v.pending {
            v.refetch = Refetch::Requested;
        }
    }

    /// The speed-switch halt begins during an HBlank transfer (after the
    /// speed change: `mcycle` is the new speed's). As [`Self::vdma_halt`],
    /// and gambatte samples HDMA for a STOP at the start of its last
    /// M-cycle (`Memory::stop(cc - 4)`), so a block due then was triggered
    /// in STOP's first M-cycle (ADR 0017, speed-switch amendment; CGB C
    /// captures `dma/hdma_late_m3speedchange_*_2`,
    /// `hdma_transition_speedchange_*`):
    /// - Triggered in the last M-cycle: it falls into the halt, which
    ///   loses it (`hdma_late_m3speedchange_{ly_scx1_1,tima_scx1_ds_2}`).
    /// - Triggered in the first: STOP's second byte runs as the next
    ///   opcode (`refetch`; `_inc_` ROMs). To double speed the block runs
    ///   in the halt, which ends the transfer with FF55 bit 7 set and its
    ///   length unchanged (gambatte `Memory::dma` while `halted()`;
    ///   `_hdma5_`, `_hdmadst10_`, `read_hdmadst00`); to single speed it
    ///   runs at the wake-up, before that opcode (`ldaaimm_scx1_ds`).
    #[cold]
    #[inline(never)]
    pub(super) fn vdma_stop(&mut self) {
        // The new speed (called after the switch); the margin below is
        // the other speed's M-cycle length.
        let speed = self.hot.speed();
        let trigger = self.sys.vdma.line.wrapping_add(hdma::HBLANK_DELAY);
        if trigger + speed.toggled().mcycle() > self.clock() {
            self.sys.vdma.pending = false;
        } else if self.sys.vdma.pending {
            if speed.is_double() {
                let blocks = self.sys.vdma.blocks;
                self.vram_dma(false);
                let v = &mut self.sys.vdma;
                v.stall = 0;
                v.blocks = blocks;
                v.hblank = false;
            }
            self.sys.vdma.refetch = Refetch::Requested;
        }
        self.vdma_halt();
    }

    /// The wake-up from a HALT that began outside the HBlank: in HDMA's
    /// period it starts a block (SameBoy `GB_cpu_run`,
    /// `allow_hdma_on_wake`; gambatte `isHdmaPeriod` at the unhalt).
    #[cold]
    pub(super) fn vdma_wake(&mut self) {
        self.sys.vdma.wake_ok = false;
        if self.sys.vdma.hblank && self.hdma_period(self.clock()) {
            self.vdma_request();
        }
    }

    /// Runs the pending transfer step at an instruction fetch: all blocks
    /// of a GDMA, or one HDMA block, and sets the CPU stall for it
    /// ([`hdma::stall_mcycles`], run by [`Self::vdma_stall`]). The bytes
    /// are copied at once at the start, after the PPU has drawn everything
    /// before it.
    ///
    /// Sources: ROM, cartridge RAM and WRAM are read; VRAM and E000-FFFF
    /// read FF (SameBoy reads the CPU data bus there, which this core does
    /// not model).
    ///
    /// `running`: the CPU is not in the speed-switch halt, so an OAM DMA
    /// transfer runs alongside ([`Self::oam_dma_during_vdma`]).
    #[inline(never)]
    pub(crate) fn vram_dma(&mut self, running: bool) {
        let now = self.clock();
        self.run_ppu_to_call(now);
        if self.sys.ppu.drawing() {
            self.sys.ppu.flush(now, self.mem, self.frame);
        }
        // An OAM DMA's bytes due so far read their source before the
        // VRAM DMA writes VRAM.
        if self.hot.dma != 0 {
            self.dma_sync(now);
        }
        let bank = self.sys.vram_off;
        let mut v = self.sys.vdma;
        let dest0 = v.dest;
        v.pending = false;
        let mut blocks = 0;
        loop {
            // Both counters are 16-aligned (FF52/FF54 bits 0-3 are
            // ignored, and a block adds 16), so a block is one 16-byte
            // run of VRAM: `dest & 0x1FFF` never wraps inside it.
            let block = vdma_block(self.mem, self.sys, v.src);
            let (vram, _) = self.mem.vram.as_chunks_mut::<16>();
            vram[((bank | usize::from(v.dest & 0x1FF0)) >> 4) & 0x3FF] = block;
            v.src = v.src.wrapping_add(16);
            v.dest = v.dest.wrapping_add(16);
            blocks += 1;
            v.blocks = v.blocks.wrapping_sub(1);
            if v.blocks == 0 || v.dest == 0 {
                v.hblank = false;
                break;
            }
            // One block per HBlank, also for a block whose transfer was
            // stopped after its trigger (`VramDma::single`).
            if v.hblank || v.single {
                break;
            }
        }
        v.single = false;
        // At most 1 + 128 * 16 M-cycles: fits. One less for the block a
        // HALT's or speed-switch STOP's latched opcode waited for
        // (`VramDma::refetch`).
        let full = hdma::stall_mcycles(blocks, self.hot.speed());
        v.stall = full as u16;
        v.refetch = if v.refetch == Refetch::Requested {
            v.stall -= 1;
            v.stall_end = now + u64::from(full) * self.hot.mcycle();
            Refetch::Ran
        } else {
            Refetch::None
        };
        self.sys.vdma = v;
        if self.hot.dma != 0 && running {
            self.oam_dma_during_vdma(now, dest0, blocks);
        }
    }

    /// An OAM DMA transfer whose steps fall inside a VRAM DMA that started
    /// at `now` (`blocks` blocks to VRAM from `dest0`): each such step
    /// writes the VRAM DMA's byte of that moment, not its own, to OAM at
    /// the VRAM DMA's source address & FF (if below A0), and its own byte
    /// is never written. The VRAM DMA moves 2 bytes per M-cycle at single
    /// speed (the step after M-cycle `k` takes byte `2k - 1`), 1 at double
    /// speed (byte `k - 1`); its last M-cycle (no bytes) and the steps
    /// before `now` are normal. Source: gambatte cgb04c captures
    /// `dma/hdma_transition_oamdma_1`, `oamdma/oamdmasrcC000_hdmasrc0000`
    /// (gambatte `Memory::dma` does this; read for behaviour only).
    /// SameBoy skips the step's write too but writes nothing in its place.
    /// Not modelled: what the PPU reads of OAM meanwhile.
    #[cold]
    #[inline(never)]
    fn oam_dma_during_vdma(&mut self, now: u64, dest0: u16, blocks: u32) {
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        let d = self.sys.ppu.dma;
        // Halted (paused): no steps.
        if d.paused != NEVER {
            return;
        }
        let m = d.speed.mcycle();
        let per = (self.hot.mcycle() as u32 / 2).max(1);
        let len = blocks * 16;
        let bank = self.sys.vram_off;
        let src0 = self.sys.vdma.src.wrapping_sub(len as u16);
        let mut copied = d.copied;
        for k in 1..=len / per {
            let t = now + u64::from(k) * m;
            if t < d.start {
                continue;
            }
            let i = (t - d.start) / m;
            if i >= u64::from(dma::LEN) {
                break;
            }
            let b = (k * per - 1) as u16;
            let p = usize::from(src0.wrapping_add(b) & 0xFF);
            if p < usize::from(dma::LEN) {
                let a = usize::from(dest0.wrapping_add(b) & 0x1FFF);
                self.mem.oam[p] = self.mem.vram[(bank | a) & 0x3FFF];
            }
            copied = i as u8 + 1;
        }
        self.sys.ppu.dma.copied = copied;
        self.sys.ppu.oam_written();
    }
}

/// One VRAM DMA block's 16 source bytes from `src` (16-aligned, so the
/// block never crosses a region or bank boundary): ROM, cartridge RAM and
/// WRAM are read as [`read`] reads them, VRAM and E000-FFFF read FF. A
/// region that is one run of memory (the ROM bank window, plain cartridge
/// RAM, a WRAM bank) is copied without the per-byte dispatch.
#[inline(always)]
fn vdma_block(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
    debug_assert!(src & 0xF == 0);
    match src >> 13 {
        0..=3 => sys.cart.rom_block(src),
        5 => sys.cart.ram_block(src),
        6 => *wram_block(mem, sys, usize::from(src)),
        // VRAM and E000-FFFF: FF (see `Ctx::vram_dma`).
        _ => [0xFF; 16],
    }
}

/// 16 bytes of C000-DFFF from a 16-aligned `a`, as [`wram`] reads them.
#[inline(always)]
fn wram_block<'m>(mem: &'m Memory, sys: &Sys, a: usize) -> &'m [u8; 16] {
    let off = if a & 0x1000 == 0 {
        a & 0x0FF0
    } else {
        sys.wram_off | (a & 0x0FF0)
    };
    let (blocks, _) = mem.wram.as_chunks::<16>();
    &blocks[(off >> 4) & 0x7FF]
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::Emulator;
    use crate::bus::test_util::{idle_to, load, rom, setup, with_ctx, write};
    use crate::bus::{Hot, read};
    use crate::cpu::Bus;

    /// The per-byte VRAM DMA source read before the block fast path: the
    /// reference for `vdma_block`.
    fn vdma_block_ref(mem: &Memory, sys: &Sys, src: u16) -> [u8; 16] {
        let hot = Hot::new();
        core::array::from_fn(|i| {
            let a = src.wrapping_add(i as u16);
            if a < 0x8000 || matches!(a & 0xE000, 0xA000 | 0xC000) {
                read::<false>(mem, sys, &hot, a)
            } else {
                0xFF
            }
        })
    }

    /// The VRAM DMA block fast path (`vdma_block`) reads what the per-byte
    /// path (`vdma_block_ref`) reads, for every 16-aligned source, in
    /// each source region and mapper state: ROM banks (one window ending
    /// mid-block), cartridge RAM disabled / banked / an RTC register,
    /// WRAM banks via SVBK (0 maps bank 1), VRAM and E000-FFFF.
    #[test]
    fn vdma_block_fast_path_matches_per_byte_reads() {
        // MBC3+TIMER+RAM+BATTERY, 32 KiB RAM; 0x8008 bytes, so ROM bank 2
        // is an 8-byte window.
        let r = rom(0x8008, 0x10, 0x03, 0x80);
        for (i, b) in r.iter_mut().enumerate().skip(0x150) {
            *b = (i * 7 + (i >> 8)) as u8;
        }
        let emu = load(r);
        for (i, b) in emu.mem.wram.iter_mut().enumerate() {
            *b = (i * 5 + (i >> 12) * 33) as u8;
        }
        for (i, b) in emu.mem.vram.iter_mut().enumerate() {
            *b = i as u8;
        }
        write(emu, 0x0000, 0x0A); // RAM and RTC enabled
        for i in 0..0x8000u32 {
            write(emu, 0x4000, (i >> 13) as u8);
            write(
                emu,
                0xA000 | (i & 0x1FFF) as u16,
                (i * 3 + (i >> 13) * 71) as u8,
            );
        }
        let check = |emu: &Emulator, what: &str| {
            for src in (0..=0xFFF0u16).step_by(16) {
                assert_eq!(
                    vdma_block(emu.mem, &emu.hw.sys, src),
                    vdma_block_ref(emu.mem, &emu.hw.sys, src),
                    "{what}: {src:04X}"
                );
            }
        };
        for (what, writes) in [
            (
                "ROM bank 1, RAM bank 0, SVBK 0",
                &[(0x2000, 1), (0x4000, 0), (0xFF70, 0)][..],
            ),
            (
                "ROM bank 2 (partial), RAM bank 2, SVBK 3",
                &[(0x2000, 2), (0x4000, 2), (0xFF70, 3)],
            ),
            (
                "RAM bank 3, SVBK 7",
                &[(0x2000, 1), (0x4000, 3), (0xFF70, 7)],
            ),
            ("RTC seconds", &[(0x4000, 0x08)]),
            ("RAM disabled", &[(0x4000, 1), (0x0000, 0x00)]),
        ] {
            for &(a, v) in writes {
                write(emu, a, v);
            }
            check(emu, what);
        }
        // The block path sees the WRAM bank: bank 7 at D000.
        assert_eq!(
            vdma_block(emu.mem, &emu.hw.sys, 0xD010)[0],
            emu.mem.wram[0x7010]
        );
    }

    /// An HBlank trigger while a block's CPU stall runs is lost; one at the
    /// stall's end starts the next block (gambatte acknowledges the
    /// request at a transfer's end; ADR 0017, trigger amendment).
    #[test]
    fn an_hblank_trigger_during_a_block_stall_is_lost() {
        for (at, kept) in [(8, false), (12, true)] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                ctx.write(0xFF40, 0x91);
                ctx.sys.vdma.hblank = true;
                ctx.sys.vdma.stall = 3;
                let now = ctx.clock();
                ctx.sys.sched.set(EventKind::VramDma, now + at);
                ctx.reschedule();
                ctx.vdma_stall(u64::MAX);
                assert_eq!(ctx.clock(), now + 12);
                assert_eq!(ctx.sys.vdma.stall, 0);
                assert_eq!(ctx.sys.vdma.pending, kept, "trigger at +{at}");
            });
        }
    }

    /// The lost-trigger window after a block that a HALT's latched opcode
    /// waited for (`Refetch::Ran`) ends at that block's full stall, however
    /// long the flag then stays set: nothing clears it until the next
    /// trigger or HALT, which can be more than 2^31 clocks (8.5 minutes)
    /// away (review finding A1-04: a 32-bit end made such a trigger look
    /// as if it came before the end, and its block was dropped).
    #[test]
    fn a_stale_ran_flag_loses_no_trigger_however_old() {
        for (later, kept) in [
            (0, false),
            (1 << 31 | 64, true),
            (1 << 32, true),
            (3 << 31, true),
        ] {
            let emu = setup();
            with_ctx(emu, |ctx| {
                ctx.write(0xFF26, 0x00); // APU off: no frame-sequencer events
                // As `vram_dma` leaves it after such a block (1 + 8
                // M-cycles) at this clock.
                ctx.sys.vdma.refetch = Refetch::Ran;
                ctx.sys.vdma.stall_end = ctx.clock() + 36;
                // `later` clocks pass (LCD off, nothing else scheduled).
                ctx.hot.deadline += later;
                ctx.write(0xFF40, 0x91);
                ctx.sys.vdma.hblank = true;
                let now = ctx.clock();
                ctx.sys.sched.set(EventKind::VramDma, now + 4);
                ctx.reschedule();
                ctx.idle();
                ctx.idle();
                assert_eq!(ctx.clock(), now + 8);
                assert_eq!(ctx.sys.vdma.pending, kept, "{later} clocks later");
                assert_eq!(ctx.sys.vdma.refetch, Refetch::None);
            });
        }
    }

    /// A register write in mode 3 that moves the line's mode-0 start (here
    /// SCX's fine scroll, which lengthens mode 3) moves the HBlank
    /// transfer's trigger with it: the trigger was scheduled from the
    /// line's timing as it was before the write, and nothing else syncs
    /// the PPU before it (found by `check-sync-invariance.sh` on
    /// `synth:worst-case`: the block ran up to 5 clocks before mode 0).
    #[test]
    fn a_mode_3_write_that_moves_mode_0_moves_the_hblank_trigger() {
        let mut moved = [0; 2];
        // Accesses from line 2's mode 2 to the end of its mode 3, SCX
        // going from 7 to 6 (in the discard: a shorter mode 3, or 7 dots
        // longer once the discard has counted past 6) or to 4.
        for (m, scx) in ((2 * 456 + 60) / 4..(2 * 456 + 260) / 4).flat_map(|m| [(m, 6), (m, 4)]) {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let e = ctx.clock();
                ctx.write(0xFF43, 0x07);
                ctx.write(0xFF40, 0x91);
                idle_to(ctx, e, (2 * 456 + 20) / 4);
                ctx.write(0xFF55, 0x87);
                assert!(ctx.sys.vdma.hblank && !ctx.sys.vdma.pending);
                idle_to(ctx, e, m);
                // The line's timing as the write's own sync sees it.
                ctx.run_ppu_to_call(ctx.clock());
                let before = ctx.sys.ppu.mode0_start();
                ctx.write(0xFF43, scx);
                let start = ctx.sys.ppu.mode0_start().unwrap();
                let trigger = start + hdma::HBLANK_DELAY;
                if trigger <= ctx.clock() {
                    // Line 2 isn't latched yet, or its trigger has passed.
                    return;
                }
                let before = before.unwrap();
                moved[0] += u32::from(start < before);
                moved[1] += u32::from(start > before);
                assert_eq!(
                    ctx.sys.sched.deadline(EventKind::VramDma),
                    trigger,
                    "access at {m}, SCX {scx}"
                );
                // The block is requested in the M-cycle that reaches it.
                while !ctx.sys.vdma.pending {
                    ctx.idle();
                }
                assert!(
                    (trigger..trigger + 4).contains(&ctx.clock()),
                    "access at {m}, SCX {scx}"
                );
            });
        }
        assert!(moved[0] > 0 && moved[1] > 0, "mode 0 moved {moved:?}");
    }

    /// CGB B/C read STAT mode 0 at line 0 dot 3 in single speed, but an
    /// HBlank-HDMA start in that M-cycle must not see HBlank: no block
    /// starts now (review finding; SameBoy keeps mode 1 there).
    #[test]
    fn hblank_hdma_write_at_line_0_dot_3_does_not_start_a_block() {
        // Frame position = clock - E + 7; line 0 begins at 154 * 456.
        let m = (154 * 456 + 3 - 7) / 4;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            assert_eq!(ctx.read(0xFF41) & 3, 0, "STAT read keeps the quirk");
        });
        let emu = setup();
        with_ctx(emu, |ctx| {
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, m);
            ctx.write(0xFF55, 0x80);
            assert!(!ctx.sys.vdma.pending, "no immediate block");
            assert!(ctx.sys.vdma.hblank);
        });
    }
}
