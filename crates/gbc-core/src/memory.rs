//! Large internal RAM arrays.

use crate::ppu::OBJ_BUF_LEN;

/// The console's large RAM blocks (WRAM, VRAM, OAM, HRAM, the FEA0-FEFF
/// area and the I/O register backing store), about 49 KiB, provided by the
/// caller and borrowed by [`Emulator`](crate::Emulator) (ADR 0009).
///
/// [`Memory::new`] is a `const fn` whose value is all zero bytes, so a
/// `static` initialized with it goes to `.bss` on the MCU: no image in
/// flash, no copy at boot, nothing on the stack. On a PC, use
/// `Box::new(Memory::new())`. The contents never need initializing by the
/// caller: [`Emulator::new`](crate::Emulator::new) zero-fills them and
/// `load`/`reset` write the post-boot values.
///
/// INVARIANT: every field is an integer array whose power-on value is
/// zero; non-zero power-on values (e.g. post-boot I/O registers) are
/// written by `reset`, not by `new`. Checked by the
/// `memory_new_is_all_zero_bytes` test.
#[cfg_attr(test, derive(PartialEq))]
pub struct Memory {
    /// 8 banks of 4 KiB (CGB). Bank 0 at C000, banks 1-7 at D000 (SVBK).
    pub(crate) wram: [u8; 0x8000],
    /// 2 banks of 8 KiB (CGB, VBK).
    pub(crate) vram: [u8; 0x4000],
    pub(crate) oam: [u8; 0xA0],
    /// OAM as it was when the last OAM DMA transfer started (FF46 write):
    /// what the PPU reads of a byte the transfer has not written yet
    /// (`dma::OamDma::ppu_read`).
    pub(crate) oam_dma_old: [u8; 0xA0],
    pub(crate) hram: [u8; 0x7F],
    /// FEA0-FEFF: on CGB revisions 0-C an extra RAM area with address
    /// bits 3-4 ignored (Pan Docs: "FEA0-FEFF range"; SameBoy
    /// `GB_read_oam`, `addr &= ~0x18` for CGB_C).
    pub(crate) extra_oam: [u8; 0x60],
    /// Backing store for I/O registers (FF00-FF7F) that are plain
    /// read/write storage until their component exists (sound until M6,
    /// most PPU and CGB palette registers until M3/M5). Indexed by the
    /// low address byte; registers with behavior live in their component.
    pub(crate) io: [u8; 0x80],
    /// Object pixels of the current line (`ppu::render::Renderer`): screen x + 8;
    /// 0 = transparent, else bits 0-1 color, bits 2-4 palette, bit 7 BG
    /// priority. Per-line scratch lives here, not in `Hw` (ADR 0009).
    pub(crate) obj_buf: [u8; OBJ_BUF_LEN],
}

impl Memory {
    /// All zero bytes. Meant for `static` initializers (`.bss`) and
    /// `Box::new(Memory::new())`; at run time in an unoptimized build the
    /// value may be built on the stack first, which is fine on a PC but
    /// not on the MCU. There is deliberately no `Default` impl.
    #[allow(clippy::new_without_default)] // Deliberate: see above.
    pub const fn new() -> Self {
        Self {
            wram: [0; 0x8000],
            vram: [0; 0x4000],
            oam: [0; 0xA0],
            oam_dma_old: [0; 0xA0],
            hram: [0; 0x7F],
            extra_oam: [0; 0x60],
            io: [0; 0x80],
            obj_buf: [0; OBJ_BUF_LEN],
        }
    }

    /// Zero-fills in place (no large temporary on the stack), giving the
    /// same value as [`Memory::new`]. Power-on RAM contents are not random
    /// here: the core is deterministic.
    pub(crate) fn clear(&mut self) {
        self.wram.fill(0);
        self.vram.fill(0);
        self.oam.fill(0);
        self.oam_dma_old.fill(0);
        self.hram.fill(0);
        self.extra_oam.fill(0);
        self.io.fill(0);
        self.obj_buf.fill(0);
    }
}

#[cfg(test)]
mod tests {
    use super::Memory;

    #[test]
    fn memory_new_is_all_zero_bytes() {
        // Evaluated at compile time, as a `static` on the MCU would be.
        static NEW: Memory = Memory::new();
        // The exhaustive pattern stops compiling when a field is added, so
        // each field must be listed (and checked) here.
        let Memory {
            wram,
            vram,
            oam,
            oam_dma_old,
            hram,
            extra_oam,
            io,
            obj_buf,
        } = &NEW;
        let fields: [&[u8]; 8] = [wram, vram, oam, oam_dma_old, hram, extra_oam, io, obj_buf];
        assert!(fields.iter().all(|f| f.iter().all(|&b| b == 0)));
        // The fields cover every byte: no padding, nothing else.
        let total: usize = fields.iter().map(|f| f.len()).sum();
        assert_eq!(size_of::<Memory>(), total);
    }
}
