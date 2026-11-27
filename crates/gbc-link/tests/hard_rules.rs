//! Enforces gbc-link's rules (plan `docs/plan/stage-2.md` Crate layout,
//! D9): `no_std`, no `alloc` or `std`, `#![forbid(unsafe_code)]`, no
//! `#[path]` or `include!`, no build script or `[lib] path`, and no
//! dependencies at all except the `hard-rules` dev-dependency that holds
//! the checker.
//!
//! The checker lives in `tools/hard-rules` (a dev-dependency, so it never
//! enters the no_std build); the checker's own tests are run from here too
//! so each package still reports them.

use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

#[test]
fn link_sources_follow_the_rules() {
    hard_rules::check_sources(&crate_dir(), "gbc-link", 4);
}

#[test]
fn link_has_no_dependencies() {
    let toml = std::fs::read_to_string(crate_dir().join("Cargo.toml")).unwrap();
    hard_rules::check_dependencies(
        &toml,
        "gbc-link (D9)",
        &[],
        &["hard-rules = { path = \"../../tools/hard-rules\" }"],
    );
}

#[test]
fn link_has_no_build_script_or_lib_path() {
    hard_rules::check_no_build_script_or_lib_path(&crate_dir(), "gbc-link");
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
