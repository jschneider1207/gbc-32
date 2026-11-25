//! Fills the TCM sections of the chosen placement from their load images
//! in flash (build.rs's memory.x; docs/perf.md "ITCM placement plan").
//! Shared with the QEMU image, where the "ITCM" is a far-away piece of
//! RAM, so the copy and the long-branch thunks between the two code
//! regions are exercised before hardware.

/// Copies a load image from flash to RAM, word by word, and returns its
/// size in bytes.
///
/// # Safety
/// `[dst, end)` must be a RAM range the linker script reserved for this
/// image, 4-byte aligned, that nothing reads or executes yet, and `src`
/// its load address in flash.
#[cfg(feature = "dtcm")]
unsafe fn copy_image(mut dst: *mut u32, end: *const u32, mut src: *const u32) -> usize {
    let mut n = 0;
    while dst.cast_const() < end {
        // SAFETY: by the caller's contract both pointers are inside their
        // ranges. Volatile, so the loop is not turned into a `memcpy` call
        // (which might itself live in the range being filled).
        unsafe {
            dst.write_volatile(src.read_volatile());
            dst = dst.add(1);
            src = src.add(1);
        }
        n += 4;
    }
    n
}

/// Fills the TCM sections of the chosen placement. Returns (ITCM bytes,
/// DTCM table bytes).
pub fn place() -> (usize, usize) {
    #[cfg(feature = "dtcm")]
    let tables = {
        unsafe extern "C" {
            static mut __sdtcm_ro: u32;
            static __edtcm_ro: u32;
            static __sidtcm_ro: u32;
        }
        // SAFETY: the three symbols delimit `.dtcm_rodata` and its load
        // image (build.rs's memory.x). The section holds only gbc-core
        // lookup tables, and no gbc-core code has run yet.
        unsafe {
            copy_image(
                &raw mut __sdtcm_ro,
                &raw const __edtcm_ro,
                &raw const __sidtcm_ro,
            )
        }
    };
    #[cfg(not(feature = "dtcm"))]
    let tables = 0;

    #[cfg(feature = "itcm")]
    let code = {
        unsafe extern "C" {
            static mut __sitcm: u32;
            static __eitcm: u32;
            static __siitcm: u32;
        }
        /// ITCMCR (Arm Cortex-M7 Devices Generic User Guide, DUI 0646,
        /// "Instruction and Data Tightly-Coupled Memory Control
        /// Registers"): bit 0 EN.
        #[cfg(not(feature = "qemu"))]
        const ITCMCR: *mut u32 = 0xE000_EF90 as *mut u32;
        // SAFETY: ITCMCR is a system control register of the Cortex-M7;
        // setting EN (already set out of reset on the STM32H7, whose TCMs
        // are enabled at boot) only keeps the ITCM mapped. Not on QEMU,
        // whose "ITCM" is ordinary RAM.
        #[cfg(not(feature = "qemu"))]
        unsafe {
            ITCMCR.write_volatile(ITCMCR.read_volatile() | 1)
        };
        // SAFETY: the three symbols delimit `.itcm` and its load image
        // (build.rs's memory.x). Only gbc-core functions are placed there,
        // and none has been called yet.
        unsafe { copy_image(&raw mut __sitcm, &raw const __eitcm, &raw const __siitcm) }
    };
    #[cfg(not(feature = "itcm"))]
    let code = 0;

    (code, tables)
}
