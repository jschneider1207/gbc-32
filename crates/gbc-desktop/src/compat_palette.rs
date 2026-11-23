//! The colors a CGB gives a DMG-only cartridge (ADR 0025). The core has no
//! boot ROM, so the front end does the boot ROM's palette pick and passes
//! the result in [`Config`](gbc_core::Config).
//!
//! Algorithm (Pan Docs, "Power Up Sequence" > "Compatibility palettes",
//! <https://gbdev.io/pandocs/Power_Up_Sequence.html#compatibility-palettes>):
//! if the licensee is Nintendo (old licensee 0x01, or 0x33 with new
//! licensee "01"), sum the 16 title bytes 0x134-0x143 mod 256 and find the
//! sum in a table. The last 14 sums are shared by several games and also
//! need the title's 4th byte to match; a shared sum whose letter doesn't
//! match keeps searching. No match, or another licensee, gives palette
//! ID 0.
//!
//! Tables: SameBoy's `BootROMs/cgb_boot.asm` (MIT, notice below), which
//! lists the shared sums once per letter row. They were cross-checked by
//! script against ISSOtm's disassembly of the real CGB boot ROM
//! (codeberg.org/ISSOtm/gb-bootroms, commit 443d7f0, `src/cgb.asm`): the
//! 120 colors, the 79 sums, the 29 tie-break letters, and the resolved
//! (OBJ0, OBJ1, BG) color offsets for all 94 table entries, including the
//! real ROM's palette shuffle flags, are identical. No disagreement found.
//! Pan Docs agrees on the algorithm but has no tables.

use gbc_core::DmgPalettes;

/// What `--dmg-palette` asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// The CGB boot ROM's choice for this cartridge.
    Auto,
    /// Plain grayscale.
    Gray,
}

/// The palettes `mode` gives `rom`.
pub fn select(mode: Mode, rom: &[u8]) -> DmgPalettes {
    match mode {
        Mode::Auto => for_rom(rom),
        Mode::Gray => DmgPalettes::GRAYSCALE,
    }
}

/// The palettes the CGB boot ROM would load for `rom`. CGB-flagged carts
/// (0x143 bit 7) never use them, and get the default (palette ID 0).
pub fn for_rom(rom: &[u8]) -> DmgPalettes {
    palettes(palette_id(rom))
}

fn palette_id(rom: &[u8]) -> usize {
    let Some(header) = rom.get(0x134..0x150) else {
        return 0;
    };
    let byte = |addr: usize| header[addr - 0x134];
    if byte(0x143) & 0x80 != 0 {
        return 0;
    }
    let nintendo = match byte(0x14B) {
        0x33 => byte(0x144) == b'0' && byte(0x145) == b'1',
        old => old == 0x01,
    };
    if !nintendo {
        return 0;
    }
    let title = &header[..16];
    let sum = title.iter().fold(0u8, |s, &b| s.wrapping_add(b));
    CHECKSUMS
        .iter()
        .enumerate()
        .find(|&(i, &c)| {
            c == sum && (i < FIRST_SHARED || FOURTH_LETTERS[i - FIRST_SHARED] == title[3])
        })
        .map_or(0, |(i, _)| usize::from(PALETTE_ID[i]))
}

fn palettes(id: usize) -> DmgPalettes {
    let [obj0, obj1, bg] = COMBOS[id];
    let pal = |at: u8| {
        let at = usize::from(at);
        [COLORS[at], COLORS[at + 1], COLORS[at + 2], COLORS[at + 3]]
    };
    DmgPalettes {
        bg: pal(bg),
        obj0: pal(obj0),
        obj1: pal(obj1),
    }
}

// The tables below are transcribed from SameBoy, BootROMs/cgb_boot.asm
// (TitleChecksums, PalettePerChecksum, Dups4thLetterArray,
// PaletteCombinations up to entry 50, Palettes 0-29; SameBoy's own
// "exclusive" key-combo palettes are left out):
//
// Copyright (c) 2015-2026 Lior Halphon
//
// Permission is hereby granted, free of charge, to any person obtaining a
// copy of this software and associated documentation files (the
// "Software"), to deal in the Software without restriction, including
// without limitation the rights to use, copy, modify, merge, publish,
// distribute, sublicense, and/or sell copies of the Software, and to permit
// persons to whom the Software is furnished to do so, subject to the
// following conditions:
//
// The above copyright notice and this permission notice shall be included
// in all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
// OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN
// NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM,
// DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR
// OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
// USE OR OTHER DEALINGS IN THE SOFTWARE.

