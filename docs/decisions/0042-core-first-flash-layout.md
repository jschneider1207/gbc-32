# 0042: Core-first flash layout for stable cycle counts

- Status: Accepted (agent decision)
- Date: 2026-11-26
- Builds on: ADR 0026 (ITCM placement at link time), ADR 0036 (firmware
  crate layout, `tools/memory-x`); plan `docs/plan/stage-2.md` M6 (the
  on-board cycle baseline with a +3% ratchet)

## Context
A +3% ratchet on the board's cycles per frame needs counts that move only
when the core's work changes. They also moved when nothing in gbc-core
changed: rebuilding the bench crate alone moved them by up to about 2%,
and docs/perf.md put that down to "code layout, most likely; not
separated". M6 separated it, on `bench-itcm-dtcm-pll480` (472 MHz, hot
set in ITCM, data in DTCM) with deliberate layout perturbations, 15
workloads each.

**Measured** (largest change of a mean cycles-per-frame figure over all
15 workloads, against the unperturbed build; the four bound workloads
worst-case, cgb-raster, cpu_instrs and event-storm are quoted below):

| Perturbation | largest move over 15, % (workload) |
|---|---|
| reflash, same image (floor) | 0.03 |
| flash `.text` +8 B | 1.67 (storm-spsw) |
| flash `.text` +16 B | 1.67 (storm-div) |
| flash `.text` +32 B | 2.73 (storm-div) |
| flash `.text` +1000 B | 4.24 (storm-div) |
| `.rodata` +32 B / +1000 B | 0.25 |
| ITCM +4 / +8 / +16 / +32 B | 0.28 / 0.43 / 1.70 / 1.05 |
| real bench-code change, main layout | 3.29 (dma-storm) |
| same change, core-first layout | 0.38 (halt-irq) |

The bound workloads moved by at most 0.61% in every case; the storms carry
the large moves.

- **It is branch prediction, not the caches.** With the branch predictor
  off (ACTLR DISBTACREAD | DISBTACALLOC, bits 13 and 14; diagnostic only),
  a flash +32 B and an ITCM +32 B shift moved every workload by at most
  0.11%, against 2.71% and 1.02% with it on. Moving `.rodata` alone moved
  at most 0.25%. Shifting flash `.text` by exactly one 32-byte line still
  moved storm-div by 2.7%, so it is not I-cache set conflicts either: the
  Cortex-M7's BTAC aliases on code addresses, in ITCM and in flash alike.
- **Alignment flags do not fix it.** `-align-all-functions=3` and
  `-align-loops=8` are already LLVM's cortex-m7 defaults (byte-identical
  `.bin`). `-align-loops=32` with 32-byte functions overflows ITCM by
  16,392 B. `-align-all-functions=5` fits (+536 B ITCM, +2,056 B flash),
  but flash +1000 B still moved storm-div 2.27% and cgb-raster 1.45%.
  Rejected.
- **The flash order made every edit a core move.** cortex-m-rt's `.text`
  took the input sections in link order: the image's crate (h7-bench
  about 8 KB), then the gbc-nucleo-h753 lib (about 12 KB), then gbc-core
  (about 50 KB). Any edit to the bench or the lib slid all of the core's
  flash code.
- **Byte determinism.** The same tree at the same path builds the same
  `.bin` twice. Two worktree paths give 40 to 60 differing bytes in
  h7-bench's `.rodata` and literal pools (the crate hash in mangled
  names depends on the path through cargo's `-C metadata`); function
  addresses and sizes are identical.

## Decision
- **gbc-core's flash code goes first, at a fixed address.** With an ITCM
  set, `tools/memory-x` emits `.text_core` at `ORIGIN(FLASH) +
  CORE_TEXT_OFFSET` (0x1_0400, so 0x0801_0400 on the H753), holding
  `*libgbc_core-*.rlib:*(.text .text.*)`: every function of gbc-core's
  own objects that the ITCM set does not take. It is in the same
  `SECTIONS { } INSERT BEFORE .text` block as `.itcm`, after it, so the
  ITCM rules still claim the hot set first. `_stext` moves to the end of
  `.text_core`, so cortex-m-rt's `.text` (the image's and the lib's code,
  and gbc-core generics instantiated in those crates) follows the core.
  The vector table stays at 0x0800_0000 with the `.itcm` load image right
  after it; the gap up to `.text_core` is zero fill in the `.bin`, so the
  image's `image_cksum` still covers exactly the `.bin`.
- **Why 0x1_0400.** The `.itcm` load image is at most the ITCM's 0xFF00 B
  (its own ASSERT) and the H753's vector table is 0x298 B, so the worst
  case ends at 0x1_0198: 0x1_0400 never needs to move with the ITCM set.
  Today's gap (the headroom): 1,864 B in `bench-itcm-dtcm-pll480` (64,032 B
  `.itcm`), 1,808 B in the gbc-nucleo-h753 ITCM images (64,088 B).
