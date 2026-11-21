//! Picture processing unit. Skeleton only: timing and rendering arrive in
//! M3. For now a white frame completes every [`FRAME_CLOCKS`] base clocks.

use crate::{Events, FRAME_CLOCKS};

pub const SCREEN_WIDTH: usize = 160;
pub const SCREEN_HEIGHT: usize = 144;

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
    /// Base clocks into the current frame.
    frame_clock: u32,
    /// The LCD was turned off and no frame has completed since
    /// ([`Emulator::screen_blank`](crate::Emulator::screen_blank)).
    blanked: bool,
}

impl Ppu {
    pub(crate) const fn new() -> Self {
        Self {
            lcdc: 0,
            frame_clock: 0,
            blanked: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
        // LCD on, BG on, tile data at 8000 (Pan Docs: Power Up Sequence).
        self.lcdc = 0x91;
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
    /// in progress and blanks the screen until the next completed frame;
    /// the PPU restarts at line 0 when it is turned back on (Pan Docs:
    /// LCDC.7 - LCD enable).
    #[allow(dead_code)] // Used by the bus from M1 on.
    pub(crate) fn write_lcdc(&mut self, value: u8) {
        if self.lcd_enabled() && value & 0x80 == 0 {
            self.blanked = true;
            self.frame_clock = 0;
        }
        self.lcdc = value;
    }

    /// Advances the PPU by `clocks` base clocks (the PPU does not change
    /// speed in CGB double-speed mode).
    ///
    /// Stub: completes an all-white frame every `FRAME_CLOCKS`.
    #[inline]
    pub(crate) fn advance(&mut self, clocks: u32, frame: &mut Frame) -> Events {
        if !self.lcd_enabled() {
            return Events::EMPTY;
        }
        self.frame_clock += clocks;
        if self.frame_clock >= FRAME_CLOCKS {
            self.frame_clock -= FRAME_CLOCKS;
            frame.fill(WHITE);
            self.blanked = false;
            return Events::FRAME_READY;
        }
        Events::EMPTY
    }
}
