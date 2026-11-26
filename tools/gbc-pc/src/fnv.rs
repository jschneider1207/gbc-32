//! 64-bit FNV-1a over bytes: the ROM hash of `RomBegin` / `RomDone`
//! (ADR 0037). The textbook per-byte FNV-1a, the same function as
//! `gbc-firmware`'s `fnv::hash` (not a dependency: that crate pulls in
//! `gbc-core`); the shared test vectors below pin them together.

/// The FNV-1a 64-bit offset basis.
pub const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
/// The FNV 64-bit prime, 2^40 + 2^8 + 0xb3.
pub const PRIME: u64 = 0x0000_0100_0000_01b3;

/// FNV-1a 64 of `bytes`.
pub fn hash(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(OFFSET_BASIS, |h, &b| (h ^ u64::from(b)).wrapping_mul(PRIME))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vectors `gbc-firmware/src/fnv.rs` tests (Landon Curt Noll's
    /// `test_fnv.c`, FNV-1a 64-bit).
    #[test]
    fn known_vectors() {
        assert_eq!(hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(hash(b"foobar"), 0x8594_4171_f739_67e8);
    }
}