- **Linker ASSERTs** (in the generated `memory.x`): the `.itcm` load image
  ends at or before `.text_core`; `.text_core` is at its fixed address;
  and it holds more than `CORE_TEXT_MIN_BYTES` (16 KiB), so a pattern
  that stops matching (a renamed archive, LTO merging the crates) fails
  the build instead of silently undoing the layout. `.text_core` has no
  upper limit of its own: `.text` follows it, and the FLASH region bounds
  both.
- **Post-link check** (`scripts/check-itcm-elf.sh`, run by both build.sh
  scripts on every ITCM image): the ELF has a `.text_core` section; no
  GLOBAL copy of a gbc-core rlib function is in flash outside it; and
  every rlib function referenced (a relocation in the rlib) by a function
  in ITCM or `.text_core` has a copy in one of them. A name alone cannot
  tell the rlib's copy from a downstream crate's: rustc compiles small and
  generic gbc-core functions into their callers' crates as LOCAL copies
  of the same name (3 or 4 per gbc-nucleo-h753 image, e.g. derived
  `Debug::fmt`s), which are reported, not failed.
- **Images without an ITCM set** (the non-headless gbc-nucleo-h753 images,
  the bench's `flash-*` placements) keep the old layout: no figures are
  ratcheted on them, and a 64 KB gap would buy nothing there.
- **Baselines are built in the main checkout.** Addresses do not depend on
  the path, but the bytes do, so a committed baseline and the `.bin` it
  names come from a build at the main checkout path.

## Consequences
- Edits to the bench, the lib or the front-end images no longer move the
  core's code: the measured bench change moved 0.38% at most (halt-irq),
  not 3.29%.
- **Core changes still move the layout**, by up to about +-1.7% through
  ITCM alignment and up to about +-4% on the storms in flash. That is
  accepted: a core change is what the ratchet measures, and a +3% move on
  a bound workload is far outside the <= 0.61% the bound workloads showed
  under any perturbation. A core change that trips the ratchet on a storm
  alone needs a look before the baseline is moved.
- The ITCM set's order and padding still move the hot set; this ADR does
  not pin ITCM addresses.
- Under 2 KB of flash is zero fill. The H753 has 2 MB.
- **The switch itself was a one-time layout move.** On the board
  (`bench-itcm-dtcm-pll480`, both images from the sources of 67884fa, main layout
  from the main checkout, core-first from a worktree), mean cycles per
  frame core-first against main: worst-case +0.23%, cgb-raster +1.18%,
  dmg-raster +0.14%, event-storm +0.62%, cpu_instrs +0.21%; largest
  storm-div +3.54%, storm-spsw +1.95%; all fifteen hashes unchanged. The
  ITCM image moved too (lld placed the long-branch thunks to flash
  elsewhere, `.itcm` +8 B). Baselines are taken after this change.
- **What carries to the H735 (Stage 3):** the whole core is meant to run
  from ITCM there, so the flash half of this mostly does not apply. The
  ITCM half does: BTAC aliasing on ITCM addresses moved counts by up to
  1.70% for a 16 B shift, so a baseline on that board must also be built
  from an unchanged layout, and front-end code placed in ITCM should come
  after the core's.

## Amendments

### 2026-11-26: canonical noise statement (M6 review P1, T9)

This is the one statement of how much the on-board cycle counts move; other
docs quote it or point here. It corrects the Consequences text above ("far
outside the <= 0.61%", "cycle counts repeat exactly") and does not change
the Decision.
- **Same binary, reflashed or rebooted:** at most 0.03-0.05% (largest
  boot-to-boot |delta| 0.047%), except `storm-spsw`, which moved 0.67%
  between two boots of one binary (14.89M vs 14.99M; docs/perf.md "QSPI
  tuning (M6)"). Counts do not repeat exactly.
- **Front-end-only changes after this ADR:** bound workloads at most about
  0.2%, largest 0.38% (halt-irq).
- **A core layout change:** bound workloads moved up to 1.18% (cgb-raster,
  the core-first switch itself) and about +-1.7% via ITCM shifts; the storms
  moved up to about 4.2% (storm-div) from layout alone.
- **Consequence for the +3% ratchet:** for a core change on the bound set
  the margin over layout noise is about 1.3-1.8 points, not "60 times the
  spread" (that figure compared +3% with the reflash spread only). The
  storms can exceed +3% from layout alone, so a storm-only trip needs a look
  and the ratchet's treatment of the storms is an open question for the
  human (review finding P3).
- **Not yet enforced:** no gate step runs `check-cycles.sh` today. Gate
  step B2 is M7 work; only the script's host test runs in the H7 checks.
