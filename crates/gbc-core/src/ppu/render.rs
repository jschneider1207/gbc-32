//! Scanline renderer (ADR 0002): draws pixels `[x0, x1)` of the current line
//! from VRAM, OAM, a register snapshot and resolved RGB555 color tables
//! (ADR 0013). The timing code calls [`Renderer::render`] once per line at
//! the end of mode 3, and additionally up to the current pixel before a
//! register write that changes the picture, so mid-line effects land on the
//! right pixel. Splitting a line into spans gives exactly the same pixels
//! as one span when no register changes between them (tested below).
//!
//! Scope: DMG compatibility mode on a CGB (M3). The pixel formats already
//! carry CGB fields (BG attribute palette and priority, OBJ palette 0-7),
//! and the color tables hold 8 palettes each, so CGB mode (M5) adds tile
//! attributes, VRAM bank 1 and OAM-index priority without changing the
//! mixing loop's shape.
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
//! per BG-only line, 2.0k with objects and the window (cgb-acid2).
//!
//! Open hardware choices and the evidence (Mealybug tearoom CGB C
//! screenshots, stretch tier):
//! - Clearing LCDC.5 doesn't reset the WY condition (SameBoy; Pan Docs
//!   says it does on GBC): resetting it fails `m2_win_en_toggle`, which
//!   passes as is.
//! - LCDC.0 clear keeps the window activation and line counter, color 0
//!   through BGP: `m3_lcdc_bg_en_change`, `m3_lcdc_bg_en_change2` pass.
//! - WX = 0 loses SCX & 7 pixels (Pan Docs). `m3_window_timing_wx_0` is
//!   one pixel off on lines with SCX & 7 != 0, so the hardware rule has
//!   one more step; unresolved.
//! - Mid-line SCX writes keep the line-start SCX & 7 (Pan Docs).
//!   `m3_scx_low_3_bits` differs at pixels 152-159, which this doesn't
//!   model; unresolved.
//! - Mid-line writes to the tile map/data selects, SCY and object
//!   attributes take effect per pixel here, per fetch on hardware
//!   (`m3_lcdc_*_change`, `m3_scy_change*` fail); M3 doesn't need it.

use super::objects::{LineObjects, MAX_PER_LINE};
use super::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH};
use crate::DmgPalettes;

const W: usize = SCREEN_WIDTH;

// LCDC bits (Pan Docs: LCDC).
const LCDC_BG_ON: u8 = 0x01; // Non-CGB mode: BG and window enable.
const LCDC_OBJ_ON: u8 = 0x02;
const LCDC_OBJ_TALL: u8 = 0x04;
const LCDC_BG_MAP: u8 = 0x08;
const LCDC_TILES_8000: u8 = 0x10;
const LCDC_WIN_ON: u8 = 0x20;
const LCDC_WIN_MAP: u8 = 0x40;

// OAM attribute bits (Pan Docs: OAM, byte 3).
const ATTR_BEHIND_BG: u8 = 0x80;
const ATTR_Y_FLIP: u8 = 0x40;
const ATTR_X_FLIP: u8 = 0x20;
const ATTR_DMG_OBP1: u8 = 0x10;

