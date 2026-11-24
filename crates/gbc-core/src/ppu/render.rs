//! Scanline renderer (ADR 0002): draws pixels `[x0, x1)` of the current line
//! from VRAM, OAM, a register snapshot and resolved RGB555 color tables
//! (ADR 0013). The timing code calls [`Renderer::render`] once per line at
//! the end of mode 3, and additionally up to the current pixel before a
//! register write that changes the picture, so mid-line effects land on the
//! right pixel. Splitting a line into spans gives exactly the same pixels
//! as one span when no register changes between them (tested below).
//!
//! Scope: DMG compatibility mode on a CGB (M3); in CGB mode (M5) the BG map
//! attributes (palette, tile bank, flips, BG-to-OBJ priority) and LCDC.0 as
//! the master priority. In CGB mode each tile fetch reads its attribute
//! byte and turns it into a palette and a priority mask for the tile's
//! pixels: no per-pixel work is added. Compatibility mode has its own copy
//! of the tile loops with no attribute read (palette 0, bank 0, no flips,
//! no BG priority). CGB-mode objects read their attributes the
//! same way at fetch time (OBJ palette 0-7, VRAM bank) and win pixels by
//! OAM index instead of X (a rank per object-buffer pixel, checked only in
//! the 8-pixel merge of each object fetch).
//!
//! What the caller keeps:
//! - per frame: [`Renderer::start_frame`] at line 0 (resets the window line
//!   counter and the WY condition), [`Renderer::check_wy`] whenever the
//!   hardware compares WY with LY;
//! - per line: [`Renderer::start_line`] at the start of mode 3 (or the end
//!   of the OAM scan) with the line's [`LineObjects`] and SCX, then
//!   [`Renderer::render`] for consecutive spans covering 0..160;
//! - [`Colors`] and the BG pixel-pair tables ([`pair_table`]), updated when
//!   BGP/OBP0/OBP1 or the CGB palette RAM are written (colors are resolved
//!   at palette-write time, ADR 0013).
//!
//! Cost per line: 21 BG/window tile fetches (one map byte and two data
//! bytes each, plus two table lookups); spans without object pixels go
//! straight from the tile bits to RGB555 (one lookup per two pixels). With
//! objects, up to 10 object fetches merge 8 pixels each into the object
//! buffer (one 64-bit merge), and the tiles are mixed with it as they are
//! decoded; a tile with no object pixel (one 8-byte test) takes the plain
//! path. No per-pixel bounds checks (fixed-size arrays, masked indices,
//! zipped iterators). Measured (x86 Ir, `docs/perf.md`): about 1.5k Ir
//! per BG-only line, 2.0k with objects and the window (cgb-acid2), before
//! the CGB attribute decode, which adds about 11 Ir per tile fetch.
//!
//! Open hardware choices and the evidence (Mealybug tearoom CGB C
//! screenshots, stretch tier):
//! - Clearing LCDC.5 doesn't reset the WY condition (SameBoy; Pan Docs
//!   says it does on GBC): resetting it fails `m2_win_en_toggle`, which
//!   passes as is.
//! - LCDC.0 clear keeps the window activation and line counter, color 0
//!   through BGP: `m3_lcdc_bg_en_change`, `m3_lcdc_bg_en_change2` pass.
//! - WX = 0 loses SCX & 7 pixels (Pan Docs). With SCX & 7 != 0 its
//!   window start also stalls one dot more (`LineTiming`, AGE
//!   `stat-mode-window`), which places the mid-line register writes of
//!   `m3_window_timing_wx_0` right: it passes.
//! - Mid-line SCX writes keep the fine scroll once the discard is over
//!   (Pan Docs; a write inside it moves it: `Ppu::hold_fetch`), but new
//!   low bits still carry into the tile column ([`bg_column`]).
//!   `m3_scx_low_3_bits` still differs at pixels 152-159 (unchanged by the
//!   carry at any calibration); unresolved.
//! - Mid-line writes to the tile map/data selects, SCY and object
//!   attributes take effect per pixel here, per fetch on hardware
//!   (`m3_lcdc_*_change`, `m3_scy_change*` fail); M3 doesn't need it.

use super::objects::{LineObjects, MAX_PER_LINE};
use super::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::DmgPalettes;

const W: usize = SCREEN_WIDTH;

// LCDC bits (Pan Docs: LCDC).
// Non-CGB mode: BG and window enable. CGB mode: BG/window master priority.
const LCDC_BG_ON: u8 = 0x01;
const LCDC_OBJ_ON: u8 = 0x02;
const LCDC_OBJ_TALL: u8 = 0x04;
const LCDC_BG_MAP: u8 = 0x08;
const LCDC_TILES_8000: u8 = 0x10;
const LCDC_WIN_ON: u8 = 0x20;
const LCDC_WIN_MAP: u8 = 0x40;

// `Renderer::slow` bits.
const OBJ_DIRTY: u8 = 1;
const ZERO_DUE: u8 = 2;

/// Calibration of the BG tile column (see [`bg_column`]): the fetch of
/// the tile whose first pixel is screen x `X` reads map column
/// `(SCX + X + FETCH_COL) / 8`.
const FETCH_COL: u8 = 0;

/// Map column (pixels) of BG pixel `x` with SCX `scx` and the line-start
/// SCX & 7 `fine`. The fine scroll is applied once, at line start (Pan
/// Docs: Scrolling), but the fetcher computes each tile's column from the
/// whole SCX at its GET_TILE step (SameBoy `advance_fetcher_state_machine`,
/// MIT: `(SCX + position_in_line + 8 - is_cgb) / 8`), and that tile starts
/// at screen x `X = 8k - fine`. So low bits of SCX that differ from `fine`
/// still move the column by a carry or borrow:
/// `((scx + X + FETCH_COL) & !7) + (x - X)`, which is the form below. With
/// `scx & 7 == fine` it is `scx + x`. `docs/design/mid-line-ppu.md` step 2.
///
/// The first tile (`x < 8 - fine`) is fetched at mode-3 start, while the
/// line position is still in the discard range, from column `SCX / 8`
/// with no position term (SameBoy `advance_fetcher_state_machine`:
/// `position_in_line + 16 < 8`), so its low SCX bits never borrow. That
/// differs only for a held SCX below a discard an SCX write moved
/// (`Ppu::hold_fetch`): gambatte `scx_during_m3/scx_0367c0` `_3`, `_ds_3`
/// (and `_2`, `_ds_2` line 0) show screen x 0 from column 0 with SCX 03
/// held and the discard moved to 7 by the 67 write.
#[inline(always)]
const fn bg_column(scx: u8, fine: u8, x: u8) -> u8 {
    // `x` < 160 and `fine` < 8: no wrap in use.
    let scx = if x.wrapping_add(fine) < 8 {
        (scx & 0xF8) | fine
    } else {
        scx
    };
    (scx.wrapping_sub(fine).wrapping_add(FETCH_COL) & 0xF8)
        .wrapping_add(fine)
        .wrapping_add(x)
}

// OAM attribute bits (Pan Docs: OAM, byte 3).
const ATTR_BEHIND_BG: u8 = 0x80;
const ATTR_Y_FLIP: u8 = 0x40;
const ATTR_X_FLIP: u8 = 0x20;
const ATTR_DMG_OBP1: u8 = 0x10;
// CGB mode: tile data in VRAM bank 1 (bits 0-2: OBJ palette).
const CGB_ATTR_BANK: u8 = 0x08;

// BG map attribute bits, CGB mode: the byte at the map entry's address in
// VRAM bank 1 (Pan Docs: VRAM Tile Maps, "BG Map Attributes"). Bits 0-2
// are the BG palette, bit 4 is unused.
const BG_ATTR_PRIORITY: u8 = 0x80;
const BG_ATTR_Y_FLIP: u8 = 0x40;
const BG_ATTR_X_FLIP: u8 = 0x20;
const BG_ATTR_BANK: u8 = 0x08;

/// The object line buffer covers screen x -8..168, so an object at any X
/// below 168 is written without clipping: buffer index = screen x + 8.
pub(crate) const OBJ_BUF_LEN: usize = W + 16;

/// The object line buffer: the object pixels of the current line and,
/// per pixel, the drawing-priority rank of the object that owns it. Line
/// scratch that [`Renderer::render`] fills and reads; the caller keeps it
/// (`Memory::obj_buf`) and passes the same one for every span of a line.
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct ObjBuf {
    /// At screen x + 8: 0 = transparent, else bits 0-1 color (1-3), bits
    /// 2-4 palette, bit 7 the OAM "behind BG" bit.
    pub(crate) pix: [u8; OBJ_BUF_LEN],
    /// Meaningful only where `pix` is opaque.
    pub(crate) rank: [u8; OBJ_BUF_LEN],
}

impl ObjBuf {
    pub(crate) const NEW: Self = Self {
        pix: [0; OBJ_BUF_LEN],
        rank: [0; OBJ_BUF_LEN],
    };
}

/// A set of a tile's two bit planes: bit 0 the low plane, bit 1 the high.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Planes(u8);

impl Planes {
    pub(crate) const NONE: Self = Self(0);
    pub(crate) const HIGH: Self = Self(2);
    pub(crate) const BOTH: Self = Self(3);

    pub(crate) const fn new(low: bool, high: bool) -> Self {
        Self(low as u8 | (high as u8) << 1)
    }

    /// These planes less `other`'s.
    pub(crate) const fn without(self, other: Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// The bits of these planes in a decoded row ([`decode`]: the low
    /// plane in the even bits).
    const fn row_mask(self) -> u16 {
        ((self.0 & 1) as u16 * 0x5555) | ((self.0 >> 1) as u16 * 0xAAAA)
    }
}

/// For a plane glitched by an LCDC.4 set ([`Mixed::glitched`]): what the
/// line's objects say about the TILE_SEL latch at that read
/// (`Ppu::hold_fetch`, [`Renderer::tile_sel_glitch`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SetLatch {
    /// No object stall near the tile: with the latch unknown, the row as
    /// read after the set.
    #[default]
    Plain,
    /// An object stall in tile j or j + 1, before or after the write (in
    /// tile 0: also one among the junk pixels), none of them known to be
    /// fetched before the tile's reads: with the latch unknown, the row
    /// as read before the set.
    NearObject,
    /// The object in this slot was the last fetched before the tile's
    /// reads: its high plane is the latch, unless the latch was read
    /// after its fetch.
    Object(u8),
}

/// The bit planes of a held part's last tile (`[end - 8, end)`) that the
/// fetcher read after the write ([`Held::mixed`]); its map entry was read
/// before (`docs/design/mid-line-ppu.md` step 3):
/// - [`Self::late`]: read after the write, with the next part's LCDC.4
///   and SCY;
/// - [`Self::glitched`]: read on an LCDC.4 change's dot (CGB TILE_SEL
///   glitch): a clear gives the tile index as their data, a set the latch
///   ([`Renderer::tile_sel_glitch`]). Not in `late`;
/// - [`Self::set_latch`]: for a set's glitched plane.
///
/// One byte (bits 0-1 `late`, 2-3 `glitched`, 4-7 `set_latch` as slot + 1,
/// 15 or 0), built and read only through the functions here. Packed so a
/// [`Held`] is 5 bytes: every SCX, SCY or LCDC write in mode 3 builds one
/// and shuffles the three held parts, and with a field per part that
/// measured 21 x86 Ir more per write (+0.11% on `synth:cgb-raster`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mixed(u8);

impl Mixed {
    /// Both planes as the map entry's fetch read them: no redraw.
    pub(crate) const NONE: Self = Self(0);

    const NEAR_OBJECT: u8 = 15;

    pub(crate) const fn new(late: Planes, glitched: Planes, set_latch: SetLatch) -> Self {
        let latch = match set_latch {
            SetLatch::Plain => 0,
            SetLatch::NearObject => Self::NEAR_OBJECT,
            // Slots are 0-9.
            SetLatch::Object(slot) => (slot & 0xF) + 1,
        };
        Self(late.0 | glitched.0 << 2 | latch << 4)
    }

    pub(crate) const fn late(self) -> Planes {
        Planes(self.0 & 3)
    }

    pub(crate) const fn glitched(self) -> Planes {
        Planes(self.0 >> 2 & 3)
    }

    pub(crate) const fn set_latch(self) -> SetLatch {
        match self.0 >> 4 {
            0 => SetLatch::Plain,
            Self::NEAR_OBJECT => SetLatch::NearObject,
            n => SetLatch::Object(n - 1),
        }
    }

    /// No plane was read after the write.
    pub(crate) const fn is_none(self) -> bool {
        self.0 & 0xF == 0
    }
}

/// One held part of a line ([`Renderer::push_hold`]): the pixels before
/// `end` are drawn with the LCDC, SCX and SCY the fetcher read for them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Held {
    pub(crate) end: u8,
    pub(crate) lcdc: u8,
    pub(crate) scx: u8,
    pub(crate) scy: u8,
    /// The planes of the part's last tile read after the write
    /// ([`Renderer::mix_planes`] redraws them).
    pub(crate) mixed: Mixed,
}

impl Held {
    const NONE: Self = Self {
        end: 0,
        lcdc: 0,
        scx: 0,
        scy: 0,
        mixed: Mixed::NONE,
    };
}

