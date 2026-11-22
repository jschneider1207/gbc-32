//! Picture processing unit: mode timing, LY/LYC, the STAT interrupt and
//! the VBlank interrupt (ADR 0002, ADR 0014), and the scanline renderer
//! (`render.rs`, objects from `objects.rs`). A frame completes at the
//! start of each VBlank.
//!
//! # Model (ADR 0014)
//! The PPU is lazy. LY, the STAT mode and coincidence bits and the
//! VRAM/OAM access windows are computed from the clock when they are
//! read; nothing runs per mode change. The PPU runs (`Ppu::sync`) only:
//! - at its `EventKind::Ppu` deadline ([`Ppu::next_event`]): the next STAT
//!   interrupt edge, the VBlank interrupt, or the end of a visible line's
//!   mode 3 (where the renderer draws the rest of the line);
//! - before a CPU write to a PPU register (the bus syncs first, and for
//!   the registers the picture depends on, draws the current line up to
//!   the pixel being output: [`Ppu::flush`]).
//!
//! A sync walks the *update points* (below) between the last sync and now
//! in order, keeping the STAT interrupt line exact. There are 3-5 per
//! line, and the walk to find the next deadline stops at the first one
//! that does something observable, so it is short.
//!
//! # Timeline (CGB revision C, single speed)
//! Positions are base clocks (dots) into the frame, `p = now -
//! frame_start`; a line is 456 dots, `t = p % 456`. The state at position
//! `p` includes every action at `p`: SameBoy (Core/display.c,
//! `GB_display_run`, MIT) runs an action timed `n` dots after the LCD was
//! enabled only once the CPU has advanced *more* than `n` dots, so its
//! nominal times are one dot earlier than the positions here. The tables
//! below are in positions; LCD enable is at position 7 (`ON_POS`), so the
//! first line's actions sit at SameBoy's nominal time + 8.
//!
//! Lines 0-143 (SameBoy states 35, 6, 7, 8, 10, 32, 21, 22):
//! - `t = 0..3`: still the previous line: LY, coincidence against LY-1,
//!   mode 0 (line 0: mode 1);
//! - `t = 3`: LY changes; the coincidence comparison is off for one dot
//!   (`ly_for_comparison = -1`: the STAT bit reads 0, the interrupt line
//!   holds); the mode-2 interrupt source rises ("the OAM STAT interrupt
//!   occurs 1 T-cycle before STAT actually changes, except on line 0");
//! - `t = 4`: STAT mode 2, coincidence against LY; the mode-2 source is
//!   a pulse, dropped right after it is evaluated;
//! - `t = 84`: mode 3; `t = 89`: the pixel pipeline starts;
//! - `t = end` (256 + SCX%8 + penalties, see [`LineTiming`]): mode 0;
//!   `t = end + 1`: the HBlank interrupt source rises.
//!
//! Lines 144-152 (states 26, 12, 24, 13): `t = 0` comparison off; `t = 2`
//! LY changes; `t = 4` comparison against LY. Line 144 also has the
//! mode-2 interrupt quirk at `t = 2` and `t = 5` (STAT bit 5 requests an
//! interrupt when entering VBlank, one M-cycle before VBlank on CGB:
//! Mooneye `misc/ppu/vblank_stat_intr-C`), and at `t = 5` mode 1, the
//! VBlank interrupt and the frame.
//!
//! Line 153 (states 19, 14, 15, 16, 29, 17): LY reads 153 from `t = 2`
//! and 0 from `t = 6` (`t = 8` in double speed); the comparison is
//! against 153 at `t = 6..8` and against 0 from `t = 12`.
//!
//! The first line after the LCD is enabled has no OAM scan (GBCTR; SameBoy
//! "Handle mode 2 on the very first line 0"): mode 0 until position 86,
//! mode 3 from there, the pipeline from 91; it is 8 dots shorter, so line
//! 1 starts at position 456 as usual.
//!
//! The STAT interrupt line is the OR of the enabled sources, and only its
//! rising edge requests the interrupt ("STAT blocking"; Pan Docs: LCD
//! Status Registers, "STAT interrupt"; SameBoy `GB_STAT_update`). It is
//! re-evaluated at the update points and at STAT/LYC writes only, so a
//! source that changes between update points is seen at the next one.

use crate::dma::{OamDma, PpuView};
use crate::memory::Memory;
use crate::schedule::NEVER;
use crate::{DmgPalettes, Events, FRAME_CLOCKS};
use objects::LineObjects;
pub(crate) use render::OBJ_BUF_LEN;
use render::{Colors, Regs, Renderer, Sources, pair_table, set_pair_color};

mod objects;
mod render;

pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;

/// Base clocks per scanline (456 dots; the PPU does not change speed in
/// CGB double-speed mode).
const LINE: u32 = 456;

/// Base clocks per frame: 154 lines.
const FRAME: u32 = FRAME_CLOCKS;

const _: () = assert!(FRAME == 154 * LINE);

/// Visible lines.
const LINES: u32 = SCREEN_HEIGHT as u32;

/// Frame position of the clock of the LCDC write that enables the LCD.
pub(crate) const ON_POS: u32 = 7;

/// Frame position of the VBlank interrupt and `FRAME_READY`: line 144,
/// dot 5 ([`Pt::VBlank`]).
pub(crate) const VBLANK_POS: u32 = 144 * LINE + 5;

/// Frame position at clock 0 after reset, where the CGB boot ROM hands
/// over: LY = 144, about 164 dots in (Gambatte's measured CGB post-boot
/// state, `setPostBiosState`: `videoCycles = 144*456 + 164`; its line
/// origin may differ from ours by a dot or two).
///
/// The phase against the M-cycle grid is exact, though: the boot ROM
/// enabled the LCD with a single-speed write at an M-cycle boundary, so
/// every M-cycle boundary since sits at a position of `ON_POS` mod 4
/// (3), as after any LCD enable. Mooneye `acceptance/ppu/intr_2_*` (which
/// keep the LCD on from boot) fail by one M-cycle with the 164 of
/// Gambatte; 163 is the nearest position on the grid.
pub(crate) const POST_BOOT_POS: u32 = 144 * LINE + 163;

const _: () = assert!(POST_BOOT_POS % 4 == ON_POS % 4);

/// The VBlank interrupt bit in IF.
pub(crate) const INT_VBLANK: u8 = 0x01;

/// The STAT interrupt bit in IF.
pub(crate) const INT_STAT: u8 = 0x02;

/// STAT interrupt selects (bits 3-6).
const SEL_MODE0: u8 = 0x08;
const SEL_MODE1: u8 = 0x10;
const SEL_MODE2: u8 = 0x20;
const SEL_LYC: u8 = 0x40;

/// `mode_for_interrupt` (SameBoy): which mode source the STAT line sees;
/// `NO_MODE` for mode 3 and the OAM scan after its first dot.
const NO_MODE: u8 = 3;

/// A frame buffer: 160x144 RGB555 pixels (see [`rgb555`](crate::rgb555)),
/// row-major, top-left first. Supplied by the caller on every run call.
/// Swap buffers only after a run call reports
/// [`Events::FRAME_READY`](crate::Events::FRAME_READY); see
/// [`Emulator::run_until`](crate::Emulator::run_until) for why, and for
/// what happens when the LCD is turned off mid-frame.
pub type Frame = [u16; SCREEN_WIDTH * SCREEN_HEIGHT];

/// RGB555 white, the color of a blank (LCD off) screen.
pub const WHITE: u16 = 0x7FFF;

/// An update point: a position where the PPU changes something the STAT
/// interrupt line or the frame depends on (module docs, "Timeline").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pt {
    /// Lines 0-143, `t = 3`: LY changes, mode-2 source (line 0: the mode-1
    /// source stays, comparison against 0).
    Ly3,
    /// Lines 0-143, `t = 4`: mode 2, comparison against LY.
    Ly4,
    /// The pixel pipeline starts: the line's mode-3 length is fixed
    /// ([`LineTiming`]).
    Latch,
    /// Mode 3 ends (STAT reads mode 0 from here, single speed).
    End,
    /// The HBlank interrupt source rises.
    HBlank,
    /// Lines 144-153, `t = 0`: comparison off.
    V0,
    /// Line 144, `t = 2`: the mode-2 interrupt quirk.
    Quirk2,
    /// Lines 144-152, `t = 4`: comparison against LY.
    V4,
    /// Line 144, `t = 5`: mode 1, VBlank interrupt, the frame.
    VBlank,
    /// Line 153, `t = 6`: comparison against 153.
    L153Cmp,
    /// Line 153, `t = 8`: comparison off (single speed).
    L153Off,
    /// Line 153, `t = 12`: comparison against 0.
    L153Zero,
}

/// The next update point after [`Ppu::synced`](Ppu): frame position (past
/// `FRAME`: the next frame's), its line (0-153; 0 past `FRAME`) and kind.
/// Advanced point by point ([`Ppu::step`]) instead of searching from a
/// position with a division per point ([`Ppu::next_point`], only used
/// to re-seek when the walk changes: LCD enable, STAT selects).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Cursor {
    pos: u32,
    line: u8,
    pt: Pt,
}

/// The STAT interrupt line and the LY=LYC part of it (SameBoy
/// `stat_interrupt_line`, `lyc_interrupt_line`).
#[derive(Clone, Copy)]
struct IrqLine {
    stat: bool,
    lyc: bool,
}

impl IrqLine {
    /// `GB_STAT_update` with mode source `mode` (`NO_MODE`: none) and
    /// comparison value `cmp` (`None`: comparison off, the LY=LYC line
    /// holds). Returns whether the line rose.
    #[inline]
    fn eval(&mut self, select: u8, lyc: u8, mode: u8, cmp: Option<u8>) -> bool {
        if let Some(v) = cmp {
            self.lyc = v == lyc;
        }
        let source = match mode {
            0 => SEL_MODE0,
            1 => SEL_MODE1,
            2 => SEL_MODE2,
            _ => 0,
        };
        let new = select & source != 0 || (self.lyc && select & SEL_LYC != 0);
        let rose = new && !self.stat;
        self.stat = new;
        rose
    }
}

/// Mode-3 timing of one visible line, fixed when its pixel pipeline
/// starts ([`Pt::Latch`]). Line-relative positions.
///
/// Mode 3 lasts 172 dots plus penalties (Pan Docs: Rendering, "Mode 3
/// length"): `SCX % 8` dots before the first pixel, 6 dots when the
/// window starts, and 6-11 dots per object ([`object_stalls`]). Pixel
/// `x` leaves the pipeline at `first_pixel + x` plus the stalls of the
/// objects and window at or left of it; mode 3 ends with the last one.
#[derive(Clone, Copy)]
pub(crate) struct LineTiming {
    /// Clock of the line's position 0; identifies the line (`NEVER`:
    /// none).
    start: u64,
    /// STAT reads mode 3 from here.
    mode3: u16,
    /// The first pixel is output at this position (no stall before it).
    first_pixel: u16,
    /// Mode 3 ends: STAT reads mode 0 (single speed).
    end: u16,
    /// Pipeline stalls `(pixel x, dots)` in x order: objects and the
    /// window start.
    stalls: [(u8, u8); 11],
    n_stalls: u8,
    /// Which stall is the window's (`NO_STALL`: none); the others are the
    /// objects', in fetch order.
    win_stall: u8,
}

const NO_STALL: u8 = 0xFF;

impl LineTiming {
    const NONE: Self = Self {
        start: NEVER,
        mode3: 0,
        first_pixel: 0,
        end: 0,
        stalls: [(0, 0); 11],
        n_stalls: 0,
        win_stall: NO_STALL,
    };

