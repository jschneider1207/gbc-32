//! The core supply, configured before anything else runs (`__pre_init`).
//!
//! THE item that can lock the board. NUCLEO-H753ZI (MB1364,
//! STM32H753ZIT6). The STM32H753 has no SMPS: its core supply is the LDO or
//! an external regulator in bypass (RM0433 section 6.4, Figure 20 and
//! Table 34 "Supply configuration control"). The MB1364 board powers VCORE
//! from the LDO (UM2407 has no SMPS or bypass option; VCAP has its
//! capacitors only). Table 34, row "LDO supply": LDOEN = 1, BYPASS = 0.
//! PWR_CR3 (RM0433 section 6.8.4, offset 0x0C): BYPASS bit 0, LDOEN bit 1,
//! SCUEN bit 2 ("single write enabled to supply configuration"), bits 7:3
//! reserved, kept as read; reset value 0x06 (rev Y) or 0x46 (rev V), i.e.
//! the default configuration is already the LDO. So the low three bits
//! become 0b010, exactly ST's `HAL_PWREx_ConfigSupply(PWR_LDO_SUPPLY)`
//! (`MODIFY_REG(PWR->CR3, PWR_CR3_SCUEN | PWR_CR3_LDOEN | PWR_CR3_BYPASS,
//! PWR_CR3_LDOEN)`). Section 6.8.4: the low byte "is written once after
//! POR"; an invalid LDOEN/BYPASS combination is ignored, locks the register
//! and leaves ACTVOSRDY low until a power cycle; the write must come before
//! any VOS or clock change. BYPASS = 1 (no core supply on this board) is the
//! value that must never be written.
//!
//! After any reset that is not a power cycle the write is ignored and the
//! value from the first write since power-on stays. The banner prints
//! PWR_CR3, so the configuration in force is always visible.
//!
//! RM0433 section 6.4.1 step 5 and its a/b: until ACTVOSRDY (PWR_CSR1 bit
//! 13, section 6.8.2) reads 1 the system is in "Run*" mode, with no RAM
//! writes and no VOS change. So this runs as cortex-m-rt's `__pre_init`,
//! the first thing after reset, and is written in assembly that uses
//! registers only: no stack, no RAM. The wait is bounded (about 2 M polls,
//! a few hundred ms at 64 MHz; ST's HAL allows 1 s) so that a board on
//! which the flag never rises still reaches the UART and reports
//! `actvosrdy=0`. PWR is at 0x5802_4800 (RM0433 section 2.3.2, Table 8;
//! checked against the PAC in `init`).

core::arch::global_asm!(
    ".section .text.__pre_init, \"ax\", %progbits",
    ".global __pre_init",
    ".type __pre_init, %function",
    ".thumb_func",
    "__pre_init:",
    "    ldr r0, =0x58024800", // PWR
    "    ldr r1, [r0, #0x0C]", // PWR_CR3
    "    bic r1, r1, #0x07",   // SCUEN, LDOEN, BYPASS
    "    orr r1, r1, #0x02",   // LDOEN alone: LDO supply, update locked
    "    str r1, [r0, #0x0C]",
    "    ldr r2, =2000000",
    "1:  ldr r1, [r0, #0x04]", // PWR_CSR1
    "    tst r1, #0x2000",     // ACTVOSRDY
    "    bne 2f",
    "    subs r2, r2, #1",
    "    bne 1b",
    "2:  bx lr",
    "    .ltorg",
    ".size __pre_init, . - __pre_init",
);

/// PWR's base address, as the assembly above uses it.
pub(crate) const PWR_BASE: usize = 0x5802_4800;

/// PWR_CR3's supply bits (SCUEN, LDOEN, BYPASS) and the value this board
/// needs (LDOEN alone).
pub const CR3_SUPPLY_MASK: u32 = 0x07;
/// See [`CR3_SUPPLY_MASK`].
pub const CR3_SUPPLY_LDO: u32 = 0x02;
