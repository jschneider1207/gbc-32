//! OAM DMA (FF46) as the bus drives it: the transfer's start, its lazy
//! copy, pauses and retiming, and the CPU's reads and writes while it
//! runs (`dma` module; `docs/reference/oam-dma.md`, ADR 0028).

use super::{Ctx, Hot, Sys, read, read_high};
use crate::dma::{self, Conflict, OamDma};
use crate::memory::Memory;
use crate::schedule::{EventKind, NEVER};
use crate::speed::Speed;

impl Ctx<'_, '_> {
    /// FF46: starts an OAM DMA transfer from `value << 8` (Pan Docs: OAM
    /// DMA Transfer). The FF46 write's M-cycle and the next one are the
    /// start delay; byte `i` is transferred in the M-cycle `2 + i` after the
    /// write's. A write while a transfer is running restarts it: the old
    /// one stops, but keeps CPU OAM reads blocked through the new start
    /// delay unless it was in its last M-cycle (SameBoy `GB_IO_DMA`,
    /// `dma_restarting`; Mooneye `oam_dma_restart`).
    ///
    /// The PPU is brought up to the write first (its OAM reads so far saw
    /// the old state), and OAM is kept as it is now for the PPU's reads of
    /// bytes not written yet (`Memory::oam_dma_old`).
    #[inline(never)]
    pub(super) fn oam_dma(&mut self, value: u8) {
        let now = self.clock();
        if self.sys.ppu.dma.tail < dma::LEN {
            self.dma_tail_end();
        }
        self.dma_ppu_event(now);
        self.mem.io[0x46] = value;
        let m = self.hot.mcycle();
        let restart = self.hot.dma != 0 && {
            self.dma_sync(now);
            now + m < self.sys.ppu.dma.end
        };
        // The old transfer still moves a byte in the next M-cycle, if it
        // is past its own start delay by then, but already on the new
        // source's bus and address (gambatte: the FF46 write sets the new
        // source at once and the old position runs on until the new
        // start; cgb04c `oamdma_src8000_srcchange0000_busyinc`).
        let old = self.sys.ppu.dma.start;
        let start = now + 2 * m;
        let end = start + u64::from(dma::LEN) * m;
        let mut d = OamDma {
            base: u16::from(value) << 8,
            start,
            block_from: if restart { now } else { start },
            end,
            speed: self.hot.speed(),
            ppu_live: true,
            ..OamDma::new()
        };
        d.set_ppu_window();
        let mut deadline = end;
        if restart && now + m >= old {
            // Until the `Dma` deadline at the real `start`
            // (`dma_tail_end`), `start` is that of a transfer at byte
            // `tail` in the next M-cycle, so the CPU's conflict path
            // (`dma_conflict`) needs no test for it; `copied` keeps the
            // lazy copy from moving bytes meanwhile.
            let tail = ((now + m - old) >> d.speed.mcycle_shift()) as u8;
            d.tail = tail;
            d.start = now + m - u64::from(tail) * m;
            d.copied = tail + 1;
            deadline = start;
        }
        self.sys.ppu.dma = d;
        self.mem.oam_dma_old = self.mem.oam;
        self.hot.dma = 0xFE00;
        self.sys.sched.set(EventKind::Dma, deadline);
        self.reschedule();
    }

    /// The `Dma` deadline at a restarted transfer's real start (or
    /// anything that needs the real state before it): undoes the restart
    /// tail's stand-in `start` (see [`Self::oam_dma`]). Cold.
    #[cold]
    #[inline(never)]
    pub(super) fn dma_tail_end(&mut self) {
        let d = &mut self.sys.ppu.dma;
        let m = d.speed.mcycle();
        d.start += (u64::from(d.tail) + 1) * m;
        d.copied = 0;
        d.tail = dma::LEN;
        self.sys.sched.set(EventKind::Dma, d.end);
    }

    /// Before the OAM DMA state changes at `now` (an FF46 write, a halt
    /// during a transfer): the PPU catches up, so its OAM reads up to now
    /// (OAM scan entries, object fetches) see the old state
    /// ([`OamDma::ppu_at`] only describes clocks after the last change).
    #[inline(never)]
    fn dma_ppu_event(&mut self, now: u64) {
        self.run_ppu_to_call(now);
        let s = &mut *self.sys;
        s.ppu.dma_event(now, self.mem, self.frame);
        self.schedule_ppu();
    }

