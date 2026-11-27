//! RCC peripheral clock enables and resets (review Q6). Every driver's
//! read-modify-write of RCC_AHBxENR, APBxENR, AHBxRSTR (and the kernel
//! clock selection next to them) goes through [`modify`], which runs it
//! with interrupts masked.
//!
//! No interrupt handler touches RCC today (the table in the crate docs),
//! so the mask changes nothing at run time; it makes "no handler splits
//! an RCC read-modify-write" a property of this function rather than a
//! claim each driver restates, and keeps it true if a handler ever does.
//! All callers are init-time driver setup, none on a timed path.
//!
//! Not routed here: `fault::die`, which runs with PRIMASK set for good
//! and never returns, and `audio_dac::AudioDac::new` (review Q6 follow-up).

use crate::pac;

/// Runs `f` on the RCC register block with interrupts masked (PRIMASK;
/// restored afterwards, so it may be called with them already masked, as
/// the fault path's `uart::init` does).
#[inline]
pub fn modify<R>(f: impl FnOnce(&'static pac::rcc::RegisterBlock) -> R) -> R {
    cortex_m::interrupt::free(|_| {
        // SAFETY: the RCC register block is memory-mapped at a fixed
        // address (RM0433 Table 8) for the whole program, and every access
        // through it is a volatile PAC access. One core, one thread, and
        // interrupts masked: nothing runs between the read and the write
        // of a read-modify-write in `f` except a fault, whose handler
        // never returns into it (crate docs, "Interrupt handlers").
        f(unsafe { &*pac::RCC::ptr() })
    })
}
