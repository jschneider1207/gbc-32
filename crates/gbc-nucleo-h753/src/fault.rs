//! The panic handler, HardFault (on its own stack) and DefaultHandler. One
//! copy per image: every image that links this lib uses these.

use core::cell::Cell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

use cortex_m::interrupt::Mutex;
use cortex_m::peripheral::SCB;
use cortex_m_rt::exception;

use crate::clock::CPU_HZ;
use crate::pac;
use crate::uart::{self, Uart};

/// The prefix of the fault line until `init` sets the image's own.
const DEFAULT_PREFIX: &str = "gbc-nucleo-h753";

/// The prefix of the fault line (the image's banner prefix, from `init`).
static PREFIX: Mutex<Cell<&'static str>> = Mutex::new(Cell::new(DEFAULT_PREFIX));

pub(crate) fn set_prefix(prefix: &'static str) {
    cortex_m::interrupt::free(|cs| PREFIX.borrow(cs).set(prefix));
}

/// The fault pattern, forever: LD3 (red) blinking fast, about 8 Hz at
/// 472 MHz, with LD1 in antiphase, and the reason on the UART about once a
/// second as `<prefix> FAULT <what>`. It sets the LEDs and the UART up
/// itself (a fault can come before `init` got there); its only state in
/// RAM is the prefix, read once.
///
/// First it quiesces the chip (M7 review C3): interrupts masked, every
/// DMA1/DMA2 stream and MDMA channel stopped ([`stop_dma`]), and USART3's
/// DMA requests off, so no handler, no `link_uart` transmit stream and no
/// audio or panel transfer runs beside it or puts bytes between the
/// FAULT line's.
fn die(what: fmt::Arguments<'_>) -> ! {
    // PRIMASK = 1 (`cpsid i`): no interrupt or SysTick handler runs from
    // here on, whatever this code runs in (thread mode, an interrupt
    // handler, HardFault). Never undone: `die` does not return, and the
    // code that faulted (or that this preempted) never resumes.
    cortex_m::interrupt::disable();
    stop_dma();
    let prefix = cortex_m::interrupt::free(|cs| PREFIX.borrow(cs).get());
    // SAFETY: interrupts are masked for good (above) and the faulting
    // code never resumes, so this is the only code that runs: these are
    // the only accesses to RCC, GPIOB and USART3 (and the DMA streams are
    // stopped, so no DMA writes USART3 either).
    let (rcc, gpiob, usart) =
        unsafe { (&*pac::RCC::ptr(), &*pac::GPIOB::ptr(), &*pac::USART3::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioben().set_bit());
    let _ = rcc.ahb4enr.read();
    gpiob
        .moder
        .modify(|_, w| w.moder0().output().moder14().output());
    // `link_uart` leaves CR3.DMAT/DMAR set, which `uart::init` (CR1 and
    // BRR only) keeps; with them clear the USART raises no DMA request at
    // all. Only with the clock on: off, nothing can have set them since
    // the reset (reset value 0, RM0433 48.7.3). Neither bit is restricted
    // to UE = 0 (48.7.3 restricts other CR3 bits only).
    if rcc.apb1lenr.read().usart3en().bit_is_set() {
        usart
            .cr3
            .modify(|_, w| w.dmat().clear_bit().dmar().clear_bit());
    }
    let _ = uart::init();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(16) {
            let _ = writeln!(Uart::new(), "\r\n{prefix} FAULT {what}\r");
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

/// The most spins [`stop_dma`] gives one stream or channel per wait:
/// 1,000,000 register reads, over 2 ms at 472 MHz, against 8,754 cycles
/// measured from an MDMA disable to CTCIF (`lcd_dma` `ABORT_CYCLES`) and
/// one byte at 115,200 bps (87 us) for a link transmit stream finishing
/// its current item.
const STOP_SPINS: u32 = 1_000_000;

// DMA_SxCR.EN (RM0433 15.5.5); MDMA_CxCR.EN (14.5.5); MDMA_CxISR TEIF,
// CTCIF, CRQA (14.5.2).
const DMA_SCR_EN: u32 = 1 << 0;
const MDMA_CR_EN: u32 = 1 << 0;
const MDMA_ISR_TEIF: u32 = 1 << 0;
const MDMA_ISR_CTCIF: u32 = 1 << 1;
const MDMA_ISR_CRQA: u32 = 1 << 16;

/// Spins until `done` or [`STOP_SPINS`] reads: a stream that never stops
/// cannot keep the FAULT line from being written.
fn spin_until(done: impl Fn() -> bool) {
    let mut i = 0u32;
    while !done() && i < STOP_SPINS {
        i += 1;
    }
}

/// Stops every stream of DMA1 and DMA2 and every channel of MDMA, the
/// controllers the images use (DMA1 stream 0 `audio_dac`, streams 1 and
/// 2 `link_uart`, MDMA channel 0 `lcd_dma`; DMA2 and the other streams
/// and channels in case a later image uses them). A controller whose RCC
/// clock enable is clear (a fault before the module that uses it, or
/// before `init`) is not touched: nothing can have started it since the
/// reset, and its registers are not accessed without a clock. BDMA (D3)
/// and the masters with their own DMA (DMA2D, Ethernet, SDMMC, USB) are
/// used by no image.
///
/// Stop sequence, RM0433 15.3.16 "DMA transfer suspension" (definitive
/// disable) and 15.3.19 step 1: clear DMA_SxCR.EN, then read EN until it
/// is 0, as the stream first finishes its ongoing transfer (15.5.5 EN:
/// "flag stream ready when read low"). For MDMA, 14.3.15 "MDMA transfer
/// suspension": clear MDMA_CxCR.EN; the ongoing buffer transfer still
/// completes and CTCIF (or TEIF, 14.3.16) marks its end, which is waited
/// for when a request was active (CRQA, 14.5.2); without one no transfer
/// is in progress and EN reading 0 is enough. Every wait is bounded
/// ([`spin_until`]). The flags are left set: interrupts are masked and
/// nothing reprograms the streams afterwards.
fn stop_dma() {
    // SAFETY: shared references to memory-mapped register blocks (RCC,
    // DMA1, DMA2, MDMA; RM0433 Table 8), valid for the whole program. The
    // caller (`die`) has masked interrupts for good and the faulting code
    // never resumes, so these are the only accesses; each EN clear is a
    // read-modify-write that keeps the stream's other bits.
    let (rcc, dma1, dma2, mdma) = unsafe {
        (
            &*pac::RCC::ptr(),
            &*pac::DMA1::ptr(),
            &*pac::DMA2::ptr(),
            &*pac::MDMA::ptr(),
        )
    };
    let ahb1 = rcc.ahb1enr.read();
    for (on, dma) in [
        (ahb1.dma1en().bit_is_set(), dma1),
        (ahb1.dma2en().bit_is_set(), dma2),
    ] {
        if !on {
            continue;
        }
        for st in dma.st.iter() {
            if st.cr.read().bits() & DMA_SCR_EN == 0 {
                continue;
            }
            // SAFETY: EN cleared, the other bits kept.
            st.cr
                .modify(|r, w| unsafe { w.bits(r.bits() & !DMA_SCR_EN) });
            spin_until(|| st.cr.read().bits() & DMA_SCR_EN == 0);
        }
    }
    if rcc.ahb3enr.read().mdmaen().bit_is_clear() {
        return;
    }
    let channels = [
        &mdma.ch0, &mdma.ch1, &mdma.ch2, &mdma.ch3, &mdma.ch4, &mdma.ch5, &mdma.ch6, &mdma.ch7,
        &mdma.ch8, &mdma.ch9, &mdma.ch10, &mdma.ch11, &mdma.ch12, &mdma.ch13, &mdma.ch14,
        &mdma.ch15,
    ];
    for ch in channels {
        if ch.cr.read().bits() & MDMA_CR_EN == 0 {
            continue;
        }
        let active = ch.isr.read().bits() & MDMA_ISR_CRQA != 0;
        // SAFETY: EN cleared, the other bits kept.
        ch.cr
            .modify(|r, w| unsafe { w.bits(r.bits() & !MDMA_CR_EN) });
        spin_until(|| ch.cr.read().bits() & MDMA_CR_EN == 0);
        if active {
            spin_until(|| ch.isr.read().bits() & (MDMA_ISR_CTCIF | MDMA_ISR_TEIF) != 0);
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    die(format_args!("panic {info}"))
}

// HardFault, straight from the vector table (this symbol replaces
// cortex-m-rt's handler and its trampoline). The exception frame is at the
// main stack pointer; no other stack is ever used in thread mode.
// The fault may be the stack itself (an overflow, RAM that does not
// answer), so before any Rust runs the stack pointer moves to
// `__fault_stack_top`, the top of the `.fault_stack` reservation in AXI
// SRAM that every image's memory.x defines (memory_x::firmware_memory_x),
// which nothing else uses; the handler never returns.
core::arch::global_asm!(
    ".section .text.HardFault, \"ax\", %progbits",
    ".global HardFault",
    ".type HardFault, %function",
    ".thumb_func",
    "HardFault:",
    "    mrs r0, msp", // the exception frame
    "    mov r1, lr",  // EXC_RETURN
    "    ldr r2, =__fault_stack_top",
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
    // Stacked only if neither the bus (BFSR.STKERR, CFSR bit 12) nor the
    // MPU (MMFSR.MSTKERR, bit 4: a stack overflow into the stack guard,
    // `mpu::REGION_STACK_GUARD`) refused the stacking; reading a frame in
    // the guard would fault again here and lock the core up.
    let stacked = in_ram && cfsr & (1 << 12 | 1 << 4) == 0;
    let (lr, pc) = if stacked {
        // SAFETY: the eight words of the frame are inside DTCM or AXI SRAM
        // (checked above) and the CPU did stack them (STKERR and MSTKERR
        // are clear); a volatile read cannot disturb anything.
        unsafe { (frame.add(5).read_volatile(), frame.add(6).read_volatile()) }
    } else {
        (0, 0)
    };
    die(format_args!(
        "hardfault sp={at:#010x} exc_return={exc_return:#010x} frame={} pc={pc:#010x} \
         lr={lr:#010x} cfsr={cfsr:#010x} hfsr={hfsr:#010x} bfar={bfar:#010x} mmfar={mmfar:#010x}",
        if stacked { "ok" } else { "none" }
    ))
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) -> ! {
    die(format_args!("unexpected exception {irqn}"))
}
