# Review C: code size and MCU fit of gbc-core

Reviewer C, tree at HEAD `49c0756`. Read-only; nothing tracked was changed.

## Verdict

**The core fits both MCUs with room to spare.** RAM: 51.8 KB of core state
against 128 KB of DTCM. Code: the ITCM hot set is 49,400 B against a
57,344 B budget (64 KiB minus an 8 KiB platform reserve). Nothing found
blocks the H753 or the H735.

The problems are in how fit is *measured and planned*, not in the fit itself:
the ITCM list is stale and misses per-access functions, the gate measures a
build the firmware will not use, and the one budget that is at 99% (total
`.text`) is not a real MCU constraint.

## How this was measured

- Build: `thumbv7em-none-eabihf`, release, `-C target-cpu=cortex-m7`, rustc
  1.100.0, same as `scripts/check-code-size.sh`. Both gate scripts pass
  (`.text` 98,124 B; ITCM set 49,400 B, margin 7,944 B).
- Tools: Arm GNU 13.3 `arm-none-eabi-{nm,size,objdump,readelf}` from
  `~/.local/opt/arm-gnu-toolchain-13.3.rel1-x86_64-arm-none-eabi/bin`.
- Struct layouts: DWARF from a `-C debuginfo=2` build of the same target
  (so these are the real thumbv7em layouts, not x86).
- Variants: rebuilt with one flag changed each, in a scratch target dir.
- One source experiment (F7) on a scratch copy of the crate, since deleted.
- All builds ran under the shared `flock`. No benchmarks were run, so every
  cycle figure below is an **estimate** and is marked as such.
- Working files (disassembly, layouts, call-graph edges, per-variant symbol
  tables) are in
  `/tmp/claude-1000/-home-jess-gbc-32/771053a9-0ac5-45ad-953b-96007463309d/scratchpad/C/`.
  I could not finish cleaning that folder's scratch outputs to nothing; about
  13 MB of text files remain there and can be deleted.

## Measured facts

### RAM (thumbv7em layouts)

| Item | Bytes | Notes |
|---|---|---|
| `Emulator` | 824 | `Hw` 816 + `&mut Memory` |
| `Hw` | 816 | `Sys` 792, `Cpu` 18, `stop_on` 2 |
| `Sys` | 792 | `Ppu` 416, `Cart` 88, `Schedule` 80, `Timer` 56, `InputQueue` 40, `Config`/`Serial`/`Hot`/`VramDma` 24 each |
| `Ppu` | 416 | `Colors` 128, `OamDma` 72, `Renderer` 52, `LineTiming` 40, `ScanProgress` 40 |
| `Memory` | 50,960 | wram 32,768; vram 16,384; `bg_pairs` 512; oam 160; `oam_dma_old` 160; hram 127; `extra_oam` 96; io 128; `obj_buf` 176; `obj_rank` 197; palettes 128; `Apu` 104; `ReadClears` 20 |
| `Ctx` (per run call, on the stack) | 64 | includes a 24 B copy of `Hot` |
| `.rodata` | 4,132 | `SPREAD` 1,024; `ATTR_FETCH` 1,024; `POP8` 256; the rest is panic locations and strings |
| **Core-owned total** | **51,784** | plus 2,304 B of tables if copied to DTCM |
| Caller: `Frame` | 46,080 | `[u16; 23040]` |
| Caller: audio | 2,200 | 550 `StereoSample` per `run_frame` |
| Caller: cart RAM | up to 131,072 | |
| Caller: ROM | up to 8 MiB | read-only, byte loads only (ADR 0027) |

Stack, static worst case over the call graph (pushes plus `sub sp`, with the
three recursion cycles of F10 broken at one level):

| Path | Bytes |
|---|---|
| `run_frame` -> `Emulator::run` (192) -> `Cpu::run` (64) -> ... -> `Ppu::latch` (168) -> ... -> `set_stalls_retriggered` (88) | 1,212 |
| `Emulator::load` | 1,224 |
| `Emulator::reset` -> `Sys::reset` (952 B frame: a `Sys` temporary) | 1,120 |
| `Emulator::eject` (936 B frame) | 1,056 |

