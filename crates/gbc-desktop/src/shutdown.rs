//! Ctrl+C / SIGTERM (and Windows console close) ask for a clean shutdown,
//! so the exit save still runs (M7 review). The loops poll [`requested`].

use std::sync::atomic::{AtomicBool, Ordering};

static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Installs the handler. A failure only warns: the run works, it just
/// loses the exit save on a signal.
pub fn install() {
    if let Err(e) = ctrlc::set_handler(|| REQUESTED.store(true, Ordering::SeqCst)) {
        eprintln!("warning: can't catch Ctrl+C ({e}); a signal skips the exit save");
    }
}

/// Whether a shutdown signal arrived.
pub fn requested() -> bool {
    REQUESTED.load(Ordering::SeqCst)
}
