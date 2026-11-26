//! The `memory.x` generator for the firmware images (ADR 0036): the chip
//! memory maps, the data placements, the parser for
//! `perf/itcm-sections.txt` (ADR 0026, docs/perf.md "ITCM placement plan")
//! and the `.itcm`, `.dtcm_rodata` and `.axisram` sections with their
//! ASSERTs.
//!
//! A host library, used from build scripts (`[build-dependencies]`), so it
//! panics with a message where a build script would: a bad set file or
//! drop list must stop the build. Root `cargo test --workspace` tests it.

use std::fmt::Write as _;

/// The set file's path from the repo root, for messages.
pub const ITCM_SET_NAME: &str = "perf/itcm-sections.txt";

/// The tiers of the set file left in flash by default, as substrings of
/// their `# Tier ...` header lines. The whole set is 68,358 B in the
/// h7-bench build (2026-11-24), over the 64 KiB ITCM, so the default drops
/// the tier the file names first: "plain tile path" (8,078 B), which leaves
/// 60,280 B.
pub const ITCM_DROP_DEFAULT: &str = "plain tile path";

/// The fewest globs a parsed set may keep: fewer means the file or the drop
/// list is wrong.
const MIN_GLOBS: usize = 40;

/// The chip (memory map) an image links for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// NUCLEO-H753ZI (STM32H753ZIT6).
    H753,
    /// NUCLEO-H755ZI-Q (STM32H755ZIT6), Cortex-M7 side.
    H755,
    /// QEMU `mps2-an500`, for the smoke runs. Its RAM map is fixed, so the
    /// data placement only decides whether `.dtcm_rodata` is emitted.
    Qemu,
}

/// Where the stack, statics and `.data`/`.bss` (the `RAM` region) live.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Data {
    /// The low half of AXI SRAM; the high half is the `AXI` region.
    AxiSram,
    /// DTCM, with all of AXI SRAM as the `AXI` region, and the core's hot
    /// lookup tables copied to DTCM (`.dtcm_rodata`).
    Dtcm,
}

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
/// zeroed: the image fills them before use.
///
/// It goes after `.got`, the last section of cortex-m-rt's script, and
/// must not go `AFTER .bss`: link.x sets `__ebss` (and `_stack_end`) from
/// the location counter after that insertion point, so a section in
/// another memory region there makes the startup code zero everything from
/// `.bss` to AXI SRAM. With the statics in DTCM that ran off the end of
/// DTCM into a bus fault on the board (2026-11-24); QEMU's RAM is
/// contiguous and did not show it. h7-bench's build.sh checks `__ebss`.
const AXI_SECTION: &str = "
SECTIONS {
  .axisram (NOLOAD) : ALIGN(32) {
    *(.axisram .axisram.*);
    . = ALIGN(32);
  } > AXI
} INSERT AFTER .got;
";

/// The core's hot lookup tables, in DTCM (docs/perf.md): part of `.data`
/// in all but name, copied by the image's startup.
///
/// Besides the three named tables, gbc-core's anonymous constant pools
/// (`.rodata.cst4/8/16/32`, merged by the linker; 128 B live in the h7-bench
/// image, 2026-11-25): small tables LLVM makes from `match`es and array
/// literals, and the ITCM set reads some of them on every call (`read_high`'s
/// FF10-FF2F read mask, the square duty and wave volume tables in the APU,
/// `Ctx::boundary`'s 8-byte table). Keyed to gbc-core's archive member
/// (lld matches the file pattern against `archive:member`), not to every
/// crate's pools. Left in flash on purpose: gbc-core's `.rodata.str1.1`
/// (the absolute source paths of panic locations) and `.rodata..L*` (the
/// panic `Location`s, plus initializers and tables of flash-resident cold
/// code: `Sys::new`, `Ppu::reset`/`write_lcdc`, `Emulator::load`,
/// `cart_ram_size`). They are read only on a panic or outside the frame
/// loop, and the paths make their size depend on the checkout path
/// (review C-P7, Stage 2 M1).
const DTCM_RODATA_SECTION: &str = "
SECTIONS {
  .dtcm_rodata : ALIGN(4) {
    __sdtcm_ro = .;
    *(.rodata._R*8gbc_core3ppu6render6SPREAD*)
    *(.rodata._R*8gbc_core3ppu6render10ATTR_FETCH*)
    *(.rodata._R*8gbc_core3apu5noise4POP8*)
    *libgbc_core-*.rlib:*(.rodata.cst*)
    . = ALIGN(4);
    __edtcm_ro = .;
  } > RAM AT > FLASH
  __sidtcm_ro = LOADADDR(.dtcm_rodata);
} INSERT AFTER .data;
";