/// Index of the first title sum that also needs the 4th title byte.
const FIRST_SHARED: usize = 65;

/// Title sums. Entries from [`FIRST_SHARED`] on repeat the 14 shared sums
/// once per tie-break letter row (14 + 14 + 1).
#[rustfmt::skip]
const CHECKSUMS: [u8; 94] = [
    0x00, 0x88, 0x16, 0x36, 0xD1, 0xDB, 0xF2, 0x3C, 0x8C, 0x92, 0x3D, 0x5C, 0x58, 0xC9, 0x3E, 0x70,
    0x1D, 0x59, 0x69, 0x19, 0x35, 0xA8, 0x14, 0xAA, 0x75, 0x95, 0x99, 0x34, 0x6F, 0x15, 0xFF, 0x97,
    0x4B, 0x90, 0x17, 0x10, 0x39, 0xF7, 0xF6, 0xA2, 0x49, 0x4E, 0x43, 0x68, 0xE0, 0x8B, 0xF0, 0xCE,
    0x0C, 0x29, 0xE8, 0xB7, 0x86, 0x9A, 0x52, 0x01, 0x9D, 0x71, 0x9C, 0xBD, 0x5D, 0x6D, 0x67, 0x3F,
    0x6B, 0xB3, 0x46, 0x28, 0xA5, 0xC6, 0xD3, 0x27, 0x61, 0x18, 0x66, 0x6A, 0xBF, 0x0D, 0xF4, 0xB3,
    0x46, 0x28, 0xA5, 0xC6, 0xD3, 0x27, 0x61, 0x18, 0x66, 0x6A, 0xBF, 0x0D, 0xF4, 0xB3,
];

/// The 4th title byte each shared sum (from [`FIRST_SHARED`]) needs.
const FOURTH_LETTERS: &[u8; 29] = b"BEFAARBEKEK R-URAR INAILICE R";

/// Palette ID (index into [`COMBOS`]) per [`CHECKSUMS`] entry.
#[rustfmt::skip]
const PALETTE_ID: [u8; 94] = [
    0, 4, 5, 35, 34, 3, 31, 15, 10, 5, 19, 36, 7, 37, 30, 44,
    21, 32, 31, 20, 5, 33, 13, 14, 5, 29, 5, 18, 9, 3, 2, 26,
    25, 25, 41, 42, 26, 45, 42, 45, 36, 38, 26, 42, 30, 41, 34, 34,
    5, 42, 6, 5, 33, 25, 42, 42, 40, 2, 16, 25, 42, 42, 5, 0,
    39, 36, 22, 25, 6, 32, 12, 36, 11, 39, 18, 39, 24, 31, 50, 17,
    46, 6, 27, 0, 47, 41, 41, 0, 0, 19, 34, 23, 18, 29,
];

/// Per palette ID: the first [`COLORS`] index of OBJ0, OBJ1 and BG. Most
/// are multiples of 4 (whole palettes); a few start mid-palette, as on
/// the real boot ROM.
#[rustfmt::skip]
const COMBOS: [[u8; 3]; 51] = [
    [16, 16, 116], [72, 72, 72], [80, 80, 80], [96, 96, 96], [36, 36, 36], [0, 0, 0],
    [108, 108, 108], [20, 20, 20], [48, 48, 48], [104, 104, 104], [64, 32, 32], [16, 112, 112],
    [16, 8, 8], [12, 16, 16], [16, 116, 116], [112, 16, 112], [8, 68, 8], [64, 64, 32],
    [16, 16, 28], [16, 16, 72], [16, 16, 80], [76, 76, 36], [15, 15, 44], [68, 68, 8],
    [16, 16, 8], [16, 16, 12], [112, 112, 0], [12, 12, 0], [0, 0, 4], [72, 88, 72],
    [80, 88, 80], [96, 88, 96], [64, 88, 32], [68, 16, 52], [111, 0, 56], [111, 16, 60],
    [76, 91, 36], [64, 112, 40], [16, 92, 112], [68, 88, 8], [16, 0, 8], [16, 112, 12],
    [112, 12, 0], [12, 112, 16], [84, 112, 16], [12, 112, 0], [100, 12, 112], [0, 112, 32],
    [16, 12, 112], [112, 12, 24], [16, 112, 116],
];

