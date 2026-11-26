//! NUCLEO-H753ZI board support (ADR 0036): everything chip- and
//! board-specific that the firmware images share. Sources: UM2407 Rev 6
//! (the board), RM0433 Rev 9 (the MCU), DS12117 Rev 10 (the STM32H753xI
//! datasheet), ES0392 Rev 16 (its errata).
//!
//! What every image gets from linking this lib:
//! - the core supply (`__pre_init`, [`power`]), the first code after reset;
//! - the panic handler, HardFault on its own stack and DefaultHandler
//!   ([`fault`]), with a report on the UART.
//!
//! And from [`init`], called first in `main`: the LEDs, the TCM copy
//! ([`tcm`]), the 472 MHz clock from the HSI ([`clock`], plan D11), the
//! UART ([`uart`]), the MPU ([`mpu`]), the I- and D-cache (write-back,
//! ES0392 section 2.1.1) and the DWT cycle counter. [`clockcheck`]
//! measures the clock against the LSE crystal.
//!
//! Only the H753 at the D11 clock: no board or clock features. The image's
//! `memory.x` comes from `memory_x::firmware_memory_x`, which defines the
//! linker symbols [`tcm`] and [`fault`] read.
#![no_std]

pub mod clock;
pub mod clockcheck;
mod fault;
pub mod led;
pub mod mpu;
pub mod power;
pub mod stack;
pub mod tcm;
pub mod uart;

use core::fmt::Write;

use cortex_m::peripheral::{DWT, SCB};
pub use stm32h7::stm32h753v as pac;

pub use clock::CPU_HZ;
pub use uart::Uart;

/// The board's name, in every banner (and so in every image: the flash
/// script refuses an image without it).
pub const BOARD: &str = "NUCLEO-H753ZI";

/// DWT_CYCCNT: CPU clock cycles, free-running, 32 bits (wraps every 9.1 s
/// at 472 MHz; one emulated frame is far shorter).
#[inline(always)]
pub fn cycles() -> u32 {
    DWT::cycle_count()
}

/// Busy-waits until `n` cycles have passed since counter value `t0`.
pub fn wait_since(t0: u32, n: u32) {
    while cycles().wrapping_sub(t0) < n {}
}

/// The board after [`init`]: the UART, and what `init` saw for the banner.
pub struct Board {
    pub uart: Uart,
    prefix: &'static str,
    /// False if the cycle counter does not count (`init` panics then).
    cyccnt: bool,
    /// Bytes copied to ITCM and to the DTCM tables.
    placed: (usize, usize),
    /// (ITCMCR, DTCMCR) as `init` found them.
    tcm_boot: (u32, u32),
    /// `None` until [`Board::measure_clock`] ran; then its result.
    lse_cpu_hz: Option<Option<u32>>,
}

/// Brings the board up. Call once, first thing in `main` (the supply was
/// configured before that, in `__pre_init`). `prefix` starts every line
/// the lib prints, the banner and the fault report.
///
/// # Panics
/// If the clock setup, the D2 domain's clocks or the cycle counter failed
/// (the fault pattern: the clock may be wrong, so the line may not be
/// readable), or on a second call.
pub fn init(prefix: &'static str) -> Board {
    use core::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    fault::set_prefix(prefix);
    assert!(!DONE.swap(true, Ordering::AcqRel), "init called twice");
    // SAFETY: called once (checked above), from `main`, with no interrupt
    // enabled: these are the only handles to the peripherals apart from
    // the write-only or read-only accesses of `led`, `uart` and the fault
    // path.
    let dp = unsafe { pac::Peripherals::steal() };
    // SAFETY: as above, for the core peripherals.
    let mut cp = unsafe { cortex_m::Peripherals::steal() };
    assert!(pac::PWR::ptr() as usize == power::PWR_BASE);

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
    // First sign of life: LD1 on, LD2 and LD3 off.
    led::init(&dp.GPIOB, &dp.GPIOE);

    let tcm_boot = tcm::control();
    let placed = tcm::place();

    let clock = clock::clock_up(&dp.PWR, &dp.RCC, &dp.FLASH, &dp.SYSCFG);
    let d2_ok = uart::init();

    // The MPU first, then the caches (Cortex-M7: 16 KB each on this part),
    // so no line is filled under the default map's write-through
    // attributes. TCM accesses bypass the caches.
    mpu::init(&mut cp.MPU);
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

    // A failed clock setup leaves the UART at an unknown baud rate and a
    // dead counter makes every figure meaningless: stop with the fault
    // blink (the panic line still goes out, readable or not).
    if let Err(step) = clock {
        panic!("clock setup failed: {step}");
    }
    assert!(d2_ok, "D2 domain clocks not ready");
    assert!(cyccnt, "DWT cycle counter does not count");
    led::ld2_on();
    Board {
        uart: Uart::new(),
        prefix,
        cyccnt,
        placed,
        tcm_boot,
        lse_cpu_hz: None,
    }
}

