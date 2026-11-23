//! A minimal PNG writer for `--screenshot`: 8-bit RGB, no filtering, zlib
//! "stored" (uncompressed) blocks. Dependency-free on purpose (ADR 0023).
//! Format: RFC 2083 / PNG spec; zlib: RFC 1950/1951.

use gbc_core::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH, rgb555_to_888};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Largest payload of one stored deflate block.
const MAX_STORED: usize = 0xFFFF;

pub fn crc32(bytes: impl IntoIterator<Item = u8>) -> u32 {
    let mut crc = !0u32;
    for b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    b << 16 | a
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let crc = crc32(kind.iter().chain(data).copied());
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encodes `w` x `h` 8-bit RGB pixels (`rgb.len() == w * h * 3`).
pub fn encode_rgb(w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    assert_eq!(rgb.len(), w as usize * h as usize * 3);
    // Each scanline is a filter byte (0 = none) then the pixels.
    let mut raw = Vec::with_capacity(rgb.len() + h as usize);
    for row in rgb.chunks(w as usize * 3) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01]; // zlib header: deflate, 32K window, no dict
    let mut blocks = raw.chunks(MAX_STORED).peekable();
    while let Some(b) = blocks.next() {
        z.push(u8::from(blocks.peek().is_none())); // BFINAL, BTYPE = 00
        z.extend_from_slice(&(b.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(b.len() as u16)).to_le_bytes());
        z.extend_from_slice(b);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, truecolor, defaults
    let mut out = SIGNATURE.to_vec();
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

/// Encodes a 160x144 frame at 1:1 scale.
pub fn encode_frame(frame: &Frame) -> Vec<u8> {
    let rgb: Vec<u8> = frame.iter().flat_map(|&p| rgb555_to_888(p)).collect();
    encode_rgb(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32, &rgb)
}

/// Writes `frame` to `path` as a PNG.
pub fn write_frame(path: &std::path::Path, frame: &Frame) -> Result<(), String> {
    std::fs::write(path, encode_frame(frame))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_checksums() {
        // Standard check values.
        assert_eq!(crc32(*b"123456789"), 0xCBF4_3926);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    /// Decodes our own output: signature, chunk CRCs, IHDR, stored-block
    /// zlib stream with Adler-32, and returns (w, h, rgb).
    fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
        assert_eq!(&png[..8], &SIGNATURE);
        let mut pos = 8;
        let (mut w, mut h) = (0, 0);
        let mut idat = Vec::new();
        let mut seen_end = false;
        while pos < png.len() {
            let len = u32::from_be_bytes(png[pos..pos + 4].try_into().unwrap()) as usize;
            let kind = &png[pos + 4..pos + 8];
            let data = &png[pos + 8..pos + 8 + len];
            let crc = u32::from_be_bytes(png[pos + 8 + len..pos + 12 + len].try_into().unwrap());
            assert_eq!(crc, crc32(png[pos + 4..pos + 8 + len].iter().copied()));
            match kind {
                b"IHDR" => {
                    w = u32::from_be_bytes(data[0..4].try_into().unwrap());
                    h = u32::from_be_bytes(data[4..8].try_into().unwrap());
                    assert_eq!(&data[8..], &[8, 2, 0, 0, 0]);
                }
                b"IDAT" => idat.extend_from_slice(data),
                b"IEND" => seen_end = true,
                k => panic!("unexpected chunk {k:?}"),
            }
            pos += 12 + len;
        }
        assert!(seen_end);
        assert_eq!(&idat[..2], &[0x78, 0x01]);
        let mut raw = Vec::new();
        let mut p = 2;
        loop {
            let last = idat[p] & 1 == 1;
            assert_eq!(idat[p] >> 1, 0, "stored block");
            let n = u16::from_le_bytes([idat[p + 1], idat[p + 2]]);
            assert_eq!(u16::from_le_bytes([idat[p + 3], idat[p + 4]]), !n);
            raw.extend_from_slice(&idat[p + 5..p + 5 + n as usize]);
            p += 5 + n as usize;
            if last {
                break;
            }
        }
        assert_eq!(
            u32::from_be_bytes(idat[p..p + 4].try_into().unwrap()),
            adler32(&raw)
        );
        assert_eq!(p + 4, idat.len());
        let stride = w as usize * 3 + 1;
        assert_eq!(raw.len(), stride * h as usize);
        let mut rgb = Vec::new();
        for row in raw.chunks(stride) {
            assert_eq!(row[0], 0, "filter none");
            rgb.extend_from_slice(&row[1..]);
        }
        (w, h, rgb)
    }

    #[test]
    fn round_trips_a_frame_spanning_several_stored_blocks() {
        let mut frame: Box<Frame> = Box::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]);
        for (i, p) in frame.iter_mut().enumerate() {
            *p = (i as u16).wrapping_mul(2654) & 0x7FFF;
        }
        let png = encode_frame(&frame);
        let (w, h, rgb) = decode(&png);
        assert_eq!((w, h), (160, 144));
        // 144 * (160*3+1) = 69,264 bytes: more than one 64 KiB block.
        assert!(png.len() > 69_264);
        let want: Vec<u8> = frame.iter().flat_map(|&p| rgb555_to_888(p)).collect();
        assert_eq!(rgb, want);
    }
}
