//! Writes this package's `memory.x` into `OUT_DIR` from the shared
//! generator (`tools/memory-x`, ADR 0036) and puts it on the link line of
//! this package's bins only; with the `headless` feature also the ROM and
//! input script that image embeds.
//!
//! `rustc-link-arg-bins` and not `rustc-link-search`: a search path set
//! here would propagate to every package that depends on this lib
//! (tools/h7-bench), whose own build script writes its own `memory.x`; two
//! on the search path and the first one found wins. The bins-only argument
//! stays in this package.
//!
//! Placement. All bins of a package share this script and so one
//! `memory.x`; the placement is chosen per build by the `headless` feature
//! (Cargo.toml), not per bin:
//! - without it (`boot`): the stack and statics in DTCM, no ITCM set (the
//!   image links no gbc-core, so a set would leave `.itcm` nearly empty and
//!   fail its ASSERT);
//! - with it (`headless`): the M6-target `itcm-dtcm` placement, as
//!   h7-bench's `itcm` images: the hot set of `perf/itcm-sections.txt` in
//!   ITCM, the stack, statics and gbc-core's tables in DTCM. The tiers left
//!   in flash come from `GBC_ITCM_DROP` (comma-separated substrings of the
//!   `# Tier ...` headers; empty drops nothing; default
//!   `memory_x::ITCM_DROP_DEFAULT`); build.sh passes the same value to
//!   `scripts/check-itcm-elf.sh`.
//!
//! A feature rather than a second package: the bins keep one lib, one
//! Cargo.lock and one target directory, and the feature is the same
//! switch h7-bench uses (`itcm`).
//!
//! The `headless` image's inputs (ROMs are never committed):
//! - `GBC_HEADLESS_ROM`: the ROM's path (required, except with the
//!   `rom-qspi` feature, which reads its ROM from the QSPI NOR at boot and
//!   ignores this), copied to `OUT_DIR/headless.gb`; its file stem is the
//!   image's `rom=` name;
//! - `GBC_HEADLESS_INPUT`: an input program (`gbc_firmware::input` text
//!   format), copied to `OUT_DIR/headless-input.txt` and parsed at boot;
//!   unset: no input;
//! - `GBC_HEADLESS_FRAMES`: the frame count; unset: the program's `end`
//!   line, else 600 (the image decides, after parsing).

use std::path::{Path, PathBuf};
use std::{env, fs};

use memory_x::{Chip, Data, ItcmSet};

const ITCM_SET: &str = "../../perf/itcm-sections.txt";

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=build.rs");
    let headless = env::var_os("CARGO_FEATURE_HEADLESS").is_some();

    let memory_x = if headless {
        println!("cargo:rerun-if-changed={ITCM_SET}");
        println!("cargo:rerun-if-env-changed=GBC_ITCM_DROP");
        let text = fs::read_to_string(ITCM_SET).unwrap_or_else(|e| panic!("{ITCM_SET}: {e}"));
        let drop = env::var("GBC_ITCM_DROP").unwrap_or_else(|_| memory_x::ITCM_DROP_DEFAULT.into());
        let set = ItcmSet::parse(&text, &memory_x::drop_list(&drop));
        memory_x::firmware_memory_x(Chip::H753, Data::Dtcm, Some(&set))
    } else {
        memory_x::firmware_memory_x(Chip::H753, Data::Dtcm, None)
    };
    fs::write(out.join("memory.x"), memory_x).expect("write memory.x");
    println!("cargo:rustc-link-arg-bins=-L{}", out.display());

    if headless {
        headless_inputs(&out, env::var_os("CARGO_FEATURE_ROM_QSPI").is_none());
    }
}

/// `embed_rom`: false for `rom-qspi`, whose ROM is not part of the image.
fn headless_inputs(out: &Path, embed_rom: bool) {
    for v in ["GBC_HEADLESS_INPUT", "GBC_HEADLESS_FRAMES"] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    if embed_rom {
        embedded_rom(out);
    }

    let input = match env::var("GBC_HEADLESS_INPUT") {
        Ok(p) if !p.is_empty() => {
            println!("cargo:rerun-if-changed={p}");
            fs::read_to_string(&p).unwrap_or_else(|e| panic!("GBC_HEADLESS_INPUT={p}: {e}"))
        }
        _ => String::new(),
    };
    fs::write(out.join("headless-input.txt"), input).expect("write input");

    let frames = env::var("GBC_HEADLESS_FRAMES").unwrap_or_default();
    assert!(
        frames.is_empty() || frames.parse::<u32>().is_ok(),
        "GBC_HEADLESS_FRAMES={frames:?} is not a u32"
    );
    println!("cargo:rustc-env=GBC_HEADLESS_FRAMES={frames}");
}

/// The ROM of the internal-flash `headless` image: `OUT_DIR/headless.gb`
/// and its `rom=` name.
fn embedded_rom(out: &Path) {
    println!("cargo:rerun-if-env-changed=GBC_HEADLESS_ROM");
    let rom_path = env::var("GBC_HEADLESS_ROM").unwrap_or_else(|_| {
        panic!(
            "\n\ngbc-nucleo-h753: the `headless` image needs GBC_HEADLESS_ROM, the path of \
             the ROM to embed (build.sh sets it; ROMs come from test-roms/, never git).\n\n"
        )
    });
    println!("cargo:rerun-if-changed={rom_path}");
    let rom = fs::read(&rom_path).unwrap_or_else(|e| panic!("GBC_HEADLESS_ROM={rom_path}: {e}"));
    fs::write(out.join("headless.gb"), rom).expect("write ROM");
    let name = Path::new(&rom_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_graphic()))
        .unwrap_or_else(|| panic!("GBC_HEADLESS_ROM={rom_path}: no printable file stem"));
    println!("cargo:rustc-env=GBC_HEADLESS_ROM_NAME={name}");
}
