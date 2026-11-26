//! The acceptance runner's modules, as a library so other host tools can
//! reuse its manifest parsing and run loop read-only (Stage 2 M5:
//! `tools/replay-input` converts a group's `input` with
//! [`manifest::parse`] and [`machine::input_timeline`], and checks the
//! firmware's replay against [`machine::run`]). The binary (`main.rs`) is
//! the runner; contract: `acceptance/README.md`.

pub mod app;
mod checks;
mod cli;
mod gambatte;
mod glob;
mod integrity;
pub mod machine;
pub mod manifest;
pub mod panics;
mod report;
