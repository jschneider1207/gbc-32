//! USART3, polled: a `core::fmt::Write` sink and a receiver. UM2407
//! section 7.6.5 and Table 10: PD8 (USART3 TX) and PD9 (USART3 RX) go to
//! the ST-LINK virtual COM port by default (SB19 and SB12 on). DS12117
//! alternate-function table: both are AF7. 8N1, [`BAUD`] after reset;
//! [`Uart::set_baud`] changes it at run time.
//!
//! Kernel clock: rcc_pclk1 (RCC_D2CCIP2R USART234578SEL resets to 000,
//! RM0433 section 8.7.21; this lib never changes it), which `clock` sets to
//! 118 MHz (CPU 472 / HPRE 2 / D2PPRE1 2). USART_PRESC stays at its reset
//! value (/1). Baud rates: [`baud_setting`] and
//! `docs/reference/qspi-nor.md` "UART".
//!
//! Receive (RM0433 section 48.5.6): [`Uart::try_read`] and
//! [`Uart::read_timeout`] poll RXNE (RXFNE in FIFO mode, the same bit).
//! An overrun (ORE: a byte arrived while RDR, or the 16-byte RX FIFO, was
//! full; at least one byte is lost) is counted and cleared (ICR.ORECF);
//! framing and noise errors likewise (the byte itself is still returned).
//! After `init` the FIFOs are off; [`Uart::set_baud`] turns them on.

use core::fmt::{self, Write};
use core::sync::atomic::{AtomicU32, Ordering};

pub use crate::baud::{BaudSetting, MAX_ERROR_PPM, baud_setting, brr_rate, effective_divisor};
use crate::clock::{pclk1_hz, wait_for};
use crate::pac;

/// The VCP's baud rate after reset (and for every image that never calls
/// [`Uart::set_baud`]).
pub const BAUD: u32 = 115_200;

/// The rate [`init`] programs: [`BAUD`] until [`Uart::set_baud`] changes
/// it, so a fault report after a change goes out at the host's rate.
static CURRENT_BAUD: AtomicU32 = AtomicU32::new(BAUD);
/// Whether [`init`] turns the FIFOs on (`set_baud` does).
static FIFO: AtomicU32 = AtomicU32::new(0);
/// Error counts since reset: overrun, framing, noise.
static OVERRUNS: AtomicU32 = AtomicU32::new(0);
static FRAMING: AtomicU32 = AtomicU32::new(0);
static NOISE: AtomicU32 = AtomicU32::new(0);

// USART_ISR (RM0433 48.7.10), USART_ICR (48.7.11) bits.
const ISR_FE: u32 = 1 << 1;
const ISR_NE: u32 = 1 << 2;
const ISR_ORE: u32 = 1 << 3;
const ISR_RXNE: u32 = 1 << 5;
const ISR_TC: u32 = 1 << 6;
const ICR_FECF: u32 = 1 << 1;
const ICR_NECF: u32 = 1 << 2;
const ICR_ORECF: u32 = 1 << 3;

/// USART3's kernel clock as the clock registers stand (rcc_pclk1).
pub fn kernel_hz() -> u32 {
    // SAFETY: read-only accesses to RCC configuration registers.
    pclk1_hz(unsafe { &*pac::RCC::ptr() })
}

/// The rate the UART runs at now.
pub fn baud() -> u32 {
    CURRENT_BAUD.load(Ordering::Relaxed)
}

/// Why [`Uart::set_baud`] refused a rate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BaudError {
    /// No BRR value reaches it (USARTDIV < 16, or the rate is 0).
    Unreachable,
    /// The best setting is off by more than [`MAX_ERROR_PPM`].
    Inexact(BaudSetting),
}

/// Receive error counts since reset (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RxErrors {
    /// ORE: bytes were lost (at least one each time).
    pub overruns: u32,
    /// FE: a stop bit was 0 (a break, a baud mismatch, or noise).
    pub framing: u32,
    /// NE: noise on a sampled bit.
    pub noise: u32,
}

