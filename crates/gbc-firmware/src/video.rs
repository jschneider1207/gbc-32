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
//! No tables. Four source pixels become six output pixels, three words
//! (`aa`, `bc`, `cd`). Each source row is converted once into a 480-byte
//! stack row (in DTCM) which is then stored to the output twice for the
//! doubled rows (source row 2k -> output rows 3k and 3k+1), and the single
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

/// One RGB555 pixel as the panel's 16-bit word, in the value (not byte)
/// sense: `B5 G6 R5` from the top. See the module doc.
#[inline(always)]
const fn panel_word(p: u16) -> u16 {
    // Bit 4 of g is p bit 9; it becomes g6's bit 0 (panel bit 5).
    (p & 0x1F) | ((p & 0x7FE0) << 1) | ((p >> 9) & 1) << 5
}

/// Two panel words as one `u32` whose memory bytes are in wire order
/// (high byte first within each pixel).
#[inline(always)]
const fn wire_pair(first: u16, second: u16) -> u32 {
    // Little-endian bytes of the result: hi(first) lo(first) hi(second)
    // lo(second); each `swap_bytes` is one REV16 half.
    (first.swap_bytes() as u32) | ((second.swap_bytes() as u32) << 16)
}

/// Converts one source row (160 pixels) to one output row (120 words).
#[inline(always)]
fn convert_row(src: &[u16; SCREEN_WIDTH], dst: &mut OutRow) {
    let (src, _) = src.as_chunks::<4>();
    let (dst, _) = dst.as_chunks_mut::<3>();
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let [a, b, c, e] = [
            panel_word(s[0]),
            panel_word(s[1]),
            panel_word(s[2]),
            panel_word(s[3]),
        ];
        // Output pixels: a a b c c e (source 0 0 1 2 2 3).
        d[0] = wire_pair(a, a);
        d[1] = wire_pair(b, c);
        d[2] = wire_pair(c, e);
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
/// Row `r0` is converted once into a stack row, then stored twice, so the
/// output buffer is only ever written.
#[inline(always)]
fn convert_band(r0: &[u16; SCREEN_WIDTH], r1: &[u16; SCREEN_WIDTH], out: &mut OutBand) {
    let [o0, o1, o2] = out;
    let mut tmp: OutRow = [0; OUT_ROW_WORDS];
    convert_row(r0, &mut tmp);
    *o0 = tmp;
    *o1 = tmp;
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
