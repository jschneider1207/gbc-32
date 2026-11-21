//! Acceptance runner. Contract: `acceptance/README.md`; design: ADR 0007.
//!
//! Exit codes: 0 = ran, no regressions, `--gate` tier passing; 1 = ran, but
//! regressions or gate failures; 2 = runner error (results meaningless).

mod app;
mod checks;
mod cli;
mod glob;
mod machine;
mod manifest;
mod report;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn main() -> ExitCode {
    app::install_panic_hook();
    // Any panic outside a ROM run is a runner bug: exit 2, never 1.
    let code = std::panic::catch_unwind(real_main).unwrap_or_else(|_| {
        eprintln!("romtest: internal error (panic)");
        2
    });
    ExitCode::from(code)
}

fn real_main() -> u8 {
    let args = match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli::Command::Run(args)) => args,
        Ok(cli::Command::Help) => {
            println!("{}", cli::USAGE);
            return 0;
        }
        Err(e) => {
            eprintln!("romtest: {e}");
            return 2;
        }
    };
    let result = app::Paths::new(&workspace_root(), &args.manifest).and_then(|paths| {
        let mut out = std::io::stdout().lock();
        app::run(&args, &paths, &app::core_backend, &mut out)
    });
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("romtest: error: {e}");
            eprintln!("romtest: runner error, exit 2 (results are not meaningful)");
            2
        }
    }
}

/// The workspace root (`test-roms/` and `target/` live there), found from
/// this crate's location so the runner works from any directory.
fn workspace_root() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.canonicalize().unwrap_or(root)
}
