//! Board bring-up: supply, clock, LEDs, the ST-LINK virtual COM port,
//! caches, TCM placement, the cycle counter and the fault handlers. Two
//! boards, chosen by a cargo feature (README.md "Boards"):
//!
//! - `h753`: NUCLEO-H753ZI (MB1364, STM32H753ZIT6), single core, LDO
//!   supply. Sources: UM2407 Rev 6 (the board), RM0433 Rev 9 (the MCU),
//!   DS12117 Rev 10 (the STM32H753xI datasheet).
//! - `h755`: NUCLEO-H755ZI-Q (MB1363, STM32H755ZIT6), dual core, direct
//!   SMPS supply; Cortex-M7 only, see README.md "Dual core" for what the
//!   Cortex-M4 does meanwhile. Sources: UM2408 Rev 6, RM0399 Rev 5,
//!   DS12919 Rev 3.
//!
//! LEDs, the UART pins and every register this file touches outside
//! PWR_CR3 are at the same places on both (UM2407 sections 7.6.1 and
//! 7.6.5; RM0433 and RM0399 register maps).

#[cfg(all(feature = "h753", feature = "h755"))]
compile_error!("features h753 and h755 exclude each other");
#[cfg(not(any(feature = "h753", feature = "h755")))]
compile_error!("one board feature is needed: h753 or h755");
#[cfg(all(feature = "pll", feature = "pll480"))]
compile_error!("features pll and pll480 exclude each other");
// The H755 board's SMPS supply rules out VOS0 (DS12919 Table 14 note 1).
#[cfg(all(feature = "pll480", feature = "h755"))]
compile_error!("feature pll480 needs the h753 board (VOS0 needs the LDO supply)");

use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use cortex_m::peripheral::{DWT, SCB};
use cortex_m_rt::exception;
#[cfg(feature = "h755")]
use stm32h7::stm32h747cm7 as pac;
#[cfg(feature = "h753")]
use stm32h7::stm32h753v as pac;

#[cfg(feature = "h753")]
const BOARD: &str = "NUCLEO-H753ZI";
#[cfg(feature = "h755")]
const BOARD: &str = "NUCLEO-H755ZI-Q";

