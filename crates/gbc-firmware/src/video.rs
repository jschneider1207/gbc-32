//! The 1.5x scaler: a core [`Frame`] (160x144 RGB555) to the panel's wire
//! format (240x216, RGB565, ADR/decision D3 in `docs/plan/stage-2.md`).
//!
//! # Scaling
//!
//! Nearest neighbour, 2 -> 3 on both axes with the exact map
//! `src = floor(dst * 2 / 3)`: destination 0, 1, 2 take source 0, 0, 1, so
//! source pixels (and rows) alternate between being doubled and single.
//!
//! # Colour
//!
//! The core's pixel is `r | g << 5 | b << 10` (`gbc_core::rgb555`). The
//! panel's 16-bit format (ST7789V datasheet V1.3 8.8.3, "65K-Colors", COLMOD 05h) is
//! `D15..D11 = R4..R0, D10..D5 = G5..G0, D4..D0 = B4..B0` with the high
//! byte sent first. With MADCTL BGR = 1 (ST7789V 9.1.28, MADCTL D3 "RGB/BGR order")
//! the panel swaps the red and blue sub-pixels, so the field the datasheet
//! calls R (top 5 bits) lights blue and the field it calls B (bottom 5
//! bits) lights red. The word is therefore
//!
//! ```text
//! core blue  (bits 10-14) -> panel bits 11-15
//! core green (bits  5-9 ) -> panel bits  5-10, g6 = (g << 1) | (g >> 4)
//! core red   (bits  0-4 ) -> panel bits  0-4
//! ```
//!
//! which keeps red where it is and only shifts green and blue up by one,
//! so it is a mask, a shift and an OR, with no channel swap.
//!
//! # Output layout
//!
//! [`OutFrame`] is `[[u32; 120]; 216]`: 25,920 words, 103,680 bytes
//! ([`OUT_BYTES`]). Each `u32` holds two pixels, and its little-endian
//! memory bytes are `hi0, lo0, hi1, lo1`, i.e. the wire order, so on the
//! little-endian MCU the buffer's memory *is* the byte stream the MDMA
//! sends (low address first), with no further pass. A `u32` array rather
//! than `[u8; N]` makes the compiler emit full word stores (the buffer sits
//! in non-cacheable AXI SRAM, where narrow stores are costly), and the
//! swap of each 16-bit half is a single `REV16` on the Cortex-M7. The
//! board takes the byte view; on the host use [`to_wire_bytes`].
//!
//! # Speed
//!
//! No tables. Two source pixels are read as one `u32` and converted
//! together (`panel_pair`: three masks, two shifts, two ORs for both),
//! then byte-swapped per half (`rev16`, one `REV16`) into wire order.
//! Four source pixels become six output pixels, three words (`aa`, `bc`,
//! `ce`), built from the two converted pairs with a shift or two. The
//! doubled source row (2k -> output rows 3k and 3k+1) stores each word to
//! both rows as it is made, with no stack row in between, and the single
//! row (2k+1 -> 3k+2) is converted straight into the output. Nothing is
//! ever read back from the output buffer. The hot loops use fixed-size
//! array chunks, so there are no bounds checks or panics.

use gbc_core::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH};

/// Output width in pixels (1.5 x 160).
pub const OUT_W: usize = 240;
/// Output height in pixels (1.5 x 144).
pub const OUT_H: usize = 216;
/// Output size in bytes (2 per pixel): 103,680.
pub const OUT_BYTES: usize = OUT_W * OUT_H * 2;
/// `u32` words per output row (two pixels each).
pub const OUT_ROW_WORDS: usize = OUT_W / 2;

const _: () = assert!(SCREEN_WIDTH == 160 && SCREEN_HEIGHT == 144);

/// One output row.
pub type OutRow = [u32; OUT_ROW_WORDS];
/// The whole output, row-major, in wire order (see the module doc).
pub type OutFrame = [OutRow; OUT_H];
/// One band: the three output rows made from two source rows.
pub type OutBand = [OutRow; 3];