    /// Pixels output before line position `t` (0-160), under the
    /// per-object stall model above.
    fn pixels_before(&self, t: u32) -> u8 {
        let mut clock = u32::from(self.first_pixel);
        let mut x = 0u32;
        for &(sx, d) in &self.stalls[..usize::from(self.n_stalls)] {
            let run = u32::from(sx).saturating_sub(x);
            if t < clock + run {
                return (x + t.saturating_sub(clock)) as u8;
            }
            clock += run;
            x = x.max(u32::from(sx));
            clock += u32::from(d);
            if t < clock {
                return x as u8;
            }
        }
        (x + t.saturating_sub(clock)).min(160) as u8
    }
}

pub(crate) struct Ppu {
    pub(crate) lcdc: u8,
    /// STAT bits 3-6 (interrupt selects) as written.
    stat_select: u8,
    pub(crate) lyc: u8,
    /// The clock at frame position 0 (wrapping: negative after reset).
    frame_start: u64,
    /// The clock the state below is exact at (`<= frame_start + FRAME`
    /// plus a few dots: the next frame's first update point).
    synced: u64,
    /// The current frame began with the LCD being enabled: its line 0 has
    /// no OAM scan.
    first_frame: bool,
    line_irq: IrqLine,
    /// The next update point after `synced` (meaningless while the LCD is
    /// off; set on enable).
    cursor: Cursor,
    /// STAT bit 2 while the LCD is off: kept from when it was turned off
    /// (Mooneye `acceptance/ppu/stat_lyc_onoff`).
    lyc_flag_off: bool,
    /// The mode-3 timing of the current (or last) visible line.
    line: LineTiming,
    /// Pixels `0..drawn` of the line `line` describes are in the frame.
    drawn: u8,
    /// Per-frame and per-line drawing state; also the one copy of the WY
    /// condition, which the mode-3 window penalty reads too.
    renderer: Renderer,
    /// BGP/OBP0/OBP1 resolved to RGB555 through the compatibility palettes
    /// when written (ADR 0013).
    colors: Colors,
    /// Which lines have objects, so the OAM scan of the others is skipped.
    objects: ObjectLines,
    /// The screen shows white
    /// ([`Emulator::screen_blank`](crate::Emulator::screen_blank)): set
    /// when the LCD is turned off, cleared by the first frame that is
    /// actually displayed.
    blanked: bool,
    /// The LCD was turned on and its first frame hasn't completed yet. That
    /// frame is not displayed, so it doesn't clear `blanked`.
    skip_frame: bool,
    /// The OAM scan of a line that OAM DMA touches, done entry by entry
    /// ([`Ppu::scan_to`]) and possibly in parts, around changes of the
    /// transfer's state.
    scan: ScanProgress,
    /// The last Y and X the PPU read from OAM (SameBoy `mode2_y_bus`,
    /// `mode2_x_bus`): what the OAM scan compares while a running OAM DMA
    /// keeps it off OAM. Tracked only while a transfer touches the lines
    /// (`bus_live`); set from OAM when one starts ([`Ppu::dma_event`]).
    bus: [u8; 2],
    bus_live: bool,
    /// The last OAM DMA transfer (FF46). The bus drives it (CPU-side
    /// timing and conflicts, `bus.rs`); the PPU reads it for its own OAM
    /// reads ([`OamDma::ppu_at`]).
    pub(crate) dma: OamDma,
    /// Object slots of the current line whose OAM fetch has happened under
    /// an earlier DMA state ([`Ppu::fix_attrs`]).
    attr_final: u16,
    /// OAM DMA touches the current line: its objects' tile index and
    /// attributes are what their fetches read then (`obj_attr`, by slot),
    /// not OAM as it is at the draw.
    attr_on: bool,
    obj_attr: [(u8, u8); objects::MAX_PER_LINE],
}

/// See [`Ppu::scan`].
#[derive(Clone, Copy)]
struct ScanProgress {
    /// Clock of the line's position 0 (`NEVER`: no scan in progress).
    line: u64,
    /// The next OAM entry to scan.
    next: u8,
    objs: LineObjects,
}

impl ScanProgress {
    const NONE: Self = Self {
        line: NEVER,
        next: 0,
        objs: LineObjects::EMPTY,
    };
}

/// OAM scan entry `i` is read at this line position (SameBoy: one entry
/// every 2 dots from the mode-2 STAT change, in both speeds).
const fn entry_pos(i: u8) -> u64 {
    4 + 2 * i as u64
}

/// Where a clock falls: line, line position, and whether line 0 is the
/// short first line after LCD enable.
#[derive(Clone, Copy)]
struct At {
    line: u32,
    t: u32,
    first: bool,
}

impl Ppu {
    pub(crate) const fn new() -> Self {
        Self {
            lcdc: 0,
            stat_select: 0,
            lyc: 0,
            frame_start: 0,
            synced: 0,
            first_frame: false,
            line_irq: IrqLine {
                stat: false,
                lyc: false,
            },
            cursor: Cursor {
                pos: 0,
                line: 0,
                pt: Pt::V0,
            },
            lyc_flag_off: false,
            line: LineTiming::NONE,
            drawn: 0,
            renderer: Renderer::new(),
            colors: Colors::new(),
            objects: ObjectLines::STALE,
            blanked: false,
            skip_frame: false,
            scan: ScanProgress::NONE,
            bus: [0; 2],
            bus_live: false,
            attr_final: 0,
            attr_on: false,
            obj_attr: [(0, 0); objects::MAX_PER_LINE],
            dma: OamDma::new(),
        }
    }

