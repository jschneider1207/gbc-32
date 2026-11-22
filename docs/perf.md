# Performance

ROMs, each run for 20 emulated seconds (83,886,080 clocks): `blargg/cpu_instrs/cpu_instrs.gb`
(CPU), `blargg/cgb_sound/cgb_sound.gb` (APU), `cgb-acid2/cgb-acid2.gbc` (PPU).
The test ROMs are read from `$TEST_ROMS` (default `test-roms/`).

Four synthetic ROMs cover paths those miss. `tools/bench` builds them in
memory (`bench synth:<name> <secs>`, hand-assembled SM83 in
`tools/bench/src/synth.rs`, 32 KiB, valid header and checksums; no ROM files
in git):

- `synth:event-storm` (2 s): CGB speed switch (KEY1 + STOP), then in double
  speed TIMA at 262 kHz with TMA=FF (an overflow every 8 clocks) and the
  serial port re-armed on the CGB fast internal clock after every transfer.
  Per emulated second: about 923k scheduler `boundary` calls (the event
  table; `run_events` is inlined there), 49,885 SC writes, 1 `Bus::stop`
  (the speed switch).
- `synth:dma-storm` (3 s): OAM DMA from C000 restarted every ~170 M-cycles
  while the CPU runs from ROM, copying ROM bytes to WRAM. Per second:
  731k `read_during_dma`, 120k `write_during_dma`, 120k `dma_sync` calls.
- `synth:halt-irq` (10 s): HALT woken by the timer IRQ (TIMA at 262 kHz
  from TMA=F0, every 256 clocks), RETI handler. Per second: about 16.4k
  wakeups (`boundary` calls, and 32.7k `step_not_running` and 2-byte IRQ
  pushes).

- `synth:worst-case` (2 s): the event storm above plus the busiest PPU
  frame in one program: LCD on with BG, window (WY=0, WX=87), 40 8x16
  objects in four groups of 10 (10 per line on 64 of 144 lines; 40 objects
  cannot cover more), every STAT source (modes 0/1/2, LYC=72) with IE=STAT
  and an 8-byte handler at 0x48 incrementing a WRAM counter. Replaces the
  sum of two separate runs (event-storm + acid2 PPU) as the worst case.
  callgrind: `Ctx::boundary` 15.9%, `Renderer::render` 9.1% (inclusive,
  with `fill_mixed`; 9.7% before the fused object mixing), timer 7.0%,
  serial 5.1%.

Call counts from `callgrind_annotate`-style totals of the `calls=` lines in
the callgrind output of a 1 s run.

Current numbers (Ir) and wall time (valgrind run of all seven, ~30 s):

| ROM | Ir |
|---|---|
| cpu_instrs.gb | 1,726,409,705 |
| cgb_sound.gb | 971,629,157 |
| cgb-acid2.gbc | 680,542,754 |
| synth:event-storm (2 s) | 446,532,249 |
| synth:dma-storm (3 s) | 439,495,485 |
| synth:halt-irq (10 s) | 290,969,131 |
| synth:worst-case (2 s) | 491,559,542 |

**Combined worst case.** `synth:worst-case` is 505.4 M Ir over 2 s =
119.455 frames, so 4.23 M Ir per emulated frame: 105% of the 4.02 M
(50% of M7) line, 0.21 M over it. The earlier sum estimate (event-storm
3.73 M + acid2 PPU 0.53 M = 3.97 M, 99%) was 0.26 M too low: the real
program also pays STAT IRQ entry (up to 3 per line) and the 10-object
lines. x86 proxy only. Fused object mixing (below) took it to 491.6 M =
4.12 M per frame, 102% of the line (0.10 M over).

