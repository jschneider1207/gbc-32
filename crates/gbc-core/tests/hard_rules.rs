//! Enforces gbc-core's hard rules (AGENTS.md, ADR 0001, ADR 0009): `no_std`,
//! no `alloc` or `std` outside inline `#[cfg(test)] mod`s, no `#[path]` or
//! `include!`, no `unsafe` (`#![forbid(unsafe_code)]` in `lib.rs`), no build
//! script or `[lib] path`, and no dependencies at all except the
//! `hard-rules` dev-dependency that holds this checker (a normal or build
//! dependency, any other dev-dependency or any other table fails).
//!
//! The checker lives in `tools/hard-rules` (see its crate docs for the
//! exact rules and the test-code rule). It is a dev-dependency, so it never
//! enters the no_std build; `cargo tree -p gbc-core -e normal,build` stays
//! empty. The checker's own tests are run from here too so each package
//! still reports them.

use std::path::{Path, PathBuf};

fn core_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn core_sources_follow_the_hard_rules() {
    // More than 5 files: guards against scanning the wrong directory.
    hard_rules::check_sources(&core_dir(), "gbc-core", 6);
}

#[test]
fn core_has_no_dependencies() {
    let toml = std::fs::read_to_string(core_dir().join("Cargo.toml")).unwrap();
    hard_rules::check_dependencies(
        &toml,
        "gbc-core (AGENTS.md: adding a dependency needs human approval)",
        &[],
        &["hard-rules = { path = \"../../tools/hard-rules\" }"],
    );
}

#[test]
fn core_has_no_build_script_or_lib_path() {
    hard_rules::check_no_build_script_or_lib_path(&core_dir(), "gbc-core");
}

#[test]
fn allows_test_modules_comments_strings_and_core_alloc() {
    hard_rules::selftest::allows_test_modules_comments_strings_and_core_alloc();
}

#[test]
fn catches_extern_crate_however_written() {
    hard_rules::selftest::catches_extern_crate_however_written();
}

#[test]
fn catches_path_and_include() {
    hard_rules::selftest::catches_path_and_include();
}

#[test]
fn catches_std_and_alloc_paths() {
    hard_rules::selftest::catches_std_and_alloc_paths();
}

#[test]
fn catches_unsafe_anywhere() {
    hard_rules::selftest::catches_unsafe_anywhere();
}

#[test]
fn crate_attributes_are_found() {
    hard_rules::selftest::crate_attributes_are_found();
}

#[test]
fn dependency_tables_are_found() {
    hard_rules::selftest::dependency_tables_are_found();
}

#[test]
fn escaped_quote_char_literals_lex_as_one_token() {
    hard_rules::selftest::escaped_quote_char_literals_lex_as_one_token();
}

#[test]
fn leading_inner_attributes_only() {
    hard_rules::selftest::leading_inner_attributes_only();
}

#[test]
fn manifest_escape_hatches_are_found() {
    hard_rules::selftest::manifest_escape_hatches_are_found();
}
