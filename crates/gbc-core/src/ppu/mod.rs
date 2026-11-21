//! Picture processing unit. Skeleton only: timing and rendering arrive in
//! M3. For now a white frame completes at the start of each VBlank, once
//! every [`FRAME_CLOCKS`] base clocks.

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
    /// Base clocks into the current frame (0 = LY 0, dot 0).
    frame_clock: u32,
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
            frame_clock: 0,
            blanked: false,
            skip_frame: false,
        }
    }

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

    /// A CPU write to LCDC (FF40). Turning the LCD off abandons the frame
    /// in progress and blanks the screen; the PPU restarts at line 0 when
    /// it is turned back on (Pan Docs: LCDC.7 - LCD enable).
    ///
    /// The first frame after the LCD is turned on is not displayed: the
    /// screen stays blank until the second one completes (Pan Docs: LCDC.7;
    /// SameBoy Core/display.c: enabling the LCD sets `frame_skip`, and
    /// `GB_display_vblank` then outputs a blank frame instead).
    #[allow(dead_code)] // Used by the bus from M1 on.
    pub(crate) fn write_lcdc(&mut self, value: u8) {
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
        }
        self.lcdc = value;
    }

    /// Advances the PPU by `clocks` base clocks (the PPU does not change
    /// speed in CGB double-speed mode). `clocks` is at most one M-cycle.
    ///
    /// Stub: completes an all-white frame at the start of every VBlank.
    #[inline]
    pub(crate) fn advance(&mut self, clocks: u32, frame: &mut Frame) -> Events {
        if !self.lcd_enabled() {
            return Events::EMPTY;
        }
        debug_assert!(clocks <= 4);
        let before = self.frame_clock;
        let mut now = before + clocks;
        if now >= FRAME_CLOCKS {
            now -= FRAME_CLOCKS;
        }
        self.frame_clock = now;
        // VBlank starts well inside the frame, so a step of at most one
        // M-cycle can't both wrap and cross it.
        if before < VBLANK_START && before + clocks >= VBLANK_START {
            frame.fill(WHITE);
            if self.skip_frame {
                self.skip_frame = false;
            } else {
                self.blanked = false;
            }
            return Events::FRAME_READY;
        }
        Events::EMPTY
    }
}