/// 30 four-color palettes, RGB555 exactly as in the boot ROM.
#[rustfmt::skip]
const COLORS: [u16; 120] = [
    0x7FFF, 0x32BF, 0x00D0, 0x0000, //  0
    0x639F, 0x4279, 0x15B0, 0x04CB, //  1
    0x7FFF, 0x6E31, 0x454A, 0x0000, //  2
    0x7FFF, 0x1BEF, 0x0200, 0x0000, //  3
    0x7FFF, 0x421F, 0x1CF2, 0x0000, //  4
    0x7FFF, 0x5294, 0x294A, 0x0000, //  5
    0x7FFF, 0x03FF, 0x012F, 0x0000, //  6
    0x7FFF, 0x03EF, 0x01D6, 0x0000, //  7
    0x7FFF, 0x42B5, 0x3DC8, 0x0000, //  8
    0x7E74, 0x03FF, 0x0180, 0x0000, //  9
    0x67FF, 0x77AC, 0x1A13, 0x2D6B, // 10
    0x7ED6, 0x4BFF, 0x2175, 0x0000, // 11
    0x53FF, 0x4A5F, 0x7E52, 0x0000, // 12
    0x4FFF, 0x7ED2, 0x3A4C, 0x1CE0, // 13
    0x03ED, 0x7FFF, 0x255F, 0x0000, // 14
    0x036A, 0x021F, 0x03FF, 0x7FFF, // 15
    0x7FFF, 0x01DF, 0x0112, 0x0000, // 16
    0x231F, 0x035F, 0x00F2, 0x0009, // 17
    0x7FFF, 0x03EA, 0x011F, 0x0000, // 18
    0x299F, 0x001A, 0x000C, 0x0000, // 19
    0x7FFF, 0x027F, 0x001F, 0x0000, // 20
    0x7FFF, 0x03E0, 0x0206, 0x0120, // 21
    0x7FFF, 0x7EEB, 0x001F, 0x7C00, // 22
    0x7FFF, 0x3FFF, 0x7E00, 0x001F, // 23
    0x7FFF, 0x03FF, 0x001F, 0x0000, // 24
    0x03FF, 0x001F, 0x000C, 0x0000, // 25
    0x7FFF, 0x033F, 0x0193, 0x0000, // 26
    0x0000, 0x4200, 0x037F, 0x7FFF, // 27
    0x7FFF, 0x7E8C, 0x7C00, 0x0000, // 28
    0x7FFF, 0x1BEF, 0x6180, 0x0000, // 29
];

#[cfg(test)]
mod tests {
    use super::*;