// --- Supply --------------------------------------------------------------
//
// THE item that can lock the board. The two boards need different values:
// the bits of PWR_CR3's low byte do not even mean the same on the two
// chips (bit 2 is SDEN on the H755 and SCUEN on the H753).
//
// NUCLEO-H753ZI (`h753`). The STM32H753 has no SMPS: its core supply is the
// LDO or an external regulator in bypass (RM0433 section 6.4, Figure 20 and
// Table 34 "Supply configuration control"). The MB1364 board powers VCORE
// from the LDO (UM2407 has no SMPS or bypass option; VCAP has its
// capacitors only). Table 34, row "LDO supply": LDOEN = 1, BYPASS = 0.
// PWR_CR3 (RM0433 section 6.8.4, offset 0x0C): BYPASS bit 0, LDOEN bit 1,
// SCUEN bit 2 ("single write enabled to supply configuration"), bits 7:3
// reserved, kept as read; reset value 0x06 (rev Y) or 0x46 (rev V), i.e.
// the default configuration is already the LDO. So the low three bits
// become 0b010, exactly ST's `HAL_PWREx_ConfigSupply(PWR_LDO_SUPPLY)`
// (`MODIFY_REG(PWR->CR3, PWR_CR3_SCUEN | PWR_CR3_LDOEN | PWR_CR3_BYPASS,
// PWR_CR3_LDOEN)`). Section 6.8.4: the low byte "is written once after
// POR"; an invalid LDOEN/BYPASS combination is ignored, locks the register
// and leaves ACTVOSRDY low until a power cycle; the write must come before
// any VOS or clock change. BYPASS = 1 (no core supply on this board) is the
// value that must never be written. Section 6.4.1 step 5 and its a/b: no
// RAM writes and no VOS change until ACTVOSRDY (PWR_CSR1 bit 13, section
// 6.8.2) reads 1; PWR at 0x5802_4800 (section 2.3.2, Table 8).
//
// NUCLEO-H755ZI-Q (`h755`). UM2408 section 7.4.8, Table 10: the
// board's default is "Supply config 2: Internal SMPS only" (SMPS on, LDO
// off), and its warning says the firmware must select the matching
// configuration, `PWR_DIRECT_SMPS_SUPPLY` in ST's HAL, or "after the reset,
// STLINK cannot connect to the target anymore" until a BOOT0 recovery.
//
// RM0399 section 7.4 Table 33 "Supply configuration control", row 2
// "Direct SMPS step-down converter supply": SDLEVEL = x, SDEXTHP = 0,
// SDEN = 1, LDOEN = 0, BYPASS = 0. PWR_CR3 (section 7.8.4, offset 0x0C)
// has BYPASS at bit 0, LDOEN bit 1, SDEN bit 2, SDEXTHP bit 3, SDLEVEL bits
// 5:4; its reset value is 0x..06 (SDEN and LDOEN: the "default
// configuration", in which the chip starts on either board wiring). So the
// low six bits become 0b00_0100, exactly ST's
// `MODIFY_REG(PWR->CR3, PWR_SUPPLY_CONFIG_MASK, PWR_CR3_SMPSEN)`; bits 7:6
// are reserved ("must be kept at reset value", and bit 6 differs between
// silicon revisions) and are written back as read.
//
// "The lower byte of this register is written once after POR": after any
// reset that is not a power cycle the write below is ignored and the value
// from the first write since power-on stays. The banner prints PWR_CR3, so
// the configuration in force is always visible.
//
// RM0399 section 7.4.1 step 5: until ACTVOSRDY (PWR_CSR1 bit 13) reads 1
// the system is in "Run*" mode and "write accesses to the RAMs are not
// permitted". So this runs as cortex-m-rt's `__pre_init`, the first thing
// after reset, and is written in assembly that uses registers only: no
// stack, no RAM. The wait is bounded (about 2 M polls, a few hundred ms at
// 64 MHz; ST's HAL allows 1 s) so that a board on which the flag never
// rises still reaches the UART and reports `actvosrdy=0`.
//
// PWR is at 0x5802_4800 (RM0399 section 2.3, memory map; checked against
// the PAC in `init`).
#[cfg(feature = "h755")]
core::arch::global_asm!(
    ".section .text.__pre_init, \"ax\", %progbits",
    ".global __pre_init",
    ".type __pre_init, %function",
    ".thumb_func",
    "__pre_init:",
    "    ldr r0, =0x58024800", // PWR
    "    ldr r1, [r0, #0x0C]", // PWR_CR3
    "    bic r1, r1, #0x3F",   // SDLEVEL, SDEXTHP, SDEN, LDOEN, BYPASS
    "    orr r1, r1, #0x04",   // SDEN alone: direct SMPS supply
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

// The H753 version: the same sequence with RM0433's bits (see above).
#[cfg(feature = "h753")]
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

const PWR_BASE: usize = 0x5802_4800;
/// PWR_CR3 supply bits (see above), the value this board needs, and its
/// name in the banner.
#[cfg(feature = "h755")]
const CR3_SUPPLY: (u32, u32, &str) = (0x3F, 0x04, "direct-smps");
#[cfg(feature = "h753")]
const CR3_SUPPLY: (u32, u32, &str) = (0x07, 0x02, "ldo");

// --- Clock ---------------------------------------------------------------

/// Phase A (default): the 64 MHz HSI the chip resets to (RM0399 section
/// 9.5.6 and 9.7.2, RM0433 section 8.5.6: the HSI is the system clock after
/// reset, HSIDIV /1), every prescaler /1, voltage scale VOS3 and the flash
/// latency left at their reset values. Nothing clock-related is written.
/// VOS3 allows 200 MHz on the H753 (DS12117 Table 111) and 64 MHz is far
/// inside the H755's VOS3 range too.
#[cfg(not(any(feature = "pll", feature = "pll480")))]
pub const CPU_HZ: u32 = 64_000_000;
#[cfg(not(any(feature = "pll", feature = "pll480")))]
const CLOCK_NAME: &str = "hsi64";

/// Phase B (`--features pll`): PLL1 from HSI, 64 MHz / 5 x 61.25 / 2 =
/// 392 MHz (fractional mode, see `PLL_REF_DIV`). DS12919 Table 13 and Table 14: with the SMPS supplying VCORE
/// the highest scale is VOS1 and its limit is 400 MHz (Table 14 note 1:
/// "VOS0 (power scale 0) is not available when the SMPS step-down converter
/// directly supplies VCORE"), so 480 MHz is out of reach on this board.
/// 392 and not 400: the HSI is the reference (no dependency on the
/// ST-LINK's 8 MHz MCO) and its tolerance of about 1% must stay under the
/// limit: at the HSI's upper bound (see `PLL_REF_DIV`) the CPU runs at
/// 397.9 MHz, AXI 198.9 MHz, APB 99.5 MHz, VCO 795.8 MHz (rev Y's wide VCO
/// stops at 836 MHz, DS12117 Table 39; rev V's at 960 MHz, Table 136).
///
/// H753 (LDO supply): DS12117 Rev 10 Table 111 (rev V): VOS1 allows a
/// 400 MHz CPU and a 200 MHz AHB/AXI clock (VOS0, 480 MHz, only on rev V);
/// rev Y's limit is 400 MHz at VOS1 too (DS12117 feature list note 3). So
/// the same 392 MHz tree at VOS1 is valid on either revision, with the same
/// flash setting (RM0433 Table 17: AXI in ]185; 210] MHz at VOS1 is 2 wait
/// states, WRHIGHFREQ = 0b10). PWR_D3CR VOS: RM0433 section 6.8.6, 0b11 =
/// scale 1. VOS0 is not used: on the H753 it also needs SYSCFG ODEN.
#[cfg(feature = "pll")]
pub const CPU_HZ: u32 = 392_000_000;
#[cfg(feature = "pll")]
const CLOCK_NAME: &str = "pll392";

/// Phase C (`--features pll480`, H753 only): VOS0, nominally 472 MHz, so
/// that the CPU never exceeds the STM32H753's 480 MHz maximum. DS12117
/// Rev 10 Table 111 (rev V): VOS0 allows a 480 MHz CPU, a 240 MHz AHB/AXI
/// clock and 120 MHz APB clocks; note 4: VOS0 only with the LDO on (this
/// board); note 5 and Table 112: TJ at most 105 °C. ES0392 section 2.2.21:
/// no VOS0 on revisions Y and W; this board is revision V
/// (`idcode=0x20036450`). The feature keeps its name ("the VOS0 clock").
///
/// Reference: the HSI, not the ST-LINK's MCO. The first version ran PLL1
/// from the HSE in bypass (8 MHz MCO, UM2407 section 7.5.1) at 8 / 2 x 240
/// / 2 = 480 MHz; the `clockcheck-pll480` image measured that against the
/// LSE crystal (2026-11-25) at 483.86 MHz (+0.80%, MCO 8.064 MHz, wandering
/// by +-0.05% between 2 s windows), over the CPU and AXI maxima. The MCO
/// has no stated tolerance (UM2407: "fixed at 8 MHz", changeable by an
/// ST-LINK firmware upgrade) and behaves like an RC oscillator, so no
/// margin on it is provable. The HSI has one: DS12117 Table 133 (rev V),
/// 63.7 to 64.3 MHz at 3.3 V and TJ 30 °C (production tested), drift +1%
/// at most for TJ -20 to 105 °C and +0.03% at most over VDD: at most
/// 64.3 x 1.0103 = 64.96 MHz (+1.5%). The board's HSI measured
/// 64.230 MHz (+0.36%).
///
/// PLL1: 64 MHz / DIVM1 5 = 12.8 MHz reference (see `PLL_REF_DIV`),
/// x 73.75 (fractional mode: DIVN1 73, FRACN1 6144 / 8192) = 944 MHz VCO
/// (wide VCO, 192 to 960 MHz, RM0433 section 8.7.12), / DIVP1 2 =
/// 472 MHz; AXI 236 MHz, APB 118 MHz. At the HSI's upper bound
/// (64.96 MHz): reference 12.99 MHz, VCO 958.2 MHz, CPU 479.1 MHz, AXI
/// 239.6 MHz, all under their limits. The next integer step, x 60 from a
/// 16 MHz reference, is the 480 MHz that leaves no margin at all. Flash
/// wait states: AXI in ]225; 240] MHz at VOS0 is 4 (RM0433 Table 17), and
/// 4 stay valid (more than needed) down to the HSI's lower bound (AXI
/// 229.9 MHz).
#[cfg(feature = "pll480")]
pub const CPU_HZ: u32 = 472_000_000;
#[cfg(feature = "pll480")]
const CLOCK_NAME: &str = "pll472";

const BAUD: u32 = 115_200;

#[cfg(feature = "itcm")]
const PLACEMENT: &str = "itcm-dtcm";
#[cfg(all(feature = "dtcm", not(feature = "itcm")))]
const PLACEMENT: &str = "flash-dtcm";
#[cfg(not(feature = "dtcm"))]
const PLACEMENT: &str = "flash-axi";

/// PLL1's reference divider (DIVM1) for both PLL images, and the
/// fractional multipliers that keep the nominal frequencies.
///
/// The reference must stay inside the PLL1RGE band at every HSI frequency
/// the part allows. DS12117 Table 133 (rev V): 63.7 to 64.3 MHz at 3.3 V
/// and TJ 30 °C, drift over temperature -2% to +1% (TJ -40 to 105 °C),
/// over VDD -0.12% to +0.03%: 62.35 to 64.96 MHz. The first version used
/// DIVM1 4, a 16 MHz reference at nominal: exactly the top of the band
/// (RM0433 section 8.7.12, PLL1RGE 0b11: 8 to 16 MHz; DS12117 Table 136:
/// fPLL_IN 2 to 16 MHz), and over it on this board (HSI measured
/// 64.228 MHz: 16.06 MHz). DIVM1 5 gives 12.47 to 12.99 MHz over the whole
/// tolerance (12.85 MHz on this board), well inside 8 to 16 MHz, with the
/// VCO selection unchanged (RM0433 section 8.5.5: above 2 MHz, the wide
/// VCO).
///
/// x 61.25 and x 73.75 are not integers, so PLL1 runs in fractional mode:
/// VCO = ref x (DIVN1 + FRACN1 / 2^13) (RM0433 section 8.7.14), the
/// DIVN1 field holding N - 1 (section 8.7.13). The ratios equal the old
/// integer settings' (61.25 / 5 = 49 / 4, 73.75 / 5 = 59 / 4), so every
/// nominal frequency is unchanged. DS12117 Table 136: sigma-delta mode
/// locks within 166 us (CKIN >= 8 MHz), and its long-term jitter is
/// +-0.8% against +-0.7% in integer mode: a spread of the period, not a
/// shift of the mean frequency, which is what the margins are about.
#[cfg(any(feature = "pll", feature = "pll480"))]
const PLL_REF_DIV: u8 = 5;
/// 392 MHz: x 61.25, DIVN1 field 60 (N 61), FRACN1 0.25 x 8192.
#[cfg(feature = "pll")]
const PLL_N_FRAC: (u16, u16) = (61 - 1, 2048);
/// 472 MHz: x 73.75, DIVN1 field 72 (N 73), FRACN1 0.75 x 8192.
#[cfg(feature = "pll480")]
const PLL_N_FRAC: (u16, u16) = (73 - 1, 6144);

/// Programs PLL1 (off after reset) from the HSI as `PLL_REF_DIV` and
/// `PLL_N_FRAC` say, P output /2 only, and turns it on. RM0433 section
/// 8.5.5, "PLL initialization phase" and Figure 48 (fractional mode):
/// source and DIVM1; FRACN1; PLLCFGR with PLL1FRACEN = 1 (its 0 to 1 edge
/// latches FRACN1 into the modulator, section 8.7.12; it is 0 after reset
/// with the PLL off, so the edge is this write); the dividers; PLL1ON.
/// Returns whether PLL1RDY rose.
#[cfg(any(feature = "pll", feature = "pll480"))]
fn pll1_on(rcc: &pac::RCC, tries: u32) -> bool {
    // Source HSI (64 MHz, HSIDIV /1 at reset), DIVM1 (RM0433 section
    // 8.7.11). The HSE is not used (see `CPU_HZ`).
    rcc.pllckselr
        .modify(|_, w| w.pllsrc().hsi().divm1().bits(PLL_REF_DIV));
    rcc.pll1fracr.write(|w| w.fracn1().bits(PLL_N_FRAC.1));
    // Reference range 8 to 16 MHz (PLL1RGE = 0b11), wide VCO (192 to
    // 960 MHz), fractional latch on, only the P output (RM0433 section
    // 8.7.12).
    rcc.pllcfgr.modify(|_, w| {
        w.pll1rge()
            .range8()
            .pll1vcosel()
            .wide_vco()
            .pll1fracen()
            .set()
            .divp1en()
            .enabled()
            .divq1en()
            .disabled()
            .divr1en()
            .disabled()
    });
    // DIVN1 field = N - 1, DIVP1 field = P - 1 = 1 (/2). RM0433 section
    // 8.7.13.
    // SAFETY: documented field values (DIVN1 4 to 512, DIVP1 /2).
    rcc.pll1divr
        .modify(|_, w| unsafe { w.divn1().bits(PLL_N_FRAC.0).divp1().bits(1) });
    rcc.cr.modify(|_, w| w.pll1on().set_bit());
    wait_for(tries, || rcc.cr.read().pll1rdy().bit_is_set())
}

/// Polls `ready` up to `tries` times.
fn wait_for(tries: u32, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..tries {
        if ready() {
            return true;
        }
    }
    false
}

/// Phase B clock tree. Returns false, leaving the system clock on the HSI,
/// if a ready flag did not rise (the UART is then at the wrong baud rate,
/// so the fault blink is the visible sign: `init` panics).
///
/// Order as RM0399 requires: supply valid, then voltage scale up, then the
/// PLL, then the prescalers, then the switch; the flash latency is lowered
/// from its reset value only after the switch.
#[cfg(feature = "pll")]
fn clock_up(pwr: &pac::PWR, rcc: &pac::RCC, flash: &pac::FLASH) -> bool {
    const TRIES: u32 = 4_000_000;
    // RM0399 section 7.4.1: VOS may only change once ACTVOSRDY is set.
    if !pwr.csr1.read().actvosrdy().bit_is_set() {
        return false;
    }
    // PWR_D3CR VOS = 0b11, scale 1 (RM0399 section 7.8.7), then VOSRDY.
    // SAFETY: 0b11 is a documented value of the 2-bit VOS field.
    pwr.d3cr.modify(|_, w| unsafe { w.vos().bits(0b11) });
    if !wait_for(TRIES, || pwr.d3cr.read().vosrdy().bit_is_set()) {
        return false;
    }
    // PLL1: 12.8 MHz reference x 61.25 = VCO 784 MHz, /2 = 392 MHz (see
    // `PLL_REF_DIV`; RM0399 sections 9.7.10 to 9.7.13 describe the same
    // PLL1 registers for the H755).
    if !pll1_on(rcc, TRIES) {
        return false;
    }
    // CPU 392 MHz (D1CPRE /1); AXI/AHB 196 MHz (HPRE /2: limit 200, and it
    // is also the Cortex-M4's clock); every APB 98 MHz (limit 100).
    // DS12919 Table 13.
    rcc.d1cfgr
        .modify(|_, w| w.d1cpre().div1().hpre().div2().d1ppre().div2());
    rcc.d2cfgr
        .modify(|_, w| w.d2ppre1().div2().d2ppre2().div2());
    rcc.d3cfgr.modify(|_, w| w.d3ppre().div2());
    // The flash latency is still the reset value (7 wait states), valid at
    // any frequency, so the switch is safe.
    rcc.cfgr.modify(|_, w| w.sw().pll1());
    if !wait_for(TRIES, || rcc.cfgr.read().sws().is_pll1()) {
        return false;
    }
    // RM0399 Table 16: AXI clock in ]185; 210] MHz at VOS1 needs 2 wait
    // states with WRHIGHFREQ = 0b10.
    // SAFETY: documented values of the LATENCY and WRHIGHFREQ fields.
    flash
        .acr
        .write(|w| unsafe { w.latency().bits(2).wrhighfreq().bits(2) });
    wait_for(TRIES, || flash.acr.read().latency().bits() == 2)
}

/// What `clock_up_480` saw of the VOS0 ramp, for the banner: SYSCFG_PWRCR
/// before ODEN was written; the polls until VOSRDY or ACTVOSRDY first read
/// 0 after it (`u32::MAX`: never seen low); the polls that then read
/// not-ready until both were 1 again.
#[cfg(feature = "pll480")]
static VOS0_DIAG: [core::sync::atomic::AtomicU32; 3] = [
    core::sync::atomic::AtomicU32::new(0),
    core::sync::atomic::AtomicU32::new(0),
    core::sync::atomic::AtomicU32::new(0),
];

/// Phase C clock tree (see [`CPU_HZ`]). On a failed wait it returns the
/// step's name, leaving the system clock on the HSI (or, if only the final
/// read-backs failed, wherever the RCC reports it: `pclk1_hz` follows
/// RCC_CFGR.SWS), and `init` panics with it once the UART is set up.
///
/// Order: supply valid; VOS1 then VOS0 (voltage before frequency, RM0433
/// section 6.8.6 VOS); PLL1; flash wait states up (before the faster
/// clock, RM0433 section 4.3.8 "Adjusting system frequency"); prescalers;
/// the switch.
#[cfg(feature = "pll480")]
fn clock_up_480(
    pwr: &pac::PWR,
    rcc: &pac::RCC,
    flash: &pac::FLASH,
    syscfg: &pac::SYSCFG,
) -> Result<(), &'static str> {
    const TRIES: u32 = 4_000_000;
    // RM0433 section 6.4.1: no VOS change before ACTVOSRDY.
    if !pwr.csr1.read().actvosrdy().bit_is_set() {
        return Err("actvosrdy");
    }
    // RM0433 section 6.6.2 "VOS0 activation/deactivation sequence":
    // 1. VOS1 in PWR_D3CR (VOS = 0b11, section 6.8.6), here also waited
    //    for; 2. SYSCFGEN in RCC_APB4ENR (section 8.7.48); 3. ODEN in
    //    SYSCFG_PWRCR (section 12.3.10: "must be written only in VOS1");
    //    4. wait for VOSRDY. Only then may the frequency go up. PWR_D3CR's
    //    VOS field keeps 0b11 at VOS0 (it has no VOS0 code) and so does
    //    PWR_CSR1.ACTVOS: ODEN = 1 with VOSRDY = 1 is what says VOS0.
    // SAFETY: 0b11 is a documented value of the 2-bit VOS field.
    pwr.d3cr.modify(|_, w| unsafe { w.vos().bits(0b11) });
    if !wait_for(TRIES, || pwr.d3cr.read().vosrdy().bit_is_set()) {
        return Err("vos1 vosrdy");
    }
    rcc.apb4enr.modify(|_, w| w.syscfgen().set_bit());
    let _ = rcc.apb4enr.read(); // the clock is on before the first access
    VOS0_DIAG[0].store(syscfg.pwrcr.read().bits(), Ordering::Relaxed);
    syscfg.pwrcr.modify(|_, w| w.oden().set_bit());
    if !syscfg.pwrcr.read().oden().bit_is_set() {
        return Err("oden write");
    }
    // RM0433 Figure 31: VOSRDY and ACTVOSRDY drop a little after ODEN
    // rises and come back once VCORE has ramped from 1.26 to 1.35 V, about
    // 20 us later. So the ready flags are still VOS1's for a moment after
    // the write: a plain "wait for VOSRDY" (step 4, and ST's HAL) can pass
    // at once, before the ramp (seen on the board, 2026-11-25: 0 polls
    // low). First wait, bounded and not fatal, for a flag to drop; then for
    // both to be 1 again.
    let ready =
        || pwr.d3cr.read().vosrdy().bit_is_set() && pwr.csr1.read().actvosrdy().bit_is_set();
    let mut to_drop = 0u32;
    let dropped = wait_for(100_000, || {
        to_drop += 1;
        !ready()
    });
    VOS0_DIAG[1].store(if dropped { to_drop } else { u32::MAX }, Ordering::Relaxed);
    let mut low = 0u32;
    let vos0 = wait_for(TRIES, || {
        let r = ready();
        low += u32::from(!r);
        r
    });
    VOS0_DIAG[2].store(low, Ordering::Relaxed);
    if !vos0 {
        return Err("vos0 vosrdy");
    }
    // And in any case at least 100 us since ODEN (64 MHz HSI: 6,400
    // cycles; `asm::delay` takes at least that many), five times the ramp
    // of Figure 31, so the frequency never goes up on the flags alone.
    cortex_m::asm::delay(6_400);
    // PLL1: 12.8 MHz reference x 73.75 = VCO 944 MHz, /2 = 472 MHz (see
    // `PLL_REF_DIV`).
    if !pll1_on(rcc, TRIES) {
        return Err("pll1 lock");
    }
    // RM0433 Table 17: AXI clock in ]225; 240] MHz at VOS0 (236 MHz) is 4
    // wait states with WRHIGHFREQ = 0b10 (FLASH_ACR 0x24). Section 4.3.8: set
    // and read back before the clock goes up; more wait states than needed
    // are harmless on the 64 MHz HSI meanwhile.
    // SAFETY: documented values of the LATENCY and WRHIGHFREQ fields.
    flash
        .acr
        .write(|w| unsafe { w.latency().bits(4).wrhighfreq().bits(0b10) });
    if !wait_for(TRIES, || {
        let acr = flash.acr.read();
        acr.latency().bits() == 4 && acr.wrhighfreq().bits() == 0b10
    }) {
        return Err("flash latency");
    }
    // CPU 472 MHz (D1CPRE /1); AXI/AHB 236 MHz (HPRE /2); every APB
    // 118 MHz (D1PPRE, D2PPRE1/2, D3PPRE /2). DS12117 Table 111, VOS0.
    // RM0433 sections 8.7.8 to 8.7.10.
    rcc.d1cfgr
        .modify(|_, w| w.d1cpre().div1().hpre().div2().d1ppre().div2());
    rcc.d2cfgr
        .modify(|_, w| w.d2ppre1().div2().d2ppre2().div2());
    rcc.d3cfgr.modify(|_, w| w.d3ppre().div2());
    // RM0433 section 8.7.7: SW = PLL1, then SWS reports it.
    rcc.cfgr.modify(|_, w| w.sw().pll1());
    if !wait_for(TRIES, || rcc.cfgr.read().sws().is_pll1()) {
        return Err("switch to pll1");
    }
    Ok(())
}

