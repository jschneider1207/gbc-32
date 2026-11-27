//! Consistent Overhead Byte Stuffing (Cheshire and Baker, "Consistent
//! Overhead Byte Stuffing", IEEE/ACM Trans. Networking, 1999; the
//! Wikipedia "Consistent Overhead Byte Stuffing" article's examples are the
//! test vectors). The encoded form contains no 0x00, so 0x00 can delimit
//! frames on the wire.
//!
//! Each block is a code byte `c` (1..=0xFF) followed by `c - 1` non-zero
//! data bytes; a block with `c < 0xFF` stands for its data plus one 0x00,
//! except at the end of the frame. A full block (`c == 0xFF`) has no
//! implied zero. The encoder emits no trailing empty block after a final
//! full block (the Wikipedia form, so 254 non-zero bytes encode to
//! `FF ..254 bytes..`); the decoder also accepts one.
//!
//! The encoder writes no delimiter; `crate::encode` appends it.

/// The largest encoded length for `n` input bytes (no delimiter):
/// one code byte per started 254-byte run, at least one.
pub const fn max_encoded_len(n: usize) -> usize {
    n + n / 254 + 1
}

/// Incremental encoder writing straight into `out`, so a frame needs no
/// intermediate payload buffer. Every write is bounds-checked: a full
/// buffer is `None`, never a panic.
pub(crate) struct Writer<'o> {
    out: &'o mut [u8],
    /// Where the current block's code byte goes (already reserved).
    code_idx: usize,
    /// The next free byte.
    pos: usize,
    /// The current block's code: 1 + its data bytes so far.
    code: u8,
}

impl<'o> Writer<'o> {
    pub(crate) fn new(out: &'o mut [u8]) -> Option<Self> {
        if out.is_empty() {
            return None;
        }
        Some(Self {
            out,
            code_idx: 0,
            pos: 1,
            code: 1,
        })
    }

    pub(crate) fn push(&mut self, b: u8) -> Option<()> {
        if self.code == 0xFF {
            // The block is full (254 data bytes): close it without an
            // implied zero. Done lazily, so a frame ending exactly on a
            // full block gets no extra empty block.
            self.close_block()?;
        }
        if b == 0 {
            self.close_block()
        } else {
            *self.out.get_mut(self.pos)? = b;
            self.pos += 1;
            self.code += 1;
            Some(())
        }
    }

    fn close_block(&mut self) -> Option<()> {
        *self.out.get_mut(self.code_idx)? = self.code;
        // Reserve the next block's code byte.
        self.out.get(self.pos)?;
        self.code_idx = self.pos;
        self.pos += 1;
        self.code = 1;
        Some(())
    }

    /// `push` for every byte of `data`, a run at a time: the bytes up to
    /// the next zero (or the end of the block's room) are found with a
    /// scan and copied in one go (review P3). Same output and the same
    /// `None` cases as the per-byte loop: a run that does not fit fails
    /// before it is copied, which only changes bytes past the error,
    /// where nothing is meaningful.
    pub(crate) fn push_slice(&mut self, mut data: &[u8]) -> Option<()> {
        while !data.is_empty() {
            if self.code == 0xFF {
                self.close_block()?;
            }
            // Data bytes the block still takes (1..=254).
            let room = usize::from(0xFF - self.code);
            let window = data.get(..room).unwrap_or(data);
            let run = window.iter().position(|&b| b == 0);
            let n = run.unwrap_or(window.len());
            let end = self.pos.checked_add(n)?;
            self.out
                .get_mut(self.pos..end)?
                .copy_from_slice(window.get(..n)?);
            self.pos = end;
            // `n <= room = 0xFF - code`, so this stays <= 0xFF.
            self.code += n as u8;
            if run.is_some() {
                self.close_block()?;
                data = data.get(n + 1..)?;
            } else {
                data = data.get(n..)?;
            }
        }
        Some(())
    }

    /// Writes the last code byte; returns the encoded length.
    pub(crate) fn finish(self) -> Option<usize> {
        *self.out.get_mut(self.code_idx)? = self.code;
        Some(self.pos)
    }
}

/// COBS-encodes `src` into `out` (no delimiter). Returns the encoded
/// length, or `None` if `out` is too small
/// (`max_encoded_len(src.len())` always suffices).
pub fn encode(src: &[u8], out: &mut [u8]) -> Option<usize> {
    let mut w = Writer::new(out)?;
    w.push_slice(src)?;
    w.finish()
}

