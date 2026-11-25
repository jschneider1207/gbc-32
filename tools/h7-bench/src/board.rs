//! NUCLEO-H755ZI-Q (MB1363, STM32H755ZIT6) bring-up: supply, clock, LEDs,
//! the ST-LINK virtual COM port, caches, TCM placement, the cycle counter
//! and the fault handlers. Cortex-M7 only; see README.md "Dual core" for
//! what the Cortex-M4 does meanwhile.
//!
//! Sources: UM2408 Rev 6 (the board), RM0399 Rev 5 (the MCU), DS12919 Rev 3
//! (the STM32H755xI datasheet).

use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

use cortex_m::peripheral::{DWT, SCB};
use cortex_m_rt::{ExceptionFrame, exception};
use stm32h7::stm32h747cm7 as pac;

// --- Supply --------------------------------------------------------------
//
// THE item that can lock the board. UM2408 section 7.4.8, Table 10: the
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

const PWR_BASE: usize = 0x5802_4800;
/// PWR_CR3 supply bits (see above) and their value for direct SMPS.
const CR3_SUPPLY_MASK: u32 = 0x3F;
const CR3_DIRECT_SMPS: u32 = 0x04;

// --- Clock ---------------------------------------------------------------

/// Phase A (default): the 64 MHz HSI the chip resets to (RM0399 section
/// 9.5.6 and 9.7.2: the HSI is the system clock after reset, HSIDIV /1), every
/// prescaler /1, voltage scale VOS3 and the flash latency left at their
/// reset values. Nothing clock-related is written.
#[cfg(not(feature = "pll"))]
pub const CPU_HZ: u32 = 64_000_000;
#[cfg(not(feature = "pll"))]
const PCLK1_HZ: u32 = 64_000_000;
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
#[cfg(feature = "pll")]
pub const CPU_HZ: u32 = 392_000_000;
/// rcc_pclk1 = 392 / 2 (HPRE) / 2 (D2PPRE1).
#[cfg(feature = "pll")]
const PCLK1_HZ: u32 = 98_000_000;
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
/// default (SB16 and SB17 on). DS12919 alternate-function table: both are
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

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(uart_byte);
        Ok(())
    }
}

// --- Init and banner ---------------------------------------------------------

pub struct Board {
    pub uart: Uart,
    /// False if the cycle counter does not count.
    cyccnt: bool,
    /// Bytes copied to ITCM and to the DTCM tables.
    placed: (usize, usize),
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

    let placed = crate::tcm::place();

    #[cfg(feature = "pll")]
    let clock_ok = clock_up(&dp.PWR, &dp.RCC, &dp.FLASH);
    #[cfg(not(feature = "pll"))]
    let clock_ok = true;

    // USART3 is in the D2 domain, the Cortex-M4's. Enabling its clock from
    // this CPU allocates it to CPU1, which keeps D2 clocked whatever the
    // M4 does (RM0399 section 9.5.10 "Peripheral allocation"); D2CKRDY
    // tells when the domain's clocks run.
    dp.RCC.apb1lenr.modify(|_, w| w.usart3en().set_bit());
    let d2_ok = wait_for(4_000_000, || dp.RCC.cr.read().d2ckrdy().bit_is_set());
    dp.GPIOD.afrh.modify(|_, w| w.afr8().af7());
    dp.GPIOD.moder.modify(|_, w| w.moder8().alternate());
    // Kernel clock: rcc_pclk1 (RCC_D2CCIP2R USART234578SEL resets to 0).
    // Oversampling by 16: BRR = f / baud, rounded.
    let brr = (PCLK1_HZ + BAUD / 2) / BAUD;
    // SAFETY: any 16-bit value of at least 16 is a valid BRR.
    dp.USART3.brr.write(|w| unsafe { w.bits(brr) });
    dp.USART3.cr1.write(|w| w.ue().set_bit().te().set_bit());

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
        let opt = flash.optsr_cur().read();
        let out = &mut self.uart;
        // The UART writer never fails.
        let _ = writeln!(
            out,
            "\r\nh7bench v1 banner board=NUCLEO-H755ZI-Q image={image} placement={PLACEMENT} \
             clock={CLOCK_NAME} cpu_hz={CPU_HZ} baud={BAUD}\r"
        );
        let _ = writeln!(
            out,
            "h7bench v1 power cr3={cr3:#010x} csr1={:#010x} d3cr={:#010x} actvosrdy={} supply={}\r",
            csr1.bits(),
            pwr.d3cr.read().bits(),
            u8::from(csr1.actvosrdy().bit_is_set()),
            if cr3 & CR3_SUPPLY_MASK == CR3_DIRECT_SMPS {
                "direct-smps"
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
        let _ = writeln!(
            out,
            "h7bench v1 dualcore optsr={:#010x} bcm7={} bcm4={} boot_c2={}\r",
            opt.bits(),
            u8::from(opt.boot_cm7().bit_is_set()),
            u8::from(opt.boot_cm4().bit_is_set()),
            u8::from(rcc.gcr.read().boot_c2().bit_is_set())
        );
    }
}

// --- Faults ------------------------------------------------------------------

/// The fault pattern, forever: LD3 (red) blinking fast, about 8 Hz at
/// 64 MHz, with LD1 in antiphase, and the reason on the UART about once a
/// second. Uses only a delay loop: no counter, no state.
fn die(what: fmt::Arguments<'_>) -> ! {
    // SAFETY: write-only BSRR accesses, as in `ld1`; nothing else runs
    // any more.
    let gpiob = unsafe { &*pac::GPIOB::ptr() };
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

#[exception]
unsafe fn HardFault(frame: &ExceptionFrame) -> ! {
    // SAFETY: read-only accesses to the fault status registers.
    let (cfsr, hfsr, bfar) = unsafe {
        let scb = &*SCB::PTR;
        (scb.cfsr.read(), scb.hfsr.read(), scb.bfar.read())
    };
    die(format_args!(
        "hardfault pc={:#010x} lr={:#010x} cfsr={cfsr:#010x} hfsr={hfsr:#010x} bfar={bfar:#010x}",
        frame.pc(),
        frame.lr()
    ))
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) -> ! {
    die(format_args!("unexpected exception {irqn}"))
}
