//! ROM image header for the board's memory-mapped QSPI NOR window (ADR 0037).
//!
//! The window holds one image: a 256-byte header at offset 0, then the ROM
//! at offset 256 (one NOR page). The loader programs the ROM first and the
//! header last, so an interrupted load leaves an erased (all-0xFF) header,
//! which is [`ImageError::Erased`] ("no image"), distinct from a corrupt one.
//!
//! Byte map (all integers little-endian):
//! ```text
//!   0..8    magic  b"GBC32ROM"
//!   8..10   format version (u16, 1)
//!  10..12   flags (u16, 0; not checked on read, so later versions may use them)
//!  12..16   ROM length in bytes (u32)
//!  16..24   FNV-1a 64 of the ROM data (u64)
//!  24..40   cartridge title, ROM bytes 0x134..0x144 (informational only)
//!  40..48   header checksum: FNV-1a 64 of header bytes 0..40 (u64)
//!  48..256  0xFF padding (erased NOR; not checked on read)
//! ```
//! The hash guards against bad programming and erased flash, not against an
//! adversary. Nothing here panics on any input.

use crate::fnv;

/// Length of the header, and the offset of the ROM data.
pub const HEADER_LEN: usize = 256;
pub const MAGIC: [u8; 8] = *b"GBC32ROM";
pub const VERSION: u16 = 1;

const TITLE_AT: usize = 24;
const TITLE_LEN: usize = 16;
const CHECKSUM_AT: usize = 40;
/// ROM header title field (Pan Docs, "The Cartridge Header").
const ROM_TITLE: core::ops::Range<usize> = 0x134..0x144;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// The header is all 0xFF: nothing (or an interrupted load) was written.
    Erased,
    /// The window or header slice is shorter than [`HEADER_LEN`].
    WindowTooShort,
    BadMagic,
    UnsupportedVersion,
    /// The header checksum does not match (torn or corrupt header).
    HeaderChecksum,
    /// Length is 0, or larger than the window minus [`HEADER_LEN`]. Nothing
    /// else about the length is checked (e.g. not that it is a valid GB size).
    LengthOutOfRange,
    /// The ROM data does not match the header's hash.
    DataHash,
}

/// A parsed, checksum-verified header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageHeader {
    pub len: u32,
    pub hash: u64,
    /// Informational only: never used for validation beyond the header
    /// checksum covering it.
    pub title: [u8; 16],
}

impl ImageHeader {
    /// Whether `rom` is the stored image, by length then hash.
    pub fn matches(&self, rom: &[u8]) -> bool {
        usize::try_from(self.len) == Ok(rom.len()) && fnv::hash(rom) == self.hash
    }
}

fn u16_at(h: &[u8; HEADER_LEN], at: usize) -> u16 {
    let mut b = [0u8; 2];
    if let Some(s) = h.get(at..at + 2) {
        b.copy_from_slice(s);
    }
    u16::from_le_bytes(b)
}

fn u32_at(h: &[u8; HEADER_LEN], at: usize) -> u32 {
    let mut b = [0u8; 4];
    if let Some(s) = h.get(at..at + 4) {
        b.copy_from_slice(s);
    }
    u32::from_le_bytes(b)
}

fn u64_at(h: &[u8; HEADER_LEN], at: usize) -> u64 {
    let mut b = [0u8; 8];
    if let Some(s) = h.get(at..at + 8) {
        b.copy_from_slice(s);
    }
    u64::from_le_bytes(b)
}

fn checksum(h: &[u8; HEADER_LEN]) -> u64 {
    fnv::hash(h.get(..CHECKSUM_AT).unwrap_or(&[]))
}

/// Build the header for `rom`. Fails with `LengthOutOfRange` if `rom` is
/// empty or longer than `u32::MAX`; the window bound is checked on read.
pub fn encode_header(rom: &[u8]) -> Result<[u8; HEADER_LEN], ImageError> {
    let len = u32::try_from(rom.len()).map_err(|_| ImageError::LengthOutOfRange)?;
    if len == 0 {
        return Err(ImageError::LengthOutOfRange);
    }
    let mut h = [0xFFu8; HEADER_LEN];
    h[0..8].copy_from_slice(&MAGIC);
    h[8..10].copy_from_slice(&VERSION.to_le_bytes());
    h[10..12].copy_from_slice(&0u16.to_le_bytes());
    h[12..16].copy_from_slice(&len.to_le_bytes());
    h[16..24].copy_from_slice(&fnv::hash(rom).to_le_bytes());
    // A ROM shorter than 0x144 bytes leaves the title zeroed.
    let mut title = [0u8; TITLE_LEN];
    if let Some(t) = rom.get(ROM_TITLE) {
        title.copy_from_slice(t);
    }
    h[TITLE_AT..TITLE_AT + TITLE_LEN].copy_from_slice(&title);
    let sum = checksum(&h);
    h[CHECKSUM_AT..CHECKSUM_AT + 8].copy_from_slice(&sum.to_le_bytes());
    Ok(h)
}

