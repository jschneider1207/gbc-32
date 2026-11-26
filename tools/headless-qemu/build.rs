//! Writes into `OUT_DIR` the `memory.x` for QEMU `mps2-an500` and the ROM
//! and input program the image embeds.
//!
//! Placement: the board `headless` image's (gbc-nucleo-h753 build.rs, the
//! M6-target `itcm-dtcm`) on QEMU's fixed RAM map (`memory_x::Chip::Qemu`):
//! the hot set of `perf/itcm-sections.txt` in the stand-in "ITCM" (a piece
//! of RAM far from the flash code, so the copy and the long-branch thunks
//! are exercised as on the board), gbc-core's tables in `.dtcm_rodata`.
//! `GBC_ITCM_DROP` as for gbc-nucleo-h753 (default
//! `memory_x::ITCM_DROP_DEFAULT`).
//!
//! Inputs, the same variables as the board image (ROMs are never
//! committed):
//! - `GBC_HEADLESS_ROM`: the ROM's path (required);
//! - `GBC_HEADLESS_INPUT`: an input program (`gbc_firmware::input`);
//!   unset: no input;
//! - `GBC_HEADLESS_FRAMES`: the frame count; unset: the program's `end`
//!   line, else 600.

use std::path::{Path, PathBuf};
use std::{env, fs};

use memory_x::{Chip, Data, ItcmSet};

const ITCM_SET: &str = "../../perf/itcm-sections.txt";

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    for v in [
        "GBC_ITCM_DROP",
        "GBC_HEADLESS_ROM",
        "GBC_HEADLESS_INPUT",
        "GBC_HEADLESS_FRAMES",
    ] {
        println!("cargo:rerun-if-env-changed={v}");
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ITCM_SET}");

    let text = fs::read_to_string(ITCM_SET).unwrap_or_else(|e| panic!("{ITCM_SET}: {e}"));
    let drop = env::var("GBC_ITCM_DROP").unwrap_or_else(|_| memory_x::ITCM_DROP_DEFAULT.into());
    let set = ItcmSet::parse(&text, &memory_x::drop_list(&drop));
    let memory_x = memory_x::memory_x(Chip::Qemu, Data::Dtcm, Some(&set));
    fs::write(out.join("memory.x"), memory_x).expect("write memory.x");
    println!("cargo:rustc-link-search={}", out.display());

    let rom_path = env::var("GBC_HEADLESS_ROM").unwrap_or_else(|_| {
        panic!(
            "\n\nheadless-qemu: needs GBC_HEADLESS_ROM, the path of the ROM to embed \
             (run.sh sets it; ROMs come from test-roms/, never git).\n\n"
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
