//! The panic boundary around calls into the emulator core.
//!
//! Only core calls are wrapped (see `machine::core_call`): a panic there is
//! the emulator's fault and becomes the ROM's `crash` status. A panic
//! anywhere else is a runner bug; it propagates, and `app::main` turns it
//! into exit 2 so it can never look like a ROM result.

use std::cell::{Cell, RefCell};
use std::panic::{AssertUnwindSafe, catch_unwind};

thread_local! {
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Installs a panic hook that records panics inside [`catch_panic`]
/// (message and location) instead of printing them; other panics go to the
/// default hook.
pub fn install_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if CAPTURING.with(Cell::get) {
            LAST_PANIC.with(|p| *p.borrow_mut() = Some(info.to_string().replace('\n', " ")));
        } else {
            default(info);
        }
    }));
}

/// Runs `f`, turning a panic into `Err(message)`.
pub fn catch_panic<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    LAST_PANIC.with(|p| p.borrow_mut().take());
    let was = CAPTURING.with(|c| c.replace(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    CAPTURING.with(|c| c.set(was));
    r.map_err(|payload| {
        LAST_PANIC
            .with(|p| p.borrow_mut().take())
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with a non-string payload".into())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_panic_returns_the_message() {
        let r: Result<(), String> = catch_panic(|| panic!("boom {}", 7));
        assert!(r.unwrap_err().contains("boom 7"));
        assert_eq!(catch_panic(|| 5), Ok(5));
    }
}
