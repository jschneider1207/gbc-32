//! Memory map. Skeleton only: the real bus (banking, I/O dispatch, access
//! timing) arrives in M1. For now this provides the debug [`peek`].
//!
//! [`peek`]: crate::Emulator::peek

use crate::Emulator;

impl Emulator<'_> {
    /// Reads a byte as the CPU would see it at `addr`, without side
    /// effects and ignoring PPU access blocking. Debug access for tools and
    /// tests; not used by emulation.
    ///
    /// Skeleton: no banking yet (ROM is read linearly, WRAM bank 1 and VRAM
    /// bank 0 are mapped) and I/O registers other than LCDC read 0xFF.
    /// Reads past the end of a short ROM return 0xFF, as if it were padded
    /// to a full bank ([`MIN_ROM_SIZE`](crate::MIN_ROM_SIZE)).
    pub fn peek(&self, addr: u16) -> u8 {
        let a = usize::from(addr);
        match addr {
            // 0xFF past the end of a short ROM (SameBoy pads with 0xFF).
            0x0000..=0x7FFF => self.hw.cart.rom.get(a).copied().unwrap_or(0xFF),
            0x8000..=0x9FFF => self.mem.vram[a - 0x8000],
            0xA000..=0xBFFF => self.hw.cart.ram().get(a - 0xA000).copied().unwrap_or(0xFF),
            0xC000..=0xDFFF => self.mem.wram[a - 0xC000],
            // Echo RAM mirrors C000-DDFF (Pan Docs: Memory Map).
            0xE000..=0xFDFF => self.mem.wram[a - 0xE000],
            0xFE00..=0xFE9F => self.mem.oam[a - 0xFE00],
            0xFEA0..=0xFEFF => 0xFF,
            0xFF40 => self.hw.ppu.lcdc,
            0xFF00..=0xFF7F => 0xFF,
            0xFF80..=0xFFFE => self.mem.hram[a - 0xFF80],
            0xFFFF => 0x00,
        }
    }
}
