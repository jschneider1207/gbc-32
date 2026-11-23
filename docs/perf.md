# Performance

ROMs, each run for 20 emulated seconds (83,886,080 clocks): `blargg/cpu_instrs/cpu_instrs.gb`
(CPU), `blargg/cgb_sound/cgb_sound.gb` (APU), `cgb-acid2/cgb-acid2.gbc` (PPU).
The test ROMs are read from `$TEST_ROMS` (default `test-roms/`).

Five synthetic ROMs cover paths those miss. `tools/bench` builds them in
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
  Since the M5 perf review it also keeps a CGB HBlank HDMA running from
  the main loop (128 blocks ROM 0000 -> VRAM 8800, restarted when it ends:
  one 16-byte block per visible line, 17,150 blocks in 2 s), the per-line
  hot path no other bench reached. The HDMA stalls replace polling-loop
  instructions, so the total fell: 442.70 -> 429.69 M (-2.9%, same core),
  while `vram_dma` costs 20.8 M inclusive (1,215 Ir per block, 174k per
  frame; self 610 Ir per block, about 38 per byte).
  Block copies (`vdma_block`, `Cart::dma_block`): both counters are
  16-aligned, so a block is one 16-byte run of its region and is copied
  whole from the ROM window, the RAM bank or the WRAM bank instead of 16
  `read` dispatches; VRAM is indexed as 16-byte chunks with a masked index
  (no bounds checks), and the speed factor is a shift (no divide-by-zero
  panics in `vram_dma`, `hdma::stall_mcycles` or `mcycles_to_deadline`).
  worst-case 429.69 -> 423.14 M (-1.5%, 3.54 M per frame, 88% of the
  line), `vram_dma` 1,215 -> 833 Ir per block inclusive (174k -> 120k per
  frame), self with `dma_block` 610 -> 219; halt-irq -0.3% (the shift),
  cgb-acid2 -0.1%. `vram_dma` 708 -> 664 B, `dma_block` + `block_tail`
  +240 B: total `.text` 84,942 -> 85,068 B (budget 85,100). Left:
  `vdma_arm` (~30 Ir per call, 3.4 calls per line: 1.76 M) and the run
  loop's per-block `Cpu::run` exit.
  callgrind: `Ctx::boundary` 15.9%, `Renderer::render` 9.1% (inclusive,
  with `fill_mixed`; 9.7% before the fused object mixing), timer 7.0%,
  serial 5.1%.
  Since M6 task 8 all four sound channels also play, at their worst
  periods for `Apu::sync`: APU power-cycled, NR50 = 77, NR51 = FF; CH1
  and CH2 at f = 2047, duty 50%, envelope 15 down pace 7 (steps for
  1.64 s), CH1's sweep running (pace 1, negate, shift 0: a calculation
  every 1/128 s that leaves f at 2047); CH3 at f = 2047, 100% volume,
  wave RAM 30 31 .. 3F; CH4 at r = 0, s = 0, 7-bit, envelope as the
  squares; lengths off. `synth:worst-case@<hex digit>` builds the same
  program with only the channels in that mask (bit n = CHn+1) set up; @0
  is the program without any APU setup (the post-boot CH1 at f = 0 stays
  on), for measuring the APU's cost. Not in the check-bench list.
- `synth:cgb-raster` (2 s): a CGB palette raster ("hi-color") effect in
  double speed. BG only (LCDC=91), one tile with all four colors, map
  attributes selecting palette (column & 7) so all 8 BG palettes are on
  screen. Every line: wait for mode 0, 32 BCPD writes with
  auto-increment (3 M-cycles apart, so the last ~10 land in mode 3 of
  the next line), wait for mode 3, then BCPD, SCX, BCPD mid-line. Per
  emulated second: about 291k BCPD writes (34 per line, 4,880 per
  frame), 8.6k SCX writes. When added: 311,983,738 Ir = 2.61 M
  Ir per frame; the BCPD path (`write_io` minus `write_ppu`, inclusive)
  is 178 Ir per write: `Ppu::sync` 55, `flush` 35 (51k of the writes
  draw: 304 Ir each), the store, palette resolution and pair-table
  rebuild ~88.
  Cheaper BCPD writes (2446bc2: resolve only the written color and patch
  its 7 pair entries, `set_pair_color`; skip the flush unless the synced
  PPU is in mode 3, `Ppu::drawing`): 293,807,294 Ir (-5.8%), 2.46 M Ir
  per frame, 147 Ir per write. What is left is mostly the sync to the
  write and `write_io` itself; the 34k mid-mode-3 writes per 2 s each
  draw a span (~370 Ir).

