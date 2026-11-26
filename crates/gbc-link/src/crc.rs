//! CRC-32 as in IEEE 802.3 / zlib ("CRC-32/ISO-HDLC" in the Rocksoft
//! catalogue): reflected, polynomial 0x04C11DB7 (0xEDB88320 reflected),
//! init and xorout 0xFFFFFFFF. Check value: "123456789" -> 0xCBF43926.
//!
//! A 16-entry (64-byte) nibble table: two lookups per byte. A 256-entry
//! table would be 1 KiB of flash for about twice the speed; the bitwise
//! form would be 8 iterations per byte. A 1 KiB frame costs a few
//! thousand cycles this way, which is negligible next to the UART time.

const POLY: u32 = 0xEDB8_8320;

#[allow(clippy::indexing_slicing)] // const evaluation; `i < 16`
const TABLE: [u32; 16] = {
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

/// Running CRC-32 state.
#[derive(Clone, Copy, Debug)]
pub struct Crc32(u32);

impl Crc32 {
    pub const fn new() -> Self {
        Self(0xFFFF_FFFF)
    }

    #[inline]
    pub fn update(&mut self, b: u8) {
        let mut c = self.0 ^ u32::from(b);
        // `& 0xF` keeps the index in range; `get` keeps it panic-free.
        c = (c >> 4) ^ TABLE.get((c & 0xF) as usize).copied().unwrap_or(0);
        c = (c >> 4) ^ TABLE.get((c & 0xF) as usize).copied().unwrap_or(0);
        self.0 = c;
    }

    pub fn update_slice(&mut self, data: &[u8]) {
        for &b in data {
            self.update(b);
        }
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
    }
}