    /// Power-on state at clock 0, as the CGB boot ROM leaves it: LCD on,
    /// BG on, tile data at 8000 (Pan Docs: Power Up Sequence), in VBlank
    /// at [`POST_BOOT_POS`].
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
        self.lcdc = 0x91;
        self.frame_start = 0u64.wrapping_sub(u64::from(POST_BOOT_POS));
        // LY = 144 against LYC = 0: no coincidence, no source enabled.
        self.seek();
    }

    #[inline]
    pub(crate) const fn lcd_enabled(&self) -> bool {
        self.lcdc & 0x80 != 0
    }

    /// See [`Emulator::screen_blank`](crate::Emulator::screen_blank).
    #[inline]
    pub(crate) const fn screen_blank(&self) -> bool {
        self.blanked || !self.lcd_enabled()
    }

    /// Line and line position of clock `now` (at most one frame past the
    /// last sync, which holds between deadlines).
    #[inline]
    fn at(&self, now: u64) -> At {
        let mut p = now.wrapping_sub(self.frame_start) as u32;
        let mut first = self.first_frame;
        if p >= FRAME {
            p -= FRAME;
            first = false;
        }
        At {
            line: p / LINE,
            t: p % LINE,
            first: first && p < LINE,
        }
    }

    /// The mode-3 end of line `l` (line position), if its timing is
    /// latched; `line_start` is the clock of its position 0.
    #[inline]
    fn latched_end(&self, line_start: u64) -> Option<u32> {
        (self.line.start == line_start).then_some(u32::from(self.line.end))
    }

    // --- Reads (side-effect free) ---

    /// LY at clock `now`.
    pub(crate) fn ly(&self, now: u64, double: bool) -> u8 {
        if !self.lcd_enabled() {
            return 0;
        }
        let a = self.at(now);
        let (l, t) = (a.line, a.t);
        let ly = match l {
            0 => 0,
            1..=143 if t < 3 => l - 1,
            1..=143 => l,
            144..=152 if t < 2 => l - 1,
            144..=152 => l,
            _ if t < 2 => 152,
            _ if t < if double { 8 } else { 6 } => 153,
            _ => 0,
        };
        ly as u8
    }

    /// The comparison value (`ly_for_comparison`) at `a`: `None` while the
    /// comparison is off. Also the last value before it, for double speed,
    /// where the STAT bit holds through it.
    fn cmp(a: At, double: bool) -> (Option<u8>, u8) {
        let (l, t) = (a.line, a.t);
        match l {
            0 => (Some(0), 0),
            1..=143 if t < 3 => (Some((l - 1) as u8), 0),
            1..=143 if t < 4 => (None, (l - 1) as u8),
            1..=143 => (Some(l as u8), 0),
            144..=152 if t < 4 => (None, (l - 1) as u8),
            144..=152 => (Some(l as u8), 0),
            _ if t < 6 => (None, 152),
            _ if t < 8 => (Some(153), 0),
            _ if t < 12 && !double => (None, 153),
            _ if t < 12 => (Some(153), 0),
            _ => (Some(0), 0),
        }
    }

    /// Mode-3 end of the line at `a` for mode reads: its latched end (+1
    /// in double speed: SameBoy changes STAT at state 22 there), or, if
    /// not latched yet, past any position it can be read at (the line's
    /// `End` deadline, at the earliest possible end, latches it first).
    fn mode3_end(&self, now: u64, a: At, double: bool) -> u32 {
        let start = now.wrapping_sub(u64::from(a.t));
        self.latched_end(start)
            .map_or(LINE, |e| e + u32::from(double))
    }

    /// STAT mode bits at `a` (Pan Docs: LCD Status Registers).
    fn mode(&self, now: u64, a: At, double: bool) -> u8 {
        let (l, t) = (a.line, a.t);
        if l >= LINES {
            return if l == 144 && t < 5 { 0 } else { 1 };
        }
        if a.first {
            return if t < 86 || t >= self.mode3_end(now, a, double) {
                0
            } else {
                3
            };
        }
        if t < 4 {
            u8::from(l == 0)
        } else if t < 84 {
            2
        } else if t < self.mode3_end(now, a, double) {
            3
        } else {
            0
        }
    }

    /// STAT at clock `now`: bit 7 reads 1; mode 0 while the LCD is off,
    /// with the coincidence bit kept from when it was turned off (Pan
    /// Docs: LCD Status Registers; SameBoy `GB_lcd_off`).
    pub(crate) fn stat(&self, now: u64, double: bool) -> u8 {
        let v = 0x80 | self.stat_select;
        if !self.lcd_enabled() {
            return v | if self.lyc_flag_off { 0x04 } else { 0 };
        }
        let a = self.at(now);
        v | self.mode(now, a, double) | if self.lyc_bit(a, double) { 0x04 } else { 0 }
    }

    /// [`Self::stat`] while an OAM DMA transfer runs (from the FF46 write
    /// to its end, as the CPU sees it): mode 2 reads as mode 0. SameBoy
    /// (`GB_STAT_update`, MIT) clears the mode bits at every STAT update
    /// while DMA is active in mode 2 (the mode-2 start, STAT/LYC/FF46
    /// writes), and `GB_dma_run` sets mode 2 again when the transfer ends
    /// during the OAM scan, so the mode reads 0 exactly while the transfer
    /// is active. Gambatte has no such rule; no test ROM we have checks
    /// it. Not affected: the STAT interrupt line.
    pub(crate) fn stat_during_dma(&self, now: u64, double: bool) -> u8 {
        let v = self.stat(now, double);
        if v & 3 == 2 { v & !3 } else { v }
    }

    /// STAT bit 2 (LY=LYC) at `a`: 0 while the comparison is off in single
    /// speed (CGB C), held in double speed (SameBoy `GB_STAT_update`). In
    /// double speed a LYC write during the off window is approximated (the
    /// hardware holds the old result).
    fn lyc_bit(&self, a: At, double: bool) -> bool {
        match Self::cmp(a, double) {
            (Some(v), _) => v == self.lyc,
            (None, prev) => double && prev == self.lyc,
        }
    }

    /// CPU access to VRAM is blocked at clock `now`: reads give FF and
    /// writes are dropped during mode 3 (Pan Docs: Rendering, "VRAM
    /// access"). CGB timing (SameBoy `vram_read_blocked` and
    /// `vram_write_blocked`, the same on CGB): from the mode-3 STAT change
    /// (first line after LCD enable: the pipeline start, or the STAT change
    /// in double speed) to the mode-3 end (one dot later in double speed).
    pub(crate) fn vram_blocked(&self, now: u64, double: bool) -> bool {
        if !self.lcd_enabled() {
            return false;
        }
        let a = self.at(now);
        if a.line >= LINES {
            return false;
        }
        let from = match (a.first, double) {
            (false, _) => 84,
            (true, false) => 91,
            (true, true) => 86,
        };
        a.t >= from && a.t < self.mode3_end(now, a, double)
    }

    /// CPU access to OAM (FE00-FEFF) is blocked at clock `now`: reads give
    /// FF and writes are dropped during the OAM scan and mode 3 (Pan Docs:
    /// Rendering, "OAM access"). CGB C timing (SameBoy `oam_read_blocked`,
    /// `oam_write_blocked`): writes from the line's first dot (double
    /// speed: its third), reads from its fourth (double speed: its fifth),
    /// both to the mode-3 end; on the first line after LCD enable, writes
    /// from SameBoy's nominal 76 and reads from 78 (no OAM scan).
    pub(crate) fn oam_blocked(&self, now: u64, double: bool, write: bool) -> bool {
        if !self.lcd_enabled() {
            return false;
        }
        let a = self.at(now);
        if a.line >= LINES {
            return false;
        }
        let from = match (a.first, write, double) {
            (true, true, _) => 84,
            (true, false, _) => 86,
            (false, true, false) => 0,
            (false, true, true) => 2,
            (false, false, false) => 3,
            (false, false, true) => 4,
        };
        a.t >= from && a.t < self.mode3_end(now, a, double)
    }

    /// `mode_for_interrupt` at `a` (SameBoy): the mode source a STAT
    /// update at this position sees.
    fn mode_source(&self, now: u64, a: At, double: bool) -> u8 {
        let (l, t) = (a.line, a.t);
        if l >= LINES {
            return if l == 144 && t < 5 { 0 } else { 1 };
        }
        let end = self.mode3_end(now, a, double);
        if a.first {
            return if t >= end { 0 } else { NO_MODE };
        }
        if t < 4 {
            if l == 0 { 1 } else { 2 }
        } else if t < end {
            NO_MODE
        } else {
            0
        }
    }

    // --- The update-point walk ---

    /// The first update point after frame position `after` (`< FRAME`),
    /// with `start` the clock of frame position 0. A position past the
    /// frame belongs to the next one. A visible line's `End` is at its
    /// latched end, or at the earliest possible end if not latched yet.
    fn next_point(&self, after: u32, first: bool, start: u64) -> (u32, Pt) {
        let (l, t) = (after / LINE, after % LINE);
        let base = l * LINE;
        if l < LINES {
            let first_line = first && l == 0;
            let pipe = if first_line { 91 } else { 89 };
            if !first_line {
                if t < 3 {
                    return (base + 3, Pt::Ly3);
                }
                if t < 4 {
                    return (base + 4, Pt::Ly4);
                }
            }
            if t < pipe {
                return (base + pipe, Pt::Latch);
            }
            match self.latched_end(start.wrapping_add(u64::from(base))) {
                // The earliest end: 167 dots after the pipeline start.
                None => return (base + pipe + 167, Pt::End),
                Some(e) if t < e => return (base + e, Pt::End),
                Some(e) if t <= e => return (base + e + 1, Pt::HBlank),
                Some(_) => {}
            }
            return if l < LINES - 1 {
                (base + LINE + 3, Pt::Ly3)
            } else {
                (base + LINE, Pt::V0)
            };
        }
        if l == 144 {
            return match t {
                0..2 => (base + 2, Pt::Quirk2),
                2..4 => (base + 4, Pt::V4),
                4 => (VBLANK_POS, Pt::VBlank),
                _ => (base + LINE, Pt::V0),
            };
        }
        if l < 153 {
            return if t < 4 {
                (base + 4, Pt::V4)
            } else {
                (base + LINE, Pt::V0)
            };
        }
        match t {
            0..6 => (base + 6, Pt::L153Cmp),
            6..8 => (base + 8, Pt::L153Off),
            8..12 => (base + 12, Pt::L153Zero),
            _ => (FRAME + 3, Pt::Ly3),
        }
    }

    /// [`Self::next_point`] while no STAT interrupt source is selected
    /// (`quiet`): only the points that do work regardless (`Latch`, `End`,
    /// `VBlank`). The STAT line stays low, and the LY=LYC part of it is
    /// recomputed when a source is selected again ([`Self::write_stat`]).
    fn next_work_point(&self, after: u32, first: bool, start: u64) -> (u32, Pt) {
        let (l, t) = (after / LINE, after % LINE);
        let base = l * LINE;
        if l < LINES {
            let pipe = if first && l == 0 { 91 } else { 89 };
            if t < pipe {
                return (base + pipe, Pt::Latch);
            }
            match self.latched_end(start.wrapping_add(u64::from(base))) {
                None => return (base + pipe + 167, Pt::End),
                Some(e) if t < e => return (base + e, Pt::End),
                Some(_) => {}
            }
            return if l < LINES - 1 {
                (base + LINE + 89, Pt::Latch)
            } else {
                (VBLANK_POS, Pt::VBlank)
            };
        }
        if after < VBLANK_POS {
            return (VBLANK_POS, Pt::VBlank);
        }
        (FRAME + 89, Pt::Latch)
    }

    /// The cursor for the current state: the first update point after
    /// `synced` (`< frame_start + FRAME`, as between syncs), by search.
    fn seek_point(&self) -> Cursor {
        let pos = self.synced.wrapping_sub(self.frame_start) as u32;
        let (pos, pt) = if self.quiet() {
            self.next_work_point(pos, self.first_frame, self.frame_start)
        } else {
            self.next_point(pos, self.first_frame, self.frame_start)
        };
        let line = if pos >= FRAME { 0 } else { (pos / LINE) as u8 };
        Cursor { pos, line, pt }
    }

    /// Re-seeks the cursor after a change to what the walk visits (LCD
    /// enable, reset, the STAT selects becoming empty or not).
    #[cold]
    fn seek(&mut self) {
        self.cursor = self.seek_point();
    }

    /// The update point after `c` (which is at or before the next frame's
    /// first point; `start` is the clock of `c`'s frame position 0): what
    /// [`Self::next_point`] (or [`Self::next_work_point`] if `quiet`)
    /// returns for `c.pos`, without dividing. A visible line's `End` is at
    /// its latched end, or at the earliest possible end if not latched yet.
    #[inline]
    fn step(&self, c: Cursor, quiet: bool, start: u64) -> Cursor {
        let l = u32::from(c.line);
        let base = l * LINE;
        let (pos, line, pt) = match c.pt {
            Pt::Ly3 => (c.pos + 1, l, Pt::Ly4),
            Pt::Ly4 => (base + 89, l, Pt::Latch),
            Pt::Latch => {
                let end = match self.latched_end(start.wrapping_add(u64::from(base))) {
                    Some(e) => base + e,
                    // The earliest end: 167 dots after the pipeline start.
                    None => c.pos + 167,
                };
                (end, l, Pt::End)
            }
            Pt::End if !quiet => (c.pos + 1, l, Pt::HBlank),
            Pt::End | Pt::HBlank if l < LINES - 1 => {
                if quiet {
                    (base + LINE + 89, l + 1, Pt::Latch)
                } else {
                    (base + LINE + 3, l + 1, Pt::Ly3)
                }
            }
            Pt::End => (VBLANK_POS, LINES, Pt::VBlank),
            Pt::HBlank => (base + LINE, LINES, Pt::V0),
            Pt::V0 => match l {
                144 => (c.pos + 2, l, Pt::Quirk2),
                153 => (c.pos + 6, l, Pt::L153Cmp),
                _ => (c.pos + 4, l, Pt::V4),
            },
            Pt::Quirk2 => (c.pos + 2, l, Pt::V4),
            Pt::V4 if l == LINES => (VBLANK_POS, l, Pt::VBlank),
            Pt::V4 => (base + LINE, l + 1, Pt::V0),
            Pt::VBlank if quiet => (FRAME + 89, 0, Pt::Latch),
            Pt::VBlank => (base + LINE, l + 1, Pt::V0),
            Pt::L153Cmp => (c.pos + 2, l, Pt::L153Off),
            Pt::L153Off => (c.pos + 4, l, Pt::L153Zero),
            Pt::L153Zero => (FRAME + 3, 0, Pt::Ly3),
        };
        Cursor {
            pos,
            line: line as u8,
            pt,
        }
    }

    /// No STAT interrupt source is selected.
    #[inline]
    fn quiet(&self) -> bool {
        self.stat_select == 0
    }

    /// Applies the STAT-line part of update point `pt` on line `l` to
    /// `line`. Returns whether it requests the STAT interrupt.
    fn point_irq(&self, line: &mut IrqLine, pt: Pt, l: u32, double: bool) -> bool {
        let (sel, lyc) = (self.stat_select, self.lyc);
        let ly = l as u8;
        match pt {
            Pt::Ly3 if l == 0 => line.eval(sel, lyc, 1, Some(0)),
            Pt::Ly3 => line.eval(sel, lyc, 2, None),
            Pt::Ly4 => {
                let a = line.eval(sel, lyc, 2, Some(ly));
                let b = line.eval(sel, lyc, NO_MODE, Some(ly));
                a | b
            }
            Pt::Latch | Pt::End => false,
            Pt::HBlank => line.eval(sel, lyc, 0, Some(ly)),
            Pt::V0 => line.eval(sel, lyc, if l == 144 { 0 } else { 1 }, None),
            Pt::Quirk2 => !line.stat && sel & SEL_MODE2 != 0,
            Pt::V4 => line.eval(sel, lyc, if l == 144 { 0 } else { 1 }, Some(ly)),
            Pt::VBlank => {
                let quirk = !line.stat && sel & SEL_MODE2 != 0;
                line.eval(sel, lyc, 1, Some(144)) | quirk
            }
            Pt::L153Cmp => line.eval(sel, lyc, 1, Some(153)),
            Pt::L153Off => line.eval(sel, lyc, 1, if double { Some(153) } else { None }),
            Pt::L153Zero => line.eval(sel, lyc, 1, Some(0)),
        }
    }

    /// When [`Ppu::sync`] must next run: the first update point after the
    /// last sync that requests an interrupt, completes the frame or ends a
    /// visible line's mode 3; never while the LCD is off.
    pub(crate) fn next_event(&self, double: bool) -> u64 {
        if !self.lcd_enabled() {
            return NEVER;
        }
        let mut line = self.line_irq;
        let mut start = self.frame_start;
        let mut c = self.cursor;
        let quiet = self.quiet();
        loop {
            if c.pos >= FRAME {
                c.pos -= FRAME;
                start = start.wrapping_add(u64::from(FRAME));
            }
            let fires = matches!(c.pt, Pt::End | Pt::VBlank)
                || (!quiet
                    && c.pt != Pt::Latch
                    && self.point_irq(&mut line, c.pt, u32::from(c.line), double));
            if fires {
                return start.wrapping_add(u64::from(c.pos));
            }
            c = self.step(c, quiet, start);
        }
    }

    /// Catches up to clock `now`: runs every update point up to it,
    /// requesting the STAT and VBlank interrupts in `iflags`. Returns
    /// `FRAME_READY` if VBlank started on the way.
    pub(crate) fn sync(
        &mut self,
        now: u64,
        mem: &mut Memory,
        frame: &mut Frame,
        iflags: &mut u8,
        double: bool,
    ) -> Events {
        let mut events = Events::EMPTY;
        if !self.lcd_enabled() {
            self.synced = now;
            return events;
        }
        let quiet = self.quiet();
        while self.frame_start.wrapping_add(u64::from(self.cursor.pos)) <= now {
            if self.cursor.pos >= FRAME {
                self.next_frame();
            }
            let c = self.cursor;
            self.synced = self.frame_start.wrapping_add(u64::from(c.pos));
            let l = u32::from(c.line);
            // `Latch` and `End` don't change the STAT line.
            if !quiet && !matches!(c.pt, Pt::Latch | Pt::End) {
                let mut line = self.line_irq;
                if self.point_irq(&mut line, c.pt, l, double) {
                    *iflags |= INT_STAT;
                }
                self.line_irq = line;
            }
            match c.pt {
                Pt::Latch => self.latch(l, self.first_frame && l == 0, mem),
                // The rest of line `l` (to pixel 160). VRAM, OAM and the
                // palettes are blocked for the CPU during mode 3, so
                // drawing here sees what the hardware saw; register writes
                // during mode 3 flushed the pixels before them already.
                Pt::End => {
                    if self.attr_on {
                        self.end_dma_line(c.line, mem, frame);
                    } else {
                        self.draw(c.line, 160, mem, frame);
                    }
                }
                Pt::VBlank => {
                    *iflags |= INT_VBLANK;
                    events |= self.frame_done();
                }
                _ => {}
            }
            // After the point's work: `Latch` fixes where `End` is.
            self.cursor = self.step(c, quiet, self.frame_start);
        }
        self.synced = now;
        if now.wrapping_sub(self.frame_start) >= u64::from(FRAME) {
            // Past line 153, before the next frame's first point.
            self.next_frame();
        }
        debug_assert_eq!(self.cursor, self.seek_point());
        events
    }

    /// Frame position 0 of the next frame is reached (the cursor is in
    /// it: rebased).
    fn next_frame(&mut self) {
        self.frame_start = self.frame_start.wrapping_add(u64::from(FRAME));
        self.cursor.pos -= FRAME;
        self.first_frame = false;
        // The window line counter and the WY condition restart (SameBoy
        // resets them after line 153).
        self.renderer.start_frame();
    }

    /// The frame is complete (VBlank): every visible line has been drawn
    /// at its mode-3 end.
    fn frame_done(&mut self) -> Events {
        if self.skip_frame {
            self.skip_frame = false;
        } else {
            self.blanked = false;
        }
        Events::FRAME_READY
    }

    /// SameBoy `wy_check`: WY = LY with the window enabled latches the
    /// window's vertical condition for the rest of the frame (Pan Docs:
    /// Window, "WY condition").
    fn check_wy(&mut self, l: u32, mem: &Memory) {
        let regs = self.regs(mem);
        self.renderer.check_wy(l as u8, &regs);
    }

    /// The registers the renderer reads: LCDC here, the others in the
    /// I/O backing store (written by the bus after it has flushed).
    #[inline]
    fn regs(&self, mem: &Memory) -> Regs {
        Regs {
            lcdc: self.lcdc,
            scy: mem.io[0x42],
            scx: mem.io[0x43],
            wy: mem.io[0x4A],
            wx: mem.io[0x4B],
        }
    }

    /// Draws line `ly` from the last pixel drawn up to pixel `x` with the
    /// current registers. Out of line: the pixel loop is the PPU's bulk
    /// and runs once or a few times per line.
    #[inline(never)]
    fn draw(&mut self, ly: u8, x: u8, mem: &mut Memory, frame: &mut Frame) {
        let from = self.drawn;
        if x <= from {
            return;
        }
        self.drawn = x;
        let regs = self.regs(mem);
        let src = Sources {
            vram: &mem.vram,
            oam: &mem.oam,
            attrs: self.attr_on.then_some(&self.obj_attr),
            colors: &self.colors,
            pairs: &mem.bg_pairs,
        };
        let row = render::frame_row(frame, usize::from(ly));
        self.renderer.render(
            row,
            usize::from(from),
            usize::from(x),
            &regs,
            &src,
            &mut mem.obj_buf,
        );
    }

    /// Before a write at clock `now` (synced) to a register the picture
    /// depends on (LCDC, SCY, SCX, BGP, OBP0, OBP1, WY, WX): if a line is
    /// in mode 3, draws it up to the pixel being output with the old
    /// value (ADR 0002 catch-up), so the write affects the pixels from
    /// there on.
    pub(crate) fn flush(&mut self, now: u64, mem: &mut Memory, frame: &mut Frame) {
        if let Some((ly, x)) = self.mode3_pixel(now) {
            if self.attr_on {
                // OAM DMA touches the line: the objects fetched so far get
                // what their fetches read.
                self.fix_attrs(mem, NEVER);
            }
            self.draw(ly, x, mem, frame);
        }
    }

    /// A BGP (FF47), OBP0 (FF48) or OBP1 (FF49) write: resolves that
    /// palette's colors now (ADR 0013), after the bus has flushed. In CGB
    /// mode BGP has no effect on colors (Pan Docs: FF47 BGP, "Non-CGB
    /// Mode only"; the BG colors come from palette RAM).
    // TODO(M5): OBP0/OBP1 too; CGB-mode objects still use them until the
    // CGB object attributes (palette number) and OCPD colors are done.
    pub(crate) fn write_palette(
        &mut self,
        reg: usize,
        value: u8,
        dmg: &DmgPalettes,
        cgb: bool,
        pairs: &mut [[u32; 16]; 8],
    ) {
        match reg {
            0x47 if cgb => {}
            0x47 => {
                self.colors.set_bgp(dmg, value);
                pairs[0] = pair_table(&self.colors.bg[0]);
            }
            _ => self.colors.set_obp(dmg, reg - 0x48, value),
        }
    }

    /// A BCPD write (CGB mode) to palette RAM byte `index` (0-63), after
    /// the bus has flushed and stored it: re-resolves the one color it is
    /// part of (2 bytes, little-endian RGB555, bit 15 not a color bit; Pan
    /// Docs: LCD Color Palettes) and its pair-table entries. Hi-color
    /// raster effects write palette RAM every line (synth:cgb-raster).
    pub(crate) fn write_bcpd(&mut self, index: usize, mem: &mut Memory) {
        let (pal, c, i) = ((index >> 3) & 7, (index >> 1) & 3, index & 0x3E);
        let col = u16::from_le_bytes([mem.bg_pal[i], mem.bg_pal[i + 1]]) & 0x7FFF;
        self.colors.bg[pal][c] = col;
        set_pair_color(&mut mem.bg_pairs[pal], c, col);
    }

    /// Whether the PPU, synced to some clock, is between a visible line's
    /// `Latch` and `End` points (its next update point is `End`). Every
    /// clock where [`mode3_pixel`](Self::mode3_pixel) is `Some` is: that
    /// needs the line latched (`Latch` passed) and a time before its mode-3
    /// end, where `End` is, with no point between the two. So a flush at
    /// the synced clock can skip the line-position arithmetic when this is
    /// false.
    #[inline]
    pub(crate) fn drawing(&self) -> bool {
        matches!(self.cursor.pt, Pt::End)
    }

    /// Resolves all palettes from the registers and palette RAM in `mem`
    /// (reset, or new compatibility colors from the front end): in DMG
    /// compatibility mode BG palette 0 and OBJ palettes 0-1 from BGP/OBP0/
    /// OBP1, in CGB mode the BG palettes from palette RAM.
    pub(crate) fn resolve_palettes(&mut self, mem: &mut Memory, dmg: &DmgPalettes, cgb: bool) {
        self.colors = Colors::dmg_compat(dmg, mem.io[0x47], mem.io[0x48], mem.io[0x49]);
        if cgb {
            for pal in 0..8 {
                self.colors.set_cgb_bg(&mem.bg_pal, pal);
            }
        }
        for (t, pal) in mem.bg_pairs.iter_mut().zip(&self.colors.bg) {
            *t = pair_table(pal);
        }
    }

    /// A WY or LCDC write at clock `now` (synced): the WY condition is
    /// checked against the current line right away (SameBoy
    /// `wy_check_scheduled`, a few dots later).
    pub(crate) fn check_wy_at(&mut self, now: u64, mem: &Memory) {
        if self.lcd_enabled() {
            let l = self.at(now).line;
            if l < LINES {
                self.check_wy(l, mem);
            }
        }
    }

    /// The pixel pipeline of line `l` starts: fix its mode-3 timing from
    /// SCX, the window and the objects the OAM scan selected (none on the
    /// first line after LCD enable). Out of line: it runs once per line,
    /// and inlined, its register loads were hoisted into every sync.
    #[inline(never)]
    fn latch(&mut self, l: u32, first: bool, mem: &Memory) {
        // SameBoy checks at the line start and on WY/LCDC writes (bus
        // `write_ppu` calls `check_wy` too); here before the window's
        // start is fixed.
        self.check_wy(l, mem);
        let scx = mem.io[0x43];
        let wx = mem.io[0x4B];
        // The window starts on this line at pixel WX - 7 (SameBoy:
        // activates for WX < 166 + is_cgb).
        let window =
            (self.lcdc & 0x20 != 0 && self.renderer.wy_triggered() && wx <= 166).then_some(wx);
        let tall = self.lcdc & 0x04 != 0;
        let start = self.synced.wrapping_sub(if first { 91 } else { 89 });
        // One OAM scan per line, for both the stalls and the pixels. On a
        // line OAM DMA touches (from the FF46 write to the transfer's end
        // as the PPU sees it), entry by entry, and the objects' fetches
        // follow the transfer too.
        let mut aware = false;
        let objs = if first {
            LineObjects::EMPTY
        } else if self.dma.ppu_live && self.dma_touches(start) {
            aware = true;
            self.scan_line(l, start, mem)
        } else {
            // No OAM read of this line is tracked on the mode-2 bus.
            self.bus_live = false;
            if self.objects.on_line(&mem.oam, l, tall) {
                LineObjects::scan(&mem.oam, l as u8, tall)
            } else {
                LineObjects::EMPTY
            }
        };
        // Priority order is ascending X (compatibility mode): sorted once
        // for both the stalls and the renderer.
        let n = objs.len();
        let order = objs.priority_order();
        let mut xs = [0u8; objects::MAX_PER_LINE];
        for (x, &slot) in xs.iter_mut().zip(&order[..n]) {
            *x = objs.get(usize::from(slot)).2;
        }
        let mode3 = if first { 86 } else { 84 };
        // 12 dots of tile fetches, then SCX % 8 dots discarding pixels.
        let first_pixel = mode3 + 12 + u16::from(scx & 7);
        let t = &mut self.line;
        t.start = start;
        t.mode3 = mode3;
        t.first_pixel = first_pixel;
        t.win_stall = NO_STALL;
        let mut total = 0u16;
        let mut k = 0;
        if n != 0 || window.is_some() {
            let mut stalls = [(0u8, 0u8); 10];
            let n_obj = sorted_object_stalls(&xs[..n], scx, window, &mut stalls);
            let mut win = window.map(|wx| (wx.saturating_sub(7), 6u8));
            for &s in &stalls[..n_obj] {
                if let Some(w) = win.filter(|w| w.0 <= s.0) {
                    t.win_stall = k as u8;
                    t.stalls[k] = w;
                    k += 1;
                    total += u16::from(w.1);
                    win = None;
                }
                t.stalls[k] = s;
                k += 1;
                total += u16::from(s.1);
            }
            if let Some(w) = win {
                t.win_stall = k as u8;
                t.stalls[k] = w;
                k += 1;
                total += u16::from(w.1);
            }
        }
        t.n_stalls = k as u8;
        t.end = first_pixel + 160 + total;
        self.drawn = 0;
        self.renderer.start_line(l as u8, scx, &objs, order);
        self.attr_on = false;
        if aware {
            self.attr_final = 0;
            self.attr_on = n != 0;
        }
    }

    /// The transfer (live since its FF46 write) still touches the line
    /// starting at clock `start`; once it doesn't, it never will again.
    #[inline]
    fn dma_touches(&mut self, start: u64) -> bool {
        let touches = self.dma.ppu_to >= start;
        self.dma.ppu_live = touches;
        touches
    }

    /// OAM changed (a CPU or DMA write): the object summary is stale.
    #[inline]
    pub(crate) fn oam_written(&mut self) {
        self.objects.valid = false;
    }

    // --- OAM DMA as the PPU sees it (`docs/reference/oam-dma.md`) ---

    /// Runs the OAM scan of line `l` (which starts at clock `start`) for
    /// the entries read up to clock `until`, continuing a scan in
    /// progress. Per entry, by what the transfer looks like then
    /// ([`OamDma::ppu_at`]; SameBoy `add_object_from_index`, MIT):
    /// - off: the entry's Y and X as OAM holds them then;
    /// - running: OAM can't be read; the scan compares the Y and X it last
    ///   read (the mode-2 bus, [`Self::bus`]), so unless that Y covers the
    ///   line, nothing is found (Pan Docs: "most PPU revisions read each
    ///   object as being off-screen");
    /// - halted (the CPU in HALT with bytes left): the scan reads the word
    ///   being written, but on CGB revisions 0-D finds nothing.
    #[cold]
    #[inline(never)]
    fn scan_to(&mut self, l: u32, start: u64, until: u64, mem: &Memory) {
        if self.scan.line != start {
            self.scan = ScanProgress {
                line: start,
                ..ScanProgress::NONE
            };
        }
        let (ly, tall) = (l as u8, self.lcdc & 0x04 != 0);
        let d = self.dma;
        // A run of entries at a time: the transfer looks the same from the
        // first one's clock to `edge`.
        while self.scan.next < 40 {
            let i = self.scan.next;
            let c = start.wrapping_add(entry_pos(i));
            if c > until {
                break;
            }
            let (view, dest) = d.ppu_at(c);
            let edge = match view {
                PpuView::Off if dest == 0 => d.ppu_from,
                PpuView::Off => NEVER,
                PpuView::Running if d.halt_from >= c => d.ppu_to.min(d.halt_from),
                PpuView::Running => d.ppu_to,
                PpuView::Halted => d.ppu_to.min(d.halt_to),
            };
            // The last entry read by `edge` and `until`.
            let span = edge.min(until).wrapping_sub(start).saturating_sub(4) / 2;
            let last = span.min(39) as u8;
            let read = |e: u8, b| OamDma::ppu_read(dest, e * 4 + b, &mem.oam, &mem.oam_dma_old);
            match view {
                PpuView::Off => {
                    for e in i..=last {
                        self.scan.objs.consider(e, read(e, 0), read(e, 1), ly, tall);
                    }
                    self.bus = [read(last, 0), read(last, 1)];
                    self.bus_live = true;
                }
                PpuView::Halted => {
                    self.bus = [read(last, 0), read(last, 1)];
                    self.bus_live = true;
                }
                PpuView::Running => {
                    if !self.bus_live {
                        self.init_bus(mem);
                    }
                    let [y, x] = self.bus;
                    if LineObjects::covers(y, ly, tall) {
                        for e in i..=last {
                            self.scan.objs.consider(e, y, x, ly, tall);
                        }
                    }
                }
            }
            self.scan.next = last + 1;
        }
    }

    /// The mode-2 bus when tracking starts: the last OAM reads were the
    /// last line's OAM scan (entry 39) and then its object fetches (the
    /// last one's tile index; SameBoy reuses `mode2_y_bus` for it), read
    /// from OAM as it is now (exact unless the CPU wrote OAM since).
    #[cold]
    fn init_bus(&mut self, mem: &Memory) {
        self.bus = [mem.oam[0x9C], mem.oam[0x9D]];
        if let Some(slot) = self.renderer.last_fetched() {
            self.bus[0] = if self.attr_on {
                self.obj_attr[slot].0
            } else {
                mem.oam[usize::from(self.renderer.objects.get(slot).0) % 40 * 4 + 2]
            };
        }
        self.bus_live = true;
    }

    /// The tile index and attributes each object of the current line gets
    /// from its fetch in mode 3, for the slots fetched up to clock `upto`
    /// (`NEVER`: all) that aren't final yet; with `upto` they become final.
    /// While bytes 1-159 are being written the fetch reads the word being
    /// written ([`OamDma::ppu_read`]). The fetch's OAM read is placed
    /// `d - 5` dots into the object's `d`-dot stall (SameBoy: after the
    /// wait for the BG fetch and one more dot; untested).
    #[cold]
    #[inline(never)]
    fn fix_attrs(&mut self, mem: &Memory, upto: u64) {
        let t = self.line;
        let r = &mut self.renderer;
        let mut slots = r.order[..r.objects.len()]
            .iter()
            .map(|&s| usize::from(s) % 10)
            .filter(|&s| r.objects.get(s).2 < 168);
        let (mut clock, mut x) = (u32::from(t.first_pixel), 0u32);
        for (j, &(sx, d)) in t.stalls[..usize::from(t.n_stalls) % 12].iter().enumerate() {
            clock += u32::from(sx).saturating_sub(x);
            x = x.max(u32::from(sx));
            let begin = clock;
            clock += u32::from(d);
            if j == usize::from(t.win_stall) {
                continue;
            }
            let Some(slot) = slots.next() else { break };
            let c = t.start.wrapping_add(u64::from(begin + u32::from(d) - 5));
            if self.attr_final & (1 << slot) != 0 || c > upto {
                continue;
            }
            let (_, dest) = self.dma.ppu_at(c);
            let base = r.objects.get(slot).0 % 40 * 4;
            let read = |b| OamDma::ppu_read(dest, base + b, &mem.oam, &mem.oam_dma_old);
            self.obj_attr[slot] = (read(2), read(3));
            if upto != NEVER {
                self.attr_final |= 1 << slot;
            }
        }
    }

    /// The End of a line OAM DMA touches: its objects get what their
    /// fetches read, it is drawn, and its last object fetch leaves that
    /// object's tile index on the mode-2 bus.
    #[cold]
    #[inline(never)]
    fn end_dma_line(&mut self, ly: u8, mem: &mut Memory, frame: &mut Frame) {
        self.fix_attrs(mem, NEVER);
        self.draw(ly, 160, mem, frame);
        if let Some(slot) = self.renderer.last_fetched() {
            self.bus[0] = self.obj_attr[slot].0;
        }
    }

    /// The whole OAM scan of line `l` (starting at clock `start`) on a
    /// line OAM DMA touches, or the rest of one in progress.
    #[cold]
    #[inline(never)]
    fn scan_line(&mut self, l: u32, start: u64, mem: &Memory) -> LineObjects {
        self.scan_to(l, start, start.wrapping_add(entry_pos(39)), mem);
        self.scan.line = NEVER;
        self.scan.objs
    }

    /// Before the OAM DMA state changes at clock `now` (synced): the OAM
    /// reads made up to now get the state they saw. In mode 3 the objects
    /// fetched so far are fixed and the line is drawn up to now; in the
    /// OAM scan the entries read so far are scanned. The mode-2 bus is set
    /// if it isn't tracked yet.
    #[cold]
    #[inline(never)]
    pub(crate) fn dma_event(&mut self, now: u64, mem: &mut Memory, frame: &mut Frame) {
        if !self.lcd_enabled() {
            return;
        }
        let a = self.at(now);
        let start = now.wrapping_sub(u64::from(a.t));
        if a.line < LINES && self.line.start == start {
            // Latched: mode 3 or later. From here on the line's object
            // fetches follow the transfer.
            if usize::from(self.drawn) < SCREEN_WIDTH {
                if !self.attr_on {
                    self.attr_final = 0;
                }
                self.fix_attrs(mem, now);
                self.attr_on = self.renderer.objects.len() != 0;
                self.flush(now, mem, frame);
            }
        } else if a.line < LINES && !a.first {
            self.scan_to(a.line, start, now, mem);
        }
        if !self.bus_live {
            self.init_bus(mem);
        }
    }

    // --- Writes (the bus syncs the PPU to the write's clock first) ---

    /// A STAT update at clock `now` (a STAT or LYC write): re-evaluates the
    /// interrupt line with the mode source and comparison of that
    /// position.
    fn update_at(&mut self, now: u64, iflags: &mut u8, double: bool) {
        let a = self.at(now);
        let mode = self.mode_source(now, a, double);
        let cmp = Self::cmp(a, double).0;
        if self.line_irq.eval(self.stat_select, self.lyc, mode, cmp) {
            *iflags |= INT_STAT;
        }
    }

    /// One phase of a STAT write at clock `now`: the interrupt selects
    /// become `value`'s bits 3-6, then the line is re-evaluated (no STAT
    /// write bug on CGB). The bus writes in two phases (SameBoy
    /// `GB_CONFLICT_STAT_CGB`: the LYC select keeps its old value for the
    /// first dot; double speed: the mode-0 select).
    pub(crate) fn write_stat(&mut self, now: u64, value: u8, iflags: &mut u8, double: bool) {
        let was_quiet = self.quiet();
        self.stat_select = value & 0x78;
        if !self.lcd_enabled() {
            return;
        }
        if was_quiet != self.quiet() {
            // The walk visits other points now (synced to `now`).
            self.seek();
        }
        if was_quiet {
            // The line was low; its LY=LYC part wasn't tracked. While the
            // comparison is off it holds the last result (approximated
            // with the current LYC; exact unless LYC changed in the last
            // few dots).
            let a = self.at(now);
            let lyc = match Self::cmp(a, double) {
                (Some(v), _) | (None, v) => v == self.lyc,
            };
            self.line_irq = IrqLine { stat: false, lyc };
        }
        self.update_at(now, iflags, double);
    }

    /// The STAT selects as written (bits 3-6).
    #[inline]
    pub(crate) fn stat_select(&self) -> u8 {
        self.stat_select
    }

    /// A LYC write taking effect at clock `now` (single speed: one dot
    /// after the access, SameBoy `GB_CONFLICT_WRITE_CPU`). Ignored by the
    /// comparison while the LCD is off. On CGB the update is deferred to
    /// the PPU's own next one while LY is changing (SameBoy states 35, 26,
    /// 15 and 16), and in two windows of line 153 the new value is first
    /// compared with 153 (SameBoy's states 14 and 29 "hacks", measured).
    pub(crate) fn write_lyc(&mut self, now: u64, value: u8, iflags: &mut u8, double: bool) {
        self.lyc = value;
        if !self.lcd_enabled() {
            return;
        }
        let a = self.at(now);
        let (l, t) = (a.line, a.t);
        let deferred = match l {
            0..=152 => t < 2 && !a.first,
            _ => (6..12).contains(&t),
        };
        if deferred {
            return;
        }
        let (sel, lyc) = (self.stat_select, self.lyc);
        let mut rose = false;
        if l == 153 && (2..6).contains(&t) {
            rose = self.line_irq.eval(sel, lyc, 1, Some(153));
        } else {
            if l == 153 && (12..24).contains(&t) {
                rose = self.line_irq.eval(sel, lyc, 1, Some(153));
            }
            let mode = self.mode_source(now, a, double);
            let cmp = Self::cmp(a, double).0;
            rose |= self.line_irq.eval(sel, lyc, mode, cmp);
        }
        if rose {
            *iflags |= INT_STAT;
        }
    }

    /// A CPU write to LCDC (FF40) at clock `now`; the caller has synced to
    /// `now`. Turning the LCD off abandons the frame in progress and blanks
    /// the screen: LY reads 0 and STAT mode 0, the coincidence bit and the
    /// STAT interrupt line keep their values (SameBoy `GB_lcd_off`).
    /// Turning it on starts the short first line (module docs), and its
    /// first STAT update compares LY = 0 with LYC (Mooneye
    /// `acceptance/ppu/stat_lyc_onoff`).
    ///
    /// The first frame after the LCD is turned on is not displayed: the
    /// screen stays blank until the second one completes (Pan Docs: LCDC.7;
    /// SameBoy Core/display.c: enabling the LCD sets `frame_skip`, and
    /// `GB_display_vblank` then outputs a blank frame instead).
    pub(crate) fn write_lcdc(&mut self, now: u64, value: u8, iflags: &mut u8, double: bool) {
        debug_assert_eq!(self.synced, now);
        let was_on = self.lcd_enabled();
        let on = value & 0x80 != 0;
        if was_on && !on {
            self.lyc_flag_off = self.lyc_bit(self.at(now), double);
            self.blanked = true;
            self.skip_frame = false;
            self.line = LineTiming::NONE;
        }
        self.lcdc = value;
        if !was_on && on {
            // The screen was white while the LCD was off (also if it has
            // been off since power-on) and stays white for this frame.
            self.blanked = true;
            self.skip_frame = true;
            self.frame_start = now.wrapping_sub(u64::from(ON_POS));
            self.first_frame = true;
            self.renderer.start_frame();
            self.line = LineTiming::NONE;
            self.seek();
            // SameBoy's first update, one dot after the write: no mode
            // source, LY = 0 against LYC.
            if self
                .line_irq
                .eval(self.stat_select, self.lyc, NO_MODE, Some(0))
            {
                *iflags |= INT_STAT;
            }
        }
    }

    /// The pixel being drawn at clock `now`: `Some((LY, x))` while line LY
    /// is in mode 3 (pixels `0..x` are out), else `None`. The renderer
    /// flushes up to here before a register write changes the picture
    /// (ADR 0002 catch-up). Valid once the line's pipeline has started.
    pub(crate) fn mode3_pixel(&self, now: u64) -> Option<(u8, u8)> {
        if !self.lcd_enabled() {
            return None;
        }
        let a = self.at(now);
        let start = now.wrapping_sub(u64::from(a.t));
        if a.line >= LINES || self.line.start != start || a.t < u32::from(self.line.mode3) {
            return None;
        }
        (a.t < u32::from(self.line.end)).then(|| (a.line as u8, self.line.pixels_before(a.t)))
    }
}