**Audio buffers (M6 task 8).** `run_emulation` calls the core the way the
MCU front end will: at most one frame per call (`run_until(min(target,
clock + FRAME_CLOCKS))`), each with a 1,024-slot audio buffer whose
samples are consumed (`black_box`) after the call. A frame is 548.6
samples, so the buffer never fills and `AUDIO_BUFFER_FULL` never splits a
call. Every bench entry runs this way (before task 8 they passed an empty
buffer: no samples, the APU only advanced).

**APU cost (M6 task 8, x86 Ir per emulated frame).** `synth:worst-case`
(2 s, 119.455 frames), all with buffers; channel cost = Ir over `@0`
(buffers, no APU setup), from `scripts/check-bench.sh`-style callgrind
runs of each `@<mask>` variant:

| Variant | 9b0b8cc (stepping) | 29cacd2 (closed form) | 4dd75a2 (batches) | 0fc52a8 (8b) |
|---|---|---|---|---|
| `@1` CH1 (sweep) | 73.0k | 32.4k | 23.7k | (unchanged code) |
| `@2` CH2 | 73.1k | 32.4k | 23.6k | (unchanged code) |
| `@4` CH3 (wave) | 409.6k | 81.3k | 44.3k | (unchanged code) |
| `@8` CH4 (noise, 7-bit) | 84.4k | 83.1k | 70.9k | 27.8k |
| all four | 682.6k | 276.0k | 214.5k | 171.3k |
| `Apu::sync` on `@0` | | | 55.8k | 52.4k |
| APU (`Ctx::apu_sync` incl.) | | | 270.6k | 224.1k |
| worst-case, M Ir/frame | 4.312 | 3.907 | 3.834 | 3.780 |

All exact: `bench --fingerprint --audio-buffer {7,64} --hash-samples`
(samples hashed, synthetic programs included) is identical to d984a8a
at every step. What changed:

- **Closed form per window** (29cacd2). After the first step in a
  window, a square's whole periods are a run of duty bits (the duty byte
  rotated to the first position, whole patterns by its popcount) and the
  wave channel's a run of wave RAM nibbles: two prefix sums over four
  32-bit SWAR words of the NR32-shifted wave RAM plus the cached sum. No
  stepping loop. 64-bit words made `Wave::integrate` 1,524 B on
  thumbv7em (64-bit shifts); 32-bit words, 614 B. No new state.
- **Batches** (4dd75a2). `Apu::sync` cuts a span into batches of up to 16
  window spans (256 B of level sums on the stack) and calls each channel
  once per batch, so the period, duty, SWAR words and prefix bases are
  set up once per batch (sync spans average ~57 windows on worst-case);
  NR50/NR51 become per-side channel weights once per sync. The batch
  loops read the span length through `core::hint::black_box`: LLVM's
  Cortex-M runtime unrolling (x4) otherwise doubles them on thumbv7em
  (`Square::integrate` 836 -> 410 B, `Wave::integrate` 1,304 -> 642 B).

Sample output with every channel silent: `Apu::sync` inclusive on `@0`
is ~56k Ir/frame (6.67 M per 2 s, ~100 Ir per window; it was ~66k
measured as buffers vs empty buffers). So the APU on worst-case is
~270k Ir/frame: **still over the 200k budget** (ADR 0021), though
worst-case is at 3.83 M Ir/frame, **95% of the 4.02 M line** (107%
before). Noise is now the largest channel (70.9k: 6 LFSR steps per word
operation in 7-bit mode, 3 popcounts per window; plan 8b has exact
ideas). thumbv7em `.text` 93,462 -> 94,120 B (`Apu::sync` 462 -> 832 B).