/// When the byte in the CGB TILE_SEL glitch latch was read, relative to
/// the object fetches of the current line ([`SelLatch`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum SelRead {
    /// Not known (power-on): any object fetch replaces it.
    #[default]
    Unknown,
    /// Before the fetches of the objects with OAM X + 8 >= this (those
    /// replace it) and after the others': the BG or window read of the
    /// tile at screen x this - 8 on this line, the last with 8000
    /// addressing before an LCDC.4 clear. 1: before all of this line's
    /// objects (a read on an earlier line, or left of the screen).
    Before(u8),
    /// BG reads with 8000 addressing since: not known, and after every
    /// object of the line.
    BgSince,
}

/// The CGB TILE_SEL glitch latch as far as modelled: the last byte the PPU
/// read with 8000-8FFF addressing, which a plane read on an LCDC.4 set's
/// dot takes as its data ([`Renderer::tile_sel_glitch`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SelLatch {
    byte: u8,
    read: SelRead,
}

impl SelLatch {
    const UNKNOWN: Self = Self {
        byte: 0,
        read: SelRead::Unknown,
    };

    /// `byte` is what the latch holds.
    const fn known(self) -> bool {
        matches!(self.read, SelRead::Before(_))
    }

    /// The object at OAM X `x` of the current line was fetched after the
    /// latched read (or the read is not known and its fetch stands in):
    /// its high plane replaces the latch.
    const fn before_object(self, x: u8) -> bool {
        match self.read {
            SelRead::Unknown => true,
            SelRead::Before(key) => x as u16 + 8 >= key as u16,
            SelRead::BgSince => false,
        }
    }
}

/// The PPU registers the renderer reads, as of the span being drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Regs {
    pub(crate) lcdc: u8,
    pub(crate) scy: u8,
    pub(crate) scx: u8,
    pub(crate) wy: u8,
    pub(crate) wx: u8,
}

/// Resolved RGB555 colors: 8 BG and 8 OBJ palettes of 4 entries (the CGB
/// palette RAM layout). DMG compatibility mode uses BG palette 0 and OBJ
/// palettes 0 and 1, as the hardware does: the CGB boot ROM loads the
/// compatibility colors into those palettes and BGP/OBP0/OBP1 index them
/// (Pan Docs: CGB Registers, "Compatibility palettes"; SameBoy
/// `render_pixel_if_possible` maps through BGP/OBPx, then the CGB palette).
/// Here the BGP/OBPx indirection is folded in when the register is
/// written, so the pixel loop does one lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Colors {
    pub(crate) bg: [[u16; 4]; 8],
    pub(crate) obj: [[u16; 4]; 8],
}

/// `base[shade of color i]` for each color i, where a DMG palette register
/// holds the shade of color i in bits 2i+1..2i (Pan Docs: BGP).
pub(crate) const fn resolve_dmg(base: &[u16; 4], reg: u8) -> [u16; 4] {
    [
        base[(reg & 3) as usize],
        base[((reg >> 2) & 3) as usize],
        base[((reg >> 4) & 3) as usize],
        base[((reg >> 6) & 3) as usize],
    ]
}

impl Colors {
    pub(crate) const fn new() -> Self {
        Self {
            bg: [[0; 4]; 8],
            obj: [[0; 4]; 8],
        }
    }

    /// All three compatibility palettes from the register values.
    pub(crate) const fn dmg_compat(dmg: &DmgPalettes, bgp: u8, obp0: u8, obp1: u8) -> Self {
        let mut c = Self::new();
        c.bg[0] = resolve_dmg(&dmg.bg, bgp);
        c.obj[0] = resolve_dmg(&dmg.obj0, obp0);
        c.obj[1] = resolve_dmg(&dmg.obj1, obp1);
        c
    }

    /// CGB mode: all 8 BG palettes from BG palette RAM `bg` (BCPD) and all
    /// 8 OBJ palettes from OBJ palette RAM `obj` (OCPD). Colors are
    /// little-endian RGB555, the frame's format (ADR 0013); bit 15 is not
    /// a color bit (Pan Docs: LCD Color Palettes, "Palette data").
    pub(crate) fn set_cgb(&mut self, bg: &[u8; 64], obj: &[u8; 64]) {
        // A loop of calls: reset-time only, and a plain conversion loop
        // gets unrolled and vectorized into ~650 bytes (code size).
        for i in 0..32 {
            self.bg[i / 4][i % 4] = cgb_color(bg, 2 * i);
            self.obj[i / 4][i % 4] = cgb_color(obj, 2 * i);
        }
    }

    /// A BGP write in compatibility mode.
    pub(crate) fn set_bgp(&mut self, dmg: &DmgPalettes, bgp: u8) {
        self.bg[0] = resolve_dmg(&dmg.bg, bgp);
    }

    /// An OBP0 (`which` = 0) or OBP1 (`which` = 1) write in compatibility
    /// mode.
    pub(crate) fn set_obp(&mut self, dmg: &DmgPalettes, which: usize, value: u8) {
        if which & 1 == 0 {
            self.obj[0] = resolve_dmg(&dmg.obj0, value);
        } else {
            self.obj[1] = resolve_dmg(&dmg.obj1, value);
        }
    }
}

/// The color at byte `i` (even) of CGB palette RAM `ram`: little-endian
/// RGB555, bit 15 not a color bit (Pan Docs: LCD Color Palettes).
#[inline(never)]
pub(crate) fn cgb_color(ram: &[u8; 64], i: usize) -> u16 {
    let i = i & 0x3E;
    u16::from_le_bytes([ram[i], ram[i + 1]]) & 0x7FFF
}

/// Memory the renderer reads.
#[derive(Clone, Copy)]
pub(crate) struct Sources<'a> {
    /// Both VRAM banks; compatibility mode only reads bank 0.
    pub(crate) vram: &'a [u8; 0x4000],
    pub(crate) oam: &'a [u8; 0xA0],
    /// Per slot, the tile index and attributes the objects' fetches read
    /// on a line OAM DMA touches (`Ppu::fix_attrs`); else they read `oam`.
    pub(crate) attrs: Option<&'a [(u8, u8); MAX_PER_LINE]>,
    pub(crate) colors: &'a Colors,
    /// Per BG palette, [`pair_table`] of `colors.bg` (kept in `Memory`).
    pub(crate) pairs: &'a [[u32; 16]; 8],
}

/// Row `ly` (0-143) of a frame, for [`Renderer::render`].
pub(crate) fn frame_row(frame: &mut Frame, ly: usize) -> &mut [u16; W] {
    &mut frame.as_chunks_mut::<W>().0[ly % SCREEN_HEIGHT]
}

/// Renderer state that lives across spans and lines.
#[derive(Clone, Debug)]
pub(crate) struct Renderer {
    // Per frame.
    /// The window's internal line counter: the window map row the next
    /// window activation draws (Pan Docs: Window, "active Window row").
    window_line: u8,
    /// The WY condition: WY matched LY at some point this frame.
    wy_triggered: bool,

    // Per line.
    ly: u8,
    /// The BG tiles' screen phase: they start at x = 8k - `bg_fine`. The
    /// fine scroll (pixels discarded at the start of mode 3, mod 8: SCX & 7
    /// at the line start, or as an SCX write inside the discard moved it,
    /// `set_fine`; later writes keep it, Pan Docs: Scrolling) until the
    /// window ends on the line: the fetcher then fetches BG tiles from
    /// where it is, so they continue the window's tile grid (Mealybug
    /// `m3_lcdc_win_en_change_multiple`; the tile column is still
    /// `(SCX + x) / 8`, SameBoy `advance_fetcher_state_machine`).
    bg_fine: u8,
    /// The window has been activated on this line and not switched off.
    window_active: bool,
    /// Window map row (pixels) this line draws.
    window_row: u8,
    /// WX + 1 for the WX at which the window starts on this line, as the
    /// line's timing decided it (with the stalls: `Ppu::latch`,
    /// `Ppu::window_write`); 0: none (so a reset is all zeros). The
    /// renderer follows that decision, not the live WY condition, LCDC.5
    /// and WX, so the pixels and the window line counter agree with mode
    /// 3's length (SameBoy display.c sets `wx_triggered` and `window_y++`
    /// at the trigger dot; later writes don't undo them).
    win_wx: u8,
    /// Window map column (pixels) at [`Renderer::window_start`].
    window_skip: u8,
    pub(crate) objects: LineObjects,
    /// Slots of `objects` from highest to lowest drawing priority.
    pub(crate) order: [u8; MAX_PER_LINE],
    /// Work for [`Renderer::render`] before its spans, one byte so the
    /// common case is one test: `OBJ_DIRTY`, the object buffer
    /// ([`ObjBuf`]) still holds the previous line's objects (cleared by
    /// the next `render`); `ZERO_DUE`, a window zero pixel is due at
    /// `win_zero` ([`Renderer::render_zero`]; dropped at the frame start).
    slow: u8,
    /// An object was fetched into the object buffer on this line (else it
    /// is all transparent).
    obj_fetched: bool,
    /// Position in the fetch order (`order`) of the line's first object
    /// not yet fetched or skipped ([`Renderer::fetch_objects`]).
    next_obj: u8,
    /// Pixels of the current line the fetcher had already fetched (in
    /// part) with older map-select (LCDC.3/6), tile-data (LCDC.4), SCX and
    /// SCY values when those were written in mode 3 (`Ppu::hold_fetch`):
    /// the pixels before `end` are drawn with these values. In `end`
    /// order; `[2]` is the last; `[2].end == 0`: none. Three parts: a part
    /// whose planes were read after a write keeps the next part's values
    /// even when a later write lands before its end.
    hold: [Held; 3],
    /// The CGB TILE_SEL glitch latch now (`Ppu::tile_sel_latch`,
    /// [`Renderer::carry_sel`]). Kept across lines.
    sel: SelLatch,
    /// `sel` as of the last LCDC.4 set in mode 3, for that set's glitched
    /// read ([`Renderer::tile_sel_glitch`]; a later clear changes `sel`
    /// before the tile is drawn).
    sel_at_set: SelLatch,
    /// Screen x + 1 of the window zero pixel due on the current line
    /// ([`Renderer::render_zero`]), 0: none.
    win_zero: u8,
    /// CGB mode (else DMG compatibility mode), fixed from reset (`Ppu::reset`).
    pub(crate) cgb: bool,
}

impl Renderer {
    pub(crate) const fn new() -> Self {
        Self {
            window_line: 0,
            wy_triggered: false,
            ly: 0,
            bg_fine: 0,
            window_active: false,
            window_row: 0,
            win_wx: 0,
            window_skip: 0,
            objects: LineObjects::EMPTY,
            order: [0; MAX_PER_LINE],
            slow: 0,
            obj_fetched: false,
            next_obj: 0,
            hold: [Held::NONE; 3],
            sel: SelLatch::UNKNOWN,
            sel_at_set: SelLatch::UNKNOWN,
            win_zero: 0,
            cgb: false,
        }
    }

    /// Start of a frame (line 0, also after the LCD is turned on): the
    /// window line counter restarts at 0 and the WY condition is cleared
    /// (Pan Docs: Window, "On each VBlank, the Y condition is cleared";
    /// SameBoy resets `window_y` and `wy_triggered` at line 0).
    pub(crate) fn start_frame(&mut self) {
        self.window_line = 0;
        self.wy_triggered = false;
        // A zero pixel due on a line the LCD going off cut short.
        self.slow &= OBJ_DIRTY;
    }

    /// A WY == LY comparison. Pan Docs: at the start of each line; SameBoy
    /// (`wy_check`) also re-checks on WY and LCDC writes, and only sets the
    /// condition while LCDC.5 (window enable) is set. Follows SameBoy.
    pub(crate) fn check_wy(&mut self, ly: u8, regs: &Regs) {
        if regs.lcdc & LCDC_WIN_ON != 0 && regs.wy == ly {
            self.wy_triggered = true;
        }
    }

    /// Whether the WY condition holds (for the timing code's window
    /// penalty).
    pub(crate) const fn wy_triggered(&self) -> bool {
        self.wy_triggered
    }

    /// The window starts on the current line at WX `wx` (`None`: it
    /// doesn't), as the line's timing decided (with its stalls: at the line
    /// start, [`Renderer::start_line`], and after a window write in mode 3).
    pub(crate) const fn set_window(&mut self, wx: Option<u8>) {
        self.win_wx = match wx {
            Some(wx) => wx.wrapping_add(1),
            None => 0,
        };
    }

    /// WX at which the window starts on the current line, if it does
    /// ([`Renderer::set_window`]).
    pub(crate) const fn window_wx(&self) -> Option<u8> {
        self.win_wx.checked_sub(1)
    }

    /// The window state at the last pixel drawn: active, its start pixel
    /// and the pixels cut from its first tile.
    pub(crate) const fn window(&self) -> (bool, u8, u8) {
        (self.window_active, self.window_start(), self.window_skip)
    }

    /// [`Renderer::window`] as it will be once pixels `[x0, x1)` are drawn
    /// with LCDC `lcdc` ([`Renderer::render`]'s window steps): for a write
    /// whose flush is deferred (`Ppu::scroll_write`).
    #[inline]
    pub(crate) fn window_after(&self, x0: usize, x1: usize, lcdc: u8) -> (bool, u8, u8) {
        let x1 = x1.min(W);
        if x0 >= x1 {
            return self.window();
        }
        let on = lcdc & LCDC_WIN_ON != 0;
        if !self.window_active && self.win_wx != 0 {
            let start = usize::from(self.window_start());
            if (x0..x1).contains(&start) {
                return (on, start as u8, self.trigger_skip());
            }
        }
        (
            self.window_active && on,
            self.window_start(),
            self.window_skip,
        )
    }

