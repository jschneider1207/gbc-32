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
#[cfg(not(feature = "pll"))]
pub const CPU_HZ: u32 = 64_000_000;
#[cfg(not(feature = "pll"))]
const CLOCK_NAME: &str = "hsi64";

/// Phase B (`--features pll`): PLL1 from HSI, 64 MHz / 4 * 49 / 2 =
/// 392 MHz. DS12919 Table 13 and Table 14: with the SMPS supplying VCORE
/// the highest scale is VOS1 and its limit is 400 MHz (Table 14 note 1:
/// "VOS0 (power scale 0) is not available when the SMPS step-down converter
/// directly supplies VCORE"), so 480 MHz is out of reach on this board.
/// 392 and not 400: the HSI is the reference (no dependency on the
/// ST-LINK's 8 MHz MCO) and its tolerance of about 1% must stay under the
/// limit.
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

const BAUD: u32 = 115_200;

#[cfg(feature = "itcm")]
const PLACEMENT: &str = "itcm-dtcm";
#[cfg(all(feature = "dtcm", not(feature = "itcm")))]
const PLACEMENT: &str = "flash-dtcm";
#[cfg(not(feature = "dtcm"))]
const PLACEMENT: &str = "flash-axi";

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
    // PLL1 (off after reset): source HSI, DIVM1 = 4 (16 MHz reference).
    // RM0399 section 9.7.10.
    rcc.pllckselr
        .modify(|_, w| w.pllsrc().hsi().divm1().bits(4));
    // Reference range 8 to 16 MHz (PLL1RGE = 0b11), wide VCO (192 to
    // 960 MHz), integer mode, only the P output. RM0399 section 9.7.11.
    rcc.pllcfgr.modify(|_, w| {
        w.pll1rge()
            .range8()
            .pll1vcosel()
            .wide_vco()
            .pll1fracen()
            .reset()
            .divp1en()
            .enabled()
            .divq1en()
            .disabled()
            .divr1en()
            .disabled()
    });
    // DIVN1 field = N - 1 = 48 (x49: VCO 784 MHz), DIVP1 field = P - 1 = 1
    // (/2: 392 MHz). RM0399 section 9.7.12.
    // SAFETY: both are documented field values (DIVN1 4 to 512, DIVP1 /2).
    rcc.pll1divr
        .modify(|_, w| unsafe { w.divn1().bits(48).divp1().bits(1) });
    rcc.cr.modify(|_, w| w.pll1on().set_bit());
    if !wait_for(TRIES, || rcc.cr.read().pll1rdy().bit_is_set()) {
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
/// path gets the baud rate right wherever the clock setup stopped.
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
#[cfg(feature = "selftest")]
pub const PROBE: u32 = 0x5A17_C0DE;

/// Selftest only: enables both TCMs, writes a pattern near the top of each
/// and reads it back. Returns (ITCM word, DTCM word); both should be
/// [`PROBE`].
#[cfg(feature = "selftest")]
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
    #[cfg(not(feature = "pll"))]
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