/// Two RGB555 pixels (the first in the low half) as two panel words in the
/// same halves, in the value (not byte) sense: `B5 G6 R5` from the top of
/// each half. See the module doc. Per half: red (bits 0-4) stays; green
/// and blue (bits 5-14) move up one (bits 6-15); green's top bit (bit 9)
/// is copied to bit 5, g6 = (g << 1) | (g >> 4). Bit 15 of a source pixel
/// is ignored (the core never sets it, ADR 0013): in the low half the
/// shift moves it to bit 16, which the mask clears, and in the high half
/// it falls off the top. No bit of one half reaches the other.
#[inline(always)]
const fn panel_pair(w: u32) -> u32 {
    (w & 0x001F_001F) | ((w << 1) & 0xFFC0_FFC0) | ((w >> 4) & 0x0020_0020)
}

/// Swaps the two bytes of each 16-bit half (one `REV16` on the Cortex-M7),
/// so that the little-endian memory bytes of two panel words are the wire
/// order: high byte first within each pixel.
///
/// Written as a byte swap plus a rotate rather than as the shift-and-mask
/// form `(w >> 8) & 0x00FF_00FF | (w << 8) & 0xFF00_FF00`: LLVM folds
/// that form into [`panel_pair`]'s masks and emits 8 instructions per
/// pixel pair (`lsr`, `uxtb16`, `orr`, ...) instead of 5 and a `REV16`
/// (thumbv7em, rustc 1.100.0, checked in the player image's disassembly).
#[inline(always)]
const fn rev16(w: u32) -> u32 {
    w.swap_bytes().rotate_left(16)
}

/// Four source pixels `a b c e` to the three output words for the pixels
/// `a a b c c e` (source 0 0 1 2 2 3), in wire order.
#[inline(always)]
fn convert4(s: &[u16; 4]) -> [u32; 3] {
    let ab = rev16(panel_pair(s[0] as u32 | (s[1] as u32) << 16));
    let ce = rev16(panel_pair(s[2] as u32 | (s[3] as u32) << 16));
    [ab << 16 | (ab & 0xFFFF), ab >> 16 | ce << 16, ce]
}

/// Converts one source row (160 pixels) to one output row (120 words).
#[inline(always)]
fn convert_row(src: &[u16; SCREEN_WIDTH], dst: &mut OutRow) {
    let (src, _) = src.as_chunks::<4>();
    let (dst, _) = dst.as_chunks_mut::<3>();
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = convert4(s);
    }
}

/// Converts one source row to two identical output rows, storing each word
/// to both as it is made (no stack row, no reads of the output).
#[inline(always)]
fn convert_row_twice(src: &[u16; SCREEN_WIDTH], dst0: &mut OutRow, dst1: &mut OutRow) {
    let (src, _) = src.as_chunks::<4>();
    let (dst0, _) = dst0.as_chunks_mut::<3>();
    let (dst1, _) = dst1.as_chunks_mut::<3>();
    for ((s, d0), d1) in src.iter().zip(dst0.iter_mut()).zip(dst1.iter_mut()) {
        let words = convert4(s);
        *d0 = words;
        *d1 = words;
    }
}

/// Scales source rows `2 * band` and `2 * band + 1` into the three output
/// rows `3 * band ..= 3 * band + 2`. `band` is 0..72; this is the unit for
/// converting part of a frame (for example one band between two other
/// jobs).
#[inline]
pub fn scale_band(frame: &Frame, band: usize, out: &mut OutBand) {
    let (rows, _) = frame.as_chunks::<SCREEN_WIDTH>();
    let [r0, r1] = match rows.get(band * 2..band * 2 + 2) {
        Some([a, b]) => [a, b],
        // Out-of-range band: nothing to do (cold, not in the frame loop).
        _ => return,
    };
    convert_band(r0, r1, out);
}

/// Scales a whole frame. Every output word is written exactly once.
pub fn scale_frame(frame: &Frame, out: &mut OutFrame) {
    let (rows, _) = frame.as_chunks::<{ 2 * SCREEN_WIDTH }>();
    let (bands, _) = out.as_chunks_mut::<3>();
    for (pair, band) in rows.iter().zip(bands.iter_mut()) {
        let (r0, r1) = pair.split_at(SCREEN_WIDTH);
        let (Ok(r0), Ok(r1)) = (
            <&[u16; SCREEN_WIDTH]>::try_from(r0),
            <&[u16; SCREEN_WIDTH]>::try_from(r1),
        ) else {
            continue;
        };
        convert_band(r0, r1, band);
    }
}

