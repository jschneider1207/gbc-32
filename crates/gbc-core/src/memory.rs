//! Large internal RAM arrays.

use crate::apu::Apu;
use crate::dma::ReadClears;
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
    pub(crate) bg_pairs: [[u32; 16]; 8],
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
    pub(crate) obj_rank: ObjRank,
    /// CGB BG palette RAM (BCPD, FF69): 8 palettes of 4 colors, RGB555
    /// little-endian, bytes as written (bit 15 included, for read-back).
    /// Pan Docs: LCD Color Palettes (CGB only).
    pub(crate) bg_pal: [u8; 64],
    /// CGB OBJ palette RAM (OCPD, FF6B), same layout.
    pub(crate) obj_pal: [u8; 64],
    /// OAM bytes cleared by conflicting CPU reads during an OAM DMA from
    /// VRAM, not yet applied to `oam` ([`ReadClears`]). Here, not in `Sys`,
    /// for `Hw`'s size limit. Empty outside a transfer and between run
    /// calls.
    pub(crate) dma_read_clears: ReadClears,
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
            obj_rank: ObjRank {
                rank: [0; OBJ_BUF_LEN],
                hold: [Held {
                    end: 0,
                    lcdc: 0,
                    scx: 0,
                    scy: 0,
                    planes: 0,
                }; 3],
                next: 0,
                sel_latch: (0, 0),
            },
            bg_pal: [0; 64],
            obj_pal: [0; 64],
            bg_pairs: [[0; 16]; 8],
            apu: Apu::new(),
            dma_read_clears: ReadClears::new(),
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
        self.obj_rank.rank.fill(0);
        self.obj_rank.next = 0;
        self.obj_rank.sel_latch = (0, 0);
        self.bg_pal.fill(0);
        self.obj_pal.fill(0);
        self.bg_pairs = [[0; 16]; 8];
        self.apu = Apu::new();
    }
}

/// Per [`Memory::obj_buf`] pixel, the drawing-priority rank of the object
/// that owns it, and the renderer's held fetches. Both reach
/// `Renderer::render` through one pointer: a second one would grow
/// `Ppu::draw` past its code-size budget, and placed here (after the hot
/// arrays in this `repr(C)` struct) the hold moves no offset that the bus
/// or the PPU address with Thumb-2 immediates.
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct ObjRank {
    /// Meaningful only where `obj_buf` is opaque.
    pub(crate) rank: [u8; OBJ_BUF_LEN],
    /// Pixels of the current line the fetcher had already fetched (in
    /// part) with older map-select (LCDC.3/6), tile-data (LCDC.4), SCX and
    /// SCY values when those were written in mode 3 (`ppu::Ppu::hold_fetch`):
    /// the pixels before `end` are drawn with these values. In `end`
    /// order; `[2]` is the last; `[2].end == 0`: none. Three parts: a part whose
    /// planes were read after a write keeps the next part's values even
    /// when a later write lands before its end.
    pub(crate) hold: [Held; 3],
    /// Position in the renderer's object fetch order of the line's first
    /// object not yet fetched or skipped (`Renderer::fetch_objects`); 0 at
    /// the line start (`Ppu::latch`).
    pub(crate) next: u8,
    /// CGB TILE_SEL glitch latch as far as modelled: the byte an LCDC.4
    /// clear's glitched read took on this line and the screen x + 8 of its
    /// tile (0: none; reset at the line start, `Ppu::latch`); objects from
    /// that x on were fetched after it (`Renderer::tile_sel_glitch`).
    pub(crate) sel_latch: (u8, u8),
}

/// One held part of a line ([`ObjRank::hold`]).
#[derive(Clone, Copy, Default)]
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) struct Held {
    pub(crate) end: u8,
    pub(crate) lcdc: u8,
    pub(crate) scx: u8,
    pub(crate) scy: u8,
    /// The bit planes of the part's last tile (`[end - 8, end)`) that the
    /// fetcher read after the write, with the next part's LCDC.4 and SCY:
    /// bit 0 the low plane, bit 1 the high plane (its map entry was read
    /// before; `docs/design/mid-line-ppu.md` step 3). Bits 2 (low) and 3
    /// (high): planes read on an LCDC.4 change's dot (CGB TILE_SEL glitch:
    /// a clear gives the tile index as their data, a set the latch); bits
    /// 4-7 for a set's latch (`Ppu::hold_fetch`).
    pub(crate) planes: u8,
}

#[cfg(test)]
mod tests {

    use super::{Held, Memory};
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
            dma_read_clears,
        } = &NEW;
        assert!(dma_read_clears.is_empty());
        let fields: [&[u8]; 11] = [
            wram,
            vram,
            oam,
            oam_dma_old,
            hram,
            extra_oam,
            io,
            obj_buf,
            &obj_rank.rank,
            bg_pal,
            obj_pal,
        ];
        assert!(fields.iter().all(|f| f.iter().all(|&b| b == 0)));
        assert!(bg_pairs.as_flattened().iter().all(|&w| w == 0));
        assert!(obj_rank.hold.iter().all(|&h| h == Held::default()));
        assert_eq!(obj_rank.next, 0);
        assert_eq!(obj_rank.sel_latch, (0, 0));
        assert_eq!(*apu, Apu::new());
        // The fields cover every byte: nothing else, and no padding but
        // the tail to the 8-byte alignment of `apu` (HRAM is 127 bytes).
        let total: usize = fields.iter().map(|f| f.len()).sum::<usize>()
            + size_of_val(bg_pairs)
            + size_of_val(&obj_rank.hold)
            + size_of_val(&obj_rank.next)
            + size_of_val(&obj_rank.sel_latch)
            + size_of::<Apu>()
            + size_of_val(dma_read_clears);
        assert_eq!(
            size_of::<Memory>(),
            total.next_multiple_of(align_of::<Memory>())
        );
    }
}
