//! Acceptance runner. Contract: `acceptance/README.md`; design: ADR 0007.
//!
//! Exit codes: 0 = ran, no regressions, `--gate` tier passing; 1 = ran, but
//! regressions or gate failures; 2 = runner error (results meaningless).

// Emulator panics are caught per ROM and become `crash`; with
// `panic = "abort"` one would kill the whole run instead.
#[cfg(not(panic = "unwind"))]
compile_error!(
    "romtest must be built with panic = \"unwind\" (acceptance/README.md, \"Statuses\"): \
     it catches emulator panics per ROM"
);

mod app;
mod checks;
mod cli;
mod gambatte;
mod glob;
mod integrity;
mod machine;
mod manifest;
mod panics;
mod report;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    panics::install_hook();
    let code = app::main_with(
        std::env::args_os().skip(1),
        &workspace_root(),
        &app::core_backend,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    ExitCode::from(code)
}

/// The workspace root (`test-roms/` and `target/` live there), found from
/// this crate's location so the runner works from any directory.
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}