### Mapping onto the MCUs

Region sizes from DS12117 (H753) and DS13312 (H735), both in
`../gbc-32-hardware/datasheets/`.

| Region | H753 | H735 | What goes there |
|---|---|---|---|
| ITCM | 64 K | 64 K, or 128/192/256 K by taking from AXI | ITCM set 49,400 B + platform reserve 8 K |
| DTCM | 128 K | 128 K | `Memory` 50,960 + `Emulator` 824 + tables 2,304 + stack ~4 K = ~58 K. With the `Frame` (F5): ~104 K |
| AXI SRAM | 512 K | 128 K + up to 192 K shared | cart RAM 128 K, audio ring, scaled line buffers |
| SRAM1-3 / AHB | 288 K | 48 K (3 x 16 K) | platform data |
| Flash | 2 MB | 1 MB | all of gbc-core is under 100 KB |
| External | QSPI NOR 8 MiB (shield) | FMC SDRAM | ROM |

H735 with 128 K of ITCM leaves 256 K of AXI SRAM: cart RAM 128 K plus two
frames 92 K is 220 K, so it fits with 36 K over. With 192 K of ITCM the AXI
SRAM is 192 K and only one frame fits beside the cart RAM.

**Hot data is compact.** The per-instruction path touches `Cpu` (18 B),
`Ctx` (64 B), and the first 100 B of `Sys` (`cart.rom_win` at +0,
`wram_off` at +92, `ie`/`iflags` at +96/+97). With the stack and `Emulator`
in DTCM none of it goes through the D-cache. The D-cache then serves only
the ROM, cart RAM and whatever buffers sit in AXI SRAM.

Nothing is sized wastefully. There are about 30 `u64` timestamps (`Schedule`
8, `OamDma` 7, `Timer` 5), but the per-access path uses the `i32`
`Hot::budget`, not the 64-bit clock.

### Where `.text` goes (98,124 B)

| Module | Bytes | Functions |
|---|---|---|
| `cpu` (`run` 24,584; `execute_cb` 6,892; `step_not_running` 566; `stop` 550) | 32,880 | 6 |
| `ppu` (mod, edges) | 19,264 | 53 |
| `bus` | 19,120 | 65 |
| `ppu::render` + `objects` | 8,018 | 26 |
| `apu` (all) | 7,000 | 27 |
| `lib.rs` (`Emulator`, `vdma_turn`) | 3,906 | 17 |
| `cart` | 3,072 | 20 |
| `timer` | 2,092 | 10 |
| `Debug`/`Display` impls and `fmt_flags` | 1,534 | 18 |
| other (joypad, schedule, dma, hdma, memory, core generics) | 1,312 | |

Cross-cutting:

| Item | Bytes |
|---|---|
| `nop` alignment padding | 5,834 (2,458 in `run`, 1,064 in `execute_cb`) |
| Cold code (`.text.unlikely.*`, i.e. `#[cold]`) | 15,700 |
| Inlined bus-read decode in `run` | ~90 copies (90 `read_high`, 89 `read_vram`, 88 `read_ram` call sites) |
| Monomorphization duplicates | 600 (`fill_partial` and `partial_tile`, `<false>`/`<true>`) |
| Panic call sites | 17 `panic_bounds_check`, 12 `slice_index_fail`, 2 `len_mismatch_fail`, 2 div-by-zero, 1 `panic_fmt`. None in `Cpu::run` or `execute_cb` |

Build variants (whole crate, same source):

