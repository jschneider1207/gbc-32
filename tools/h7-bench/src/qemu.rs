//! The QEMU mps2-an500 smoke run (`--no-default-features --features
//! qemu`): the bench's frame loop with semihosting output and no board
//! code, so it links no gbc-nucleo-h753 lib (whose supply setup, clocks and
//! fault handlers are the H753's). What it needs of the lib's services is
//! here, cut to QEMU: the TCM copy (QEMU's "ITCM" is a far-away piece of
//! RAM, so the copy and the long-branch thunks between the two code
//! regions are exercised before hardware), the stack depth and a panic
//! handler.

use cortex_m_semihosting::{debug, hio};
// cortex-m-rt (with the `device` feature the PAC turns on) wants the PAC's
// interrupt vector table.
use stm32h7 as _;

use crate::{bench, workload};

/// The same calls as the board's measurement, printed through semihosting.
/// QEMU does not model the cycle counter: the cycle fields are 0, as on the
/// host, and only calls/frames/clocks/hash are compared.
pub fn run(mut b: bench::Buffers) -> ! {
    place();
    if let Ok(mut out) = hio::hstdout() {
        for i in 0..workload::WORKLOADS.len() {
            let r = bench::run(i, &mut b, || 0, |_| ());
            workload::report_line(&mut out, i, &r);
        }
    }
    // On stderr: stdout is compared with expected-hashes.txt.
    if let Ok(mut err) = hio::hstderr() {
        use core::fmt::Write;
        let (used, size) = stack_usage();
        let _ = writeln!(err, "h7bench v1 stack used={used} of={size}");
    }
    debug::exit(debug::EXIT_SUCCESS);
    loop {
        cortex_m::asm::nop();
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    debug::exit(debug::EXIT_FAILURE);
    loop {
        cortex_m::asm::nop();
    }
}

/// Copies a load image from flash to RAM, word by word.
///
/// # Safety
/// `[dst, end)` must be a RAM range the linker script reserved for this
/// image, 4-byte aligned, that nothing reads or executes yet, and `src`
/// its load address in flash.
#[cfg(feature = "dtcm")]
unsafe fn copy_image(mut dst: *mut u32, end: *const u32, mut src: *const u32) {
    while dst.cast_const() < end {
        // SAFETY: by the caller's contract both pointers are inside their
        // ranges. Volatile, so the loop is not turned into a `memcpy` call
        // (which might itself live in the range being filled).
        unsafe {
            dst.write_volatile(src.read_volatile());
            dst = dst.add(1);
            src = src.add(1);
        }
    }
}

/// Fills the TCM sections of the placement from flash (build.rs's
/// memory.x, `memory_x::memory_x`, which defines their symbols only when
/// the placement has them).
fn place() {
    #[cfg(feature = "dtcm")]
    {
        unsafe extern "C" {
            static mut __sdtcm_ro: u32;
            static __edtcm_ro: u32;
            static __sidtcm_ro: u32;
        }
        // SAFETY: the three symbols delimit `.dtcm_rodata` and its load
        // image. The section holds only gbc-core lookup tables, and no
        // gbc-core code has run yet.
        unsafe {
            copy_image(
                &raw mut __sdtcm_ro,
                &raw const __edtcm_ro,
                &raw const __sidtcm_ro,
            )
        };
    }
    #[cfg(feature = "itcm")]
    {
        unsafe extern "C" {
            static mut __sitcm: u32;
            static __eitcm: u32;
            static __siitcm: u32;
        }
        // SAFETY: the three symbols delimit `.itcm` and its load image.
        // Only gbc-core functions are placed there, and none has been
        // called yet.
        unsafe { copy_image(&raw mut __sitcm, &raw const __eitcm, &raw const __siitcm) };
        // The copied code is fetched through the instruction side.
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
    }
}

/// (deepest stack use so far in bytes, size of the stack region in bytes),
/// from the paint cortex-m-rt's startup code leaves (`paint-stack`): the
/// stack region starts out filled with 0xCCCC_CCCC.
fn stack_usage() -> (usize, usize) {
    const PAINT: u32 = 0xCCCC_CCCC;
    unsafe extern "C" {
        static _stack_end: u32;
        static _stack_start: u32;
    }
    let (end, start) = (&raw const _stack_end, &raw const _stack_start);
    let mut p = end;
    // SAFETY: `_stack_end` and `_stack_start` are cortex-m-rt's linker
    // symbols for the bounds of the stack region, word-aligned RAM that is
    // always readable; the words below the current stack pointer are not
    // otherwise accessed, and a volatile read of them is harmless.
    unsafe {
        while p < start && p.read_volatile() == PAINT {
            p = p.add(1);
        }
    }
    (start as usize - p as usize, start as usize - end as usize)
}