// --- LEDs ----------------------------------------------------------------
//
// UM2408 section 7.6: LD1 green on PB0 (default solder bridges: SB54 on,
// SB65 off), LD2 yellow on PE1, LD3 red on PB14; "on when the I/O is HIGH".
// UM2407 section 7.6.1 (H753 board): the same pins, LD1 on PB0 with SB39
// on and SB47 off.
// LD1 is the liveness sign, LD2 is on once `init` is through, LD3 means a
// fault.

static LED_ON: AtomicBool = AtomicBool::new(false);

fn ld1(on: bool) {
    // SAFETY: a single write to the write-only GPIOB_BSRR, which exists to
    // make pin updates atomic; nothing else in this firmware owns GPIOB.
    let gpiob = unsafe { &*pac::GPIOB::ptr() };
    gpiob.bsrr.write(|w| {
        if on {
            w.bs0().set_bit()
        } else {
            w.br0().set_bit()
        }
    });
}

/// Toggles LD1 (green).
pub fn led_toggle() {
    let on = !LED_ON.load(Ordering::Relaxed);
    LED_ON.store(on, Ordering::Relaxed);
    ld1(on);
}

// --- Cycle counter ---------------------------------------------------------

/// DWT_CYCCNT: CPU clock cycles, free-running, 32 bits (wraps every 67 s at
/// 64 MHz; one emulated frame is far shorter).
#[inline(always)]
pub fn cycles() -> u32 {
    DWT::cycle_count()
}

