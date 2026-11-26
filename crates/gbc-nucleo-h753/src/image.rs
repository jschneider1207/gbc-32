//! The image's identity: the length and POSIX `cksum` of the flashed image
//! as the image itself reads it from flash, so a host can tell that the
//! board runs the `.bin` it flashed and not an earlier one (the ST-LINK
//! drive has accepted a copy and not programmed it; flash.sh). The
//! banner's first line carries it (`image_bytes=<n> image_cksum=<crc>`),
//! and `cksum < image.bin` on the host gives the same pair.
//!
//! The range is what `objcopy -O binary` writes: from the start of flash to
//! the end of the last load image in flash. With cortex-m-rt's `link.x` and
//! `memory_x::firmware_memory_x` that is the largest of the end of
//! `.rodata` (`__erodata`) and the ends of the load images of `.data`,
//! `.itcm` and `.dtcm_rodata` (a section the placement leaves out has
//! start = end outside flash, and is skipped). build.sh checks that the
//! length equals the `.bin`'s, so a section added to flash later cannot be
//! silently left out.

/// The start of internal flash, where every image is linked.
const FLASH_START: usize = 0x0800_0000;
/// The end of the two flash banks (2 MiB).
const FLASH_END: usize = FLASH_START + 2048 * 1024;

/// The flashed image: its bytes in flash, from [`FLASH_START`] to the end
/// of its last load image.
pub fn bytes() -> &'static [u8] {
    unsafe extern "C" {
        static __erodata: u8;
        static __sdata: u8;
        static __edata: u8;
        static __sidata: u8;
        static __sitcm: u8;
        static __eitcm: u8;
        static __siitcm: u8;
        static __sdtcm_ro: u8;
        static __edtcm_ro: u8;
        static __sidtcm_ro: u8;
    }
    // Only the symbols' addresses are taken, nothing is read through them.
    let a = |p: *const u8| p as usize;
    let load_end = |s: *const u8, e: *const u8, li: *const u8| a(li) + (a(e) - a(s));
    let ends = [
        a(&raw const __erodata),
        load_end(&raw const __sdata, &raw const __edata, &raw const __sidata),
        load_end(&raw const __sitcm, &raw const __eitcm, &raw const __siitcm),
        load_end(
            &raw const __sdtcm_ro,
            &raw const __edtcm_ro,
            &raw const __sidtcm_ro,
        ),
    ];
    let end = ends
        .into_iter()
        .filter(|&e| e > FLASH_START && e <= FLASH_END)
        .max()
        .unwrap_or(FLASH_START);
    // SAFETY: [FLASH_START, end) is inside the internal flash (checked
    // above), which is always mapped and readable, and holds this image;
    // nothing writes flash while the image runs.
    unsafe { core::slice::from_raw_parts(FLASH_START as *const u8, end - FLASH_START) }
}

/// POSIX `cksum` (IEEE Std 1003.1, "cksum"): CRC-32 with the generator
/// 0x04C11DB7, most significant bit first, initial value 0, over the data
/// and then its length (least significant byte first, only as many bytes
/// as it needs), complemented. The host checks with `cksum`.
pub fn cksum(data: &[u8]) -> u32 {
    fn feed(mut c: u32, b: u8) -> u32 {
        c ^= u32::from(b) << 24;
        for _ in 0..8 {
            c = if c & 0x8000_0000 != 0 {
                (c << 1) ^ 0x04C1_1DB7
            } else {
                c << 1
            };
        }
        c
    }
    let mut c = data.iter().fold(0, |c, &b| feed(c, b));
    let mut n = data.len();
    while n != 0 {
        c = feed(c, n as u8);
        n >>= 8;
    }
    !c
}

/// (length, `cksum`) of [`bytes`].
pub fn identity() -> (usize, u32) {
    let b = bytes();
    (b.len(), cksum(b))
}
