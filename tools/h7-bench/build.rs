//! Writes, into `OUT_DIR`:
//! - the workload ROMs: the synthetic ones from the bench's own generator
//!   (`tools/bench/src/synth.rs`, compiled into this script, so the programs
//!   are never copied) and cpu_instrs from `H7_BENCH_CPU_INSTRS` (ROMs are
//!   never committed);
//! - `memory.x` for the chosen placement, with the `.itcm` input-section
//!   rules generated from `perf/itcm-sections.txt` (docs/perf.md "ITCM
//!   placement plan": one source of truth for the list).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

#[allow(dead_code)]
#[path = "../bench/src/synth.rs"]
mod synth;

const SYNTH: [&str; 4] = ["worst-case", "cgb-raster", "dmg-raster", "event-storm"];
const CPU_INSTRS_DEFAULT: &str = "/home/jess/gbc-32/test-roms/blargg/cpu_instrs/cpu_instrs.gb";
const ITCM_SET: &str = "../../perf/itcm-sections.txt";

/// NUCLEO-H755ZI-Q. RM0399 Rev 5, section 2.3 "Memory organization" (memory
/// map): flash bank 1 at 0x0800_0000 (1 MB; bank 2 at 0x0810_0000 is the
/// Cortex-M4's default boot address, section 2.6 Table 9, so the M7 image
/// must stay out of it), ITCM 64 KB at 0, DTCM 128 KB at 0x2000_0000, AXI
/// SRAM 512 KB at 0x2400_0000.
///
/// ITCM starts at 0x100 here: nothing is placed at address 0, so the copy
/// loop never forms a null pointer and a null function pointer does not land
/// on code.
const MEM_H755: &str = "\
  FLASH : ORIGIN = 0x08000000, LENGTH = 1024K
  ITCM  : ORIGIN = 0x00000100, LENGTH = 0xFF00
";
/// NUCLEO-H753ZI. RM0433 Rev 9, section 2.3.2, Table 7 "Memory map and
/// default device memory area attributes": flash bank 1 at 0x0800_0000 and
/// bank 2 at 0x0810_0000 (1 MB each, contiguous; single core, so the image
/// may use both), ITCM 64 KB at 0, DTCM 128 KB at 0x2000_0000, AXI SRAM
/// 512 KB at 0x2400_0000: the same RAM map as the H755.
const MEM_H753: &str = "\
  FLASH : ORIGIN = 0x08000000, LENGTH = 2048K
  ITCM  : ORIGIN = 0x00000100, LENGTH = 0xFF00
";
const RAM_DTCM: &str = "\
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K   /* DTCM */
  AXI   : ORIGIN = 0x24000000, LENGTH = 512K   /* AXI SRAM */
";
const RAM_AXI: &str = "\
  RAM   : ORIGIN = 0x24000000, LENGTH = 256K   /* AXI SRAM, low half */
  AXI   : ORIGIN = 0x24040000, LENGTH = 256K   /* AXI SRAM, high half */
";
/// QEMU `mps2-an500`: 4 MB of code RAM at 0 and 4 MB of RAM at 0x2000_0000.
/// The stand-in "ITCM" is the same size as the real one and, like it, out
/// of branch range of the code in "flash", so the linker needs the same
/// long-branch thunks.
const MEM_QEMU: &str = "\
  FLASH : ORIGIN = 0x00000000, LENGTH = 4096K
  RAM   : ORIGIN = 0x20000000, LENGTH = 2048K
  AXI   : ORIGIN = 0x20200000, LENGTH = 1024K
  ITCM  : ORIGIN = 0x20300100, LENGTH = 0xFF00
";

/// Frame, audio and cartridge RAM buffers (`.axisram`): in AXI SRAM, where
/// the LCD and audio DMA will need them (docs/perf.md). Not loaded and not
/// zeroed: `workload::run` fills them before use.
///
/// It goes after `.got`, the last section of cortex-m-rt's script, and
/// must not go `AFTER .bss`: link.x sets `__ebss` (and `_stack_end`) from
/// the location counter after that insertion point, so a section in
/// another memory region there makes the startup code zero everything from
/// `.bss` to AXI SRAM. With the statics in DTCM that ran off the end of
/// DTCM into a bus fault on the board (2026-11-24); QEMU's RAM is
/// contiguous and did not show it. build.sh now checks `__ebss`.
const AXI_SECTION: &str = "
SECTIONS {
  .axisram (NOLOAD) : ALIGN(32) {
    *(.axisram .axisram.*);
    . = ALIGN(32);
  } > AXI
} INSERT AFTER .got;
";

fn feature(name: &str) -> bool {
    env::var_os(format!("CARGO_FEATURE_{}", name.to_uppercase())).is_some()
}

/// The tiers of the set file left in flash, as substrings of their
/// `# Tier ...` header lines, comma-separated. The whole set is 68,358 B in
/// this build (2026-11-24), over the 64 KiB ITCM, so the default drops the
/// tier the file names first: "plain tile path" (8,078 B), which leaves
/// 60,280 B. `H7_BENCH_ITCM_DROP` overrides it (empty: drop nothing);
/// build.sh reads the same variable for its post-link check.
const ITCM_DROP_DEFAULT: &str = "plain tile path";

