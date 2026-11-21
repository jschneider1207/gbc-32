//! Acceptance runner. Contract: `acceptance/README.md`.

fn main() {
    // Exit code 2 = runner error, so a missing runner can never look like
    // ROMs simply failing.
    eprintln!("romtest: not implemented");
    std::process::exit(2);
}
