# 0026: Hot core code goes in ITCM by front-end link rules

- Status: Accepted
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

### 2026-11-24: sized for the final MCU; the H753 plan is report-only

Human decision, 2026-11-24: the size caps are the ones valid for the final
MCU, the STM32H735IGK6, not for the dev board; the Stage 2 NUCLEO-H753ZI
may run slower as a result (it is a temporary step). The per-function caps
(measured + 5% on each of the 55 functions in the H753 set) kept bending
the code for a limit the final chip does not have: a fix chose a slower
form to stay 1 byte under `write_ppu`'s cap, and `Apu::write` sat at
322 of 322 B.

Facts (RM0468 Rev 3 §2.4 Table 8; option byte `TCM_AXI_SHARED[1:0]` in
`FLASH_OPTSR2_PRG`, §4.9.25): the H735's ITCM is 64, 128, 192 or 256 KiB,
taking 0, 64, 128 or 192 KiB from AXI SRAM (320, 256, 192 or 128 KiB
left). gbc-core's whole `.text` is 97,136 B (rlib, padding included).

What changes:
- **H735 (Stage 3+): ITCM = 128 KiB (`TCM_AXI_SHARED` = 01) and all of
  gbc-core goes in it.** There is no hot set to choose. 192 KiB would
  leave 196,608 B of AXI SRAM, less than the 231,424 B of two frames,
  audio buffers and 128 KiB of worst-case cart RAM; 128 KiB leaves
  262,144 B (30,720 B spare). Arithmetic in docs/perf.md "ITCM placement
  plan".
- **The one size gate** is gbc-core total `.text` <= 131,072 - 8,192
  (platform reserve, the same assumption as above) = 122,880 B, in
  `perf/code-size-budgets.txt`, checked by `scripts/check-code-size.sh`.
  Provisional until the H735 firmware crate exists and its linked ELF can
  be measured. Every per-function line is now report-only (size, old cap,
  %); a missing symbol is a warning.
- **The H753 placement above becomes the dev-board plan.**
  `perf/itcm-sections.txt` and `scripts/check-itcm-set.sh` stay: a glob
  that matches nothing still fails (the Stage 2 linker rules are generated
  from the list), but the 57,344 B fit and the callee closure are reported
  with a `WARN` line and exit 0. If the set outgrows the H753 budget, the
  Stage 2 build drops functions in the reverse fill order of the
  2026-11-24 amendment above.
- The docs/perf.md section is now "ITCM placement plan" (the "(M8)"
  references above are to the same section): final-chip plan first,
  dev-board plan second.

### 2026-11-26: measured placement on the NUCLEO-H753ZI (Stage 2 M6)

Status moved from Proposed to Accepted: the mechanism (link-time globs in
`perf/itcm-sections.txt`, generated by `tools/memory-x`, checked by
`check-itcm-set.sh` and `check-itcm-elf.sh`) is what the `headless`,
`paced` and `player` images ship, and the board runs them. The decision
text is unchanged. Facts below are from `crates/gbc-nucleo-h753/out/player.elf`
(section headers and `nm`, 2026-11-26, the M5 build) and `tools/memory-x`.

**Code.**
- `.itcm`: 64,080 B of the 65,280 B usable (ITCM at 0x100..0x10000; the
  first 256 B are left free), 1,200 B free. It is the ITCM set minus the
  plain tile path (`GBC_ITCM_DROP` default `plain tile path`,
  `memory_x::ITCM_DROP_DEFAULT`), plus the memory builtins and long-branch
  thunks. The ELF's `Renderer::render` is in flash (0x08027599). The
  64,080 B is the same in `headless`, `paced` and `player`
  (docs/perf.md "Paced loop (M4)", "Player (M5)").
- The front end is not in ITCM: DMA1 and LTDC/SysTick handlers, the DAC
  ISR and `scale_frame` are in flash (0x0800xxxx), behind the I-cache.
  The "8 KiB reserve for the front end" assumed above is not used. With
  that, the player's whole-loop time is 14.6% (cgb-acid2) and 29.7% (Wario
  Land 3) of the real-time budget at most, 0 frames over budget, so no
  front-end function has needed to move (docs/perf.md "Player (M5)").