/// Splits a comma-separated drop list (tier header substrings), trimming
/// each entry and skipping empty ones, so "" drops nothing.
pub fn drop_list(list: &str) -> Vec<&str> {
    list.split(',')
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .collect()
}

/// The parsed ITCM set: the `.itcm` input-section rules for the kept globs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItcmSet {
    rules: String,
    /// Globs kept (one rule each).
    pub kept: usize,
    /// Globs in dropped tiers, left in flash.
    pub dropped: usize,
}

impl ItcmSet {
    /// Parses the set file's text: one `*(<glob>)` rule per glob line
    /// (comments, the `ITCM_BUDGET_BYTES=` line and `FLASH_OK` lines are not
    /// globs), minus the globs under a `# Tier ...` header that contains an
    /// entry of `drop`. A tier runs until the next `# Tier` header.
    ///
    /// # Panics
    /// On a glob that is not a `.text._R` (v0-mangled) section, when fewer
    /// than 40 globs are kept, or when `drop` is non-empty and matches no
    /// glob.
    pub fn parse(text: &str, drop: &[&str]) -> ItcmSet {
        let mut rules = String::new();
        let (mut kept, mut dropped, mut skip) = (0, 0, false);
        for line in text.lines() {
            if line.starts_with("# Tier") {
                skip = drop.iter().any(|d| line.contains(d));
            }
            let Some(glob) = line.split_whitespace().next() else {
                continue;
            };
            if glob.starts_with('#') || glob.starts_with("ITCM_BUDGET_BYTES=") || glob == "FLASH_OK"
            {
                continue;
            }
            assert!(
                glob.starts_with(".text._R"),
                "{ITCM_SET_NAME}: odd glob {glob}"
            );
            if skip {
                dropped += 1;
                continue;
            }
            writeln!(rules, "    *({glob})").unwrap();
            kept += 1;
        }
        assert!(kept >= MIN_GLOBS, "{ITCM_SET_NAME}: only {kept} globs kept");
        assert!(
            drop.is_empty() || dropped > 0,
            "the ITCM drop list matches no tier of {ITCM_SET_NAME}"
        );
        ItcmSet {
            rules,
            kept,
            dropped,
        }
    }

    /// The rules, one `    *(<glob>)` line each.
    pub fn rules(&self) -> &str {
        &self.rules
    }
}

/// Where gbc-core's flash code (`.text_core`) starts, as an offset from the
/// start of flash (ADR 0042). Fixed, so that edits to the image's own code
/// or the lib do not move the core's flash code: the code layout moves the
/// cycle counts by up to ~4 % through branch-predictor aliasing. Behind the
/// vector table and the `.itcm` load image, which can never be over the
/// ITCM's 0xFF00 B: 0x298 B of H7 vectors + 0xFF00 = 0x1_0198, so 0x1_0400
/// leaves 616 B even for a full ITCM (the linker ASSERTs it).
pub const CORE_TEXT_OFFSET: u32 = 0x1_0400;
// The worst case fits: the H7 vector table and a full ITCM load image.
const _: () = assert!(0x298 + 0xFF00 <= CORE_TEXT_OFFSET);

/// The least `.text_core` may hold: less means its file pattern no longer
/// matches gbc-core (renamed archive, LTO merging the crates).
pub const CORE_TEXT_MIN_BYTES: u32 = 0x4000;