    /// Screen x where the window starts (was activated) on this line.
    #[inline(always)]
    const fn window_start(&self) -> u8 {
        self.win_wx.saturating_sub(8)
    }

    /// The pixels cut from the window's first tile when it starts at the
    /// line's trigger WX (`win_wx`).
    #[inline(always)]
    const fn trigger_skip(&self) -> u8 {
        match self.win_wx {
            // The line's fine scroll: no window has ended before x = 0.
            1 => 7 + self.bg_fine,
            w @ 2..=7 => 8 - w,
            _ => 0,
        }
    }

    /// Start of line `ly`'s pixel output, with SCX as of that moment and
    /// the objects the OAM scan selected; the window starts at WX `window`
    /// ([`Renderer::set_window`]).
    pub(crate) fn start_line(
        &mut self,
        ly: u8,
        scx: u8,
        objects: &LineObjects,
        order: [u8; MAX_PER_LINE],
        window: Option<u8>,
    ) {
        self.ly = ly;
        self.bg_fine = scx & 7;
        self.window_active = false;
        self.set_window(window);
        self.objects = *objects;
        self.order = order;
        // The object buffer is all transparent unless an object was
        // fetched; it is cleared by the first `render` of the line.
        self.slow |= u8::from(self.obj_fetched);
        self.obj_fetched = false;
        self.next_obj = 0;
        self.hold[2].end = 0;
    }

    /// An SCX write moved the line's fine-scroll discard to `n` pixels
    /// (`Ppu::hold_fetch`); `n` and `n + 8` give the same columns.
    pub(crate) const fn set_fine(&mut self, n: u8) {
        self.bg_fine = n & 7;
    }

    /// The BG tiles' screen phase (`bg_fine`): the fine scroll, or the
    /// window's grid once the window ended on the line.
    pub(crate) const fn bg_fine(&self) -> u8 {
        self.bg_fine
    }

    /// The slot of the line's last object fetch: the last object in
    /// priority (fetch) order on screen horizontally (X < 168; objects at
    /// X >= 168 are never fetched). CGB: fetched even with LCDC.1 clear.
    #[inline(never)]
    pub(crate) fn last_fetched(&self) -> Option<usize> {
        self.order[..self.objects.len()]
            .iter()
            .rev()
            .map(|&s| usize::from(s) % MAX_PER_LINE)
            .find(|&s| self.objects.get(s).2 < 168)
    }

    /// Draws pixels `[x0, x1)` of the current line into `row` with the
    /// registers `regs`. Spans must be consecutive and in order.
    pub(crate) fn render(
        &mut self,
        row: &mut [u16; W],
        x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj: &mut ObjBuf,
    ) {
        let x1 = x1.min(W);
        if x0 >= x1 {
            return;
        }
        if usize::from(self.hold[2].end) > x0 {
            return self.render_held(row, x0, x1, regs, src, obj);
        }
        if self.slow != 0 {
            if self.slow & ZERO_DUE != 0 {
                return self.render_zero(row, x0, x1, regs, src, obj);
            }
            obj.pix = [0; OBJ_BUF_LEN];
            self.slow = 0;
        }
        let lcdc = regs.lcdc;

        // Window (Pan Docs: Window, "Window rendering criteria"; SameBoy
        // FIFO path). The window starts where the pixel counter equals WX
        // (screen x = WX - 7) while LCDC.5 and the WY condition hold, as the
        // line's timing decided that (`win_wx`, set with the window's
        // stall); that activation reads the next window row, even if LCDC.5
        // was cleared since the decision (SameBoy: `window_y++` at the
        // trigger, then the fetcher sees LCDC.5 clear and fetches the BG).
        // Clearing LCDC.5 switches back to the BG at the first tile the
        // fetcher fetches after it (the held parts keep LCDC.5 until then,
        // `Ppu::hold_fetch`; Mealybug `m3_lcdc_win_en_change_multiple`:
        // "always display a multiple of 8 pixels"), so `x0` is on the
        // window's tile grid and the BG tiles continue it (`bg_fine`). The
        // window can start again later on the line at a new decision
        // (`Ppu::retrigger`). WX = 0-6 start the window at x = 0 with 7 - WX
        // pixels cut off; WX = 0 also loses SCX & 7 pixels (Pan Docs: "the
        // Window is switched to before the initial fine scroll
        // adjustment"). WX = 166 shows one pixel on CGB (the DMG next-line
        // bug is DMG-only, SameBoy `!GB_is_cgb`); WX >= 167 never matches.
        if self.window_active && lcdc & LCDC_WIN_ON == 0 {
            self.window_active = false;
            self.bg_fine = (x0 as u8).wrapping_neg() & 7;
        }
        let mut win_from = x1;
        if self.window_active {
            win_from = x0;
        } else if self.win_wx != 0 {
            let start = usize::from(self.window_start());
            if (x0..x1).contains(&start) {
                self.window_skip = self.trigger_skip();
                self.window_row = self.window_line;
                self.window_line = self.window_line.wrapping_add(1);
                if lcdc & LCDC_WIN_ON != 0 {
                    self.window_active = true;
                    win_from = start;
                }
            }
        }

        // Objects are fetched when the span reaches their first visible
        // pixel, in priority order; a pixel keeps the first opaque object
        // written to it. Fetching per span (not per line) keeps split and
        // whole-line rendering identical.
        if usize::from(self.next_obj) < self.objects.len() {
            self.fetch_objects(obj, src, lcdc, x1);
        }

        // LCDC.1 is checked when pixels are output, not at the scan (CGB;
        // SameBoy).
        let out = &mut row[x0..x1];
        if lcdc & LCDC_OBJ_ON == 0 || !self.obj_fetched {
            // No object pixel in this span: tiles straight to colors, no
            // mixing (most lines of most games).
            self.bg_span::<false>(out, &[], x0, win_from, regs, src);
        } else {
            // Mixed while the BG/window tiles are decoded, 8 pixels at a
            // time; tiles with no object pixel take the plain path.
            self.bg_span::<true>(out, &obj.pix[x0 + 8..x1 + 8], x0, win_from, regs, src);
        }
    }

    /// [`Renderer::render`] while pixels the fetcher fetched before a
    /// map-select, tile-data, SCX or SCY write in mode 3 are still to be
    /// drawn (`Ppu::hold_fetch`): each held part with the LCDC bits 3-6
    /// (LCDC.5 too: the fetcher checks it at each tile), the SCX and the
    /// SCY its tiles were fetched with (and the planes of its last tile
    /// read after the write redrawn, see
    /// [`Renderer::mix_planes`]), then the rest with `regs`. Out of line
    /// and cold: only after such a write.
    #[cold]
    #[inline(never)]
    fn render_held(
        &mut self,
        row: &mut [u16; W],
        mut x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj: &mut ObjBuf,
    ) {
        const FETCHED: u8 = LCDC_BG_MAP | LCDC_TILES_8000 | LCDC_WIN_MAP | LCDC_WIN_ON;
        let regs_of = |h: &Held| Regs {
            lcdc: (regs.lcdc & !FETCHED) | (h.lcdc & FETCHED),
            scx: h.scx,
            scy: h.scy,
            ..*regs
        };
        // Taken while the parts are drawn, so they are drawn as usual.
        let hold = core::mem::take(&mut self.hold);
        for (i, h) in hold.iter().enumerate() {
            let end = usize::from(h.end);
            if end <= x0 {
                continue;
            }
            let held = regs_of(h);
            let to = end.min(x1);
            self.render(row, x0, to, &held, src, obj);
            let from = x0.max(end.saturating_sub(8));
            if !h.mixed.is_none() && from < to {
                let next = hold.get(i + 1).map_or(*regs, regs_of);
                self.mix_planes(row, &obj.pix, from, to, &held, &next, h.mixed, src);
            }
            x0 = to;
        }
        self.render(row, x0, x1, regs, src, obj);
        self.hold = hold;
    }

    /// Whether screen x `z` is the first pixel of a window tile after the
    /// window's first one, on the window's grid as it is (or will be, once
    /// started) drawn on this line ([`Renderer::set_zero`]).
    pub(crate) fn window_tile_edge(&self, z: u8) -> bool {
        let start = self.window_start();
        let skip = if self.window_active {
            self.window_skip
        } else {
            self.trigger_skip()
        };
        self.win_wx != 0 && z > start && (z - start).wrapping_add(skip) & 7 == 0
    }

    /// A window zero pixel is due at screen x `z` (`None`: none is), see
    /// [`Renderer::render_zero`]; `Ppu::window_zero` decides it.
    pub(crate) fn set_zero(&mut self, z: Option<u8>) {
        self.win_zero = z.map_or(0, |z| z + 1);
        self.slow |= ZERO_DUE;
    }

    /// [`Renderer::render`] while a window zero pixel is due at `win_zero`
    /// (`ZERO_DUE`): the pixels before it, then, if the window is still
    /// drawn there from an earlier start, a color-0 pixel of BG palette 0 (objects mixed over it
    /// as over any color 0) that delays the window's pixels by one, then
    /// the rest. The window pixel due at the zero pixel comes out one dot
    /// later and the line's last one is lost: the FIFO isn't popped for it
    /// (SameBoy display.c `insert_bg_pixel`, "Insert a pixel right at the
    /// FIFO's end"). Out of line and cold: only after a WX write in mode 3.
    #[cold]
    #[inline(never)]
    fn render_zero(
        &mut self,
        row: &mut [u16; W],
        x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj: &mut ObjBuf,
    ) {
        self.slow &= !ZERO_DUE;
        // The zero pixel's x + 1 (0: none).
        let z1 = usize::from(self.win_zero);
        // Up to it as usual first: its objects are fetched.
        let cut = z1.min(x1).max(x0);
        self.render(row, x0, cut, regs, src, obj);
        if z1 > x1 {
            // Not reached yet (the rest is empty).
            self.slow |= ZERO_DUE;
        } else if z1 > x0 {
            self.win_zero = 0;
            // Not at a (re)start: the window's first tile is still fetched.
            if self.window_active && z1 > usize::from(self.window_start()) + 1 {
                if let (Some(p), Some(&o)) = (row.get_mut(z1 - 1), obj.pix.get(z1 + 7)) {
                    let o = if regs.lcdc & LCDC_OBJ_ON != 0 { o } else { 0 };
                    *p = mix(0, o, 0, &src.colors.bg[0], src.colors);
                }
                self.window_skip = self.window_skip.wrapping_sub(1);
            }
        }
        self.render(row, cut, x1, regs, src, obj);
    }