**Task 8b** (d236d92, 0fc52a8; both sample-identical to 281186f by the
same fingerprints at buffers 7 and 64):

- **Noise as its output sequence** (d236d92). `Noise::integrate` runs a
  batch on the outputs s(m) instead of the register: s(m + n) =
  !(s(m) ^ s(m + 1)) (n = 15, or 7 in 7-bit mode), applied to itself
  s(m + 2n) = !(s(m) ^ s(m + 2)) and s(m + 4n) = !(s(m) ^ s(m + 4)), so
  one word operation yields 24 outputs (7-bit, lag 28) or 28 (15-bit,
  lag 30), at most one per window. A window's high count is two lookups
  in a 256-byte popcount table (`count_ones` has no instruction on
  thumbv7em or baseline x86-64: a ~12-instruction SWAR sequence). The
  register is rebuilt once per batch (7-bit: bits 7-14 are the last 8
  feedback bits, with a shifted correction for the first 8 steps). CH4
  70.9k -> 27.8k. Of the plan's ideas, (a) k-bit steps of the register
  were what 4dd75a2 had (6 per operation in 7-bit mode); (b) the
  doubling identity is what made the difference. Exactness tests: runs
  of 0-16 steps against single steps (random registers, both modes,
  register after each run) and random batches against clock by clock
  (both modes, the shift-14/15 freeze); mutation-checked.
- **Mixer bookkeeping** (0fc52a8). Batch spans in closed form (all
  `SAMPLE_PERIOD` but the first and last), only a batch's last span can
  leave a window open, sums and counters in locals, `sample` as one
  shift. `Apu::sync` on `@0` 55.8k -> 52.4k. Skipping constant channels
  was not done: worst-case has all four playing, so it cannot help the
  budget, and a per-channel loop costs more than the four fixed
  multiplies.

The APU on worst-case (`Ctx::apu_sync` inclusive, measured directly) is
now **224.1k Ir/frame**, within the 230k budget (human decision
2026-11-22, after the 200k estimate was missed; ADR 0021 amendment). Per window (65,536
per 2 s): CH3 ~114 Ir, CH4 ~83, CH1/CH2 ~76 each, the mixer ~59.
worst-case 3.78 M Ir/frame, 94.0% of the 4.02 M line. thumbv7em
`.text` 94,120 -> 94,492 B (`Noise::integrate` 1,094 B; `Apu::sync`
832 -> 798 B; plus the 256 B table in `.rodata`).

Call counts from `callgrind_annotate`-style totals of the `calls=` lines in
the callgrind output of a 1 s run.

Current numbers (Ir) and wall time (valgrind run of all eight, ~35 s):

| ROM | Ir |
|---|---|
| cpu_instrs.gb | 1,664,998,305 |
| cgb_sound.gb | 1,117,954,081 |
| cgb-acid2.gbc | 654,553,744 |
| synth:event-storm (2 s) | 400,369,260 |
| synth:dma-storm (3 s) | 435,395,674 |
| synth:halt-irq (10 s) | 279,203,312 |
| synth:worst-case (2 s) | 451,592,462 |
| synth:cgb-raster (2 s) | 317,452,483 |

After M6 task 8b (perf/bench-baseline.txt: noise output sequence, mixer
bookkeeping). worst-case is 3.78 M Ir per emulated frame (119.455 frames
in 2 s), 94.0% of the 4.02 M line. After 8a: 1,669.1 / 1,120.8 / 658.6 /
400.8 / 436.0 / 281.2 / 457.2 / 317.9 M; worst-case 3.83 M, 95%. At M6 task 8 (9b0b8cc: buffers, four
channels, stepping): 1,685.5 / 1,124.8 / 671.4 / 402.1 / 437.9 / 287.6 /
514.4 / 319.1 M; worst-case 4.31 M Ir per emulated frame (119.455 frames
in 2 s), 107% of the 4.02 M (50% of M7) line; cgb-raster 2.67 M. Before
task 8 (empty buffers, M6 channels in the core): 1,623.2 / 1,011.0 /
592.4 / 394.1 / 426.1 / 248.1 / 424.9 / 311.2 M.