/// The `.itcm` section: the hot set runs from ITCM and is loaded from
/// flash; the image copies it before the first call into gbc-core. No KEEP:
/// unused functions are still garbage-collected. The rules must come before
/// cortex-m-rt's `*(.text .text.*)` (the first matching rule gets an input
/// section), hence INSERT BEFORE .text.
///
/// Then `.text_core`: the rest of gbc-core's code, at the fixed
/// [`CORE_TEXT_OFFSET`] in flash (ADR 0042), keyed to gbc-core's archive
/// member as `.dtcm_rodata` is. In the same SECTIONS block as `.itcm`, so
/// that `.itcm`'s rules take the hot set first. cortex-m-rt puts `.text` at
/// `_stext`, by default right after the vector table, which is where the
/// load image of `.itcm` now is, so `_stext` moves behind `.text_core`:
/// the image's and the lib's code come after the core's, and their edits
/// move nothing of gbc-core. gbc-core's generics instantiated in other
/// crates are those crates' code and land in `.text`.
fn write_itcm(s: &mut String, set: &ItcmSet) {
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
  .text_core ORIGIN(FLASH) + {CORE_TEXT_OFFSET:#x} : {{
    __stext_core = .;
    *libgbc_core-*.rlib:*(.text .text.*)
    . = ALIGN(8);
    __etext_core = .;
  }} > FLASH
}} INSERT BEFORE .text;
_stext = __etext_core;
ASSERT(__eitcm <= ORIGIN(ITCM) + LENGTH(ITCM), \"the ITCM set does not fit\");
ASSERT(__eitcm - __sitcm > 0x8000, \"the ITCM set is nearly empty: the globs no longer match\");
ASSERT(__siitcm + SIZEOF(.itcm) <= __stext_core, \"the .itcm load image overlaps .text_core: raise memory_x::CORE_TEXT_OFFSET\");
ASSERT(__stext_core == ORIGIN(FLASH) + {CORE_TEXT_OFFSET:#x}, \".text_core is not at its fixed address\");
ASSERT(__etext_core - __stext_core > {CORE_TEXT_MIN_BYTES:#x}, \".text_core is nearly empty: its pattern no longer matches gbc-core\");
",
        set.rules()
    )
    .unwrap();
}

/// The `memory.x` text for `chip` with the data in `data` and, if `itcm` is
/// given, the hot set in ITCM. cortex-m-rt's `link.x` includes it.
pub fn memory_x(chip: Chip, data: Data, itcm: Option<&ItcmSet>) -> String {
    generate(chip, data, itcm, false)
}

/// Bytes reserved in AXI SRAM for the HardFault handler's stack
/// (`.fault_stack`, [`firmware_memory_x`]). The handler formats one report
/// line and never returns; 2 KiB is several times its depth.
pub const FAULT_STACK_BYTES: u32 = 2048;

/// Bytes of the stack guard ([`firmware_memory_x`]): a no-access MPU region
/// (gbc-nucleo-h753 `mpu::REGION_STACK_GUARD`) between the statics and the
/// stack, so a stack overflow faults instead of silently writing `.bss`
/// (Stage 2 M1 review F-2, M5 review P-1). A power of two (an MPU region
/// is), and the guard is aligned to its size. 128 rather than the minimum
/// 32: an overflowing `push` faults with the stack pointer just above the
/// guard, and the HardFault entry then stacks its frame below that, up to
/// 104 B for an extended (FP) frame (Armv7-M B1.5.7) whose eight basic
/// words are the lowest; with a 32 B guard they would land in the statics
/// below it. With 128 B they fall in the guard (the stacking faults,
/// MMFSR.MSTKERR, and nothing is written).
pub const STACK_GUARD_BYTES: u32 = 128;

/// The `memory.x` text for an image that links `gbc-nucleo-h753`'s lib
/// (ADR 0036): [`memory_x`]'s map, plus what the lib reads from every
/// image whatever its placement, so the lib needs no placement features:
/// - `.fault_stack`, a NOLOAD reservation of [`FAULT_STACK_BYTES`] in AXI
///   SRAM right after `.axisram`, with `__fault_stack_top` at its top
///   (8-byte aligned, as a stack pointer must be); no placement puts
///   anything else there, and the linker refuses an AXI region that
///   overflows;
/// - `__stack_guard`, the base of the [`STACK_GUARD_BYTES`] guard: the end
///   of the statics (`__euninit`) rounded up to the guard's size; and
///   `_stack_end` right above it, which replaces cortex-m-rt's `PROVIDE`d
///   default (`__euninit` itself), so the stack's paint (`paint-stack`) and
///   the lib's `stack::usage` start above the guard and never touch it;
/// - `__sitcm`, `__eitcm` and `__siitcm` without an ITCM set, and
///   `__sdtcm_ro`, `__edtcm_ro` and `__sidtcm_ro` with the data in AXI
///   SRAM: start = end, so the lib's TCM copy copies nothing. They are
///   plain symbols rather than empty output sections, whose `LOADADDR` the
///   linker may not define once it discards the empty section.
pub fn firmware_memory_x(chip: Chip, data: Data, itcm: Option<&ItcmSet>) -> String {
    generate(chip, data, itcm, true)
}

/// `.axisram` and `.fault_stack` in one block: a second `INSERT AFTER
/// .axisram` would depend on the order in which the linker applies
/// INSERT commands.
fn axi_fault_section() -> String {
    format!(
        "
SECTIONS {{
  .axisram (NOLOAD) : ALIGN(32) {{
    *(.axisram .axisram.*);
    . = ALIGN(32);
  }} > AXI
  .fault_stack (NOLOAD) : ALIGN(8) {{
    __fault_stack_bottom = .;
    . += {FAULT_STACK_BYTES};
    . = ALIGN(8);
    __fault_stack_top = .;
  }} > AXI
}} INSERT AFTER .got;
"
    )
}

/// The stack guard's symbols (see [`firmware_memory_x`]).
fn stack_guard_symbols() -> String {
    format!(
        "
/* Stack guard: {STACK_GUARD_BYTES} B, no-access MPU region, below the stack. */
__stack_guard = ALIGN(__euninit, {STACK_GUARD_BYTES});
_stack_end = __stack_guard + {STACK_GUARD_BYTES};
"
    )
}

/// The TCM symbols of a placement that leaves a section out (see
/// [`firmware_memory_x`]).
const NO_ITCM_SYMBOLS: &str = "
/* No ITCM set: an empty copy for the image's TCM code. */
__sitcm = ORIGIN(ITCM);
__eitcm = __sitcm;
__siitcm = __sitcm;
";
const NO_DTCM_RODATA_SYMBOLS: &str = "
/* Data in AXI SRAM: no DTCM tables, an empty copy. */
__sdtcm_ro = ORIGIN(RAM);
__edtcm_ro = __sdtcm_ro;
__sidtcm_ro = __sdtcm_ro;
";

fn generate(chip: Chip, data: Data, itcm: Option<&ItcmSet>, firmware: bool) -> String {
    let mem = match chip {
        Chip::Qemu => MEM_QEMU.to_owned(),
        Chip::H753 | Chip::H755 => {
            let flash = if chip == Chip::H755 {
                MEM_H755
            } else {
                MEM_H753
            };
            let ram = match data {
                Data::Dtcm => RAM_DTCM,
                Data::AxiSram => RAM_AXI,
            };
            format!("{flash}{ram}")
        }
    };
    let axi = if firmware {
        axi_fault_section()
    } else {
        AXI_SECTION.to_owned()
    };
    let mut s = format!("MEMORY {{\n{mem}}}\n{axi}");
    if firmware {
        s.push_str(&stack_guard_symbols());
    }
    match itcm {
        Some(set) => write_itcm(&mut s, set),
        None if firmware => s.push_str(NO_ITCM_SYMBOLS),
        None => {}
    }
    if data == Data::Dtcm {
        s.push_str(DTCM_RODATA_SECTION);
    } else if firmware {
        s.push_str(NO_DTCM_RODATA_SYMBOLS);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A set file with `n` globs in tier A and two in a "plain tile path"
    /// tier B, plus the header, budget line and FLASH_OK lines.
    fn set_text(n: usize) -> String {
        let mut s =
            String::from("# header comment\n#\nITCM_BUDGET_BYTES=57344\n#\n# Tier A: hot\n");
        for i in 0..n {
            writeln!(s, ".text._R*8gbc_core3cpu{i}*   # Cpu::f{i}").unwrap();
        }
        s.push_str("\n   \n# Tier B, plain tile path (drop first)\n");
        s.push_str(".text._R*8gbc_core3ppu4tile*  # tile\n");
        s.push_str(".text._R*8gbc_core3ppu5tile2*\n");
        s.push_str("# Tier C: kept again\n.text._R*8gbc_core3apu*  # apu\n");
        s.push_str("# FLASH_OK: warm functions\n");
        s.push_str("FLASH_OK .text._R*8gbc_core3bus7read_sb*  # SB reads\n");
        s
    }

    #[test]
    fn keeps_every_glob_without_drops() {
        let set = ItcmSet::parse(&set_text(40), &[]);
        assert_eq!((set.kept, set.dropped), (43, 0));
        assert!(set.rules().starts_with("    *(.text._R*8gbc_core3cpu0*)\n"));
        assert!(
            set.rules()
                .contains("    *(.text._R*8gbc_core3ppu4tile*)\n")
        );
        assert!(set.rules().ends_with("    *(.text._R*8gbc_core3apu*)\n"));
    }

    #[test]
    fn drops_a_tier_until_the_next_tier_header() {
        let set = ItcmSet::parse(&set_text(40), &drop_list(ITCM_DROP_DEFAULT));
        assert_eq!((set.kept, set.dropped), (41, 2));
        assert!(!set.rules().contains("tile"));
        // Tier C follows the dropped tier and is kept.
        assert!(set.rules().contains("    *(.text._R*8gbc_core3apu*)\n"));
    }

    #[test]
    fn comments_budget_and_flash_ok_are_not_globs() {
        let rules = ItcmSet::parse(&set_text(40), &[]).rules().to_owned();
        assert_eq!(rules.lines().count(), 43);
        assert!(rules.lines().all(|l| l.starts_with("    *(.text._R")));
        assert!(!rules.contains("read_sb"), "a FLASH_OK glob became a rule");
        assert!(
            !rules.contains('#'),
            "a trailing comment leaked into a rule"
        );
    }

    #[test]
    fn drop_list_trims_and_skips_empty_entries() {
        assert_eq!(drop_list(""), Vec::<&str>::new());
        assert_eq!(drop_list(" a , ,b,"), vec!["a", "b"]);
    }

    #[test]
    #[should_panic(expected = "odd glob .text.plain")]
    fn odd_glob_panics() {
        let mut s = set_text(40);
        s.push_str(".text.plain_symbol  # not v0-mangled\n");
        ItcmSet::parse(&s, &[]);
    }

    #[test]
    #[should_panic(expected = "only 39 globs kept")]
    fn too_few_globs_panics() {
        ItcmSet::parse(&set_text(38), &["plain tile path"]);
    }

    #[test]
    #[should_panic(expected = "matches no tier")]
    fn unmatched_drop_panics() {
        ItcmSet::parse(&set_text(40), &["no such tier"]);
    }

    #[test]
    fn the_real_set_parses_with_the_default_drop() {
        let text = include_str!("../../../perf/itcm-sections.txt");
        let all = ItcmSet::parse(text, &[]);
        let set = ItcmSet::parse(text, &drop_list(ITCM_DROP_DEFAULT));
        assert!(set.dropped > 0);
        assert_eq!(set.kept + set.dropped, all.kept);
    }

    #[test]
    fn memory_maps_and_placements() {
        let h753 = memory_x(Chip::H753, Data::AxiSram, None);
        assert!(h753.starts_with("MEMORY {\nFLASH : ORIGIN = 0x08000000, LENGTH = 2048K\n"));
        assert!(h753.contains("RAM   : ORIGIN = 0x24000000, LENGTH = 256K"));
        assert!(h753.contains("} INSERT AFTER .got;"));
        assert!(!h753.contains(".itcm") && !h753.contains(".dtcm_rodata"));

        let h755 = memory_x(Chip::H755, Data::Dtcm, None);
        assert!(h755.contains("FLASH : ORIGIN = 0x08000000, LENGTH = 1024K"));
        assert!(h755.contains("RAM   : ORIGIN = 0x20000000, LENGTH = 128K"));
        assert!(h755.ends_with("} INSERT AFTER .data;\n"));

        // QEMU's map ignores the placement; DTCM still emits the tables.
        let qemu = memory_x(Chip::Qemu, Data::Dtcm, None);
        assert!(qemu.contains("ITCM  : ORIGIN = 0x20300100"));
        assert!(!qemu.contains("0x24000000"));
        assert!(qemu.contains(".dtcm_rodata"));
    }

    #[test]
    fn dtcm_rodata_holds_gbc_core_constant_pools() {
        let s = memory_x(Chip::H753, Data::Dtcm, None);
        let start = s.find(".dtcm_rodata :").unwrap();
        let end = start + s[start..].find("__edtcm_ro = .;").unwrap();
        let section = &s[start..end];
        // Keyed to gbc-core's archive member, so other crates' pools and
        // gbc-core's cold strings and panic locations stay in flash.
        assert!(section.contains("\n    *libgbc_core-*.rlib:*(.rodata.cst*)\n"));
        assert!(
            !section.contains("\n    *(.rodata.cst"),
            "unkeyed: every crate's pools"
        );
        assert!(!section.contains(".rodata.str") && !section.contains(".rodata..L"));
        // Not emitted when the data is in AXI SRAM.
        assert!(!memory_x(Chip::H753, Data::AxiSram, None).contains(".rodata.cst"));
    }

    #[test]
    fn itcm_section_holds_the_rules_and_the_asserts() {
        let set = ItcmSet::parse(&set_text(40), &drop_list(ITCM_DROP_DEFAULT));
        let s = memory_x(Chip::H753, Data::Dtcm, Some(&set));
        let rules_at = s.find(set.rules()).expect("rules in memory.x");
        let header_at = s.find("/* gbc-core, generated from").unwrap();
        let end_at = s.find("__eitcm = .;").unwrap();
        assert!(header_at < rules_at && rules_at < end_at);
        assert!(s.contains("\"the ITCM set does not fit\""));
        assert!(s.contains("\"the ITCM set is nearly empty: the globs no longer match\""));
        // `.itcm` comes before `.dtcm_rodata`.
        assert!(s.find(".itcm :").unwrap() < s.find(".dtcm_rodata :").unwrap());
    }

    #[test]
    fn core_text_follows_the_itcm_rules_at_its_fixed_address() {
        let set = ItcmSet::parse(&set_text(40), &[]);
        let s = memory_x(Chip::H753, Data::Dtcm, Some(&set));
        let itcm = s.find(".itcm : ALIGN(8) {").unwrap();
        let core = s.find(".text_core ORIGIN(FLASH) + 0x10400 : {").unwrap();
        let insert = s.find("} INSERT BEFORE .text;").unwrap();
        // One block, `.itcm` first: its rules take the hot set.
        assert!(itcm < core && core < insert);
        assert!(!s[itcm..core].contains("INSERT"));
        assert!(s[core..insert].contains("\n    *libgbc_core-*.rlib:*(.text .text.*)\n"));
        // The image's code follows the core's.
        assert!(s.contains("\n_stext = __etext_core;\n"));
        assert!(s.contains("overlaps .text_core"));
        assert!(s.contains("is not at its fixed address"));
        assert!(s.contains("> 0x4000, \".text_core is nearly empty"));
        // Without a set, no `.text_core`.
        assert!(!memory_x(Chip::H753, Data::Dtcm, None).contains(".text_core"));
    }

    /// Every symbol the lib reads, in each placement.
    const LIB_SYMBOLS: [&str; 9] = [
        "__stack_guard =",
        "_stack_end =",
        "__sitcm =",
        "__eitcm =",
        "__siitcm =",
        "__sdtcm_ro =",
        "__edtcm_ro =",
        "__sidtcm_ro =",
        "__fault_stack_top =",
    ];

    #[test]
    fn firmware_map_always_defines_the_lib_symbols() {
        let set = ItcmSet::parse(&set_text(40), &[]);
        for (data, itcm) in [
            (Data::AxiSram, None),
            (Data::Dtcm, None),
            (Data::Dtcm, Some(&set)),
        ] {
            let s = firmware_memory_x(Chip::H753, data, itcm);
            for sym in LIB_SYMBOLS {
                assert_eq!(s.matches(sym).count(), 1, "{sym} in {data:?} {itcm:?}");
            }
        }
        // The plain map defines none of the fallbacks or the fault stack.
        let plain = memory_x(Chip::H753, Data::AxiSram, None);
        assert!(LIB_SYMBOLS.iter().all(|sym| !plain.contains(sym)));
    }

    #[test]
    fn firmware_map_without_a_set_has_no_itcm_section() {
        let s = firmware_memory_x(Chip::H753, Data::Dtcm, None);
        assert!(!s.contains(".itcm :") && !s.contains("nearly empty"));
        assert!(s.contains("__eitcm = __sitcm;"));
        // The DTCM tables stay a real section in the DTCM placement.
        assert!(s.contains(".dtcm_rodata :") && !s.contains("__edtcm_ro = __sdtcm_ro;"));
    }

    #[test]
    fn fault_stack_follows_the_axi_buffers() {
        let s = firmware_memory_x(Chip::H753, Data::Dtcm, None);
        let axi = s.find(".axisram (NOLOAD)").unwrap();
        let fault = s.find(".fault_stack (NOLOAD)").unwrap();
        let insert = s.find("} INSERT AFTER .got;").unwrap();
        assert!(axi < fault && fault < insert);
        assert!(s.contains(&format!(". += {FAULT_STACK_BYTES};")));
        assert_eq!(FAULT_STACK_BYTES % 8, 0);
        // The rest of the map is the plain one's.
        assert!(s.starts_with(&memory_x(Chip::H753, Data::Dtcm, None)[..60]));
    }
}