    /// Copies the OAM DMA bytes due by the CPU access at `now` (an M-cycle
    /// boundary). Called before anything can observe OAM or change a
    /// source byte not yet copied: a CPU write during the transfer, a
    /// restart, the end of a run call and the `Dma` deadline.
    pub(super) fn dma_sync(&mut self, now: u64) {
        let d = self.sys.ppu.dma;
        // Paused while halted: no progress past the pause.
        let due = d.due(now.min(d.paused), self.hot.mcycle_shift());
        for i in d.copied..due {
            let src = d.base + u16::from(i);
            // CGB: a source at E000-FFFF reads FF (SameBoy `GB_dma_run`).
            self.mem.oam[usize::from(i)] = if src >= 0xE000 {
                0xFF
            } else {
                read::<false>(self.mem, self.sys, &self.hot.without_dma(), src)
            };
        }
        if due > d.copied {
            self.sys.ppu.oam_written();
        }
        self.sys.ppu.dma.copied = d.copied.max(due);
        // Only a transfer from VRAM records any (`dma_read_clear`).
        if d.base & 0xE000 == 0x8000 {
            self.dma_apply_read_clears();
        }
    }

    /// Clears the OAM bytes recorded by [`dma_read_clear`] (all copied by
    /// now: each was due by its read) and empties the record.
    #[cold]
    #[inline(never)]
    fn dma_apply_read_clears(&mut self) {
        if self.sys.dma_read_clears.apply(&mut self.mem.oam) {
            self.sys.ppu.oam_written();
        }
    }

    /// A CPU write while an OAM DMA transfer runs: OAM writes are dropped
    /// for the whole transfer, start delay included (SameBoy
    /// `write_high_memory`), and the rest follows [`dma::conflict`] for
    /// CGB C. A write that conflicts with the DMA never reaches its
    /// target; what it does to OAM follows gambatte's cgb04c captures
    /// (ADR 0028). The transfer is synced first, because the write may
    /// change a source byte or its mapping.
    #[inline(never)]
    pub(super) fn write_during_dma(&mut self, addr: u16, value: u8) {
        let now = self.clock();
        self.dma_sync(now);
        if addr >= 0xFE00 {
            if addr >= 0xFF00 {
                self.write_ff(addr, value);
            }
            return;
        }
        match dma_conflict(&self.sys.ppu.dma, &self.hot, now, addr) {
            (Conflict::None, _) => self.write_to(addr, value),
            (Conflict::Wram(a), _) => self.write_to(a, value),
            (Conflict::Open | Conflict::Dma | Conflict::DmaVram, i) => {
                // CGB C (gambatte cgb04c `oamdma_src*_busypush*`, ADR
                // 0028): the CPU's data replaces the OAM byte just
                // transferred, except that a VRAM source gets 00 there
                // and a WRAM source leaves it. SameBoy instead clears it
                // for any source below A000 and drops the rest.
                let base = self.sys.ppu.dma.base;
                if !(0xC000..0xE000).contains(&base) {
                    self.mem.oam[usize::from(i)] = if base & 0xE000 == 0x8000 { 0 } else { value };
                    self.sys.ppu.oam_written();
                }
            }
        }
    }

    /// HALT during a transfer: the DMA makes no progress while the CPU is
    /// halted (SameBoy `GB_dma_run` returns early while `halted`, and
    /// `GB_advance_cycles` overwrites `dma_cycles`, so the halted cycles
    /// are dropped, not banked). Called at the end of HALT's fetch
    /// M-cycle, whose own step still counts. OAM stays blocked (the
    /// transfer is still active) and the `Dma` deadline is cleared, so the
    /// halted skip doesn't stop at it. `docs/reference/oam-dma.md`.
    ///
    /// For the PPU (synced first: a halt changes what its OAM reads see):
    /// with bytes left, the transfer stays active and halted until the
    /// wake-up; halted in the start delay, it hasn't started for the PPU;
    /// with every byte written, its last step isn't held and the PPU sees
    /// it end on time (gambatte `oamdma_late_halt_stat_2`, cgb04c).
    #[inline(never)]
    pub(super) fn dma_pause(&mut self) {
        let now = self.clock();
        if now >= self.sys.ppu.dma.end {
            return;
        }
        self.dma_ppu_event(now);
        self.dma_sync(now);
        let d = &mut self.sys.ppu.dma;
        d.paused = now;
        d.halt_from = now;
        d.halt_to = NEVER;
        d.halt_dest = d.copied;
        if now <= d.ppu_from {
            d.ppu_from = NEVER;
            d.ppu_to = NEVER;
        } else if d.copied < dma::LEN {
            d.ppu_to = NEVER;
        }
        self.sys.sched.set(EventKind::Dma, NEVER);
        self.reschedule();
    }

