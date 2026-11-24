//! CGB VRAM DMA (FF51-FF55): general-purpose (GDMA) and HBlank (HDMA)
//! transfers into VRAM. Design: ADR 0017.
//!
//! The register semantics follow SameBoy (Core/memory.c, MIT, Copyright
//! (c) 2015-2025 Lior Halphon: `GB_IO_HDMA1`-`GB_IO_HDMA5` in
//! `write_high_memory`, `GB_IO_HDMA5` in `read_high_memory`,
//! `GB_hdma_run`), rewritten here; Pan Docs "LCD VRAM DMA Transfers".
//!
//! - The source and destination are running 16-bit counters, not
//!   reloaded per transfer: a new FF55 write without new FF51-FF54 writes
//!   continues where the last transfer stopped (SameSuite
//!   `dma/gbc_dma_cont`). FF51-FF54 are write-only (read FF).
//! - A transfer runs at the CPU's next instruction fetch (SameBoy runs
//!   `GB_hdma_run` right after the opcode fetch), not inside an
//!   instruction; an interrupt dispatch due then goes first.
//! - The CPU is stalled while it runs: one M-cycle, plus 2 dots per byte
//!   (SameBoy: 2 T-cycles per byte at single speed, 4 at double speed,
//!   before and after the bytes 2 T-cycles at single speed, 4 before them
//!   at double speed). So 8 M-cycles per block plus one at single speed,
//!   16 plus one at double speed.

/// What one transfer step does: the M-cycles the CPU is stalled for `n`
/// 16-byte blocks at `mcycle` base clocks per M-cycle (4, or 2 in double
/// speed): 2 dots per byte, plus one M-cycle.
pub(crate) const fn stall_mcycles(blocks: u32, mcycle: u32) -> u32 {
    // `mcycle` is 4 or 2: a shift by 2 or 1 (no division, so no
    // divide-by-zero panic path).
    1 + ((blocks * 32) >> (mcycle >> 1))
}

/// HDMA starts a block this many dots after the line's mode-3 end
/// ([`Ppu::mode0_start`](crate::ppu::Ppu::mode0_start), where STAT reads
/// mode 0 at single speed), in both speeds: one dot after the mode-0 STAT
/// interrupt request. gambatte (read for behaviour) triggers HDMA at its
/// xpos 167, one dot after its mode-0 interrupt (xpos 166); its CGB C
/// captures `dma/hdma_start_scx{,2,3,5}*` and `hdma_late_disable_scx*`
/// (both speeds) allow only 2 here. SameBoy (`GB_display_run`) uses 3.
pub(crate) const HBLANK_DELAY: u64 = 2;

/// HDMA's HBlank period, in which an FF55 write starts a block at once and
/// a HALT counts as in the HBlank (no block at its wake-up), ends this
/// many dots into the next line (where STAT still reads mode 0). gambatte
/// CGB C captures bracket it (`dma/hdma_late_m0halt_{,ds_}{1,2}`,
/// `hdma_late_enable_{,ds_}{1,2}`).
pub(crate) const HBLANK_UNTIL: u64 = 4;

/// [`VramDma::refetch`]: a block requested at HALT or STOP waits for the
/// wake-up.
pub(crate) const REQUESTED: u8 = 1;
/// [`VramDma::refetch`]: that block ran; triggers before `switch_end` are
/// lost.
pub(crate) const RAN: u8 = 2;

