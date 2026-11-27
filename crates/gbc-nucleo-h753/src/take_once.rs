//! Take-once statics (review Q4): a buffer in a `static`, handed out once
//! as a `&'static mut` for the life of the program.
//!
//! [`take_once!`](crate::take_once!) is the one place that does it:
//!
//! ```ignore
//! let frame: Option<&'static mut Frame> = take_once!(
//!     // SAFETY: (why the section is right for the type)
//!     #[unsafe(link_section = ".axisram.frame")]
//!     Frame = [0; SCREEN_WIDTH * SCREEN_HEIGHT]
//! );
//! ```
//!
//! It evaluates to `Some` the first time that call site runs and `None`
//! every time after. Each call site declares two statics of its own:
//!
//! - the buffer, a [`Slot`], which gets the call's attributes (typically
//!   a `link_section`);
//! - its guard, an `AtomicBool`, which never does. The guard has to be
//!   apart from the buffer: `.axisram` is a NOLOAD section
//!   (`memory_x::firmware_memory_x`), not zeroed or loaded at boot, so a
//!   flag inside the buffer would start as whatever the SRAM held. The
//!   guard is `AtomicBool::new(false)`, all zero bits, so it lands in
//!   `.bss`, which cortex-m-rt's startup code zeroes before `main`.
//!
//! Soundness rests on scoping: statics declared in the macro's block can
//! be named only inside it, so each [`Slot`] has exactly one guard and is
//! reached only through that block's one [`Slot::take`] call. A second
//! call site is a second pair of statics.
//!
//! The guard is an atomic swap: on the Cortex-M7 an LDREXB/STREXB loop,
//! and exception entry and return clear the exclusive monitor, so an
//! interrupt that takes the same slot between the load and the store makes
//! the interrupted store fail and retry; exactly one caller ever reads
//! `false`. The flag publishes nothing (it is never reset, and the
//! buffer's initial value is written by the startup code before `main`),
//! so `Relaxed` would do; `AcqRel` costs one DMB per buffer, once, at
//! init.
//!
//! Host-tested by `tools/memory-x/tests/take_once.rs`, which compiles
//! this file with `#[path]` (this crate builds only for thumbv7em).

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// A buffer declared by [`take_once!`](crate::take_once!). Not for direct
/// use: only the macro pairs it with its guard.
#[doc(hidden)]
pub struct Slot<T>(UnsafeCell<T>);

// SAFETY: the value is reached only through `Slot::take`, which hands
// out at most one `&mut T` for the life of the program (its guard), so no
// two contexts ever access it; `T: Send` because that one reference may
// be used in another context (say, an interrupt handler) than the one
// that took it.
unsafe impl<T: Send> Sync for Slot<T> {}

impl<T> Slot<T> {
    #[doc(hidden)]
    pub const fn new(value: T) -> Self {
        Slot(UnsafeCell::new(value))
    }

    /// The one `&'static mut` to the value: `Some` on the first call,
    /// `None` on every later one.
    ///
    /// # Safety
    /// `taken` is this slot's guard and no other slot's, and every access
    /// to this slot goes through this function with it (what
    /// [`take_once!`](crate::take_once!) ensures).
    // A `&mut` from a shared `&self` is the point: the guard makes it
    // the only one.
    #[allow(clippy::mut_from_ref)]
    #[doc(hidden)]
    pub unsafe fn take(&'static self, taken: &'static AtomicBool) -> Option<&'static mut T> {
        if taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        // SAFETY: the swap lets exactly one caller past for this guard,
        // and by the caller's contract the guard is this slot's only one,
        // so this is the only reference to the value there will ever be.
        Some(unsafe { &mut *self.0.get() })
    }
}

/// `take_once!($(#[attr])* Type = init)`: a `static` buffer of `Type`
/// initialised to `init` (a constant expression), as
/// `Option<&'static mut Type>`: `Some` the first time this call site runs,
/// `None` after. The attributes go on the buffer only; its guard stays in
/// `.bss` (module docs).
#[macro_export]
macro_rules! take_once {
    ($(#[$($attr:tt)*])* $ty:ty = $init:expr $(,)?) => {{
        $(#[$($attr)*])*
        static SLOT: $crate::take_once::Slot<$ty> = $crate::take_once::Slot::new($init);
        // Zero bits: `.bss`, zeroed at boot, whatever section SLOT is in.
        static TAKEN: ::core::sync::atomic::AtomicBool =
            ::core::sync::atomic::AtomicBool::new(false);
        // SAFETY: SLOT and TAKEN are declared in this block and can be
        // named nowhere else, so TAKEN is SLOT's only guard and this is
        // the only access to SLOT.
        unsafe { $crate::take_once::Slot::take(&SLOT, &TAKEN) }
    }};
}
