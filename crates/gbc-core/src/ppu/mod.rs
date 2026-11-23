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
//!   mode 0 (line 0: mode 1, except mode 0 at `t = 3` in single speed);
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
use crate::memory::{Held, Memory};
use crate::schedule::NEVER;
use crate::{DmgPalettes, Events, FRAME_CLOCKS};
use objects::LineObjects;
pub(crate) use render::OBJ_BUF_LEN;
use render::{Colors, Regs, Renderer, Sources, pair_table, set_pair_color};

pub(crate) mod edges;
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
pub(crate) const VBLANK_POS: u32 = 144 * LINE + edges::VBLANK;

/// Frame position at clock 0 after reset, where the CGB boot ROM hands
/// over: LY = 144, 171 dots in.
///
/// The phase against the M-cycle grid is exact: the boot ROM enabled the
/// LCD with a single-speed write at an M-cycle boundary, so every M-cycle
/// boundary since sits at a position of `ON_POS` mod 4 (3), as after any
/// LCD enable (an off-grid value such as 164 fails hundreds of ROMs).
/// On the grid, Gambatte `display_startstate/stat{,_scx2,_scx3,_scx5}_1`
/// and `_2` (CGB-mode cartridges, results from a CGB04C) pin it: their
/// STAT reads one M-cycle apart must see mode 3 (out87) and mode 0
/// (out84). That holds at 171 only; 167 and below read mode 3 in `_2`,
/// 175 and above mode 0 in `_1`. Mooneye (DMG-mode cartridges:
/// `intr_2_*`, `boot_*`, `stat_irq_blocking`) passes at 155-171 and does
/// not tell them apart; the DMG-mode boot ROM path could end at another
/// position, but no test we have measures it (ADR 0022: the post-boot
/// DIV does differ by mode). Gambatte's own post-boot state,
/// `videoCycles = 144*456 + 164`, is in its line origin, not ours.
pub(crate) const POST_BOOT_POS: u32 = 144 * LINE + 171;

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

/// How a CPU write sees the STAT interrupt sources (design doc
/// `docs/design/lcd-offset-phase.md` 3.2). Single speed, lines 1-152, the
/// walk compares the new LY and ends the mode-2 source at
/// [`edges::CMP_WALK_SS`], one dot after the STAT bit
/// ([`edges::CMP_ON`]); the writes see that dot their own way:
/// - `StatPhase1`, the first phase of a STAT write (at the access): the
///   mode-2 source still live, as gambatte's `statChangeTriggersM2IrqCgb`
///   window (behaviour only, GPL) and `m2enable/late_enable_lcdoffset3_1`
///   (CGB04C) need;
/// - `StatPhase2` and `LycWrite` (one T-cycle in): the mode-2 source
///   gone and the comparison done, as before the walk's point moved:
///   gambatte never lets the mode-2 source block a LYC-write trigger
///   (`lycRegChangeStatTriggerBlockedByM0OrM1Irq`), and the default-phase
///   ROMs `m2_late_m0disable_2`, `lyc1_m2irq_late_lyc255_3` and
///   `lyc1_m2irq_late_lycdisable_2` pin it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum View {
    StatPhase1,
    StatPhase2,
    LycWrite,
}

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
    /// Lines 0-143: comparison against LY and the end of the mode-2
    /// source; line 0 at `t = 4` (with its mode-2 source), lines 1-143 at
    /// [`edges::cmp_walk`] (single speed `t = 5`).
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
    /// Lines 144-152, [`edges::cmp_walk`] (single speed `t = 5`):
    /// comparison against LY. On line 144 in single speed at the same
    /// position as `VBlank`, and before it.
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
    /// Where the walk compares the new LY (and ends the mode-2 source) on
    /// lines 0-152: [`edges::cmp_walk`] for the current CPU speed
    /// ([`Ppu::speed_changed`]). Carried by every cursor so the walk needs
    /// no speed argument (and it fits in the padding).
    cmp: u8,
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

/// Dots after a tile's first pixel is output at which the next tile's
/// map entry has been read (GET_TILE T1 ran in the dot before): a
/// map-select or SCX write from then on misses that tile
/// ([`Ppu::hold_fetch`]). SameBoy pops a pixel, then advances the fetcher,
/// in the same dot.
const FETCH_SAMPLED: i32 = 1;

/// Dots after a tile's first pixel is output at which the next tile's low
/// and high bit planes have been read: their addresses (LCDC.4, SCY) are
/// computed 2 and 4 dots after the map entry's (SameBoy
/// `advance_fetcher_state_machine`: 2-dot GET_TILE, DATA_LOW and
/// DATA_HIGH steps, registers read at each step's first dot). Calibrated
/// with SCY writes (gambatte `scy_during_m3`, Mealybug `m3_scy_change2`).
const FETCH_LOW: i32 = FETCH_SAMPLED + 2;
const FETCH_HIGH: i32 = FETCH_SAMPLED + 4;

/// Dots by which an LCDC write's tile-data select (LCDC.4) reaches the
/// DATA_LOW/DATA_HIGH steps after an SCY write at the same dot would. CGB
/// LCDC writes land in phases (SameBoy `GB_CONFLICT_LCDC_CGB`: the
/// TILE_SEL change takes effect one dot after the access dot). Calibrated
/// over the full manifest (vs 0 extra dots): 0 -> 30 flips, 1 -> 45 (15
/// gambatte `bgtiledata_spx*` more), 2 -> 39; no regressions.
const LCDC_TILES_LATE: i32 = 1;

/// Dots between the fine-scroll compare of discard pixel `k` and that
/// pixel's slot in the output (`mode3 + 12 + k`): SameBoy compares at
/// `position_in_line` -16..-9 and then pops the 8 positions -8..-1 before
/// x = 0 ([`Ppu::hold_fetch`]). Calibrated over the full manifest (live
/// compare at `k` = write dot `- mode3 - 12 + DISCARD_LEAD`): 0-6 regress
/// (7-32 ROMs), 7 -> 17 flips / 4 regressions, 8 -> 21 / 0, 9 -> 9 / 0,
/// 10-12 -> 4-1 / 0.
const DISCARD_LEAD: i32 = 8;