impl Board {
    /// The prefix `init` was given.
    pub fn prefix(&self) -> &'static str {
        self.prefix
    }

    /// Bytes copied to ITCM and to the DTCM tables by `init`.
    pub fn placed(&self) -> (usize, usize) {
        self.placed
    }

    /// Measures the CPU clock against the LSE ([`clockcheck::cpu_hz`],
    /// about 2 s) and keeps the result for the banner (plan D11).
    pub fn measure_clock(&mut self) -> Option<u32> {
        let hz = clockcheck::cpu_hz();
        self.lse_cpu_hz = Some(hz);
        hz
    }

    /// The banner: what is running and the registers that prove how. Its
    /// first line is `<prefix> <image> board=... cpu_hz=... lse_cpu_hz=...`
    /// (`lse_cpu_hz=unmeasured` before [`Board::measure_clock`], `fail` if
    /// it failed); the others are `<prefix> <topic> ...` lines.
    pub fn banner(&mut self, image: &str) {
        // SAFETY: read-only accesses to status and configuration
        // registers; SYSCFG's clock has been on since `clock::clock_up`.
        let (pwr, rcc, flash, dbg, syscfg) = unsafe {
            (
                &*pac::PWR::ptr(),
                &*pac::RCC::ptr(),
                &*pac::FLASH::ptr(),
                &*pac::DBGMCU::ptr(),
                &*pac::SYSCFG::ptr(),
            )
        };
        // SAFETY: CPUID is a read-only core register; MPU_TYPE and
        // MPU_CTRL are read only.
        let (cpuid, mpu_type, mpu_ctrl) = unsafe {
            let mpu = &*cortex_m::peripheral::MPU::PTR;
            (
                (*cortex_m::peripheral::CPUID::PTR).base.read(),
                mpu._type.read(),
                mpu.ctrl.read(),
            )
        };
        let p = self.prefix;
        let cr3 = pwr.cr3.read().bits();
        let csr1 = pwr.csr1.read();
        let d3cr = pwr.d3cr.read().bits();
        let pwrcr = syscfg.pwrcr.read().bits();
        let acr = flash.acr.read().bits();
        let out = &mut self.uart;
        // The UART writer never fails.
        let _ = write!(
            out,
            "\r\n{p} {image} board={BOARD} clock={} cpu_hz={CPU_HZ} lse_cpu_hz=",
            clock::CLOCK_NAME
        );
        let _ = match self.lse_cpu_hz {
            None => write!(out, "unmeasured"),
            Some(None) => write!(out, "fail"),
            Some(Some(hz)) => write!(out, "{hz}"),
        };
        let _ = writeln!(
            out,
            " baud={} d3cr={d3cr:#010x} syscfg_pwrcr={pwrcr:#010x} oden={} flash_acr={acr:#04x} \
             icache={} dcache={} mpu={} cyccnt={}\r",
            uart::BAUD,
            pwrcr & 1,
            u8::from(SCB::icache_enabled()),
            u8::from(SCB::dcache_enabled()),
            u8::from(mpu_ctrl & 1 != 0),
            u8::from(self.cyccnt),
        );
        let _ = writeln!(
            out,
            "{p} power cr3={cr3:#010x} csr1={:#010x} d3cr={d3cr:#010x} actvosrdy={} supply={}\r",
            csr1.bits(),
            u8::from(csr1.actvosrdy().bit_is_set()),
            if cr3 & power::CR3_SUPPLY_MASK == power::CR3_SUPPLY_LDO {
                "ldo"
            } else {
                "MISMATCH"
            }
        );
        let _ = writeln!(
            out,
            "{p} clock rcc_cr={:#010x} cfgr={:#010x} d1cfgr={:#010x} d2cfgr={:#010x} \
             d3cfgr={:#010x} flash_acr={acr:#04x} rsr={:#010x}\r",
            rcc.cr.read().bits(),
            rcc.cfgr.read().bits(),
            rcc.d1cfgr.read().bits(),
            rcc.d2cfgr.read().bits(),
            rcc.d3cfgr.read().bits(),
            rcc.rsr.read().bits()
        );
        let _ = writeln!(
            out,
            "{p} pll pllckselr={:#010x} pllcfgr={:#010x} pll1divr={:#010x} pll1fracr={:#010x}\r",
            rcc.pllckselr.read().bits(),
            rcc.pllcfgr.read().bits(),
            rcc.pll1divr.read().bits(),
            rcc.pll1fracr.read().bits()
        );
        let (before, to_drop, low) = clock::vos0_diag();
        let _ = writeln!(
            out,
            "{p} vos0 syscfg_pwrcr={pwrcr:#010x} oden={} pwrcr_before={before:#010x} \
             polls_to_drop={} polls_low={low}\r",
            pwrcr & 1,
            to_drop as i32,
        );
        let _ = writeln!(
            out,
            "{p} cpu cpuid={cpuid:#010x} idcode={:#010x} optsr={:#010x} mpu_ctrl={mpu_ctrl:#x} \
             mpu_regions={}\r",
            dbg.idc.read().bits(),
            flash.optsr_cur().read().bits(),
            (mpu_type >> 8) & 0xFF
        );
        let (itcmcr, dtcmcr) = tcm::control();
        let _ = writeln!(
            out,
            "{p} tcm itcmcr={itcmcr:#x} dtcmcr={dtcmcr:#x} at_init={:#x},{:#x} itcm_bytes={} \
             dtcm_table_bytes={} msp={:#010x}\r",
            self.tcm_boot.0,
            self.tcm_boot.1,
            self.placed.0,
            self.placed.1,
            cortex_m::register::msp::read()
        );
    }
}
