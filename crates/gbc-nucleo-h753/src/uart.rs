//! USART3 transmitter, polled, as a `core::fmt::Write` sink. UM2407 section
//! 7.6.5 and Table 10: PD8 (USART3 TX) and PD9 (USART3 RX) go to the
//! ST-LINK virtual COM port by default (SB19 and SB12 on). DS12117
//! alternate-function table: both are AF7. 115200 8N1. Only TX is used;
//! PD9 is left alone.

use core::fmt::{self, Write};

use crate::clock::{pclk1_hz, wait_for};
use crate::pac;

/// The VCP's baud rate.
pub const BAUD: u32 = 115_200;

/// The UART writer. It never fails: a byte the transmitter does not take
/// within the bounded wait is dropped.
pub struct Uart(());

impl Uart {
    /// A writer for USART3. Every writer shares the one transmitter; this
    /// lib has no interrupts and no second thread, so writes never
    /// interleave.
    pub(crate) const fn new() -> Self {
        Uart(())
    }
}

/// One byte, with a bounded wait for the transmit register, so that a
/// UART that is not running (a fault before `init` set it up) cannot hang
/// the fault handler.
fn uart_byte(b: u8) {
    // SAFETY: shared access to USART3's registers; this lib has no
    // interrupts and no second thread, so register accesses never overlap.
    let usart = unsafe { &*pac::USART3::ptr() };
    if wait_for(1_000_000, || usart.isr.read().txe().bit_is_set()) {
        usart.tdr.write(|w| w.tdr().bits(u16::from(b)));
    }
}

/// Sets USART3 up for transmission at the clock the RCC reports; safe to
/// repeat. Returns false if the D2 domain's clocks did not come up.
pub(crate) fn init() -> bool {
    // SAFETY: shared access to RCC, GPIOD and USART3; no interrupt and no
    // second thread exists, so register accesses never overlap.
    let (rcc, gpiod, usart) =
        unsafe { (&*pac::RCC::ptr(), &*pac::GPIOD::ptr(), &*pac::USART3::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioden().set_bit());
    // USART3 is in the D2 domain. Enabling its clock from this CPU
    // allocates it to the CPU, which keeps D2 clocked (RM0433 section
    // 8.5.10 "Peripheral allocation"); D2CKRDY (RCC_CR bit 15) tells when
    // the domain's clocks run.
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