/// The FF51-FF55 state.
#[derive(Clone, Copy)]
pub(crate) struct VramDma {
    /// The source counter (SameBoy `hdma_current_src`): FF51 high byte,
    /// FF52 bits 4-7. A source in E000-FFFF is forced to F000-FFFF.
    pub(crate) src: u16,
    /// The destination counter (`hdma_current_dest`): FF53 (all 8 bits)
    /// high, FF54 bits 4-7. Bytes go to VRAM offset `dest & 0x1FFF` in the
    /// current VBK bank; the transfer also ends when the counter wraps to
    /// 0000 at a block end.
    pub(crate) dest: u16,
    /// Blocks left (`hdma_steps_left`): FF55 bits 0-6, plus one.
    pub(crate) blocks: u8,
    /// An HBlank transfer is active (`hdma_on_hblank`).
    pub(crate) hblank: bool,
    /// A transfer step is due at the next instruction fetch
    /// (`hdma_on`): all blocks of a GDMA, or one HDMA block.
    pub(crate) pending: bool,
    /// The pending step is one block whatever `hblank` says: its HBlank
    /// trigger came before the FF55 write that stopped the transfer, in
    /// the same instruction ([`Self::write_hdma5`]).
    pub(crate) single: bool,
    /// The CPU is in HALT while an HBlank transfer is active: HBlanks
    /// don't start blocks (SameBoy: `!gb->halted` in the HBlank trigger).
    pub(crate) halted: bool,
    /// HALT began outside the HBlank (`allow_hdma_on_wake`) during an
    /// HBlank transfer: the wake-up, in mode 0, starts a block. Cleared at
    /// the wake-up.
    pub(crate) wake_ok: bool,
    /// A block requested in HALT's M-cycle or a speed-switch STOP's first
    /// (gambatte `Memory::halt`/`stop`: `prefetched = hdmaReqFlagged`):
    /// [`REQUESTED`] until that block runs at the wake-up (to double
    /// speed the switch runs it at once). Its stall is then one M-cycle
    /// shorter, but triggers until its full length are still lost
    /// (gambatte: `cc -= 4` after a DMA in the `hdma_requested` halt
    /// state, acknowledged at the transfer's end): [`RAN`], with the end
    /// in `switch_end`, until the next trigger. 0 otherwise. Fits this
    /// struct's padding.
    pub(crate) refetch: u8,
    /// M-cycles left of the current transfer step's CPU stall
    /// ([`stall_mcycles`]; at most 2,049). A run call can end inside the
    /// stall (a stop event or its target); the next call resumes it
    /// before the CPU runs (ADR 0021, GDMA stall amendment).
    pub(crate) stall: u16,
    /// Not VRAM DMA state: during the CGB speed-switch halt
    /// (`Ctx::speed_switch`), the low 32 bits of the clock at which its
    /// last M-cycle (the wake-up) starts; the halt is far shorter than
    /// 2^31 clocks, so a wrapping compare is exact. After a [`RAN`] block,
    /// the end of its full stall (the same way). Stale otherwise. Placed
    /// in this struct's padding while `Hw` had a size cap (dropped
    /// 2026-11-24); it belongs in `Sys`.
    pub(crate) switch_end: u32,
    /// The mode-0 start clock of the last line whose HBlank trigger was
    /// scheduled or passed, so each line starts at most one block.
    pub(crate) line: u64,
}

impl VramDma {
    pub(crate) const fn new() -> Self {
        Self {
            src: 0,
            dest: 0,
            blocks: 0,
            hblank: false,
            pending: false,
            single: false,
            halted: false,
            wake_ok: false,
            refetch: 0,
            stall: 0,
            switch_end: 0,
            line: crate::schedule::NEVER,
        }
    }

    /// FF51-FF54 (`reg` = 1-4): the address counters.
    pub(crate) fn write_addr(&mut self, reg: usize, value: u8) {
        let v = u16::from(value);
        match reg {
            1 => {
                self.src = (self.src & 0x00F0) | (v << 8);
                // E000-FFFF behaves as F000-FFFF (SameBoy: "can't
                // overflow (with 0x800 bytes) to anything meaningful").
                if self.src >= 0xE000 {
                    self.src |= 0xF000;
                }
            }
            2 => self.src = (self.src & 0xFF00) | (v & 0xF0),
            3 => self.dest = (self.dest & 0x00F0) | (v << 8),
            _ => self.dest = (self.dest & 0xFF00) | (v & 0xF0),
        }
    }

    /// FF55 write. `mode0`: STAT reads mode 0 now (also with the LCD
    /// off), so an HBlank transfer starts its first block at once.
    pub(crate) fn write_hdma5(&mut self, value: u8, mode0: bool) {
        // Also on a cancel: the read-back then shows the written length.
        self.blocks = (value & 0x7F) + 1;
        let hblank = value & 0x80 != 0;
        if !hblank && self.hblank {
            // Bit 7 = 0 during an HBlank transfer stops it. A block whose
            // HBlank trigger came earlier in this instruction still runs,
            // as that one block of the length just written, not as a
            // general-purpose transfer of all of it: gambatte (read for
            // behaviour) leaves the request flagged at a disable, and
            // `Memory::dma` copies 16 bytes for a flagged HBlank request
            // whatever FF55 holds (its CGB C captures
            // `dma/hdma_late_disable_*_2` show that block for a written
            // length of 0). SameBoy (`GB_IO_HDMA5`, `GB_hdma_run`) differs:
            // with `hdma_on` set its loop runs every block of the new
            // length. FF55 follows gambatte here already, so the block
            // does too.
            self.hblank = false;
            self.single = self.pending;
            return;
        }
        self.single = false;
        self.hblank = hblank;
        self.pending = !hblank || mode0;
    }