**Combined worst case.** `synth:worst-case` is 505.4 M Ir over 2 s =
119.455 frames, so 4.23 M Ir per emulated frame: 105% of the 4.02 M
(50% of M7) line, 0.21 M over it. The earlier sum estimate (event-storm
3.73 M + acid2 PPU 0.53 M = 3.97 M, 99%) was 0.26 M too low: the real
program also pays STAT IRQ entry (up to 3 per line) and the 10-object
lines. x86 proxy only. Fused object mixing (below) took it to 491.6 M =
4.12 M per frame, 102% of the line (0.10 M over). The PPU timing cuts
(below) took it to 482.1 M = 4.04 M per frame: 100.4%, 0.016 M over.

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

**PPU timing cuts (ADR 0014 "Further cuts").** The walk over update
points keeps a cursor (`Ppu::cursor`: the next point after the last sync,
with its line) and advances it point by point (`Ppu::step`, no
division); the division-based search (`next_point`) only re-seeks after
LCD enable, reset or a STAT write that changes whether any source is
selected (`Ppu::seek`, cold). Sync no longer searches once more to find
that it is done. Also: `Latch` and `End` skip the STAT-line evaluation
(they never change it); `Ppu::latch` is out of line (inlined, LLVM
hoisted its register loads into every sync, ~32 instructions per call);
the objects are sorted by X once per line, for both the mode-3 stalls and
the renderer's priority order (was sorted twice). Ir (20 s / 2 s):

| Step | cgb-acid2 | synth:worst-case | event-storm |
|---|---|---|---|
| Before (0c23c48) | 680,542,754 | 491,559,542 | 446,532,249 |
| Cursor | 632,998,283 | 485,961,638 | 444,532,470 |
| + `latch` out of line, no STAT evaluation at `Latch`/`End` | | 482,779,272 | |
| + one X sort per line (chosen) | 599,044,146 | 482,065,134 | 443,445,759 |

cgb-acid2 -12.0% (~68k Ir per frame), worst-case -1.9% (4.12 -> 4.04 M
per frame), cpu_instrs -1.2%, cgb_sound -2.2%, halt-irq -3.6%, dma-storm
-1.3%, event-storm -0.7%. The cursor is 8 bytes: `Hw` 992 -> 1,000 B.

Reusing the look-ahead walk did not pay and is not done: `next_event`
recorded the STAT line before its deadline, and a sync reaching that
deadline skipped the evaluation at the points before it. worst-case
482.8 -> 484.2 M (+0.3%): with the cursor a re-walked point costs about
40 Ir, the bookkeeping on every sync and walk cost more, and it needed 16
more bytes of `Hw`. `#[inline(always)]` on `point_irq`: -0.1 M for +800 B
of `next_event`; not done.

Remaining worst-case cost per visible line (callgrind, 2 s): two PPU
events one dot apart (the `End` draw, then the HBlank STAT edge), each a
scheduler dispatch + `sync_ppu` + `sync` + `next_event` (sync 5.5 M,
next_event 2.8 M, `point_irq` 1.6 M, `sync_ppu` 2.0 M self); `latch` 11.6 M
inclusive (~680 Ir per line: OAM scan, sort, stalls, renderer line
start); `render` + `fill_mixed` ~44 M. Drawing at the HBlank event instead
of a separate `End` event would change behavior: CPU VRAM writes do not
sync the PPU (`Ctx::write_vram`).

