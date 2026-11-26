//! 64-bit FNV-1a (Fowler/Noll/Vo, "FNV-1a": for each byte, `h ^= byte;
//! h *= prime`), over bytes only. Multi-byte values are folded as their
//! little-endian bytes, so the hash of a value is the same on every target.
//!
//! This is the textbook per-byte FNV-1a, unlike the four-pixels-per-step
//! frame folding of `tools/bench/src/fingerprint.rs` and
//! `tools/h7-bench/src/workload.rs`: the headless hashes are not
//! comparable with theirs, but they can be checked with any FNV-1a
//! implementation (e.g. over a frame dumped as little-endian `u16`s).

use gbc_core::{Frame, StereoSample};

/// The FNV-1a 64-bit offset basis.
pub const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// The FNV 64-bit prime, 2^40 + 2^8 + 0xb3.
pub const PRIME: u64 = 0x0000_0100_0000_01b3;

/// A running 64-bit FNV-1a hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fnv1a(u64);

impl Default for Fnv1a {
    fn default() -> Self {
        Self::new()
    }
}

impl Fnv1a {
    /// The hash of no bytes.
    pub const fn new() -> Self {
        Self(OFFSET_BASIS)
    }

    /// Resumes a hash from a state [`get`](Self::get) returned (the ISR
    /// side of `audio::FeedTap` keeps its state in two atomics).
    pub(crate) const fn resume(state: u64) -> Self {
        Self(state)
    }

    /// The hash of everything folded in so far.
    pub const fn get(self) -> u64 {
        self.0
    }

    #[inline]
    pub fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(PRIME);
    }

    #[inline]
    pub fn bytes(&mut self, bs: &[u8]) {
        for &b in bs {
            self.byte(b);
        }
    }

    pub fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    /// A frame: every pixel (row-major, as `Frame` stores them) as its two
    /// little-endian bytes.
    pub fn frame(&mut self, f: &Frame) {
        for &px in f {
            self.bytes(&px.to_le_bytes());
        }
    }

    /// Audio samples, in order, each as `left` then `right`, both
    /// little-endian `i16`.
    pub fn samples(&mut self, s: &[StereoSample]) {
        for a in s {
            self.bytes(&a.left.to_le_bytes());
            self.bytes(&a.right.to_le_bytes());
        }
    }
}

/// FNV-1a of `bs`.
pub fn hash(bs: &[u8]) -> u64 {
    let mut h = Fnv1a::new();
    h.bytes(bs);
    h.get()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gbc_core::WHITE;

    /// Test vectors from the FNV reference (Landon Curt Noll's
    /// `test_fnv.c`, FNV-1a 64-bit).
    #[test]
    fn known_vectors() {
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn frame_is_little_endian_pixel_bytes() {
        let mut f: Frame = [WHITE; 160 * 144];
        f[1] = 0x1234;
        let mut bytes = [0u8; 2 * 160 * 144];
        for (i, px) in f.iter().enumerate() {
            bytes[2 * i..2 * i + 2].copy_from_slice(&px.to_le_bytes());
        }
        let mut h = Fnv1a::new();
        h.frame(&f);
        assert_eq!(h.get(), hash(&bytes));
    }

    #[test]
    fn samples_are_left_then_right_little_endian() {
        let s = [
            StereoSample {
                left: 0x0102,
                right: -2,
            },
            StereoSample { left: 3, right: 4 },
        ];
        let mut h = Fnv1a::new();
        h.samples(&s);
        assert_eq!(h.get(), hash(&[0x02, 0x01, 0xfe, 0xff, 3, 0, 4, 0]));
    }
}