| Variant | `.text` | `run` | `execute_cb` | nop bytes | ITCM set |
|---|---|---|---|---|---|
| Gate build (opt 3) | 98,124 | 24,584 | 6,892 | 5,834 | 49,400 |
| `-C llvm-args=-align-loops=1` | 92,128 | 22,108 | 5,806 | 0 | 45,016 |
| `opt-level = 2` | 89,498 | 21,824 | 4,558 | 290 | 43,086 |
| `opt-level = "s"` | 84,400 | 21,798 | 4,530 | 286 | 40,860 |
| `opt-level = "z"` | 67,586 | 13,206 | 3,814 | 108 | not measured |
| `codegen-units = 1` | 94,880 | 24,512 | 6,892 | 5,794 | 49,630, 3 globs missing |
| `-C force-frame-pointers=no` | 98,124 | 24,584 | 6,892 | 5,834 | identical: no effect |

Two things stand out. Opt-level 2 gives almost the same `run` as opt-level 3
minus the padding (21,824 against 22,108 without nops). And `execute_cb` at
opt 3 carries 1,270 B of real extra code over opt 2 beyond its 1,064 B of
nops.

---

## Findings

### C1 The ITCM set misses per-access functions and has no closure check   [severity: medium] [confidence: high]
- Where: `perf/itcm-sections.txt`; `scripts/check-itcm-set.sh`;
  `docs/perf.md` "ITCM placement plan (M8)"; ADR 0026.
  `crates/gbc-core/src/ppu/mod.rs:934-949` (`vram_blocked` ->
  `cpu_blocked`), `crates/gbc-core/src/bus.rs:894` (`run_ppu_to_call`),
  `ppu/mod.rs:2592-2594` (`scan_line`).
- What: the list was written at 5e65f6f and not re-derived after the size
  pass moved code around. Three defects:
  1. Functions that ITCM code calls on per-access paths are in flash, so
     each call crosses ITCM -> flash through a linker veneer and runs behind
     the I-cache:
     - `Ppu::cpu_blocked` (186 B). `vram_blocked` and `oam_blocked` shrank to
       58 B and 68 B thunks that call it unconditionally, so it runs on
       **every CPU VRAM and OAM access**. The thunks are in the set; the body
       is not.
     - `Cart::read_ram` (68 B) and `Cart::read_rtc` (92 B): 88 call sites in
       `Cpu::run`, on every cartridge-RAM read.
     - `Ctx::run_ppu_to_call` (142 B): called from `sync_ppu`, `write_ppu`
       and `Cpu::run`.
  2. `Ppu::scan_line` (78 B) is in the set but is `#[cold]` (only lines an
     OAM DMA touches). `docs/perf.md` still lists `scan_to` as a per-line
     hot function; it is `#[cold]` too.
  3. The docs say the margin is 1,538 B ("thin", ADR 0026). It is now
     7,944 B, and the per-line PPU set that was pushed to flash for lack of
     room would fit: `latch` 1,702 + `fill_mixed` 1,036 + `set_stalls` 824 +
     `fetch_objects` 750 + `partial_tile::<true>` 182 +
     `fill_partial::<true>` 138 + `carry_sel` 132 + `wy_late_latched` 56 =
     4,820 B. (`LineObjects::scan` and `priority_order` have no remaining
     callers in the rlib: they are inlined, see C4.)
- Measurement: a call-graph pass over the rlib relocations finds 35
  non-cold functions (13,756 B) called directly from the ITCM set but
  outside it, and 24 cold ones (9,056 B). Estimate: a veneer plus a flash
  fetch costs 10-15 cycles per call; at around 10,000 VRAM/OAM accesses in a
  heavy frame that is 0.1-0.15 M cycles, under 2% of an 8 M-cycle frame.
  The cost is small; the point is that the check cannot see it.
- Also: the sample linker fragment in `docs/perf.md` places
  `__aeabi_uldivmod` in ITCM. All four of its call sites are `#[cold]`
  (`noise::long_steps` x2, `apu::rem_long`, `Ctx::oam_dma_during_vdma`), so
  it should stay in flash.
