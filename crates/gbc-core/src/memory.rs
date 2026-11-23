//! Large internal RAM arrays.

use crate::apu::Apu;
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
/// INVARIANT: every field is an integer array (or, for `apu`, a struct of
/// integers and flags) whose power-on value is zero; non-zero power-on values (e.g. post-boot I/O registers) are
/// written by `reset`, not by `new`. Checked by the
/// `memory_new_is_all_zero_bytes` test.
// `repr(C)`: `apu` (8-byte aligned) last, so the hot arrays keep the
// offsets they had before it (rustc puts the most aligned field first),
// which the bus and renderer code size depends on (Thumb-2 immediates).
#[cfg_attr(test, derive(PartialEq))]
#[repr(C)]
pub struct Memory {
    /// Per BG palette, the pixel-pair table of its resolved colors
    /// (`ppu::render::pair_table`), kept up to date on palette writes so
    /// the renderer never builds one. Palette 0 in DMG compatibility mode
    /// is BGP's colors.
    pub(crate) bg_pairs: BgPairs,
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
    /// Per `obj_buf` pixel, the drawing-priority rank of the object that
    /// owns it (`ppu::render::Renderer::render`; meaningful only where
    /// `obj_buf` is opaque).
    pub(crate) obj_rank: [u8; OBJ_BUF_LEN],
    /// CGB BG palette RAM (BCPD, FF69): 8 palettes of 4 colors, RGB555
    /// little-endian, bytes as written (bit 15 included, for read-back).
    /// Pan Docs: LCD Color Palettes (CGB only).
    pub(crate) bg_pal: [u8; 64],
    /// CGB OBJ palette RAM (OCPD, FF6B), same layout.
    pub(crate) obj_pal: [u8; 64],
    /// The APU's internal state (ADR 0021 section 2): here, not in `Hw`, as
    /// it is only touched on cold paths. All zero is powered off.
    pub(crate) apu: Apu,
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
            obj_rank: [0; OBJ_BUF_LEN],
            bg_pal: [0; 64],
            obj_pal: [0; 64],
            bg_pairs: BgPairs {
                pal: [[0; 16]; 8],
                hold: [(0, 0, 0); 2],
            },
            apu: Apu::new(),
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
        self.obj_rank.fill(0);
        self.bg_pal.fill(0);
        self.obj_pal.fill(0);
        self.bg_pairs.pal = [[0; 16]; 8];
        self.apu = Apu::new();
    }
}

/// Per BG palette, `ppu::render::pair_table` of its colors (kept here and
/// rebuilt when the palette changes), and the renderer's held fetches.
/// Both reach `Renderer::render` through the one `Sources::pairs` pointer
/// (a second pointer would grow `Ppu::draw` past its code-size budget).
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct BgPairs {
    pub(crate) pal: [[u32; 16]; 8],
    /// Pixels of the current line the fetcher had already fetched with
    /// older map-select (LCDC.3/6) and SCX values when those were written
    /// in mode 3 (`ppu::Ppu::hold_fetch`): `(end, LCDC, SCX)`, the pixels
    /// before `end` drawn with that LCDC's bits 3 and 6 and that SCX. In
    /// `end` order; `[1]` is the last; `[1].0 == 0`: none.
    pub(crate) hold: [(u8, u8, u8); 2],
}

#[cfg(test)]
mod tests {

    use super::Memory;
    use crate::apu::Apu;

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
            obj_rank,
            bg_pal,
            obj_pal,
            bg_pairs,
            apu,
        } = &NEW;
        let fields: [&[u8]; 11] = [
            wram,
            vram,
            oam,
            oam_dma_old,
            hram,
            extra_oam,
            io,
            obj_buf,
            obj_rank,
            bg_pal,
            obj_pal,
        ];
        assert!(fields.iter().all(|f| f.iter().all(|&b| b == 0)));
        assert!(bg_pairs.pal.as_flattened().iter().all(|&w| w == 0));
        assert!(bg_pairs.hold.iter().all(|&h| h == (0, 0, 0)));
        assert_eq!(*apu, Apu::new());
        // The fields cover every byte: nothing else, and no padding but
        // the tail to the 8-byte alignment of `apu` (HRAM is 127 bytes).
        let total: usize = fields.iter().map(|f| f.len()).sum::<usize>()
            + size_of_val(bg_pairs)
            + size_of::<Apu>();
        assert_eq!(
            size_of::<Memory>(),
            total.next_multiple_of(align_of::<Memory>())
        );
    }
}