/// The visible lines any object covers (bit `ly`), from OAM Y and the
/// object height, rebuilt when OAM or the height changed: most lines have
/// no object, and their OAM scan is skipped (`Ppu::latch`).
#[derive(Clone, Copy)]
struct ObjectLines {
    bits: [u32; 5],
    tall: bool,
    /// No OAM write since the bits were built ([`Ppu::oam_written`]).
    valid: bool,
}

impl ObjectLines {
    const STALE: Self = Self {
        bits: [0; 5],
        tall: false,
        valid: false,
    };

    /// Some object covers line `ly`.
    fn on_line(&mut self, oam: &[u8; 0xA0], ly: u32, tall: bool) -> bool {
        if !self.valid || self.tall != tall {
            self.build(oam, tall);
        }
        self.bits[(ly >> 5) as usize] & (1 << (ly & 31)) != 0
    }

    fn build(&mut self, oam: &[u8; 0xA0], tall: bool) {
        let height = if tall { 16 } else { 8 };
        self.bits = [0; 5];
        for obj in oam.as_chunks::<4>().0 {
            // Rows Y-16 .. Y-16+height, clipped to the screen.
            let top = i32::from(obj[0]) - 16;
            for ly in top.max(0)..(top + height).min(LINES as i32) {
                self.bits[(ly >> 5) as usize] |= 1 << (ly & 31);
            }
        }
        self.tall = tall;
        self.valid = true;
    }
}