    /// A header with `title` (NUL-padded), Nintendo's old licensee code
    /// and a DMG-only CGB flag.
    fn rom(title: &[u8]) -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x134..0x134 + title.len()].copy_from_slice(title);
        rom[0x14B] = 0x01;
        rom
    }

    const RED: [u16; 4] = [0x7FFF, 0x421F, 0x1CF2, 0x0000]; // palette 4
    const GREEN: [u16; 4] = [0x7FFF, 0x1BEF, 0x0200, 0x0000]; // palette 3

    /// Palette ID 0 (SameBoy PaletteCombinations 0): OBJ palette 4, BG 29.
    const DEFAULT: DmgPalettes = DmgPalettes {
        bg: [0x7FFF, 0x1BEF, 0x6180, 0x0000],
        obj0: RED,
        obj1: RED,
    };

    #[test]
    fn pokemon_red_is_red_with_green_obj0() {
        // Sum 0x14: table entry 22, palette ID 13 = comb (OBJ0 3, OBJ1 4, BG 4).
        let r = rom(b"POKEMON RED");
        assert_eq!(
            r[0x134..0x144].iter().fold(0u8, |s, &b| s.wrapping_add(b)),
            0x14
        );
        let p = for_rom(&r);
        assert_eq!(p.bg, RED);
        assert_eq!(p.obj0, GREEN);
        assert_eq!(p.obj1, RED);
    }

    #[test]
    fn new_licensee_01_counts_as_nintendo() {
        let mut r = rom(b"POKEMON RED");
        r[0x14B] = 0x33;
        r[0x144..0x146].copy_from_slice(b"01");
        assert_eq!(for_rom(&r).bg, RED);
        r[0x145] = b'8';
        assert_eq!(for_rom(&r), DEFAULT);
    }

    #[test]
    fn shared_sum_is_split_by_the_fourth_letter() {
        // Both sum to 0x61. 'E' is row 0 (POKEMON BLUE, ID 11 = comb 4, 28,
        // 28); 'A' is row 1 (VEGAS STAKES, ID 41 = comb 4, 28, 3); anything
        // else falls through to the default.
        let blue = for_rom(&rom(b"POKEMON BLUE"));
        let pal28 = [0x7FFF, 0x7E8C, 0x7C00, 0x0000];
        assert_eq!(
            blue,
            DmgPalettes {
                bg: pal28,
                obj0: RED,
                obj1: pal28
            }
        );
        let row1 = for_rom(&rom(b"POKAMON BLUI"));
        assert_eq!(
            row1,
            DmgPalettes {
                bg: GREEN,
                obj0: RED,
                obj1: pal28
            }
        );
        assert_eq!(for_rom(&rom(b"POKIMON BLUA")), DEFAULT);
    }

    #[test]
    fn third_row_letter_is_reached() {
        // Sum 0xB3 with 4th letter 'R' (TETRIS ATTACK): entry 93, ID 29 =
        // comb (OBJ0 18, OBJ1 22, BG 18).
        let mut t = *b"TETRIS ATTACK\0\0\0";
        let s = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[15] = 0xB3u8.wrapping_sub(s);
        let p = for_rom(&rom(&t));
        assert_eq!(p.bg, [0x7FFF, 0x03EA, 0x011F, 0x0000]);
        assert_eq!(p.obj1, [0x7FFF, 0x7EEB, 0x001F, 0x7C00]);
    }

    #[test]
    fn mid_palette_offset_is_kept() {
        // SUPER MARIOLAND, ID 22: OBJ starts at color 15 (palette 3's last).
        let mut t = *b"SUPER MARIOLAND\0";
        let s = t.iter().fold(0u8, |s, &b| s.wrapping_add(b));
        t[15] = 0x46u8.wrapping_sub(s);
        assert_eq!(for_rom(&rom(&t)).obj0, [0x0000, 0x7FFF, 0x421F, 0x1CF2]);
    }

    #[test]
    fn other_licensee_gets_the_default() {
        let mut r = rom(b"POKEMON RED");
        r[0x14B] = 0x08;
        assert_eq!(for_rom(&r), DEFAULT);
    }

    #[test]
    fn unknown_title_gets_the_default() {
        // Sum 0x7A is not in the table.
        assert!(!CHECKSUMS.contains(&0x7A));
        assert_eq!(for_rom(&rom(b"HOMEBREW!")), DEFAULT);
    }

    #[test]
    fn cgb_flagged_cart_gets_the_default() {
        for flag in [0x80, 0xC0] {
            let mut r = rom(b"POKEMON RED");
            // 0x143 is also the last title byte: compensate in 0x142 so the
            // sum still matches POKEMON RED and only the flag decides.
            r[0x143] = flag;
            r[0x142] = 0u8.wrapping_sub(flag);
            let sum = r[0x134..0x144].iter().fold(0u8, |s, &b| s.wrapping_add(b));
            assert_eq!(sum, 0x14);
            assert_eq!(for_rom(&r), DEFAULT, "flag {flag:#x}");
        }
    }

    #[test]
    fn gray_mode_is_grayscale() {
        let r = rom(b"POKEMON RED");
        assert_eq!(select(Mode::Gray, &r), DmgPalettes::GRAYSCALE);
        assert_eq!(select(Mode::Auto, &r), for_rom(&r));
    }

    #[test]
    fn short_rom_gets_the_default() {
        assert_eq!(for_rom(&[0; 0x140]), DEFAULT);
    }

    #[test]
    fn tables_are_consistent() {
        for id in PALETTE_ID {
            assert!(usize::from(id) < COMBOS.len());
        }
        for c in COMBOS.iter().flatten() {
            assert!(usize::from(*c) + 4 <= COLORS.len());
        }
        assert_eq!(CHECKSUMS.len() - FIRST_SHARED, FOURTH_LETTERS.len());
    }
}
