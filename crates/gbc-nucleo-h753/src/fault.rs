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
fn die(what: fmt::Arguments<'_>) -> ! {
    let prefix = cortex_m::interrupt::free(|cs| PREFIX.borrow(cs).get());
    // SAFETY: nothing else runs any more, so these are the only accesses
    // to RCC and GPIOB.
    let (rcc, gpiob) = unsafe { (&*pac::RCC::ptr(), &*pac::GPIOB::ptr()) };
    rcc.ahb4enr.modify(|_, w| w.gpioben().set_bit());
    let _ = rcc.ahb4enr.read();
    gpiob
        .moder
        .modify(|_, w| w.moder0().output().moder14().output());
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
