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
/// mode 0 at single speed): SameBoy (Core/display.c, `GB_display_run`)
/// sets `hdma_on` 1 + 2 dots after the single-speed mode-0 STAT write,
/// in both speeds.
pub(crate) const HBLANK_DELAY: u64 = 3;

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
    /// The CPU is in HALT while an HBlank transfer is active: HBlanks
    /// don't start blocks (SameBoy: `!gb->halted` in the HBlank trigger).
    pub(crate) halted: bool,
    /// HALT began outside mode 0 (`allow_hdma_on_wake`): waking up in
    /// mode 0 starts a block.
    pub(crate) wake_ok: bool,
    /// Not VRAM DMA state: during the CGB speed-switch halt
    /// (`Ctx::speed_switch`), the low 32 bits of the clock at which its
    /// last M-cycle (the wake-up) starts; the halt is far shorter than
    /// 2^31 clocks, so a wrapping compare is exact. Stale otherwise. Here
    /// because it fits this struct's padding (the `Hw` size budget), next
    /// to the other HALT state.
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
            halted: false,
            wake_ok: false,
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
            // Bit 7 = 0 during an HBlank transfer stops it.
            self.hblank = false;
            return;
        }
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
        // HALT, far enough not to be reached by stepping from 0100.
        rom[0x7000] = 0x76;
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
        while emu.peek(0xFF41) & 3 != 0 || emu.peek(0xFF44) == 0 {
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
}
