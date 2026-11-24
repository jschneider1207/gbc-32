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

## Amendments

### 2026-11-24: set re-derived under `codegen-units = 1`; callee-closure check

M10 review finding C1. The numbers above (65.4 KB hot set, 1.5 KB margin)
are from 5e65f6f; later size work shrank the set, and the release profile
is now pinned to one codegen unit. Current numbers are in docs/perf.md
"ITCM placement plan (M8)": the set is 54,974 B of the 57,344 B budget
(margin 2,370 B). What changed:

- Added: the functions the set called on per-access paths but left in
  flash (`Ppu::cpu_blocked`, `Cart::read_ram`, `Cart::read_rtc`,
  `Ctx::run_ppu_to_call`, `Timer::schedule_overflow`), and the per-line
  object path that the fill order above left in flash for lack of room
  (`Ppu::latch`, `Ppu::set_stalls`, `LineTiming::finish`,
  `Renderer::fetch_objects`, `fill_mixed`, `fill_partial::<true>`,
  `partial_tile::<true>`). New drop order if the firmware build is over:
  that per-line object path first, then the plain tile path.
- Removed: `Ppu::scan_line`, which is `#[cold]`.
- The pre-link check gained a third condition: a direct call from the set
  to a gbc-core function outside it fails unless the callee is `#[cold]`
  or has a `FLASH_OK` line with a reason in `perf/itcm-sections.txt`. So
  "veneers are fine on cold edges" is now checked, not assumed.
