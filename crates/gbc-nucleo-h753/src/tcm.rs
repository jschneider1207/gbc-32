//! The TCMs: their control registers and the copy of the TCM sections
//! from their load images in flash (`memory.x` from `tools/memory-x`,
//! ADR 0026, ADR 0036; docs/perf.md "ITCM placement plan").
//!
//! Every image's `memory.x` defines the six symbols, with start = end when
//! its placement leaves a section out (`memory_x::firmware_memory_x`), so
//! the copy needs no placement feature and cannot disagree with the map the
//! image linked.

/// ITCMCR and DTCMCR (Arm Cortex-M7 Devices Generic User Guide, DUI 0646,
/// "Instruction and Data Tightly-Coupled Memory Control Registers"): bit 0
/// EN, bit 1 RMW, bit 2 RETEN, bits 6:3 SZ.
const ITCMCR: *mut u32 = 0xE000_EF90 as *mut u32;
const DTCMCR: *mut u32 = 0xE000_EF94 as *mut u32;

/// (ITCMCR, DTCMCR) as they are now.
pub fn control() -> (u32, u32) {
    // SAFETY: reads of two system control registers of the Cortex-M7.
    unsafe { (ITCMCR.read_volatile(), DTCMCR.read_volatile()) }
}

/// Copies a load image from flash to RAM, word by word, and returns its
/// size in bytes.
///
/// # Safety
/// `[dst, end)` must be a RAM range the linker script reserved for this
/// image, 4-byte aligned, that nothing reads or executes yet, and `src`
/// its load address in flash.
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

/// Fills `.itcm` and `.dtcm_rodata` from flash. Returns (ITCM bytes, DTCM
/// table bytes); (0, 0) for a placement without them. Called once by
/// `init`, before any code placed there runs.
pub(crate) fn place() -> (usize, usize) {
    unsafe extern "C" {
        static mut __sdtcm_ro: u32;
        static __edtcm_ro: u32;
        static __sidtcm_ro: u32;
        static mut __sitcm: u32;
        static __eitcm: u32;
        static __siitcm: u32;
    }
    // SAFETY: the three symbols delimit `.dtcm_rodata` and its load image
    // (memory.x), or are all equal when the placement has no DTCM tables.
    // The section holds only gbc-core lookup tables, and no gbc-core code
    // has run yet.
    let tables = unsafe {
        copy_image(
            &raw mut __sdtcm_ro,
            &raw const __edtcm_ro,
            &raw const __sidtcm_ro,
        )
    };
    // SAFETY: ITCMCR is a system control register of the Cortex-M7;
    // setting EN (already set out of reset on the STM32H7, whose TCMs are
    // enabled at boot) only keeps the ITCM mapped.
    unsafe { ITCMCR.write_volatile(ITCMCR.read_volatile() | 1) };
    // SAFETY: the three symbols delimit `.itcm` and its load image
    // (memory.x), or are all equal without an ITCM set. Only gbc-core
    // functions and the memory routines they call are placed there, and
    // none has been called yet.
    let code = unsafe { copy_image(&raw mut __sitcm, &raw const __eitcm, &raw const __siitcm) };
    // The copied code is fetched through the instruction side: complete
    // the stores before anything branches there (Armv7-M ARM A3.7.3).
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
    (code, tables)
}