/// Dots before the window's stall would begin at which its start is
/// decided: a WX, WY or LCDC.5 write after that doesn't move it
/// ([`Ppu::window_write`]). Approximations, calibrated over the full
/// manifest (`docs/design/mid-line-ppu.md`, step E1); no reference pins
/// the values:
/// - SameBoy (display.c, mode-3 loop, MIT) compares the live WX with
///   `position_in_line + 7` each dot while LCDC.5 and `wy_triggered`
///   hold, and `wx_triggered`/`window_y++` fix the start at that dot. So a
///   write takes effect at a dot, not with a lead, which fits
///   `WINDOW_LEAD` = 0. Its compare is the same for a window that appears
///   or goes; the extra dot of `WINDOW_ON_LEAD` for a new start is
///   calibrated (CGB single speed, sm83_cpu.c: a WX write lands one dot
///   after an LCDC write, `GB_CONFLICT_WRITE_CPU` vs `GB_CONFLICT_LCDC_CGB`,
///   so it fits WX at most, not LCDC.5).
/// - A WY write is checked later (SameBoy `wy_check_scheduled`): on CGB
///   single speed at the next dot where `wy_check_modulo & 7 == 0`, 1-8
///   dots after the write on an 8-dot grid aligned to the LCD-on clock,
///   then the window compare skips a dot (`wy_just_checked`). `WY_LEAD` = 4
///   (measured on the line clock, without the fine-scroll discard) stands
///   for that phase-dependent delay; 3-5 give the same manifest results.
/// - Pan Docs (Window) and GBCTR give no write-to-effect timing.
const WINDOW_LEAD: i32 = 0;
const WINDOW_ON_LEAD: i32 = 1;
const WY_LEAD: i32 = 4;
/// Stall dots kept by an LCDC.5 clear during the window stall: those
/// before the write's clock plus this (calibrated, step E2).
const WINDOW_OFF_LEAD: i32 = 0;
/// Line dot (as `at` counts it) from which the line's sticky WY == LY
/// check has seen WY ([`Ppu::wy_write`]): line 0, other lines. gambatte
/// (ppu.cpp `weMasterCheck*LineCycle`, behaviour only) samples line 0 at
/// its cycle 2 and lines 1-143 at the previous line's cycle 454, 4 dots
/// earlier relative to the line; SameBoy checks at each line start
/// (`wy_check`). Calibrated (step E2): one value for all lines, 7 -> 6
/// flips / 0 regressions, 3-6 -> 2-4 regressions (line 0 ROMs).
use edges::WY_CHECK_AT;

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
    pub(crate) colors: Colors,
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
                cmp: edges::cmp_walk(false) as u8,
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
    /// at [`POST_BOOT_POS`]. `cgb`: CGB mode (else DMG compatibility mode),
    /// which selects how the renderer reads the BG map attributes and LCDC.0.
    pub(crate) fn reset(&mut self, cgb: bool) {
        *self = Self::new();
        self.renderer.cgb = cgb;
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
            // LY changes 3 dots into every line 1-153. Just before, for 2
            // dots (1 in double speed), it reads LY & (LY + 1), the
            // counter's bits mid-carry. 153 lasts 4 dots (5 in double
            // speed). AGE `lcd-align-ly` (verified on CGB B/C; its
            // "glitch" rows) and `gb_lcd::read_ly` (Apache-2.0, read only);
            // only the CPU/PPU alignments after speed switches show the
            // glitch and the line 144-153 dot.
            1..=152 if t >= edges::LY_NEW => l,
            1..=152 if t + edges::LY_GLITCH - double as u32 >= edges::LY_NEW => (l - 1) & l,
            1..=152 => l - 1,
            _ if t < edges::LY_NEW => 152,
            _ if t < if double {
                edges::LY153_ZERO_DS
            } else {
                edges::LY153_ZERO_SS
            } =>
            {
                153
            }
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
            1..=143 if t < edges::CMP_OFF => (Some((l - 1) as u8), 0),
            1..=143 if t < edges::CMP_ON => (None, (l - 1) as u8),
            1..=143 => (Some(l as u8), 0),
            144..=152 if t < edges::CMP_ON => (None, (l - 1) as u8),
            144..=152 => (Some(l as u8), 0),
            _ if t < edges::L153_CMP => (None, 152),
            _ if t < edges::L153_OFF => (Some(153), 0),
            _ if t < edges::L153_ZERO && !double => (None, 153),
            _ if t < edges::L153_ZERO => (Some(153), 0),
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
            .map_or(LINE, |e| e + edges::MODE0_DS_LATE * u32::from(double))
    }

    /// STAT mode bits at `a` (Pan Docs: LCD Status Registers).
    fn mode(&self, now: u64, a: At, double: bool, quirk: bool) -> u8 {
        let (l, t) = (a.line, a.t);
        if l >= LINES {
            return if l == 144 && t < edges::VBLANK { 0 } else { 1 };
        }
        if a.first {
            return if t < edges::MODE3_FIRST || t >= self.mode3_end(now, a, double) {
                0
            } else {
                3
            };
        }
        if t < edges::MODE2 {
            // Line 0 is still mode 1 from VBlank, but on CGB B/C it reads
            // mode 0 for one M-cycle at `t = 3` in single speed. AGE
            // `stat-mode/stat-mode.inc` (`M1E`: "no mode-0 m-cycle at the
            // end of mode-1 on CGB-E", so B/C have one; `stat-mode-
            // dmgC-cgbBC`, `-ncmBC`); Gambatte `ly0/lycint152_ly0stat_2`
            // and `enable_display/frame1_m2stat_count_1` (CGB04C). SameBoy
            // clears the mode there on DMG only: it models CGB-E here.
            // `quirk` is for STAT register reads only: internal HBlank
            // users (HDMA start) see mode 1 here, as SameBoy does.
            u8::from(l == 0 && (t != edges::LINE0_MODE0_QUIRK || double || !quirk))
        } else if t < edges::MODE3 {
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
        self.stat_with(now, double, true)
    }

    fn stat_with(&self, now: u64, double: bool, quirk: bool) -> u8 {
        let v = 0x80 | self.stat_select;
        if !self.lcd_enabled() {
            return v | if self.lyc_flag_off { 0x04 } else { 0 };
        }
        let a = self.at(now);
        v | self.mode(now, a, double, quirk) | if self.lyc_bit(a, double) { 0x04 } else { 0 }
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
        let first = if double {
            edges::VRAM_FROM_FIRST_DS
        } else {
            edges::VRAM_FROM_FIRST_SS
        };
        self.cpu_blocked(now, double, [edges::VRAM_FROM, first], 0)
    }

    /// The CPU access windows ([`Self::vram_blocked`],
    /// [`Self::palette_blocked`], [`Self::oam_blocked`]): on a visible line
    /// with the LCD on, from `from[0]` (`from[1]` on the first line after
    /// LCD enable) to `end_extra` dots after the mode-3 end. Out of line:
    /// one copy for the three (code size).
    #[inline(never)]
    fn cpu_blocked(&self, now: u64, double: bool, from: [u32; 2], end_extra: u32) -> bool {
        if !self.lcd_enabled() {
            return false;
        }
        let a = self.at(now);
        a.line < LINES
            && a.t >= from[usize::from(a.first)]
            && a.t < self.mode3_end(now, a, double) + end_extra
    }

    /// CPU access to CGB palette RAM (BCPD/OCPD) is blocked at clock `now`:
    /// reads give FF and writes are dropped while the PPU reads palettes
    /// (Pan Docs: LCD Color Palettes, "inaccessible ... during Mode 3").
    /// SameBoy (`display.c`, `cgb_palettes_blocked`, MIT): from 3 dots
    /// after the VRAM block starts (first line after LCD enable: dot 88,
    /// 3 dots before it) to 4 dots after the mode-3 end, 2 in double speed
    /// (where `mode3_end` is already one dot later). Gambatte
    /// `cgbpal_m3/*` and `enable_display/ly{0,1}_late_cgbp*` (CGB04C)
    /// pass with these edges; DMG compatibility mode never gets here
    /// (BCPD/OCPD are closed there).
    #[inline(never)]
    pub(crate) fn palette_blocked(&self, now: u64, double: bool) -> bool {
        let extra = if double {
            edges::PALETTE_EXTRA_DS
        } else {
            edges::PALETTE_EXTRA_SS
        };
        // Fast path (hi-color raster writes, synth:cgb-raster): a latched
        // line (`mode3` set) is latched past position 89, at or before
        // `now`. So `now` is on it, past the window start, or on the next
        // line before position 87, where `t` is past the window end.
        let t = now.wrapping_sub(self.line.start);
        if t < u64::from(LINE + edges::PALETTE_FROM) && self.line.mode3 != 0 && self.lcd_enabled() {
            return t < u64::from(
                u32::from(self.line.end) + edges::MODE0_DS_LATE * u32::from(double) + extra,
            );
        }
        self.cpu_blocked(
            now,
            double,
            [edges::PALETTE_FROM, edges::PALETTE_FROM_FIRST],
            extra,
        )
    }

    /// CPU access to OAM (FE00-FEFF) is blocked at clock `now`: reads give
    /// FF and writes are dropped during the OAM scan and mode 3 (Pan Docs:
    /// Rendering, "OAM access"). CGB C timing (SameBoy `oam_read_blocked`,
    /// `oam_write_blocked`): writes from the line's first dot (double
    /// speed: its third), reads from its fourth (double speed: its fifth),
    /// both to the mode-3 end; on the first line after LCD enable, writes
    /// from SameBoy's nominal 76 and reads from 78 (no OAM scan).
    pub(crate) fn oam_blocked(&self, now: u64, double: bool, write: bool) -> bool {
        let from = match (write, double) {
            (true, false) => [edges::OAM_WRITE_FROM_SS, edges::OAM_WRITE_FROM_FIRST],
            (true, true) => [edges::OAM_WRITE_FROM_DS, edges::OAM_WRITE_FROM_FIRST],
            (false, false) => [edges::OAM_READ_FROM_SS, edges::OAM_READ_FROM_FIRST],
            (false, true) => [edges::OAM_READ_FROM_DS, edges::OAM_READ_FROM_FIRST],
        };
        self.cpu_blocked(now, double, from, 0)
    }

    /// `mode_for_interrupt` at `a` (SameBoy): the mode source a STAT
    /// update at this position sees, through `view`.
    fn mode_source(&self, now: u64, a: At, double: bool, view: View) -> u8 {
        let (l, t) = (a.line, a.t);
        if l >= LINES {
            return if l == 144 && t < edges::VBLANK { 0 } else { 1 };
        }
        let end = self.mode3_end(now, a, double);
        if a.first {
            return if t >= end { 0 } else { NO_MODE };
        }
        let m2_end = if view == View::StatPhase1 && !double && l != 0 {
            edges::CMP_WALK_SS
        } else {
            edges::MODE2_SOURCE_END
        };
        if t < m2_end {
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
        let cmp = u32::from(self.cursor.cmp);
        let (l, t) = (after / LINE, after % LINE);
        let base = l * LINE;
        if l < LINES {
            let first_line = first && l == 0;
            let pipe = if first_line {
                edges::PIPE_FIRST
            } else {
                edges::PIPE
            };
            if !first_line {
                if t < edges::PT_LY {
                    return (base + edges::PT_LY, Pt::Ly3);
                }
                let cmp = if l == 0 { edges::CMP_ON } else { cmp };
                if t < cmp {
                    return (base + cmp, Pt::Ly4);
                }
            }
            if t < pipe {
                return (base + pipe, Pt::Latch);
            }
            match self.latched_end(start.wrapping_add(u64::from(base))) {
                // The earliest end: 167 dots after the pipeline start.
                None => return (base + pipe + edges::MIN_MODE3, Pt::End),
                Some(e) if t < e => return (base + e, Pt::End),
                Some(e) if t <= e => return (base + e + edges::HBLANK_AFTER_END, Pt::HBlank),
                Some(_) => {}
            }
            return if l < LINES - 1 {
                (base + LINE + edges::PT_LY, Pt::Ly3)
            } else {
                (base + LINE + edges::VCMP_OFF, Pt::V0)
            };
        }
        if l == 144 {
            return match t {
                _ if t < edges::QUIRK2 => (base + edges::QUIRK2, Pt::Quirk2),
                _ if t < cmp => (base + cmp, Pt::V4),
                _ if t < edges::VBLANK => (VBLANK_POS, Pt::VBlank),
                _ => (base + LINE + edges::VCMP_OFF, Pt::V0),
            };
        }
        if l < 153 {
            return if t < cmp {
                (base + cmp, Pt::V4)
            } else {
                (base + LINE + edges::VCMP_OFF, Pt::V0)
            };
        }
        match t {
            _ if t < edges::L153_CMP => (base + edges::L153_CMP, Pt::L153Cmp),
            _ if t < edges::L153_OFF => (base + edges::L153_OFF, Pt::L153Off),
            _ if t < edges::L153_ZERO => (base + edges::L153_ZERO, Pt::L153Zero),
            _ => (FRAME + edges::PT_LY, Pt::Ly3),
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
            let pipe = if first && l == 0 {
                edges::PIPE_FIRST
            } else {
                edges::PIPE
            };
            if t < pipe {
                return (base + pipe, Pt::Latch);
            }
            match self.latched_end(start.wrapping_add(u64::from(base))) {
                None => return (base + pipe + edges::MIN_MODE3, Pt::End),
                Some(e) if t < e => return (base + e, Pt::End),
                Some(_) => {}
            }
            return if l < LINES - 1 {
                (base + LINE + edges::PIPE, Pt::Latch)
            } else {
                (VBLANK_POS, Pt::VBlank)
            };
        }
        if after < VBLANK_POS {
            return (VBLANK_POS, Pt::VBlank);
        }
        (FRAME + edges::PIPE, Pt::Latch)
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
        Cursor {
            pos,
            line,
            pt,
            cmp: self.cursor.cmp,
        }
    }

    /// Re-seeks the cursor after a change to what the walk visits (LCD
    /// enable, reset, the STAT selects becoming empty or not).
    #[cold]
    fn seek(&mut self) {
        self.cursor = self.seek_point();
    }

    /// The CPU speed changed: the walk's comparison points move
    /// ([`edges::cmp_walk`]). A pending comparison point moves with it; if
    /// it is then at or before the last sync (where every point up to it
    /// has run), it runs now and the walk continues after it, so no
    /// comparison is lost.
    #[cold]
    pub(crate) fn speed_changed(&mut self, iflags: &mut u8, double: bool) {
        let c = self.cursor;
        let cmp = edges::cmp_walk(double) as u8;
        self.cursor.cmp = cmp;
        if !self.lcd_enabled() || !matches!(c.pt, Pt::Ly4 | Pt::V4) {
            return;
        }
        self.cursor.pos = c.pos + u32::from(cmp) - u32::from(c.cmp);
        if self.frame_start.wrapping_add(u64::from(self.cursor.pos)) <= self.synced {
            let mut line = self.line_irq;
            if self.point_irq(&mut line, c.pt, u32::from(c.line), double) {
                *iflags |= INT_STAT;
            }
            self.line_irq = line;
            self.seek();
        }
    }

    /// The update point after `c` (which is at or before the next frame's
    /// first point; `start` is the clock of `c`'s frame position 0): what
    /// [`Self::next_point`] (or [`Self::next_work_point`] if `quiet`)
    /// returns for `c.pos`, without dividing. A visible line's `End` is at
    /// its latched end, or at the earliest possible end if not latched yet.
    #[inline]
    fn step(&self, c: Cursor, quiet: bool, start: u64, cmp: u32) -> Cursor {
        let l = u32::from(c.line);
        let base = l * LINE;
        let (pos, line, pt) = match c.pt {
            // Line 0 compares at `t = 4` in both speeds (design step 5).
            Pt::Ly3 => (base + if l == 0 { edges::CMP_ON } else { cmp }, l, Pt::Ly4),
            Pt::Ly4 => (base + edges::PIPE, l, Pt::Latch),
            Pt::Latch => {
                let end = match self.latched_end(start.wrapping_add(u64::from(base))) {
                    Some(e) => base + e,
                    // The earliest end: 167 dots after the pipeline start.
                    None => c.pos + edges::MIN_MODE3,
                };
                (end, l, Pt::End)
            }
            Pt::End if !quiet => (c.pos + edges::HBLANK_AFTER_END, l, Pt::HBlank),
            Pt::End | Pt::HBlank if l < LINES - 1 => {
                if quiet {
                    (base + LINE + edges::PIPE, l + 1, Pt::Latch)
                } else {
                    (base + LINE + edges::PT_LY, l + 1, Pt::Ly3)
                }
            }
            Pt::End => (VBLANK_POS, LINES, Pt::VBlank),
            Pt::HBlank => (base + LINE + edges::VCMP_OFF, LINES, Pt::V0),
            Pt::V0 => match l {
                144 => (c.pos + (edges::QUIRK2 - edges::VCMP_OFF), l, Pt::Quirk2),
                153 => (c.pos + (edges::L153_CMP - edges::VCMP_OFF), l, Pt::L153Cmp),
                _ => (base + cmp, l, Pt::V4),
            },
            Pt::Quirk2 => (base + cmp, l, Pt::V4),
            Pt::V4 if l == LINES => (VBLANK_POS, l, Pt::VBlank),
            Pt::V4 => (base + LINE + edges::VCMP_OFF, l + 1, Pt::V0),
            Pt::VBlank if quiet => (FRAME + edges::PIPE, 0, Pt::Latch),
            Pt::VBlank => (base + LINE + edges::VCMP_OFF, l + 1, Pt::V0),
            Pt::L153Cmp => (c.pos + (edges::L153_OFF - edges::L153_CMP), l, Pt::L153Off),
            Pt::L153Off => (
                c.pos + (edges::L153_ZERO - edges::L153_OFF),
                l,
                Pt::L153Zero,
            ),
            Pt::L153Zero => (FRAME + edges::PT_LY, 0, Pt::Ly3),
        };
        Cursor {
            pos,
            line: line as u8,
            pt,
            cmp: c.cmp,
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
            // The mode-2 source ends (line 0: a pulse). Lines 1-143 had it
            // from `Ly3`, and a write since kept the line up for it
            // ([`Self::update_at`]), so only line 0 can rise for it here.
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
        let cmp = u32::from(c.cmp);
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
            c = self.step(c, quiet, start, cmp);
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
        let cmp = u32::from(self.cursor.cmp);
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
            // After the point's work: `Latch` fixes where `End` is. (`cmp`
            // stays.)
            let n = self.step(c, quiet, self.frame_start, cmp);
            (self.cursor.pos, self.cursor.line, self.cursor.pt) = (n.pos, n.line, n.pt);
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
            &mut mem.obj_rank,
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

    /// After [`Ppu::flush`], before a write at clock `now` of `value` to
    /// LCDC (`reg` 0x40), SCY (0x42) or SCX (0x43): if it changes the map
    /// select (LCDC.3, LCDC.6), the tile data select (LCDC.4), SCY or SCX in
    /// mode 3, the tiles the fetcher already has keep the old values
    /// (`ObjRank::hold`). The fetcher reads the map select, SCX and SCY at
    /// each tile's GET_TILE step, and LCDC.4 and SCY again (CGB C) at the
    /// DATA_LOW and DATA_HIGH steps, `FETCH_LOW` and `FETCH_HIGH` dots in
    /// (SameBoy `advance_fetcher_state_machine`, MIT; Pan Docs: Pixel FIFO;
    /// Mealybug: SCY "read during the B, 0 and 1 stages", TILE_SEL "during
    /// the 0 and 1 stages"). A tile caught between them keeps its map entry
    /// and the planes read before the write (`Held::planes`: the others
    /// with the new values). When pixel `x` is the next out, the tile it
    /// belongs to is in the FIFO, and the next one is fetched from the dot
    /// the first pixel of that tile is output (its map entry is read
    /// `FETCH_SAMPLED` dots later) or completely during an object or window
    /// stall at or after that pixel (SameBoy finishes the BG fetch before
    /// fetching an object). `docs/design/mid-line-ppu.md` step 1.
    #[cold]
    #[inline(never)]
    pub(crate) fn hold_fetch(
        &mut self,
        now: u64,
        mem: &mut Memory,
        reg: usize,
        value: u8,
        deferred: bool,
    ) -> bool {
        let lt = &self.line;
        let a = self.at(now);
        let t = a.t as i32;
        let changed = if reg == 0x40 {
            (value ^ self.lcdc) & 0x58 != 0
        } else {
            value != mem.io[reg]
        };
        // In mode 3 (as `mode3_pixel`; LCD off or a line without one never
        // matches `lt.start`); `flush` drew up to the pixel being output.
        if !changed
            || lt.start != now.wrapping_sub(u64::from(a.t))
            || !(i32::from(lt.mode3)..i32::from(lt.end)).contains(&t)
        {
            return false;
        }
        let mut first_pixel = i32::from(lt.first_pixel);
        // The fine-scroll discard compares its count with the live SCX & 7
        // each dot and wraps past 7 (SameBoy display.c,
        // `render_pixel_if_possible`, MIT: `position_in_line & 7` vs SCX,
        // -9 back to -16; Pan Docs: Pixel FIFO, "SCX % 8 pixels are
        // discarded"). An SCX write seen at compare `w`, at or before the
        // match (`w <= d`, one test here), moves it to the next `n >= w`
        // with `n & 7 == SCX & 7`: the first pixel and mode 3's end move by
        // `n - d` (gambatte `scx_during_m3`, `scx_m3_extend`). `w >= 0`:
        // earlier writes are latched (`latch`, dot `mode3 + 5`).
        if reg == 0x43 && t + DISCARD_LEAD <= first_pixel {
            let start = i32::from(lt.mode3) + 12;
            let w = t - start + DISCARD_LEAD;
            let n = w + ((i32::from(value) - w) & 7);
            let lt = &mut self.line;
            lt.end = (i32::from(lt.end) + start + n - first_pixel) as u16;
            first_pixel = start + n;
            lt.first_pixel = first_pixel as u16;
            self.renderer.set_fine(n as u8);
            // The objects' stalls follow the new tile alignment (`n & 7`).
            if lt.n_stalls != 0 {
                self.set_stalls(n as u8, self.renderer.window_wx());
            }
        }
        let lt = &self.line;
        // The pixel being output: `drawn` after a `flush`; for a deferred
        // flush ([`Ppu::scroll_write`]) from the line's timing.
        let x = i32::from(if deferred {
            lt.pixels_before(a.t).max(self.drawn)
        } else {
            self.drawn
        });
        // Position in the line, the SCX & 7 pixels discarded at the start
        // counted like output pixels: they are popped from 12 dots into
        // mode 3, before the first pixel (after an SCX write above, as
        // many as the new discard count).
        let pos = if t < first_pixel { t - first_pixel } else { x };
        // As drawn up to `x` (no change after a `flush`).
        let (active, start, skip) =
            self.renderer
                .window_after(usize::from(self.drawn), x as usize, self.lcdc);
        let phase =
            (pos + if active {
                i32::from(skip) - i32::from(start)
            } else {
                first_pixel - i32::from(lt.mode3) - 12
            }) & 7;
        // Screen x of the first pixel of the tile being output.
        let tile = pos - phase;
        // An object stalls where the line position reaches X - 8 (SameBoy
        // display.c, `x_for_object_match`; fine-scroll discard counted, so
        // tile 0 starts at -(SCX & 7)). One left of that (X = 0, or X - 8 <
        // -(SCX & 7)) is fetched while the junk pixels before tile 0 are
        // popped, before tile 1's fetch begins: its stall, clamped to pixel
        // 0 here, doesn't finish tile 1 (gambatte `scx_during_m3_spx*_ds`).
        // Object stall `i` is the line's object `i` in fetch order (`i - 1`
        // after the window's).
        let r = &self.renderer;
        let win = usize::from(lt.win_stall);
        // Only looked up where it decides `end` (not for most SCX writes).
        let stalled = || {
            t >= first_pixel
                && lt.stalls[..usize::from(lt.n_stalls)]
                    .iter()
                    .enumerate()
                    .any(|(i, &(sx, _))| {
                        let slot = r.order[(i - usize::from(i > win)) % objects::MAX_PER_LINE];
                        let obj_x = i32::from(r.objects.get(usize::from(slot)).2);
                        (i == win || obj_x >= 8 - i32::from(r.fine()))
                            && (tile..=x).contains(&i32::from(sx))
                    })
        };
        // Tile j + 1 (from `tile + 8`): fetched with the old values, its
        // map entry only (`planes`: the planes still to read), or not yet.
        // An LCDC.4 change reaches the data steps one dot later
        // (`LCDC_TILES_LATE`).
        // Only SCY and LCDC.4 reach the data steps: other writes leave
        // `planes` 0 (no redraw, `Renderer::mix_planes`).
        let late = i32::from(reg == 0x40) * LCDC_TILES_LATE;
        let data = reg == 0x42 || (reg == 0x40 && (value ^ self.lcdc) & 0x10 != 0);
        let (mut end, mut planes) = if phase >= FETCH_HIGH + late || !data || stalled() {
            (
                tile + 16 - 8 * i32::from(phase < FETCH_SAMPLED && !stalled()),
                0,
            )
        } else if phase >= FETCH_SAMPLED {
            (tile + 16, if phase >= FETCH_LOW + late { 2 } else { 3 })
        } else {
            (tile + 8, 0)
        };
        // The window's first tile is fetched after it starts (from WX).
        let wx = self.renderer.window_wx().filter(|_| self.lcdc & 0x20 != 0);
        if let Some(wx) = wx.filter(|_| !active) {
            let s = i32::from(wx.saturating_sub(7));
            if s >= x && s < end {
                // Tile j + 1 cut by the window start: its planes as of
                // its map entry (approximation).
                end = s;
                planes = 0;
            }
        }
        if end <= x {
            return false;
        }
        let seg = Held {
            end: end.min(176) as u8,
            lcdc: self.lcdc,
            scx: mem.io[0x43],
            scy: mem.io[0x42],
            planes,
        };
        let hold = &mut mem.obj_rank.hold;
        let last = hold[2];
        if i32::from(last.end) <= x {
            *hold = [Held::default(), Held::default(), seg];
        } else if seg.end > last.end || (seg.end == last.end && last.planes != 0 && planes == 0) {
            // Same end: the held tile's planes read after the earlier
            // write were read with the values before this one, which the
            // (empty) new part keeps for `Renderer::mix_planes`.
            *hold = [hold[1], last, seg];
        }
        true
    }

    /// An SCY (`reg` 0x42) or SCX (0x43) write of `value` at clock `now`:
    /// [`Ppu::flush`], then [`Ppu::hold_fetch`]. When the line has no held
    /// part yet and the write holds the pixels up to the fetcher's
    /// frontier, the flush is left out: every pixel before the frontier,
    /// drawn or not, is drawn with the held (old) SCX and SCY, and every
    /// other register the picture depends on flushes before it changes
    /// (palette RAM and VRAM are blocked in mode 3, VRAM DMA flushes), so
    /// the pixels before the write come out the same, in one span with the
    /// held part instead of two. Not on a line OAM DMA touches (`flush`
    /// fixes the objects' fetches first).
    #[inline(never)]
    pub(crate) fn scroll_write(
        &mut self,
        now: u64,
        mem: &mut Memory,
        frame: &mut Frame,
        reg: usize,
        value: u8,
    ) {
        if !self.attr_on && mem.obj_rank.hold[2].end == 0 {
            if !self.hold_fetch(now, mem, reg, value, true) {
                self.flush(now, mem, frame);
            }
        } else {
            self.flush(now, mem, frame);
            self.hold_fetch(now, mem, reg, value, false);
        }
    }

    /// A BGP (FF47), OBP0 (FF48) or OBP1 (FF49) write: resolves that
    /// palette's colors now (ADR 0013), after the bus has flushed. In CGB
    /// mode BGP has no effect on colors (Pan Docs: FF47 BGP, "Non-CGB
    /// Mode only"; the BG colors come from palette RAM). Nor do OBP0/OBP1 (the
    /// OBJ colors come from OCPD palette RAM).
    pub(crate) fn write_palette(
        &mut self,
        reg: usize,
        value: u8,
        dmg: &DmgPalettes,
        cgb: bool,
        pairs: &mut [[u32; 16]; 8],
    ) {
        match reg {
            0x47..=0x49 if cgb => {}
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

    /// An OCPD write (CGB mode) to OBJ palette RAM byte `index` (0-63),
    /// after the bus has flushed and stored it: re-resolves the one color
    /// it is part of. The renderer looks object colors up per pixel in
    /// `Colors::obj` (palette number from the object buffer), so there is
    /// no pair table.
    pub(crate) fn write_ocpd(&mut self, index: usize, mem: &Memory) {
        let (pal, c) = ((index >> 3) & 7, (index >> 1) & 3);
        self.colors.obj[pal][c] = render::cgb_color(&mem.obj_pal, index);
    }

    /// The clock at which the current (or last) latched visible line
    /// enters mode 0 (its `End` point: STAT reads mode 0 at single
    /// speed), or `None` with the LCD off or no line latched since it was
    /// turned on. For HDMA's HBlank trigger (ADR 0017).
    pub(crate) fn mode0_start(&self) -> Option<u64> {
        if !self.lcd_enabled() || self.line.start == NEVER {
            return None;
        }
        Some(self.line.start.wrapping_add(u64::from(self.line.end)))
    }

    /// Whether clock `now` is in HDMA's HBlank period of the latched
    /// visible line: from `from` dots after its mode-3 end to more than
    /// `tail` dots before the next line starts (gambatte `isHdmaPeriod`,
    /// read for behaviour). False with the LCD off or no line latched.
    #[cold]
    pub(crate) fn hdma_period(&self, now: u64, from: u64, until: u64) -> bool {
        self.mode0_start()
            .is_some_and(|s| now >= s + from && now < self.line.start + u64::from(LINE) + until)
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
    /// OBP1, in CGB mode the BG and OBJ palettes from palette RAM.
    pub(crate) fn resolve_palettes(&mut self, mem: &mut Memory, dmg: &DmgPalettes, cgb: bool) {
        self.colors = Colors::dmg_compat(dmg, mem.io[0x47], mem.io[0x48], mem.io[0x49]);
        if cgb {
            self.colors.set_cgb(&mem.bg_pal, &mem.obj_pal);
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

    /// Before a WY write at clock `now`: once past the line's WY == LY
    /// check point ([`WY_CHECK_AT`]), the old WY was compared with LY, so a
    /// match then holds even if the write changes WY before mode 3 (here
    /// only `latch` and writes check otherwise).
    #[cold]
    #[inline(never)]
    pub(crate) fn wy_write(&mut self, now: u64, mem: &Memory) {
        if self.lcd_enabled() {
            let a = self.at(now);
            if a.line < LINES && a.t >= WY_CHECK_AT[usize::from(a.line != 0)] {
                self.check_wy(a.line, mem);
            }
        }
    }

    /// The pixel pipeline of line `l` starts: fix its mode-3 timing from
    /// SCX, the window and the objects the OAM scan selected (none on the
    /// first line after LCD enable). Out of line: it runs once per line,
    /// and inlined, its register loads were hoisted into every sync.
    #[inline(never)]
    fn latch(&mut self, l: u32, first: bool, mem: &mut Memory) {
        // SameBoy checks at the line start and on WY/LCDC writes (bus
        // `write_ppu` calls `check_wy` too); here before the window's
        // start is fixed.
        self.check_wy(l, mem);
        let scx = mem.io[0x43];
        let window = self.window_wx(mem);
        let tall = self.lcdc & 0x04 != 0;
        let start = self.synced.wrapping_sub(u64::from(if first {
            edges::PIPE_FIRST
        } else {
            edges::PIPE
        }));
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
        } else if self.scan.line == start {
            // LCDC.2 changed in this line's OAM scan ([`Self::obj_size_write`]).
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
        // Fetch order is ascending X in both modes: sorted once for both
        // the stalls and the renderer (CGB-mode drawing priority by OAM
        // index is resolved when the renderer merges fetched objects).
        let n = objs.len();
        let order = objs.priority_order();
        let mode3 = if first { 86 } else { 84 };
        // 12 dots of tile fetches, then SCX % 8 dots discarding pixels.
        let t = &mut self.line;
        t.start = start;
        t.mode3 = mode3;
        t.first_pixel = mode3 + 12 + u16::from(scx & 7);
        self.drawn = 0;
        mem.obj_rank.hold[2].end = 0;
        mem.obj_rank.next = 0;
        // The pixels follow the window start the stalls are built with.
        self.renderer.start_line(l as u8, scx, &objs, order, window);
        if n != 0 || window.is_some() {
            self.set_stalls(scx, window);
        } else {
            // No stall (most lines): no call.
            let t = &mut self.line;
            t.win_stall = NO_STALL;
            t.n_stalls = 0;
            t.end = t.first_pixel + 160;
        }
        self.attr_on = false;
        if aware {
            self.attr_final = 0;
            self.attr_on = n != 0;
        }
    }

    /// WX if the window starts on the current line: LCDC.5 and the WY
    /// condition hold and WX <= 166 (SameBoy: activates for WX < 166 +
    /// is_cgb).
    fn window_wx(&self, mem: &Memory) -> Option<u8> {
        let wx = mem.io[0x4B];
        (self.lcdc & 0x20 != 0 && self.renderer.wy_triggered() && wx <= 166).then_some(wx)
    }

    /// The pipeline stalls of the current line (`line.stalls`, `win_stall`,
    /// `end` from `first_pixel`): the renderer's objects in fetch order
    /// and the window start at WX `window`.
    #[inline(never)]
    fn set_stalls(&mut self, scx: u8, window: Option<u8>) {
        let r = &self.renderer;
        let n = r.objects.len();
        let mut xs = [0u8; objects::MAX_PER_LINE];
        for (x, &slot) in xs.iter_mut().zip(&r.order[..n]) {
            *x = r.objects.get(usize::from(slot)).2;
        }
        let t = &mut self.line;
        t.win_stall = NO_STALL;
        let mut total = 0u16;
        let mut k = 0;
        if n != 0 || window.is_some() {
            let mut stalls = [(0u8, 0u8); 10];
            let n_obj = sorted_object_stalls(&xs[..n], scx, window, &mut stalls);
            // The window start stalls 6 dots; at WX = 0 with SCX & 7 != 0
            // one more (AGE `stat-mode-window`, verified on CGB B/C/E;
            // SameBoy display.c has the extra dot, `cycles_for_line += 1`,
            // for DMG only).
            let fine = u8::from(scx & 7 != 0);
            let mut win = window.map(|wx| (wx.saturating_sub(7), 6 + fine * u8::from(wx == 0)));
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
        t.end = t.first_pixel + 160 + total;
    }

    /// Clock (line position) at which the window's stall begins, under the
    /// current stalls (`None`: no window stall).
    fn window_stall_at(&self) -> Option<i32> {
        let lt = &self.line;
        let (mut clock, mut x) = (i32::from(lt.first_pixel), 0);
        for (j, &(sx, d)) in lt.stalls[..usize::from(lt.n_stalls) % 12]
            .iter()
            .enumerate()
        {
            clock += (i32::from(sx) - x).max(0);
            x = x.max(i32::from(sx));
            if j == usize::from(lt.win_stall) {
                return Some(clock);
            }
            clock += i32::from(d);
        }
        None
    }

    /// A WX, WY or LCDC.5 write seen by the pixel pipeline at clock `now`
    /// (registers and the WY condition updated): in mode 3, before the
    /// window's start has been decided, the window stall moves, appears or
    /// goes (Pan Docs: Rendering, "Mode 3 length"; SameBoy display.c
    /// compares `position_in_line` with the live WX - 7 each dot while
    /// LCDC.5 and `wy_triggered` hold, and clears `wx_triggered` when a
    /// fetch starts with LCDC.5 clear). The decision is made
    /// `WINDOW_LEAD` dots before the stall would begin. Approximation: once
    /// it has begun, later writes don't move it (SameBoy: `wx_triggered`).
    /// The renderer starts the window and counts its row from the same
    /// decision (`set_stalls`, `Renderer::set_window`), not from the live
    /// registers, so the pixels agree with mode 3's length.
    #[cold]
    #[inline(never)]
    pub(crate) fn window_write(&mut self, now: u64, mem: &Memory, wy: bool) {
        let lt = &self.line;
        let a = self.at(now);
        let t = a.t as i32;
        if lt.start != now.wrapping_sub(u64::from(a.t))
            || !(i32::from(lt.mode3)..i32::from(lt.end)).contains(&t)
        {
            return;
        }
        // WX < 7 activates at position WX - 7, before the first pixel
        // (SameBoy: `position_in_line == WX - 7`; WX = 0 at -7).
        let adj = |wx: u8| i32::from(wx.min(7)) - 7;
        let decided = |c: Option<i32>, lead| c.is_some_and(|c| t + lead > c);
        // The WX the stalls were built with (the old start for a WX write).
        let old = self.renderer.window_wx().unwrap_or(0);
        let at = self.window_stall_at();
        if decided(at, WINDOW_LEAD - adj(old)) {
            // CGB: LCDC.5 clear during the stall ends it; its remaining
            // dots output pixels (gambatte ppu.cpp `StartWindowDraw::inc`,
            // CGB only; `window/late_disable*_1` cgb04c out0, dmg08 out3).
            if let (Some(c), 0) = (at, self.lcdc & 0x20) {
                let lt = &mut self.line;
                let d = &mut lt.stalls[usize::from(lt.win_stall) % 11].1;
                let keep = (t + WINDOW_OFF_LEAD - c).clamp(0, i32::from(*d)) as u8;
                lt.end -= u16::from(*d - keep);
                *d = keep;
            }
            return;
        }
        // The tile alignment the line's stalls were built with (the
        // discard count, `latch` or `hold_fetch`), not the live SCX: an SCX
        // write after the discard doesn't move the fetcher's tile phase.
        let scx = self.renderer.fine();
        let window = self.window_wx(mem);
        self.set_stalls(scx, window);
        self.renderer.set_window(window);
        // The new start is already past: the window waits for the next line.
        let fine = i32::from(self.line.first_pixel) - i32::from(self.line.mode3) - 12;
        let lead = if wy { WY_LEAD + fine } else { WINDOW_ON_LEAD };
        if decided(self.window_stall_at(), lead - adj(mem.io[0x4B])) {
            self.set_stalls(scx, None);
            self.renderer.set_window(None);
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
                        self.scan
                            .objs
                            .consider_cold(e, read(e, 0), read(e, 1), ly, tall);
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
                            self.scan.objs.consider_cold(e, y, x, ly, tall);
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

    /// A CPU write at clock `now` (synced) changes LCDC.2, the object
    /// height; call before [`Self::write_lcdc`]. The OAM scan compares
    /// each entry's Y with the height as of that entry's read (SameBoy
    /// `add_object_from_index` reads LCDC per entry, MIT; Pan Docs: OAM
    /// scan), so in mode 2 the entries read up to `now - 2` are scanned
    /// with the old height here and the latch finishes the scan with the
    /// new one ([`Self::scan_line`]). In single speed the entry read at
    /// `now - 1` matches with either height, i.e. as 8x16 (gambatte
    /// `sprites/late_sizechange*` `sp01`/`sp39`/no-`sp` `_2` (CGB 3) vs
    /// `late_sizechange2_*_1`; behavior only, GPL). The object fetch
    /// reads LCDC.2 at the fetch already (`render.rs`).
    #[cold]
    #[inline(never)]
    pub(crate) fn obj_size_write(&mut self, now: u64, double: bool, mem: &Memory) {
        if !self.lcd_enabled() {
            return;
        }
        let a = self.at(now);
        let start = now.wrapping_sub(u64::from(a.t));
        if a.line < LINES && !a.first && self.line.start != start {
            // The entry read at `now - 1` (single speed) is 8x16 with the
            // old value 8x16, and with the new one otherwise.
            let either = u64::from(!double && self.lcdc & 0x04 != 0);
            self.scan_to(a.line, start, (now + either).saturating_sub(2), mem);
        }
    }

    /// A STAT update at clock `now` (a STAT or LYC write): re-evaluates the
    /// interrupt line with the mode source and comparison of that
    /// position.
    #[inline(never)]
    fn update_at(&mut self, now: u64, iflags: &mut u8, double: bool, view: View) {
        let a = self.at(now);
        let mode = self.mode_source(now, a, double, view);
        let cmp = Self::cmp(a, double).0;
        if self.line_irq.eval(self.stat_select, self.lyc, mode, cmp) {
            *iflags |= INT_STAT;
        }
        // Lines 1-143: the walk's mode-2 source lasts to its comparison
        // point (`Ly4`, [`edges::cmp_walk`]), past where this view sees it
        // end: the line stays up for it, with no interrupt, and `Ly4`
        // ends it.
        if (1..LINES).contains(&a.line)
            && (edges::MODE2_SOURCE_END..u32::from(self.cursor.cmp)).contains(&a.t)
        {
            self.line_irq.stat |= self.stat_select & SEL_MODE2 != 0;
        }
    }

    /// A write landing between the STAT bit's comparison of the new LY
    /// ([`edges::CMP_ON`]) and the walk's ([`edges::CMP_WALK_SS`]; single
    /// speed, lines 1-152): the write's view has seen the comparison with
    /// the old LYC and selects (where the walk compared before design step
    /// 1), so the line takes it before the write changes them.
    ///
    /// The bus has synced the PPU to `now`, so that is exactly when the
    /// walk's next point is that comparison (`Ly4` of lines 1-143, `V4`),
    /// at most `cmp - CMP_ON` dots ahead: no division. While no source is
    /// selected the walk skips it, and the line, low, can't rise.
    #[inline]
    fn early_cmp(&mut self, now: u64, iflags: &mut u8, double: bool, view: View) {
        let c = self.cursor;
        let ahead = self
            .frame_start
            .wrapping_add(u64::from(c.pos))
            .wrapping_sub(now);
        if matches!(c.pt, Pt::Ly4 | Pt::V4)
            && c.line != 0
            && ahead <= u64::from(c.cmp) - u64::from(edges::CMP_ON)
            && self.lcd_enabled()
        {
            self.update_at(now, iflags, double, view);
        }
    }

    /// One phase of a STAT write at clock `now`: the interrupt selects
    /// become `value`'s bits 3-6, then the line is re-evaluated (no STAT
    /// write bug on CGB). The bus writes in two phases (SameBoy
    /// `GB_CONFLICT_STAT_CGB`: the LYC select keeps its old value for the
    /// first dot; double speed: the mode-0 select).
    #[inline(never)]
    pub(crate) fn write_stat(
        &mut self,
        now: u64,
        value: u8,
        iflags: &mut u8,
        double: bool,
        view: View,
    ) {
        self.early_cmp(now, iflags, double, view);
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
        self.update_at(now, iflags, double, view);
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
    #[inline(never)]
    pub(crate) fn write_lyc(&mut self, now: u64, value: u8, iflags: &mut u8, double: bool) {
        self.early_cmp(now, iflags, double, View::LycWrite);
        self.lyc = value;
        if !self.lcd_enabled() {
            return;
        }
        let a = self.at(now);
        let (l, t) = (a.line, a.t);
        let deferred = match l {
            0..=152 => t < edges::LYC_DEFER_UNTIL && !a.first,
            _ => (edges::L153_CMP..edges::L153_ZERO).contains(&t),
        };
        if deferred {
            return;
        }
        // Line 153, dots 2-5 and 12-23 (6-11 were deferred above): first
        // compared with 153; in 12-23 the usual update follows.
        if l == 153
            && (edges::LYC153_OLD_FROM..edges::LYC153_OLD_UNTIL).contains(&t)
            && self.line_irq.eval(self.stat_select, self.lyc, 1, Some(153))
        {
            *iflags |= INT_STAT;
        }
        if !(l == 153 && (edges::LYC153_OLD_FROM..edges::L153_CMP).contains(&t)) {
            self.update_at(now, iflags, double, View::LycWrite);
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
            r.ppu
                .write_stat(0, select, &mut r.iflags, false, View::StatPhase2);
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

    /// The window start costs 6 dots, at WX = 0 with SCX & 7 != 0 one
    /// more (AGE `stat-mode-window`, CGB B/C/E).
    #[test]
    fn window_at_wx_0_with_fine_scroll_costs_one_more_dot() {
        let end = |wx: u8, scx: u8| {
            let on = 1000;
            let mut r = Rig::on_at(on, 0, 0xFF);
            r.ppu.lcdc |= 0x20;
            r.mem.io[0x4A] = 0;
            r.mem.io[0x4B] = wx;
            r.mem.io[0x43] = scx;
            let l5 = clock(on, 0, 5 * LINE);
            r.sync(l5 + 455);
            (84..LINE).find(|&t| r.stat(l5 + u64::from(t)) & 3 == 0)
        };
        assert_eq!(end(1, 0), Some(256 + 6));
        assert_eq!(end(0, 0), Some(256 + 6));
        assert_eq!(end(1, 3), Some(256 + 3 + 6));
        assert_eq!(end(0, 3), Some(256 + 3 + 7));
        assert_eq!(end(0, 7), Some(256 + 7 + 7));
    }

    /// A WX, WY or LCDC.5 write in mode 3 before the window's start moves,
    /// adds or removes its 6-dot stall; after it, mode 3 keeps it.
    #[test]
    fn window_writes_in_mode_3_move_its_stall() {
        // (register, value, write position) -> mode-3 end; window at
        // x = 64 (WX = 0x47), WY = 0 or (for the WY write) 0xFF.
        let end = |w: Option<(usize, u8, u32)>| {
            let on = 1000;
            let mut r = Rig::on_at(on, 0, 0xFF);
            r.ppu.lcdc |= 0x20;
            r.mem.io[0x4A] = if matches!(w, Some((0x4A, ..))) {
                0xFF
            } else {
                0
            };
            r.mem.io[0x4B] = 0x47;
            let l5 = clock(on, 0, 5 * LINE);
            if let Some((reg, v, t)) = w {
                let now = l5 + u64::from(t);
                r.sync(now);
                match reg {
                    0x40 => {
                        r.ppu.lcdc = v;
                        r.ppu.window_write(now, &r.mem, false);
                    }
                    0x4A => {
                        r.mem.io[0x4A] = v;
                        r.ppu.check_wy_at(now, &r.mem);
                        r.ppu.window_write(now, &r.mem, true);
                    }
                    _ => {
                        r.mem.io[0x4B] = v;
                        r.ppu.window_write(now, &r.mem, false);
                    }
                }
            }
            r.sync(l5 + 455);
            (84..LINE).find(|&t| r.stat(l5 + u64::from(t)) & 3 == 0)
        };
        let off = 0x91 & !0x20;
        assert_eq!(end(None), Some(256 + 6));
        assert_eq!(end(Some((0x40, off, 100))), Some(256));
        assert_eq!(end(Some((0x40, off, 200))), Some(256 + 6));
        // CGB: LCDC.5 clear during the stall (dots 160-165) keeps only the
        // stall dots before the write (gambatte `StartWindowDraw::inc`);
        // a WX or WY write there doesn't.
        assert_eq!(end(Some((0x40, off, 160))), Some(256));
        assert_eq!(end(Some((0x40, off, 162))), Some(256 + 2));
        assert_eq!(end(Some((0x40, off, 165))), Some(256 + 5));
        assert_eq!(end(Some((0x40, off, 166))), Some(256 + 6));
        assert_eq!(end(Some((0x4B, 0xFF, 162))), Some(256 + 6));
        assert_eq!(end(Some((0x4A, 5, 100))), Some(256 + 6));
        assert_eq!(end(Some((0x4A, 5, 200))), Some(256));
        assert_eq!(end(Some((0x4B, 0xA0, 100))), Some(256 + 6));
        assert_eq!(end(Some((0x4B, 0xFF, 100))), Some(256));
        assert_eq!(end(Some((0x4B, 0x17, 150))), Some(256));
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

    /// Line 0 after VBlank: mode 1, a single mode-0 dot at `t = 3` in
    /// single speed (CGB B/C), then mode 2. Double speed has no mode-0 dot.
    #[test]
    fn line_0_after_vblank_reads_mode_0_at_dot_3_in_single_speed() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        let l0 = clock(on, 1, 0);
        r.sync(l0 + 8);
        let at = |t: u64, double: bool| r.ppu.stat(l0 + t, double) & 3;
        let single: Vec<u8> = (0..6).map(|t| at(t, false)).collect();
        assert_eq!(single, [1, 1, 1, 0, 2, 2]);
        let double: Vec<u8> = (0..6).map(|t| at(t, true)).collect();
        assert_eq!(double, [1, 1, 1, 1, 2, 2]);
        assert_eq!((r.ly(l0 + 2), r.ly(l0 + 3)), (0, 0));
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
        // Single speed: the interrupt one dot after the bit
        // ([`edges::CMP_WALK_SS`]).
        assert_eq!(irqs, [(l5 + 5, INT_STAT)]);
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
        // LYC = the new LY rises at t = 5 (the bit at t = 4).
        let mut r = Rig::on_at(on, SEL_LYC, 0xFF);
        r.sync(l6);
        r.ppu.write_lyc(l6, 6, &mut r.iflags, false);
        assert_eq!(r.iflags, 0);
        assert_eq!(r.irqs(l6 + 10), [(l6 + 5, INT_STAT)]);
    }

    /// Design step 1 (docs/design/lcd-offset-phase.md 3.2): single speed,
    /// the walk compares at `t = 5`; a write at `t = 4` sees that dot
    /// through its own view.
    #[test]
    fn writes_at_the_dot_before_the_walk_comparison_use_their_view() {
        let on = 1000;
        let l5 = clock(on, 0, 5 * LINE);
        // STAT phase 1 at t = 4 still sees the mode-2 source: enabling it
        // interrupts (gambatte `m2enable/late_enable_lcdoffset3_1`) ...
        let mut r = Rig::on_at(on, 0, 0xFF);
        r.ppu
            .write_stat(0, SEL_MODE0, &mut r.iflags, false, View::StatPhase2);
        r.sync(l5 + 4);
        r.iflags = 0;
        r.ppu
            .write_stat(l5 + 4, SEL_MODE2, &mut r.iflags, false, View::StatPhase1);
        assert_eq!(r.iflags, INT_STAT);
        // ... phase 2 (and a LYC write) don't, and neither does the walk's
        // point at t = 5 after it.
        let mut r = Rig::on_at(on, 0, 0xFF);
        r.ppu
            .write_stat(0, SEL_MODE0, &mut r.iflags, false, View::StatPhase2);
        r.sync(l5 + 4);
        r.iflags = 0;
        r.ppu
            .write_stat(l5 + 4, SEL_MODE2, &mut r.iflags, false, View::StatPhase2);
        assert_eq!(r.iflags, 0);
        assert_eq!(r.irqs(l5 + 10), []);
        // A LYC write at t = 4 comes after the comparison with the old LYC:
        // LYC = 5 on line 5 still interrupts (gambatte `lycEnable/
        // ff45_disable_2`, default phase).
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        r.sync(l5 + 4);
        r.iflags = 0;
        r.ppu.write_lyc(l5 + 4, 0xFF, &mut r.iflags, false);
        assert_eq!(r.iflags, INT_STAT);
        assert_eq!(r.irqs(l5 + 10), []);
        // So does a STAT phase-2 write there that clears the LYC select.
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        r.sync(l5 + 4);
        r.iflags = 0;
        r.ppu
            .write_stat(l5 + 4, 0, &mut r.iflags, false, View::StatPhase2);
        assert_eq!(r.iflags, INT_STAT);
        // Lines 144-152 too: LYC = 150 at t = 4 of line 150.
        let l150 = clock(on, 0, 150 * LINE);
        let mut r = Rig::on_at(on, SEL_LYC, 150);
        r.sync(l150 + 4);
        r.iflags = 0;
        r.ppu.write_lyc(l150 + 4, 0xFF, &mut r.iflags, false);
        assert_eq!(r.iflags, INT_STAT);
    }

    /// Double speed keeps the walk's comparison at `t = 4` (design step 3
    /// moves it); a speed change re-seeks the walk without losing it.
    #[test]
    fn walk_comparison_point_by_speed() {
        let on = 1000;
        let l5 = clock(on, 0, 5 * LINE);
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        r.sync(l5);
        assert_eq!(r.ppu.next_event(false), l5 + 5);
        r.ppu.speed_changed(&mut r.iflags, true);
        assert_eq!(r.ppu.next_event(true), l5 + 4);
        // A switch while the LCD is off still moves the point (the walk
        // seeks with it at the enable).
        let mut off = Ppu::new();
        off.speed_changed(&mut r.iflags, true);
        assert_eq!(u32::from(off.cursor.cmp), edges::CMP_WALK_DS);
        off.speed_changed(&mut r.iflags, false);
        assert_eq!(u32::from(off.cursor.cmp), edges::CMP_WALK_SS);
        // To double speed at t = 4: the comparison (at 5 in single speed,
        // at 4 in double) runs at the switch.
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        r.sync(l5 + 4);
        r.iflags = 0;
        r.ppu.speed_changed(&mut r.iflags, true);
        assert_eq!(r.iflags, INT_STAT);
        assert_eq!(r.ppu.cursor, r.ppu.seek_point());
        // To single speed at t = 3: the comparison moves to t = 5.
        let mut r = Rig::on_at(on, SEL_LYC, 5);
        r.sync(l5 + 3);
        r.ppu.speed_changed(&mut r.iflags, true);
        assert_eq!(r.ppu.next_event(true), l5 + 4);
        r.ppu.speed_changed(&mut r.iflags, false);
        assert_eq!(r.ppu.next_event(false), l5 + 5);
    }

    #[test]
    fn line_153_reads_ly_0_early() {
        let on = 1000;
        let mut r = Rig::on_at(on, SEL_LYC, 153);
        let l153 = clock(on, 0, 153 * LINE);
        let irqs = r.irqs(l153 + 100);
        assert_eq!(irqs.last(), Some(&(l153 + 6, INT_STAT)));
        // 152 & 153 = 152 mid-carry (t = 1, 2), 153 from t = 3, 0 from 7.
        let ly: Vec<u8> = [0, 2, 3, 6, 7, 12]
            .iter()
            .map(|&t| r.ly(l153 + t))
            .collect();
        assert_eq!(ly, [152, 152, 153, 153, 0, 0]);
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

    /// LY reads LY & (LY + 1) for the 2 dots (1 in double speed) before it
    /// changes at dot 3, on every line 1-152 (AGE `lcd-align-ly` glitch
    /// rows, CGB B/C).
    #[test]
    fn ly_reads_mid_carry_before_it_changes() {
        let on = 1000;
        let r = Rig::on_at(on, 0, 0xFF);
        let ly = |l: u32, double: bool| -> Vec<u8> {
            let s = clock(on, 0, l * LINE);
            (0..4).map(|t| r.ppu.ly(s + t, double)).collect()
        };
        assert_eq!(ly(4, false), [3, 0, 0, 4]);
        assert_eq!(ly(4, true), [3, 3, 0, 4]);
        assert_eq!(ly(144, false), [143, 0x80, 0x80, 144]);
        assert_eq!(ly(146, true), [145, 145, 144, 146]);
    }

    #[test]
    fn power_on_is_in_vblank_on_the_mcycle_grid() {
        let mut p = Ppu::new();
        p.reset(false);
        assert_eq!((p.ly(0, false), p.stat(0, false) & 3), (144, 1));
        // Line 144, dot 171: line 145 starts 285 clocks later, and LY
        // reads 145 from its dot 3 (144 & 145 = 144 at dot 2).
        assert_eq!((p.ly(287, false), p.ly(288, false)), (144, 145));
        // No interrupt source is enabled: the first deadline is the end of
        // the next frame's line 0 mode 3 (70224 - 65835 + 256).
        assert_eq!(p.next_event(false), 4645);
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
        // CGB palette RAM: 3 dots after VRAM, to 4 dots after the mode-3
        // end (2 in double speed, from its later end). Asked synced, as
        // the bus does (the fast path needs `now` at or past the sync).
        let mut r2 = Rig::on_at(on, 0, 0xFF);
        let mut pal = |t: u64, double| {
            r2.sync(l5 + t);
            r2.ppu.palette_blocked(l5 + t, double)
        };
        assert_eq!(
            [
                pal(86, false),
                pal(87, false),
                pal(259, false),
                pal(260, false)
            ],
            [false, true, true, false]
        );
        let mut r2 = Rig::on_at(on, 0, 0xFF);
        let mut pal = |t: u64, double| {
            r2.sync(l5 + t);
            r2.ppu.palette_blocked(l5 + t, double)
        };
        assert_eq!(
            [pal(86, true), pal(87, true), pal(258, true), pal(259, true)],
            [false, true, true, false]
        );
        // Synced to the line start (line 4 latched): 86 on the fast path,
        // 87 on the slow one.
        let mut r2 = Rig::on_at(on, 0, 0xFF);
        r2.sync(l5);
        let pal = |t: u64| r2.ppu.palette_blocked(l5 + t, false);
        assert_eq!([pal(86), pal(87)], [false, true]);
        // VBlank: free.
        let l150 = clock(on, 0, 150 * LINE + 100);
        r.sync(l150);
        assert!(!r.ppu.vram_blocked(l150, false) && !r.ppu.oam_blocked(l150, false, true));
        assert!(!r.ppu.palette_blocked(l150, false));
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
        // Palette RAM: from dot 88 in both speeds.
        assert_eq!(
            [
                p.palette_blocked(first(87), false),
                p.palette_blocked(first(88), false),
                p.palette_blocked(first(87), true),
                p.palette_blocked(first(88), true)
            ],
            [false, true, false, true]
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

    /// A WY write after the line's WY == LY check point (`WY_CHECK_AT`)
    /// doesn't undo the old WY's match (gambatte `window/arg/late_wy_1toFF`).
    #[test]
    fn wy_write_after_the_line_check_keeps_the_old_match() {
        // (line, WY = line, then FF written at dot `t`) -> mode-3 end; the
        // window at x = 64 stalls 6 dots.
        let end = |line: u32, t: u32| {
            let on = 1000;
            let mut r = Rig::on_at(on, 0, 0xFF);
            r.ppu.lcdc |= 0x20;
            r.mem.io[0x4A] = line as u8;
            r.mem.io[0x4B] = 0x47;
            // Frame 1: line 0 is not the first line after LCD enable.
            let l = clock(on, 1, line * LINE);
            let now = l + u64::from(t);
            r.sync(now);
            r.ppu.wy_write(now, &r.mem);
            r.mem.io[0x4A] = 0xFF;
            r.ppu.check_wy_at(now, &r.mem);
            r.sync(l + 455);
            (84..LINE).find(|&t| r.stat(l + u64::from(t)) & 3 == 0)
        };
        assert_eq!(end(5, 2), Some(256));
        assert_eq!(end(5, 3), Some(256 + 6));
        assert_eq!(end(5, 40), Some(256 + 6));
        // Line 0 is checked 4 dots later than the others.
        assert_eq!(end(0, 6), Some(256));
        assert_eq!(end(0, 7), Some(256 + 6));
    }

    /// The window's pixels and line counter follow the same start decision
    /// as mode 3's length (`set_stalls`), not the live WY, LCDC.5 and WX
    /// (M8-close review): (a) a WY write too late to start the window this
    /// line, though before the renderer reached WX - 7, draws no window
    /// and doesn't count a window row; (b) a WX or LCDC.5 write during the
    /// window's stall doesn't undo it: the row is counted (SameBoy
    /// `window_y++` at the trigger) and the window starts where decided.
    #[test]
    fn window_pixels_follow_the_timing_decision() {
        // BG (map 9800, tile 0): 1. Window (map 9C00, tile 1) at x = 64
        // (WX 0x47): row 0 and row 6: 4, other rows: 2.
        // `(WY, writes on line 5 at dot)` -> (mode-3 end of line 5, line 5
        // x = 64 and x = 72, line 6 x = 72).
        let run = |wy: u8, writes: &[(u32, usize, u8)]| {
            let on = 1000;
            let mut r = fetch_rig(on);
            for row in 0..8 {
                if row != 0 && row != 6 {
                    r.mem.vram[17 + 2 * row] = 0;
                }
            }
            r.ppu.lcdc = 0xF1;
            r.mem.io[0x4A] = wy;
            r.mem.io[0x4B] = 0x47;
            let l5 = clock(on, 0, 5 * LINE);
            for &(t, reg, v) in writes {
                let now = l5 + u64::from(t);
                r.sync(now);
                r.ppu.flush(now, &mut r.mem, &mut r.frame);
                if reg == 0x40 {
                    r.ppu.lcdc = v;
                } else {
                    r.mem.io[reg] = v;
                }
                if reg != 0x4B {
                    r.ppu.check_wy_at(now, &r.mem);
                }
                r.ppu.window_write(now, &r.mem, reg == 0x4A);
            }
            r.sync(l5 + 455);
            let end = (84..LINE).find(|&t| r.stat(l5 + u64::from(t)) & 3 == 0);
            r.sync(l5 + 3 * u64::from(LINE));
            let px = |ly: usize, x: usize| r.frame[ly * SCREEN_WIDTH + x];
            (end, px(5, 64), px(5, 72), px(6, 72))
        };
        // No write: window rows 5 and 6.
        assert_eq!(run(0, &[]), (Some(256 + 6), 2, 2, 4));
        // (a) WY = 5 written 2 dots before x = 64's stall (too late, within
        // WY_LEAD): no window on line 5; line 6 draws row 0.
        assert_eq!(run(0xFF, &[]), (Some(256), 1, 1, 1));
        assert_eq!(run(0xFF, &[(158, 0x4A, 5)]), (Some(256), 1, 1, 4));
        // (b) During the stall (from dot 96 + 64 = 160): LCDC.5 cleared (and
        // set again in mode 0): the BG on line 5, row 5 counted, so line 6
        // draws row 6, and the stall keeps its 2 dots before the write
        // (CGB, step E2); WX moved to x = 72: the window still starts at 64
        // (line 6: at 72, row 6).
        let off = 0xF1 & !0x20;
        assert_eq!(
            run(0, &[(162, 0x40, off), (300, 0x40, 0xF1)]),
            (Some(256 + 2), 1, 1, 4)
        );
        assert_eq!(run(0, &[(162, 0x4B, 0x4F)]), (Some(256 + 6), 2, 2, 4));
    }
    /// A rig in DMG compatibility mode with BG color 0 = 1 and color 3 = 4,
    /// tile 0 color 0 and tile 1 color 3 (LCDC.4 = 1), map 9800 all tile 0
    /// and map 9C00 all tile 1.
    fn fetch_rig(on: u64) -> Rig {
        let mut r = Rig::on_at(on, 0, 0xFF);
        let dmg = DmgPalettes {
            bg: [1, 2, 3, 4],
            obj0: [5, 6, 7, 8],
            obj1: [9, 10, 11, 12],
        };
        r.mem.io[0x47] = 0xE4;
        r.ppu.resolve_palettes(&mut r.mem, &dmg, false);
        r.mem.vram[16..32].fill(0xFF);
        r.mem.vram[0x1C00..0x2000].fill(1);
        r
    }

    /// A mid-line LCDC or SCX write at clock `now`, as `Ctx::write_ppu`
    /// does it.
    fn write_fetch_reg(r: &mut Rig, now: u64, reg: usize, value: u8) {
        r.sync(now);
        r.ppu.flush(now, &mut r.mem, &mut r.frame);
        r.ppu.hold_fetch(now, &mut r.mem, reg, value, false);
        if reg == 0x40 {
            r.ppu.write_lcdc(now, value, &mut r.iflags, false);
        } else {
            r.mem.io[reg] = value;
        }
    }

    /// The first pixel of line `ly` (from 0) with color `c`, after `from`.
    fn first_of(r: &Rig, ly: usize, from: usize, c: u16) -> Option<usize> {
        let row = &r.frame[ly * SCREEN_WIDTH..][..SCREEN_WIDTH];
        (from..SCREEN_WIDTH).find(|&x| row[x] == c)
    }

    /// An SCX write inside the fine-scroll discard moves it: the compare
    /// for discard pixel `k` runs at `mode3 + 12 + k - DISCARD_LEAD` and
    /// wraps past 7 (SameBoy), moving mode 3's end and the image.
    #[test]
    fn scx_write_inside_the_discard_moves_it() {
        // Line 5 starts with SCX 3; `(write dot, new SCX)` -> (mode 3's
        // end, first pixel of tile 1, map 9800 alternating tiles 0 and 1).
        let run = |write: Option<(u32, u8)>| {
            let on = 1000;
            let mut r = fetch_rig(on);
            for (i, b) in r.mem.vram[0x1800..0x1C00].iter_mut().enumerate() {
                *b = (i & 1) as u8;
            }
            r.mem.io[0x43] = 3;
            let l5 = clock(on, 0, 5 * LINE);
            if let Some((t, scx)) = write {
                write_fetch_reg(&mut r, l5 + u64::from(t), 0x43, scx);
            }
            r.sync(l5 + 455);
            let end = (84..LINE).find(|&t| r.stat(l5 + u64::from(t)) & 3 == 0);
            (end, first_of(&r, 5, 0, 4))
        };
        // Line-start fine scroll 3: map x 8 (tile 1) at screen x 5.
        assert_eq!(run(None), (Some(256 + 3), Some(5)));
        // Seen at compare 1 (dot 89): SCX & 7 = 2 matches at 2; 0 only
        // after the wrap, at 8: the first tile wholly discarded, the image
        // as with fine scroll 0 (`bg_column` is the same for n and n + 8).
        assert_eq!(run(Some((89, 2))), (Some(256 + 2), Some(6)));
        assert_eq!(run(Some((89, 0))), (Some(256 + 8), Some(8)));
        // Seen at compare 3 (dot 91), before its match: wraps to 8. From
        // dot 92 compare 3 has matched the line-start 3: no change.
        assert_eq!(run(Some((91, 0))).0, Some(256 + 8));
        assert_eq!(run(Some((92, 0))).0, Some(256 + 3));
    }

    #[test]
    fn map_select_write_misses_the_tiles_already_fetched() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // An object at screen x 24 on line 3 (stall at pixel 24; OBJ off,
        // tile 0: no object pixel).
        r.mem.oam[..4].copy_from_slice(&[16 + 3, 32, 0, 0]);
        r.ppu.oam_written();
        // SCX = 0: the first pixel is out at line position 96, pixel x at
        // 96 + x. Line 1: write at pixel 20, 4 dots into tile 2: tile 3's
        // map entry was read too, the new map shows from pixel 32. Line 2:
        // at pixel 24, the dot tile 3's first pixel is output: tile 4 is
        // not fetched yet, the new map from pixel 32. Line 3: during the
        // stall at pixel 24: tile 4 is fetched in the stall, from 40.
        for (ly, t, want) in [(1u32, 96u64 + 20, 32), (2, 96 + 24, 32)] {
            let l = clock(on, 0, ly * LINE);
            write_fetch_reg(&mut r, l + t, 0x40, 0x99);
            write_fetch_reg(&mut r, l + 300, 0x40, 0x91);
            r.sync(l + u64::from(LINE));
            assert_eq!(first_of(&r, ly as usize, 0, 4), Some(want), "line {ly}");
        }
        let l3 = clock(on, 0, 3 * LINE);
        let t = (l3 + 96..)
            .find(|&t| {
                r.sync(t);
                r.ppu.mode3_pixel(t) == Some((3, 24))
            })
            .unwrap();
        assert_eq!(r.ppu.mode3_pixel(t + 3), Some((3, 24)), "in the stall");
        write_fetch_reg(&mut r, t + 3, 0x40, 0x99);
        r.sync(l3 + u64::from(LINE));
        assert_eq!(first_of(&r, 3, 0, 4), Some(40));
    }

    #[test]
    fn scx_write_misses_the_tiles_already_fetched() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // Map 9800 columns 3 and 5: tile 1.
        r.mem.vram[0x1800 + 3] = 1;
        r.mem.vram[0x1800 + 5] = 1;
        // Line 1, pixel 20: SCX 0 -> 8. Tiles 2 and 3 (columns 2, 3) were
        // fetched with SCX 0; from pixel 32 the map column is 8 + x.
        let l = clock(on, 0, LINE);
        write_fetch_reg(&mut r, l + 96 + 20, 0x43, 8);
        r.sync(l + 300);
        r.mem.io[0x43] = 0;
        r.sync(l + u64::from(LINE));
        assert_eq!(first_of(&r, 1, 0, 4), Some(24));
        assert_eq!(first_of(&r, 1, 24, 1), Some(40));
    }

    #[test]
    fn object_left_of_tile_0_stalls_before_tile_1_is_fetched() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // Map 9800 column 2: tile 1 (color 4); map row 2 column 1: tile 2,
        // row 4 color 1, row 5 color 2.
        r.mem.vram[0x1800 + 2] = 1;
        r.mem.vram[0x1800 + 64 + 1] = 2;
        r.mem.vram[32 + 8..32 + 12].copy_from_slice(&[0xFF, 0, 0, 0xFF]);
        // Line 1: an object at X = 2 (fetched with the junk pixels, 9-dot
        // stall); line 10: at X = 8 (pixel 0, 11 dots); line 20, SCX 3: at
        // X = 6 (in tile 0, which starts at -3; 10 dots). All stall at
        // pixel 0 here, from the first pixel (OBJ off: no object pixel).
        let oam = [16 + 1, 2, 0, 0, 16 + 10, 8, 0, 0, 16 + 20, 6, 0, 0];
        r.mem.oam[..12].copy_from_slice(&oam);
        r.ppu.oam_written();
        // SCX 0 -> 8, 4 dots into the stall. Line 1: tile 1 is fetched
        // after it (column 2 at pixels 8-15). Line 10: tile 1 was fetched
        // in the stall with SCX 0 (column 1), then column x / 8 + 1.
        for (ly, want) in [(1, Some(8)), (10, None)] {
            let l = clock(on, 0, ly * LINE);
            write_fetch_reg(&mut r, l + 96 + 4, 0x43, 8);
            r.sync(l + 300);
            r.mem.io[0x43] = 0;
            r.sync(l + u64::from(LINE));
            assert_eq!(first_of(&r, ly as usize, 0, 4), want, "line {ly}");
        }
        // Line 20: SCY 0 -> 1 in the stall; tile 1 (column 1, pixels 5-12)
        // was fetched in it, both planes from row 4 (color 1).
        r.mem.io[0x43] = 3;
        let l = clock(on, 0, 20 * LINE);
        write_fetch_reg(&mut r, l + 99 + 4, 0x42, 1);
        r.sync(l + 300);
        r.mem.io[0x42] = 0;
        r.sync(l + u64::from(LINE));
        assert_eq!(r.frame[20 * SCREEN_WIDTH..][5..13], [2; 8]);
    }

    #[test]
    fn second_scy_write_in_a_tile_keeps_the_planes_read_between() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // Tile 0 row 1: color 1 (low plane); row 2: color 2 (high plane);
        // row 3: color 0.
        r.mem.vram[2..4].copy_from_slice(&[0xFF, 0]);
        r.mem.vram[4..6].copy_from_slice(&[0, 0xFF]);
        // Line 1: SCY 0 -> 1 between tile 3's plane reads, then 1 -> 2
        // after both: tile 3's high plane was read with 1 (row 2), not 2.
        let l = clock(on, 0, LINE);
        write_fetch_reg(&mut r, l + 96 + 16 + FETCH_LOW as u64, 0x42, 1);
        write_fetch_reg(&mut r, l + 96 + 16 + FETCH_HIGH as u64, 0x42, 2);
        // Then 2 -> 0 between tile 4's plane reads (pixel 27): tile 3's
        // part is still to draw.
        write_fetch_reg(&mut r, l + 96 + 24 + FETCH_LOW as u64, 0x42, 0);
        r.sync(l + u64::from(LINE));
        let row = &r.frame[SCREEN_WIDTH..][..SCREEN_WIDTH];
        assert_eq!((row[23], row[24], row[31], row[32]), (2, 4, 4, 1));
    }

    #[test]
    fn object_stalls_follow_the_discard_not_the_live_scx() {
        // Line 5, an object at X = 8 (pixel 0): 11 dots with fine scroll
        // 0, 8 with 3. `scx`: SCX at the line start; `write`: (dot, SCX);
        // `wx_at`: a WX = 0x77 write (window at x 112, LCDC.5 set, WX
        // 0xFF before) at that dot. -> mode 3's end.
        let end = |scx: u8, write: Option<(u32, u8)>, wx_at: Option<u32>| {
            let on = 1000;
            let mut r = fetch_rig(on);
            r.mem.oam[..4].copy_from_slice(&[16 + 5, 8, 0, 0]);
            r.ppu.oam_written();
            r.ppu.lcdc |= 0x20;
            r.mem.io[0x4B] = 0xFF;
            r.mem.io[0x43] = scx;
            let l5 = clock(on, 0, 5 * LINE);
            if let Some((t, v)) = write {
                write_fetch_reg(&mut r, l5 + u64::from(t), 0x43, v);
            }
            if let Some(t) = wx_at {
                let now = l5 + u64::from(t);
                r.sync(now);
                r.mem.io[0x4B] = 0x77;
                r.ppu.window_write(now, &r.mem, false);
            }
            r.sync(l5 + 455);
            (84..LINE).find(|&t| r.stat(l5 + u64::from(t)) & 3 == 0)
        };
        assert_eq!(end(3, None, None), Some(256 + 3 + 8));
        // Seen in the discard (dot 89): it moves to 8 pixels, and the
        // object's stall follows the new tile alignment.
        assert_eq!(end(3, Some((89, 0)), None), Some(256 + 8 + 11));
        // After the discard, an SCX write doesn't move the alignment: a
        // window write rebuilding the stalls keeps the object's 11 dots.
        assert_eq!(end(0, None, Some(160)), Some(256 + 11 + 6));
        assert_eq!(end(0, Some((150, 3)), Some(160)), Some(256 + 11 + 6));
    }

    /// The pixels of tile 3 (x 24-31) and pixel 32 on line `ly`.
    fn tile3(r: &Rig, ly: usize) -> ([u16; 8], u16) {
        let row = &r.frame[ly * SCREEN_WIDTH..][..SCREEN_WIDTH];
        (row[24..32].try_into().unwrap(), row[32])
    }

    #[test]
    fn tile_data_write_mixes_the_planes_of_the_tile_being_fetched() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // Map 9800 is tile 0: color 0 at 8000 (LCDC.4 = 1), color 3 at
        // 9000 (LCDC.4 = 0). SCX = 0: pixel x is out at line position
        // 96 + x; tile 3's fetch starts as pixel 16 is output.
        r.mem.vram[0x1000..0x1010].fill(0xFF);
        for (ly, p, want) in [
            // Before its map entry is read: tile 3 all new (color 3).
            (1u32, 0u64, [4u16; 8]),
            // Map entry read, no plane: both planes new.
            (2, FETCH_SAMPLED as u64, [4; 8]),
            // The LCDC.4 change reaches the low plane a dot late: none read.
            (3, FETCH_LOW as u64, [4; 8]),
            // Low plane read with 8000 (0), high plane with 9000 (1):
            // color 2.
            (4, (FETCH_LOW + LCDC_TILES_LATE) as u64, [3; 8]),
            // Both planes read: tile 3 old (color 0).
            (5, (FETCH_HIGH + LCDC_TILES_LATE) as u64, [1; 8]),
        ] {
            let l = clock(on, 0, ly * LINE);
            write_fetch_reg(&mut r, l + 96 + 16 + p, 0x40, 0x81);
            write_fetch_reg(&mut r, l + 300, 0x40, 0x91);
            r.sync(l + u64::from(LINE));
            assert_eq!(tile3(&r, ly as usize), (want, 4), "line {ly}");
        }
    }

    #[test]
    fn scy_write_mixes_the_rows_of_the_tile_being_fetched() {
        let on = 1000;
        let mut r = fetch_rig(on);
        // Tile 0 (map 9800) row 1: color 1 (low plane); row 2: color 2
        // (high plane). Line 1 reads row 1 with SCY = 0, row 2 with 1.
        r.mem.vram[2..4].copy_from_slice(&[0xFF, 0]);
        r.mem.vram[4..6].copy_from_slice(&[0, 0xFF]);
        let l = clock(on, 0, LINE);
        write_fetch_reg(&mut r, l + 96 + 16 + FETCH_LOW as u64, 0x42, 1);
        write_fetch_reg(&mut r, l + 300, 0x42, 0);
        r.sync(l + u64::from(LINE));
        // Tiles 0-2 row 1, tile 3 the low plane of row 1 and the high
        // plane of row 2 (color 3), then row 2.
        let row = &r.frame[SCREEN_WIDTH..][..SCREEN_WIDTH];
        assert_eq!((row[23], row[24], row[31], row[32]), (2, 4, 4, 3));
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

    /// CGB mode: objects draw by OAM index with colors from OBJ palette
    /// RAM and tiles from either bank, but are still fetched in X order,
    /// so mode 3 is as long as the X-order stalls say. OBP0/OBP1 writes
    /// change no colors.
    #[test]
    fn cgb_objects_draw_by_oam_index_and_stall_in_x_order() {
        let on = 1000;
        let mut r = Rig::on_at(on, 0, 0xFF);
        r.ppu.renderer.cgb = true;
        r.ppu.lcdc |= 0x02; // OBJ on
        r.mem.bg_pal[..2].copy_from_slice(&0x0042u16.to_le_bytes());
        r.mem.obj_pal[14..16].copy_from_slice(&0x1234u16.to_le_bytes()); // pal 1 color 3
        r.mem.obj_pal[22..24].copy_from_slice(&0x0567u16.to_le_bytes()); // pal 2 color 3
        r.ppu
            .resolve_palettes(&mut r.mem, &DmgPalettes::GRAYSCALE, true);
        let before = r.ppu.colors;
        for reg in [0x48, 0x49] {
            r.ppu.write_palette(
                reg,
                0x1B,
                &DmgPalettes::GRAYSCALE,
                true,
                &mut r.mem.bg_pairs,
            );
        }
        assert_eq!(r.ppu.colors, before);
        // Tile 1: color 3, in both banks.
        r.mem.vram[0x0010..0x0020].fill(0xFF);
        r.mem.vram[0x2010..0x2020].fill(0xFF);
        // OAM order is not X order: entry 0 at x 22-29 (palette 1, bank
        // 1), entry 1 at x 20-27 (palette 2), in the same BG tile, where
        // only the first object fetched waits for the tile.
        r.mem.oam[..8].copy_from_slice(&[16, 30, 1, 0x09, 16, 28, 1, 0x02]);
        r.ppu.oam_written();
        let mut stalls = [(0, 0); 10];
        let n = object_stalls(&[30, 28], 0, None, &mut stalls);
        let end = 256 + stalls[..n].iter().map(|s| u64::from(s.1)).sum::<u64>();
        let l1 = clock(on, 0, LINE);
        r.sync(l1 + end - 1);
        assert_eq!((r.stat(l1 + end - 1) & 3, r.stat(l1 + end) & 3), (3, 0));
        r.sync(clock(on, 0, VBLANK_POS));
        let row = &r.frame[SCREEN_WIDTH..][..SCREEN_WIDTH];
        for (x, &p) in row.iter().enumerate() {
            let want = match x {
                20..22 => 0x0567,
                22..30 => 0x1234,
                _ => 0x0042,
            };
            assert_eq!(p, want, "x {x}");
        }
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

    /// An LCDC.2 write at t = 9 of line 5's OAM scan: entries 1-4 (read at
    /// t = 6, 8, 10, 12) cover line 5 only as 8x16. Entries read up to
    /// t = 7 see the old height, the rest the new one, except that in
    /// single speed entry 2 (read at t = 8) matches with either (gambatte
    /// `sprites/late_sizechange*`).
    #[test]
    fn obj_size_write_splits_the_oam_scan() {
        for (lcdc, double, want) in [
            (0x91, false, vec![2, 3, 4]),
            (0x95, false, vec![1, 2]),
            (0x95, true, vec![1]),
        ] {
            let (mut r, l5) = dma_rig();
            r.mem.oam = [0; 0xA0];
            for e in 1..5 {
                r.mem.oam[e * 4] = 16 + 5 - 10;
            }
            r.ppu.oam_written();
            r.ppu.write_lcdc(l5, lcdc, &mut r.iflags, false);
            r.sync(l5 + 9);
            r.ppu.obj_size_write(l5 + 9, double, &r.mem);
            r.ppu.write_lcdc(l5 + 9, lcdc ^ 0x04, &mut r.iflags, false);
            r.sync(l5 + 89);
            let got: Vec<u8> = line_objects(&r).iter().map(|o| o.0).collect();
            assert_eq!(got, want, "LCDC {lcdc:02X}, double {double}");
        }
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
