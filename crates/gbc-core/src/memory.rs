//! Large internal RAM arrays.

/// The big RAM blocks of the console, kept apart from the rest of the
/// state so that [`Emulator::init`](crate::Emulator::init) can initialize
/// them in place by zero-filling.
///
/// INVARIANT: every field is a plain integer or an array of them, so the
/// all-zero bit pattern is a valid value and equals [`Memory::new`].
/// `Emulator::init` relies on this. It is enforced at compile time by
/// [`Memory::ZERO_VALID`] (every field must implement [`ZeroValid`]) and
/// tested against `new()` and `clear()` (lib.rs, `init_matches_new` and
/// `memory_zero_init_equals_cleared_state`).
#[cfg_attr(test, derive(PartialEq))]
pub(crate) struct Memory {
    /// 8 banks of 4 KiB (CGB). Bank 0 at C000, banks 1-7 at D000 (SVBK).
    pub(crate) wram: [u8; 0x8000],
    /// 2 banks of 8 KiB (CGB, VBK).
    pub(crate) vram: [u8; 0x4000],
    pub(crate) oam: [u8; 0xA0],
    pub(crate) hram: [u8; 0x7F],
}

/// Types for which the all-zero bit pattern is a valid value.
///
/// # Safety
/// Implement only for types where all-zero bytes are a valid value
/// (integers and arrays of them; not references or most enums).
pub(crate) unsafe trait ZeroValid {}
// SAFETY: every bit pattern is a valid integer.
unsafe impl ZeroValid for u8 {}
// SAFETY: as above.
unsafe impl ZeroValid for u16 {}
// SAFETY: as above.
unsafe impl ZeroValid for u32 {}
// SAFETY: an array of zero-valid elements is zero-valid.
unsafe impl<T: ZeroValid, const N: usize> ZeroValid for [T; N] {}

impl Memory {
    /// Compile-time guard for the invariant: the exhaustive pattern stops
    /// compiling when a field is added, and each field must be `ZeroValid`.
    #[allow(dead_code)]
    const ZERO_VALID: fn(&Memory) = |m| {
        fn check<T: ZeroValid>(_: &T) {}
        let Memory {
            wram,
            vram,
            oam,
            hram,
        } = m;
        check(wram);
        check(vram);
        check(oam);
        check(hram);
    };

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
