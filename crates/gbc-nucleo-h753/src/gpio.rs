//! GPIO pin setup shared by the drivers ([`crate::qspi`], [`crate::lcd`]).
//! Register layout: RM0433 Rev 9 section 11.4.

use crate::pac;

/// Configures one pin of a port as alternate function `af`, very high
/// speed, push-pull, with a pull-up if `pull_up`. The order is that of
/// `uart::init`: AF and output settings first, MODER last, so the pin
/// never drives the AF signal with stale settings.
pub(crate) fn pin_af(gpio: &pac::gpioa::RegisterBlock, pin: u32, af: u32, pull_up: bool) {
    // `field(r, shift, mask, v)`: `r` with one pin's field set to `v`.
    let field = |r: u32, shift: u32, mask: u32, v: u32| (r & !(mask << shift)) | (v << shift);
    let two = pin * 2;
    let pupd = u32::from(pull_up);
    // Each write below is a read-modify-write of one pin's field with a
    // value valid for it (RM0433 section 11.4: AF 0..15; OTYPER 0
    // push-pull; OSPEEDR 0b11 very high; PUPDR 0b00 none or 0b01 pull-up;
    // MODER 0b10 alternate function); the other pins' fields are kept.
    if pin < 8 {
        // SAFETY: see above.
        gpio.afrl
            .modify(|r, w| unsafe { w.bits(field(r.bits(), pin * 4, 0xF, af)) });
    } else {
        // SAFETY: see above.
        gpio.afrh
            .modify(|r, w| unsafe { w.bits(field(r.bits(), (pin - 8) * 4, 0xF, af)) });
    }
    // SAFETY: see above.
    gpio.otyper
        .modify(|r, w| unsafe { w.bits(field(r.bits(), pin, 1, 0)) });
    // SAFETY: see above.
    gpio.ospeedr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0b11)) });
    // SAFETY: see above.
    gpio.pupdr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, pupd)) });
    // SAFETY: see above.
    gpio.moder
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0b10)) });
}

/// Configures one pin of a port as a push-pull output, low speed, no pull,
/// driving `high` from the moment it becomes an output (BSRR before
/// MODER).
pub(crate) fn pin_output(gpio: &pac::gpioa::RegisterBlock, pin: u32, high: bool) {
    let field = |r: u32, shift: u32, mask: u32, v: u32| (r & !(mask << shift)) | (v << shift);
    let two = pin * 2;
    let bit = if high { 1 << pin } else { 1 << (pin + 16) };
    // SAFETY: BSRR is write-only and only affects the pins whose bits are
    // set; the others are read-modify-writes of this pin's field with a
    // valid value (OTYPER 0 push-pull, OSPEEDR 0b00 low, PUPDR 0b00 none,
    // MODER 0b01 output), as in `pin_af`.
    gpio.bsrr.write(|w| unsafe { w.bits(bit) });
    // SAFETY: see above.
    gpio.otyper
        .modify(|r, w| unsafe { w.bits(field(r.bits(), pin, 1, 0)) });
    // SAFETY: see above.
    gpio.ospeedr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0)) });
    // SAFETY: see above.
    gpio.pupdr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0)) });
    // SAFETY: see above.
    gpio.moder
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0b01)) });
}

/// Configures one pin of a port as an input without pull.
pub(crate) fn pin_input(gpio: &pac::gpioa::RegisterBlock, pin: u32) {
    let field = |r: u32, shift: u32, mask: u32, v: u32| (r & !(mask << shift)) | (v << shift);
    let two = pin * 2;
    // SAFETY: read-modify-writes of this pin's fields with valid values
    // (PUPDR 0b00 none, MODER 0b00 input).
    gpio.pupdr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0)) });
    // SAFETY: see above.
    gpio.moder
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0)) });
}