/// Busy-waits until `n` cycles have passed since counter value `t0`.
pub fn wait_since(t0: u32, n: u32) {
    while cycles().wrapping_sub(t0) < n {}
}

// --- UART ------------------------------------------------------------------

/// USART3 transmitter, polled. UM2408 section 7.10 and Table 12: PD8
/// (USART3 TX) and PD9 (USART3 RX) go to the ST-LINK virtual COM port by
/// default (SB16 and SB17 on); UM2407 section 7.6.5 and Table 10 (H753
/// board): the same pins, SB19 and SB12 on. DS12919 alternate-function table: both are
/// AF7. 115200 8N1. Only TX is used; PD9 is left alone.
pub struct Uart(());

/// One byte, with a bounded wait for the transmit register, so that a
/// UART that is not running (a fault before `init` set it up) cannot hang
/// the fault handler.
fn uart_byte(b: u8) {
    // SAFETY: shared access to USART3's registers; this firmware has no
    // interrupts and no second thread, so register accesses never overlap.
    let usart = unsafe { &*pac::USART3::ptr() };
    if wait_for(1_000_000, || usart.isr.read().txe().bit_is_set()) {
        usart.tdr.write(|w| w.tdr().bits(u16::from(b)));
    }
}

/// rcc_pclk1 as the clock registers stand (only the values this firmware
/// sets: HSI or PLL1 at `CPU_HZ`, HPRE and D2PPRE1 /1 or /2), so the fault
/// path gets the baud rate right wherever the clock setup stopped. No
/// image uses the HSE, so SWS is HSI or PLL1 in every image.
fn pclk1_hz(rcc: &pac::rcc::RegisterBlock) -> u32 {
    let sys = if rcc.cfgr.read().sws().is_pll1() {
        CPU_HZ
    } else {
        64_000_000
    };
    let hpre = u32::from(rcc.d1cfgr.read().hpre().bits() & 0b1000 != 0);
    let ppre = u32::from(rcc.d2cfgr.read().d2ppre1().bits() & 0b100 != 0);
    sys >> hpre >> ppre
}