- Proposed change:
  1. Add `cpu_blocked`, `read_ram`, `read_rtc`, `run_ppu_to_call` (488 B)
     and the 4,820 B per-line set; drop `scan_line`. Total about 54,630 B,
     margin about 2,700 B. C3's alignment flag would free a further
     4,384 B of the current set if more room is wanted.
  2. Add a closure check to `check-itcm-set.sh`: from `objdump -dr`
     relocations, fail when an ITCM function calls a function that is
     neither in the set nor in a `.text.unlikely.` section, unless it is on
     an explicit "warm, stays in flash" allow-list in the same file. This is
     a stricter check (allowed without escalation).
  - Effort S (list) + M (script). Risk: none to behavior. Test: mutation,
    remove `cpu_blocked` from the list and see the check fail.

### C2 The size gate measures a build configuration the firmware will not use   [severity: medium] [confidence: high]
- Where: `scripts/check-code-size.sh:16-18`, `scripts/check-itcm-set.sh:17-19`
  (plain `cargo build --release`, default 16 codegen units, no LTO);
  `perf/code-size-budgets.txt`; `perf/itcm-sections.txt`.
- What: every budget and every ITCM glob is checked against the default
  release rlib. A firmware build will use `codegen-units = 1` and LTO, which
  changes inlining, so the checked symbol names and sizes are not the ones
  that will be linked. `docs/perf.md` says "measure the firmware ELF, not
  the rlib" but defers that to Stage 2.
- Measurement: with only `CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1`, `.text`
  is 94,880 B (-3,244), the function count drops from 262 to 223, and
  **3 of the 55 ITCM globs match nothing**: `Ppu::vram_blocked`,
  `Ppu::oam_blocked`, `render::frame_row` (inlined). Budget lines for
  `Sys::reset` and `LineObjects::scan` would report MISSING too. LTO was not
  measured (there is no firmware crate to link), and will move more.
- Proposed change: either (a) pin `codegen-units = 1` in both scripts now
  and re-derive the lists against it, or (b) better, add a 30-line
  `tools/mcu-probe` `no_std` binary for thumbv7em (a `main` that calls
  `Emulator::new/load/run_frame`, a trivial panic handler, LTO on) and run
  the size and ITCM checks on its ELF. (b) also gives the real post-GC size
  and the veneer count today instead of in Stage 2. Effort M. Risk: none to
  the core. Note (b) adds a workspace member, not a `gbc-core` dependency.

### C3 The total `.text` budget is at 99% but is not an MCU constraint   [severity: medium] [confidence: high]
- Where: `perf/code-size-budgets.txt:122-126` (`99000 (total .text)`,
  raised by the human on 2026-11-24); `docs/perf.md` "Code-size budgets",
  "Size pass, M9", "Code size vs other cores".
- What: flash is 2 MB (H753) or 1 MB (H735); 98 KB is 5-10% of it. The real
  constraint is the ITCM set, which has 7,944 B of margin. The total budget
  counts bytes that never reach ITCM and some that never reach the ELF:
  - 5,834 B of `nop` padding;
  - 15,700 B of `#[cold]` code, which is exactly where correctness work
    (mid-line PPU writes) lands;
  - 1,534 B of `Debug`/`Display` code and API such as `Emulator::peek`
    (414 B), `step` (342 B) and `set_buttons_at` (640 B) that the linker
    drops if the firmware does not use them.
  The budget has already cost a human escalation for three cold-path
  Mealybug fixes.
- Measurement: see the variants table. `-align-loops=1` alone gives
  92,128 B; opt-level 2 gives 89,498 B.
- Two corrections to `docs/perf.md`:
  - "Per-module opt levels (cold paths at z, hot set at 3) are a lever": not
    within one crate on the pinned toolchain. `#[optimize(size)]` is
    rejected by rustc 1.100.0 (E0658, unstable). It would need the cold
    code split into its own crate with a per-package profile.
  - The nop padding is attributed to "blocks after unconditional branches";
    it disappears completely with `-C llvm-args=-align-loops=1` and almost
    completely at opt-level 2.
