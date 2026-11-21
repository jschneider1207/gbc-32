//! Acceptance runner. Contract: `acceptance/README.md`.

// The modules are wired into `main` in the next commit.
#![allow(dead_code)]

mod cli;
mod glob;
mod manifest;

fn main() {
    // Exit code 2 = runner error, so a missing runner can never look like
    // ROMs simply failing.
    eprintln!("romtest: not implemented");
    std::process::exit(2);
}