/// Sets USART3 up for transmission; safe to repeat. Returns false if the
/// D2 domain's clocks did not come up.
fn uart_init() -> bool {
    // SAFETY: shared access to RCC, GPIOD and USART3; no interrupt and no
    // second thread exists, so register accesses never overlap.
    let (rcc, gpiod, usart) =
        unsafe { (&*pac::RCC::ptr(), &*pac::GPIOD::ptr(), &*pac::USART3::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioden().set_bit());
    // USART3 is in the D2 domain, the Cortex-M4's on the H755. Enabling its
    // clock from this CPU allocates it to CPU1, which keeps D2 clocked
    // whatever the M4 does (RM0399 section 9.5.10 "Peripheral allocation";
    // RM0433 section 8.5.10 on the H753); D2CKRDY (RCC_CR bit 15, both
    // manuals) tells when the domain's clocks run.
    rcc.apb1lenr.modify(|_, w| w.usart3en().set_bit());
    let d2_ok = wait_for(4_000_000, || rcc.cr.read().d2ckrdy().bit_is_set());
    gpiod.afrh.modify(|_, w| w.afr8().af7());
    gpiod.moder.modify(|_, w| w.moder8().alternate());
    // Kernel clock: rcc_pclk1 (RCC_D2CCIP2R USART234578SEL resets to 0).
    // Oversampling by 16: BRR = f / baud, rounded. BRR can only be written
    // while the UART is disabled.
    let brr = (pclk1_hz(rcc) + BAUD / 2) / BAUD;
    usart.cr1.write(|w| w.ue().clear_bit());
    // SAFETY: any 16-bit value of at least 16 is a valid BRR.
    usart.brr.write(|w| unsafe { w.bits(brr) });
    usart.cr1.write(|w| w.ue().set_bit().te().set_bit());
    d2_ok
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(uart_byte);
        Ok(())
    }
}

// --- TCM control -------------------------------------------------------------

/// ITCMCR and DTCMCR (Arm Cortex-M7 Devices Generic User Guide, DUI 0646,
/// "Instruction and Data Tightly-Coupled Memory Control Registers"): bit 0
/// EN, bit 1 RMW, bit 2 RETEN, bits 6:3 SZ.
const ITCMCR: *mut u32 = 0xE000_EF90 as *mut u32;
const DTCMCR: *mut u32 = 0xE000_EF94 as *mut u32;

/// (ITCMCR, DTCMCR) as they are now.
pub fn tcm_cr() -> (u32, u32) {
    // SAFETY: reads of two system control registers of the Cortex-M7.
    unsafe { (ITCMCR.read_volatile(), DTCMCR.read_volatile()) }
}

/// The pattern [`tcm_probe`] writes.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
pub const PROBE: u32 = 0x5A17_C0DE;

/// Selftest only: enables both TCMs, writes a pattern near the top of each
/// and reads it back. Returns (ITCM word, DTCM word); both should be
/// [`PROBE`].
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
pub fn tcm_probe() -> (u32, u32) {
    const I: *mut u32 = 0x0000_FFF0 as *mut u32;
    const D: *mut u32 = 0x2001_FFF0 as *mut u32;
    // SAFETY: the selftest image keeps nothing in either TCM (its RAM is
    // AXI SRAM), so enabling them and writing one word in each disturbs
    // nothing; the addresses are inside the 64 KB ITCM and 128 KB DTCM.
    unsafe {
        ITCMCR.write_volatile(ITCMCR.read_volatile() | 1);
        DTCMCR.write_volatile(DTCMCR.read_volatile() | 1);
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        I.write_volatile(PROBE);
        D.write_volatile(!PROBE);
        (I.read_volatile(), !D.read_volatile())
    }
}