/// The `*(<glob>)` rules for `.itcm`, one per glob line of the set file
/// (comments, the budget line and `FLASH_OK` lines are not globs), minus
/// the dropped tiers.
fn itcm_rules() -> String {
    let text = fs::read_to_string(ITCM_SET).unwrap_or_else(|e| panic!("{ITCM_SET}: {e}"));
    let drop = env::var("H7_BENCH_ITCM_DROP").unwrap_or_else(|_| ITCM_DROP_DEFAULT.into());
    let drop: Vec<&str> = drop
        .split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .collect();
    let mut rules = String::new();
    let (mut n, mut dropped, mut skip) = (0, 0, false);
    for line in text.lines() {
        if line.starts_with("# Tier") {
            skip = drop.iter().any(|d| line.contains(d));
        }
        let Some(glob) = line.split_whitespace().next() else {
            continue;
        };
        if glob.starts_with('#') || glob.starts_with("ITCM_BUDGET_BYTES=") || glob == "FLASH_OK" {
            continue;
        }
        assert!(glob.starts_with(".text._R"), "{ITCM_SET}: odd glob {glob}");
        if skip {
            dropped += 1;
            continue;
        }
        writeln!(rules, "    *({glob})").unwrap();
        n += 1;
    }
    assert!(n >= 40, "{ITCM_SET}: only {n} globs kept");
    assert!(
        drop.is_empty() || dropped > 0,
        "H7_BENCH_ITCM_DROP matches no tier of {ITCM_SET}"
    );
    rules
}

fn memory_x() -> String {
    let mut s = if feature("qemu") {
        format!("MEMORY {{\n{MEM_QEMU}}}\n{AXI_SECTION}")
    } else {
        let ram = if feature("dtcm") { RAM_DTCM } else { RAM_AXI };
        let mem = if feature("h755") { MEM_H755 } else { MEM_H753 };
        format!("MEMORY {{\n{mem}{ram}}}\n{AXI_SECTION}")
    };
    if feature("itcm") {
        // The hot set runs from ITCM and is loaded from flash; main copies
        // it before the first call into gbc-core. No KEEP: unused functions
        // are still garbage-collected. The rules must come before
        // cortex-m-rt's `*(.text .text.*)` (the first matching rule gets an
        // input section), hence INSERT BEFORE .text; cortex-m-rt puts
        // `.text` at `_stext`, by default right after the vector table,
        // which is where the load image of `.itcm` now is, so `_stext`
        // moves behind it.
        write!(
            s,
            "
SECTIONS {{
  .itcm : ALIGN(8) {{
    __sitcm = .;
    /* The memory routines the set calls (docs/perf.md: part of the
       front end's ITCM reserve). */
    *(.text.__aeabi_memcpy* .text.__aeabi_memclr* .text.__aeabi_memset*)
    *(.text._R*17compiler_builtins3mem*)
    /* gbc-core, generated from perf/itcm-sections.txt */
{}    . = ALIGN(8);
    __eitcm = .;
  }} > ITCM AT > FLASH
  __siitcm = LOADADDR(.itcm);
}} INSERT BEFORE .text;
_stext = ALIGN(LOADADDR(.itcm) + SIZEOF(.itcm), 8);
ASSERT(__eitcm <= ORIGIN(ITCM) + LENGTH(ITCM), \"the ITCM set does not fit\");
ASSERT(__eitcm - __sitcm > 0x8000, \"the ITCM set is nearly empty: the globs no longer match\");
",
            itcm_rules()
        )
        .unwrap();
    }
    if feature("dtcm") {
        // The core's hot lookup tables, in DTCM (docs/perf.md): part of
        // `.data` in all but name, copied by main.
        s.push_str(
            "
SECTIONS {
  .dtcm_rodata : ALIGN(4) {
    __sdtcm_ro = .;
    *(.rodata._R*8gbc_core3ppu6render6SPREAD*)
    *(.rodata._R*8gbc_core3ppu6render10ATTR_FETCH*)
    *(.rodata._R*8gbc_core3apu5noise4POP8*)
    . = ALIGN(4);
    __edtcm_ro = .;
  } > RAM AT > FLASH
  __sidtcm_ro = LOADADDR(.dtcm_rodata);
} INSERT AFTER .data;
",
        );
    }
    s
}

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../bench/src/synth.rs");
    println!("cargo:rerun-if-changed={ITCM_SET}");
    println!("cargo:rerun-if-env-changed=H7_BENCH_CPU_INSTRS");
    println!("cargo:rerun-if-env-changed=H7_BENCH_ITCM_DROP");

    let (itcm, dtcm, qemu) = (feature("itcm"), feature("dtcm"), feature("qemu"));
    assert!(
        feature("h753") != feature("h755"),
        "exactly one board feature: h753 or h755 (build.sh: H7_BENCH_BOARD)"
    );
    assert!(!(itcm && !dtcm), "feature `itcm` needs `dtcm`");
    assert!(
        !(qemu && (feature("selftest") || feature("pll"))),
        "feature `qemu` excludes `selftest` and `pll`"
    );

    fs::write(out.join("memory.x"), memory_x()).expect("write memory.x");
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