/// The UART writer and receiver. Writing never fails: a byte the
/// transmitter does not take within the bounded wait is dropped.
pub struct Uart(());

impl Uart {
    /// A handle for USART3. Every handle shares the one peripheral; this
    /// lib has no interrupts and no second thread, so accesses never
    /// interleave.
    pub(crate) const fn new() -> Self {
        Uart(())
    }

    /// Sends one raw byte (bounded wait, as the `Write` impl).
    pub fn write_byte(&mut self, b: u8) {
        uart_byte(b);
    }

    /// Sends one raw byte if the transmitter takes it now (TXE; TXFNF in
    /// FIFO mode), without waiting: false if it is full.
    pub fn try_write(&mut self, b: u8) -> bool {
        let usart = regs();
        if usart.isr.read().txe().bit_is_set() {
            usart.tdr.write(|w| w.tdr().bits(u16::from(b)));
            true
        } else {
            false
        }
    }

    /// One received byte if there is one, without waiting. Counts and
    /// clears any overrun, framing or noise error first.
    pub fn try_read(&mut self) -> Option<u8> {
        let usart = regs();
        let isr = usart.isr.read().bits();
        let errs = isr & (ISR_ORE | ISR_FE | ISR_NE);
        if errs != 0 {
            if isr & ISR_ORE != 0 {
                OVERRUNS.fetch_add(1, Ordering::Relaxed);
            }
            if isr & ISR_FE != 0 {
                FRAMING.fetch_add(1, Ordering::Relaxed);
            }
            if isr & ISR_NE != 0 {
                NOISE.fetch_add(1, Ordering::Relaxed);
            }
            let icr = (if isr & ISR_ORE != 0 { ICR_ORECF } else { 0 })
                | (if isr & ISR_FE != 0 { ICR_FECF } else { 0 })
                | (if isr & ISR_NE != 0 { ICR_NECF } else { 0 });
            // SAFETY: ORECF, FECF and NECF are write-1-to-clear bits; the
            // others are written 0 (no effect).
            usart.icr.write(|w| unsafe { w.bits(icr) });
        }
        if isr & ISR_RXNE == 0 {
            return None;
        }
        // Reading RDR pops the byte (and clears RXNE when it was the last).
        Some(usart.rdr.read().bits() as u8)
    }

    /// One received byte, waiting at most `timeout_cycles` CPU cycles
    /// (DWT; 472 per microsecond). `None` on timeout.
    pub fn read_timeout(&mut self, timeout_cycles: u32) -> Option<u8> {
        let t0 = crate::cycles();
        loop {
            if let Some(b) = self.try_read() {
                return Some(b);
            }
            if crate::cycles().wrapping_sub(t0) >= timeout_cycles {
                return None;
            }
        }
    }

    /// The receive errors counted so far ([`Uart::try_read`] counts them).
    pub fn rx_errors(&self) -> RxErrors {
        RxErrors {
            overruns: OVERRUNS.load(Ordering::Relaxed),
            framing: FRAMING.load(Ordering::Relaxed),
            noise: NOISE.load(Ordering::Relaxed),
        }
    }

    /// Waits (bounded) until the last byte written has left the shift
    /// register (ISR.TC).
    pub fn flush(&mut self) {
        let usart = regs();
        let _ = wait_for(4_000_000, || usart.isr.read().bits() & ISR_TC != 0);
    }

    /// Switches to `bps` ([`baud_setting`] at the current kernel clock),
    /// with the TX and RX FIFOs on (16 bytes each, CR1.FIFOEN, 48.5.4).
    /// Waits for the transmitter to finish first (TC), so no byte already
    /// written is cut; bytes received meanwhile are lost (the USART is
    /// disabled for a few cycles: BRR, OVER8 and FIFOEN can only be written
    /// with UE = 0). Refuses a rate more than [`MAX_ERROR_PPM`] off.
    pub fn set_baud(&mut self, bps: u32) -> Result<BaudSetting, BaudError> {
        let s = baud_setting(kernel_hz(), bps).ok_or(BaudError::Unreachable)?;
        if s.error_ppm.unsigned_abs() > MAX_ERROR_PPM {
            return Err(BaudError::Inexact(s));
        }
        self.flush();
        CURRENT_BAUD.store(bps, Ordering::Relaxed);
        FIFO.store(1, Ordering::Relaxed);
        configure(&s, true);
        Ok(s)
    }
}