- Proposed change: keep a total as a loose tripwire (for example 110,000 B)
  and make the ITCM-set budget the binding one. Loosening the total needs
  the human (it is a gate change that lets a failing build pass). Decide
  the alignment flag and opt-level 2 with reviewer B's instruction counts:
  estimate for dropping alignment is 0 to 1 cycle per taken branch to an
  unaligned target, under 1% of core time. Effort S.

### C4 Seventeen function budgets have so much slack they guard nothing   [severity: low] [confidence: high]
- Where: `perf/code-size-budgets.txt`; the size table in `docs/perf.md`
  "Code-size budgets" (still "as of ADR 0011 step 4": `run` 27,760,
  `execute_cb` 12,622, `latch` 4,368).
- What: budgets were set at "measured + 5%" and not lowered when functions
  shrank.
- Measurement: 17 lines are under 80% used, with 7,099 B of slack between
  them. Worst: `Ppu::latch` 1,702 of 4,590 (37%), `vram_blocked` 58 of 210,
  `oam_blocked` 68 of 260, `fill_partial::<true>` 138 of 455, `boundary`
  1,142 of 2,220, `render` 1,182 of 1,765. `latch` could grow 170% unseen.
  Separately, the `LineObjects::scan` line (482 of 510) measures a copy
  nothing calls: every use is inlined (no relocation in the rlib targets
  it), so that budget guards dead code the linker will drop.
- Proposed change: reset every line to measured + 5% (stricter, no
  escalation needed) and regenerate the perf.md table from the script
  output. Effort S.