// --- Clock check (`clockcheck`) ----------------------------------------------

/// The CPU clock measured against the 32.768 kHz LSE crystal, an on-chip
/// reference independent of the HSI, the HSE and the PC.
///
/// The NUCLEO's X2 crystal drives OSC32_IN/OUT by default (UM2407 Rev 6
/// section 7.5.2: SB40 and SB41 off, R38 and R39 on). TIM16's TI1 input
/// can be the LSE itself (RM0433 section 41.6.18, TIM16_TISEL TI1SEL =
/// 0b0010), so channel 1 in input capture mode (section 41.4.6) sets CC1IF
/// on every rising LSE edge. The CPU polls that flag and reads DWT_CYCCNT
/// at the first and the last edge of a window of [`PERIODS`] LSE periods:
/// the polling latency is the same at both ends, so it cancels to a few
/// dozen cycles in a window of about a billion. TIM16's own capture
/// registers give a second count of the same window in timer kernel clocks
/// (latched in hardware, no polling at all).
#[cfg(feature = "clockcheck")]
pub mod clockcheck {
    use super::{CPU_HZ, cycles, pac, wait_for};

    /// Nominal LSE frequency.
    pub const LSE_HZ: u32 = 32_768;
    /// LSE periods per window: 2 s.
    pub const PERIODS: u32 = 2 * LSE_HZ;

    /// The oscillator the system clock is derived from, and its nominal
    /// frequency: the measured CPU clock scaled by `REF.1 / CPU_HZ` is that
    /// oscillator's real frequency (the PLL multiplies exactly). Every
    /// image's system clock comes from the HSI.
    pub const REF: (&str, u32) = ("hsi", 64_000_000);

    /// TIM16's kernel clock, nominal. RM0433 section 8.5.6, Table 57
    /// (TIMPRE = 0, its reset value): with D2PPRE2 /1 it is rcc_hclk1, with
    /// /2 also rcc_hclk1 (twice pclk2). HSI images: hclk = 64 MHz; PLL
    /// images: HPRE /2, so CPU_HZ / 2.
    #[cfg(not(any(feature = "pll", feature = "pll480")))]
    pub const TIM_HZ: u32 = CPU_HZ;
    #[cfg(any(feature = "pll", feature = "pll480"))]
    pub const TIM_HZ: u32 = CPU_HZ / 2;

    /// Starts the LSE (if it is not already running: it lives in the
    /// backup domain, which keeps it across resets) and TIM16. Returns the
    /// CPU cycles waited for LSERDY (0 if it was already ready), or `None`
    /// if it did not come up within about 5 s (DS12117 Table 131, rev V:
    /// tSU typically 2 s).
    pub fn start() -> Option<u32> {
        // SAFETY: the clock check image owns these peripherals; no
        // interrupt and no second thread exist.
        let (pwr, rcc, tim) =
            unsafe { (&*pac::PWR::ptr(), &*pac::RCC::ptr(), &*pac::TIM16::ptr()) };
        // RCC_BDCR is write-protected after reset until PWR_CR1.DBP = 1
        // (RM0433 section 8.7.26; PWR_CR1 bit 8, section 6.8.1).
        pwr.cr1.modify(|_, w| w.dbp().set_bit());
        if !wait_for(1_000_000, || pwr.cr1.read().dbp().bit_is_set()) {
            return None;
        }
        let mut waited = 0;
        if rcc.bdcr.read().lserdy().bit_is_clear() {
            // Crystal, not bypass (LSEBYP may only be written while the
            // LSE is off and not ready: section 8.7.26). Medium-high drive
            // (LSEDRV 0b10) for a sure start: DS12117 gives the critical
            // gm per drive level (Table 131) and UM2407 does not name
            // the crystal. ES0392 section 2.2.15 (swapped LSEDRV bits) is
            // revision Y only; this board is revision V.
            if rcc.bdcr.read().lseon().bit_is_clear() {
                rcc.bdcr
                    .modify(|_, w| w.lsebyp().clear_bit().lsedrv().medium_high());
            }
            rcc.bdcr.modify(|_, w| w.lseon().set_bit());
            let t0 = cycles();
            // 5 s at the nominal clock; at 480 MHz still under 2^32.
            let limit = CPU_HZ / 4 * 20;
            loop {
                if rcc.bdcr.read().lserdy().bit_is_set() {
                    break;
                }
                if cycles().wrapping_sub(t0) > limit {
                    return None;
                }
            }
            waited = cycles().wrapping_sub(t0);
        }
        // TIM16 (APB2, RCC_APB2ENR bit 17, section 8.7.47): TI1 = LSE,
        // CC1 an input capture of TI1 (CC1S = 0b01), no filter, no input
        // prescaler, rising edges (CC1P = 0), counter free-running over
        // 16 bits at the kernel clock (PSC 0, ARR 0xFFFF).
        rcc.apb2enr.modify(|_, w| w.tim16en().set_bit());
        let _ = rcc.apb2enr.read();
        tim.cr1.write(|w| w.cen().clear_bit());
        // SAFETY: 0b0010 is the documented LSE value of TI1SEL (section
        // 41.6.18), 0b01 the documented "IC1 mapped on TI1" value of CC1S,
        // and any 16-bit PSC and ARR is valid.
        unsafe {
            tim.tim16_tisel.write(|w| w.ti1sel().bits(0b0010));
            tim.ccmr1_input()
                .write(|w| w.cc1s().bits(0b01).ic1psc().bits(0).ic1f().bits(0));
            tim.psc.write(|w| w.psc().bits(0));
            tim.arr.write(|w| w.arr().bits(0xFFFF));
        }
        tim.ccer.write(|w| w.cc1e().set_bit());
        tim.egr.write(|w| w.ug().set_bit());
        tim.cr1.write(|w| w.cen().set_bit());
        Some(waited)
    }

    /// One window: the CPU cycles and the TIM16 kernel clocks between two
    /// rising LSE edges [`PERIODS`] apart.
    pub struct Window {
        pub cycles: u32,
        pub tim_clocks: u32,
        /// CC1OF seen: an edge was captured while the previous one was
        /// still unread, so the window may be short of an edge.
        pub overcapture: bool,
    }