/// The output as the byte stream the panel receives (host and tests; the
/// board views the same memory as bytes).
pub fn to_wire_bytes(out: &OutFrame, bytes: &mut [u8; OUT_BYTES]) {
    let (chunks, _) = bytes.as_chunks_mut::<4>();
    for (chunk, word) in chunks.iter_mut().zip(out.iter().flatten()) {
        *chunk = word.to_le_bytes();
    }
}

/// Source row `r0` (doubled) then `r1` (single) to the band's three rows.
/// Row `r0` is converted once and each word stored to both of its rows, so
/// the output buffer is only ever written.
#[inline(always)]
fn convert_band(r0: &[u16; SCREEN_WIDTH], r1: &[u16; SCREEN_WIDTH], out: &mut OutBand) {
    let [o0, o1, o2] = out;
    convert_row_twice(r0, o0, o1);
    convert_row(r1, o2);
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;
    use std::boxed::Box;
    use std::vec;
    use std::vec::Vec;

    /// SplitMix64: deterministic, no deps.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }
    }

    /// The per-pixel conversion the scaler used before [`panel_pair`]
    /// (one RGB555 pixel to one panel word, value sense), kept as the
    /// reference for the two-pixel form.
    const fn panel_word(p: u16) -> u16 {
        // Bit 4 of g is p bit 9; it becomes g6's bit 0 (panel bit 5).
        (p & 0x1F) | ((p & 0x7FE0) << 1) | ((p >> 9) & 1) << 5
    }

    /// Every u16 in each half of [`panel_pair`] against [`panel_word`],
    /// with a varying neighbour in the other half (so a bit leaking across
    /// halves shows), and [`rev16`] against `u16::swap_bytes` per half.
    #[test]
    fn panel_pair_matches_panel_word_exhaustively() {
        for v in 0..=u16::MAX {
            for other in [0, 0xFFFF, v.rotate_left(7) ^ 0xA5C3, !v] {
                let lo = panel_pair(v as u32 | (other as u32) << 16);
                let hi = panel_pair(other as u32 | (v as u32) << 16);
                assert_eq!(lo as u16, panel_word(v), "low half {v:#06x}");
                assert_eq!((hi >> 16) as u16, panel_word(v), "high half {v:#06x}");
                let w = v as u32 | (other as u32) << 16;
                assert_eq!(rev16(w) as u16, v.swap_bytes(), "rev16 low {v:#06x}");
                assert_eq!((rev16(w) >> 16) as u16, other.swap_bytes());
            }
        }
    }

    /// Frames of one value per bit (alone and as its complement), bit 15
    /// alone and with everything, and column/row stripes, against the
    /// reference.
    #[test]
    fn matches_reference_on_edge_frames() {
        let mut values: Vec<u16> = (0..16).map(|b| 1u16 << b).collect();
        values.extend((0..16).map(|b| !(1u16 << b)));
        values.extend([0x0000, 0xFFFF, 0x8000, 0x7FFF, 0x5555, 0xAAAA]);
        for &p in &values {
            let f = filled(p);
            assert!(scaled(&f) == reference(&f), "pixel {p:#06x}");
        }
        // Stripes: alternating columns, then alternating rows, of a and b.
        for (a, b) in [(0x0000, 0xFFFF), (0x7C1F, 0x03E0), (0x8000, 0x0201)] {
            let mut f = filled(0);
            for (i, p) in f.iter_mut().enumerate() {
                *p = if i.is_multiple_of(2) { a } else { b };
            }
            assert!(scaled(&f) == reference(&f), "columns {a:#06x}/{b:#06x}");
            for (i, p) in f.iter_mut().enumerate() {
                *p = if (i / SCREEN_WIDTH).is_multiple_of(2) {
                    a
                } else {
                    b
                };
            }
            assert!(scaled(&f) == reference(&f), "rows {a:#06x}/{b:#06x}");
        }
    }

    /// Naive per-pixel reference, written from the spec in the module doc:
    /// the exact map, the 6-bit green expansion and B5 G6 R5 big-endian.
    fn reference(frame: &Frame) -> Vec<u8> {
        let mut v = Vec::with_capacity(OUT_BYTES);
        for y in 0..OUT_H {
            for x in 0..OUT_W {
                let p = frame[(y * 2 / 3) * SCREEN_WIDTH + x * 2 / 3];
                let (r, g, b) = (p & 31, (p >> 5) & 31, (p >> 10) & 31);
                let g6 = (g << 1) | (g >> 4);
                let w = (b << 11) | (g6 << 5) | r;
                v.push((w >> 8) as u8);
                v.push(w as u8);
            }
        }
        v
    }

    fn scaled(frame: &Frame) -> Vec<u8> {
        let mut out: Box<OutFrame> = Box::new([[0; OUT_ROW_WORDS]; OUT_H]);
        scale_frame(frame, &mut out);
        let mut bytes = vec![0u8; OUT_BYTES];
        to_wire_bytes(&out, bytes.as_mut_slice().try_into().unwrap());
        bytes
    }

    fn filled(p: u16) -> Box<Frame> {
        Box::new([p; SCREEN_WIDTH * SCREEN_HEIGHT])
    }

    #[test]
    fn matches_reference_on_random_frames() {
        let mut rng = Rng(1);
        for _ in 0..8 {
            let mut f = filled(0);
            for p in f.iter_mut() {
                // Includes bit 15, which the core never sets but must be ignored.
                *p = rng.next() as u16;
            }
            assert!(scaled(&f) == reference(&f));
        }
    }

    #[test]
    fn white_black_and_single_channels() {
        for p in [0x7FFF, 0, 0x001F, 0x03E0, 0x7C00, 0x0010, 0x0200, 0x4000] {
            let f = filled(p);
            let got = scaled(&f);
            assert!(got == reference(&f), "pixel {p:#06x}");
        }
        // Pin the bit assignment itself, independent of the reference:
        // white is 0xFFFF; core red alone is panel bits 0-4, green alone
        // bits 5-10, blue alone bits 11-15; bytes are high first.
        assert_eq!(scaled(&filled(0x7FFF))[..2], [0xFF, 0xFF]);
        assert_eq!(scaled(&filled(0x001F))[..2], [0x00, 0x1F]);
        assert_eq!(scaled(&filled(0x03E0))[..2], [0x07, 0xE0]);
        assert_eq!(scaled(&filled(0x7C00))[..2], [0xF8, 0x00]);
    }

    #[test]
    fn row_and_column_map_is_floor_two_thirds() {
        // Every source pixel unique (23,040 < 32,768 once bit 15 is clear).
        let mut f = filled(0);
        for (i, p) in f.iter_mut().enumerate() {
            *p = i as u16;
        }
        assert!(scaled(&f) == reference(&f));
        // Pin the map itself: columns 0..6 -> 0 0 1 2 2 3, rows 0..3 -> 0 0 1.
        let got = scaled(&f);
        let px = |x: usize, y: usize| {
            let i = (y * OUT_W + x) * 2;
            u16::from_be_bytes([got[i], got[i + 1]])
        };
        let src_of = |w: u16| {
            // Undo panel_word for a 15-bit index: (b, g6 >> 1, r).
            (w & 31) | (((w >> 6) & 31) << 5) | ((w >> 11) << 10)
        };
        for (x, s) in [0usize, 0, 1, 2, 2, 3].iter().enumerate() {
            assert_eq!(src_of(px(x, 0)) as usize, *s);
        }
        for (y, s) in [0usize, 0, 1, 2, 2, 3].iter().enumerate() {
            assert_eq!(src_of(px(0, y)) as usize, s * SCREEN_WIDTH);
        }
    }

    #[test]
    fn bands_match_whole_frame() {
        let mut rng = Rng(7);
        let mut f = filled(0);
        for p in f.iter_mut() {
            *p = rng.next() as u16;
        }
        let mut whole: Box<OutFrame> = Box::new([[0; OUT_ROW_WORDS]; OUT_H]);
        scale_frame(&f, &mut whole);
        for band in 0..SCREEN_HEIGHT / 2 {
            let mut b: OutBand = [[0; OUT_ROW_WORDS]; 3];
            scale_band(&f, band, &mut b);
            assert!(b[..] == whole[band * 3..band * 3 + 3]);
        }
    }
}
