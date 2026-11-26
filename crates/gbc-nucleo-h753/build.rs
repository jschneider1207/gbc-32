//! Writes this package's `memory.x` into `OUT_DIR` from the shared
//! generator (`tools/memory-x`, ADR 0036) and puts it on the link line of
//! this package's bins only.
//!
//! `rustc-link-arg-bins` and not `rustc-link-search`: a search path set
//! here would propagate to every package that depends on this lib
//! (tools/h7-bench), whose own build script writes its own `memory.x`; two
//! on the search path and the first one found wins. The bins-only argument
//! stays in this package.
//!
//! The M1 layout: the stack and statics in DTCM, no ITCM set (the `boot`
//! image links no gbc-core, so a set would leave `.itcm` nearly empty and
//! fail its ASSERT). All bins of a package share one build script, so when
//! the `headless` bin (gbc-core with the ITCM set) arrives, the set is
//! chosen by a cargo feature or the bin moves to its own package.

use std::path::PathBuf;
use std::{env, fs};

use memory_x::{Chip, Data};

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=build.rs");
    fs::write(
        out.join("memory.x"),
        memory_x::firmware_memory_x(Chip::H753, Data::Dtcm, None),
    )
    .expect("write memory.x");
    println!("cargo:rustc-link-arg-bins=-L{}", out.display());
}
