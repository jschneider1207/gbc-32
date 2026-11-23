# 0026: Hot core code goes in ITCM by front-end link rules

- Status: Proposed
- Date: 2026-11-22
- Builds on: ADR 0009 (`forbid(unsafe_code)` in gbc-core), ADR 0008 (CPU/bus hot path)

## Context
gbc-core's `.text` is 94.8 KB, and the code that runs every frame is
65.4 KB (docs/perf.md, "ITCM placement plan (M8)"). The H753's ITCM is a
fixed 64 KB and its I-cache 16 KiB. Flash runs at 4 wait states at 240 MHz
AXI (RM0433 Table 17). The interpreter (`Cpu::run` + `execute_cb`, 37.6 KB)
jumps around its code on every instruction, which is the worst access
pattern for a small I-cache.

Rust emits one section per function (`.text.<v0-mangled name>`).
gbc-core cannot choose its own sections: `link_section` is an unsafe
attribute in edition 2024, and `forbid(unsafe_code)` rejects it.

## Decision
- Placement is the MCU front end's job, done at link time. Its linker
  script has a `.itcm` output section (`> ITCM AT > FLASH`, `INSERT BEFORE
  .text`) whose input rules are globs on the length-prefixed v0 path
  segments, e.g. `*(.text._R*8gbc_core3cpu*3Cpu3run*)`, never on the crate
  hash. The front end copies it from flash before calling the core. Read-only
  tables (`SPREAD`, `ATTR_FETCH`, `POP8`) go to DTCM the same way.
- The globs live in one file, `perf/itcm-sections.txt` (glob plus demangled
  name). The front end's `build.rs` generates the linker rules from it. A
  pre-link check (rlib: every glob matches, the total stays within budget)
  and a post-link check (firmware ELF: every listed name exists and sits
  in the ITCM address range) keep renames and inlining from silently
  dropping a function into flash.
- H753 core budget: 56 KiB of ITCM, assuming the front end needs 8 KiB.
  Fill order: CPU/bus/scheduler, frame driver, APU integrate set, the
  per-event PPU functions, then the plain tile path. The per-line heavy PPU
  set and the warm paths (~14 KB) stay in flash behind the I-cache. H735:
  configure 128 KB of ITCM and place the whole hot and warm set.
- The `execute`/`execute_cb` <= 40 KB target is dropped as a separate
  goal. The ITCM-set total budget replaces it, together with tighter
  `run`/`execute_cb` budgets.

## Consequences
- The core needs no `unsafe` and no target-specific code, and the desktop
  build is unaffected.
- The glob list is coupled to symbol names. That is why the checks exist:
  a rename fails a check rather than costing speed in silence.
- The selection is measured on the rlib (no LTO). The firmware ELF is the
  authority. If LTO makes the set too large, remove items in reverse fill
  order.
- Calls between ITCM and flash go through linker veneers. Fine on cold
  edges. The post-link check reports how many there are.
- The 1.5 KB of margin on the H753 is thin. Core growth on the hot path now
  has an ITCM cost, which the total budget makes visible.
