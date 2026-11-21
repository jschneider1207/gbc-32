//! Picture processing unit. Skeleton only: timing and rendering arrive in
//! M3. For now a white frame completes at the start of each VBlank, once
//! every [`FRAME_CLOCKS`] base clocks, with the VBlank interrupt; LY and
//! the STAT mode bits follow the nominal line timing (mode 3 fixed at 172
//! dots), enough for software that polls them.
//!
//! The PPU is caught up lazily (ADR 0002): its state is valid as of
//! `synced`, and the bus calls [`Ppu::sync`] before a PPU register write
//! and when the clock reaches [`Ppu::next_event`]. Reads compute LY/STAT
//! for the current clock without mutating anything.

use crate::{Events, FRAME_CLOCKS};

pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;

/// Base clocks per scanline (456 dots; the PPU does not change speed in
/// CGB double-speed mode).
const LINE_CLOCKS: u32 = 456;

/// Base clocks from the start of a frame (LY=0, dot 0) to the start of
/// VBlank (LY=144), when the frame is complete: 144 x 456 = 65,664 (Pan
/// Docs: LCD Status, "STAT modes"). `FRAME_READY` fires here.
const VBLANK_START: u32 = SCREEN_HEIGHT as u32 * LINE_CLOCKS;

const _: () = assert!(VBLANK_START == 65_664 && FRAME_CLOCKS == 154 * LINE_CLOCKS);

/// The VBlank interrupt bit in IF.
pub(crate) const INT_VBLANK: u8 = 0x01;

/// A frame buffer: 160x144 RGB555 pixels (see [`rgb555`](crate::rgb555)),
/// row-major, top-left first. Supplied by the caller on every run call.
/// Swap buffers only after a run call reports
/// [`Events::FRAME_READY`](crate::Events::FRAME_READY); see
/// [`Emulator::run_until`](crate::Emulator::run_until) for why, and for
/// what happens when the LCD is turned off mid-frame.
pub type Frame = [u16; SCREEN_WIDTH * SCREEN_HEIGHT];

/// RGB555 white, the color of a blank (LCD off) screen.
pub const WHITE: u16 = 0x7FFF;

pub(crate) struct Ppu {
    pub(crate) lcdc: u8,
    /// STAT bits 3-6 (interrupt selects) as written.
    stat_select: u8,
    pub(crate) lyc: u8,
    /// Base clocks into the current frame (0 = LY 0, dot 0) at clock
    /// `synced`. 0 while the LCD is off.
    frame_clock: u32,
    /// The emulator clock `frame_clock` refers to.
    synced: u64,
    /// The screen shows white
    /// ([`Emulator::screen_blank`](crate::Emulator::screen_blank)): set
    /// when the LCD is turned off, cleared by the first frame that is
    /// actually displayed.
    blanked: bool,
    /// The LCD was turned on and its first frame hasn't completed yet. That
    /// frame is not displayed, so it doesn't clear `blanked`.
    skip_frame: bool,
}

impl Ppu {
    pub(crate) const fn new() -> Self {
        Self {
            lcdc: 0,
            stat_select: 0,
            lyc: 0,
            frame_clock: 0,
            synced: 0,
            blanked: false,
            skip_frame: false,
        }
    }