    /// Redraws pixels `[x0, x1)` of one BG or window tile, just drawn with
    /// `held`, with the bit planes `mixed.late()` read with `next`'s LCDC.4
    /// and SCY instead (and `mixed.glitched()` as the TILE_SEL glitch gives
    /// them): the fetcher read the map entry
    /// and the other planes before a write and these after it (CGB C;
    /// Mealybug "mixing tile bitplane data from different rows" and "from
    /// two different tile patterns"). The window row ignores SCY.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn mix_planes(
        &self,
        row: &mut [u16; W],
        obj_buf: &[u8; OBJ_BUF_LEN],
        x0: usize,
        x1: usize,
        held: &Regs,
        next: &Regs,
        mixed: Mixed,
        src: &Sources,
    ) {
        let lcdc = held.lcdc;
        let master = if lcdc & LCDC_BG_ON != 0 { 0xFF } else { 0 };
        if !self.cgb && master == 0 {
            // Color 0 whatever the tile data.
            return;
        }
        let win = self.window_active && x0 >= usize::from(self.window_start());
        let (map_bit, y, y_next, col) = if win {
            let col =
                ((x0 - usize::from(self.window_start())) as u8).wrapping_add(self.window_skip);
            (LCDC_WIN_MAP, self.window_row, self.window_row, col)
        } else {
            (
                LCDC_BG_MAP,
                self.ly.wrapping_add(held.scy),
                self.ly.wrapping_add(next.scy),
                bg_column(held.scx, self.bg_fine, x0 as u8),
            )
        };
        let map = if lcdc & map_bit != 0 { 0x1C00 } else { 0x1800 };
        let fetch = |row_y: u8, tiles: u8| {
            let (base, flip) = tile_data(tiles);
            let f = Fetch {
                vram: src.vram,
                map_row: map | (usize::from(y >> 3) << 5),
                row_offset: usize::from(row_y & 7) << 1,
                base,
                flip,
            };
            if self.cgb {
                f.row::<true>(col)
            } else {
                f.row::<false>(col)
            }
        };
        let (old, attr) = fetch(y, lcdc);
        let (new, _) = fetch(y_next, next.lcdc);
        let (m, g) = (mixed.late().row_mask(), mixed.glitched().row_mask());
        let glitch = if g != 0 {
            let index =
                src.vram[(map | (usize::from(y >> 3) << 5) | usize::from(col >> 3)) & 0x1FFF];
            self.tile_sel_glitch(src, lcdc, mixed.set_latch(), index, attr, old, new)
        } else {
            0
        };
        let w = ((old & !m & !g) | (new & m) | (glitch & g)) << (2 * (col & 7));
        // One instance (mixed with no object pixels when none are shown):
        // at most 8 pixels on a cold path, where a plain twin measured no
        // Ir difference on any bench.
        let obj = if lcdc & LCDC_OBJ_ON != 0 && self.obj_fetched {
            &obj_buf[x0 + 8..x1 + 8]
        } else {
            &[0; 8]
        };
        fill_partial::<true>(&mut row[x0..x1], obj, w, attr, master, src.colors);
    }

    /// For [`Renderer::mix_planes`]: the decoded row (as [`decode`]) whose
    /// glitched plane (`Mixed::glitched`, one) the fetcher took for the
    /// tile at screen x `tile_x` with tile index `index`, attributes
    /// `attr` and row `y`, read across an LCDC.4 change on that plane's
    /// read dot (CGB TILE_SEL glitch; Mealybug TILE_SEL notes, SameBoy
    /// `data_for_tile_sel_glitch`, MIT). `lcdc` is from before the change.
    /// The data is a byte, X-flipped at the push:
    /// - a clear: the tile index, except for an index >= 0x80 (both data
    ///   areas map it to 8800-8FFF: the read doesn't change). The byte at
    ///   the old address is latched (`Ppu::hold_fetch`);
    /// - a set: the latch, the last byte read with 8000-8FFF addressing
    ///   (Mealybug: "bitplane 1 data from the most recently drawn sprite
    ///   ... or tile"): the high plane of the last object fetched before
    ///   this tile's reads (`set_latch`, [`SetLatch::Object`]) unless the
    ///   latch as of the set (`sel_at_set`) was read after it. With
    ///   neither known, the row as `old` (an object stall in tile j or
    ///   j + 1, `Ppu::hold_fetch`) or `new` ([`SetLatch::Plain`]),
    ///   which the earlier `LCDC_TILES_LATE` calibration fitted.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn tile_sel_glitch(
        &self,
        src: &Sources,
        lcdc: u8,
        set_latch: SetLatch,
        index: u8,
        attr: u8,
        old: u16,
        new: u16,
    ) -> u16 {
        let latch = self.sel_at_set;
        let data = if lcdc & LCDC_TILES_8000 != 0 {
            if index >= 0x80 {
                return old;
            }
            index
        } else {
            let object = match set_latch {
                SetLatch::Object(s) => Some(usize::from(s)),
                _ => None,
            }
            .filter(|&s| s < MAX_PER_LINE && latch.before_object(self.objects.get(s).2));
            match object {
                Some(s) => src.vram[self.obj_high_addr(src.oam, src.attrs, s, self.ly, lcdc)],
                None if latch.known() => latch.byte,
                None if set_latch == SetLatch::Plain => return new,
                None => return old,
            }
        };
        SPREAD[usize::from(attr & BG_ATTR_X_FLIP != 0)][usize::from(data)] * 3
    }

    /// The byte of the BG or window tile at screen x `x` (its first pixel)
    /// in bit plane `plane` (0 low, 1 high) at its 8000-8FFF address, with
    /// the map select of `lcdc`, `scx` and `scy`, as drawn so far: what the
    /// fetcher read for it with LCDC.4 set (TILE_SEL glitch latch,
    /// `Ppu::hold_fetch`). Cold: LCDC.4 clears in mode 3 only.
    #[cold]
    #[inline(never)]
    pub(crate) fn tile_byte_8000(
        &self,
        vram: &[u8; 0x4000],
        x: u8,
        lcdc: u8,
        scx: u8,
        scy: u8,
        plane: usize,
    ) -> u8 {
        let (map_bit, y, col) = if self.window_active && x >= self.window_start() {
            let col = x
                .wrapping_sub(self.window_start())
                .wrapping_add(self.window_skip);
            (LCDC_WIN_MAP, self.window_row, col)
        } else {
            let col = bg_column(scx, self.bg_fine, x);
            (LCDC_BG_MAP, self.ly.wrapping_add(scy), col)
        };
        let map = if lcdc & map_bit != 0 { 0x1C00 } else { 0x1800 };
        let i = map | (usize::from(y >> 3) << 5) | usize::from(col >> 3);
        let attr = if self.cgb { vram[i | 0x2000] } else { 0 };
        let a = (usize::from(vram[i]) << 4 | usize::from(y & 7) << 1 | plane)
            ^ (ATTR_FETCH[usize::from(attr)] as usize & 0x3FFF);
        vram[a & 0x3FFF]
    }

    /// The CGB TILE_SEL glitch latch (`sel`) at the
    /// start of a line, LCDC `lcdc`, before the renderer leaves the line
    /// that ended: the high plane of that line's last object fetch (OAM as
    /// it is now, `Renderer::obj_high_addr`) if it came after the latched
    /// read; a read on that line becomes one on an earlier line
    /// (`SelRead::Before(1)`);
    /// BG reads with 8000 addressing to its end ([`SelRead::BgSince`])
    /// unknown (not
    /// modelled: SameBoy keeps the last high-plane read, Mealybug "the read
    /// in progress during pixel 159/160"). With LCDC.4 set, the new line's
    /// BG reads use 8000 addressing from its start (`BgSince`). The hardware
    /// latch persists across lines: Mealybug `m3_lcdc_tile_sel_change`
    /// line 128 (line 127's clear), 129-135 (line 128's object),
    /// `m3_lcdc_tile_sel_win_change` line 64 (line 63's BG read before the
    /// clear), gambatte `bgtiledata_spx*` (the same). Cold: called only
    /// on lines with LCDC.4 clear or after one ([`Renderer::sel_carries`]).
    #[cold]
    #[inline(never)]
    pub(crate) fn carry_sel(&mut self, oam: &[u8; 0xA0], vram: &[u8; 0x4000], lcdc: u8) {
        let mut latch = self.sel;
        // On an earlier line from here on.
        if latch.known() {
            latch.read = SelRead::Before(1);
        } else {
            latch.read = SelRead::Unknown;
        }
        if let Some(s) = self.last_fetched()
            && self.sel.before_object(self.objects.get(s).2)
        {
            latch.byte = vram[self.obj_high_addr(oam, None, s, self.ly, lcdc)];
            latch.read = SelRead::Before(1);
        }
        if lcdc & LCDC_TILES_8000 != 0 {
            latch.read = SelRead::BgSince;
        }
        self.sel = latch;
    }

    /// Whether the line that starts with LCDC `lcdc` changes the TILE_SEL
    /// glitch latch at its start ([`Renderer::carry_sel`]): not while
    /// LCDC.4 stays set (most lines: one test, no call).
    #[inline]
    pub(crate) const fn sel_carries(&self, lcdc: u8) -> bool {
        !matches!(self.sel.read, SelRead::BgSince) || lcdc & LCDC_TILES_8000 == 0
    }

    /// An LCDC.4 set in mode 3 ([`Renderer::tile_sel_glitch`] reads the
    /// latch as of now for its glitched plane): from here on BG reads with
    /// 8000 addressing replace the latch (unknown).
    pub(crate) const fn sel_set(&mut self) {
        self.sel_at_set = self.sel;
        self.sel.read = SelRead::BgSince;
    }

    /// An LCDC.4 clear in mode 3: the latch holds `byte`, the BG or window
    /// read of the tile at screen x `x` (its first pixel; may be left of
    /// the screen), the last with 8000 addressing.
    pub(crate) fn sel_clear(&mut self, byte: u8, x: i32) {
        self.sel = SelLatch {
            byte,
            read: SelRead::Before((x + 8).clamp(1, 254) as u8),
        };
    }

    /// The line has a held part still to draw ([`Renderer::render_held`]).
    #[inline]
    pub(crate) const fn holding(&self) -> bool {
        self.hold[2].end != 0
    }

    /// A new held part `seg` for a write with pixel `x` next out
    /// (`Ppu::hold_fetch`): the line's last part, or none when an earlier
    /// write already holds further.
    pub(crate) fn push_hold(&mut self, seg: Held, x: i32) {
        let hold = &mut self.hold;
        let last = hold[2];
        if i32::from(last.end) <= x {
            *hold = [Held::default(), Held::default(), seg];
        } else if seg.end > last.end
            || (seg.end == last.end && !last.mixed.is_none() && seg.mixed.is_none())
        {
            // Same end: the held tile's planes read after the earlier
            // write were read with the values before this one, which the
            // (empty) new part keeps for `Renderer::mix_planes`.
            *hold = [hold[1], last, seg];
        }
    }

    /// The window's first tile is fetched after it starts: no held part
    /// reaches past a start at screen x `start` decided after it.
    pub(crate) fn cut_holds(&mut self, start: u8) {
        for h in &mut self.hold {
            if h.end > start {
                h.end = start;
                h.mixed = Mixed::NONE;
            }
        }
    }

    /// BG/window pixels `[x0, x0 + out.len())`, the window from `win_from`
    /// on; with `MIX`, mixed with the object pixels `obj` (same length as
    /// `out`, see [`mix`]). With LCDC.0 clear in non-CGB mode both
    /// show color 0, which still goes through BGP, and objects are drawn
    /// over them regardless of their priority bit (Pan Docs: LCDC.0;
    /// SameBoy `render_pixel_if_possible`: `bg_enabled = false` makes the
    /// pixel 0, then BGP applies). The window state still advances: the
    /// fetcher runs, only the mixer blanks the pixels. In CGB mode LCDC.0
    /// clear draws the BG and window, and opaque object pixels go on top of
    /// them whatever the BG attribute and OAM priority bits say (Pan Docs:
    /// LCDC.0, "CGB: BG and Window master priority"; SameBoy
    /// `render_pixel_if_possible`: `bg_priority` only with `bg_enabled`).
    #[inline(always)]
    fn bg_span<const MIX: bool>(
        &self,
        out: &mut [u16],
        obj: &[u8],
        x0: usize,
        win_from: usize,
        regs: &Regs,
        src: &Sources,
    ) {
        let lcdc = regs.lcdc;
        let colors = src.colors;
        if self.cgb {
            let master = if lcdc & LCDC_BG_ON != 0 { 0xFF } else { 0 };
            self.bg_parts::<MIX, true>(out, obj, x0, win_from, regs, src, master);
        } else if lcdc & LCDC_BG_ON == 0 {
            if MIX {
                for (p, &o) in out.iter_mut().zip(obj) {
                    *p = mix(0, o, ATTR_BEHIND_BG, &colors.bg[0], colors);
                }
            } else {
                out.fill(colors.bg[0][0]);
            }
        } else {
            self.bg_parts::<MIX, false>(out, obj, x0, win_from, regs, src, 0xFF);
        }
    }

    /// [`Renderer::bg_span`]'s tiles: the BG part up to `win_from`, then the
    /// window part. `CGB`: CGB mode, the tiles have attributes
    /// ([`Fetch::row`]); `master`: see [`prio_mask`].
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn bg_parts<const MIX: bool, const CGB: bool>(
        &self,
        out: &mut [u16],
        obj: &[u8],
        x0: usize,
        win_from: usize,
        regs: &Regs,
        src: &Sources,
        master: u8,
    ) {
        let lcdc = regs.lcdc;
        let split = (win_from - x0).min(out.len());
        let (bg, win) = out.split_at_mut(split);
        let (obj_bg, obj_win) = if MIX {
            obj.split_at_checked(split).unwrap_or((obj, &[]))
        } else {
            (&[][..], &[][..])
        };
        if !bg.is_empty() {
            let col = bg_column(regs.scx, self.bg_fine, x0 as u8);
            let map = if lcdc & LCDC_BG_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            let y = self.ly.wrapping_add(regs.scy);
            fill_tiles::<MIX, CGB>(bg, obj_bg, src, lcdc, master, map, y, col);
        }
        if !win.is_empty() {
            let col = ((win_from - usize::from(self.window_start())) as u8)
                .wrapping_add(self.window_skip);
            let map = if lcdc & LCDC_WIN_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            fill_tiles::<MIX, CGB>(win, obj_win, src, lcdc, master, map, self.window_row, col);
        }
    }

    /// Fetches the objects whose first visible pixel is in `[x0, x1)`, in
    /// fetch order. `order` is ascending X and the spans of a line are
    /// consecutive from pixel 0, so the objects of a span are the next ones
    /// in `order` from `next_obj`: each object is looked at once per line,
    /// not once per span. Out of line: once per span with objects, and
    /// forced into `render` it measured slower (cpu_instrs +0.06%,
    /// worst-case +0.04%, cgb-acid2 -0.01% x86 Ir) for 568 B more.
    #[inline(never)]
    fn fetch_objects(&mut self, obj: &mut ObjBuf, src: &Sources, lcdc: u8, x1: usize) {
        let mut k = usize::from(self.next_obj);
        while k < self.objects.len() {
            let slot = usize::from(self.order[k % MAX_PER_LINE]);
            let (index, y, x) = self.objects.get(slot);
            // X = 0 and X >= 168 are off-screen (Pan Docs: OAM byte 1); X
            // >= 168 ends the line's fetches (ascending X).
            if x >= 168 || usize::from(x.saturating_sub(8)) >= x1 {
                break;
            }
            if x != 0 {
                self.fetch_object(obj, src, lcdc, slot, index, y, x);
            }
            k += 1;
        }
        self.next_obj = k as u8;
    }

    /// Fetches the line's next object in fetch order (`next_obj`)
    /// now, with LCDC `lcdc`: an LCDC.2 write lands after its fetch read
    /// the tile row (`Ppu::obj_size_fetch`). Cold: only on such writes.
    #[cold]
    #[inline(never)]
    pub(crate) fn fetch_next_object(&mut self, obj: &mut ObjBuf, src: &Sources, lcdc: u8) {
        if self.slow & OBJ_DIRTY != 0 {
            obj.pix = [0; OBJ_BUF_LEN];
            self.slow &= !OBJ_DIRTY;
        }
        // `fetch_objects` up to that object only.
        let all = self.objects;
        self.objects = all.with_len(self.next_obj.wrapping_add(1));
        self.fetch_objects(obj, src, lcdc, usize::MAX);
        self.objects = all;
    }

    /// The fetch-order position and the slot of the line's next object to
    /// fetch, if any is left.
    pub(crate) fn next_object(&self) -> Option<(usize, usize)> {
        let next = usize::from(self.next_obj);
        (next < self.objects.len()).then(|| {
            (
                next,
                usize::from(self.order[next % MAX_PER_LINE]) % MAX_PER_LINE,
            )
        })
    }

    /// The VRAM address of the high bit plane byte of line `ly`'s row of
    /// the line's object in slot `slot` (tile and attributes from `attrs`
    /// or `oam`, as [`Renderer::fetch_object`]), its height from `lcdc`.
    #[cold]
    #[inline(never)]
    pub(crate) fn obj_high_addr(
        &self,
        oam: &[u8; 0xA0],
        attrs: Option<&[(u8, u8); MAX_PER_LINE]>,
        slot: usize,
        ly: u8,
        lcdc: u8,
    ) -> usize {
        let (index, y, _) = self.objects.get(slot);
        let (tile, flags) = match attrs {
            Some(a) => a[slot % MAX_PER_LINE],
            None => {
                let b = usize::from(index.min(39)) * 4;
                (oam[b + 2], oam[b + 3])
            }
        };
        let bank = usize::from(self.cgb && flags & CGB_ATTR_BANK != 0) << 13;
        ((object_row(ly, y, tile, flags, lcdc) | bank) + 1) & 0x3FFF
    }

    /// Decodes one object's row into the object buffer.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)] // Object fields plus the buffer.
    fn fetch_object(
        &mut self,
        obj: &mut ObjBuf,
        src: &Sources,
        lcdc: u8,
        slot: usize,
        index: u8,
        y: u8,
        x: u8,
    ) {
        self.obj_fetched = true;
        let (tile, flags) = match src.attrs {
            Some(a) => a[slot % MAX_PER_LINE],
            None => {
                // `index` < 40 (an OAM entry): `min` only keeps it in bounds.
                let base = usize::from(index.min(39)) * 4;
                (src.oam[base + 2], src.oam[base + 3])
            }
        };
        // Objects always use 8000-8FFF addressing (Pan Docs: VRAM Tile Data).
        // Palette (buffer bits 2-4), tile data bank and drawing-priority
        // rank. CGB mode (Pan Docs: OAM byte 3): bits 0-2 select OBJ
        // palette 0-7 (OCPD), bit 3 the VRAM bank, bit 4 is unused; the
        // object with the lower OAM index wins a pixel, whatever the fetch
        // (X) order (SameBoy `fifo_overlay_object_row`: a pixel is replaced
        // by an opaque one of a lower `priority`, the OAM index in CGB
        // mode). The slot number orders the same way (slots are in OAM
        // order). Compatibility mode: bit 4 selects OBP1 (OBJ palette 1),
        // bank 0, and every object has rank 0, so the first fetch (lowest
        // X, then OAM index) keeps the pixel (SameBoy: priority 0 with
        // `GB_OBJECT_PRIORITY_X`). The mode is fixed after boot: SameBoy
        // only applies OPRI writes while the boot ROM runs (or in the KEY0
        // PGB mode, which a game can't reach), see `objects`.
        let (pal, bank, rank) = if self.cgb {
            (
                (flags & 7) << 2,
                usize::from(flags & CGB_ATTR_BANK) << 10,
                (slot % MAX_PER_LINE) as u8,
            )
        } else {
            (u8::from(flags & ATTR_DMG_OBP1 != 0) << 2, 0, 0)
        };
        let addr = object_row(self.ly, y, tile, flags, lcdc) | bank;
        let (mut lo, mut hi) = (src.vram[addr & 0x3FFF], src.vram[(addr + 1) & 0x3FFF]);
        if flags & ATTR_X_FLIP != 0 {
            lo = lo.reverse_bits();
            hi = hi.reverse_bits();
        }
        let w = decode(lo, hi);
        let attr = pal | (flags & ATTR_BEHIND_BG);
        // All 8 pixels at once: a byte per pixel, leftmost first. A pixel
        // is written where the new color is opaque and the buffer is either
        // still transparent or holds an object of lower drawing priority
        // (a higher rank).
        let x = usize::from(x);
        if let (Some(dst), Some(ranks)) = (
            obj.pix
                .get_mut(x..x + 8)
                .and_then(|s| <&mut [u8; 8]>::try_from(s).ok()),
            obj.rank
                .get_mut(x..x + 8)
                .and_then(|s| <&mut [u8; 8]>::try_from(s).ok()),
        ) {
            let new = spread_bytes(w);
            let old = u64::from_be_bytes(*dst);
            let old_rank = u64::from_be_bytes(*ranks);
            let taken = opaque_bytes(old);
            let mut put = opaque_bytes(new) & !taken;
            if taken != 0 {
                // Overlap: also the bytes whose rank is above `rank` (ranks
                // are at most 9: no borrow crosses a byte).
                let lower = ((old_rank | BYTES_80).wrapping_sub(u64::from(rank + 1) * BYTES_01)
                    & BYTES_80)
                    >> 7;
                put |= opaque_bytes(new) & (lower * 0xFF);
            }
            *dst = ((old & !put) | ((new | (u64::from(attr) * BYTES_01)) & put)).to_be_bytes();
            *ranks = ((old_rank & !put) | ((u64::from(rank) * BYTES_01) & put)).to_be_bytes();
        }
    }
}