/// Check magic, version, header checksum and that the length fits a window
/// of `window_len` bytes. Does not look at the ROM data.
pub fn parse_header(
    header: &[u8; HEADER_LEN],
    window_len: usize,
) -> Result<ImageHeader, ImageError> {
    if header.iter().all(|&b| b == 0xFF) {
        return Err(ImageError::Erased);
    }
    if header.get(0..8) != Some(&MAGIC[..]) {
        return Err(ImageError::BadMagic);
    }
    if u16_at(header, 8) != VERSION {
        return Err(ImageError::UnsupportedVersion);
    }
    if u64_at(header, CHECKSUM_AT) != checksum(header) {
        return Err(ImageError::HeaderChecksum);
    }
    let len = u32_at(header, 12);
    let max = window_len.saturating_sub(HEADER_LEN);
    if len == 0 || usize::try_from(len).map_or(true, |l| l > max) {
        return Err(ImageError::LengthOutOfRange);
    }
    let mut title = [0u8; TITLE_LEN];
    if let Some(t) = header.get(TITLE_AT..TITLE_AT + TITLE_LEN) {
        title.copy_from_slice(t);
    }
    Ok(ImageHeader {
        len,
        hash: u64_at(header, 16),
        title,
    })
}

/// Parse the header of `window` (the whole mapped region) without hashing
/// the data: enough to decide whether an incoming ROM is already stored.
pub fn read_header(window: &[u8]) -> Result<ImageHeader, ImageError> {
    let header: &[u8; HEADER_LEN] = window
        .get(..HEADER_LEN)
        .and_then(|s| s.try_into().ok())
        .ok_or(ImageError::WindowTooShort)?;
    parse_header(header, window.len())
}

