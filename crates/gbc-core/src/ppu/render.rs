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
//! the master priority. Each tile fetch reads its attribute byte (masked to
//! 0 in compatibility mode, so that path draws the same pixels as before)
//! and turns it into a palette and a priority mask for the tile's pixels:
//! no per-pixel work is added. CGB-mode objects read their attributes the
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
use crate::memory::{Held, ObjRank};

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
#[inline(always)]
const fn bg_column(scx: u8, fine: u8, x: u8) -> u8 {
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
    /// The fine scroll: pixels discarded at the start of mode 3, mod 8.
    /// SCX & 7 at the line start, or as an SCX write inside the discard
    /// moved it (`set_fine`); later writes keep it (Pan Docs: Scrolling).
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
    /// CGB mode (else DMG compatibility mode), fixed from reset (`Ppu::reset`).
    pub(crate) cgb: bool,
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

    /// The window state at the last pixel drawn: active, its start pixel
    /// and the pixels cut from its first tile.
    pub(crate) const fn window(&self) -> (bool, u8, u8) {
        (self.window_active, self.window_start, self.window_skip)
    }

    /// [`Renderer::window`] as it will be once pixels `[x0, x1)` are drawn
    /// with LCDC `lcdc` and WX `wx` ([`Renderer::render`]'s window steps):
    /// for a write whose flush is deferred (`Ppu::scroll_write`).
    #[inline]
    pub(crate) fn window_after(&self, x0: usize, x1: usize, lcdc: u8, wx: u8) -> (bool, u8, u8) {
        let x1 = x1.min(W);
        if x0 >= x1 {
            return self.window();
        }
        let on = lcdc & LCDC_WIN_ON != 0;
        if !self.window_active && on && self.wy_triggered && wx <= 166 {
            let start = usize::from(wx.saturating_sub(7));
            if (x0..x1).contains(&start) {
                return (true, start as u8, self.window_skip_of(wx));
            }
        }
        (
            self.window_active && on,
            self.window_start,
            self.window_skip,
        )
    }

    /// Pixels cut from the window's first tile when it starts at WX `wx`.
    #[inline(always)]
    const fn window_skip_of(&self, wx: u8) -> u8 {
        match wx {
            0 => 7 + self.fine_scx,
            1..=6 => 7 - wx,
            _ => 0,
        }
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

    /// An SCX write moved the line's fine-scroll discard to `n` pixels
    /// (`Ppu::hold_fetch`); `n` and `n + 8` give the same columns.
    pub(crate) const fn set_fine(&mut self, n: u8) {
        self.fine_scx = n & 7;
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
    // The object buffer and its ranks are separate `&mut`s (they live in
    // `Memory`, while `Sources` is shared borrows of other parts of it).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn render(
        &mut self,
        row: &mut [u16; W],
        x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
        obj_rank: &mut ObjRank,
    ) {
        let x1 = x1.min(W);
        if x0 >= x1 {
            return;
        }
        if usize::from(obj_rank.hold[1].end) > x0 {
            return self.render_held(row, x0, x1, regs, src, obj_buf, obj_rank);
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
                self.window_skip = self.window_skip_of(regs.wx);
                self.window_row = self.window_line;
                self.window_line = self.window_line.wrapping_add(1);
                win_from = start;
            }
        }

        // Objects are fetched when the span reaches their first visible
        // pixel, in priority order; a pixel keeps the first opaque object
        // written to it. Fetching per span (not per line) keeps split and
        // whole-line rendering identical.
        if usize::from(obj_rank.next) < self.objects.len() {
            self.fetch_objects(obj_buf, obj_rank, src, lcdc, x1);
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

    /// [`Renderer::render`] while pixels the fetcher fetched before a
    /// map-select, tile-data, SCX or SCY write in mode 3 are still to be
    /// drawn (`Ppu::hold_fetch`): each held part with the LCDC bits 3, 4
    /// and 6, the SCX and the SCY its tiles were fetched with (and the
    /// planes of its last tile read after the write redrawn, see
    /// [`Renderer::mix_planes`]), then the rest with `regs`. Out of line
    /// and cold: only after such a write.
    #[cold]
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn render_held(
        &mut self,
        row: &mut [u16; W],
        mut x0: usize,
        x1: usize,
        regs: &Regs,
        src: &Sources,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
        obj_rank: &mut ObjRank,
    ) {
        const FETCHED: u8 = LCDC_BG_MAP | LCDC_TILES_8000 | LCDC_WIN_MAP;
        let regs_of = |h: &Held| Regs {
            lcdc: (regs.lcdc & !FETCHED) | (h.lcdc & FETCHED),
            scx: h.scx,
            scy: h.scy,
            ..*regs
        };
        // Taken while the parts are drawn, so they are drawn as usual.
        let hold = core::mem::take(&mut obj_rank.hold);
        for (i, h) in hold.iter().enumerate() {
            let end = usize::from(h.end);
            if end <= x0 {
                continue;
            }
            let held = regs_of(h);
            let to = end.min(x1);
            self.render(row, x0, to, &held, src, obj_buf, obj_rank);
            let from = x0.max(end.saturating_sub(8));
            if h.planes != 0 && from < to {
                let next = if i == 0 { regs_of(&hold[1]) } else { *regs };
                self.mix_planes(row, obj_buf, from, to, &held, &next, h.planes, src);
            }
            x0 = to;
        }
        self.render(row, x0, x1, regs, src, obj_buf, obj_rank);
        obj_rank.hold = hold;
    }

    /// Redraws pixels `[x0, x1)` of one BG or window tile, just drawn with
    /// `held`, with the bit planes in `planes` (bit 0 low, bit 1 high) read
    /// with `next`'s LCDC.4 and SCY instead: the fetcher read the map entry
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
        planes: u8,
        src: &Sources,
    ) {
        let lcdc = held.lcdc;
        let master = if lcdc & LCDC_BG_ON != 0 { 0xFF } else { 0 };
        let attr_mask = if self.cgb {
            0xFF
        } else if master == 0 {
            // Color 0 whatever the tile data.
            return;
        } else {
            0
        };
        let win = self.window_active && x0 >= usize::from(self.window_start);
        let (map_bit, y, y_next, col) = if win {
            let col = ((x0 - usize::from(self.window_start)) as u8).wrapping_add(self.window_skip);
            (LCDC_WIN_MAP, self.window_row, self.window_row, col)
        } else {
            (
                LCDC_BG_MAP,
                self.ly.wrapping_add(held.scy),
                self.ly.wrapping_add(next.scy),
                bg_column(held.scx, self.fine_scx, x0 as u8),
            )
        };
        let map = if lcdc & map_bit != 0 { 0x1C00 } else { 0x1800 };
        let fetch = |row_y: u8, tiles: u8| {
            let (base, flip) = tile_data(tiles);
            Fetch {
                vram: src.vram,
                map_row: map | (usize::from(y >> 3) << 5),
                row_offset: usize::from(row_y & 7) << 1,
                base,
                flip,
                attr_mask,
            }
            .row(col)
        };
        let (old, attr) = fetch(y, lcdc);
        let (new, _) = fetch(y_next, next.lcdc);
        // Decoded rows: the low plane in the even bits ([`decode`]).
        let m = (u16::from(planes & 1) * 0x5555) | (u16::from(planes >> 1) * 0xAAAA);
        let w = ((old & !m) | (new & m)) << (2 * (col & 7));
        // One instance (mixed with no object pixels when none are shown):
        // calling the plain one too grows it past its size budget.
        let obj = if lcdc & LCDC_OBJ_ON != 0 && self.obj_fetched {
            &obj_buf[x0 + 8..x1 + 8]
        } else {
            &[0; 8]
        };
        fill_partial::<true>(&mut row[x0..x1], obj, w, attr, master, src.colors);
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
        let mode = if self.cgb {
            Mode {
                attr_mask: 0xFF,
                master: if lcdc & LCDC_BG_ON != 0 { 0xFF } else { 0 },
            }
        } else if lcdc & LCDC_BG_ON == 0 {
            if MIX {
                for (p, &o) in out.iter_mut().zip(obj) {
                    *p = mix(0, o, ATTR_BEHIND_BG, &colors.bg[0], colors);
                }
            } else {
                out.fill(colors.bg[0][0]);
            }
            return;
        } else {
            Mode::DMG
        };
        let split = (win_from - x0).min(out.len());
        let (bg, win) = out.split_at_mut(split);
        let (obj_bg, obj_win) = if MIX {
            obj.split_at_checked(split).unwrap_or((obj, &[]))
        } else {
            (&[][..], &[][..])
        };
        if !bg.is_empty() {
            let col = bg_column(regs.scx, self.fine_scx, x0 as u8);
            let map = if lcdc & LCDC_BG_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            let y = self.ly.wrapping_add(regs.scy);
            fill::<MIX>(bg, obj_bg, src, lcdc, mode, map, y, col);
        }
        if !win.is_empty() {
            let col =
                ((win_from - usize::from(self.window_start)) as u8).wrapping_add(self.window_skip);
            let map = if lcdc & LCDC_WIN_MAP != 0 {
                0x1C00
            } else {
                0x1800
            };
            fill::<MIX>(win, obj_win, src, lcdc, mode, map, self.window_row, col);
        }
    }

    /// Fetches the objects whose first visible pixel is in `[x0, x1)`, in
    /// fetch order. `order` is ascending X and the spans of a line are
    /// consecutive from pixel 0, so the objects of a span are the next ones
    /// in `order` from `ObjRank::next`: each object is looked at once per line,
    /// not once per span. Out of line: once per span with objects, and
    /// inlined it would grow `render` past its size budget.
    #[inline(never)]
    fn fetch_objects(
        &mut self,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
        obj_rank: &mut ObjRank,
        src: &Sources,
        lcdc: u8,
        x1: usize,
    ) {
        let mut k = usize::from(obj_rank.next);
        while k < self.objects.len() {
            let slot = usize::from(self.order[k % MAX_PER_LINE]);
            let (index, y, x) = self.objects.get(slot);
            // X = 0 and X >= 168 are off-screen (Pan Docs: OAM byte 1); X
            // >= 168 ends the line's fetches (ascending X).
            if x >= 168 || usize::from(x.saturating_sub(8)) >= x1 {
                break;
            }
            if x != 0 {
                self.fetch_object(obj_buf, &mut obj_rank.rank, src, lcdc, slot, index, y, x);
            }
            k += 1;
        }
        obj_rank.next = k as u8;
    }

    /// Decodes one object's row into the object buffer.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)] // Object fields plus the buffers.
    fn fetch_object(
        &mut self,
        obj_buf: &mut [u8; OBJ_BUF_LEN],
        obj_rank: &mut [u8; OBJ_BUF_LEN],
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
        let addr = ((usize::from(tile) << 4) + (usize::from(line) << 1)) | bank;
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
            obj_buf
                .get_mut(x..x + 8)
                .and_then(|s| <&mut [u8; 8]>::try_from(s).ok()),
            obj_rank
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

/// How tile fetches read the BG map attributes, fixed per span.
#[derive(Clone, Copy)]
struct Mode {
    /// ANDed with each attribute byte: 0xFF in CGB mode, 0 in compatibility
    /// mode (no attributes: palette 0, bank 0, no flips, no BG priority).
    attr_mask: u8,
    /// ANDed with each tile's priority mask ([`mix`]): 0 in CGB mode with
    /// LCDC.0 clear (objects always on top), else 0xFF.
    master: u8,
}

impl Mode {
    const DMG: Self = Self {
        attr_mask: 0,
        master: 0xFF,
    };
}

/// [`fill_tiles`], both copies out of line: inlined, each would be
/// duplicated in `render` (BG and window; code size, `docs/perf.md`). One
/// call per BG or window part of a span.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn fill<const MIX: bool>(
    out: &mut [u16],
    obj: &[u8],
    src: &Sources,
    lcdc: u8,
    mode: Mode,
    map: usize,
    y: u8,
    col: u8,
) {
    if MIX {
        fill_mixed(out, obj, src, lcdc, mode, map, y, col);
    } else {
        fill_plain(out, src, lcdc, mode, map, y, col);
    }
}

#[inline(never)]
fn fill_plain(out: &mut [u16], src: &Sources, lcdc: u8, mode: Mode, map: usize, y: u8, col: u8) {
    fill_tiles::<false>(out, &[], src, lcdc, mode, map, y, col);
}

#[inline(never)]
#[allow(clippy::too_many_arguments)]
fn fill_mixed(
    out: &mut [u16],
    obj: &[u8],
    src: &Sources,
    lcdc: u8,
    mode: Mode,
    map: usize,
    y: u8,
    col: u8,
) {
    fill_tiles::<true>(out, obj, src, lcdc, mode, map, y, col);
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

/// One BG or window tile row fetch: map row, tile data addressing and how
/// to read the attributes, fixed for a part of a span.
struct Fetch<'a> {
    vram: &'a [u8; 0x4000],
    /// VRAM offset of the map row.
    map_row: usize,
    /// Byte offset of the pixel row in a tile (2 * fine y).
    row_offset: usize,
    /// [`tile_data`].
    base: usize,
    flip: u8,
    attr_mask: u8,
}

impl Fetch<'_> {
    /// The decoded row of the tile at map column `col` and its attributes:
    /// in CGB mode the byte at the same map address in VRAM bank 1 (Pan
    /// Docs: VRAM Tile Maps, "BG Map Attributes"; SameBoy `fetcher_step`
    /// reads it from `vram[map + 0x2000]` when `cgb_mode`), else 0. Bit 3
    /// selects the tile data bank, bit 6 flips the row, bit 5 the pixels.
    #[inline(always)]
    fn row(&self, col: u8) -> (u16, u8) {
        let vram = self.vram;
        let i = (self.map_row | usize::from(col >> 3)) & 0x1FFF;
        let attr = vram[i | 0x2000] & self.attr_mask;
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
/// BG priority bit; ANDed with `master` ([`Mode`]).
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
/// attributes ([`Fetch::row`]).
#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn fill_tiles<const MIX: bool>(
    out: &mut [u16],
    obj: &[u8],
    src: &Sources,
    lcdc: u8,
    mode: Mode,
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
        attr_mask: mode.attr_mask,
    };
    // Part of one tile from map column `col`: the span's start up to the
    // next tile boundary, or its end.
    let part = Part {
        f,
        master: mode.master,
        colors,
    };
    let f = &part.f;
    let partial = |out: &mut [u16], obj: &[u8], col: u8| {
        partial_tile::<MIX>(out, obj, &part, col);
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
        let (w, attr) = f.row(col);
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
                let (bg, m) = (&colors.bg[pal], prio_mask(attr, mode.master));
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
    /// [`Mode::master`].
    master: u8,
    colors: &'a Colors,
}

/// Part of the tile at map column `col` (from pixel `col & 7` on), mixed
/// with `obj` with `MIX`: the fetch, then [`fill_partial`]. Out of line: at
/// most two per span, and inlined (with its tile fetch) it would be copied
/// twice into every `fill_tiles` instance (code size).
#[inline(never)]
fn partial_tile<const MIX: bool>(out: &mut [u16], obj: &[u8], p: &Part, col: u8) {
    let (w, attr) = p.f.row(col);
    fill_partial::<MIX>(out, obj, w << (2 * (col & 7)), attr, p.master, p.colors);
}

/// Part of a tile: the pixels of the decoded row `w` from bits 15-14 on, of
/// a tile with attributes `attr` (palette; priority with `master`, see
/// [`prio_mask`]), mixed with `obj` with `MIX`.
#[inline(never)]
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
        obj_buf: [u8; OBJ_BUF_LEN],
        obj_rank: ObjRank,
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
                obj_rank: ObjRank {
                    rank: [0; OBJ_BUF_LEN],
                    hold: [Held::default(); 2],
                    next: 0,
                },
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
            self.obj_rank.next = 0;
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
                pairs: &core::array::from_fn(|p| pair_table(&self.colors.bg[p])),
            };
            self.r.render(
                row,
                x0,
                x1,
                &self.regs,
                &src,
                &mut self.obj_buf,
                &mut self.obj_rank,
            );
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
                obj_buf: [0; OBJ_BUF_LEN],
                obj_rank: ObjRank {
                    rank: [0; OBJ_BUF_LEN],
                    hold: [Held::default(); 2],
                    next: 0,
                },
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