/// The VRAM address (bank 0) of the low bit plane of line `ly`'s row of an
/// object with OAM Y `y`, tile index `tile` and attributes `flags`, its
/// height from LCDC `lcdc`. LCDC.2 is read at the fetch (SameBoy
/// `get_object_line_address`); 8x16 objects ignore bit 0 of the tile index
/// (Pan Docs: OAM byte 2). Objects always use 8000-8FFF addressing (Pan
/// Docs: VRAM Tile Data).
#[inline(always)]
pub(crate) fn object_row(ly: u8, y: u8, tile: u8, flags: u8, lcdc: u8) -> usize {
    let (mask, tile) = if lcdc & LCDC_OBJ_TALL != 0 {
        (15, tile & 0xFE)
    } else {
        (7, tile)
    };
    let mut line = ly.wrapping_add(16).wrapping_sub(y) & mask;
    if flags & ATTR_Y_FLIP != 0 {
        line ^= mask;
    }
    (usize::from(tile) << 4) + (usize::from(line) << 1)
}

/// One pixel of a span with objects: BG color index `c` (before the
/// palette) of a tile with BG palette `bg` and priority mask `m`, object
/// pixel `o`. An opaque object pixel is drawn unless `c` is non-zero and
/// `o & m` is non-zero. `m` is 0x80 (the OAM "behind BG" bit, Pan Docs: OAM
/// byte 3 bit 7) for a tile without the BG attribute priority bit, 0xFF
/// (any opaque object) for one with it, and 0 with the CGB master priority
/// (LCDC.0) clear (Pan Docs: "BG-to-OBJ Priority in CGB Mode").
#[inline(always)]
fn mix(c: u8, o: u8, m: u8, bg: &[u16; 4], colors: &Colors) -> u16 {
    if o != 0 && (c == 0 || o & m == 0) {
        colors.obj[usize::from(o >> 2) & 7][usize::from(o & 3)]
    } else {
        bg[usize::from(c & 3)]
    }
}

/// One BG or window tile row fetch: map row and tile data addressing,
/// fixed for a part of a span.
struct Fetch<'a> {
    vram: &'a [u8; 0x4000],
    /// VRAM offset of the map row.
    map_row: usize,
    /// Byte offset of the pixel row in a tile (2 * fine y).
    row_offset: usize,
    /// [`tile_data`].
    base: usize,
    flip: u8,
}

impl Fetch<'_> {
    /// The decoded row of the tile at map column `col` and its attributes:
    /// in CGB mode (`CGB`) the byte at the same map address in VRAM bank 1
    /// (Pan Docs: VRAM Tile Maps, "BG Map Attributes"; SameBoy
    /// `fetcher_step` reads it from `vram[map + 0x2000]` when `cgb_mode`),
    /// else 0. Bit 3 selects the tile data bank, bit 6 flips the row, bit 5
    /// the pixels.
    #[inline(always)]
    fn row<const CGB: bool>(&self, col: u8) -> (u16, u8) {
        let vram = self.vram;
        let i = (self.map_row | usize::from(col >> 3)) & 0x1FFF;
        if !CGB {
            // Compatibility mode: no attributes, tile data in bank 0.
            let addr =
                ((self.base + (usize::from(vram[i] ^ self.flip) << 4)) | self.row_offset) & 0x1FFF;
            return (decode(vram[addr], vram[addr | 1]), 0);
        }
        let attr = vram[i | 0x2000];
        // Flips and bank from a table: fewer operations per tile than
        // decoding the bits (every CGB-mode tile pays it).
        let e = ATTR_FETCH[usize::from(attr)];
        let addr = (((self.base + (usize::from(vram[i] ^ self.flip) << 4)) | self.row_offset)
            ^ (e as usize & 0x3FFF))
            & 0x3FFF;
        // X flip: the mirrored spread table, no branch (a conditional bit
        // reversal costs ~25 x86 instructions per tile even when not taken).
        let t = &SPREAD[(e >> 31) as usize];
        let w = t[usize::from(vram[addr])] | (t[usize::from(vram[addr | 1])] << 1);
        (w, attr)
    }
}

/// Per BG attribute byte, for [`Fetch::row`]: bits 0-13, what it does to
/// a tile row's VRAM offset (row offset 2 * fine y, tile data below
/// 0x1800) as an XOR: Y flip (bit 6) takes row 7 - fine y (offset ^ 14),
/// bank 1 (bit 3) adds 0x2000; bit 31, X flip (bit 5): the mirrored
/// [`SPREAD`] table.
static ATTR_FETCH: [u32; 256] = {
    let mut t = [0u32; 256];
    let mut a = 0;
    while a < 256 {
        let attr = a as u8;
        if attr & BG_ATTR_Y_FLIP != 0 {
            t[a] |= 14;
        }
        if attr & BG_ATTR_BANK != 0 {
            t[a] |= 0x2000;
        }
        if attr & BG_ATTR_X_FLIP != 0 {
            t[a] |= 1 << 31;
        }
        a += 1;
    }
    t
};

/// The priority mask [`mix`] uses for a tile with attributes `attr`: the
/// OAM bit (0x80), or any opaque object pixel (0xFF) with the attribute's
/// BG priority bit; ANDed with `master`: 0 in CGB mode with LCDC.0 clear
/// (objects always on top), else 0xFF.
#[inline(always)]
fn prio_mask(attr: u8, master: u8) -> u8 {
    (((attr & BG_ATTR_PRIORITY) as i8 >> 7) as u8 | ATTR_BEHIND_BG) & master
}

/// Fills `out` with the colors of the BG/window pixels from the 32x32 tile
/// map at `map` (VRAM offset), map pixel row `y`, starting at map pixel
/// column `col` and wrapping at 256. With `MIX`, `obj` holds the object
/// pixels at the same positions (same length as `out`) and each pixel is
/// [`mix`]ed; a whole tile with no object pixel (one 8-byte test) is drawn
/// as without `MIX`. Each tile's palette and priority come from its
/// attributes ([`Fetch::row`]; none without `CGB`).
///
/// Left to the compiler, which keeps one out-of-line copy per `MIX`/`CGB`
/// pair. Forced inline (BG and window call sites: 8 copies) measured
/// cgb-acid2 -1.49%, cpu_instrs -0.30%, worst-case -0.12%, cgb-raster
/// +0.09%, two DMG-mode ROMs -0.44% and -0.73% x86 Ir for 4,634 B more.
#[allow(clippy::too_many_arguments)]
fn fill_tiles<const MIX: bool, const CGB: bool>(
    out: &mut [u16],
    obj: &[u8],
    src: &Sources,
    lcdc: u8,
    master: u8,
    map: usize,
    y: u8,
    mut col: u8,
) {
    let colors = src.colors;
    let (base, flip) = tile_data(lcdc);
    let f = Fetch {
        vram: src.vram,
        map_row: map | (usize::from(y >> 3) << 5),
        row_offset: usize::from(y & 7) << 1,
        base,
        flip,
    };
    // Part of one tile from map column `col`: the span's start up to the
    // next tile boundary, or its end.
    let part = Part { f, master, colors };
    let f = &part.f;
    let partial = |out: &mut [u16], obj: &[u8], col: u8| {
        partial_tile::<MIX, CGB>(out, obj, &part, col);
    };

    let (mut out, mut obj) = (out, obj);
    let c = usize::from(col & 7);
    if c != 0 {
        let n = (8 - c).min(out.len());
        let (head, rest) = out.split_at_mut(n);
        let (obj_head, obj_rest) = if MIX {
            obj.split_at_checked(n).unwrap_or((obj, &[]))
        } else {
            (&[][..], &[][..])
        };
        partial(head, obj_head, col);
        (out, obj) = (rest, obj_rest);
        col = col.wrapping_add(n as u8);
    }
    // Whole tiles: fixed trip counts, unrolled.
    let (tiles, tail) = out.as_chunks_mut::<8>();
    let (obj_tiles, obj_tail) = if MIX {
        obj.as_chunks::<8>()
    } else {
        (&[][..], &[][..])
    };
    for (i, tile) in tiles.iter_mut().enumerate() {
        let (w, attr) = f.row::<CGB>(col);
        let pal = usize::from(attr & 7);
        let obj8 = if MIX {
            obj_tiles.get(i).filter(|o| u64::from_ne_bytes(**o) != 0)
        } else {
            None
        };
        match obj8 {
            // No object pixel: two pixels per lookup (and, merged by the
            // compiler on little-endian targets, per store).
            None => {
                let pairs = &src.pairs[pal];
                for (j, p) in tile.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let v = pairs[usize::from((w >> (12 - 4 * j)) as u8 & 15)];
                    *p = [v as u16, (v >> 16) as u16];
                }
            }
            Some(o8) => {
                let (bg, m) = (&colors.bg[pal], prio_mask(attr, master));
                for (i, (p, &o)) in tile.iter_mut().zip(o8).enumerate() {
                    *p = mix((w >> (14 - 2 * i)) as u8 & 3, o, m, bg, colors);
                }
            }
        }
        col = col.wrapping_add(8);
    }
    if !tail.is_empty() {
        partial(tail, obj_tail, col);
    }
}