/// The object line buffer covers screen x -8..168, so an object at any X
/// below 168 is written without clipping: buffer index = screen x + 8.
pub(crate) const OBJ_BUF_LEN: usize = W + 16;

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

    /// CGB mode: BG palette `pal` from palette RAM `ram` (BCPD). Colors are
    /// little-endian RGB555, the frame's format (ADR 0013); bit 15 is not
    /// a color bit (Pan Docs: LCD Color Palettes, "Palette data").
    pub(crate) fn set_cgb_bg(&mut self, ram: &[u8; 64], pal: usize) {
        let pal = pal & 7;
        for (e, c) in self.bg[pal].iter_mut().enumerate() {
            let i = pal * 8 + e * 2;
            *c = u16::from_le_bytes([ram[i], ram[i + 1]]) & 0x7FFF;
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

/// Memory the renderer reads.
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
    /// SCX & 7 as of the start of the line: the hardware discards that
    /// many pixels once, at the start of mode 3 (Pan Docs: Scrolling, "the
    /// low 3 bits of SCX are only read at the beginning of the scanline").
    fine_scx: u8,
    /// The window has been activated on this line and not switched off.
    window_active: bool,
    /// Screen x where the window was activated.
    window_start: u8,
    /// Window map column (pixels) at `window_start`.
    window_skip: u8,
    /// Window map row (pixels) this line draws.
    window_row: u8,
    pub(crate) objects: LineObjects,
    /// Slots of `objects` from highest to lowest drawing priority.
    pub(crate) order: [u8; MAX_PER_LINE],
    /// Object pixels of the line, at screen x + 8: 0 = transparent, else
    /// bits 0-1 color (1-3), bits 2-4 palette, bit 7 OAM "behind BG".
    /// `obj_buf` still holds the previous line's objects (cleared by the
    /// next `render`).
    obj_dirty: bool,
    /// An object was fetched into `obj_buf` on this line (else it is all
    /// transparent).
    obj_fetched: bool,
}

impl Renderer {
    pub(crate) const fn new() -> Self {
        Self {
            window_line: 0,
            wy_triggered: false,
            ly: 0,
            fine_scx: 0,
            window_active: false,
            window_start: 0,
            window_skip: 0,
            window_row: 0,
            objects: LineObjects::EMPTY,
            order: [0; MAX_PER_LINE],
            obj_dirty: false,
            obj_fetched: false,
        }
    }

    /// Start of a frame (line 0, also after the LCD is turned on): the
    /// window line counter restarts at 0 and the WY condition is cleared
    /// (Pan Docs: Window, "On each VBlank, the Y condition is cleared";
    /// SameBoy resets `window_y` and `wy_triggered` at line 0).
    pub(crate) fn start_frame(&mut self) {
        self.window_line = 0;
        self.wy_triggered = false;
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

    /// Start of line `ly`'s pixel output, with SCX as of that moment and
    /// the objects the OAM scan selected.
    pub(crate) fn start_line(
        &mut self,
        ly: u8,
        scx: u8,
        objects: &LineObjects,
        order: [u8; MAX_PER_LINE],
    ) {
        self.ly = ly;
        self.fine_scx = scx & 7;
        self.window_active = false;
        self.objects = *objects;
        self.order = order;
        // The buffer (`Memory::obj_buf`) is all transparent unless an object
        // was fetched; it is cleared by the first `render` of the line.
        self.obj_dirty |= self.obj_fetched;
        self.obj_fetched = false;
    }

    /// The slot of the line's last object fetch: the last object in
    /// priority (fetch) order on screen horizontally (X < 168; objects at
    /// X >= 168 are never fetched). CGB: fetched even with LCDC.1 clear.
    pub(crate) fn last_fetched(&self) -> Option<usize> {
        self.order[..self.objects.len()]
            .iter()
            .rev()
            .map(|&s| usize::from(s) % MAX_PER_LINE)
            .find(|&s| self.objects.get(s).2 < 168)
    }

    /// Draws pixels `[x0, x1)` of the current line into `row` with the
    /// registers `regs`. Spans must be consecutive and in order.
    // The object buffer is a separate `&mut` (it lives in `Memory`, while
    // `Sources` is shared borrows of other parts of it).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        row: &mut [u16; W],
        x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
    ) {
        let x1 = x1.min(W);
        if x0 >= x1 {
            return;
        }
        if self.obj_dirty {
            *obj_buf = [0; OBJ_BUF_LEN];
            self.obj_dirty = false;
        }
        let lcdc = regs.lcdc;

        // Window (Pan Docs: Window, "Window rendering criteria"; SameBoy
        // FIFO path). The window starts where the pixel counter equals WX
        // (screen x = WX - 7) while LCDC.5 and the WY condition hold; that
        // activation reads the next window row. Clearing LCDC.5 switches
        // back to the BG; the window can only start again at a later WX
        // match. WX = 0-6 start the window at x = 0 with 7 - WX pixels cut
        // off; WX = 0 also loses SCX & 7 pixels (Pan Docs: "the Window is
        // switched to before the initial fine scroll adjustment"). WX = 166
        // shows one pixel on CGB (the DMG next-line bug is DMG-only, SameBoy
        // `!GB_is_cgb`); WX >= 167 never matches.
        if self.window_active && lcdc & LCDC_WIN_ON == 0 {
            self.window_active = false;
        }
        let mut win_from = x1;
        if self.window_active {
            win_from = x0;
        } else if self.wy_triggered && lcdc & LCDC_WIN_ON != 0 && regs.wx <= 166 {
            let start = usize::from(regs.wx.saturating_sub(7));
            if (x0..x1).contains(&start) {
                self.window_active = true;
                self.window_start = start as u8;
                self.window_skip = match regs.wx {
                    0 => 7 + self.fine_scx,
                    1..=6 => 7 - regs.wx,
                    _ => 0,
                };
                self.window_row = self.window_line;
                self.window_line = self.window_line.wrapping_add(1);
                win_from = start;
            }
        }

        // Objects are fetched when the span reaches their first visible
        // pixel, in priority order; a pixel keeps the first opaque object
        // written to it. Fetching per span (not per line) keeps split and
        // whole-line rendering identical.
        for k in 0..self.objects.len() {
            let slot = usize::from(self.order[k % MAX_PER_LINE]);
            let (index, y, x) = self.objects.get(slot);
            // X = 0 and X >= 168 are off-screen (Pan Docs: OAM byte 1).
            if x == 0 || x >= 168 {
                continue;
            }
            let first = usize::from(x.saturating_sub(8));
            if (x0..x1).contains(&first) {
                self.fetch_object(obj_buf, src, lcdc, slot, index, y, x);
            }
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
            self.bg_span::<true>(out, &obj_buf[x0 + 8..x1 + 8], x0, win_from, regs, src);
        }
    }

    /// BG/window pixels `[x0, x0 + out.len())`, the window from `win_from`
    /// on; with `MIX`, mixed with the object pixels `obj` (same length as
    /// `out`, see [`mix`]). With LCDC.0 clear (non-CGB mode) both
    /// show color 0, which still goes through BGP, and objects are drawn
    /// over them regardless of their priority bit (Pan Docs: LCDC.0;
    /// SameBoy `render_pixel_if_possible`: `bg_enabled = false` makes the
    /// pixel 0, then BGP applies). The window state still advances: the
    /// fetcher runs, only the mixer blanks the pixels.
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
        if lcdc & LCDC_BG_ON == 0 {
            if MIX {
                for (p, &o) in out.iter_mut().zip(obj) {
                    *p = mix(0, o, colors);
                }
            } else {
                out.fill(colors.bg[0][0]);
            }
            return;
        }
        // TODO(M5): the palette from each tile's BG attributes.
        let pairs = &src.pairs[0];
        let split = (win_from - x0).min(out.len());
        let (bg, win) = out.split_at_mut(split);
        let (obj_bg, obj_win) = if MIX {
            obj.split_at_checked(split).unwrap_or((obj, &[]))
        } else {
            (&[][..], &[][..])
        };
        if !bg.is_empty() {
            // BG pixel x is map column SCX + x; the fine part was latched
            // at line start, the tile part is read per fetch.
            let col = (regs.scx & 0xF8)
                .wrapping_add(self.fine_scx)
                .wrapping_add(x0 as u8);
            let map = if lcdc & LCDC_BG_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            let y = self.ly.wrapping_add(regs.scy);
            fill::<MIX>(bg, obj_bg, src.vram, colors, pairs, lcdc, map, y, col);
        }
        if !win.is_empty() {
            let col =
                ((win_from - usize::from(self.window_start)) as u8).wrapping_add(self.window_skip);
            let map = if lcdc & LCDC_WIN_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            fill::<MIX>(
                win,
                obj_win,
                src.vram,
                colors,
                pairs,
                lcdc,
                map,
                self.window_row,
                col,
            );
        }
    }

    /// Decodes one object's row into the object buffer.
    #[allow(clippy::too_many_arguments)] // Object fields plus the buffer.
    fn fetch_object(
        &mut self,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
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
                let base = usize::from(index) % 40 * 4;
                (src.oam[base + 2], src.oam[base + 3])
            }
        };
        // LCDC.2 is read at fetch time (SameBoy `get_object_line_address`).
        // 8x16 objects ignore bit 0 of the tile index (Pan Docs: OAM byte 2).
        let (mask, tile) = if lcdc & LCDC_OBJ_TALL != 0 {
            (15, tile & 0xFE)
        } else {
            (7, tile)
        };
        let mut line = self.ly.wrapping_add(16).wrapping_sub(y) & mask;
        if flags & ATTR_Y_FLIP != 0 {
            line ^= mask;
        }
        // Objects always use 8000-8FFF addressing (Pan Docs: VRAM Tile Data).
        let addr = (usize::from(tile) << 4) + (usize::from(line) << 1);
        let (mut lo, mut hi) = (src.vram[addr & 0x1FFF], src.vram[(addr + 1) & 0x1FFF]);
        if flags & ATTR_X_FLIP != 0 {
            lo = lo.reverse_bits();
            hi = hi.reverse_bits();
        }
        let w = decode(lo, hi);
        // DMG compatibility mode: OBP1 is OBJ palette 1.
        let attr = if flags & ATTR_DMG_OBP1 != 0 {
            1 << 2
        } else {
            0
        } | (flags & ATTR_BEHIND_BG);
        // All 8 pixels at once: a byte per pixel, leftmost first. A pixel
        // is written where the new color is opaque and the buffer is still
        // transparent (an earlier, higher-priority object keeps it).
        let x = usize::from(x);
        if let Some(dst) = obj_buf
            .get_mut(x..x + 8)
            .and_then(|s| <&mut [u8; 8]>::try_from(s).ok())
        {
            let new = spread_bytes(w);
            let old = u64::from_be_bytes(*dst);
            let put = opaque_bytes(new) & !opaque_bytes(old);
            *dst = (old | ((new | (u64::from(attr) * BYTES_01)) & put)).to_be_bytes();
        }
    }
}