- So nothing beyond the plain tile path leaves ITCM, and the drop order
  above stays as the plan if the set grows (per-line object path, then
  `write_ppu`, then the APU set).

**Data** (RM0433 Table 7; the MPU map is `src/mpu.rs`):

| Region | Contents (player image) | Bytes | Why |
|---|---|---|---|
| DTCM `.data` | `MEM` (`Memory`: WRAM, VRAM, OAM) 51,016; `RING` (audio feed ring) 8,228 | 59,284 | zero-wait, CPU-only |
| DTCM `.dtcm_rodata` | `SPREAD`, `ATTR_FETCH`, `POP8`, gbc-core's `.rodata.cst*` pools, copied from flash at boot | 2,432 | read per access/line by the ITCM set |
| DTCM `.bss` | `FRAME` (the core's `Frame`) 46,080, the rest of the statics | 46,212 | the CPU alone writes and the scaler reads it; the LCD DMA never does |
| DTCM guard + stack | 128 B no-access MPU region, then the stack (holds `Emulator`, 952 B) | 128 + 22,912 | measured use 12,968 B (docs/reference/player.md) |
| AXI SRAM `.axisram` | `CART_RAM` 131,072; `SAVE` 65,600; `STASH` 46,080; `AUDIO` 2,200; `TEE` 4,156 | 249,108 (below 0x2403d000; padding between) | too big for DTCM: cart RAM is up to 128 KiB (MBC5), save and stash are whole-buffer copies |
| AXI SRAM, DMA buffers | LCD transfer buffer 131,072 (at 0x24040000, 128 KiB aligned); DAC buffer 2,048; link buffer 8,192 | 141,312 | DMA1/2 cannot reach DTCM; each is an MPU Normal non-cacheable window |
| AXI SRAM `.fault_stack` | HardFault handler stack | 2,048 | survives a stack overflow |

DTCM is fully used: statics end at 0x2001a600 (108,032 B), then the guard and the stack to 0x20020000.
`.axisram` and the fault stack end at 0x24060800 (394,240 B of 512 KiB used).
The shipped change from the plan (docs/perf.md said "frame and audio
buffers go in AXI SRAM") is that `Frame` and the audio feed ring are in
DTCM; only buffers a DMA engine reads are in AXI.

**Carries over to the H735:** the mechanism and checks; the data split
(CPU-only data in DTCM, DMA-visible buffers and cart RAM in AXI, the
guard and fault stack); the 128 KiB DTCM budget (the statics above leave
the stack its measured 13 KB). **Does not carry over:** the ITCM tiering
and the plain tile path drop (the H735's ITCM takes all of gbc-core, see
the 2026-11-24 amendment); the 64 KiB `.itcm` fit; the front end running
from flash (a Stage 3 measurement).

**Open items.**
- The cost of the plain tile path drop was never measured on the board
  with and without it (the set does not fit with it). Only the H755
  bench's whole-ITCM vs flash figures exist (docs/perf.md "NUCLEO-H755ZI-Q").
- The `player` image's AXI use (394,240 B) is above the H735's 256 KiB of AXI SRAM at 128 KiB of ITCM; the
  Stage 3 layout needs its own sizing (the save and stash buffers are
  front-end choices).
- The player refuses saves of 128 KiB cart RAM (64 KiB save buffer;
  fixable by section order, M5 P-2).

### 2026-11-26: the plain tile path drop, measured (closes the first open item above)

Measured on the board (docs/perf.md "ITCM tier A/B (M6)"): the only fitting
ways to keep the plain tile path in ITCM are dropping Tier B per-event or
Tier C (APU) instead. Both are slower on the bound set (worst case +1.42% /
+2.35%, event-storm +1.14% / +1.86%) for at most -0.2% on cgb-raster. The
plain tile path stays the dropped tier.