/// The mode-3 stalls of the objects at X coordinates `xs` (the selected
/// objects, OAM order, up to 10), as `(pixel x, dots)` in x order, written
/// to `out`; returns how many. Pan Docs: Rendering, "OBJ penalty
/// algorithm", for each object by X (only its leftmost pixel, "The
/// Pixel", at screen x = X - 8, matters):
/// 1. find the BG or window tile The Pixel is in (`window`: WX, if the
///    window starts on this line);
/// 2. if no earlier object was in that tile, wait for its fetch:
///    `max(0, pixels of the tile right of The Pixel - 2)` dots;
/// 3. fetch the object: 6 dots.
///
/// An object at X = 0 costs 11 dots if it is the first in its tile. Objects
/// at X >= 168 are off screen and cost nothing. Checked against every case
/// of Mooneye `acceptance/ppu/intr_2_mode0_timing_sprites` (unit test
/// below; SameBoy's fetcher gives the same lengths).
#[cfg(test)]
pub(crate) fn object_stalls(
    xs: &[u8],
    scx: u8,
    window: Option<u8>,
    out: &mut [(u8, u8); 10],
) -> usize {
    let mut sorted = [0u8; 10];
    let n = xs.len().min(10);
    sorted[..n].copy_from_slice(&xs[..n]);
    sorted[..n].sort_unstable();
    sorted_object_stalls(&sorted[..n], scx, window, out)
}