    /// Measures one window; `None` if an edge did not come within about
    /// 10 ms (1/100 s at the nominal clock; the period is 30.5 us).
    pub fn window() -> Option<Window> {
        // SAFETY: as in `start`.
        let tim = unsafe { &*pac::TIM16::ptr() };
        let limit = CPU_HZ / 100;
        // Waits for the next capture; returns (DWT at detection, CCR1).
        // Reading CCR1 clears CC1IF (section 41.6.4, TIMx_SR).
        let edge = || {
            let t = cycles();
            loop {
                if tim.sr.read().cc1if().bit_is_set() {
                    let now = cycles();
                    return Some((now, tim.ccr[0].read().bits() as u16));
                }
                if cycles().wrapping_sub(t) > limit {
                    return None;
                }
            }
        };
        // Drop a stale capture, clear the flags (rc_w0: write 0).
        let _ = tim.ccr[0].read();
        // SAFETY: writing 0 to TIMx_SR clears every flag; no bit is set.
        tim.sr.write(|w| unsafe { w.bits(0) });
        let (t0, c0) = edge()?;
        let (mut t1, mut prev) = (t0, c0);
        let mut tim_clocks = 0u32;
        for _ in 0..PERIODS {
            let (t, c) = edge()?;
            // Each period is under 2^16 kernel clocks (7,324 at 240 MHz).
            tim_clocks += u32::from(c.wrapping_sub(prev));
            prev = c;
            t1 = t;
        }
        Some(Window {
            cycles: t1.wrapping_sub(t0),
            tim_clocks,
            overcapture: tim.sr.read().cc1of().bit_is_set(),
        })
    }
}

// --- Init and banner ---------------------------------------------------------

pub struct Board {
    pub uart: Uart,
    /// False if the cycle counter does not count.
    cyccnt: bool,
    /// Bytes copied to ITCM and to the DTCM tables.
    placed: (usize, usize),
    /// (ITCMCR, DTCMCR) as `init` found them.
    tcm_boot: (u32, u32),
}

/// Brings the board up. Call once, first thing in `main` (the supply was
/// configured before that, in `__pre_init`).
pub fn init() -> Board {
    // SAFETY: called once, from `main`, with no interrupt enabled: these
    // are the only handles to the peripherals apart from the write-only or
    // read-only accesses of `ld1`, `uart_byte` and the fault path.
    let dp = unsafe { pac::Peripherals::steal() };
    // SAFETY: as above, for the core peripherals.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    assert!(pac::PWR::ptr() as usize == PWR_BASE);

    // GPIO clocks (D3 domain, RCC_AHB4ENR): ports B, D and E.
    dp.RCC.ahb4enr.modify(|_, w| {
        w.gpioben()
            .set_bit()
            .gpioden()
            .set_bit()
            .gpioeen()
            .set_bit()
    });
    let _ = dp.RCC.ahb4enr.read(); // a read back: the clock is on before the first access
    dp.GPIOB
        .moder
        .modify(|_, w| w.moder0().output().moder14().output());
    dp.GPIOE.moder.modify(|_, w| w.moder1().output());
    // First sign of life: LD1 on, LD2 and LD3 off.
    dp.GPIOB.bsrr.write(|w| w.bs0().set_bit().br14().set_bit());
    dp.GPIOE.bsrr.write(|w| w.br1().set_bit());
    LED_ON.store(true, Ordering::Relaxed);

    let tcm_boot = tcm_cr();
    let placed = crate::tcm::place();

    #[cfg(feature = "pll")]
    let clock_ok = clock_up(&dp.PWR, &dp.RCC, &dp.FLASH);
    #[cfg(feature = "pll480")]
    let clock_step = clock_up_480(&dp.PWR, &dp.RCC, &dp.FLASH, &dp.SYSCFG);
    #[cfg(feature = "pll480")]
    let clock_ok = clock_step.is_ok();
    #[cfg(not(any(feature = "pll", feature = "pll480")))]
    let clock_ok = true;

    let d2_ok = uart_init();

    // Caches (Cortex-M7: 16 KB each on this part). TCM accesses bypass
    // them.
    cp.SCB.enable_icache();
    cp.SCB.enable_dcache(&mut cp.CPUID);

    // Cycle counter: trace enable (DEMCR.TRCENA), the DWT lock access
    // register of the Cortex-M7 (0xC5ACCE55 to DWT_LAR; without it the
    // enable can be ignored when no debugger has unlocked the DWT), then
    // CYCCNTENA.
    cp.DCB.enable_trace();
    DWT::unlock();
    cp.DWT.set_cycle_count(0);
    cp.DWT.enable_cycle_counter();
    let t0 = cycles();
    cortex_m::asm::delay(64);
    let cyccnt = DWT::has_cycle_counter() && cycles() != t0;

    let board = Board {
        uart: Uart(()),
        cyccnt,
        placed,
        tcm_boot,
    };
    // A failed clock setup leaves the UART at an unknown baud rate and a
    // dead counter makes every figure meaningless: stop with the fault
    // blink (the panic line still goes out, readable or not).
    #[cfg(feature = "pll480")]
    if let Err(step) = clock_step {
        panic!("clock setup failed: {step}");
    }
    assert!(clock_ok, "clock setup failed");
    assert!(d2_ok, "D2 domain clocks not ready");
    assert!(cyccnt, "DWT cycle counter does not count");
    dp.GPIOE.bsrr.write(|w| w.bs1().set_bit());
    board
}

