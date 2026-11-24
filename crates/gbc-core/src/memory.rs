//! Large internal RAM arrays.

use crate::apu::Apu;
use crate::dma::ReadClears;
use crate::ppu::ObjBuf;

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
    /// The renderer's object line buffer: the object pixels of the current
    /// line and their drawing-priority ranks ([`ObjBuf`]; ADR 0019). Line
    /// scratch, kept here with the other large arrays (ADR 0009).
    pub(crate) obj_buf: ObjBuf,
    /// CGB BG palette RAM (BCPD, FF69): 8 palettes of 4 colors, RGB555
    /// little-endian, bytes as written (bit 15 included, for read-back).
    /// Pan Docs: LCD Color Palettes (CGB only).
    pub(crate) bg_pal: [u8; 64],
    /// CGB OBJ palette RAM (OCPD, FF6B), same layout.
    pub(crate) obj_pal: [u8; 64],
    /// OAM bytes cleared by conflicting CPU reads during an OAM DMA from
    /// VRAM, not yet applied to `oam` ([`ReadClears`]). Placed here, not in
    /// `Sys`, while `Hw` had a size cap (dropped 2026-11-24). Empty outside a transfer and between run
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
            obj_buf: ObjBuf::NEW,
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
        self.obj_buf.pix.fill(0);
        self.obj_buf.rank.fill(0);
        self.bg_pal.fill(0);
        self.obj_pal.fill(0);
        self.bg_pairs = [[0; 16]; 8];
        self.apu = Apu::new();
        self.dma_read_clears = ReadClears::new();
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::Memory;
    use crate::apu::Apu;
    use crate::ppu::ObjBuf;

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
            &obj_buf.pix,
            &obj_buf.rank,
            bg_pal,
            obj_pal,
        ];
        assert!(fields.iter().all(|f| f.iter().all(|&b| b == 0)));
        assert!(bg_pairs.as_flattened().iter().all(|&w| w == 0));
        assert_eq!(*apu, Apu::new());
        // The fields cover every byte: nothing else, and no padding but
        // the tail to the 8-byte alignment of `apu` (HRAM is 127 bytes).
        let total: usize = fields.iter().map(|f| f.len()).sum::<usize>()
            + size_of_val(bg_pairs)
            + size_of::<Apu>()
            + size_of_val(dma_read_clears);
        assert_eq!(
            size_of::<Memory>(),
            total.next_multiple_of(align_of::<Memory>())
        );
    }
    /// `clear` gives `Memory::new()` whatever the `Memory` held: every
    /// field is dirtied first. The exhaustive patterns stop compiling when
    /// a field is added, so each one must be dirtied (and compared) here.
    #[test]
    fn clear_restores_new_from_every_field_dirty() {
        static NEW: Memory = Memory::new();
        let mut m = std::boxed::Box::new(Memory::new());
        {
            let Memory {
                bg_pairs,
                wram,
                vram,
                oam,
                oam_dma_old,
                hram,
                extra_oam,
                io,
                obj_buf,
                bg_pal,
                obj_pal,
                dma_read_clears,
                apu,
            } = &mut *m;
            bg_pairs.as_flattened_mut().fill(0x1234_5678);
            wram.fill(1);
            vram.fill(2);
            oam.fill(3);
            oam_dma_old.fill(4);
            hram.fill(5);
            extra_oam.fill(6);
            io.fill(7);
            let ObjBuf { pix, rank } = obj_buf;
            pix.fill(8);
            rank.fill(9);
            bg_pal.fill(21);
            obj_pal.fill(22);
            dma_read_clears.record(23);
            apu.post_boot();
        }
        m.clear();
        // Which fields `clear` left dirty, by name.
        let Memory {
            bg_pairs,
            wram,
            vram,
            oam,
            oam_dma_old,
            hram,
            extra_oam,
            io,
            obj_buf,
            bg_pal,
            obj_pal,
            dma_read_clears,
            apu,
        } = &*m;
        let same = [
            ("bg_pairs", *bg_pairs == NEW.bg_pairs),
            ("wram", *wram == NEW.wram),
            ("vram", *vram == NEW.vram),
            ("oam", *oam == NEW.oam),
            ("oam_dma_old", *oam_dma_old == NEW.oam_dma_old),
            ("hram", *hram == NEW.hram),
            ("extra_oam", *extra_oam == NEW.extra_oam),
            ("io", *io == NEW.io),
            ("obj_buf.pix", obj_buf.pix == NEW.obj_buf.pix),
            ("obj_buf.rank", obj_buf.rank == NEW.obj_buf.rank),
            ("bg_pal", *bg_pal == NEW.bg_pal),
            ("obj_pal", *obj_pal == NEW.obj_pal),
            ("dma_read_clears", *dma_read_clears == NEW.dma_read_clears),
            ("apu", *apu == NEW.apu),
        ];
        let dirty: std::vec::Vec<_> = same.iter().filter(|f| !f.1).map(|f| f.0).collect();
        assert!(dirty.is_empty(), "left dirty by clear(): {dirty:?}");
        assert!(*m == NEW);
    }
}