/// [`object_stalls`] of X coordinates already in ascending order (the
/// line's priority order, [`LineObjects::priority_order`]).
fn sorted_object_stalls(
    sorted: &[u8],
    scx: u8,
    window: Option<u8>,
    out: &mut [(u8, u8); 10],
) -> usize {
    let n = sorted.len().min(10);
    let win_x = window.map(|wx| i32::from(wx) - 7);
    let mut last_tile = None;
    let mut k = 0;
    for &x in &sorted[..n] {
        if x >= 168 {
            break;
        }
        let px = i32::from(x) - 8;
        let (tile, right) = match win_x {
            Some(w) if px >= w => ((true, (px - w) >> 3), 7 - ((px - w) & 7)),
            _ => {
                let bx = px + i32::from(scx);
                ((false, (bx + 8) >> 3), 7 - (bx & 7))
            }
        };
        let mut dots = 6;
        if last_tile != Some(tile) {
            last_tile = Some(tile);
            dots += if x == 0 { 5 } else { (right - 2).max(0) };
        }
        out[k] = (px.max(0) as u8, dots as u8);
        k += 1;
    }
    k
}

/// The total mode-3 penalty of the objects at X coordinates `xs` (see
/// [`object_stalls`]).
#[cfg(test)]
pub(crate) fn object_penalty(xs: &[u8], scx: u8, window: Option<u8>) -> u32 {
    let mut out = [(0, 0); 10];
    let n = object_stalls(xs, scx, window, &mut out);
    out[..n].iter().map(|&(_, d)| u32::from(d)).sum()
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    use std::vec;
    use std::vec::Vec;

    /// A PPU with its own memory and frame, driven like the bus drives it.
    struct Rig {
        ppu: Ppu,
        mem: Box<Memory>,
        frame: Box<Frame>,
        iflags: u8,
    }

    impl Rig {
        /// LCD off at clock 0, then on at `on` with STAT selects `select`
        /// and LYC `lyc`.
        fn on_at(on: u64, select: u8, lyc: u8) -> Self {
            let mut r = Self {
                ppu: Ppu::new(),
                mem: Box::new(Memory::new()),
                frame: Box::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]),
                iflags: 0,
            };
            r.ppu.write_stat(0, select, &mut r.iflags, false);
            r.ppu.write_lyc(0, lyc, &mut r.iflags, false);
            r.sync(on);
            r.ppu.write_lcdc(on, 0x91, &mut r.iflags, false);
            r
        }

        fn sync(&mut self, now: u64) -> Events {
            let (p, m, f) = (&mut self.ppu, &mut *self.mem, &mut *self.frame);
            p.sync(now, m, f, &mut self.iflags, false)
        }

        /// Runs every deadline up to `until`: the clocks at which IF bits
        /// were requested, with the bits.
        fn irqs(&mut self, until: u64) -> Vec<(u64, u8)> {
            let mut out = Vec::new();
            loop {
                let at = self.ppu.next_event(false);
                if at > until {
                    break;
                }
                self.iflags = 0;
                self.sync(at);
                if self.iflags != 0 {
                    out.push((at, self.iflags));
                }
            }
            self.sync(until);
            out
        }

        fn stat(&self, now: u64) -> u8 {
            self.ppu.stat(now, false)
        }

        fn ly(&self, now: u64) -> u8 {
            self.ppu.ly(now, false)
        }
    }

    /// The clock of frame position `p` in the first frame of a rig turned
    /// on at `on`, and in the frames after it.
    fn clock(on: u64, frame: u64, p: u32) -> u64 {
        on - u64::from(ON_POS) + frame * u64::from(FRAME) + u64::from(p)
    }

    #[test]
    fn first_line_after_lcd_enable_skips_the_oam_scan() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        let mut modes = Vec::new();
        for c in on..clock(on, 0, LINE) {
            r.sync(c);
            let m = r.stat(c) & 3;
            if modes.last().is_none_or(|&(_, last)| last != m) {
                modes.push((c - on, m));
            }
        }
        // Mode 0 (no OAM scan) until SameBoy's nominal 78 (seen one dot
        // later), then 172 dots of mode 3.
        assert_eq!(modes, [(0, 0), (79, 3), (79 + 172, 0)]);
        // Line 1 starts 448 dots after SameBoy's LCD enable (449 after the
        // write's clock); LY changes 3 dots in.
        let line1 = clock(on, 0, LINE);
        r.sync(line1 + 4);
        assert_eq!((r.ly(line1 + 2), r.ly(line1 + 3)), (0, 1));
        assert_eq!(r.stat(line1 + 4) & 3, 2);
        assert_eq!(line1 - on, 449);
    }

    #[test]
    fn line_timing_of_a_plain_line() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        let l5 = clock(on, 0, 5 * LINE);
        r.sync(l5 + 455);
        let modes: Vec<(u32, u8, u8)> = [0, 2, 3, 4, 83, 84, 255, 256, 455]
            .iter()
            .map(|&t| (t, r.stat(l5 + u64::from(t)) & 3, r.ly(l5 + u64::from(t))))
            .collect();
        assert_eq!(
            modes,
            [
                (0, 0, 4),
                (2, 0, 4),
                (3, 0, 5),
                (4, 2, 5),
                (83, 2, 5),
                (84, 3, 5),
                (255, 3, 5),
                (256, 0, 5),
                (455, 0, 5)
            ]
        );
    }

    #[test]
    fn stat_interrupt_sources_and_blocking() {
        let on = 1000;
        // Mode 2 only: one interrupt per line, 3 dots in (lines 1-143;
        // line 0 comes from VBlank: 4 dots in), plus the line-144 quirk.
        let mut r = Rig::on_at(on, SEL_MODE2, 0xFF);
        let irqs = r.irqs(clock(on, 1, 3 * LINE));
        let first: Vec<u64> = irqs
            .iter()
            .take(3)
            .map(|&(c, _)| c - clock(on, 0, 0))
            .collect();
        assert_eq!(first, [456 + 3, 2 * 456 + 3, 3 * 456 + 3]);
        let vblank = clock(on, 0, VBLANK_POS);
        assert!(irqs.contains(&(vblank - 3, INT_STAT)), "quirk at 144:2");
        // The quirk again at 144:5 (SameBoy), with the VBlank interrupt.
        assert!(irqs.contains(&(vblank, INT_VBLANK | INT_STAT)));
        assert!(
            irqs.contains(&(clock(on, 1, 4), INT_STAT)),
            "line 0 from VBlank"
        );

        // Mode 0 and mode 2: the HBlank source is still high when the
        // mode-2 source rises, so only HBlank interrupts (Mooneye
        // `acceptance/ppu/stat_irq_blocking`; Pan Docs "STAT blocking").
        let mut r = Rig::on_at(on, SEL_MODE0 | SEL_MODE2, 0xFF);
        let l5 = clock(on, 0, 5 * LINE);
        r.sync(l5);
        let irqs = r.irqs(l5 + u64::from(LINE));
        assert_eq!(irqs, [(l5 + 257, INT_STAT)]);

        // Mode 1: once per frame, at VBlank.
        let mut r = Rig::on_at(on, SEL_MODE1, 0xFF);
        let irqs = r.irqs(clock(on, 1, VBLANK_POS) + 1);
        let stat: Vec<u64> = irqs
            .iter()
            .filter(|i| i.1 & INT_STAT != 0)
            .map(|i| i.0)
            .collect();
        assert_eq!(stat, [vblank, clock(on, 1, VBLANK_POS)]);
    }

    #[test]
    fn lyc_coincidence_bit_and_interrupt() {
        let on = 1000;
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        let l5 = clock(on, 0, 5 * LINE);
        let irqs = r.irqs(clock(on, 0, 7 * LINE));
        assert_eq!(irqs, [(l5 + 4, INT_STAT)]);
        // The bit: off while the comparison is off (t = 3), on from t = 4
        // of line 5 to t = 2 of line 6.
        let bit = |c: u64| r.stat(c) & 4 != 0;
        assert_eq!(
            [
                bit(l5 + 3),
                bit(l5 + 4),
                bit(l5 + 456 + 2),
                bit(l5 + 456 + 3)
            ],
            [false, true, true, false]
        );
        // Writing LYC = LY raises it at once (single speed: one dot into
        // the write's M-cycle, here at t = 20)...
        let l6 = l5 + 456;
        let mut r = Rig::on_at(on, SEL_LYC, 0xFF);
        r.sync(l6 + 20);
        r.ppu.write_lyc(l6 + 20, 6, &mut r.iflags, false);
        assert_eq!(r.iflags, INT_STAT);
        // ...but at t = 0-1, while LY changes, the update waits for the
        // PPU's (SameBoy state 35): LYC = the old LY (still compared until
        // t = 3) raises nothing, the line holds through t = 3, and at t = 4
        // LY no longer matches.
        let mut r = Rig::on_at(on, SEL_LYC, 0xFF);
        r.sync(l6);
        r.ppu.write_lyc(l6, 5, &mut r.iflags, false);
        assert_eq!(r.iflags, 0);
        assert_eq!(r.irqs(l6 + 10), []);
        // LYC = the new LY rises at t = 4.
        let mut r = Rig::on_at(on, SEL_LYC, 0xFF);
        r.sync(l6);
        r.ppu.write_lyc(l6, 6, &mut r.iflags, false);
        assert_eq!(r.iflags, 0);
        assert_eq!(r.irqs(l6 + 10), [(l6 + 4, INT_STAT)]);
    }

    #[test]
    fn line_153_reads_ly_0_early() {
        let on = 1000;
        let mut r = Rig::on_at(on, SEL_LYC, 153);
        let l153 = clock(on, 0, 153 * LINE);
        let irqs = r.irqs(l153 + 100);
        assert_eq!(irqs.last(), Some(&(l153 + 6, INT_STAT)));
        let ly: Vec<u8> = [0, 2, 5, 6, 12].iter().map(|&t| r.ly(l153 + t)).collect();
        assert_eq!(ly, [152, 153, 153, 0, 0]);
        // Double speed: 0 from t = 8.
        assert_eq!(
            (r.ppu.ly(l153 + 7, true), r.ppu.ly(l153 + 8, true)),
            (153, 0)
        );
        // LYC = 0 matches from t = 12 of line 153, through line 0.
        let mut r = Rig::on_at(on, SEL_LYC, 0);
        r.irqs(clock(on, 0, 100 * LINE));
        let irqs = r.irqs(clock(on, 1, 2 * LINE));
        let stat = irqs.iter().find(|i| i.1 & INT_STAT != 0);
        assert_eq!(stat, Some(&(l153 + 12, INT_STAT)));
        let stat = r.stat(clock(on, 1, 200));
        assert_eq!(stat & 4, 4);
    }

    #[test]
    fn lcd_off_keeps_the_coincidence_bit_and_on_compares_with_0() {
        // Mooneye `acceptance/ppu/stat_lyc_onoff`, round 1 and 4.
        let on = 1000;
        let mut r = Rig::on_at(on, SEL_LYC, 0x90);
        let l144 = clock(on, 0, 0x90 * LINE + 10);
        r.irqs(l144);
        assert_eq!(r.stat(l144) & 4, 4);
        r.ppu.write_lcdc(l144, 0x11, &mut r.iflags, false);
        r.iflags = 0;
        r.ppu.write_lyc(l144, 1, &mut r.iflags, false);
        assert_eq!(r.stat(l144 + 8), 0xC4, "kept while off");
        r.sync(l144 + 100);
        r.ppu.write_lcdc(l144 + 100, 0x91, &mut r.iflags, false);
        assert_eq!(r.stat(l144 + 104), 0xC0);
        assert_eq!(r.iflags, 0, "the line falls: no interrupt");
        // Off with the bit clear, LYC = 0, on: the line rises.
        r.sync(l144 + 200);
        r.ppu.write_lcdc(l144 + 200, 0x11, &mut r.iflags, false);
        r.ppu.write_lyc(l144 + 200, 0, &mut r.iflags, false);
        r.sync(l144 + 300);
        r.ppu.write_lcdc(l144 + 300, 0x91, &mut r.iflags, false);
        assert_eq!(r.iflags, INT_STAT);
    }

    #[test]
    fn power_on_is_in_vblank_on_the_mcycle_grid() {
        let mut p = Ppu::new();
        p.reset();
        assert_eq!((p.ly(0, false), p.stat(0, false) & 3), (144, 1));
        // Line 144, dot 163: line 145 starts 293 clocks later, and LY
        // reads 145 from its dot 2.
        assert_eq!((p.ly(294, false), p.ly(295, false)), (144, 145));
        // No interrupt source is enabled: the first deadline is the end of
        // the next frame's line 0 mode 3 (70224 - 65827 + 256).
        assert_eq!(p.next_event(false), 4653);
        // Position 3 mod 4 at every M-cycle boundary, as after an enable.
        assert_eq!(POST_BOOT_POS % 4, 3);
    }

    #[test]
    fn vram_and_oam_blocking_windows() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        let l5 = clock(on, 0, 5 * LINE);
        r.sync(l5 + 300);
        let p = &r.ppu;
        let vram = |t: u64| p.vram_blocked(l5 + t, false);
        assert_eq!(
            [vram(83), vram(84), vram(255), vram(256)],
            [false, true, true, false]
        );
        let oam = |t: u64, write| p.oam_blocked(l5 + t, false, write);
        assert_eq!(
            [
                oam(0, true),
                oam(2, false),
                oam(3, false),
                oam(255, false),
                oam(256, true)
            ],
            [true, false, true, true, false]
        );
        // Double speed: one dot later at both ends.
        assert_eq!(
            [
                p.oam_blocked(l5 + 1, true, true),
                p.oam_blocked(l5 + 2, true, true)
            ],
            [false, true]
        );
        assert_eq!(
            [
                p.vram_blocked(l5 + 256, true),
                p.vram_blocked(l5 + 257, true)
            ],
            [true, false]
        );
        // VBlank: free.
        let l150 = clock(on, 0, 150 * LINE + 100);
        r.sync(l150);
        assert!(!r.ppu.vram_blocked(l150, false) && !r.ppu.oam_blocked(l150, false, true));
        // The first line after LCD enable: OAM writes from SameBoy's 76,
        // reads from 78 (mode 3), VRAM from the pipeline start.
        let r = Rig::on_at(on, 0, 0xFF);
        let first = |pos: u32| clock(on, 0, pos);
        let p = &r.ppu;
        assert_eq!(
            [
                p.oam_blocked(first(83), false, true),
                p.oam_blocked(first(84), false, true)
            ],
            [false, true]
        );
        assert_eq!(
            [
                p.oam_blocked(first(85), false, false),
                p.oam_blocked(first(86), false, false)
            ],
            [false, true]
        );
        assert_eq!(
            [
                p.vram_blocked(first(90), false),
                p.vram_blocked(first(91), false)
            ],
            [false, true]
        );
    }

    /// Each visible line is in the frame by VBlank, BG and objects, the
    /// objects from the same OAM scan that sets the mode-3 length; the
    /// first line after LCD enable has no OAM scan, so no objects.
    #[test]
    fn lines_are_drawn_with_the_objects_that_stall_them() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        r.ppu.lcdc |= 0x02; // OBJ on
        let dmg = DmgPalettes {
            bg: [1, 2, 3, 4],
            obj0: [5, 6, 7, 8],
            obj1: [9, 10, 11, 12],
        };
        r.mem.io[0x47] = 0xE4;
        r.mem.io[0x48] = 0xE4;
        r.ppu.resolve_palettes(&mut r.mem, &dmg, false);
        // Tile 0: color 1 everywhere (BG); tile 1: color 3 (the object).
        for row in 0..8 {
            r.mem.vram[2 * row] = 0xFF;
        }
        r.mem.vram[16..32].fill(0xFF);
        // One object on lines 0-7 at screen x 20-27.
        r.mem.oam[..4].copy_from_slice(&[16, 28, 1, 0]);
        r.ppu.oam_written();
        // Line 1: the object at x 20 waits 1 dot for its BG tile, then 6
        // (Pan Docs: OBJ penalty algorithm): mode 3 ends at 263.
        let l1 = clock(on, 0, LINE);
        r.sync(l1 + 262);
        assert_eq!((r.stat(l1 + 262) & 3, r.stat(l1 + 263) & 3), (3, 0));
        let events = r.sync(clock(on, 0, VBLANK_POS));
        assert_eq!(events, Events::FRAME_READY);
        for ly in 0..SCREEN_HEIGHT {
            let row = &r.frame[ly * SCREEN_WIDTH..][..SCREEN_WIDTH];
            for (x, &p) in row.iter().enumerate() {
                let obj = (1..8).contains(&ly) && (20..28).contains(&x);
                assert_eq!(p, if obj { 8 } else { 2 }, "({x}, {ly})");
            }
        }
    }

    #[test]
    fn drawing_whenever_a_mode3_pixel_is_due() {
        // Synced to t, `drawing` holds wherever `mode3_pixel(t)` is Some
        // (the BCPD write skips the flush otherwise), quiet or not, with
        // object stalls on line 5, over two frames.
        for select in [0, 0x08] {
            let on = 1000;
            let mut r = Rig::on_at(on, select, 0xFF);
            r.mem.oam[0] = 16 + 5;
            r.mem.oam[1] = 8;
            let mut due = 0;
            for t in on..on + 2 * u64::from(FRAME) {
                r.sync(t);
                if r.ppu.mode3_pixel(t).is_some() {
                    due += 1;
                    assert!(r.ppu.drawing(), "select {select:#x}, clock {t}");
                }
            }
            assert!(due > 2 * 144 * 160, "{due}");
        }
    }

    #[test]
    fn mode3_pixel_follows_the_stalls() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        // One object at X = 8 (screen x 0) on line 5: 11 dots at pixel 0.
        r.mem.oam[0] = 16 + 5;
        r.mem.oam[1] = 8;
        let l5 = clock(on, 0, 5 * LINE);
        r.sync(l5 + 89);
        let px = |t: u64| r.ppu.mode3_pixel(l5 + t);
        assert_eq!(px(83), None);
        assert_eq!(px(84), Some((5, 0)));
        // First pixel at 96 + the 11-dot stall: 107.
        assert_eq!(
            (px(106), px(107), px(108)),
            (Some((5, 0)), Some((5, 0)), Some((5, 1)))
        );
        assert_eq!(px(266), Some((5, 159)));
        assert_eq!(px(267), None, "mode 3 ends at 256 + 11");
        assert_eq!(r.stat(l5 + 267) & 3, 0);
    }

    #[test]
    fn object_stalls_come_out_in_x_order() {
        let mut out = [(0, 0); 10];
        let n = object_stalls(
            &[88, 20, 168, 8, 60, 20, 100, 44, 30, 12],
            0,
            None,
            &mut out,
        );
        let px: Vec<u8> = out[..n].iter().map(|&(p, _)| p).collect();
        assert_eq!(px, [0, 4, 12, 12, 22, 36, 52, 80, 92]);
    }

    /// Every case of Mooneye `acceptance/ppu/intr_2_mode0_timing_sprites`:
    /// (extra M-cycles of mode 3, object X coordinates). The test's reads
    /// see mode 0 one M-cycle later per 4 dots of penalty.
    #[test]
    fn object_penalties_match_mooneye_sprite_timing() {
        let r = |x: u8, n: usize| [x; 10][..n].to_vec();
        let mut cases: Vec<(u32, Vec<u8>)> = Vec::new();
        for (n, extra) in [2, 4, 5, 7, 8, 10, 11, 13, 14, 16].iter().enumerate() {
            cases.push((*extra, r(0, n + 1)));
        }
        for (x, extra) in [
            (1, 16),
            (2, 15),
            (3, 15),
            (4, 15),
            (5, 15),
            (6, 15),
            (7, 15),
            (8, 16),
            (9, 16),
            (10, 15),
            (11, 15),
            (12, 15),
            (13, 15),
            (14, 15),
            (15, 15),
            (16, 16),
            (17, 16),
            (32, 16),
            (33, 16),
            (160, 16),
            (161, 16),
            (162, 15),
            (167, 15),
            (168, 0),
            (169, 0),
        ] {
            cases.push((extra, r(x, 10)));
        }
        for (a, b, extra) in [
            (0, 160, 17),
            (1, 161, 17),
            (2, 162, 16),
            (3, 163, 16),
            (4, 164, 15),
            (5, 165, 15),
            (6, 166, 15),
            (7, 167, 15),
            (64, 160, 17),
            (65, 161, 17),
            (66, 162, 16),
            (67, 163, 16),
            (68, 164, 15),
            (69, 165, 15),
            (70, 166, 15),
            (71, 167, 15),
        ] {
            let mut xs = r(a, 5);
            xs.extend(r(b, 5));
            cases.push((extra, xs));
        }
        for (x, extra) in [
            (0, 2),
            (1, 2),
            (2, 2),
            (3, 2),
            (4, 1),
            (5, 1),
            (6, 1),
            (7, 1),
            (8, 2),
            (9, 2),
            (10, 2),
            (11, 2),
            (12, 1),
            (13, 1),
            (14, 1),
            (15, 1),
            (16, 2),
            (17, 2),
            (160, 2),
            (161, 2),
            (162, 2),
            (163, 2),
            (164, 1),
            (165, 1),
            (166, 1),
            (167, 1),
        ] {
            cases.push((extra, std::vec![x]));
        }
        for (x0, extra) in [
            (0, 5),
            (1, 5),
            (2, 4),
            (3, 4),
            (4, 3),
            (5, 3),
            (6, 3),
            (7, 3),
            (8, 5),
            (9, 5),
            (10, 4),
            (11, 4),
            (12, 3),
            (13, 3),
            (14, 3),
            (15, 3),
            (16, 5),
        ] {
            cases.push((extra, std::vec![x0, x0 + 8]));
        }
        for (x0, extra) in [
            (0, 27),
            (1, 25),
            (2, 22),
            (3, 20),
            (4, 17),
            (5, 15),
            (6, 15),
            (7, 15),
        ] {
            cases.push((extra, (0..10).map(|i| x0 + 8 * i).collect()));
        }
        for (x0, extra) in [(72, 27), (73, 25)] {
            cases.push((extra, (0..10).map(|i| x0 - 8 * i).collect()));
        }
        assert_eq!(cases.len(), 104); // the ROM runs X = 167 twice
        for (extra, xs) in cases {
            assert_eq!(object_penalty(&xs, 0, None) / 4, extra, "{xs:?}");
        }
    }

    #[test]
    fn object_penalty_follows_scx_and_the_window() {
        // The Pixel at screen x 0 with SCX = 3 is pixel 3 of its tile: 4
        // pixels to the right, 2 dots of wait.
        assert_eq!(object_penalty(&[8], 3, None), 8);
        // X = 0 always costs 11 when first in its tile.
        assert_eq!(object_penalty(&[0], 3, None), 11);
        // In the window (WX = 7: window x 0), SCX doesn't matter.
        assert_eq!(object_penalty(&[8], 3, Some(7)), 11);
    }

    // --- OAM DMA as the PPU sees it ---

    /// A Rig with the LCD on at 1000, synced to the start of line 5, and
    /// objects at OAM entries 0 and 20 covering line 5 (OAM and the
    /// pre-transfer copy alike); returns it and that line's clock.
    fn dma_rig() -> (Rig, u64) {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        let l5 = clock(on, 0, 5 * LINE);
        r.sync(l5);
        r.mem.oam[..2].copy_from_slice(&[16 + 5, 30]);
        r.mem.oam[80..82].copy_from_slice(&[16 + 5, 60]);
        r.mem.oam_dma_old = r.mem.oam;
        r.ppu.oam_written();
        r.ppu.bus = [0, 0];
        r.ppu.bus_live = true;
        (r, l5)
    }

    /// A transfer (single speed) the PPU sees running over `(from, to]`.
    fn running(from: u64, to: u64) -> OamDma {
        OamDma {
            start: from + 2,
            end: to.saturating_add(2),
            ppu_from: from,
            ppu_to: to,
            mcycle: 4,
            ppu_live: true,
            ..OamDma::new()
        }
    }

    /// The current line's selected objects as (OAM entry, X).
    fn line_objects(r: &Rig) -> Vec<(u8, u8)> {
        let o = &r.ppu.renderer.objects;
        (0..o.len()).map(|s| (o.get(s).0, o.get(s).2)).collect()
    }

    /// A running transfer keeps the OAM scan off OAM: entries 0-9 (read
    /// at t = 4..22) compare the last Y and X read instead, so entry 0 is
    /// missed; entry 20, read after the transfer, is found. With a last Y
    /// that covers the line, each of those entries becomes an object at
    /// the last X (SameBoy `add_object_from_index`).
    #[test]
    fn running_dma_scans_the_last_read_y_and_x() {
        for (bus, want) in [
            ([0, 0], vec![(20, 60)]),
            ([16 + 5, 77], (0..10).map(|e| (e, 77)).collect()),
        ] {
            let (mut r, l5) = dma_rig();
            r.ppu.dma = running(l5 + 3, l5 + 22);
            r.ppu.bus = bus;
            r.sync(l5 + 89);
            assert_eq!(line_objects(&r), want, "bus {bus:?}");
        }
    }

    /// Halted with bytes left (CGB C): the OAM scan finds nothing, but
    /// reads the word being written (bytes 6-7 here, not written yet).
    #[test]
    fn halted_dma_blocks_the_oam_scan() {
        let (mut r, l5) = dma_rig();
        // Byte 6, about to be written, would put an object on line 5.
        r.mem.oam_dma_old[6..8].copy_from_slice(&[16 + 5, 0xCD]);
        r.ppu.dma = OamDma {
            halt_from: l5 - 50,
            halt_to: NEVER,
            halt_dest: 6,
            ..running(l5 - 100, NEVER)
        };
        r.sync(l5 + 89);
        assert_eq!(line_objects(&r), vec![]);
        assert_eq!(r.ppu.bus, [16 + 5, 0xCD]);
    }

    /// A change of the transfer's state mid-scan (here an FF46 write at
    /// t = 30): the entries read before it keep what they saw, because
    /// `dma_event` scans them first. Entry 0 was read during the old
    /// transfer (missed), entry 12 after it (found, from OAM as it is,
    /// not as the new transfer found it at its start).
    #[test]
    fn dma_event_keeps_the_entries_read_before_it() {
        let (mut r, l5) = dma_rig();
        r.mem.oam[48..50].copy_from_slice(&[16 + 5, 90]);
        r.ppu.dma = running(l5 + 3, l5 + 22);
        r.sync(l5 + 30);
        let (p, m, f) = (&mut r.ppu, &mut *r.mem, &mut *r.frame);
        p.dma_event(l5 + 30, m, f);
        r.ppu.dma = running(l5 + 36, l5 + 676);
        r.sync(l5 + 89);
        assert_eq!(line_objects(&r), vec![(12, 90)]);
    }

    /// A transfer during mode 3: the object fetch reads the word being
    /// written (Pan Docs: OAM DMA bus conflicts; SameBoy `oam_read`), and
    /// leaves its tile index on the mode-2 bus.
    #[test]
    fn object_fetch_reads_the_word_being_written() {
        let (mut r, l5) = dma_rig();
        // Only entry 0 (X = 30, screen x 22) on line 5.
        r.mem.oam[80] = 0;
        r.mem.oam_dma_old[80] = 0;
        // Bytes 10-11 differ between OAM (written) and the old copy.
        r.mem.oam[10..12].copy_from_slice(&[0x05, 0x00]);
        r.mem.oam_dma_old[10..12].copy_from_slice(&[0x07, 0x20]);
        // The object's 6-dot stall (Pan Docs: OBJ penalty algorithm)
        // starts at 96 + 22 = 118, its OAM read is 1 dot in, at 119.
        // Byte i is written at 77 + 4i: by then 11 bytes, so the read
        // gets the word 10-11: byte 10 written, byte 11 not.
        r.ppu.dma = running(l5 + 77, l5 + 400);
        r.sync(l5 + 300);
        assert_eq!(line_objects(&r), vec![(0, 30)]);
        assert_eq!(r.ppu.obj_attr[0], (0x05, 0x20));
        assert_eq!(r.ppu.bus[0], 0x05);
    }

    /// A transfer starting in mode 3 (here at t = 150): the object fetched
    /// before it (entry 0, at t = 119) keeps what it read then; the one
    /// fetched after it (entry 20, at t = 204) reads the word being
    /// written. The line was latched without DMA, so `dma_event` starts
    /// the per-slot attributes from scratch.
    #[test]
    fn dma_event_in_mode_3_fixes_the_fetches_so_far() {
        let (mut r, l5) = dma_rig();
        r.mem.oam[2..4].copy_from_slice(&[0x03, 0x00]);
        r.mem.oam_dma_old[2..4].copy_from_slice(&[0x0A, 0x0B]);
        // Entry 20 at X = 108 (screen x 100): its 7-dot stall starts at
        // 202, its OAM read at 204.
        r.mem.oam[81] = 108;
        r.mem.oam_dma_old[81] = 108;
        r.mem.oam_dma_old[16..18].copy_from_slice(&[0x09, 0x40]);
        // Left over from an earlier line.
        r.ppu.attr_final = 0xFFFF;
        r.ppu.obj_attr = [(0xEE, 0xEE); 10];
        r.sync(l5 + 150);
        let (p, m, f) = (&mut r.ppu, &mut *r.mem, &mut *r.frame);
        p.dma_event(l5 + 150, m, f);
        // Byte i written at 140 + 4i: 16 bytes by 204, the word 16-17.
        r.ppu.dma = running(l5 + 140, l5 + 400);
        r.sync(l5 + 400);
        assert_eq!(line_objects(&r), vec![(0, 30), (20, 108)]);
        assert_eq!(r.ppu.obj_attr[..2], [(0x03, 0x00), (0x09, 0x40)]);
    }
}
