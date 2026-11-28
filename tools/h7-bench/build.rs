//! Writes, into `OUT_DIR`:
//! - the workload ROMs: the synthetic ones from the bench's own generator
//!   (`tools/bench/src/synth.rs`, compiled into this script, so the programs
//!   are never copied) and cpu_instrs from `H7_BENCH_CPU_INSTRS` (ROMs are
//!   never committed);
//! - `memory.x` for the chosen placement, from the shared generator
//!   (`tools/memory-x`, ADR 0036), with the `.itcm` input-section rules
//!   generated from `perf/itcm-sections.txt` (docs/perf.md "ITCM
//!   placement plan": one source of truth for the list). The board images
//!   get `firmware_memory_x`: the gbc-nucleo-h753 lib they link reads its
//!   TCM and fault-stack symbols from every image. The QEMU image links no
//!   lib and gets the plain map.
//!
//! The map goes on the link line through `rustc-link-search`. The lib's
//! own `memory.x` (for its bins) is passed with `rustc-link-arg-bins` in
//! its package and does not reach this link (ADR 0036; build.sh checks
//! the placement-specific initial SP and startup symbols of every image).

use std::path::{Path, PathBuf};
use std::{env, fs};

use memory_x::{Chip, Data, ItcmSet};

#[allow(dead_code)]
#[path = "../bench/src/synth.rs"]
mod synth;

/// The synthetic workloads, in report order (`workload::WORKLOADS`, minus
/// cpu_instrs).
const SYNTH: [&str; 14] = [
    "worst-case",
    "cgb-raster",
    "dmg-raster",
    "event-storm",
    "dma-storm",
    "halt-irq",
    "apu-writes",
    "oam-lines",
    "storm-control",
    "storm-apu",
    "storm-spsw",
    "storm-div",
    "storm-gdma",
    "storm-ppu",
];
const CPU_INSTRS_DEFAULT: &str = "/home/jess/gbc-32/test-roms/blargg/cpu_instrs/cpu_instrs.gb";
const ITCM_SET: &str = "../../perf/itcm-sections.txt";

fn feature(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some()
}

/// `memory.x` for the chosen placement (tools/memory-x). The
/// tiers of the set file left in flash come from `H7_BENCH_ITCM_DROP`,
/// comma-separated substrings of their `# Tier ...` headers (empty: drop
/// nothing; default `memory_x::ITCM_DROP_DEFAULT`); build.sh reads the same
/// variable for its post-link check.
fn generate_memory_x() -> String {
    let data = if feature("dtcm") {
        Data::Dtcm
    } else {
        Data::AxiSram
    };
    let set = feature("itcm").then(|| {
        let text = fs::read_to_string(ITCM_SET).unwrap_or_else(|e| panic!("{ITCM_SET}: {e}"));
        let drop =
            env::var("H7_BENCH_ITCM_DROP").unwrap_or_else(|_| memory_x::ITCM_DROP_DEFAULT.into());
        // A/B layout sets only (layout-set.sh): bytes of pad at the start
        // of `.itcm` (memory_x::ItcmSet::with_pad). Unset in every gate image.
        let pad = env::var("H7_BENCH_ITCM_PAD").map_or(0, |p| {
            p.parse()
                .unwrap_or_else(|e| panic!("H7_BENCH_ITCM_PAD={p}: {e}"))
        });
        ItcmSet::parse(&text, &memory_x::drop_list(&drop)).with_pad(pad)
    });
    if feature("qemu") {
        memory_x::memory_x(Chip::Qemu, data, set.as_ref())
    } else {
        memory_x::firmware_memory_x(Chip::H753, data, set.as_ref())
    }
}

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../bench/src/synth.rs");
    println!("cargo:rerun-if-changed={ITCM_SET}");
    println!("cargo:rerun-if-env-changed=H7_BENCH_CPU_INSTRS");
    println!("cargo:rerun-if-env-changed=H7_BENCH_ITCM_DROP");
    println!("cargo:rerun-if-env-changed=H7_BENCH_ITCM_PAD");

    let (itcm, dtcm, qemu) = (feature("itcm"), feature("dtcm"), feature("qemu"));
    assert!(
        qemu != feature("board"),
        "exactly one of `board` (the default) and `qemu` (build with --no-default-features)"
    );
    assert!(!(itcm && !dtcm), "feature `itcm` needs `dtcm`");
    assert!(
        !(qemu && feature("selftest")),
        "feature `qemu` excludes `selftest`"
    );

    fs::write(out.join("memory.x"), generate_memory_x()).expect("write memory.x");
    println!("cargo:rustc-link-search={}", out.display());

    // The selftest image has no emulator and needs no ROM.
    if feature("selftest") {
        return;
    }
    for name in SYNTH {
        let rom = synth::build(name).unwrap_or_else(|| panic!("no synthetic ROM {name}"));
        fs::write(out.join(format!("{name}.gb")), rom).expect("write ROM");
    }
    let path = env::var("H7_BENCH_CPU_INSTRS").unwrap_or_else(|_| CPU_INSTRS_DEFAULT.into());
    println!("cargo:rerun-if-changed={path}");
    let rom = fs::read(Path::new(&path)).unwrap_or_else(|e| {
        panic!(
            "\n\nh7-bench: cannot read the cpu_instrs ROM at {path}: {e}\n\
             Run scripts/fetch-test-roms.sh in the repo, or point \
             H7_BENCH_CPU_INSTRS at blargg/cpu_instrs/cpu_instrs.gb.\n\n"
        )
    });
    fs::write(out.join("cpu_instrs.gb"), rom).expect("write ROM");
}