**Pixel-pair lookups.** Whole BG/window tiles with no object pixel look
up two pixels at a time in a 16-entry table of RGB555 pairs
(`pair_table`: entry 4a + b = color a | color b << 16), built on the
stack per span from BG palette 0 (64 B; not in `Hw`, which has 24 B
left). The two u16 stores merge into one u32 store on little-endian
targets. The tile loop is split into a partial head tile, whole tiles in
a fixed-trip loop (no per-tile shift, min or length test; LCDC.4
addressing hoisted, `tile_data`) and a partial tail (`fill_partial`, out
of line). The plain copy is out of line too (`fill_plain`), so `render`
holds neither copy. Ir (20 s / 2 s / 20 s), from d7a0481:

| Step | cgb-acid2 | synth:worst-case | cpu_instrs | `render` + `fill_*` (B) |
|---|---|---|---|---|
| Before | 599,044,146 | 482,065,134 | 1,705,666,917 | 2,216 + 792 (last measured) |
| Pair table only | 554,452,585 | 477,096,013 | 1,637,376,130 | 2,364 + 736 |
| + whole-tile loop, inlined | 508,396,952 | 471,299,421 | 1,590,190,248 | 4,218 + 1,820 |
| + plain copy and partial tiles out of line (chosen) | 514,287,199 | 472,244,651 | 1,598,254,766 | 1,680 + 1,000 + 1,104 + 548 |

cgb-acid2 -14.1% (~71k Ir per frame), worst-case -2.0% (4.04 -> 3.95 M
per frame, 98.3% of the line), cpu_instrs -6.3%, cgb_sound -11.3%,
halt-irq -19.2%, dma-storm -3.6%, event-storm -2.4%. `diff-fingerprint`:
identical on 3,822 ROMs. Code: +1.3 KB over the four functions.

CGB palette colors (M4): the 8 BG pair tables (512 B) are kept in the
caller's `Memory` (`bg_pairs`, next to palette RAM) and rebuilt per
palette when it changes: a BCPD write (CGB mode; the whole palette's 16
entries, not only the 7 holding the color), a BGP write (palette 0, DMG
compatibility mode), reset and `set_dmg_palettes`. The renderer no longer
builds a table per span; it uses `pairs[0]` until M5's BG attributes pick
`pairs[attribute palette]` per tile. Ir vs the b799d82 baseline (20 s / 2 s):
cpu_instrs -0.05%, cgb_sound +0.02%, cgb-acid2 +0.29%, event-storm +0.33%,
dma-storm +0.02%, halt-irq -0.19%, worst-case +0.32% (436.8 M = 3.66 M
per frame, 91% of the 4.02 M line). `Renderer::render` 1,680 -> 1,512 B.

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

**Event dispatch cuts.** Every dispatch takes the due entry and refolds
the event-table minimum, about 1.8 M times in `synth:worst-case` (2 s;
1.29 M `boundary` calls, 1.42 events each: Timer + Serial often share a
clock). The kinds after `Ppu` fire rarely, so their minimum is cached
(`Schedule::slow`, +8 B: `Hw` 1,000 -> 1,008 B) and the per-event fold is
over 4 values, not 7. `take_first_due` is inlined into `boundary` (the
caller has checked `next`, so the loop does one compare per event, not
two); `boundary` reschedules from the `now` it has; the Serial handler
uses `set_inline`. MCU code: `boundary` 1,792 -> 2,112 B (budget 1,870
-> 2,220) and the 292 B `take_first_due` symbol is gone: +28 B net.
Ir at 68c5d32 -> after (2 s / 20 s):

| Step | synth:worst-case | event-storm |
|---|---|---|
| Before | 472,244,651 | 432,681,063 |
| All (chosen) | 435,415,183 | 394,765,192 |