/// The colors of two adjacent pixels of palette `pal`: entry 4a + b holds
/// color a (left pixel) in bits 0-15 and color b (right pixel) in bits
/// 16-31, so a decoded tile row ([`decode`]) gives an index per 4 bits.
/// Kept per palette in `Memory` and rebuilt when the palette changes, so
/// a span's whole tiles take 4 lookups each instead of 8 with no per-span
/// setup.
pub(crate) fn pair_table(pal: &[u16; 4]) -> [u32; 16] {
    let mut t = [0u32; 16];
    for (i, e) in t.iter_mut().enumerate() {
        *e = u32::from(pal[i >> 2]) | (u32::from(pal[i & 3]) << 16);
    }
    t
}

/// Sets color `c` (0-3) of the palette whose [`pair_table`] is `t` to
/// `col`: the 4 entries with it as the left pixel (4c + k) and the 4 with
/// it as the right pixel (4k + c; entry 5c is both). The same table as a
/// rebuild, for half the stores and no palette read (a BCPD write changes
/// one color).
pub(crate) fn set_pair_color(t: &mut [u32; 16], c: usize, col: u16) {
    let c = c & 3;
    let col = u32::from(col);
    for k in 0..4 {
        let l = &mut t[4 * c + k];
        *l = (*l & 0xFFFF_0000) | col;
        let r = &mut t[4 * k + c];
        *r = (*r & 0xFFFF) | (col << 16);
    }
}

/// What [`fill_partial`] needs besides the pixels.
struct Part<'a> {
    f: Fetch<'a>,
    /// [`prio_mask`]'s `master`.
    master: u8,
    colors: &'a Colors,
}

/// Part of the tile at map column `col` (from pixel `col & 7` on), mixed
/// with `obj` with `MIX`: the fetch, then [`fill_partial`]. At most two per
/// part of a span.
fn partial_tile<const MIX: bool, const CGB: bool>(out: &mut [u16], obj: &[u8], p: &Part, col: u8) {
    let (w, attr) = p.f.row::<CGB>(col);
    fill_partial::<MIX>(out, obj, w << (2 * (col & 7)), attr, p.master, p.colors);
}

/// Part of a tile: the pixels of the decoded row `w` from bits 15-14 on, of
/// a tile with attributes `attr` (palette; priority with `master`, see
/// [`prio_mask`]), mixed with `obj` with `MIX`.
fn fill_partial<const MIX: bool>(
    out: &mut [u16],
    obj: &[u8],
    mut w: u16,
    attr: u8,
    master: u8,
    colors: &Colors,
) {
    let bg = &colors.bg[usize::from(attr & 7)];
    if MIX {
        let m = prio_mask(attr, master);
        for (p, &o) in out.iter_mut().zip(obj) {
            *p = mix((w >> 14) as u8, o, m, bg, colors);
            w <<= 2;
        }
    } else {
        for p in out {
            *p = bg[usize::from(w >> 14)];
            w <<= 2;
        }
    }
}

/// 0x01 in every byte of a u64.
const BYTES_01: u64 = 0x0101_0101_0101_0101;
const BYTES_80: u64 = 0x8080_8080_8080_8080;

/// The 2-bit fields of a decoded tile row ([`decode`]) one per byte:
/// field i (pixel 7 - i) to byte i, so `to_be_bytes` gives the pixels left
/// to right.
#[inline]
fn spread_bytes(w: u16) -> u64 {
    let mut x = u64::from(w);
    x = (x | (x << 24)) & 0x0000_00FF_0000_00FF;
    x = (x | (x << 12)) & 0x000F_000F_000F_000F;
    (x | (x << 6)) & 0x0303_0303_0303_0303
}

/// 0xFF in each byte whose color bits (1-0) are non-zero, else 0x00.
#[inline]
fn opaque_bytes(v: u64) -> u64 {
    ((v | (v >> 1)) & BYTES_01) * 0xFF
}

/// BG/window tile data addressing: the tile at `index` is at VRAM offset
/// `base + 16 * (index ^ flip)`. LCDC.4 = 1: 8000 + 16 * index; LCDC.4 = 0:
/// 9000 + 16 * (signed index), i.e. 8800-97FF (Pan Docs: VRAM Tile Data),
/// where `index ^ 0x80` maps -128..127 to 0..255 from 8800.
#[inline]
fn tile_data(lcdc: u8) -> (usize, u8) {
    if lcdc & LCDC_TILES_8000 != 0 {
        (0, 0)
    } else {
        (0x0800, 0x80)
    }
}

/// Spreads the 8 bits of `b` to the even bits of a u16 (bit i to bit 2i).
#[inline]
const fn spread(b: u8) -> u16 {
    let mut x = b as u16;
    x = (x | (x << 4)) & 0x0F0F;
    x = (x | (x << 2)) & 0x3333;
    (x | (x << 1)) & 0x5555
}

/// [`spread`] of every byte, and of every byte mirrored (X flip; 1 KiB of
/// flash): one load per bit plane instead of ~10 operations.
static SPREAD: [[u16; 256]; 2] = {
    let mut t = [[0u16; 256]; 2];
    let mut i = 0;
    while i < 256 {
        t[0][i] = spread(i as u8);
        t[1][i] = spread((i as u8).reverse_bits());
        i += 1;
    }
    t
};