**Scanline renderer (M3).** Drawing pixels costs about 220k Ir per frame
with the BG only (cpu_instrs, cgb_sound, halt-irq, event-storm and
dma-storm all +205-226k Ir/frame: ~1.5k Ir per line, ~9.5 Ir per pixel
including the per-line setup) and 375k Ir per frame with objects and the
window (cgb-acid2, mixing on most lines; it is a CGB game drawn in the
compatibility path until M5). From 1,464.1 / 709.3 / 341.7 / 422.1 /
399.0 / 159.7 M: +17.9%, +37.0%, +131.2%, +5.8%, +10.1%, +82.2%.
event-storm is now 3.74 M Ir per emulated frame (the 50%-of-M7 line in
the plan's M8 is 4.02 M, x86 proxy only). How the pixel loop got there
(cpu_instrs, Ir per frame added by rendering):

| Pixel loop | Ir/frame |
|---|---|
| Index buffer + mix for every span (renderer as written) | 466k |
| Spans with no object pixel: tiles straight to RGB555, no index buffer or mix | 318k |
| + 256-entry bit-spread table (512 B flash), unrolled whole-tile path | 220k |

**Fused object mixing.** Spans with object pixels used to decode the
tiles into a 160-byte color-index buffer, then mix it with the object
buffer pixel by pixel. Now the tiles are mixed as they are decoded, and a
whole tile with no object pixel (one 8-byte test of the object buffer)
takes the plain path; object fetches merge their 8 pixels into the object
buffer with one 64-bit operation instead of 8 conditional byte stores.
Measured at c33713d (Ir, 20 s / 2 s):

| Variant | cgb-acid2 | synth:worst-case | `render` + `fill_mixed` (B) |
|---|---|---|---|
| Before (index buffer, then mix) | 788,859,655 | 505,402,675 | 2,684 |
| Fused, unrolled mixed tile inlined twice (BG, window) | 681,103,233 | 492,404,783 | 3,684 |
| Fused, mixed tile as a loop | 691,503,254 | 495,893,713 | 2,856 |
| Fused, unrolled mixed tile out of line (`fill_mixed`) + 64-bit object merge (chosen) | 680,542,754 | 491,559,542 | 2,216 + 792 |

cgb-acid2 -13.7% (about 91k Ir per frame: rendering ~375k -> ~285k),
worst-case -2.7% (4.23 -> 4.12 M Ir per frame). BG-only ROMs and the
other synthetic benches moved less than 0.05%. The 64-bit object merge
alone was -1.5 M Ir on worst-case and -4.8 M on cgb-acid2 for +28 B; it
also removes 8 data-dependent branches per object fetch on the M7.
`Renderer::render` and `fill_mixed` have no panic or bounds-check calls
(x86 disassembly; on thumbv7em their only relocations are `SPREAD` and,
from `render`, `fill_mixed`).

PPU mode timing (ADR 0014) added one event per visible line (the end of
mode 3, where the renderer draws) plus STAT interrupt edges: from
1,388.0 / 607.5 / 12.9 / 413.7 / 372.1 / 111.4 M (+6.8%, +16.8%, +274k Ir
per frame on cgb-acid2, +2.6%, +9.0%, +45.7%). About 470 Ir per line event
with no STAT source selected and no objects, 940 with sources and objects
(cgb-acid2), dispatch included. CPU VRAM/OAM blocking then moved VRAM
accesses out of the inlined bus path (`read_vram`, `Ctx::write_vram`):
`Cpu::run` 29,224 -> 27,760 B and -1.2% on cpu_instrs, -1.6% on dma-storm
and halt-irq.

The incremental event-table minimum (`schedule.rs`: the table is rescanned
only when the earliest entry moves later or is taken; handlers re-arm
without a rescan) took event-storm from 516.4 M to 413.7 M (-19.9%;
`boundary` inclusive 190.7 -> 135.9 Ir per call, 1.85 M calls: about
4.32 M -> 3.46 M Ir per emulated frame), halt-irq -7.0%, dma-storm -3.2%.

## Code-size budgets

Hot CPU/bus functions in `gbc-core` have code-size budgets so the MCU build
does not silently grow (flash and I-cache pressure on the Cortex-M7).
Budgets are in `perf/code-size-budgets.txt` (`<bytes> <exact demangled symbol>`).

Run: `scripts/check-code-size.sh` (about 0.3 s warm, a few seconds cold).
Exit is non-zero if a symbol is over budget, is not found (renamed or
inlined away: update the budget file, never drop the line to pass), or the
build fails. It prints symbol, size, budget and % used.

Measurement: `RUSTFLAGS="-C target-cpu=cortex-m7" cargo build -p gbc-core
--release --target thumbv7em-none-eabihf --target-dir target/codesize`, then
`nm -S -C --radix=d` (GNU binutils; `llvm-nm`/`rust-nm` preferred if present)
on each object in the rlib. Sizes of same-named symbols (CGU copies) are
summed. Sizes as of ADR 0011 step 4, unchanged by step 5 (lazy timer: the tick is only the
countdown, so `run` dropped from 36,416 B and `execute_cb` from 15,254 B):

| Symbol | Size (B) | Budget (B) |
|---|---|---|
| `Cpu::run::<Ctx>` | 27,760 | 29,150 |
| `Cpu::execute_cb::<Ctx>` | 12,622 | 13,250 |
| `Ctx::write_high` | 80 | 90 |
| `Ctx::write_io` | 884 | 930 |
| `bus::read_high` | 848 | 890 |
| `Ctx::sync_ppu` | 214 | 230 |
| `Ppu::sync` | 2,758 | 3,120 |
| `Ppu::next_event` | 482 | 510 |
| `Ppu::next_point` | 412 | 440 |
| `Ppu::point_irq` | 494 | 520 |
| `Ctx::write_ppu` (PPU registers, out of `write_io`; mid-line flush) | 1,226 | 1,290 |
| `Renderer::render` (pixel loop) | 2,216 | 2,330 |
| `render::fill_mixed` (tile loop with object mixing) | 792 | 830 |
| `Renderer::start_line` (priority sort, buffer clear) | 2,208 | 2,320 |
| `LineObjects::scan` (OAM scan) | 482 | 510 |
| `Ppu::draw`, `Ppu::flush` | 122, 290 | 130, 305 |
| `Ctx::write_mapped` | 460 | 480 |
| `bus::read_vram`, `Ctx::write_vram` | 218, 216 | 230, 230 |
| `bus::read_oam`, `Ctx::write_oam` | 280, 304 | 300, 320 |
| `Ppu::vram_blocked`, `Ppu::oam_blocked` | 196, 240 | 210, 260 |
| `Ctx::write_timer_io` (cold) | 892 | 1,620 |
| `Timer::read` (cold) | 362 | 380 |

OAM DMA (M2): every CPU access ORs `Hot::dma` (FE00 during a transfer)
into its region dispatch, so a transfer sends every address to the
E000-FFFF arm, the only one that tests the flag (`read_high`, and the last
arms of the out-of-line `write_mapped`). Echo RAM moved out of the inlined
read into `read_high`. `write_high`, `write_io` and `sync_ppu` stay out of
line. The OR is +1 instruction per access (x86 `movzwl`+`or` replacing
`test`); the ROM windows became `[&[u8]; 2]` indexed by address bit 14,
which removed a window select LLVM computed twice per ROM read. Measured
layouts (check-bench Ir vs the pre-DMA baseline; `run` on thumbv7em):

| Layout | cpu_instrs | cgb_sound | cgb-acid2 | `run` (B) |
|---|---|---|---|---|
| 7bdaee2: read OR + flag test first in `#[inline(never)]` `write_mapped` | +2.94% | +4.31% | +3.05% | 32,560 |
| same, `write_mapped` `#[inline]` | -2.70% | +2.91% | +1.70% | 40,862 |
| OR in reads and writes, echo out of line (u16 key) | +2.03% | +3.74% | +3.18% | 29,928 |
| same, `usize` key | | +5.50% | | 29,536 |
| same (u16) + ROM windows by bit 14 (chosen) | +0.40% | -2.45% | -1.01% | 28,560 |

Earlier 7bdaee2 measurements: a flag test with an out-of-line call per
read and write was 32,928 B (34,504 with `#[cold]`), an extra match bit
50,376 B. With the chosen layout `run` is back under the pre-DMA
30,920 B; its budget is now 30,000 B (`perf/code-size-budgets.txt`).

Rule: raising a budget is committed with the change that needs it, with the
measured size and the reason in the commit body. Initial budgets are the
measured size plus about 5%.

## Instruction-count benchmark

`scripts/check-bench.sh` runs `tools/bench` (release build) under
`valgrind --tool=callgrind` and compares the instructions executed (Ir) per
ROM with `perf/bench-baseline.txt`. It exits non-zero if any ROM is more than
3% above its baseline; improvements are printed but pass. Ir is deterministic
(two runs gave identical counts), so the threshold covers real code change,
not noise.

What it measures: only the emulation loop. `bench` calls `run_emulation`
(`#[inline(never)]`) and callgrind is told `--toggle-collect='*run_emulation*'`,
so process startup, ROM loading and setup are not counted. The ROMs are
hash-checked against `acceptance/test-roms.sha256` first.

ROMs, each run for 20 emulated seconds (83,886,080 clocks): `blargg/cpu_instrs/cpu_instrs.gb`
(CPU), `blargg/cgb_sound/cgb_sound.gb` (APU), `cgb-acid2/cgb-acid2.gbc` (PPU).

Current numbers (Ir) and wall time (valgrind run of all three, ~22-24 s):

| ROM | Ir |
|---|---|
| cpu_instrs.gb | 1,726,409,705 |
| cgb_sound.gb | 971,629,157 |
| cgb-acid2.gbc | 680,542,754 |

(The scanline renderer took these from 1,464.1 M, 709.3 M and 341.7 M;
ADR 0014, PPU mode timing, before it from 1,388.0 M, 607.5 M and 12.9 M;
see the table above.)

(ADR 0011 step 4, the lazy timer, took these from 1,921.3 M, 922.7 M and
928.5 M: -28.0%, -32.6% and -24.9%. ADR 0011 step 5, HALT/STOP idling
straight to the next deadline, took cgb-acid2 from 697.2 M to 13.1 M
(-98.1%: it spends most of its time halted waiting for VBlank);
cpu_instrs -0.02%, cgb_sound unchanged.)

Run: `scripts/check-bench.sh`. Update the baseline:
`scripts/check-bench.sh --update-baseline`.

Rule: a baseline change is committed together with the change that caused it,
with the measured delta per ROM and the reason in the commit body. Changing the
ROM list or durations also needs a baseline update in the same commit.

## Behavior fingerprint

`bench --fingerprint [--seconds N] [--root DIR]` (N defaults to 3 and must be
at least 1, root to `test-roms`) prints one line per run, `<16-hex FNV-1a
hash> <path>`, sorted by path, so two builds can be compared bit for bit (ADR
0011 "Verification"). Corpus: every `.gb`/`.gbc` under Mooneye `acceptance`,
`misc` and `emulator-only`, `blargg`, `age-test-roms`, `same-suite`,
`cgb-acid2`, `cgb-acid-hell`, `dmg-acid2`, `mealybug-tearoom-tests`,
`rtc3test` and `mbc3-tester` (334 ROMs, each run twice: plain and
`<path>#input`), plus the 3,154 gambatte `*cgb04c*` ROMs run once for N/8
seconds: 3,822 runs, about 8 s wall on 8 threads at N=3. Fewer than 3,800
runs (a missing or partial `test-roms/`) is an error; a ROM that fails to
load prints `LOAD-ERROR <path>`. Each ROM runs on the only supported model
(CGB C, default palettes) in 456-clock `run_until` chunks with `LD_B_B` and
`SERIAL_TX` stops on. After each chunk the hash folds the clock, the chunk's
clocks and events, the serial byte (on `SERIAL_TX`), the CPU registers and,
via `peek`, all of FF00-FF7F (joypad, serial, timer, IF, APU with wave RAM,
PPU and CGB registers) and IE; on `FRAME_READY` the frame buffer. At the end
it folds VRAM 8000-9FFF, WRAM C000-DFFF, OAM with FEA0-FEFF and HRAM (all via
`peek`, so only the mapped VRAM and WRAM banks), the whole cartridge RAM and
the RTC state. Not covered, because the public API has no side-effect-free
access: the unmapped VRAM bank and WRAM banks, and CGB palette RAM. The
`#input` runs schedule a fixed press/release sequence with `set_buttons_at`
(one change every 1/8 s, at clocks inside chunks), so the joypad and
scheduled-input paths are covered. Runs are parallel; output order does not
depend on it.

`scripts/diff-fingerprint.sh <rev> [args]` builds `<rev>` in a temporary
worktree and the working tree, runs both fingerprints with the same args,
lists the differing ROMs and exits 0 only if none differ. It warns loudly when
the working tree has uncommitted changes under `crates/`, `tools/` or
`Cargo.*` (the new side is the working tree, not HEAD), when the two sides
have different run counts, and when ROMs fail to load. Use it on commits
that restructure core timing, which must not change behavior.