impl Board {
    /// The banner: what is running and the registers that prove how.
    pub fn banner(&mut self, image: &str) {
        // SAFETY: read-only accesses to status and configuration registers.
        let (pwr, rcc, flash, dbg) = unsafe {
            (
                &*pac::PWR::ptr(),
                &*pac::RCC::ptr(),
                &*pac::FLASH::ptr(),
                &*pac::DBGMCU::ptr(),
            )
        };
        // SAFETY: CPUID is a read-only core register.
        let cpuid = unsafe { (*cortex_m::peripheral::CPUID::PTR).base.read() };
        let cr3 = pwr.cr3.read().bits();
        let csr1 = pwr.csr1.read();
        let out = &mut self.uart;
        // The UART writer never fails.
        let _ = writeln!(
            out,
            "\r\nh7bench v1 banner board={BOARD} image={image} placement={PLACEMENT} \
             clock={CLOCK_NAME} cpu_hz={CPU_HZ} baud={BAUD}\r"
        );
        let _ = writeln!(
            out,
            "h7bench v1 power cr3={cr3:#010x} csr1={:#010x} d3cr={:#010x} actvosrdy={} supply={}\r",
            csr1.bits(),
            pwr.d3cr.read().bits(),
            u8::from(csr1.actvosrdy().bit_is_set()),
            if cr3 & CR3_SUPPLY.0 == CR3_SUPPLY.1 {
                CR3_SUPPLY.2
            } else {
                "MISMATCH"
            }
        );
        let _ = writeln!(
            out,
            "h7bench v1 clock rcc_cr={:#010x} cfgr={:#010x} d1cfgr={:#010x} flash_acr={:#04x} \
             rsr={:#010x}\r",
            rcc.cr.read().bits(),
            rcc.cfgr.read().bits(),
            rcc.d1cfgr.read().bits(),
            flash.acr.read().bits(),
            rcc.rsr.read().bits()
        );
        // PLL1 as programmed (reset values in the HSI images).
        let _ = writeln!(
            out,
            "h7bench v1 pll pllckselr={:#010x} pllcfgr={:#010x} pll1divr={:#010x} \
             pll1fracr={:#010x}\r",
            rcc.pllckselr.read().bits(),
            rcc.pllcfgr.read().bits(),
            rcc.pll1divr.read().bits(),
            rcc.pll1fracr.read().bits()
        );
        let _ = writeln!(
            out,
            "h7bench v1 cpu cpuid={cpuid:#010x} idcode={:#010x} icache={} dcache={} cyccnt={} \
             itcm_bytes={} dtcm_table_bytes={}\r",
            dbg.idc.read().bits(),
            u8::from(SCB::icache_enabled()),
            u8::from(SCB::dcache_enabled()),
            u8::from(self.cyccnt),
            self.placed.0,
            self.placed.1
        );
        let (itcmcr, dtcmcr) = tcm_cr();
        let _ = writeln!(
            out,
            "h7bench v1 tcm itcmcr={itcmcr:#x} dtcmcr={dtcmcr:#x} at_init={:#x},{:#x} msp={:#010x}\r",
            self.tcm_boot.0,
            self.tcm_boot.1,
            cortex_m::register::msp::read()
        );
        // Read only: this firmware never changes an option byte.
        #[cfg(feature = "h755")]
        {
            let opt = flash.optsr_cur().read();
            let _ = writeln!(
                out,
                "h7bench v1 dualcore optsr={:#010x} bcm7={} bcm4={} boot_c2={}\r",
                opt.bits(),
                u8::from(opt.boot_cm7().bit_is_set()),
                u8::from(opt.boot_cm4().bit_is_set()),
                u8::from(rcc.gcr.read().boot_c2().bit_is_set())
            );
        }
        #[cfg(feature = "h753")]
        let _ = writeln!(
            out,
            "h7bench v1 options optsr={:#010x}\r",
            flash.optsr_cur().read().bits()
        );
        // VOS0: SYSCFG_PWRCR (ODEN is bit 0) now and before the write, and
        // what the wait saw of the ramp (see `VOS0_DIAG`).
        #[cfg(feature = "pll480")]
        {
            // SAFETY: a read-only access to SYSCFG_PWRCR; SYSCFG's clock
            // has been on since `clock_up_480`.
            let pwrcr = unsafe { (*pac::SYSCFG::ptr()).pwrcr.read().bits() };
            let _ = writeln!(
                out,
                "h7bench v1 vos0 syscfg_pwrcr={pwrcr:#010x} oden={} pwrcr_before={:#010x} \
                 polls_to_drop={} polls_low={}\r",
                pwrcr & 1,
                VOS0_DIAG[0].load(Ordering::Relaxed),
                VOS0_DIAG[1].load(Ordering::Relaxed) as i32,
                VOS0_DIAG[2].load(Ordering::Relaxed)
            );
        }
    }
}

// --- Faults ------------------------------------------------------------------

/// The fault pattern, forever: LD3 (red) blinking fast, about 8 Hz at
/// 64 MHz, with LD1 in antiphase, and the reason on the UART about once a
/// second. It sets the LEDs and the UART up itself (a fault can come
/// before `init` got there) and keeps no state in RAM.
fn die(what: fmt::Arguments<'_>) -> ! {
    // SAFETY: nothing else runs any more, so these are the only accesses
    // to RCC and GPIOB.
    let (rcc, gpiob) = unsafe { (&*pac::RCC::ptr(), &*pac::GPIOB::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioben().set_bit());
    let _ = rcc.ahb4enr.read();
    gpiob
        .moder
        .modify(|_, w| w.moder0().output().moder14().output());
    let _ = uart_init();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(16) {
            let _ = writeln!(Uart(()), "\r\nh7bench v1 FAULT {what}\r");
        }
        gpiob.bsrr.write(|w| {
            if n.is_multiple_of(2) {
                w.bs14().set_bit().br0().set_bit()
            } else {
                w.br14().set_bit().bs0().set_bit()
            }
        });
        cortex_m::asm::delay(CPU_HZ / 16);
        n = n.wrapping_add(1);
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    die(format_args!("panic {info}"))
}

// HardFault, straight from the vector table (this symbol replaces
// cortex-m-rt's handler and its trampoline). The exception frame is at the
// main stack pointer; no other stack is ever used.
// The fault may be the stack itself (an overflow, RAM that does not
// answer), so before any Rust runs the stack pointer moves to the top of
// AXI SRAM (0x2408_0000; RM0399 section 2.3), which no placement uses: its
// statics and buffers sit at the bottom of their regions and the handler
// never returns.
core::arch::global_asm!(
    ".section .text.HardFault, \"ax\", %progbits",
    ".global HardFault",
    ".type HardFault, %function",
    ".thumb_func",
    "HardFault:",
    "    mrs r0, msp", // the exception frame
    "    mov r1, lr",  // EXC_RETURN
    "    ldr r2, =0x24080000",
    "    msr msp, r2",
    "    b {handler}",
    "    .ltorg",
    ".size HardFault, . - HardFault",
    handler = sym hard_fault,
);

/// Prints the fault status registers (Armv7-M: CFSR = UFSR:BFSR:MMFSR,
/// HFSR, BFAR, MMFAR) and, if the exception frame is in RAM, the stacked
/// PC and LR. `frame` is the stack pointer at the fault.
extern "C" fn hard_fault(frame: *const u32, exc_return: u32) -> ! {
    // SAFETY: read-only accesses to the fault status registers.
    let (cfsr, hfsr, bfar, mmfar) = unsafe {
        let scb = &*SCB::PTR;
        (
            scb.cfsr.read(),
            scb.hfsr.read(),
            scb.bfar.read(),
            scb.mmfar.read(),
        )
    };
    let at = frame as usize;
    let in_ram = at.is_multiple_of(4)
        && ((0x2000_0000..=0x2002_0000 - 32).contains(&at)
            || (0x2400_0000..=0x2408_0000 - 32).contains(&at));
    // The frame: r0-r3, r12, lr, pc, xpsr.
    let (lr, pc) = if in_ram && cfsr & (1 << 12) == 0 {
        // SAFETY: the eight words of the frame are inside DTCM or AXI SRAM
        // (checked above) and the CPU did stack them (BFSR.STKERR, CFSR
        // bit 12, is clear); a volatile read cannot disturb anything.
        unsafe { (frame.add(5).read_volatile(), frame.add(6).read_volatile()) }
    } else {
        (0, 0)
    };
    die(format_args!(
        "hardfault sp={at:#010x} exc_return={exc_return:#010x} frame={} pc={pc:#010x} \
         lr={lr:#010x} cfsr={cfsr:#010x} hfsr={hfsr:#010x} bfar={bfar:#010x} mmfar={mmfar:#010x}",
        if in_ram { "ok" } else { "none" }
    ))
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) -> ! {
    die(format_args!("unexpected exception {irqn}"))
}