Measured one at a time on e15d85f (worst-case 482.07 M): hot/slow split
-10.7 M, the `boundary` cuts -13.3 M, inlining `take_first_due` -14.2 M
(keeping only the slow-kind path out of line: -10.0 M at 1,976 B, so the
whole function is inlined). worst-case -7.8% (3.95 -> 3.65 M Ir per
frame, 90.7% of the 4.02 M line), event-storm -8.8%, halt-irq -1.8%,
cgb-acid2 -0.7%, the others under 0.3%. Remaining per event: the
4-value fold, the handler's own work (Timer reload ~33 Ir, Serial shift
and next deadline ~34 Ir), and ~22 Ir of `boundary` prologue/epilogue
per call.

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
| `Ppu::sync` (cursor walk; 2,758 B with `latch` inlined) | 914 | 960 |
| `Ppu::next_event` (`step` inlined; was 482 B calling `next_point`) | 562 | 590 |
| `Ppu::seek` (cold: the division-based search, was `next_point` 412 B) | 576 | 605 |
| `Ppu::point_irq` | 494 | 520 |
| `Ctx::write_ppu` (PPU registers, out of `write_io`; mid-line flush) | 1,226 | 1,290 |
| `Renderer::render` (pixel loop) | 2,216 | 2,330 |
| `render::fill_mixed` (tile loop with object mixing) | 792 | 830 |
| `Ppu::latch` (once per line: OAM scan, X sort, stalls; `Renderer::start_line`, 2,208 B out of line before, inlined) | 4,368 | 4,590 |
| `LineObjects::scan` (OAM scan) | 482 | 510 |
| `Ppu::draw`, `Ppu::flush` | 146, 290 | 155, 305 |
| `Ctx::write_mapped` | 460 | 480 |
| `bus::read_vram`, `Ctx::write_vram` | 218, 216 | 230, 230 |
| `bus::read_oam`, `Ctx::write_oam` | 280, 304 | 300, 320 |
| `Ppu::vram_blocked`, `Ppu::oam_blocked` | 196, 240 | 210, 260 |
| `Ctx::write_timer_io` (cold) | 892 | 1,620 |
| `Timer::read` (cold) | 362 | 380 |
| All of gbc-core (`(total .text)`: every `.text*` section in the rlib, measured at 2446bc2) | 83,408 | 85,100 |

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

Current numbers (Ir) at 0dec57e (the same runs as the table at the top):

| ROM | Ir |
|---|---|
| cpu_instrs.gb | 1,595,152,445 |
| cgb_sound.gb | 840,971,582 |
| cgb-acid2.gbc | 512,284,188 |

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

`bench --fingerprint [--seconds N] [--root DIR] [--audio-buffer N [--hash-samples]]` (N defaults to 3 and must be
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

`--audio-buffer N` passes an N-sample audio buffer to every call instead of
an empty one. `AUDIO_BUFFER_FULL` then ends calls early; each 456-clock chunk
is finished with more calls to the same target and hashed as one (summed
clocks, events without `AUDIO_BUFFER_FULL`), and samples are not hashed, so
the output must be identical to the run without it: the APU sync invariant
(ADR 0021 section 3), checked over the corpus (plan M6 task 7). A small N
(e.g. 3) splits most chunks mid-way, the strictest setting. Compare the two
outputs with `diff`; it is a heavy step, so run it alone.

`--hash-samples` (with `--audio-buffer`) also folds every sample the calls
wrote into the hash and adds the synthetic programs (`synth:<name>` for
every bench program, plus `worst-case@1/2/4/8`, all for N seconds), so an
exact APU optimization can be shown sample-identical: run it before and
after, at two buffer sizes (e.g. 7 and 64; the hashes do not depend on the
split), and `diff`. 3,831 runs, about 11 s wall. Without the flag the
output is unchanged.

`scripts/diff-fingerprint.sh <rev> [args]` builds `<rev>` in a temporary
worktree and the working tree, runs both fingerprints with the same args,
lists the differing ROMs and exits 0 only if none differ. It warns loudly when
the working tree has uncommitted changes under `crates/`, `tools/` or
`Cargo.*` (the new side is the working tree, not HEAD), when the two sides
have different run counts, and when ROMs fail to load. Use it on commits
that restructure core timing, which must not change behavior.