    /// The wake-up from HALT, at the start of the wake-up M-cycle: the
    /// transfer resumes there. SameBoy drops every halted M-cycle,
    /// including the wake-up one, then takes exactly one step on wake-up
    /// (`GB_cpu_run`: `dma_cycles = 4; GB_dma_run`): the same count, by the
    /// next CPU access, as resuming one M-cycle early. Everything still
    /// ahead moves out by the paused time. The PPU's OAM scan sees the CPU
    /// halted through the wake-up M-cycle (SameBoy clears `halted` after
    /// it); what it sees up to now doesn't change, so it needn't sync.
    #[inline(never)]
    pub(super) fn dma_resume(&mut self) {
        let now = self.clock();
        let m = self.hot.mcycle();
        let d = &mut self.sys.ppu.dma;
        let shift = now - d.paused;
        if d.block_from > d.paused {
            d.block_from += shift;
        }
        d.start += shift;
        d.end += shift;
        d.paused = NEVER;
        d.halt_to = now + m;
        let held = d.ppu_to == NEVER;
        if d.ppu_from == NEVER {
            d.set_ppu_window();
        } else if held {
            d.ppu_to = d.end - dma::PPU_LEAD;
        }
        self.sys.sched.set(EventKind::Dma, d.end);
        self.reschedule();
    }

    /// A speed switch while the transfer is paused ([`Self::dma_pause`]):
    /// what is still ahead keeps its M-cycle count at the new speed
    /// `to`, around the pause (SameBoy counts the DMA in CPU cycles,
    /// `dma_cycles`). Unverified on hardware (`docs/reference/oam-dma.md`).
    #[cold]
    #[inline(never)]
    pub(super) fn dma_retime(&mut self, to: Speed) {
        let d = &mut self.sys.ppu.dma;
        let p = d.paused;
        // Clocks from `p` (on the old M-cycle grid, as `p` is) scaled to
        // the new M-cycle length: halved to double speed, else doubled.
        let scale = |x: u64| if to.is_double() { x >> 1 } else { x << 1 };
        let at = |t: u64| {
            if t >= p {
                p + scale(t - p)
            } else {
                // Back to single speed within 640 clocks of reset, a
                // transfer started at double speed can have a rescaled
                // start before clock 0, which a `u64` can't hold. It
                // starts at the earliest clock the PPU's view can describe
                // (`PPU_LEAD`, `OamDma::set_ppu_window`/`ppu_at`) instead,
                // so what is left runs that many clocks late rather than
                // (wrapped) never being copied.
                p.saturating_sub(scale(p - t)).max(dma::PPU_LEAD)
            }
        };
        if d.block_from > p {
            d.block_from = at(d.block_from);
        }
        d.start = at(d.start);
        d.end = d.start + u64::from(dma::LEN) * to.mcycle();
        d.speed = to;
    }
}

/// The conflict for a CPU access to `addr` (below FE00) at `now` during
/// the transfer `d`, and the index of the byte being transferred. No
/// conflict in the start delay. (The restart tail, [`OamDma::tail`], is
/// in `d` as a transfer that started earlier: nothing here for it, as
/// any test here cost every read during a transfer up to 3% Ir on
/// `synth:dma-storm`, measured.)
fn dma_conflict(d: &OamDma, hot: &Hot, now: u64, addr: u16) -> (Conflict, u8) {
    if now < d.start {
        return (Conflict::None, 0);
    }
    // A shift, not a u64 division (a libcall on thumbv7em).
    let i = ((now - d.start) >> hot.mcycle_shift()) as u8;
    (dma::conflict(d.base, d.base + u16::from(i), addr), i)
}

/// A CPU read while an OAM DMA transfer runs (`Hot::dma`): OAM reads FF
/// (Pan Docs: OAM DMA bus conflicts; SameBoy `read_high_memory`), HRAM and
/// I/O are unaffected, and the rest follows [`dma::conflict`]. Side-effect
/// free like [`read`], except a conflicting read with a VRAM source
/// ([`dma_read_clear`]).
#[inline(never)]
pub(super) fn read_during_dma(mem: &Memory, sys: &Sys, hot: &Hot, addr: u16) -> u8 {
    let now = hot.clock();
    let plain = hot.without_dma();
    if addr >= 0xFE00 {
        if addr < 0xFF00 && now >= sys.ppu.dma.block_from {
            return 0xFF;
        }
        if addr == 0xFF41 {
            return sys.ppu.stat_during_dma(now, hot.speed());
        }
        return read_high(mem, sys, &plain, addr);
    }
    let addr = match dma_conflict(&sys.ppu.dma, hot, now, addr) {
        (Conflict::None, _) => addr,
        (Conflict::Open, _) => return 0xFF,
        (Conflict::Wram(a), _) => a,
        (Conflict::DmaVram, i) => {
            dma_read_clear(sys, i);
            sys.ppu.dma.base + u16::from(i)
        }
        (Conflict::Dma, i) => sys.ppu.dma.base + u16::from(i),
    };
    read::<true>(mem, sys, &plain, addr)
}

