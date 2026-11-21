//! Large internal RAM arrays.

/// The big RAM blocks of the console, kept apart from the rest of the
/// state so that [`Emulator::init`](crate::Emulator::init) can initialize
/// them in place by zero-filling.
///
/// INVARIANT: this struct contains only `u8` arrays (or other plain
/// integers), so the all-zero bit pattern is a valid value and equals
/// [`Memory::new`]. `Emulator::init` relies on this.
pub(crate) struct Memory {
    /// 8 banks of 4 KiB (CGB). Bank 0 at C000, banks 1-7 at D000 (SVBK).
    pub(crate) wram: [u8; 0x8000],
    /// 2 banks of 8 KiB (CGB, VBK).
    pub(crate) vram: [u8; 0x4000],
    pub(crate) oam: [u8; 0xA0],
    pub(crate) hram: [u8; 0x7F],
}

impl Memory {
    pub(crate) const fn new() -> Self {
        Self {
            wram: [0; 0x8000],
            vram: [0; 0x4000],
            oam: [0; 0xA0],
            hram: [0; 0x7F],
        }
    }

    /// Zero-fills in place (no large temporary on the stack). Power-on RAM
    /// contents are not random here: the core is deterministic.
    pub(crate) fn clear(&mut self) {
        self.wram.fill(0);
        self.vram.fill(0);
        self.oam.fill(0);
        self.hram.fill(0);
    }
}