/// [`fill_tiles`], both copies out of line: inlined, each would be
/// duplicated in `render` (BG and window; code size, `docs/perf.md`). One
/// call per BG or window part of a span.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn fill<const MIX: bool>(
    out: &mut [u16],
    obj: &[u8],
    vram: &[u8; 0x4000],
    colors: &Colors,
    pairs: &[u32; 16],
    lcdc: u8,
    map: usize,
    y: u8,
    col: u8,
) {
    if MIX {
        fill_mixed(out, obj, vram, colors, pairs, lcdc, map, y, col);
    } else {
        fill_plain(out, vram, colors, pairs, lcdc, map, y, col);
    }
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn fill_plain(
    out: &mut [u16],
    vram: &[u8; 0x4000],
    colors: &Colors,
    pairs: &[u32; 16],
    lcdc: u8,
    map: usize,
    y: u8,
    col: u8,
) {
    fill_tiles::<false>(out, &[], vram, colors, pairs, lcdc, map, y, col);
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn fill_mixed(
    out: &mut [u16],
    obj: &[u8],
    vram: &[u8; 0x4000],
    colors: &Colors,
    pairs: &[u32; 16],
    lcdc: u8,
    map: usize,
    y: u8,
    col: u8,
) {
    fill_tiles::<true>(out, obj, vram, colors, pairs, lcdc, map, y, col);
}

/// One pixel of a span with objects. An opaque object pixel `o` is drawn
/// unless its "behind BG" bit is set and the BG color index `c` (before
/// BGP) is non-zero (Pan Docs: OAM byte 3 bit 7).
#[inline(always)]
fn mix(c: u8, o: u8, colors: &Colors) -> u16 {
    if o != 0 && (c == 0 || o & ATTR_BEHIND_BG == 0) {
        colors.obj[usize::from(o >> 2) & 7][usize::from(o & 3)]
    } else {
        colors.bg[0][usize::from(c & 3)]
    }
}

/// Fills `out` with the colors of the BG/window pixels from the 32x32 tile
/// map at `map` (VRAM offset), map pixel row `y`, starting at map pixel
/// column `col` and wrapping at 256. With `MIX`, `obj` holds the object
/// pixels at the same positions (same length as `out`) and each pixel is
/// [`mix`]ed; a whole tile with no object pixel (one 8-byte test) is drawn
/// as without `MIX`.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn fill_tiles<const MIX: bool>(
    out: &mut [u16],
    obj: &[u8],
    vram: &[u8; 0x4000],
    colors: &Colors,
    pairs: &[u32; 16],
    lcdc: u8,
    map: usize,
    y: u8,
    mut col: u8,
) {
    let map_row = map | (usize::from(y >> 3) << 5);
    let row_offset = usize::from(y & 7) << 1;
    let (base, flip) = tile_data(lcdc);
    // The decoded row of the tile at map column `col`.
    let row = |col: u8| {
        let tile = vram[(map_row | usize::from(col >> 3)) & 0x1FFF];
        let addr = (base + (usize::from(tile ^ flip) << 4)) | row_offset;
        decode(vram[addr & 0x1FFF], vram[(addr | 1) & 0x1FFF])
    };
    // Part of one tile from map column `col`: the span's start up to the
    // next tile boundary, or its end.
    let partial = |out: &mut [u16], obj: &[u8], col: u8| {
        fill_partial::<MIX>(out, obj, row(col) << (2 * (col & 7)), colors);
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
        let w = row(col);
        let obj8 = if MIX {
            obj_tiles.get(i).filter(|o| u64::from_ne_bytes(**o) != 0)
        } else {
            None
        };
        match obj8 {
            // No object pixel: two pixels per lookup (and, merged by the
            // compiler on little-endian targets, per store).
            None => {
                for (j, p) in tile.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let v = pairs[usize::from((w >> (12 - 4 * j)) as u8 & 15)];
                    *p = [v as u16, (v >> 16) as u16];
                }
            }
            Some(o8) => {
                for (i, (p, &o)) in tile.iter_mut().zip(o8).enumerate() {
                    *p = mix((w >> (14 - 2 * i)) as u8 & 3, o, colors);
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

/// Part of a tile: the pixels of the decoded row `w` from bits 15-14 on,
/// mixed with `obj` with `MIX`. Out of line: at most two per span, and
/// inlined it would be copied into every `fill_tiles` instance (code size).
#[inline(never)]
fn fill_partial<const MIX: bool>(out: &mut [u16], obj: &[u8], mut w: u16, colors: &Colors) {
    if MIX {
        for (p, &o) in out.iter_mut().zip(obj) {
            *p = mix((w >> 14) as u8, o, colors);
            w <<= 2;
        }
    } else {
        for p in out {
            *p = colors.bg[0][usize::from(w >> 14)];
            w <<= 2;
        }
    }
}

/// 0x01 in every byte of a u64.
const BYTES_01: u64 = 0x0101_0101_0101_0101;

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

/// [`spread`] of every byte (512 bytes of flash): one load per bit plane
/// instead of ~10 operations.
static SPREAD: [u16; 256] = {
    let mut t = [0u16; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = spread(i as u8);
        i += 1;
    }
    t
};

/// Interleaves a tile row's two bit planes (Pan Docs: Tile Data) into 2-bit
/// color indices, leftmost pixel in bits 15-14.
#[inline]
fn decode(lo: u8, hi: u8) -> u16 {
    SPREAD[usize::from(lo)] | (SPREAD[usize::from(hi)] << 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ppu::objects::LineObjects;

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
        obj_buf: [u8; OBJ_BUF_LEN],
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
                obj_buf: [0; OBJ_BUF_LEN],
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
            self.r
                .start_line(ly, self.regs.scx, &objs, objs.priority_order());
        }

        fn span(&mut self, row: &mut [u16; W], x0: usize, x1: usize) {
            let src = Sources {
                vram: &self.vram,
                oam: &self.oam,
                attrs: None,
                colors: &self.colors,
                pairs: &[pair_table(&self.colors.bg[0]); 8],
            };
            self.r
                .render(row, x0, x1, &self.regs, &src, &mut self.obj_buf);
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

    #[test]
    fn split_spans_match_one_span() {
        for seed in 0..64u64 {
            let mut rng = Lcg(seed);
            let mut s = Scene::new();
            s.vram[..0x2000].fill_with(|| rng.next());
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
            let mut split = Scene {
                vram: s.vram,
                oam: s.oam,
                colors: s.colors,
                regs: s.regs,
                r: Renderer::new(),
                obj_buf: [0; OBJ_BUF_LEN],
            };
            split.r.start_frame();
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