fn regs() -> &'static pac::usart1::RegisterBlock {
    // SAFETY: shared access to USART3's registers; this lib has no
    // interrupts and no second thread, so register accesses never overlap.
    unsafe { &*pac::USART3::ptr() }
}

/// One byte, with a bounded wait for the transmit register (TXE; TXFNF in
/// FIFO mode, the same bit), so that a UART that is not running (a fault
/// before `init` set it up) cannot hang the fault handler.
fn uart_byte(b: u8) {
    let usart = regs();
    if wait_for(1_000_000, || usart.isr.read().txe().bit_is_set()) {
        usart.tdr.write(|w| w.tdr().bits(u16::from(b)));
    }
}

/// Disables the USART, writes BRR, OVER8 and FIFOEN, and enables it with
/// TE and RE (RM0433 48.5.2: these only while UE = 0).
fn configure(s: &BaudSetting, fifo: bool) {
    let usart = regs();
    usart.cr1.write(|w| w.ue().clear_bit());
    // SAFETY: `baud_setting` gives a BRR from a USARTDIV of 16..=0xFFFF in
    // the layout OVER8 wants (48.5.7).
    usart.brr.write(|w| unsafe { w.bits(s.brr) });
    usart.cr1.write(|w| {
        w.fifoen()
            .bit(fifo)
            .over8()
            .bit(s.over8)
            .ue()
            .set_bit()
            .te()
            .set_bit()
            .re()
            .set_bit()
    });
}

/// Sets USART3 up at the current rate ([`BAUD`] until
/// [`Uart::set_baud`]) for the clock the RCC reports, with TX and RX;
/// safe to repeat (the fault handler does). Returns false if the D2
/// domain's clocks did not come up.
pub(crate) fn init() -> bool {
    // SAFETY: shared access to RCC and GPIOD; no interrupt and no second
    // thread exists, so register accesses never overlap.
    let (rcc, gpiod) = unsafe { (&*pac::RCC::ptr(), &*pac::GPIOD::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioden().set_bit());
    // USART3 is in the D2 domain. Enabling its clock from this CPU
    // allocates it to the CPU, which keeps D2 clocked (RM0433 section
    // 8.5.10 "Peripheral allocation"); D2CKRDY (RCC_CR bit 15) tells when
    // the domain's clocks run.
    rcc.apb1lenr.modify(|_, w| w.usart3en().set_bit());
    let d2_ok = wait_for(4_000_000, || rcc.cr.read().d2ckrdy().bit_is_set());
    // PD8 TX and PD9 RX, AF7. RX gets a pull-up so it idles high (a
    // mark) if the ST-LINK ever stops driving it.
    gpiod.afrh.modify(|_, w| w.afr8().af7().afr9().af7());
    gpiod.pupdr.modify(|_, w| w.pupdr9().pull_up());
    gpiod
        .moder
        .modify(|_, w| w.moder8().alternate().moder9().alternate());
    let ker = pclk1_hz(rcc);
    // The stored rate if the clock reaches it, else BAUD (a fault before
    // the PLL ran, on the HSI). 115200 is reachable from any clock here.
    let s = baud_setting(ker, baud())
        .filter(|s| s.error_ppm.unsigned_abs() <= MAX_ERROR_PPM)
        .or_else(|| baud_setting(ker, BAUD));
    if let Some(s) = s {
        configure(&s, FIFO.load(Ordering::Relaxed) != 0);
    }
    d2_ok
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        s.bytes().for_each(uart_byte);
        Ok(())
    }
}
