//! CRC-32 as in IEEE 802.3 / zlib ("CRC-32/ISO-HDLC" in the Rocksoft
//! catalogue): reflected, polynomial 0x04C11DB7 (0xEDB88320 reflected),
//! init and xorout 0xFFFFFFFF. Check value: "123456789" -> 0xCBF43926.
//!
//! Slicing-by-4 (Kounavis and Berry, "A Systematic Approach to Building
//! High Performance Software-Based CRC Generators", ISCC 2005): four
//! 256-entry tables (4 KiB of `.rodata`, flash on the board), one 32-bit
//! XOR and four lookups per 4 bytes; single bytes and a slice's tail use
//! the first table (the classic Sarwate byte table). It replaced a 16-entry
//! nibble table (two lookups per byte) because the player CRCs every
//! streamed audio byte (review P3, `docs/perf.md` "COBS and CRC (P3)").

const POLY: u32 = 0xEDB8_8320;

/// `TABLES[0]` is the byte table; `TABLES[k][i]` is the CRC of byte `i`
/// followed by `k` zero bytes, so four bytes fold in one step.
#[allow(clippy::indexing_slicing)] // const evaluation; `i < 256`, `k < 4`
const TABLES: [[u32; 256]; 4] = {
    let mut t = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut k = 1;
    while k < 4 {
        let mut i = 0;
        while i < 256 {
            let p = t[k - 1][i];
            t[k][i] = (p >> 8) ^ t[0][(p & 0xFF) as usize];
            i += 1;
        }
        k += 1;
    }
    t
};

/// `table[i]`; a `u8` index into 256 entries is always in range, so the
/// `get` compiles to a plain load.
#[inline(always)]
fn at(table: &[u32; 256], i: u32) -> u32 {
    table.get((i & 0xFF) as usize).copied().unwrap_or(0)
}

/// Running CRC-32 state.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    #[inline]
    pub fn update(&mut self, b: u8) {
        let [t0, ..] = &TABLES;
        let c = self.0;
        self.0 = (c >> 8) ^ at(t0, c ^ u32::from(b));
    }

    pub fn update_slice(&mut self, data: &[u8]) {
        let [t0, t1, t2, t3] = &TABLES;
        let (words, tail) = data.as_chunks::<4>();
        let mut c = self.0;
        for &w in words {
            c ^= u32::from_le_bytes(w);
            c = at(t3, c) ^ at(t2, c >> 8) ^ at(t1, c >> 16) ^ at(t0, c >> 24);
        }
        for &b in tail {
            c = (c >> 8) ^ at(t0, c ^ u32::from(b));
        }
        self.0 = c;
    }

    pub const fn finish(self) -> u32 {
        !self.0
    }
}

impl Default for Crc32 {
    fn default() -> Self {
        Self::new()
    }
}

/// CRC-32 of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = Crc32::new();
    c.update_slice(data);
    c.finish()
}

/// The previous implementation (16-entry nibble table, two lookups per
/// byte), kept as the reference the tests compare against.
#[cfg(test)]
pub(crate) fn reference_crc32(data: &[u8]) -> u32 {
    const NIBBLE: [u32; 16] = {
        let mut t = [0u32; 16];
        let mut i = 0;
        while i < 16 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 4 {
                c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
                k += 1;
            }
            t[i] = c;
            i += 1;
        }
        t
    };
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        c = (c >> 4) ^ NIBBLE[(c & 0xF) as usize];
        c = (c >> 4) ^ NIBBLE[(c & 0xF) as usize];
    }
    !c
}

/// A small xorshift generator for the property tests (no dependencies).
#[cfg(test)]
pub(crate) struct TestRng(pub(crate) u64);

#[cfg(test)]
impl TestRng {
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// `len` bytes, each zero with probability `zero_in_256 / 256`, else
    /// a random non-zero byte.
    pub(crate) fn bytes(&mut self, len: usize, zero_in_256: u32, out: &mut [u8]) {
        for b in &mut out[..len] {
            let r = self.next();
            *b = if ((r >> 8) & 0xFF) < u64::from(zero_in_256) {
                0
            } else {
                (r as u8).max(1)
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_value() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn known_values() {
        assert_eq!(crc32(b""), 0);
        // zlib.crc32(b"The quick brown fox jumps over the lazy dog")
        assert_eq!(
            crc32(b"The quick brown fox jumps over the lazy dog"),
            0x414F_A339
        );
        assert_eq!(crc32(&[0u8; 4]), 0x2144_DF1C);
    }

    #[test]
    fn table_matches_bitwise() {
        let bitwise = |data: &[u8]| {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= u32::from(b);
                for _ in 0..8 {
                    c = if c & 1 != 0 { (c >> 1) ^ POLY } else { c >> 1 };
                }
            }
            !c
        };
        let mut data = [0u8; 256];
        for (i, d) in data.iter_mut().enumerate() {
            *d = i as u8;
        }
        assert_eq!(crc32(&data), bitwise(&data));
        assert_eq!(reference_crc32(&data), bitwise(&data));
    }

    /// Every length 0..=MAX_PAYLOAD, several zero densities: equal to the
    /// nibble implementation, whole and fed in random pieces (each piece
    /// by `update_slice` or byte by byte, so every alignment of the
    /// 4-byte steps and tails is crossed).
    #[test]
    fn matches_reference_on_random_data() {
        let mut rng = TestRng(0x9E37_79B9_7F4A_7C15);
        let mut buf = [0u8; crate::MAX_PAYLOAD];
        for zero in [0, 1, 128, 256] {
            for len in 0..=crate::MAX_PAYLOAD {
                rng.bytes(len, zero, &mut buf);
                let data = &buf[..len];
                let want = reference_crc32(data);
                assert_eq!(crc32(data), want, "len {len} zero {zero}");
                let mut c = Crc32::new();
                let mut rest = data;
                while !rest.is_empty() {
                    let k = (rng.next() as usize % 9).min(rest.len());
                    let (piece, r) = rest.split_at(k);
                    if rng.next() & 1 == 0 {
                        c.update_slice(piece);
                    } else {
                        piece.iter().for_each(|&b| c.update(b));
                    }
                    rest = r;
                }
                assert_eq!(c.finish(), want, "pieces, len {len} zero {zero}");
            }
        }
    }
}
