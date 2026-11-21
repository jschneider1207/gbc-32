//! Configuration supplied by the caller when a cartridge is loaded.
//!
//! The emulated model is fixed: CPU CGB revision C (ADR 0003). There is no
//! model or revision option.

/// Packs 5-bit red, green and blue channels into the core's RGB555 pixel
/// format: bits 0-4 red, 5-9 green, 10-14 blue, bit 15 zero. This is the
/// CGB palette RAM layout (Pan Docs: LCD Color Palettes), so CGB colors are
/// copied through unchanged.
#[inline]
pub const fn rgb555(r: u8, g: u8, b: u8) -> u16 {
    (r as u16 & 0x1F) | ((g as u16 & 0x1F) << 5) | ((b as u16 & 0x1F) << 10)
}

/// Converts 8-bit channels to RGB555 by dropping the low three bits. This is
/// the exact inverse of the `(x << 3) | (x >> 2)` expansion used for
/// screenshot comparison (acceptance/README.md).
#[inline]
pub const fn rgb888_to_555(r: u8, g: u8, b: u8) -> u16 {
    rgb555(r >> 3, g >> 3, b >> 3)
}

/// Expands one RGB555 pixel to 8-bit `[r, g, b]` with `(x << 3) | (x >> 2)`.
#[inline]
pub const fn rgb555_to_888(pixel: u16) -> [u8; 3] {
    const fn expand(x: u16) -> u8 {
        let x = (x & 0x1F) as u8;
        (x << 3) | (x >> 2)
    }
    [expand(pixel), expand(pixel >> 5), expand(pixel >> 10)]
}

/// Colors for games running in DMG compatibility mode. On hardware the CGB
/// boot ROM picks these from the cartridge title; without a boot ROM the
/// caller supplies them. Each entry maps DMG shade 0 (lightest) to 3
/// (darkest) to an RGB555 color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmgPalettes {
    pub bg: [u16; 4],
    pub obj0: [u16; 4],
    pub obj1: [u16; 4],
}

impl DmgPalettes {
    /// Plain grayscale for all three palettes.
    pub const GRAYSCALE: Self = {
        let p = [
            rgb555(31, 31, 31),
            rgb555(21, 21, 21),
            rgb555(10, 10, 10),
            rgb555(0, 0, 0),
        ];
        Self {
            bg: p,
            obj0: p,
            obj1: p,
        }
    };
}

impl Default for DmgPalettes {
    fn default() -> Self {
        Self::GRAYSCALE
    }
}

/// Per-cartridge configuration, passed to [`Emulator::load`](crate::Emulator::load).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Config {
    /// Used only when the cartridge runs in DMG compatibility mode.
    pub dmg_palettes: DmgPalettes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_round_trip_for_reference_palette() {
        // acceptance/README.md "common palette": all exactly representable.
        for rgb in [
            [0x00, 0x00, 0x00],
            [0x00, 0x63, 0xC6],
            [0x7B, 0xFF, 0x31],
            [0xFF, 0xFF, 0xFF],
            [0x94, 0x39, 0x39],
            [0xFF, 0x84, 0x84],
        ] {
            let p = rgb888_to_555(rgb[0], rgb[1], rgb[2]);
            assert_eq!(rgb555_to_888(p), rgb);
        }
    }

    #[test]
    fn channel_layout() {
        assert_eq!(rgb555(31, 0, 0), 0x001F);
        assert_eq!(rgb555(0, 31, 0), 0x03E0);
        assert_eq!(rgb555(0, 0, 31), 0x7C00);
        assert_eq!(rgb555(31, 31, 31), 0x7FFF);
    }
}