/// A CPU read on the VRAM bus in the M-cycle of byte `i` of a transfer
/// from VRAM (`Conflict::DmaVram`): besides returning the byte the DMA reads,
/// on CGB C it clears OAM byte `i` (gambatte cgb04c
/// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, which gambatte
/// `Memory::nontrivial_read` reproduces; ADR 0028 amendment). Recorded
/// here and applied by the next `Ctx::dma_sync`, which copies byte `i`
/// first (it is due by now), so the lazy copy can't overwrite the 00.
/// Inline: a call here would cost every read during a transfer saved
/// registers (measured on `synth:dma-storm`).
#[inline(always)]
fn dma_read_clear(sys: &Sys, i: u8) {
    sys.dma_read_clears.record(i);
}

#[cfg(test)]
mod tests {
    //! OAM DMA timing, blocking and CGB C conflicts (`docs/reference/oam-dma.md`).
    extern crate std;
    use super::*;
    use crate::bus::test_util::{idle_to, setup, setup_with, with_ctx};
    use crate::cpu::Bus;
    use crate::ppu::Frame;
    use crate::{Buttons, Emulator, Events};
    use std::boxed::Box;

    #[test]
    fn start_delay_blocking_and_end() {
        let emu = setup();
        let mut t0 = 0;
        with_ctx(emu, |ctx| {
            t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // M-cycle 0
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + 162 * 4);
            // M-cycle 1, the start delay: OAM is readable and untouched.
            assert_eq!(ctx.read(0xFE00), 0x55);
            // M-cycle 2: byte 0 transferred, OAM blocked; HRAM is not.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.read(0xFEA5), 0xFF);
            ctx.write(0xFF80, 0x12);
            assert_eq!(ctx.read(0xFF80), 0x12);
            // OAM writes are dropped.
            ctx.write(0xFE30, 0x99);
            ctx.write(0xFEA0, 0x99);
            idle_to(ctx, t0, 10);
        });
        // Between run calls OAM is synced to the clock: bytes 0-8 are in
        // (byte 8 counts from the access at M-cycle 10), the rest are not.
        // `peek` sees OAM as it is, without FF or conflicts.
        assert_eq!(emu.peek(0xFE00), 0xA5);
        assert_eq!(emu.peek(0xFE08), 8 ^ 0xA5);
        assert_eq!(emu.peek(0xFE09), 0x55);
        assert_eq!(emu.peek(0xC000), emu.mem.wram[0]);
        assert_eq!(emu.peek(0xC105), 5 ^ 0xA5);
        with_ctx(emu, |ctx| {
            idle_to(ctx, t0, 161);
            // The last transfer M-cycle: still blocked.
            assert_eq!(ctx.read(0xFE9F), 0xFF);
            // M-cycle 162: done.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFE9F), 0x9F ^ 0xA5);
            assert_eq!(ctx.read(0xFEA0), 0);
            ctx.write(0xFE30, 0x99);
            assert_eq!(ctx.read(0xFE30), 0x99);
        });
        for (i, &b) in emu.mem.oam.iter().enumerate() {
            if i != 0x30 {
                assert_eq!(b, i as u8 ^ 0xA5, "{i:#x}");
            }
        }
    }

    #[test]
    fn restart_keeps_oam_blocked_through_the_new_delay() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0xC2); // M-cycle 20
            // The new start delay: the old transfer still blocks OAM.
            assert_eq!(ctx.read(0xFE00), 0xFF);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), t0 + (20 + 162) * 4);
            // The new transfer from C200.
            idle_to(ctx, t0, 20 + 162);
            assert_eq!(ctx.read(0xFE00), 0xA5);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// The M-cycle after a restarting FF46 write: the old transfer moves
    /// its next byte from the new source, so a CPU access on the new
    /// source's bus conflicts with that byte (gambatte cgb04c
    /// `oamdma_src8000_srcchange0000_busyinc`).
    #[test]
    fn restart_moves_the_old_byte_from_the_new_source() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 20);
            ctx.write(0xFF46, 0x40); // M-cycle 20: restart from ROM 4000
            // M-cycle 21: the old transfer's byte 19, from 4013.
            assert_eq!(ctx.read(0x0123), 19 ^ 0x3C);
            // M-cycle 22: the new transfer's byte 0.
            assert_eq!(ctx.read(0x0123), 0x3C);
            // The new transfer copies all 160 bytes from 4000 as usual.
            idle_to(ctx, t0, 190);
            assert_eq!(ctx.hot.dma, 0);
            for (i, &b) in ctx.mem.oam.iter().enumerate() {
                assert_eq!(b, i as u8 ^ 0x3C, "{i:#x}");
            }
            // Restarted in its own start delay: byte 0 in the next M-cycle.
            idle_to(ctx, t0, 200);
            ctx.write(0xFF46, 0xC1); // M-cycle 200
            ctx.write(0xFF46, 0x40); // 201
            assert_eq!(ctx.read(0x0123), 0x3C); // 202: byte 0 from 4000
            assert_eq!(ctx.read(0x0123), 0x3C); // 203: the new byte 0
        });
        // A fresh start conflicts with nothing in its start delay.
        let emu = setup();
        with_ctx(emu, |ctx| {
            ctx.write(0xFF46, 0x40);
            assert_eq!(ctx.read(0x0123), 0);
            assert_eq!(ctx.read(0x0123), 0x3C);
        });
    }

    #[test]
    fn restart_in_the_last_mcycle_is_a_fresh_start() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.write(0xFF46, 0xC1); // the old transfer's last M-cycle
            assert_eq!(ctx.read(0xFE10), 0x10 ^ 0xA5);
            assert_eq!(ctx.read(0xFE10), 0xFF);
        });
    }

    /// OAM DMA steps inside a VRAM DMA write the VRAM DMA's byte of that
    /// moment to OAM at its source address & FF, not their own (gambatte
    /// cgb04c `dma/hdma_transition_oamdma_1`).
    #[test]
    fn oam_dma_steps_during_a_vram_dma_take_its_bytes() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1); // bytes i ^ A5
            for (reg, v) in [(0xFF51, 0x40), (0xFF52, 0), (0xFF53, 0), (0xFF54, 0)] {
                ctx.write(reg, v);
            }
            ctx.write(0xFF55, 0x00); // a 1-block GDMA from ROM 4000
            idle_to(ctx, t0, 10);
            // At M-cycle 10 (bytes 0-8 due): the block's 8 M-cycles hold
            // the steps of bytes 9-16, which take block bytes 1, 3, .. 15.
            ctx.vram_dma(true);
            idle_to(ctx, t0, 170);
        });
        for (p, &b) in emu.mem.oam.iter().enumerate() {
            let want = match p {
                1..=15 if p % 2 == 1 => p as u8 ^ 0x3C,
                10 | 12 | 14 | 16 => 0x55,
                _ => p as u8 ^ 0xA5,
            };
            assert_eq!(b, want, "{p:#x}");
        }
    }

    #[test]
    fn wram_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            // The start delay: no conflict.
            assert_eq!(ctx.read(0xC000), ctx.mem.wram[0]);
            idle_to(ctx, t0, 7);
            // WRAM reads see the DMA's byte (M-cycle 7: byte 5, C105); ROM
            // and VRAM are free.
            assert_eq!(ctx.read(0xD000), 5 ^ 0xA5);
            assert_eq!(ctx.read(0x4001), 1 ^ 0x3C);
            // The address the DMA reads next is not a conflict.
            assert_eq!(ctx.read(0xC108), 8 ^ 0xA5); // M-cycle 9: byte 7
            // WRAM writes are dropped.
            let before = ctx.mem.wram[0x10];
            ctx.write(0xC010, 0x77);
            assert_eq!(ctx.mem.wram[0x10], before);
            // A VRAM write is free.
            ctx.write(0x8000, 0x66);
            assert_eq!(ctx.mem.vram[0], 0x66);
        });
    }

    #[test]
    fn rom_source_conflicts() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0x40);
            idle_to(ctx, t0, 4);
            // External bus: the DMA's byte (M-cycle 4: byte 2, 4002).
            assert_eq!(ctx.read(0x0150), 2 ^ 0x3C);
            // WRAM: redirected to the DMA's bank half (bit 12 clear).
            ctx.mem.wram[0x123] = 0x42;
            assert_eq!(ctx.read(0xD123), 0x42);
            ctx.write(0xD124, 0x43); // M-cycle 6, goes to C124
            assert_eq!(ctx.mem.wram[0x124], 0x43);
            // An external-bus write is dropped, and its data replaces the
            // OAM byte just transferred (byte 5; CGB C, ADR 0028).
            ctx.write(0x2000, 0x05); // M-cycle 7
            assert_eq!(ctx.mem.oam[5], 0x05);
            assert_eq!(ctx.mem.oam[4], 4 ^ 0x3C);
            assert_eq!(ctx.sys.cart.read_rom(0x4000), 0x3C); // bank unchanged
            // VRAM is free.
            ctx.mem.vram[5] = 0x31;
            assert_eq!(ctx.read(0x8005), 0x31);
        });
    }

    #[test]
    fn high_source_reads_ff_and_opens_the_external_bus() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xFE);
            idle_to(ctx, t0, 3);
            assert_eq!(ctx.read(0x0150), 0xFF);
            ctx.mem.wram[0x1456] = 0x24;
            assert_eq!(ctx.read(0xC456), 0x24); // FE00: bit 12 set, D456
            idle_to(ctx, t0, 162);
        });
        assert!(emu.mem.oam.iter().all(|&b| b == 0xFF));
    }

    /// Without HALT (STOP, lockup: not paused, `docs/reference/oam-dma.md`)
    /// the idle skip stops at the `Dma` deadline like any other.
    #[test]
    fn unhalted_skip_stops_at_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            assert_eq!(ctx.hot.dma, 0xFE00);
            // Other deadlines may come first (in CGB mode the first
            // DIV-APU edge is 352 clocks after power on, ADR 0022): skip
            // until the transfer ends; no skip may pass its deadline.
            // Each skip advances the clock, so a few dozen iterations reach
            // the deadline; the cap turns a stuck skip into a failure.
            let mut skips = 0;
            while ctx.hot.dma != 0 {
                skips += 1;
                assert!(
                    skips <= 1000,
                    "idle skip made no progress: dma still {:#06X} after {skips} skips, clock {} (deadline {})",
                    ctx.hot.dma,
                    ctx.clock(),
                    t0 + 162 * 4
                );
                ctx.idle_halted();
                assert!(ctx.clock() <= t0 + 162 * 4);
            }
            assert_eq!(ctx.clock(), t0 + 162 * 4);
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.mem.oam[0x9F], 0x9F ^ 0xA5);
        });
    }

    /// HALT in M-cycle 10 of a transfer, 50 halted M-cycles, wake-up. The
    /// transfer makes no progress while halted, OAM stays blocked, and the
    /// end moves out by the paused time (SameBoy `GB_dma_run`).
    #[test]
    fn halt_pauses_the_transfer() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            ctx.read(0x0100); // HALT's fetch: its step counts
            ctx.halt_begin();
            let f = ctx.clock();
            assert_eq!(ctx.sys.ppu.dma.copied, 10);
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), NEVER);
            for _ in 0..50 {
                ctx.idle();
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.sys.ppu.dma.copied, 10);
                assert_eq!(ctx.hot.dma, 0xFE00);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE00), 0xFF);
                assert_eq!(read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFE9F), 0xFF);
            }
            assert_eq!(ctx.mem.oam[10], 0x55);
            // The wake-up M-cycle: pushed out by 50 M-cycles.
            ctx.halt_end();
            assert_eq!(ctx.clock(), f + 50 * 4);
            let end = t0 + (162 + 50) * 4;
            assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), end);
            ctx.idle();
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 11);
            while ctx.clock() < end - 4 {
                ctx.idle();
            }
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.idle();
            assert_eq!((ctx.clock(), ctx.hot.dma), (end, 0));
        });
        assert!((0..0xA0).all(|i| emu.mem.oam[i] == i as u8 ^ 0xA5));
    }

    /// STOP mode pauses the transfer like HALT (SameBoy `GB_dma_run`
    /// returns while `stopped`; `leave_stop_mode` takes one step).
    #[test]
    fn stop_mode_pauses_the_transfer() {
        // P1 = 10 (action buttons); IF = 0; LD A,C1; LDH (46),A; STOP 00;
        // LD B,B.
        let emu = setup_with(&[
            0x3E, 0x10, 0xE0, 0x00, 0xAF, 0xE0, 0x0F, 0x3E, 0xC1, 0xE0, 0x46, 0x10, 0x00, 0x40,
        ]);
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        for _ in 0..7 {
            let _ = emu.step(&mut fb, &mut []);
        }
        assert_eq!(emu.hw.cpu.registers().pc, 0x10D);
        // A byte per M-cycle from the one after the FF46 write's: STOP's
        // fetch and its second byte.
        let copied = emu.hw.sys.ppu.dma.copied;
        assert_eq!(copied, 2);
        let _ = emu.run_cycles(10_000, &mut fb, &mut []);
        assert_eq!(emu.hw.sys.ppu.dma.copied, copied);
        assert_eq!(emu.hw.sys.hot.dma, 0xFE00);
        assert_eq!(emu.mem.oam[usize::from(copied)], 0x55);
        emu.set_buttons(Buttons::A);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(1000, &mut fb, &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        // The two wake-up M-cycles and LD B,B's fetch: three more bytes.
        assert_eq!(emu.hw.sys.ppu.dma.copied, copied + 3);
        let _ = emu.run_cycles(4 * 160, &mut fb, &mut []);
        assert_eq!(emu.hw.sys.hot.dma, 0);
        assert!((0..0xA0).all(|i| emu.mem.oam[i] == i as u8 ^ 0xA5));
    }

    /// A CPU write on the bus the transfer reads, in the M-cycle of byte
    /// 5: it never reaches its target, and OAM byte 5 gets the CPU's data
    /// (ROM/SRAM and E000-FFFF sources), 00 (VRAM source) or keeps the
    /// DMA's byte (WRAM source). gambatte cgb04c `oamdma_src*_busypush*`,
    /// ADR 0028.
    #[test]
    fn conflicting_write_lands_in_oam_by_source() {
        // (FF46 value, CPU write address, OAM byte 5 afterwards).
        let cases = [
            (0x40, 0xA123, 0x9A),
            (0x40, 0x7FFF, 0x9A),
            (0x80, 0x9123, 0x00),
            (0xC1, 0xD123, 5 ^ 0xA5),
            (0xFE, 0x2000, 0x9A),
            (0xFE, 0xB000, 0x9A),
        ];
        for (src, addr, want) in cases {
            let emu = setup();
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                ctx.write(addr, 0x9A);
                ctx.dma_sync(ctx.clock());
            });
            assert_eq!(emu.mem.oam[5], want, "source {src:02X}, write {addr:04X}");
            match addr {
                0x9123 => assert_eq!(emu.mem.vram[0x1123], 0),
                0xD123 => assert_eq!(emu.mem.wram[0x1123], 0),
                _ => {}
            }
        }
    }

    /// A CPU read on the VRAM bus during a transfer from VRAM returns the
    /// byte being transferred and clears that OAM byte (gambatte cgb04c
    /// `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`, ADR 0028 amendment):
    /// byte 5 (not yet copied when read), byte 6 (copied by a sync at the
    /// same clock, before the read) and byte 40 (a second word of the
    /// record), all applied by the next sync. A WRAM source's conflicting
    /// read leaves OAM alone.
    #[test]
    fn conflicting_vram_read_clears_the_oam_byte() {
        for (src, addr) in [(0x80u8, 0x9123), (0xC1, 0xD123)] {
            let emu = setup();
            for (i, b) in emu.mem.vram[..0xA0].iter_mut().enumerate() {
                *b = i as u8 ^ 0x69;
            }
            let byte = |i: usize| {
                if src == 0x80 {
                    i as u8 ^ 0x69
                } else {
                    i as u8 ^ 0xA5
                }
            };
            with_ctx(emu, |ctx| {
                let t0 = ctx.clock();
                ctx.write(0xFF46, src);
                idle_to(ctx, t0, 2 + 5);
                assert_eq!(ctx.read(addr), byte(5));
                ctx.dma_sync(ctx.clock());
                assert_eq!(ctx.read(addr), byte(6));
                idle_to(ctx, t0, 2 + 40);
                assert_eq!(ctx.read(addr), byte(40));
                ctx.idle();
                ctx.dma_sync(ctx.clock());
            });
            let cleared = |i: usize| src == 0x80 && matches!(i, 5 | 6 | 40);
            for (i, &b) in emu.mem.oam[..=41].iter().enumerate() {
                let want = if cleared(i) { 0 } else { byte(i) };
                assert_eq!(b, want, "source {src:02X}, byte {i}");
            }
            assert!(emu.hw.sys.dma_read_clears.is_empty());
        }
    }

    /// The halted skip passes the (cleared) `Dma` deadline: a paused
    /// transfer is no deadline (ADR 0011).
    #[test]
    #[cfg_attr(feature = "eager-sync", ignore = "the skip is one M-cycle there")]
    fn halted_skip_passes_the_dma_deadline() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // APU off: no frame-sequencer events.
            ctx.write(0xFF26, 0x00);
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 3);
            ctx.halt_begin();
            ctx.idle_halted();
            assert!(ctx.clock() > t0 + 1_000_000);
            assert_eq!(ctx.hot.dma, 0xFE00);
            ctx.dma_sync(ctx.clock());
            assert_eq!(ctx.sys.ppu.dma.copied, 2);
        });
    }

    /// The CPU's hooks: `LD A,C1; LDH (46),A; HALT`, 40 halted M-cycles, an
    /// IRQ with IME off, then `LD A,(D000)`, which conflicts with the WRAM
    /// source and reads the byte being transferred. SameBoy: the HALT's
    /// fetch M-cycle steps (byte 0), the halted ones including the wake-up
    /// M-cycle don't, the wake-up takes one step (byte 1), and the three
    /// M-cycles before the read step (bytes 2-4).
    #[test]
    fn wake_up_takes_one_dma_step() {
        let emu = setup_with(&[0x3E, 0xC1, 0xE0, 0x46, 0x76, 0xFA, 0x00, 0xD0, 0x18, 0xFE]);
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        let cpu = &mut hw.cpu;
        cpu.set_ime(false);
        ctx.sys.ie = 0x01;
        ctx.sys.iflags = 0;
        cpu.step(&mut ctx);
        cpu.step(&mut ctx);
        let w = ctx.clock() - 4;
        cpu.step(&mut ctx);
        assert!(cpu.halted());
        assert_eq!(ctx.clock(), w + 8);
        for _ in 0..40 {
            ctx.idle();
        }
        ctx.sys.iflags = 0x01;
        cpu.step(&mut ctx); // wake-up M-cycle, LD A,(D000)
        assert!(!cpu.halted());
        assert_eq!(cpu.registers().a, 4 ^ 0xA5);
        assert_eq!(ctx.sys.sched.deadline(EventKind::Dma), w + (162 + 40) * 4);
    }

    /// HALT with bytes left: the PPU sees the transfer halted until the
    /// end of the wake-up M-cycle (SameBoy clears `halted` after it), then
    /// running again, moved out by the halted time.
    #[test]
    fn halt_holds_the_ppu_window_while_bytes_are_left() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 10);
            let p = ctx.clock();
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(p), (Running, 9));
            assert_eq!(d.ppu_at(p + 1), (Halted, 9));
            assert_eq!(d.ppu_at(p + 5000), (Halted, 9));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 4), (Halted, 9));
            assert_eq!(d.ppu_at(r + 5), (Running, 10));
            assert_eq!(d.ppu_to, d.end - 2);
            assert_eq!(d.ppu_at(d.end - 2), (Running, 160));
            assert_eq!(d.ppu_at(d.end - 1), (Off, 160));
        });
    }

    /// The speed-switch halt after the last byte, before the end: unlike
    /// HALT, the transfer's last step is held for the PPU through the
    /// halt (gambatte cgb04c `oamdma/oamdma_late_speedchange_stat_1`).
    #[test]
    fn speed_switch_after_the_last_byte_holds_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            assert!(ctx.speed_switch(true, false));
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.copied, 160);
            assert_eq!(d.ppu_at(t0 + 647), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 5000), (Halted, 160));
        });
    }

    /// HALT after the last byte: the transfer's last step isn't held for
    /// the PPU, which sees it end on time (gambatte
    /// `oamdma/oamdma_late_halt_stat_2`, cgb04c).
    #[test]
    fn halt_after_the_last_byte_does_not_hold_the_ppu() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            idle_to(ctx, t0, 161);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 646), (Halted, 160));
            assert_eq!(d.ppu_at(t0 + 647), (Off, 160));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            assert_eq!(ctx.sys.ppu.dma.ppu_at(r + 1), (Off, 160));
        });
    }

    /// HALT in the start delay: the transfer hasn't started for the PPU,
    /// and starts only after the wake-up.
    #[test]
    fn halt_in_the_start_delay_delays_the_ppu_window() {
        use crate::dma::PpuView::*;
        let emu = setup();
        with_ctx(emu, |ctx| {
            let t0 = ctx.clock();
            ctx.write(0xFF46, 0xC1);
            ctx.halt_begin();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(t0 + 7), (Off, 0));
            assert_eq!(d.ppu_at(t0 + 5000), (Off, 0));
            for _ in 0..50 {
                ctx.idle();
            }
            let r = ctx.clock();
            ctx.halt_end();
            let d = ctx.sys.ppu.dma;
            assert_eq!(d.ppu_at(r + 2), (Off, 0));
            assert_eq!(d.ppu_at(r + 5), (Running, 1));
        });
    }

    /// STAT reads mode 0 instead of 2 while a transfer runs (SameBoy
    /// `GB_STAT_update`); modes 3 and 0 read as usual, and mode 2 again
    /// once the transfer is over.
    #[test]
    fn stat_reads_mode_2_as_0_during_oam_dma() {
        let emu = setup();
        with_ctx(emu, |ctx| {
            // LCD on at E: frame position = clock - E + 7, so the access at
            // E + 4k is at position 4k + 7.
            let e = ctx.clock();
            ctx.write(0xFF40, 0x91);
            idle_to(ctx, e, 100);
            ctx.write(0xFF46, 0xC1); // Line 0 (the short first line), HBlank.
            idle_to(ctx, e, 118); // Line 1, t = 23: mode 2.
            assert_eq!(ctx.read(0xFF41) & 3, 0);
            idle_to(ctx, e, 137); // Line 1, t = 99: mode 3.
            assert_eq!(ctx.read(0xFF41) & 3, 3);
            idle_to(ctx, e, 346); // Line 3, t = 23: mode 2, transfer over.
            assert_eq!(ctx.hot.dma, 0);
            assert_eq!(ctx.read(0xFF41) & 3, 2);
        });
    }
}