    /// FF55 read: bit 7 = 0 while a transfer is active; bits 0-6 = blocks
    /// left minus one (7F after a completed transfer).
    pub(crate) const fn read_hdma5(&self) -> u8 {
        let active = if self.hblank || self.pending { 0 } else { 0x80 };
        active | (self.blocks.wrapping_sub(1) & 0x7F)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::HBLANK_DELAY;
    use crate::bus::Ctx;
    use crate::ppu::Frame;
    use crate::{Config, Emulator, Events};
    use std::boxed::Box;
    use std::vec;

    /// A CGB-mode (or, `cgb_flag` 0, DMG compatibility mode) emulator
    /// running NOPs, WRAM C100-C1FF = `i ^ 0xA5`, LCDC = `lcdc`, and the
    /// transfer addresses set to C10F -> 9F0F, so to C100 -> 8000 once
    /// masked (dest bits 13-15 kept).
    fn setup(lcdc: u8, cgb_flag: u8) -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x143] = cgb_flag;
        // HALT, far enough not to be reached by stepping from 0100, then
        // LD A,(8000); then EI, NOP.
        rom[0x7000..0x7006].copy_from_slice(&[0x76, 0xFA, 0x00, 0x80, 0xFB, 0x00]);
        // STOP with INC A as its second byte, then NOPs.
        rom[0x7100..0x7102].copy_from_slice(&[0x10, 0x3C]);
        // HALT, INC A, then NOPs; LDH (55),A running into VRAM at 8000.
        rom[0x6000..0x6002].copy_from_slice(&[0x76, 0x3C]);
        // LDH (55),A, then NOPs.
        rom[0x6100..0x6102].copy_from_slice(&[0xE0, 0x55]);
        rom[0x7FFE..0x8000].copy_from_slice(&[0xE0, 0x55]);
        let emu = crate::tests::fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        for i in 0..0x100 {
            emu.mem.wram[0x100 + i] = i as u8 ^ 0xA5;
        }
        write(emu, 0xFF40, lcdc);
        for (reg, v) in [
            (0xFF51, 0xC1),
            (0xFF52, 0x0F),
            (0xFF53, 0x60),
            (0xFF54, 0x0F),
        ] {
            write(emu, reg, v);
        }
        emu
    }

    /// A CPU write through the bus, without an M-cycle.
    fn write(emu: &mut Emulator, addr: u16, value: u8) {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        let Emulator { mem, hw } = emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        ctx.write_mapped(addr, value);
    }

    /// One `emu.step`: its length in base clocks.
    fn step(emu: &mut Emulator) -> u64 {
        let mut fb: Box<Frame> = Box::new([0; 160 * 144]);
        emu.step(&mut fb, &mut []).clocks
    }

    /// Steps until one takes longer than a NOP: its start clock and
    /// length.
    fn step_to_stall(emu: &mut Emulator) -> (u64, u64) {
        for _ in 0..100_000 {
            let t = emu.clock();
            let d = step(emu);
            if d > 4 {
                return (t, d);
            }
        }
        panic!("no stall");
    }

    fn block(i: usize) -> [u8; 16] {
        core::array::from_fn(|j| (16 * i + j) as u8 ^ 0xA5)
    }

    #[test]
    fn registers_decode_and_read_back() {
        let emu = setup(0x91, 0x80);
        // FF51-FF54 are write-only; the low 4 bits are ignored.
        for reg in 0xFF51..=0xFF54 {
            assert_eq!(emu.peek(reg), 0xFF);
        }
        let v = &emu.hw.sys.vdma;
        assert_eq!((v.src, v.dest), (0xC100, 0x6000));
        assert_eq!(emu.peek(0xFF55), 0xFF);
        // A source in E000-FFFF reads as F000-FFFF.
        write(emu, 0xFF51, 0xE2);
        assert_eq!(emu.hw.sys.vdma.src, 0xF200);
        // DMG compatibility mode: unmapped.
        let dmg = setup(0x91, 0x00);
        assert_eq!(dmg.hw.sys.vdma.src, 0);
        write(dmg, 0xFF55, 0x05);
        assert_eq!(dmg.peek(0xFF55), 0xFF);
        assert!(!dmg.hw.sys.vdma.pending);
    }

    #[test]
    fn gdma_copies_everything_at_the_next_fetch_and_stalls() {
        let emu = setup(0x11, 0x80); // LCD off
        write(emu, 0xFF55, 0x01); // 2 blocks
        assert_eq!(emu.peek(0xFF55), 0x01);
        // The transfer, 1 + 2 * 8 M-cycles, then the NOP.
        assert_eq!(step(emu), 4 * 18);
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.mem.vram[16..32], block(1));
        assert_eq!(emu.mem.vram[32], 0);
        assert_eq!(emu.peek(0xFF55), 0xFF);
        assert_eq!(step(emu), 4);
    }

    #[test]
    fn gdma_in_double_speed_takes_16_mcycles_per_block() {
        let emu = setup(0x11, 0x80);
        emu.hw.sys.hot.mcycle = 2;
        write(emu, 0xFF55, 0x00);
        assert_eq!(step(emu), 2 * (1 + 16 + 1));
    }

    #[test]
    fn hdma_copies_one_block_per_hblank() {
        let emu = setup(0x91, 0x80);
        // Start in mode 2 of some line.
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81); // 2 blocks
        assert_eq!(emu.peek(0xFF55), 0x01);
        let (t, d) = step_to_stall(emu);
        // At the first fetch at or after the HBlank trigger, 3 dots after
        // the mode-3 end: 1 + 8 M-cycles, then the NOP.
        let m0 = emu.hw.sys.ppu.mode0_start().unwrap();
        assert!(t >= m0 + 3 && t < m0 + 3 + 4, "{t} {m0}");
        assert_eq!(d, 4 * 10);
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.mem.vram[16], 0);
        assert_eq!(emu.peek(0xFF55), 0x00);
        // The next line's HBlank: the second (last) block.
        let (t2, _) = step_to_stall(emu);
        assert_eq!(emu.hw.sys.ppu.mode0_start().unwrap() - m0, 456);
        assert!((452..460).contains(&(t2 - t)), "{}", t2 - t);
        assert_eq!(emu.mem.vram[16..32], block(1));
        assert_eq!(emu.peek(0xFF55), 0xFF);
        // Done: no more stalls on the next lines.
        for _ in 0..1000 {
            assert_eq!(step(emu), 4);
        }
    }

    #[test]
    fn hdma_starts_at_once_in_mode_0_and_can_be_cancelled() {
        let emu = setup(0x91, 0x80);
        // Mode 0, past the HBlank trigger.
        while emu.peek(0xFF41) & 3 != 0
            || emu.peek(0xFF44) == 0
            || !(8..100).contains(&(emu.clock() - emu.hw.sys.ppu.mode0_start().unwrap()))
        {
            step(emu);
        }
        write(emu, 0xFF55, 0x83); // 4 blocks, in mode 0: one now
        assert_eq!(step(emu), 4 * 10);
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.peek(0xFF55), 0x02);
        // Cancel: bit 7 reads 1; the read-back length is the written one.
        write(emu, 0xFF55, 0x05);
        assert_eq!(emu.peek(0xFF55), 0x85);
        for _ in 0..2000 {
            assert_eq!(step(emu), 4);
        }
        assert_eq!(emu.mem.vram[16], 0);
    }

    #[test]
    fn an_enable_at_the_hblank_end_waits_for_the_next_trigger() {
        let emu = setup(0x91, 0x80);
        // The first M-cycle of a line where STAT still reads mode 0.
        let ly = loop {
            let ly = emu.peek(0xFF44);
            step(emu);
            if ly != 0 && emu.peek(0xFF44) != ly && emu.peek(0xFF41) & 3 == 0 {
                break emu.peek(0xFF44);
            }
        };
        write(emu, 0xFF55, 0x80);
        // gambatte `dma/hdma_late_enable_{,ds_}2`: no block now ...
        assert_eq!(step(emu), 4);
        assert_eq!(emu.mem.vram[0], 0);
        assert_eq!(emu.peek(0xFF55), 0x00);
        // ... but at this line's trigger.
        step_to_stall(emu);
        assert_eq!(emu.peek(0xFF44), ly);
        assert_eq!(emu.mem.vram[..16], block(0));
    }

    #[test]
    fn a_block_goes_before_an_interrupt_dispatch_due_with_it() {
        let emu = setup(0x11, 0x80); // LCD off
        // EI, NOP: then the timer interrupt is due.
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7004;
        emu.hw.cpu.set_registers(r);
        step(emu);
        step(emu);
        emu.hw.sys.ie = 0x04;
        emu.hw.sys.iflags = 0x04;
        write(emu, 0xFF55, 0x00); // GDMA, 1 block
        // gambatte `irq_precedence/*hdma_vs_*`: the block (1 + 8 M-cycles),
        // then the dispatch (5), in one step.
        assert_eq!(step(emu), 4 * (9 + 5));
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.hw.cpu.registers().pc, 0x0050);
    }

    #[test]
    fn lcd_off_outside_mode_0_starts_a_block() {
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 3 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81);
        write(emu, 0xFF40, 0x11);
        assert_eq!(step(emu), 4 * 10);
        assert_eq!(emu.mem.vram[..16], block(0));
        // No HBlanks while the LCD is off.
        for _ in 0..2000 {
            assert_eq!(step(emu), 4);
        }
        assert_eq!(emu.peek(0xFF55), 0x00);
    }

    #[test]
    fn halt_skips_hblanks_and_waking_in_mode_0_starts_a_block() {
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81);
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7000;
        emu.hw.cpu.set_registers(r);
        emu.hw.sys.ie = 0x04;
        step(emu); // HALT, in mode 2
        assert!(emu.hw.cpu.halted());
        // HBlanks pass while halted: no block.
        let t0 = emu.clock();
        while emu.clock() < t0 + 3 * 456 {
            step(emu);
        }
        assert_eq!(emu.mem.vram[0], 0);
        // Wake up in mode 0, past this line's trigger.
        loop {
            let m0 = emu.hw.sys.ppu.mode0_start().unwrap();
            let t = emu.clock();
            if emu.peek(0xFF41) & 3 == 0 && t > m0 && t >= m0 + 8 {
                break;
            }
            step(emu);
        }
        assert_eq!(emu.mem.vram[0], 0);
        emu.hw.sys.iflags = 0x04;
        for _ in 0..3 {
            step(emu);
        }
        assert!(!emu.hw.cpu.halted());
        assert_eq!(emu.mem.vram[..16], block(0));
    }

    /// Wakes a CPU halted (IME off) in an HBlank transfer at the next
    /// mode 0 at least 8 dots past its start, in a line whose HBlank
    /// trigger passed in HALT: returns whether the wake-up started a block.
    fn wake_in_next_mode_0(emu: &mut Emulator) -> bool {
        let m0 = emu.hw.sys.ppu.mode0_start().unwrap_or(0);
        loop {
            let s = emu.hw.sys.ppu.mode0_start().unwrap_or(0);
            let t = emu.clock();
            if s > m0 && emu.peek(0xFF41) & 3 == 0 && (s + 8..s + 40).contains(&t) {
                break;
            }
            step(emu);
        }
        let before = emu.peek(0xFF55);
        emu.hw.sys.iflags = 0x04;
        for _ in 0..3 {
            step(emu);
        }
        assert!(!emu.hw.cpu.halted());
        emu.peek(0xFF55) != before
    }

    #[test]
    fn a_wake_up_block_runs_before_the_next_instruction() {
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81);
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7000;
        emu.hw.cpu.set_registers(r);
        emu.hw.sys.ie = 0x04;
        step(emu); // HALT, in mode 2
        assert!(wake_in_next_mode_0(emu));
        // LD A,(8000) after the HALT ran after the block (SameBoy: fetch,
        // `GB_hdma_run`, execute), so it reads the copied byte.
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.hw.cpu.registers().a, block(0)[0]);
    }

    #[test]
    fn halt_after_the_mode_0_stat_change_but_before_the_trigger_counts_as_before_it() {
        // A HALT whose fetch M-cycle ends at the mode-0 STAT change or up
        // to 2 dots after it: the SCX fine scroll moves the mode-3 end
        // against the CPU's 4-dot grid until one does.
        let mut tested = false;
        for scx in 0..4 {
            let emu = setup(0x91, 0x80);
            write(emu, 0xFF43, scx);
            while emu.peek(0xFF41) & 3 != 2 {
                step(emu);
            }
            write(emu, 0xFF55, 0x81);
            // This line's mode-0 start, then the next line's.
            while emu.peek(0xFF41) & 3 != 0 {
                step(emu);
            }
            let m0 = emu.hw.sys.ppu.mode0_start().unwrap() + 456;
            while emu.clock() + 4 < m0 {
                step(emu);
            }
            let end = emu.clock() + 4;
            if end >= m0 + HBLANK_DELAY {
                continue;
            }
            // This line's block already ran (the trigger came before).
            assert_eq!(emu.mem.vram[0], block(0)[0]);
            let mut r = emu.hw.cpu.registers();
            r.pc = 0x7000;
            emu.hw.cpu.set_registers(r);
            emu.hw.sys.ie = 0x04;
            step(emu); // HALT
            assert!(emu.hw.cpu.halted());
            assert_eq!(emu.hw.sys.ppu.mode0_start(), Some(m0));
            assert_eq!(emu.peek(0xFF41) & 3, 0, "scx {scx}");
            // The trigger fell into the HALT: not this line's block, but
            // the wake-up's (gambatte `dma/hdma_late_m3halt_m0unhalt_*`).
            assert!(wake_in_next_mode_0(emu), "scx {scx}");
            tested = true;
        }
        assert!(tested);
    }

    #[test]
    fn hblanks_missed_in_halt_start_nothing_on_a_wake_outside_mode_0() {
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81);
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7000;
        emu.hw.cpu.set_registers(r);
        emu.hw.sys.ie = 0x04;
        let t0 = emu.clock();
        while emu.clock() < t0 + 3 * 456 || emu.peek(0xFF41) & 3 != 3 {
            step(emu);
        }
        emu.hw.sys.iflags = 0x04;
        for _ in 0..3 {
            step(emu);
        }
        assert!(!emu.hw.cpu.halted());
        assert_eq!(emu.mem.vram[0], 0);
        assert_eq!(emu.peek(0xFF55), 0x01);
    }

    /// Sets PC to the STOP at 7100 with the speed switch armed (KEY1 bit 0,
    /// no P1 line selected, IE = 0) and A = 1.
    fn arm_stop(emu: &mut Emulator) {
        write(emu, 0xFF00, 0x30);
        write(emu, 0xFF4D, 0x01);
        emu.hw.sys.ie = 0;
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7100;
        r.a = 1;
        emu.hw.cpu.set_registers(r);
    }

    /// Steps until A reads `a`: the length of that step.
    fn step_until_a(emu: &mut Emulator, a: u8) -> u64 {
        for _ in 0..200_000 {
            let d = step(emu);
            if emu.hw.cpu.registers().a == a {
                return d;
            }
        }
        panic!("A never {a}");
    }

    /// Steps until PC leaves `pc`: the length of that step.
    fn step_until_pc_moves(emu: &mut Emulator, pc: u16) -> u64 {
        for _ in 0..200_000 {
            let d = step(emu);
            if emu.hw.cpu.registers().pc != pc {
                return d;
            }
        }
        panic!("pc stuck");
    }

    /// Starts an HBlank transfer of `len` (FF55 bits 0-6) in mode 2 and
    /// steps NOPs until that line's HBlank trigger falls in STOP's first
    /// M-cycle, then runs the STOP (a speed switch, nothing pending).
    fn stop_on_trigger(emu: &mut Emulator, len: u8) {
        // A mode-0 start, then the next line's (456 dots on).
        while emu.peek(0xFF41) & 3 != 0
            || !(1..=140).contains(&emu.peek(0xFF44))
            || emu.hw.sys.ppu.mode0_start().is_none()
        {
            step(emu);
        }
        let t = emu.hw.sys.ppu.mode0_start().unwrap() + 456 + super::HBLANK_DELAY;
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x80 | len);
        let m = emu.hw.sys.hot.mcycle as u64;
        while emu.clock() + m < t {
            step(emu);
        }
        assert!(emu.clock() < t);
        arm_stop(emu);
        step(emu);
    }

    #[test]
    fn a_block_due_in_stops_first_mcycle_runs_in_the_halt_to_double_speed() {
        // gambatte CGB C `dma/hdma_late_m3speedchange_{hdma5,inc,
        // read_hdmadst00}_scx1_2`, `hdma_transition_speedchange_hdmalen*`:
        // the block runs in the halt and ends the transfer (FF55 bit 7
        // set, length kept), and STOP's second byte runs as an opcode.
        let emu = setup(0x91, 0x80);
        stop_on_trigger(emu, 0x01);
        assert_eq!(emu.hw.sys.hot.mcycle, 2);
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.peek(0xFF55), 0x81);
        // INC A, latched at the STOP, runs with PC unchanged.
        step_until_a(emu, 2);
        assert_eq!(emu.hw.cpu.registers().pc, 0x7102);
        // Ended: no block on the next lines.
        let t0 = emu.clock();
        while emu.clock() < t0 + 3 * 456 {
            assert_eq!(step(emu), 2);
        }
        assert_eq!(emu.mem.vram[16], 0);
    }

    #[test]
    fn a_block_due_in_stops_first_mcycle_runs_at_the_wake_up_to_single_speed() {
        // gambatte CGB C `dma/hdma_late_m3speedchange_*_ds_2`,
        // `hdma_transition_speedchange_ldaaimm_scx1_ds`: the block runs
        // at the wake-up, before STOP's second byte runs as an opcode, and
        // its stall is one M-cycle shorter (`tima_scx1_ds_{3,4}`).
        let emu = setup(0x91, 0x80);
        // To double speed first (no transfer active).
        arm_stop(emu);
        step(emu);
        step_until_pc_moves(emu, 0x7102);
        assert_eq!(emu.hw.sys.hot.mcycle, 2);
        stop_on_trigger(emu, 0x00);
        assert_eq!(emu.hw.sys.hot.mcycle, 4);
        assert_eq!(emu.mem.vram[0], 0);
        assert_eq!(emu.peek(0xFF55), 0x00);
        let d = step_until_a(emu, 2);
        assert_eq!(emu.hw.cpu.registers().pc, 0x7102);
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.peek(0xFF55), 0xFF);
        // The stall (1 + 8 M-cycles, less one), then INC A.
        assert_eq!(d, 4 * 8 + 4);
    }

    /// Steps the speed-switch halt until `pred`, then ends it at the next
    /// M-cycle boundary at or after `at(clock)`.
    fn end_switch_halt(emu: &mut Emulator, at: impl Fn(&Emulator) -> Option<u64>) {
        for _ in 0..200_000 {
            if let Some(w) = at(emu) {
                emu.hw.sys.vdma.switch_end = w as u32;
                return;
            }
            step(emu);
        }
        panic!("no wake-up point");
    }

    #[test]
    fn the_speed_switch_countdown_wakes_into_a_block_in_the_hblank() {
        // gambatte `intevent_unhalt` (CGB C capture
        // `dma/hdma_m3speedchange_late_m0wakeup_1`): a switch begun
        // outside the HBlank whose countdown ends in it starts a block.
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x80);
        arm_stop(emu);
        step(emu);
        end_switch_halt(emu, |e| {
            let m0 = e.hw.sys.ppu.mode0_start()?;
            let c = e.clock();
            (e.peek(0xFF41) & 3 == 0 && c > m0 + 40).then_some(c)
        });
        for _ in 0..3 {
            step(emu);
        }
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.peek(0xFF55), 0xFF);
    }

    #[test]
    fn an_hblank_in_the_speed_switch_wake_up_mcycle_starts_no_block() {
        // SameBoy clears `halted` after the wake-up M-cycle (CGB C capture
        // `dma/hdma_m0speedchange_late_m3wakeup_scx2_2`): a trigger inside
        // it is lost, as in the halt.
        let emu = setup(0x91, 0x80);
        // Begun in the HBlank (no wake-up block): 2 blocks, the first now.
        while emu.peek(0xFF41) & 3 != 2 || emu.peek(0xFF44) == 0 {
            step(emu);
        }
        write(emu, 0xFF55, 0x81);
        step_to_stall(emu);
        assert_eq!(emu.mem.vram[..16], block(0));
        arm_stop(emu);
        step(emu);
        assert_eq!(emu.hw.sys.hot.mcycle, 2);
        // Wake up 1 clock before a trigger (double speed: it falls in the
        // wake-up M-cycle).
        end_switch_halt(emu, |e| {
            let t = e.hw.sys.ppu.mode0_start()? + 456 + super::HBLANK_DELAY;
            let c = e.clock();
            (t - 1 > c + 8 && t - 1 < c + 16 && (t - 1 - c).is_multiple_of(2)).then_some(t - 1)
        });
        let t0 = emu.clock();
        while emu.clock() < t0 + 100 {
            step(emu);
        }
        assert_eq!(emu.mem.vram[16], 0);
        assert_eq!(emu.peek(0xFF55), 0x00);
    }

    #[test]
    fn a_block_requested_in_halts_mcycle_makes_the_next_byte_run_twice() {
        // gambatte `Memory::halt` (`prefetched`; CGB C captures
        // `dma/hdma_late_m3halt_m2unhalt_inc_scx{1,2}_2`): the byte after
        // HALT, latched at the HALT, runs after the wake-up and the block
        // with PC unchanged, so INC A runs twice.
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 0
            || !(1..=140).contains(&emu.peek(0xFF44))
            || emu.hw.sys.ppu.mode0_start().is_none()
        {
            step(emu);
        }
        let t = emu.hw.sys.ppu.mode0_start().unwrap() + 456 + super::HBLANK_DELAY;
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x80);
        while emu.clock() + 4 < t {
            step(emu);
        }
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x6000;
        r.a = 1;
        emu.hw.cpu.set_registers(r);
        emu.hw.sys.ie = 0x04;
        emu.hw.sys.iflags = 0;
        step(emu);
        assert!(emu.hw.cpu.halted());
        assert_eq!(emu.mem.vram[0], 0);
        for _ in 0..50 {
            step(emu);
        }
        emu.hw.sys.iflags = 0x04;
        while emu.hw.cpu.registers().pc < 0x6003 {
            step(emu);
        }
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.hw.cpu.registers().a, 3);
    }

    #[test]
    fn the_opcode_after_a_block_is_fetched_before_it() {
        // SameBoy `GB_cpu_run` and gambatte (`Interrupter::prefetch`)
        // fetch the next opcode before the block (CGB C capture
        // `dma/hdma_pc_7ffe`): INC A at 8000 runs, not the byte the block
        // writes there.
        let emu = setup(0x91, 0x80);
        while emu.peek(0xFF41) & 3 != 0
            || emu.peek(0xFF44) == 0
            || !(8..100).contains(&(emu.clock() - emu.hw.sys.ppu.mode0_start().unwrap()))
        {
            step(emu);
        }
        emu.mem.vram[0] = 0x3C;
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x7FFE;
        r.a = 0x80;
        emu.hw.cpu.set_registers(r);
        step(emu); // LDH (55),A: an HBlank transfer, in mode 0
        assert_eq!(step(emu), 4 * 9 + 4); // the block, then INC A
        assert_eq!(emu.mem.vram[..16], block(0));
        assert_eq!(emu.hw.cpu.registers().a, 0x81);
        assert_eq!(emu.hw.cpu.registers().pc, 0x8001);
    }

    /// 16-byte blocks copied since `setup`.
    fn copied(emu: &Emulator) -> u16 {
        (emu.hw.sys.vdma.src - 0xC100) / 16
    }

    /// Starts an HBlank transfer of 4 blocks in mode 2, then runs `LDH
    /// (55),A` with A = `a` at the first instruction boundary within
    /// `lead` clocks of the next line's HBlank trigger (after it if
    /// negative), and one NOP: whether this line's block ran before the
    /// instruction, and the length of the NOP's step.
    fn ldh_hdma5_near_trigger(
        scx: u8,
        lead: i64,
        a: u8,
    ) -> (&'static mut Emulator<'static>, bool, u64) {
        let emu = setup(0x91, 0x80);
        write(emu, 0xFF43, scx);
        while emu.peek(0xFF41) & 3 != 0
            || !(1..=140).contains(&emu.peek(0xFF44))
            || emu.hw.sys.ppu.mode0_start().is_none()
        {
            step(emu);
        }
        let t = emu.hw.sys.ppu.mode0_start().unwrap() + 456 + HBLANK_DELAY;
        while emu.peek(0xFF41) & 3 != 2 {
            step(emu);
        }
        write(emu, 0xFF55, 0x83);
        while emu.clock() as i64 + lead < t as i64 {
            step(emu);
        }
        let mut r = emu.hw.cpu.registers();
        r.pc = 0x6100;
        r.a = a;
        emu.hw.cpu.set_registers(r);
        // At a trigger in the M-cycle before it, the block runs first.
        let ran = copied(emu) == 1;
        let ran = (step(emu) == 4 * (9 + 3)) || ran;
        let nop = step(emu);
        (emu, ran, nop)
    }

    #[test]
    fn a_cancel_after_the_trigger_copies_only_that_block() {
        // The HBlank trigger falls inside the `LDH (55),A` that cancels the
        // transfer, before its write: the block already requested still
        // runs, as one block and not as a general-purpose transfer of the
        // written length (gambatte `Memory::dma`: a flagged HBlank request
        // is always 16 bytes; CGB C captures `dma/hdma_late_disable_*_2`
        // for a written length of 0). FF55 then reads the written length
        // less that block, bit 7 set.
        let (mut before, mut raced, mut after) = (0, 0, 0);
        for a in [0x7F, 0x00, 0x05] {
            for scx in 0..4 {
                for lead in (-8..=20).step_by(4) {
                    let (emu, ran, nop) = ldh_hdma5_near_trigger(scx, lead, a);
                    let n = copied(emu);
                    let at = std::format!("a {a:02X} scx {scx} lead {lead}");
                    assert!(n <= 1, "{at}: {n} blocks");
                    let ff55 = emu.peek(0xFF55);
                    if ran {
                        // The block ran before the instruction.
                        assert_eq!((n, nop, ff55), (1, 4, 0x80 | a), "{at}");
                        after += 1;
                    } else if n == 0 {
                        // Cancelled before the trigger: no block.
                        assert_eq!((nop, ff55), (4, 0x80 | a), "{at}");
                        before += 1;
                    } else {
                        // Triggered, then cancelled, in one instruction.
                        let left = 0x80 | (a.wrapping_sub(1) & 0x7F);
                        assert_eq!((nop, ff55), (4 * 10, left), "{at}");
                        assert_eq!(emu.mem.vram[..16], block(0), "{at}");
                        raced += 1;
                    }
                    assert_eq!(emu.mem.vram[16 * usize::from(n)], 0, "{at}");
                    // Stopped: no block on the next lines.
                    for _ in 0..500 {
                        assert_eq!(step(emu), 4, "{at}");
                    }
                    assert_eq!((copied(emu), emu.peek(0xFF55)), (n, ff55), "{at}");
                }
            }
        }
        assert!(
            before > 0 && raced > 0 && after > 0,
            "{before} {raced} {after}"
        );
    }

    #[test]
    fn a_cancel_with_the_lcd_off_stops_it_and_a_gdma_can_follow() {
        let emu = setup(0x11, 0x80); // LCD off
        write(emu, 0xFF55, 0x83); // 4 blocks: one at once, no HBlanks after
        assert_eq!(step(emu), 4 * 10);
        assert_eq!(emu.peek(0xFF55), 0x02);
        write(emu, 0xFF55, 0x05);
        assert_eq!(emu.peek(0xFF55), 0x85);
        for _ in 0..500 {
            assert_eq!(step(emu), 4);
        }
        assert_eq!(copied(emu), 1);
        // Bit 7 = 0 with no HBlank transfer active: a general-purpose
        // transfer, from where the counters stopped.
        write(emu, 0xFF55, 0x01);
        assert_eq!(emu.peek(0xFF55), 0x01);
        assert_eq!(step(emu), 4 * 18);
        assert_eq!(emu.mem.vram[16..32], block(1));
        assert_eq!(emu.mem.vram[32..48], block(2));
        assert_eq!(emu.mem.vram[48], 0);
        assert_eq!(emu.peek(0xFF55), 0xFF);
    }
}