    /// Power-on state at clock 0.
    pub(crate) fn reset(&mut self) {
        *self = Self::new();
        // LCD on, BG on, tile data at 8000 (Pan Docs: Power Up Sequence).
        self.lcdc = 0x91;
        // TODO(M3): start at the PPU phase where the CGB boot ROM hands
        // over instead of LY=0 dot 0 (docs/plan/stage-1.md, M3: "PPU and
        // DIV phase at power-on").
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

    /// Base clocks from `frame_clock` to the next VBlank start (1 to
    /// `FRAME_CLOCKS`).
    #[inline]
    fn clocks_to_vblank(&self) -> u32 {
        if self.frame_clock < VBLANK_START {
            VBLANK_START - self.frame_clock
        } else {
            VBLANK_START + FRAME_CLOCKS - self.frame_clock
        }
    }

    /// When [`Ppu::sync`] must next run: the next VBlank start, or never
    /// while the LCD is off.
    #[inline]
    pub(crate) fn next_event(&self) -> u64 {
        if self.lcd_enabled() {
            self.synced + u64::from(self.clocks_to_vblank())
        } else {
            u64::MAX
        }
    }

    /// The frame position at clock `now` (`now` is never past
    /// `next_event`, so less than a frame has passed since `synced`).
    #[inline]
    fn frame_clock_at(&self, now: u64) -> u32 {
        if !self.lcd_enabled() {
            return 0;
        }
        let fc = self.frame_clock + (now - self.synced) as u32;
        if fc >= FRAME_CLOCKS {
            fc - FRAME_CLOCKS
        } else {
            fc
        }
    }

    /// Catches up to clock `now`. Completes the frame (white, for now),
    /// requests the VBlank interrupt and returns `FRAME_READY` if VBlank
    /// started on the way.
    pub(crate) fn sync(&mut self, now: u64, frame: &mut Frame, iflags: &mut u8) -> Events {
        if !self.lcd_enabled() {
            self.synced = now;
            return Events::EMPTY;
        }
        let to_vblank = self.clocks_to_vblank();
        let crossed = now - self.synced >= u64::from(to_vblank);
        self.frame_clock = self.frame_clock_at(now);
        self.synced = now;
        if !crossed {
            return Events::EMPTY;
        }
        *iflags |= INT_VBLANK;
        frame.fill(WHITE);
        if self.skip_frame {
            self.skip_frame = false;
        } else {
            self.blanked = false;
        }
        Events::FRAME_READY
    }

    /// LY at clock `now`.
    pub(crate) fn ly(&self, now: u64) -> u8 {
        (self.frame_clock_at(now) / LINE_CLOCKS) as u8
    }

    /// STAT at clock `now`: bit 7 reads 1; the mode is 0 while the LCD is
    /// off (Pan Docs: LCD Status Registers).
    pub(crate) fn stat(&self, now: u64) -> u8 {
        let mut v = 0x80 | self.stat_select;
        if !self.lcd_enabled() {
            return v;
        }
        let fc = self.frame_clock_at(now);
        let (ly, dot) = (fc / LINE_CLOCKS, fc % LINE_CLOCKS);
        if ly as u8 == self.lyc {
            v |= 0x04;
        }
        v | if ly >= SCREEN_HEIGHT as u32 {
            1
        } else if dot < 80 {
            2
        } else if dot < 80 + 172 {
            3
        } else {
            0
        }
    }

    pub(crate) fn write_stat(&mut self, value: u8) {
        self.stat_select = value & 0x78;
    }

    /// A CPU write to LCDC (FF40) at clock `now`; the caller has synced to
    /// `now`. Turning the LCD off abandons the frame in progress and blanks
    /// the screen; the PPU restarts at line 0 when it is turned back on
    /// (Pan Docs: LCDC.7 - LCD enable).
    ///
    /// The first frame after the LCD is turned on is not displayed: the
    /// screen stays blank until the second one completes (Pan Docs: LCDC.7;
    /// SameBoy Core/display.c: enabling the LCD sets `frame_skip`, and
    /// `GB_display_vblank` then outputs a blank frame instead).
    pub(crate) fn write_lcdc(&mut self, now: u64, value: u8) {
        debug_assert_eq!(self.synced, now);
        let was_on = self.lcd_enabled();
        let on = value & 0x80 != 0;
        if was_on && !on {
            self.blanked = true;
            self.skip_frame = false;
            self.frame_clock = 0;
        } else if !was_on && on {
            // The screen was white while the LCD was off (also if it has
            // been off since power-on) and stays white for this frame.
            self.blanked = true;
            self.skip_frame = true;
            self.frame_clock = 0;
        }
        self.synced = now;
        self.lcdc = value;
    }
}
