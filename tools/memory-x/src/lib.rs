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
const DTCM_RODATA_SECTION: &str = "
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

/// The `.itcm` section: the hot set runs from ITCM and is loaded from
/// flash; the image copies it before the first call into gbc-core. No KEEP:
/// unused functions are still garbage-collected. The rules must come before
/// cortex-m-rt's `*(.text .text.*)` (the first matching rule gets an input
/// section), hence INSERT BEFORE .text; cortex-m-rt puts `.text` at
/// `_stext`, by default right after the vector table, which is where the
/// load image of `.itcm` now is, so `_stext` moves behind it.
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
}} INSERT BEFORE .text;
_stext = ALIGN(LOADADDR(.itcm) + SIZEOF(.itcm), 8);
ASSERT(__eitcm <= ORIGIN(ITCM) + LENGTH(ITCM), \"the ITCM set does not fit\");
ASSERT(__eitcm - __sitcm > 0x8000, \"the ITCM set is nearly empty: the globs no longer match\");
",
        set.rules()
    )
    .unwrap();
}

/// The `memory.x` text for `chip` with the data in `data` and, if `itcm` is
/// given, the hot set in ITCM. cortex-m-rt's `link.x` includes it.
pub fn memory_x(chip: Chip, data: Data, itcm: Option<&ItcmSet>) -> String {
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
    let mut s = format!("MEMORY {{\n{mem}}}\n{AXI_SECTION}");
    if let Some(set) = itcm {
        write_itcm(&mut s, set);
    }
    if data == Data::Dtcm {
        s.push_str(DTCM_RODATA_SECTION);
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
}