/// Interleaves a tile row's two bit planes (Pan Docs: Tile Data) into 2-bit
/// color indices, leftmost pixel in bits 15-14.
#[inline]
fn decode(lo: u8, hi: u8) -> u16 {
    SPREAD[0][usize::from(lo)] | (SPREAD[0][usize::from(hi)] << 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::objects::LineObjects;

    #[test]
    fn set_pair_color_matches_a_rebuilt_table() {
        for c in 0..4 {
            let mut pal = [0x1111, 0x2222, 0x3333, 0x4444];
            let mut t = pair_table(&pal);
            pal[c] = 0x7ABC;
            set_pair_color(&mut t, c, 0x7ABC);
            assert_eq!(t, pair_table(&pal), "color {c}");
        }
    }

    // Distinct colors per palette and shade: BG shade s = 100 + s, OBJ0
    // 200 + s, OBJ1 300 + s.
    const PAL: DmgPalettes = DmgPalettes {
        bg: [100, 101, 102, 103],
        obj0: [200, 201, 202, 203],
        obj1: [300, 301, 302, 303],
    };
    const IDENTITY: u8 = 0xE4;

    struct Scene {
        vram: [u8; 0x4000],
        oam: [u8; 0xA0],
        colors: Colors,
        regs: Regs,
        r: Renderer,
        obj: ObjBuf,
    }

    impl Scene {
        fn new() -> Self {
            let mut r = Renderer::new();
            r.start_frame();
            Self {
                vram: [0; 0x4000],
                oam: [0; 0xA0],
                colors: Colors::dmg_compat(&PAL, IDENTITY, IDENTITY, IDENTITY),
                // LCD, BG on, tiles at 8000, objects on.
                regs: Regs {
                    lcdc: 0x93,
                    ..Regs::default()
                },
                r,
                obj: ObjBuf::NEW,
            }
        }

        /// Sets row `row` of tile data at VRAM offset `addr` to colors
        /// `px` (left to right).
        fn tile_row(&mut self, addr: usize, row: usize, px: [u8; 8]) {
            let (mut lo, mut hi) = (0u8, 0u8);
            for (i, &c) in px.iter().enumerate() {
                lo |= (c & 1) << (7 - i);
                hi |= ((c >> 1) & 1) << (7 - i);
            }
            self.vram[addr + row * 2] = lo;
            self.vram[addr + row * 2 + 1] = hi;
        }

        fn solid_tile(&mut self, addr: usize, color: u8) {
            for row in 0..8 {
                self.tile_row(addr, row, [color; 8]);
            }
        }

        fn obj(&mut self, i: usize, y: u8, x: u8, tile: u8, flags: u8) {
            self.oam[i * 4..i * 4 + 4].copy_from_slice(&[y, x, tile, flags]);
        }

        /// Starts line `ly` (WY check, OAM scan) without drawing.
        fn begin(&mut self, ly: u8) {
            self.r.check_wy(ly, &self.regs);
            let objs = LineObjects::scan(&self.oam, ly, self.regs.lcdc & LCDC_OBJ_TALL != 0);
            // As `Ppu::window_wx` at the line start.
            let r = &self.regs;
            let on = r.lcdc & LCDC_WIN_ON != 0 && self.r.wy_triggered && r.wx <= 166;
            let order = objs.priority_order();
            self.r
                .start_line(ly, r.scx, &objs, order, on.then_some(r.wx));
        }

        fn span(&mut self, row: &mut [u16; W], x0: usize, x1: usize) {
            let src = Sources {
                vram: &self.vram,
                oam: &self.oam,
                attrs: None,
                colors: &self.colors,
                pairs: &core::array::from_fn(|p| pair_table(&self.colors.bg[p])),
            };
            self.r.render(row, x0, x1, &self.regs, &src, &mut self.obj);
        }

        fn line(&mut self, ly: u8) -> [u16; W] {
            let mut row = [0xFFFF; W];
            self.begin(ly);
            self.span(&mut row, 0, W);
            row
        }
    }

    /// Asserts `row[from..to]` is all `color`.
    #[track_caller]
    fn run(row: &[u16; W], from: usize, to: usize, color: u16) {
        for (x, &p) in row.iter().enumerate().take(to).skip(from) {
            assert_eq!(p, color, "pixel {x}");
        }
    }

    #[test]
    fn decode_tile_row() {
        // Pan Docs Tile Data example: 3C 7E -> 0 2 3 3 3 3 2 0.
        let w = decode(0x3C, 0x7E);
        let px: [u16; 8] = core::array::from_fn(|i| (w >> (14 - 2 * i)) & 3);
        assert_eq!(px, [0, 2, 3, 3, 3, 3, 2, 0]);
    }

    #[test]
    fn bg_scroll_wraps_in_x_and_y() {
        let mut s = Scene::new();
        s.tile_row(0x10, 4, [3; 8]); // tile 1: only row 4 is color 3
        s.solid_tile(0x20, 1); // tile 2
        s.solid_tile(0x30, 2); // tile 3
        let mut t4 = [1; 8];
        t4[4..].fill(2);
        for row in 0..8 {
            s.tile_row(0x40, row, t4); // tile 4: left half 1, right half 2
        }
        s.vram[0x1800 + 31 * 32 + 31] = 1; // map row 31, column 31
        s.vram[0x1800 + 31 * 32] = 2; // row 31, column 0
        s.vram[0x1800 + 31] = 3; // row 0, column 31
        s.vram[0x1800] = 4; // row 0, column 0
        s.regs.scx = 252;
        s.regs.scy = 252;
        // LY 0: map y 252 (row 31, fine row 4); x 0-3 = map x 252-255.
        let row = s.line(0);
        run(&row, 0, 4, 103);
        run(&row, 4, 12, 101);
        run(&row, 12, W, 100);
        // LY 1: fine row 5 of tile 1 is blank.
        let row = s.line(1);
        run(&row, 0, 4, 100);
        // LY 4: map y 0 wraps to row 0.
        let row = s.line(4);
        run(&row, 0, 4, 102);
        run(&row, 4, 8, 101);
        run(&row, 8, 12, 102);
        run(&row, 12, W, 100);
    }

    #[test]
    fn bg_map_select() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.vram[0x1800] = 1;
        s.vram[0x1C00] = 2;
        run(&s.line(0), 0, 8, 101);
        s.regs.lcdc |= LCDC_BG_MAP;
        run(&s.line(0), 0, 8, 102);
    }

    #[test]
    fn bg_tile_data_signed_and_unsigned() {
        let mut s = Scene::new();
        // Index 0x01: 8010 (unsigned) or 9010 (signed). Index 0x7F: 87F0 or
        // 97F0. Index 0x80: 8800 in both modes.
        s.solid_tile(0x0010, 1);
        s.solid_tile(0x1010, 2);
        s.solid_tile(0x07F0, 3);
        s.solid_tile(0x17F0, 1);
        s.solid_tile(0x0800, 2);
        s.vram[0x1800..0x1803].copy_from_slice(&[0x01, 0x7F, 0x80]);
        let row = s.line(0);
        run(&row, 0, 8, 101);
        run(&row, 8, 16, 103);
        run(&row, 16, 24, 102);
        s.regs.lcdc &= !LCDC_TILES_8000;
        let row = s.line(0);
        run(&row, 0, 8, 102);
        run(&row, 8, 16, 101);
        run(&row, 16, 24, 102);
    }

    #[test]
    fn bgp_maps_bg_colors() {
        let mut s = Scene::new();
        s.tile_row(0x00, 0, [0, 1, 2, 3, 0, 1, 2, 3]);
        s.colors.set_bgp(&PAL, 0b00_01_10_11); // reversed
        let row = s.line(0);
        assert_eq!(row[..4], [103, 102, 101, 100]);
    }

    /// Window map 9C00: row 0 = tile 1 (color 1), tile 2 (color 2); row 1 =
    /// tile 3 (color 3). BG all color 0.
    fn window_scene() -> Scene {
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.solid_tile(0x30, 3);
        let mut t4 = [1; 8];
        t4[4..].fill(2);
        for row in 0..8 {
            s.tile_row(0x40, row, t4);
        }
        s.vram[0x1C00] = 1;
        s.vram[0x1C01] = 2;
        s.vram[0x1C20] = 3;
        s.regs.lcdc |= LCDC_WIN_ON | LCDC_WIN_MAP;
        s
    }

    #[test]
    fn window_position() {
        let mut s = window_scene();
        s.regs.wy = 10;
        s.regs.wx = 27;
        for ly in 0..10 {
            run(&s.line(ly), 0, W, 100);
        }
        let row = s.line(10);
        run(&row, 0, 20, 100);
        run(&row, 20, 28, 101);
        run(&row, 28, 36, 102);
        // The WY condition holds for the rest of the frame.
        s.regs.wy = 200;
        let row = s.line(11);
        run(&row, 20, 28, 101);
        // Next frame: WY moved to a line already passed never matches.
        s.r.start_frame();
        for ly in 0..5 {
            run(&s.line(ly), 0, W, 100);
        }
        s.regs.wy = 2;
        for ly in 5..10 {
            run(&s.line(ly), 0, W, 100);
        }
    }

    #[test]
    fn window_line_counter_pauses_when_hidden() {
        let mut s = window_scene();
        s.regs.wx = 7;
        // Lines 0-3: window rows 0-3.
        for ly in 0..4 {
            run(&s.line(ly), 0, 8, 101);
        }
        // Lines 4-6 hidden by LCDC.5, lines 7-9 by WX > 166.
        s.regs.lcdc &= !LCDC_WIN_ON;
        for ly in 4..7 {
            run(&s.line(ly), 0, W, 100);
        }
        s.regs.lcdc |= LCDC_WIN_ON;
        s.regs.wx = 167;
        for ly in 7..10 {
            run(&s.line(ly), 0, W, 100);
        }
        // Lines 10-13: window rows 4-7 (tile 1); line 14: row 8 (tile 3).
        s.regs.wx = 7;
        for ly in 10..14 {
            run(&s.line(ly), 0, 8, 101);
        }
        run(&s.line(14), 0, 8, 103);
        // A new frame restarts at row 0.
        s.r.start_frame();
        run(&s.line(0), 0, 8, 101);
    }

    #[test]
    fn window_wx_edges() {
        let mut s = window_scene();
        s.vram[0x1C00] = 4; // left half 1, right half 2
        // WX = 3: 4 pixels cut off.
        s.regs.wx = 3;
        let row = s.line(0);
        run(&row, 0, 4, 102);
        run(&row, 4, 12, 102);
        run(&row, 12, W, 100);
        // WX = 0 with SCX & 7 = 2: 7 + 2 pixels cut off (Pan Docs).
        s.regs.wx = 0;
        s.regs.scx = 2;
        let row = s.line(1);
        run(&row, 0, 7, 102);
        run(&row, 7, 15, 100);
        // WX = 166: one window pixel at x = 159.
        s.regs.scx = 0;
        s.regs.wx = 166;
        let row = s.line(2);
        run(&row, 0, 159, 100);
        assert_eq!(row[159], 101);
    }

    #[test]
    fn window_switched_off_mid_line() {
        let mut s = window_scene();
        s.solid_tile(0x00, 1); // BG tile 0 is now color 1...
        s.vram[0x1C00..0x1C20].fill(3); // ...and the window color 3.
        s.regs.wx = 7;
        let mut row = [0; W];
        s.begin(0);
        s.span(&mut row, 0, 40);
        s.regs.lcdc &= !LCDC_WIN_ON;
        s.span(&mut row, 40, 80);
        // Re-enabling after the WX match does not restart it.
        s.regs.lcdc |= LCDC_WIN_ON;
        s.span(&mut row, 80, W);
        run(&row, 0, 40, 103);
        run(&row, 40, W, 101);
    }

    #[test]
    fn lcdc0_off_blanks_bg_and_window_to_color_0() {
        let mut s = window_scene();
        s.solid_tile(0x00, 2);
        s.obj(0, 16, 40, 3, ATTR_BEHIND_BG);
        s.regs.wx = 87;
        s.regs.lcdc &= !LCDC_BG_ON;
        s.colors.set_bgp(&PAL, 0xE5); // color 0 -> shade 1
        let row = s.line(0);
        // Objects still draw, over BG regardless of the priority bit.
        run(&row, 0, 32, 101);
        run(&row, 32, 40, 203);
        run(&row, 40, W, 101);
        // The window was still activated on line 0, so line 8 draws
        // window row 8 (tile 3) rather than row 7 (tile 1).
        s.regs.lcdc |= LCDC_BG_ON;
        for ly in 1..8 {
            s.line(ly);
        }
        let row = s.line(8);
        run(&row, 80, 88, 103);
    }

    #[test]
    fn object_flips_and_clipping() {
        let mut s = Scene::new();
        // Tile 1: row 0 = 3 1 1 1 1 1 1 1, rows 1-7 = 3 0 0 0 0 0 0 0.
        s.tile_row(0x10, 0, [3, 1, 1, 1, 1, 1, 1, 1]);
        for row in 1..8 {
            s.tile_row(0x10, row, [3, 0, 0, 0, 0, 0, 0, 0]);
        }
        s.obj(0, 16, 8, 1, 0); // x 0-7
        s.obj(1, 16, 28, 1, ATTR_X_FLIP); // x 20-27
        s.obj(2, 16, 48, 1, ATTR_Y_FLIP); // x 40-47
        let row = s.line(0);
        assert_eq!(row[0], 203);
        run(&row, 1, 8, 201);
        run(&row, 20, 27, 201);
        assert_eq!(row[27], 203);
        assert_eq!(row[40], 203);
        run(&row, 41, 48, 100);
        let row = s.line(7);
        assert_eq!(row[0], 203);
        run(&row, 1, 8, 100);
        assert_eq!(row[27], 203);
        assert_eq!(row[40], 203);
        run(&row, 41, 48, 201);
        // X = 4: the left 4 columns are off-screen.
        s.oam.fill(0);
        s.obj(0, 24, 4, 1, 0);
        let row = s.line(8);
        run(&row, 0, 4, 201);
        run(&row, 4, W, 100);
    }

    #[test]
    fn tall_objects_ignore_tile_bit_0() {
        let mut s = Scene::new();
        s.solid_tile(0x40, 1); // tile 4
        s.solid_tile(0x50, 2); // tile 5
        s.solid_tile(0x60, 3); // tile 6
        s.regs.lcdc |= LCDC_OBJ_TALL;
        s.obj(0, 16, 8, 5, 0);
        s.obj(1, 16, 20, 5, ATTR_Y_FLIP);
        for ly in 0..8 {
            let row = s.line(ly);
            run(&row, 0, 8, 201);
            run(&row, 12, 20, 202);
        }
        for ly in 8..16 {
            let row = s.line(ly);
            run(&row, 0, 8, 202);
            run(&row, 12, 20, 201);
        }
        run(&s.line(16), 0, W, 100);
    }

    #[test]
    fn object_palettes_and_transparency() {
        let mut s = Scene::new();
        s.tile_row(0x10, 0, [0, 1, 2, 3, 0, 1, 2, 3]);
        s.tile_row(0x00, 0, [2; 8]); // BG color 2 behind
        s.obj(0, 16, 8, 1, 0);
        s.obj(1, 16, 16, 1, ATTR_DMG_OBP1);
        s.colors.set_obp(&PAL, 0, 0xE4);
        s.colors.set_obp(&PAL, 1, 0x1B); // reversed
        let row = s.line(0);
        assert_eq!(row[..8], [102, 201, 202, 203, 102, 201, 202, 203]);
        assert_eq!(row[8..16], [102, 302, 301, 300, 102, 302, 301, 300]);
    }

    #[test]
    fn pending_obj_clear_survives_a_line_without_render() {
        // Line 0 fetches an object; line 1 latches but draws nothing (LCDC.7
        // cleared before its first pixel). The clear owed to line 0's
        // objects must still happen on the next line that renders.
        let mut s = Scene::new();
        s.solid_tile(0x10, 3);
        s.obj(0, 16, 8, 1, 0); // line 0, screen x 0..8
        s.line(0);
        s.obj(0, 0, 0, 1, 0); // off screen from now on
        s.begin(1); // latched, never rendered
        s.obj(1, 18, 88, 1, 0); // line 2, screen x 80..88
        let row = s.line(2);
        run(&row, 0, 8, 100);
        run(&row, 80, 88, 203);
    }

    #[test]
    fn object_behind_bg() {
        let mut s = Scene::new();
        s.tile_row(0x00, 0, [0, 0, 0, 0, 1, 2, 3, 0]);
        s.solid_tile(0x10, 3);
        s.obj(0, 16, 8, 1, ATTR_BEHIND_BG);
        // BG color 0 maps to the darkest shade but is still index 0.
        s.colors.set_bgp(&PAL, 0xE7);
        let row = s.line(0);
        assert_eq!(row[..8], [203, 203, 203, 203, 101, 102, 103, 203]);
    }

    #[test]
    fn objects_disabled_by_lcdc1() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 3);
        s.obj(0, 16, 8, 1, 0);
        s.regs.lcdc &= !LCDC_OBJ_ON;
        run(&s.line(0), 0, W, 100);
    }

    #[test]
    fn ten_objects_per_line() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 3);
        // Two off-screen objects and eight visible ones take the 10 slots;
        // the 11th (entry 10, x 100-107) is not drawn.
        s.obj(0, 16, 0, 1, 0);
        s.obj(1, 16, 170, 1, 0);
        for i in 2..11 {
            s.obj(i, 16, 8 + 10 * i as u8, 1, 0);
        }
        let row = s.line(0);
        for i in 2..10 {
            run(&row, 10 * i, 10 * i + 8, 203);
        }
        run(&row, 100, W, 100);
    }

    #[test]
    fn object_priority_by_x_then_oam_order() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.solid_tile(0x30, 3);
        // Entry 0 at x 22-29 loses to entry 1 at x 20-27 (smaller X).
        s.obj(0, 16, 30, 1, 0);
        s.obj(1, 16, 28, 2, 0);
        // Equal X: entry 2 beats entry 3.
        s.obj(2, 16, 58, 2, 0);
        s.obj(3, 16, 58, 3, 0);
        let row = s.line(0);
        run(&row, 20, 28, 202);
        run(&row, 28, 30, 201);
        run(&row, 50, 58, 202);
    }

    #[test]
    fn behind_bg_winner_hides_lower_priority_object() {
        // Object priority is resolved before BG priority: the winning
        // object is behind the BG, so the BG shows even where a lower
        // priority object without the bit is opaque (Pan Docs: OAM, "Object
        // priority"; SameBoy keeps only the winning object's pixel).
        let mut s = Scene::new();
        s.solid_tile(0x00, 1);
        s.solid_tile(0x10, 3);
        s.obj(0, 16, 20, 1, 0); // x 12-19
        s.obj(1, 16, 16, 1, ATTR_BEHIND_BG); // x 8-15, wins on 12-15
        let row = s.line(0);
        run(&row, 8, 16, 101);
        run(&row, 16, 20, 203);
    }

    #[test]
    fn scx_fine_bits_latched_at_line_start() {
        // Pan Docs: Scrolling, the low 3 bits of SCX are only read at the
        // start of the line; a later write moves only the tile column.
        let mut s = Scene::new();
        let mut t = [1; 8];
        t[4..].fill(2);
        s.tile_row(0x00, 0, t); // tile 0 row 0: left half 1, right half 2
        let mut row = [0; W];
        s.begin(0);
        s.span(&mut row, 0, 40);
        s.regs.scx = 3;
        s.span(&mut row, 40, W);
        for x in (0..W).step_by(8) {
            assert_eq!(
                row[x..x + 8],
                [101, 101, 101, 101, 102, 102, 102, 102],
                "x {x}"
            );
        }
    }

    #[test]
    fn bg_column_with_constant_scx_is_scx_plus_x() {
        for scx in 0..=255u8 {
            for x in 0..=255u8 {
                assert_eq!(bg_column(scx, scx & 7, x), scx.wrapping_add(x), "{scx} {x}");
            }
        }
    }

    #[test]
    fn first_tile_column_ignores_a_held_scx_below_the_moved_discard() {
        // Line-start SCX 3; a write of 0x67 inside the discard moved it to
        // 7 (`set_fine`) after tile 0's map entry was read with SCX 3
        // (held up to x 1). Tile 0 is map column 0 (its pixel 7 at x 0),
        // not column 31 by a borrow; tile 1 on is column 13 (0x67 / 8 + 1).
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.solid_tile(0x30, 3);
        s.vram[0x1800] = 1;
        s.vram[0x1800 + 31] = 2;
        s.vram[0x1800 + 13] = 3;
        let mut row = [0; W];
        s.regs.scx = 3;
        s.begin(0);
        s.r.set_fine(7);
        s.regs.scx = 0x67;
        s.r.hold[2] = Held {
            end: 1,
            lcdc: s.regs.lcdc,
            scx: 3,
            scy: 0,
            mixed: Mixed::NONE,
        };
        s.span(&mut row, 0, W);
        run(&row, 0, 1, 101);
        run(&row, 1, 9, 103);
        run(&row, 9, 17, 100);
    }

    #[test]
    fn scx_low_bits_carry_into_the_tile_column() {
        // Line-start SCX 3 (fine 3): map column 9 (pixels 72-79) is at
        // screen x 69-76. At x 40 SCX changes: the tile column comes from
        // the whole new SCX minus the line-start fine scroll, so low bits
        // below 3 borrow one tile (SameBoy `advance_fetcher_state_machine`).
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.vram[0x1800..0x1820].fill(1);
        s.vram[0x1800 + 9] = 2;
        for (scx, from) in [(3, 69), (7, 69), (2, 77), (0, 77), (11, 61)] {
            let mut row = [0; W];
            s.regs.scx = 3;
            s.begin(0);
            s.span(&mut row, 0, 40);
            s.regs.scx = scx;
            s.span(&mut row, 40, W);
            run(&row, 40, from, 101);
            run(&row, from, from + 8, 102);
            run(&row, from + 8, W, 101);
        }
    }

    #[test]
    fn mid_line_palette_and_scroll_changes() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.vram[0x1800..0x1820].fill(1);
        s.vram[0x1800 + 10] = 2;
        let mut row = [0; W];
        s.begin(0);
        s.span(&mut row, 0, 40);
        s.colors.set_bgp(&PAL, 0xEC); // color 1 -> shade 3
        s.regs.scx = 8; // map column 10 (tile 2) is now at x 72-79
        s.span(&mut row, 40, W);
        run(&row, 0, 40, 101);
        run(&row, 40, 72, 103);
        run(&row, 72, 80, 102);
        run(&row, 80, W, 103);
    }

    /// A CGB-mode scene: BG palette p color c is 1000 + 10p + c; the
    /// objects keep the compatibility palettes (OBJ0 200 + s).
    fn cgb_scene() -> Scene {
        let mut s = Scene::new();
        s.r.cgb = true;
        for (p, pal) in s.colors.bg.iter_mut().enumerate() {
            for (c, col) in pal.iter_mut().enumerate() {
                *col = (1000 + 10 * p + c) as u16;
            }
        }
        s
    }

    #[test]
    fn cgb_bg_attributes_select_palette_and_tile_bank() {
        let mut s = cgb_scene();
        s.solid_tile(0x0010, 1); // tile 1, bank 0
        s.solid_tile(0x2010, 2); // tile 1, bank 1
        s.vram[0x1800..0x1803].copy_from_slice(&[1, 1, 1]);
        // Attributes (bank 1 at the map address): palette 0 bank 0,
        // palette 3 bank 1, palette 7 bank 0.
        s.vram[0x3800..0x3803].copy_from_slice(&[0x00, 0x0B, 0x07]);
        let row = s.line(0);
        run(&row, 0, 8, 1001);
        run(&row, 8, 16, 1032);
        run(&row, 16, 24, 1071);
        // The window reads its map's attributes the same way.
        s.regs.lcdc |= LCDC_WIN_ON | LCDC_WIN_MAP;
        s.regs.wx = 87;
        s.vram[0x1C00] = 1;
        s.vram[0x3C00] = 0x0D; // palette 5, bank 1
        let row = s.line(0);
        run(&row, 8, 16, 1032);
        run(&row, 80, 88, 1052);
    }

    #[test]
    fn cgb_bg_attributes_flip_tiles() {
        let mut s = cgb_scene();
        s.tile_row(0x10, 0, [1, 0, 0, 0, 0, 0, 0, 2]);
        s.tile_row(0x10, 7, [3, 3, 0, 0, 0, 0, 0, 0]);
        s.vram[0x1801..0x1805].fill(1); // x 8-39
        s.vram[0x3801..0x3805].copy_from_slice(&[0x00, 0x20, 0x40, 0x60]);
        // Odd SCX: the flipped tiles are also drawn as partial tiles.
        for scx in [0, 3] {
            s.regs.scx = scx;
            let row = s.line(0);
            let at = |x: usize| row[x - usize::from(scx)];
            assert_eq!([at(8), at(15)], [1001, 1002], "plain, scx {scx}");
            assert_eq!([at(16), at(23)], [1002, 1001], "x flip, scx {scx}");
            assert_eq!([at(24), at(25), at(26)], [1003, 1003, 1000], "y flip");
            assert_eq!([at(37), at(38), at(39)], [1000, 1003, 1003], "x+y flip");
        }
    }

    #[test]
    fn cgb_bg_priority_attribute() {
        let mut s = cgb_scene();
        s.tile_row(0x10, 0, [0, 0, 0, 0, 2, 2, 2, 2]);
        s.solid_tile(0x20, 3);
        s.vram[0x1800..0x1803].fill(1);
        s.vram[0x3800..0x3803].copy_from_slice(&[0x80, 0x00, 0x00]);
        s.obj(0, 16, 8, 2, 0); // x 0-7, over the BG-priority tile
        s.obj(1, 16, 16, 2, 0); // x 8-15, plain tile, plain object
        s.obj(2, 16, 24, 2, ATTR_BEHIND_BG); // x 16-23, OAM behind-BG bit
        let row = s.line(0);
        // BG priority: BG color 0 still shows the object.
        assert_eq!(row[..8], [203, 203, 203, 203, 1002, 1002, 1002, 1002]);
        run(&row, 8, 16, 203);
        assert_eq!(row[16..24], [203, 203, 203, 203, 1002, 1002, 1002, 1002]);
    }

    #[test]
    fn cgb_lcdc0_clear_puts_objects_on_top_and_keeps_the_bg() {
        let mut s = cgb_scene();
        s.solid_tile(0x10, 2);
        s.solid_tile(0x20, 3);
        s.vram[0x1800..0x1820].fill(1);
        s.vram[0x3800] = 0x84; // BG priority, palette 4
        s.obj(0, 16, 8, 2, 0);
        s.obj(1, 16, 16, 2, ATTR_BEHIND_BG);
        s.regs.lcdc &= !LCDC_BG_ON;
        let row = s.line(0);
        run(&row, 0, 16, 203);
        run(&row, 16, W, 1002);
        // Master priority on: both priority bits hide the objects again.
        s.regs.lcdc |= LCDC_BG_ON;
        let row = s.line(0);
        run(&row, 0, 8, 1042);
        run(&row, 8, 16, 1002);
    }

    #[test]
    fn dmg_compat_mode_ignores_bank_1_attributes() {
        let mut s = Scene::new();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x2010, 2);
        s.tile_row(0x20, 0, [1, 0, 0, 0, 0, 0, 0, 2]);
        s.solid_tile(0x30, 3);
        s.vram[0x1800..0x1803].copy_from_slice(&[1, 2, 1]);
        s.vram[0x3800..0x3803].copy_from_slice(&[0xEF, 0xEF, 0xEF]);
        s.obj(0, 16, 24, 3, 0);
        let row = s.line(0);
        run(&row, 0, 8, 101);
        assert_eq!([row[8], row[15]], [101, 102]);
        run(&row, 16, 24, 203);
    }

    /// Deterministic pseudo-random bytes (64-bit LCG).
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u8 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 56) as u8
        }
    }

    /// [`cgb_scene`] with CGB OBJ palette p color c = 2000 + 10p + c.
    fn cgb_obj_scene() -> Scene {
        let mut s = cgb_scene();
        for (p, pal) in s.colors.obj.iter_mut().enumerate() {
            for (c, col) in pal.iter_mut().enumerate() {
                *col = (2000 + 10 * p + c) as u16;
            }
        }
        s
    }

    #[test]
    fn cgb_object_palette_and_tile_bank() {
        // Pan Docs: OAM byte 3 in CGB mode: bits 0-2 OBJ palette, bit 3
        // VRAM bank, bit 4 (DMG palette) ignored.
        let mut s = cgb_obj_scene();
        s.solid_tile(0x0010, 1); // tile 1, bank 0
        s.solid_tile(0x2010, 2); // tile 1, bank 1
        s.obj(0, 16, 8, 1, 0x03); // palette 3, bank 0: x 0-7
        s.obj(1, 16, 16, 1, 0x0D); // palette 5, bank 1: x 8-15
        s.obj(2, 16, 24, 1, 0x17); // palette 7, bank 0, bit 4 set
        let row = s.line(0);
        run(&row, 0, 8, 2031);
        run(&row, 8, 16, 2052);
        run(&row, 16, 24, 2071);
        run(&row, 24, W, 1000);
    }

    #[test]
    fn cgb_object_priority_by_oam_index() {
        // CGB mode: the lower OAM index wins, whatever the X (Pan Docs:
        // Object Priority and Conflicts; SameBoy fifo_overlay_object_row).
        let mut s = cgb_obj_scene();
        s.solid_tile(0x10, 1);
        s.solid_tile(0x20, 2);
        s.obj(0, 16, 30, 1, 0); // x 22-29, palette 0
        s.obj(1, 16, 28, 2, 1); // x 20-27, palette 1, fetched first
        s.obj(2, 16, 58, 2, 0); // equal X: entry 2 beats entry 3
        s.obj(3, 16, 58, 1, 1);
        let row = s.line(0);
        run(&row, 20, 22, 2012);
        run(&row, 22, 30, 2001);
        run(&row, 50, 58, 2002);
        // A span boundary between the two fetches (a mid-line register
        // write): entry 0, fetched in the second span, still takes the
        // pixels not output yet from entry 1.
        for split in [21, 22] {
            let mut row = [0xFFFF; W];
            s.begin(0);
            s.span(&mut row, 0, split);
            s.span(&mut row, split, W);
            run(&row, 20, 22, 2012);
            run(&row, 22, 30, 2001);
        }
    }

    #[test]
    fn cgb_bg_priority_comes_from_the_oam_index_winner() {
        // Entry 0 (behind BG) wins x 12-15 over entry 1 (fetched first):
        // the BG shows there (only the winning pixel is kept).
        let mut s = cgb_obj_scene();
        s.solid_tile(0x00, 1);
        s.solid_tile(0x10, 3);
        s.obj(0, 16, 20, 1, ATTR_BEHIND_BG); // x 12-19
        s.obj(1, 16, 16, 1, 1); // x 8-15
        let row = s.line(0);
        run(&row, 8, 12, 2013);
        run(&row, 12, 20, 1001);
        // LCDC.0 clear: objects on top, entry 0's colors on 12-19.
        s.regs.lcdc &= !LCDC_BG_ON;
        let row = s.line(0);
        run(&row, 8, 12, 2013);
        run(&row, 12, 20, 2003);
    }

    #[test]
    fn split_spans_match_one_span() {
        // Seeds 64-127: CGB mode, with attributes in VRAM bank 1, random BG
        // palettes and LCDC.0 either way.
        for seed in 0..128u64 {
            let cgb = seed >= 64;
            let mut rng = Lcg(seed);
            let mut s = Scene::new();
            let vram_len = if cgb { 0x4000 } else { 0x2000 };
            s.vram[..vram_len].fill_with(|| rng.next());
            s.oam.fill_with(|| rng.next());
            // Keep objects mostly on the first 40 lines so many overlap.
            for i in 0..40 {
                s.oam[i * 4] %= 60;
                s.oam[i * 4 + 1] %= 176;
            }
            s.regs = Regs {
                lcdc: 0x80 | (rng.next() & 0x7F) | 0x01,
                scy: rng.next(),
                scx: rng.next(),
                wy: rng.next() % 30,
                wx: rng.next() % 170,
            };
            s.colors = Colors::dmg_compat(&PAL, rng.next(), rng.next(), rng.next());
            if cgb {
                s.r.cgb = true;
                s.regs.lcdc &= !LCDC_BG_ON | rng.next();
                for c in s.colors.bg.as_flattened_mut() {
                    *c = u16::from_le_bytes([rng.next(), rng.next()]) & 0x7FFF;
                }
            }
            let mut split = Scene {
                vram: s.vram,
                oam: s.oam,
                colors: s.colors,
                regs: s.regs,
                r: Renderer::new(),
                obj: ObjBuf::NEW,
            };
            split.r.start_frame();
            split.r.cgb = cgb;
            for ly in 0..48 {
                let whole = s.line(ly);
                let mut row = [0; W];
                split.begin(ly);
                let mut x = 0;
                while x < W {
                    let n = usize::from(rng.next() % 12);
                    let end = (x + n).min(W);
                    split.span(&mut row, x, end);
                    x = end;
                }
                assert_eq!(row, whole, "seed {seed} line {ly}");
            }
        }
    }
}
