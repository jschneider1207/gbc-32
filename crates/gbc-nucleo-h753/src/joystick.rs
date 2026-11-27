//! The board's six raw inputs (plan Stage 2 M5, decision D2) as
//! `gbc_firmware::buttons::RawInputs`: the X-NUCLEO-GFX02Z1 joystick and
//! the Nucleo's B1.
//!
//! - Joystick (UM2905 section 5.4, Table 4): LEFT PE3, CENTER PA0, DOWN
//!   PF8, UP PF9, RIGHT PF7, each "active low", the common contact to
//!   GND. UM2905 names no pull-up resistor for these lines (its text and
//!   tables; the schematic was not checked), so the pins get the internal
//!   pull-ups (RM0433 11.4.4; ~40 kOhm, DS12117 Rev 10 Table 146, rev V: RPU 30 to 50 kOhm);
//!   they would be harmless next to external ones.
//! - B1 USER (UM2407 section 7.6.2 and Table 10, "B1 USER"): PC13 by
//!   default (SB51 on, SB58 off). UM2407's text names only the pin, not
//!   the polarity: active high as on every Nucleo-144 (pressed drives the
//!   pin to VDD; to be confirmed by the human's button check, which the
//!   `joy=` echo bit 5 shows). The internal pull-down is set, so an open
//!   line reads released.
//!
//! [`Joystick::read`] returns 1 = pressed per line; debounce and mapping
//! are gbc-firmware's (`buttons::Debouncer`, `to_buttons`).

use gbc_firmware::buttons::{Raw, RawInputs};

use crate::pac;

// RCC_AHB4ENR (RM0433 8.7.43): GPIOA, C, E, F clocks.
const AHB4_GPIOA: u32 = 1 << 0;
const AHB4_GPIOC: u32 = 1 << 2;
const AHB4_GPIOE: u32 = 1 << 4;
const AHB4_GPIOF: u32 = 1 << 5;

const PULL_UP: u32 = 0b01;
const PULL_DOWN: u32 = 0b10;

fn port(p: *const pac::gpioa::RegisterBlock) -> &'static pac::gpioa::RegisterBlock {
    // SAFETY: only the input pins listed in the module docs are touched,
    // by read-modify-write of their own MODER/PUPDR fields, and IDR reads.
    unsafe { &*p }
}

fn gpioa() -> &'static pac::gpioa::RegisterBlock {
    port(pac::GPIOA::ptr())
}
fn gpioc() -> &'static pac::gpioa::RegisterBlock {
    port(pac::GPIOC::ptr() as *const _)
}
fn gpioe() -> &'static pac::gpioa::RegisterBlock {
    port(pac::GPIOE::ptr() as *const _)
}
fn gpiof() -> &'static pac::gpioa::RegisterBlock {
    port(pac::GPIOF::ptr() as *const _)
}

/// Makes `pin` an input with `pull`.
fn input(gpio: &pac::gpioa::RegisterBlock, pin: u32, pull: u32) {
    let shift = 2 * pin;
    // SAFETY: only this pin's 2-bit fields change; 00 = input mode.
    unsafe {
        gpio.pupdr
            .modify(|r, w| w.bits(r.bits() & !(0b11 << shift) | pull << shift));
        gpio.moder
            .modify(|r, w| w.bits(r.bits() & !(0b11 << shift)));
    }
}

/// The six input pins (module docs).
pub struct Joystick(());

impl Joystick {
    /// Clocks the ports and sets the pins up.
    pub fn new() -> Self {
        // RCC_AHB4ENR: four port clocks; the read back makes the clocks
        // run before the first port access.
        crate::rcc::modify(|rcc| {
            // SAFETY: the four GPIO clock-enable bits are added to the
            // value read, every other bit kept.
            rcc.ahb4enr.modify(|r, w| unsafe {
                w.bits(r.bits() | AHB4_GPIOA | AHB4_GPIOC | AHB4_GPIOE | AHB4_GPIOF)
            });
            let _ = rcc.ahb4enr.read();
        });
        input(gpioe(), 3, PULL_UP);
        input(gpioa(), 0, PULL_UP);
        input(gpiof(), 7, PULL_UP);
        input(gpiof(), 8, PULL_UP);
        input(gpiof(), 9, PULL_UP);
        input(gpioc(), 13, PULL_DOWN);
        Joystick(())
    }
}

impl Default for Joystick {
    fn default() -> Self {
        Self::new()
    }
}

impl RawInputs for Joystick {
    fn read(&mut self) -> Raw {
        let low = |g: &pac::gpioa::RegisterBlock, pin: u32| g.idr.read().bits() & (1 << pin) == 0;
        let f = gpiof().idr.read().bits();
        let mut r = Raw::NONE;
        for (pressed, line) in [
            (low(gpioe(), 3), Raw::LEFT),
            (low(gpioa(), 0), Raw::CENTER),
            (f & (1 << 8) == 0, Raw::DOWN),
            (f & (1 << 9) == 0, Raw::UP),
            (f & (1 << 7) == 0, Raw::RIGHT),
            (gpioc().idr.read().bits() & (1 << 13) != 0, Raw::B1),
        ] {
            if pressed {
                r = r | line;
            }
        }
        r
    }
}