/// Check the header and the data hash of `window`; returns the ROM bytes.
pub fn validate(window: &[u8]) -> Result<&[u8], ImageError> {
    let h = read_header(window)?;
    let len = usize::try_from(h.len).map_err(|_| ImageError::LengthOutOfRange)?;
    let rom = window
        .get(HEADER_LEN..)
        .and_then(|d| d.get(..len))
        .ok_or(ImageError::LengthOutOfRange)?;
    if fnv::hash(rom) != h.hash {
        return Err(ImageError::DataHash);
    }
    Ok(rom)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIN: usize = 4096;

    fn rom(n: usize) -> [u8; 1024] {
        let mut r = [0u8; 1024];
        for (i, b) in r.iter_mut().enumerate().take(n) {
            *b = (i as u8).wrapping_mul(7).wrapping_add(1);
        }
        r
    }

    /// A window with `rom[..n]` stored the way the loader would.
    fn window(n: usize) -> [u8; WIN] {
        let r = rom(n);
        let mut w = [0xFFu8; WIN];
        w[..HEADER_LEN].copy_from_slice(&encode_header(&r[..n]).unwrap());
        w[HEADER_LEN..HEADER_LEN + n].copy_from_slice(&r[..n]);
        w
    }

    fn refix(w: &mut [u8; WIN]) {
        let h: &mut [u8; HEADER_LEN] = (&mut w[..HEADER_LEN]).try_into().unwrap();
        let s = checksum(h);
        h[CHECKSUM_AT..CHECKSUM_AT + 8].copy_from_slice(&s.to_le_bytes());
    }

    #[test]
    fn round_trip() {
        let w = window(1024);
        assert_eq!(validate(&w).unwrap(), &rom(1024)[..]);
        let h = read_header(&w).unwrap();
        assert_eq!(h.len, 1024);
    }

    #[test]
    fn title_copied_from_rom() {
        let mut r = [0u8; 0x200];
        r[0x134..0x144].copy_from_slice(b"TESTTITLE\0\0\0\0\0\0\0");
        let h = encode_header(&r).unwrap();
        assert_eq!(&h[24..40], b"TESTTITLE\0\0\0\0\0\0\0");
        assert_eq!(&h[48..], &[0xFFu8; 208][..]);
    }

    #[test]
    fn erased_is_distinct() {
        let w = [0xFFu8; WIN];
        assert_eq!(validate(&w), Err(ImageError::Erased));
        let mut w = window(100);
        w[5] = 0xFF; // corrupt, not erased
        assert_eq!(validate(&w), Err(ImageError::BadMagic));
    }

    #[test]
    fn corrupt_fields() {
        let mut w = window(100);
        w[0] ^= 1;
        assert_eq!(validate(&w), Err(ImageError::BadMagic));

        let mut w = window(100);
        w[8] = 2;
        refix(&mut w);
        assert_eq!(validate(&w), Err(ImageError::UnsupportedVersion));

        // Each header byte covered by the checksum, flipped alone.
        for i in 8..CHECKSUM_AT + 8 {
            let mut w = window(100);
            w[i] ^= 0x10;
            let e = validate(&w).unwrap_err();
            assert!(
                matches!(
                    e,
                    ImageError::HeaderChecksum | ImageError::UnsupportedVersion
                ),
                "byte {i}: {e:?}"
            );
        }
    }

    #[test]
    fn data_bit_flip() {
        let mut w = window(100);
        w[HEADER_LEN + 50] ^= 0x01;
        assert_eq!(validate(&w), Err(ImageError::DataHash));
        // A flip past the ROM length is not part of the image.
        let mut w = window(100);
        w[HEADER_LEN + 100] ^= 0x01;
        assert!(validate(&w).is_ok());
    }

    #[test]
    fn length_bounds() {
        let mut w = window(100);
        let bad = |w: &mut [u8; WIN], len: u32| {
            w[12..16].copy_from_slice(&len.to_le_bytes());
            refix(w);
        };
        bad(&mut w, 0);
        assert_eq!(validate(&w), Err(ImageError::LengthOutOfRange));
        bad(&mut w, (WIN - HEADER_LEN + 1) as u32);
        assert_eq!(validate(&w), Err(ImageError::LengthOutOfRange));
        bad(&mut w, u32::MAX);
        assert_eq!(validate(&w), Err(ImageError::LengthOutOfRange));
        // Exactly the maximum is allowed (then fails on the data hash).
        bad(&mut w, (WIN - HEADER_LEN) as u32);
        assert_eq!(validate(&w), Err(ImageError::DataHash));
        assert_eq!(encode_header(&[]), Err(ImageError::LengthOutOfRange));
        // parse_header alone enforces the bound (validate's slicing would
        // also catch it, which must not mask this check).
        let h = encode_header(&[7; 10]).unwrap();
        assert!(parse_header(&h, HEADER_LEN + 10).is_ok());
        assert_eq!(
            parse_header(&h, HEADER_LEN + 9),
            Err(ImageError::LengthOutOfRange)
        );
    }

    #[test]
    fn full_window_rom() {
        let mut w = [0u8; WIN];
        for (i, b) in w.iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        let h = encode_header(&w[HEADER_LEN..]).unwrap();
        w[..HEADER_LEN].copy_from_slice(&h);
        assert_eq!(validate(&w).unwrap().len(), WIN - HEADER_LEN);
    }

    #[test]
    fn short_window() {
        assert_eq!(validate(&[]), Err(ImageError::WindowTooShort));
        assert_eq!(
            validate(&[0xFF; HEADER_LEN - 1]),
            Err(ImageError::WindowTooShort)
        );
        // Header only, no room for data: any length is out of range.
        let h = encode_header(&[1, 2, 3]).unwrap();
        assert_eq!(validate(&h), Err(ImageError::LengthOutOfRange));
    }

    #[test]
    fn title_is_informational_but_checksummed() {
        let mut w = window(100);
        w[24..40].copy_from_slice(b"SOMETHING ELSE!!");
        assert_eq!(validate(&w), Err(ImageError::HeaderChecksum));
        refix(&mut w);
        assert!(validate(&w).is_ok());
    }

    #[test]
    fn flags_and_padding_not_checked() {
        let mut w = window(100);
        w[100] = 0x00; // padding
        w[10] = 1; // flags
        refix(&mut w);
        assert!(validate(&w).is_ok());
    }

    #[test]
    fn matches_by_len_and_hash() {
        let w = window(100);
        let h = read_header(&w).unwrap();
        let r = rom(100);
        assert!(h.matches(&r[..100]));
        assert!(!h.matches(&r[..99]));
        let mut r2 = r;
        r2[3] ^= 1;
        assert!(!h.matches(&r2[..100]));
    }
}
