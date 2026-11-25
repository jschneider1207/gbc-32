//! Optional Cortex-M4 image for the NUCLEO-H755ZI-Q: it parks the M4 in
//! Sleep (WFI with SLEEPDEEP clear: CSleep, the CPU clock gated, no power
//! domain transition) and never touches a peripheral, a clock, RAM or the
//! hardware semaphore. Flash it at 0x0810_0000 only if the M4 must not run
//! into erased flash (README "Dual core"); the M7 image does not need it.
//!
//! No interrupt is ever enabled, so WFI does not return in practice; if it
//! does (a debug event), the loop sleeps again. Every exception vector
//! points at the same loop.
#![no_std]
#![no_main]

core::arch::global_asm!(
    ".section .vectors, \"a\", %progbits",
    ".global __vectors",
    "__vectors:",
    // Initial MSP: the top of SRAM1 at its CPU2 alias 0x1000_0000 (RM0399
    // section 2.3). Never used: the code below has no stack access.
    "    .word 0x10020000",
    "    .word Reset",
    "    .rept 14",
    "    .word Reset",
    "    .endr",
    ".section .text.Reset, \"ax\", %progbits",
    ".global Reset",
    ".type Reset, %function",
    ".thumb_func",
    "Reset:",
    "1:  wfi",
    "    b 1b",
    ".size Reset, . - Reset",
);

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
