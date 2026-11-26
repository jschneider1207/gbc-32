//! The user LEDs. UM2407 section 7.6.1: LD1 green on PB0 (SB39 on, SB47
//! off), LD2 yellow on PE1, LD3 red on PB14; "on when the I/O is HIGH".
//! LD1 is the liveness sign, LD2 is on once `init` is through, LD3 means a
//! fault.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::pac;

static LD1_ON: AtomicBool = AtomicBool::new(false);

/// Sets the three LED pins up as outputs, LD1 on, LD2 and LD3 off. GPIOB
/// and GPIOE clocks must be on.
pub(crate) fn init(gpiob: &pac::GPIOB, gpioe: &pac::GPIOE) {
    gpiob
        .moder
        .modify(|_, w| w.moder0().output().moder14().output());
    gpioe.moder.modify(|_, w| w.moder1().output());
    gpiob.bsrr.write(|w| w.bs0().set_bit().br14().set_bit());
    gpioe.bsrr.write(|w| w.br1().set_bit());
    LD1_ON.store(true, Ordering::Relaxed);
}

/// LD2 (yellow) on: `init` is through.
pub(crate) fn ld2_on() {
    // SAFETY: a single write to the write-only GPIOE_BSRR, which exists to
    // make pin updates atomic; nothing else in this lib owns PE1.
    let gpioe = unsafe { &*pac::GPIOE::ptr() };
    gpioe.bsrr.write(|w| w.bs1().set_bit());
}

/// Toggles LD1 (green).
pub fn toggle() {
    let on = !LD1_ON.load(Ordering::Relaxed);
    LD1_ON.store(on, Ordering::Relaxed);
    // SAFETY: a single write to the write-only GPIOB_BSRR, which exists to
    // make pin updates atomic; nothing else in this lib owns PB0.
    let gpiob = unsafe { &*pac::GPIOB::ptr() };
    gpiob.bsrr.write(|w| {
        if on {
            w.bs0().set_bit()
        } else {
            w.br0().set_bit()
        }
    });
}