### C5 The plan puts the frame buffer in AXI SRAM for a reason that does not hold   [severity: medium] [confidence: medium]
- Where: `docs/perf.md` "ITCM placement plan (M8)", DTCM paragraph ("Frame
  and audio buffers go in AXI SRAM, because the LCD and SAI DMA must reach
  them"); ADR 0013; `crates/gbc-core/src/lib.rs:479` (`run_frame`).
- What: ADR 0013 establishes that neither display shows the frame 1:1, so
  the platform always reads every pixel once to scale and convert it. The
  `Frame` is therefore never a DMA source; the scaled line or frame buffer
  is. That removes the reason to keep it out of DTCM. Keeping it in AXI SRAM
  sends 46,080 B of writes per frame through a 16 KiB D-cache (H753) that
  also holds the ROM lines.
- Measurement: DTCM use with the frame is 50,960 + 824 + 2,304 + 46,080 =
  100,168 B, leaving 30.9 KB for the stack and platform data. Estimate of
  the cost avoided: 1,440 cache lines written per frame, each a
  write-allocate fill (~20 cycles, ~29 K cycles per frame) and each evicting
  a line that may be ROM. If all evicted lines were ROM lines refetched at
  150-400 cycles (SDRAM estimate from `stage3-rom-memory-options.md`; QSPI
  is slower), that is 0.2-0.6 M cycles per frame, 2.5-7% of an 8 M-cycle
  frame. Upper bound, unmeasured.
- Related, for the Stage 2 design rather than the core: the ILI9341 on the
  shield takes two byte writes per pixel on the 8-bit bus. At its 66 ns
  minimum write cycle (unverified from memory of the datasheet; confirm in
  `ili9341v-lcd-driver-datasheet.pdf`), a 240x216 image is 103,680 writes,
  about 6.8 ms, 41% of a frame if the CPU blocks on it. So the push must be
  DMA-driven from a converted buffer, which is one more reason the `Frame`
  itself need not be DMA-reachable. [Correction 2026-11-26: the shield on this board is the $AZ2 variant with an ST7789V; same 66 ns write cycle.]
- Proposed change: in the placement plan, put `Frame` in DTCM and scale and
  convert right after `FRAME_READY` into a DMA-reachable buffer; one `Frame`
  is then enough (no double buffering, 46 KB saved, which matters on the
  H735's 256 K AXI SRAM). No core change. Effort S (doc); confirm in
  Stage 2 with the cycle counter.

### C6 Every instruction pays four separate flag checks before the fetch   [severity: medium] [confidence: medium]
- Where: `crates/gbc-core/src/cpu/mod.rs:386-437` (`run`, `step_inline`);
  disassembly of `Cpu::run` offsets 0x40-0x302.
- What: the common path from loop top to the `tbh` dispatch is 40
  instructions (46 with IME set): 14 loads, 2 stores, 11 conditional
  branches. Eleven of them are four independent byte tests: `ctx.done`,
  `self.mode != Running`, `self.ime && ie & if`, `self.ei_pending`. Five of
  the loads re-read loop-invariant pointers (`&ctx.hot.dma`, `&ctx.sys`,
  spilled to `[sp, #20]` and `[sp, #24]`, then dereferenced).
- Measurement: 464 of `run`'s 8,996 instructions (5.2%) are reloads from
  the stack of five loop-invariant addresses; `run` stores to the stack only
  15 times. Estimate: fetch plus dispatch costs about 35 cycles on the M7
  including one mispredicted `tbh`; a NOP sled at double speed (2.1 M
  fetches per second) would be about 73 M cycles per second, 15% of
  480 MHz. Folding the four checks into one would remove about 9 of the 40
  instructions, an estimated 5-8% of CPU-interpreter time. Unmeasured;
  reviewer B's benchmark decides.
- Proposed change: one "attention" condition for the slow cases. The
  mechanism already exists: `Hot::budget <= 0` sends the CPU to
  `Ctx::boundary`. Make `done`, a pending-and-enabled interrupt, and
  `ei_pending` force the budget to zero when they are set (on IE/IF/IME
  writes, on EI, when the run target is reached), and test `mode` and the
  rest only on that path. Effort M. Risk medium: interrupt and EI timing is
  covered by the SM83 tests, mooneye `ei_timing`/`intr_timing`, and
  `diff-fingerprint.sh` must stay identical.
- Not actionable on stable: r7 is reserved as a frame pointer in every
  function (the target's default; `-C force-frame-pointers=no` produced a
  byte-identical build), which costs the interpreter one of eight
  callee-saved registers.

### C7 Register pairs are stored big-endian, so each 16-bit use byte-swaps   [severity: low] [confidence: high]
- Where: `crates/gbc-core/src/cpu/mod.rs:21-32` (`Registers`, fields in
  order a, f, b, c, d, e, h, l); `cpu/exec.rs:75-88` (`bc`/`de`/`hl` via
  `from_be_bytes`).
- What: with `h` at offset 10 and `l` at 11, `hl()` compiles to
  `ldrh; rev; lsrs #16` and `set_hl` to the reverse. Storing the low byte
  first makes it one `ldrh` or `strh`.
- Measurement (scratch copy, `#[repr(C)]` with fields f, a, c, b, e, d, l,
  h): `.text` 98,124 -> 97,910 B (-214), `run` -208 B, 47 `rev`
  instructions removed. Saves 2 instructions on each (HL)/(BC)/(DE) operand
  and each 16-bit inc/dec/add.
- Proposed change: reorder the fields and add `#[repr(C)]` (safe; it also
  makes the layout guaranteed rather than incidental). The public field
  names do not change. Effort S. Test: the SM83 single-step suite and
  `Emulator::registers` tests.

### C8 `Memory` puts the two big arrays first, so small hot arrays sit past the 4 KB offset range   [severity: low] [confidence: medium]
- Where: `crates/gbc-core/src/memory.rs` (`bg_pairs` +0, `wram` +512,
  `vram` +33,280, then `oam` +49,664, `hram` +49,984, `io` +50,207,
  palettes +50,708, `apu` +50,856).
- What: Thumb-2 load/store offsets reach 4,095 B. Every access to OAM,
  HRAM, IO, the palettes, `obj_buf` or the APU needs the offset built first
  (`movw` plus an add).
- Measurement: 125 instructions in the crate materialize a constant in
  49,000-50,999, about 500 B and one extra instruction per access. None are
  in `Cpu::run` (HRAM goes through `read_high`/`write_high`). Not measured
  as a variant.
- Proposed change: declare the small arrays first (about 1.6 KB in all) and
  `wram`, `vram` last. `wram` then starts near +2,100; only the first 2 KB
  of it stays in direct range, so measure the size before committing.
  Effort S, zero behavior risk (all-zero `const fn new` is unaffected).

### C9 One dirty bit for up to 128 KiB of save RAM forces a snapshot or a stall   [severity: low] [confidence: medium]
- Where: `crates/gbc-core/src/lib.rs:688-725` (`cart_ram`, `save_dirty`,
  `clear_save_dirty`); ADR 0015.
- What: `cart_ram()` borrows the emulator, so the firmware cannot run a
  frame while a background eMMC or flash write reads the buffer. It must
  either block emulation for the whole write or copy the save first. With
  one `bool`, it cannot tell which part changed, so the copy and the write
  are the full size.
- Measurement: worst case a 128 KiB shadow buffer (half of the H735's AXI
  SRAM at the planned split) and a 128 KiB write per save, when a typical
  game save touches a few hundred bytes. Estimate, not measured.
- Proposed change: add a `u32` dirty map, one bit per 4 KiB of cart RAM
  (`Emulator::save_dirty_blocks()`, cleared with the flag). Cost: one
  shift-and-OR on the cart-RAM write path, 4 B in `Cart` (88 B, `Hw` stays
  under its 864 B limit). The firmware then copies and writes 4 KiB at a
  time. This is a public API addition, so it needs an ADR and one reviewer.
  Effort S-M. Could wait for Stage 2 to show whether saves hitch.

### C10 Three mutual recursions block a static stack bound   [severity: low] [confidence: high]
- Where: `bus.rs:2194`/`2231` (`read_high` <-> `read_during_dma`);
  `ppu/mod.rs:2163-2223` (`set_stalls` <-> `set_stalls_retriggered`);
  `ppu/render.rs:474-656` (`render` <-> `render_held`/`render_zero`).
- What: each is bounded at one level by state (`hot.without_dma()`,
  `prev_wx`, the held/zero flags), which I confirmed by reading, but a tool
  such as `cargo-call-stack` cannot prove it. Separately, `Sys::reset`
  (952 B frame) and `Emulator::eject` (936 B) build a whole `Sys` on the
  stack and copy it.
- Measurement: worst static depth 1,224 B with the cycles cut at one level.
  The `lib.rs` doc assumes "a few KiB" of stack; that holds.
- Proposed change: none needed for fit. If a provable bound is wanted in
  Stage 2, split each pair so the inner call targets a non-recursive
  `*_plain` function. Effort S each.

### C11 The hardware contract overstates emulator state by 4x   [severity: low] [confidence: high]
- Where: `../gbc-32-hardware/docs/firmware-interface.md` RAM row ("Emulator
  state (~200 KiB)"); `../gbc-32-hardware/docs/decisions/0002-mcu-stm32h735igk6.md`
  ("emulator state (~200 KB)").
- What: measured state is 51,784 B. Even with one frame, audio and the
  maximum cart RAM it is about 231 KiB, of which 128 KiB is cart RAM.
- Proposed change: the hardware repo is human-owned, so this goes on
  STATUS "Needs human" as a one-line correction. It does not change the MCU
  choice.

---

## Platform interface realism (question 4)

| Area | Assessment |
|---|---|
| Frame | Whole-frame RGB555, written line by line. Workable. The format conversion is free when fused with scaling (ADR 0013 holds). See C5 for placement. A per-line sink would cut 46 KB to 320 B but adds a callback or 144 returns per frame; not worth it at these RAM sizes. |
| Tearing | Not controllable from the core. `FRAME_READY` at line 144 is the right hook; whether the shield exposes the panel's TE pin is a board question. |
| Audio | `StereoSample` is `#[repr(C)]` `{i16, i16}`, so the free half of an I2S/SAI DMA ring can be passed straight in as the `audio` slice, zero-copy. `AUDIO_BUFFER_FULL` plus `run_until` supports pacing emulation from the audio clock. The DC-blocking filter is left to the platform and can run in place. 32,768 Hz needs an 8.388608 MHz MCLK (x256) from a fractional PLL. |
| ROM | `&[u8]` over memory-mapped QSPI or SDRAM. ROM loads are byte loads (ADR 0027; the gate script checks it, I did not re-run it). Miss cost is covered in `stage3-rom-memory-options.md`; real-game miss rates are still unmeasured. |
| Save RAM | Works; see C9 for the granularity. |
| Input, RTC | `set_buttons` once per frame and `set_rtc_state` after `load` are sufficient. |
| Construction | `Memory::new()` is `const` and all zeros, so it lands in `.bss`. `Emulator::new` returns 824 B by value; fine. |

---

## Bigger-picture recommendations

Ranked by value over cost.

1. **Re-derive the ITCM list and add the closure check (C1).** Half a day.
   It turns "which code is hot" from a hand-kept list into something the
   gate verifies, and it uses margin that is sitting idle.
2. **Measure a linked ELF now (C2).** A minimal thumbv7em probe binary with
   LTO gives the real size, the real symbol set and the veneer count before
   Stage 2 depends on them. Every size number in the repo today is for a
   build nobody will flash.
3. **Demote the total `.text` budget (C3, C4).** Make ITCM the binding
   budget, tighten the per-function lines to measured + 5%, and refresh the
   stale tables in `docs/perf.md` and ADR 0026. Needs the human for the
   loosening half.
4. **Decide `-align-loops=1` and opt-level 2 with reviewer B's numbers
   (C3).** Between them they are worth 6-8.6 KB overall and 4.4-6.3 KB of
   ITCM, for a cost that is probably under 1% but is unmeasured.
5. **Fix the DTCM plan to include the frame (C5).** A doc change now, a
   measurement in Stage 2.
6. **Trim the per-instruction preamble (C6) and the register byte order
   (C7).** These are speed work with a size side effect; do them only with
   benchmark numbers.

## Looked at and found fine

- **State size and layout.** 51.8 KB, hot fields contiguous, no padding of
  note, no oversized enums or `Option`s.
- **Floats.** No VFP instruction anywhere in the crate.
- **Literal pools.** None; constants and table addresses use `movw`/`movt`.
- **Division.** `__aeabi_uldivmod` has 4 call sites, all `#[cold]`.
  Hardware `udiv` (2-12 cycles) appears once or twice in each APU
  `advance`/`integrate` and in `Noise::rebase`. I did not establish whether
  the ones in `integrate` sit inside the per-segment loop.
- **64-bit arithmetic.** 3 carry-chain instructions in `Cpu::run`, 0 in
  `execute_cb`, 33 in `boundary`, 11 in `Ppu::sync`, 14 in `write_ppu`. The
  per-access tick is a 32-bit load, subtract, store, compare.
- **Opcode dispatch.** One `tbh` over 256 entries, single level, handlers
  branch back to one loop head. `execute_cb` uses two `tbh`.
- **Panic machinery.** 34 sites in the whole crate, none in the
  interpreter. `core::fmt` is referenced only from `Debug`/`Display` impls
  and the panic paths, and is dropped at link if unused.
- **Renderer inner loop** (`fill_plain`). About 40 instructions per 8-pixel
  tile, 9 loads, 4 word stores, no bounds checks inside the loop.
- **Monomorphization.** `Cpu::run::<Ctx>` has one instance; the only
  duplicates are the two small const-generic pairs (600 B).
- **ROM loads.** Every ROM read I saw in `Cpu::run` is `ldrb`; I did not re-run `check-rom-loads.sh`.
- **Audio sample type.** `#[repr(C)]`, DMA-compatible.
- **Datasheet facts in `docs/perf.md`.** ITCM/DTCM/cache sizes for both
  parts match DS12117 and DS13312.