/// Decodes one COBS frame (without its delimiter) in place and returns the
/// decoded length; the data is `buf[..len]`. `None` if the input is not
/// valid COBS: empty, a zero byte anywhere, or a block running past the
/// end. Decoding in place is safe because the write index never passes
/// the read index.
pub fn decode_in_place(buf: &mut [u8]) -> Option<usize> {
    let len = buf.len();
    if len == 0 {
        return None;
    }
    let (mut r, mut w) = (0usize, 0usize);
    while r < len {
        let code = *buf.get(r)?;
        if code == 0 {
            return None;
        }
        r += 1;
        let end = r.checked_add(usize::from(code - 1))?;
        // A block running past the end fails the `get(r)` below.
        while r < end {
            let b = *buf.get(r)?;
            if b == 0 {
                return None;
            }
            *buf.get_mut(w)? = b;
            w += 1;
            r += 1;
        }
        if code != 0xFF && r < len {
            *buf.get_mut(w)? = 0;
            w += 1;
        }
    }
    Some(w)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn run(raw: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut out = [0u8; 600];
        let n = encode(raw, &mut out).unwrap();
        assert!(n <= max_encoded_len(raw.len()));
        let enc = out[..n].to_vec();
        let mut buf = enc.clone();
        let m = decode_in_place(&mut buf).unwrap();
        assert_eq!(&buf[..m], raw, "round trip");
        (enc, buf[..m].to_vec())
    }

    /// Wikipedia "Consistent Overhead Byte Stuffing", Encoding examples
    /// (the trailing 00 there is the delimiter, not part of the encoding).
    #[test]
    fn wikipedia_vectors() {
        let seq = |a: u8, b: u8| (a..=b).collect::<Vec<u8>>();
        let cat = |parts: &[&[u8]]| parts.concat();
        let cases: Vec<(Vec<u8>, Vec<u8>)> = std::vec![
            (std::vec![0x00], std::vec![0x01, 0x01]),
            (std::vec![0x00, 0x00], std::vec![0x01, 0x01, 0x01]),
            (
                std::vec![0x00, 0x11, 0x00],
                std::vec![0x01, 0x02, 0x11, 0x01]
            ),
            (
                std::vec![0x11, 0x22, 0x00, 0x33],
                std::vec![0x03, 0x11, 0x22, 0x02, 0x33]
            ),
            (
                std::vec![0x11, 0x22, 0x33, 0x44],
                std::vec![0x05, 0x11, 0x22, 0x33, 0x44]
            ),
            (
                std::vec![0x11, 0x00, 0x00, 0x00],
                std::vec![0x02, 0x11, 0x01, 0x01, 0x01]
            ),
            (seq(0x01, 0xFE), cat(&[&[0xFF], &seq(0x01, 0xFE)])),
            (
                cat(&[&[0x00], &seq(0x01, 0xFE)]),
                cat(&[&[0x01, 0xFF], &seq(0x01, 0xFE)])
            ),
            (
                seq(0x01, 0xFF),
                cat(&[&[0xFF], &seq(0x01, 0xFE), &[0x02, 0xFF]])
            ),
            (
                cat(&[&seq(0x02, 0xFF), &[0x00]]),
                cat(&[&[0xFF], &seq(0x02, 0xFF), &[0x01, 0x01]])
            ),
            (
                cat(&[&seq(0x03, 0xFF), &[0x00, 0x01]]),
                cat(&[&[0xFE], &seq(0x03, 0xFF), &[0x02, 0x01]])
            ),
        ];
        for (raw, want) in cases {
            let (enc, _) = run(&raw);
            assert_eq!(enc, want, "encoding of {raw:02x?}");
        }
    }

    #[test]
    fn empty_input_encodes_to_one_code_byte() {
        let (enc, _) = run(&[]);
        assert_eq!(enc, [0x01]);
    }

    #[test]
    fn trailing_empty_block_after_full_block_is_accepted() {
        let mut buf: Vec<u8> = [&[0xFF][..], &[7u8; 254], &[0x01]].concat();
        let n = decode_in_place(&mut buf).unwrap();
        assert_eq!(&buf[..n], &[7u8; 254]);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        for bad in [
            &[][..],
            &[0x00],
            &[0x03, 0x11],
            &[0x02, 0x00],
            &[0x01, 0x00],
        ] {
            assert_eq!(decode_in_place(&mut bad.to_vec()), None, "{bad:02x?}");
        }
    }

    #[test]
    fn small_output_buffer_is_an_error() {
        let mut out = [0u8; 4];
        assert_eq!(encode(&[1, 2, 3, 4], &mut out), None);
        assert_eq!(encode(&[1, 2, 3], &mut out), Some(4));
        assert_eq!(encode(&[], &mut []), None);
    }

    /// The per-byte encoder (`push` for every byte: the implementation
    /// before `push_slice`), the reference the block-wise path must match.
    fn reference(src: &[u8], out: &mut [u8]) -> Option<usize> {
        let mut w = Writer::new(out)?;
        for &b in src {
            w.push(b)?;
        }
        w.finish()
    }

    /// Byte-identical to the per-byte encoder for every length
    /// 0..=MAX_PAYLOAD at several zero densities (none, rare, half, all,
    /// and zeros only at block boundaries' neighbours via the 1/256 rate),
    /// whole and fed as random pieces mixing `push` and `push_slice`.
    #[test]
    fn push_slice_matches_per_byte_push() {
        let mut rng = crate::crc::TestRng(0x2545_F491_4F6C_DD1D);
        let mut src = [0u8; crate::MAX_PAYLOAD];
        let mut want = [0u8; crate::MAX_ENCODED];
        let mut got = [0u8; crate::MAX_ENCODED];
        for zero in [0, 1, 16, 128, 256] {
            for len in 0..=crate::MAX_PAYLOAD {
                rng.bytes(len, zero, &mut src);
                let src = &src[..len];
                let n = reference(src, &mut want).unwrap();
                assert_eq!(encode(src, &mut got), Some(n), "len {len} zero {zero}");
                assert_eq!(&got[..n], &want[..n], "len {len} zero {zero}");
                got.fill(0xAA);
                let mut w = Writer::new(&mut got).unwrap();
                let mut rest = src;
                while !rest.is_empty() {
                    let k = (rng.next() as usize % 300).min(rest.len());
                    let (piece, r) = rest.split_at(k);
                    if rng.next() & 3 == 0 {
                        piece.iter().for_each(|&b| w.push(b).unwrap());
                    } else {
                        w.push_slice(piece).unwrap();
                    }
                    rest = r;
                }
                assert_eq!(w.finish(), Some(n), "pieces, len {len} zero {zero}");
                assert_eq!(&got[..n], &want[..n], "pieces, len {len} zero {zero}");
            }
        }
    }

    /// Zero runs around the 254-byte block limit, exhaustively: a zero (or
    /// none) at every position of payloads 250..=512 bytes long.
    #[test]
    fn push_slice_matches_at_block_edges() {
        let mut want = [0u8; 600];
        let mut got = [0u8; 600];
        for len in 250..=512usize {
            for z in 0..=len {
                let src: std::vec::Vec<u8> = (0..len)
                    .map(|i| if i == z { 0 } else { (i % 255) as u8 + 1 })
                    .collect();
                let n = reference(&src, &mut want).unwrap();
                assert_eq!(encode(&src, &mut got), Some(n), "len {len} zero at {z}");
                assert_eq!(&got[..n], &want[..n], "len {len} zero at {z}");
            }
        }
    }

    /// A too-small `out` is `None` exactly when the per-byte encoder says
    /// so, at every buffer size.
    #[test]
    fn push_slice_small_buffers_match() {
        let mut rng = crate::crc::TestRng(0xDEAD_BEEF_CAFE_F00D);
        let mut src = [0u8; 700];
        for (len, zero) in [
            (0, 0),
            (1, 256),
            (253, 0),
            (254, 0),
            (255, 0),
            (600, 8),
            (700, 0),
        ] {
            rng.bytes(len, zero, &mut src);
            let src = &src[..len];
            for size in 0..=max_encoded_len(len) + 1 {
                let mut a = std::vec![0u8; size];
                let mut b = std::vec![0u8; size];
                let ra = reference(src, &mut a);
                assert_eq!(encode(src, &mut b), ra, "len {len} size {size}");
                if let Some(n) = ra {
                    assert_eq!(a[..n], b[..n]);
                }
            }
        }
    }
}
