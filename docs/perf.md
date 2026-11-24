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

**APU cost (M6 task 8, x86 Ir per emulated frame).** Final state: the APU
on `synth:worst-case` is **224.1k Ir/frame** (`Ctx::apu_sync` inclusive),
**over** ADR 0021's original 200k budget/estimate. The human accepted it and
set the APU budget to 230k (2026-11-22; ADR 0021 last amendment). It stays
a recorded optimization target if the M7 measurement needs the room (plan
M8 task "APU cost is over its original estimate"). worst-case was 3.78 M
Ir/frame, 94.0% of the 4.02 M line, then; with NR50 writes per line it is
3.908 M, 97.2% (per-sync fixed cost, below); thumbv7em `.text` 94,754 of 96,000 B.
**Split APU budget (human, 2026-11-22, after the M6 perf review):** (a) channels
and mixing, measured on a worst case without APU register writes: 230k
Ir/frame (224.1k at 0fc52a8); (b) APU register writes: <= 950 x86 Ir per
write (884.6 at 117461b, `synth:apu-writes` minus its `@hram` control).
The overall bar, `synth:worst-case` <= 4.02 M Ir/frame (now with ~146
NR50 writes/frame, APU share ~353k), stays the real guard.

The rest of this section is how it got there (the table's earlier columns
and the 270k statement are history). `synth:worst-case`
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

**Per-sync fixed cost (M6 perf review).** Every APU register write syncs
the APU, so a game writing the APU at PCM rate pays a sync's fixed cost
(prologue, NR50/NR51 weights, the lane clear, each channel's setup:
square period and duty, the wave channel's SWAR words and prefix bases,
the noise `Run::new` fills) once per write, on top of the windows.
`synth:apu-writes` measures it: 8 kHz PCM through NR50 (a timer IRQ every
512 clocks, 137.2 writes a frame) with all four channels at their worst
periods; `synth:apu-writes@hram` is the same program writing FF80 (the
control, not in the bench list). Ir per APU write = the difference over
the 16,384 writes in 2 s: **894.2** at b78ed12, of which 811 in
`Ctx::apu_sync` (per write: noise ~195, both squares ~205, wave ~180,
`Apu::sync` itself ~195; the control spends ~414 per window, so about
half of a write's cost is the extra span a write splits off and half is
fixed setup). Joined to worst-case (b78ed12): its STAT handler (one IRQ
per line, ~146 a frame) also toggles NR50 for masks 1-F, so the 4.02 M
bar covers PCM-rate writes. This only makes the check stricter, and PCM
playback (voice samples) is a pattern real games use; worst-case went
from 3.780 to 3.911 M Ir/frame (97.3% of 4.02 M). On the M7 the review
estimated 450-550 cycles a sync, i.e. ~70k cycles/frame at this rate.

Mitigation, exact (`--fingerprint` plain and `--audio-buffer {7,64}
--hash-samples` identical to b78ed12): the per-batch level lanes are
`i16` (a lane is one channel's level times clocks over one span, at most
1,920), so the clear every sync pays is 128 B instead of 256 B (half of
`__aeabi_memclr4` on thumbv7em). x86 Ir per write 894.2 -> 884.6
(x86's vector memset hides most of it); worst-case 3.908 M Ir/frame,
apu-writes 713.0k; `.text` 94,808 -> 94,754 B. Not done: removing the
clear altogether needs the lanes in `Apu` (store semantics; the `Apu`
size test caps it at 128 B) or uninitialized memory (`unsafe`); the
noise and wave setup are already one pass per batch and the noise
channel steps in every span at r = 0, so laziness would not help the
worst case. Caching the weights would not help PCM through NR50 (every
write changes them).

**NR50/NR51 writes without a channel sync (M10 B12, 2026-11-24).** A
mixer write changes no channel state, so it no longer syncs the
channels: `Apu::sync_write` lists the spans up to the write (`mix::Spans`,
at most 16, cut at window ends and mixer writes) and notes the new
weights; the channels integrate the list in one call each when it is
full or when anything else syncs, and `Apu::flush` mixes it in runs of
constant weights (ADR 0021, amendment of 2026-11-24). The per-channel
setup is then paid once per 16 spans instead of once per write. Exact
(`--audio-buffer {7,64} --hash-samples` fingerprints identical). x86 Ir
per APU write (`synth:apu-writes` minus `@hram`, over 16,384 writes):
917.2 at 4564c6c, 748.9 with this and the plain integrator loops; this
change alone: apu-writes 85.37 M -> 83.17 M (-2.6%), worst-case 370.96 M
-> 368.21 M (-0.74%), cgb_sound +0.13% (a channel write still syncs
everything, now through the list), `.text` +1,344 B. What a mixer write
still costs is the span it cuts (about 290 Ir in the four integrators at
the worst periods, plus its mix) and about 240 Ir of listing and
dispatch.

History (state at 4dd75a2, superseded by task 8b below): sample output with every channel silent: `Apu::sync` inclusive on `@0`
is ~56k Ir/frame (6.67 M per 2 s, ~100 Ir per window; it was ~66k
measured as buffers vs empty buffers). So the APU on worst-case is
~270k Ir/frame then, over the 200k budget (ADR 0021); worst-case was
3.83 M Ir/frame, 95% of the 4.02 M line (107% before). Noise is now the largest channel (70.9k: 6 LFSR steps per word
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
now **224.1k Ir/frame**: still over the original 200k estimate, accepted
under the human's 230k budget (2026-11-22; ADR 0021 amendment), not a
met target. Per window (65,536
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

**M8 baseline (2026-11-23).** Ir (perf/bench-baseline.txt): cpu_instrs
1,668,850,426; cgb_sound 1,122,285,440; cgb-acid2 660,041,342; event-storm
(2 s) 397,507,065; dma-storm (3 s) 439,306,467; halt-irq (10 s)
280,584,307; worst-case (2 s) 476,467,536; cgb-raster (2 s) 321,950,601;
apu-writes (2 s) 85,482,907. `synth:worst-case` now also writes SCX once
per line in mode 3 (human decision, ADR 0010 amendment): 3.989 M Ir per
emulated frame (119.455 frames in 2 s), 99.2% of the 4.02 M line.

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

Rules as of 2026-11-24 (human: the caps are the ones valid for the final
MCU, the STM32H735IGK6, not for the dev board; the Stage 2 NUCLEO-H753ZI
may run slower as a result, it is a temporary step). On the H735 all of
gbc-core runs from a 128 KiB ITCM ("ITCM placement plan" below), so the
one size constraint is the total. No single function has a cap: per-function
caps kept bending the code (a fix chose a slower form to stay 1 byte under
`write_ppu`'s cap; `Apu::write` sat at 322 of 322 B) for a limit that the
final chip does not have.

| Check | Gates? | Where |
|---|---|---|
| Total `.text` of gbc-core <= 122,880 B (H735: 128 KiB ITCM minus an 8 KiB platform reserve; provisional) | yes | `scripts/check-code-size.sh`, the `122880 (total .text)` line of `perf/code-size-budgets.txt` |
| Ir per bench at most 3% over baseline | yes | `scripts/check-bench.sh` ("Instruction-count benchmark" below) |
| Every glob of the Stage 2 placement list matches a function | yes | `scripts/check-itcm-set.sh`, `perf/itcm-sections.txt` |
| ROM reads are byte loads (ADR 0027) | yes | `scripts/check-rom-loads.sh` |
| Per-function size (size, the old cap, % of it) | no, printed; a missing symbol is a `WARN` | `report` lines of `perf/code-size-budgets.txt` |
| H753 ITCM set fits 56 KiB; its callee closure | no, printed with a `WARN` line | `scripts/check-itcm-set.sh` |
| `size_of` `Emulator` (`Hw` plus a pointer) and `Memory` on thumbv7em | no, printed | `scripts/check-code-size.sh` |

There is no cap on `size_of::<Hw>()` any more (it was 864 B, asserted by
a host test on the x86-64 layout; ADR 0006 amendment of 2026-11-24).

Run: `scripts/check-code-size.sh` (about 1 s warm, a few seconds cold).
`perf/code-size-budgets.txt` has two kinds of line:
- `<bytes> (total .text)`, exactly one: **the gate**. Exit is non-zero if
  the total is over it, or if the line is missing, doubled or marked
  `report`. The figure comes from the final-chip arithmetic in "ITCM
  placement plan" below; raising it needs the human.
- `report <bytes> <symbol>` for every per-function line: prints the size,
  the figure (the cap the line had while it gated) and % of it, marks a
  size above it "over, report only", and prints a `WARN` for a symbol that
  is not found (renamed or inlined away: update the line). It never fails.
  A per-function line without `report` fails, so a cap cannot creep back
  in unnoticed. Growth shows in review; lower a figure when a function
  shrinks for good.

Measurement: `RUSTFLAGS="-C target-cpu=cortex-m7" cargo build -p gbc-core
--release --target thumbv7em-none-eabihf --target-dir target/codesize`
(release is pinned to `codegen-units = 1` in `Cargo.toml`, the build the
firmware will use; no LTO, there is no firmware crate to link yet), then
`nm -S --radix=d` (GNU binutils; `llvm-nm`/`rust-nm` preferred if present)
on the object in the rlib, with `-C` for the names in the budgets file.
`(total .text)` is the sum of every `.text*` section (`size -A`),
alignment padding included; the total without padding is not printed,
because counting the `nop`s needs an ARM disassembler and none is
installed (the M10 review measured 5.8 KB of padding with one). The struct
sizes come from a three-line probe crate, compiled by `rustc` against the
rlib, whose statics are `[u8; size_of::<T>()]`. The rlib total counts
functions the firmware's link may drop and none of the `core` or
`compiler_builtins` code the core calls; that is why the gate stays
provisional until the firmware ELF can be measured.

Sizes on 2026-11-24 (the script prints all of them): total `.text`
97,136 B, 79% of 122,880 (margin 25,744 B); `Emulator` 848 B, `Memory`
50,968 B. Largest per-function lines (report only):

| Symbol | Size (B) | Old cap (B) |
|---|---|---|
| `cpu::Cpu::run` | 24512 | 25738 |
| `cpu::Cpu::execute_cb` | 6892 | 7237 |
| `ppu::Ppu::latch` | 1710 | 1796 |
| `bus::Ctx::write_ppu` | 1520 | 1563 |
| `ppu::render::Renderer::render` | 1192 | 1252 |
| `apu::noise::Noise::integrate` | 1134 | 1191 |
| `bus::Ctx::boundary` | 1052 | 1183 |
| `ppu::render::fill_mixed` | 1030 | 1050 |
| `ppu::Ppu::sync` | 930 | 960 |
| `Emulator::load` | 876 | 890 |
| `bus::read_high` | 824 | 866 |
| `ppu::Ppu::set_stalls` | 824 | 866 |

Mutation-checked 2026-11-24 (scratch copies of the budgets file through
`CODE_SIZE_BUDGETS_FILE`): a total figure of 97,000 B (below the measured
97,136) exits 1 with `OVER BUDGET`, and 97,136 exits 0; `report 20000`
on `Cpu::run` exits 0 with "over, report only"; a misspelt symbol exits 0
with a `WARN`; a per-function line without `report` exits 1; `report` on
the total line exits 1.

History, M2 (the per-function caps of that time). OAM DMA (M2): every CPU
access ORs `Hot::dma` (FE00 during a transfer)
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
50,376 B. With the chosen layout `run` was back under the pre-DMA
30,920 B; its cap then became 30,000 B.

### Size pass, M9 (2026-11-23, branch `core-size`)

`.text` 97,068 -> 95,652 B (-1,416) with no behavior change
(diff-fingerprint vs b965ceb: 4,020 ROMs identical) and lower Ir: synth:worst-case
477,624,031 -> 476,567,819 (-0.22%), cgb-acid2 685,168,960 -> 684,330,998
(-0.12%):

| Change | B saved | Note |
|---|---|---|
| `set_stalls` writes stalls in place; window inserted by shifting; dots summed in `sorted_object_stalls`; `LineTiming::finish` shared with `set_stalls_retriggered` | 596 | A first version (`copy_within` + re-summing in `finish`) saved 606 B but cost +0.2% worst-case Ir (memmove call, sum loop) |
| `sorted_object_stalls`: no window = `i32::MAX` start, tile key one `i32` | 248 | The `Option` window made LLVM keep a loop copy per case; an `Option<(bool, i32)>` tile compare cost +0.45% cgb-acid2 Ir |
| `window_stall_at` `#[cold]` `#[inline(never)]` | 516 | Three cold callers each had a copy |
| `OamDma::ppu_read` `#[inline(never)]` | 56 | Cold callers only |

Tried and dropped: feeding the object X iterator straight into
`sorted_object_stalls` (+712 B, LLVM unrolled the mapped iterator).

Unused lever: about 5.8 KB of `.text` is `nop` alignment padding (2,919
`nop`s; 2.5 KB in `Cpu::run`, 1 KB in `execute_cb`): with
`target-cpu=cortex-m7` LLVM aligns blocks after unconditional branches
to 8 bytes. Removing it is a build-flag choice (an LLVM alignment
option) whose cost on the real M7 fetch path callgrind can't show; it
needs an MCU measurement and the human.

## ITCM placement plan

Two plans, one per MCU. The final chip's plan is the one the gates enforce
(human, 2026-11-24: the caps are the final MCU's; the dev board may run
slower, it is a temporary step). ADR 0026 (Proposed, amended 2026-11-24)
has the decision and its reasons.

### Final chip: STM32H735IGK6 (Stage 3+)

**Memory facts** (RM0468 Rev 3, DS13312 Rev 4 and ES0491 Rev 9, all in
`../gbc-32-hardware/datasheets/`):
- RAM (RM0468 §2.4 "Embedded SRAM"; DS13312 §3.3.2): 128 KiB AXI SRAM,
  64 KiB ITCM, 128 KiB DTCM, plus **192 KiB that is either ITCM or AXI
  SRAM, in 64 KiB steps**; SRAM1 and SRAM2 (16 KiB each, D2), SRAM4
  (16 KiB, D3), 4 KiB backup SRAM. ITCM and DTCM are zero-wait; they are
  reachable by the CPU and by the MDMA (through the M7's AHBS), not by
  DMA1/2 (DS13312 §3.3.2).
- The split is the option byte `TCM_AXI_SHARED[1:0]` in
  `FLASH_OPTSR2_PRG` (RM0468 §4.9.25; the current value reads back in
  `FLASH_OPTSR2_CUR`, §4.9.24, and in `SYSCFG_UR17.TCM_AXI_SHARED_CFG`,
  §12.4.25). RM0468 §2.4 Table 8:

  | `TCM_AXI_SHARED` | ITCM | ITCM range | AXI SRAM | AXI range |
  |---|---|---|---|---|
  | 00 (default) | 64 KiB | 0x0000_0000-0x0000_FFFF | 320 KiB | 0x2400_0000-0x2404_FFFF |
  | 01 | 128 KiB | 0x0000_0000-0x0001_FFFF | 256 KiB | 0x2400_0000-0x2403_FFFF |
  | 10 | 192 KiB | 0x0000_0000-0x0002_FFFF | 192 KiB | 0x2400_0000-0x2402_FFFF |
  | 11 | 256 KiB | 0x0000_0000-0x0003_FFFF | 128 KiB | 0x2400_0000-0x2401_FFFF |

- ES0491 §2.3.1: with `TCM_AXI_SHARED` = 10, MDMA writes to the reserved
  0x0003_0000-0x0003_FFFF are dropped without a bus error. Only matters
  for the 192 KiB setting, and only for a bad address.
- L1 caches 32 KiB I + 32 KiB D (DS13312 §3.1). CPU 520 MHz at VOS0,
  550 MHz with `CPU_FREQ_BOOST` (DS13312 Table 13), which turns off the
  ITCM/DTCM ECC (RM0468 §2.4 "Error code correction"; `SYSCFG_UR18`,
  §12.4.26).

**What has to live where.** Numbers from `check-code-size.sh` on
2026-11-24 and the core's public types (ADR 0006):
- ITCM: all of gbc-core, 97,136 B of `.text` (rlib, padding included),
  plus a platform reserve of 8 KiB (8,192 B: the audio/display DMA and
  SysTick ISRs, the frame blit, the `compiler_builtins` the core calls
  (`__aeabi_memcpy*`, `__aeabi_memclr*`), linker veneers). The reserve is
  the same assumption as for the H753; nothing yet justifies another
  figure. Launcher, menu, eMMC and USB code runs from flash.
- DTCM (128 KiB, fixed): the stack, `Emulator` 848 B, `Memory` 50,968 B
  (WRAM, VRAM, OAM) and the tables `SPREAD`, `ATTR_FETCH`, `POP8`
  (2,304 B): 54,120 B plus the stack.
- AXI SRAM (DMA has to reach these, so not DTCM): two frames,
  2 x 46,080 B (`Frame` is `[u16; 160 * 144]`) = 92,160 B; the audio
  buffers, at most 8,192 B (32,768 Hz is 549 `StereoSample`s of 4 B per
  frame, 2,196 B; double-buffered 4,392 B, rounded up); cartridge RAM up to
  131,072 B (MBC5). Total 231,424 B (226 KiB).

**128 vs 192 KiB of ITCM:**

| ITCM | gbc-core share (ITCM - 8,192) | gbc-core now | AXI SRAM | AXI left after 231,424 B |
|---|---|---|---|---|
| 128 KiB (01) | 122,880 B | 97,136 B, 79%, margin 25,744 B | 262,144 B | 30,720 B |
| 192 KiB (10) | 188,416 B | 97,136 B, 52%, margin 91,280 B | 196,608 B | -34,816 B: does not fit |

**Recommendation: 128 KiB** (`TCM_AXI_SHARED` = 01). It already holds the
whole core with 21% to spare, and leaves 30 KiB of AXI SRAM (plus 48 KiB
of SRAM1/2/4) for the launcher, eMMC/FAT and USB buffers. At 192 KiB the
worst-case cartridge RAM and double-buffered frames no longer fit in AXI
SRAM (it would take a single frame buffer, or cart RAM moved elsewhere),
to buy ITCM the core does not need. Not covered by either: a full-panel
LTDC frame buffer (320 x 240 RGB565 is 153,600 B) does not fit in AXI
SRAM next to 128 KiB of cart RAM with any split; the display is still
undecided, and such a buffer would go in external RAM.

**The gate** (`scripts/check-code-size.sh`, the only size gate):
gbc-core total `.text` <= 131,072 - 8,192 = **122,880 B**. Provisional:
it is measured on the rlib (no LTO; it counts functions the link may drop
and none of the `core`/`compiler_builtins` code the core calls). When the
H735 firmware crate exists, measure the linked ELF's `.itcm` instead and
revisit the figure with the human.

**Consequences.** No hot set and no per-function caps on this chip: every
gbc-core function is zero-wait, and the 32 KiB I-cache serves only
platform code. The D-cache serves the ROM, which sits in HyperRAM on
OCTOSPI (human decision 2026-11-24; hardware ADR 0001 pending update).
Linker rule: every gbc-core `.text*` input section goes to `.itcm` (by
archive member, `*libgbc_core*.rlib:*(.text .text.*)`, or with LTO by the
`8gbc_core` path segment); the post-link check then requires every
gbc-core function in [0x0000_0000, 0x0002_0000) and reports the veneer
count.

**The 4.02 M Ir line** (the M8 bar for `synth:worst-case`, "Instruction-
count benchmark" and the APU budget above) was derived as 50% of an H753
frame at 480 MHz: 480 MHz / 59.73 fps / 2 = 4.02 M cycles. On the H735 at
520 MHz, 50% of a frame is 4.35 M cycles (4.60 M at 550 MHz). The 4.02 M
line stays as the stricter figure; no Ir gate changes.

### Dev board: NUCLEO-H753ZI (Stage 2 only)

This plan places part of the core in the H753's fixed 64 KiB ITCM. Since
2026-11-24 its fit and callee closure are reported, not gated
(`scripts/check-itcm-set.sh` prints a `WARN` line and exits 0); a glob
that matches nothing still fails, because the Stage 2 linker rules are
generated from the list. If the set outgrows the budget, the Stage 2 build
drops functions in the order given under "H753 placement" below and runs
somewhat slower; that is accepted.

Measured 2026-11-24 on the same build as `check-code-size.sh`
(thumbv7em-none-eabihf, release with `codegen-units = 1` as pinned in
`Cargo.toml`, `-C target-cpu=cortex-m7`, no LTO), section sizes from
`size -A` on the rlib object. Memory facts: RM0433 (H753) ITCM 64 KB
at 0x0000_0000 and DTCM 128 KB at 0x2000_0000, both zero-wait and not
cached; flash at 240 MHz AXI needs 4 wait states (5 flash clocks per
256-bit flash word, RM0433 Table 17), so an I-cache miss to flash costs
about 10+ CPU cycles at 480 MHz before bus latency. The H753 L1 I-cache is
16 KiB.

**Totals.** gbc-core `.text` 94,960 B (all `.text*` sections, 224
functions, 54 of them `#[cold]` in `.text.unlikely.*`, 15,696 B),
`.rodata` 3,865 B. `run` + `execute_cb` = 24,512 + 6,892 = 31,404 B.
(Since then `.text` is 97,136 B; the set is 55,072 B, 64 globs, margin
2,272 B. `check-itcm-set.sh` prints the current figures.)

**The ITCM set** (`perf/itcm-sections.txt`, 63 globs, 68 functions at the
time of the table), by tier:

| Tier | Contents | Size (B) |
|---|---|---|
| A: per instruction/access | `Cpu::run`, `execute_cb`, `step_not_running`; `read_high`, `read_vram`, `read_oam`, `write_mapped`, `write_high`, `write_io`, `write_vram`, `write_oam`, `sync_ppu`, `boundary`, `run_ppu_to_call`; `Ppu::cpu_blocked`; `Cart::{read_ram, read_rtc}`; `halt_begin/end`, `idle_halted`; `Schedule::set`, `Timer::schedule_overflow` | 36,834 |
| B: PPU, per event | `Ppu::{sync, next_event, point_irq, draw, flush, stat, ly, check_wy_at}`, `Ctx::write_ppu`, `cgb_color` | 4,722 |
| B: PPU, plain tile path | `Renderer::render`, `fill_plain`, `fill_partial::<false>`, `partial_tile::<false>` | 2,348 |
| B: PPU, per line with objects | `Ppu::{latch, set_stalls}`, `LineTiming::finish`, `Renderer::fetch_objects`, `fill_mixed`, `fill_partial::<true>`, `partial_tile::<true>` | 4,720 |
| C: APU, per sync | `Apu::{sync, step, write}`, the three `integrate`s and `advance`s, `frame_step` x2, `Sweep::clock`, `mix::constant`, `div_apu_event`, `set_apu_deadline`, `apu_sync`, `set_div_apu_deadline`, `start_audio`, `finish`, `audio_buffer_event`, `write_apu` | 5,624 |
| D: frame driver | `Emulator::{run, run_frame, run_until, run_cycles}` | 726 |

Total 54,974 B. Largest: `Cpu::run` 24,512; `execute_cb` 6,892;
`Ppu::latch` 1,710; `Ctx::write_ppu` 1,488; `Renderer::render` 1,192;
`Noise::integrate` 1,134; `Ctx::boundary` 1,126; `fill_mixed` 1,030;
`Ppu::sync` 930; `read_high` 824; `Ppu::set_stalls` 824.

**H753 placement.** Assumed ITCM reserve for the front end: 8 KiB (ISRs for
audio/LCD DMA and SysTick, the line/frame blit, the `compiler_builtins` the
set calls: `__aeabi_memcpy*`, `__aeabi_memclr*`, plus linker veneers). The vector table stays in flash
(VTOR). That leaves **56 KiB (57,344 B) for gbc-core**:

1. ITCM, 54,974 B (2,370 B margin): the whole set above.
2. Flash behind the I-cache, 9,236 B (56% of 16 KiB): the 26 warm
   functions the set calls directly, listed with a reason each as
   `FLASH_OK` lines in `perf/itcm-sections.txt`: I/O register reads and
   writes other than the per-access ones (`write_stat`, `write_lyc`,
   `write_lcdc`, `write_timer_io`, `Timer::read`, `read_apu`, ...), MBC
   register writes (`Cart::write_rom`), the OAM DMA and VRAM DMA paths
   (`read/write_during_dma`, `dma_sync`, `vdma_turn`), and
   `Ppu::point_irq_rare` (lines 0, 144 and 153).
3. Flash, cold: everything else, including every `#[cold]` function the
   set calls (24 of them, 8,928 B: `Ppu::scan_to`, `scan_line`,
   `hold_fetch`, `Ctx::oam_dma`, `Cpu::stop`, ...). `__aeabi_uldivmod`
   stays in flash too: its three callers (`noise::long_steps`,
   `apu::rem_long`, `Ctx::oam_dma_during_vdma`) are all `#[cold]`.

If the set (or the Stage 2 firmware build, with LTO) is larger than
56 KiB, take things out in reverse order: the per-line object path, then
the plain tile path, then `write_ppu`, then the APU set. Measure the
firmware ELF, not the rlib, because LTO changes inlining.

**DTCM** (128 KB, zero wait states, reachable by the CPU and MDMA only, not
DMA1/2): the stack, `Hw`/`Emulator` (848 B), `Memory` (50,968 B: WRAM,
VRAM, OAM) and the read-only tables `SPREAD` 1,024 B, `ATTR_FETCH` 1,024 B
and `POP8` 256 B (2,304 B, copied from flash at boot like `.itcm`). Frame
and audio buffers go in AXI SRAM, because the LCD and SAI DMA must reach
them. Cart RAM goes in AXI SRAM too (it can be up to 128 KB with MBC5).
`check-code-size.sh` prints both sizes for thumbv7em. The same split
applies on the H735 (above).

### Mechanism (both chips)

gbc-core cannot name its own sections. In edition 2024
`link_section` is an unsafe attribute (`#[unsafe(link_section = "...")]`),
and `#![forbid(unsafe_code)]` also rejects it. So placement happens only at
link time in the front end. rustc emits one section per function
(`.text.<v0-mangled symbol>`, e.g.
`.text._RINvMs0_NtCsdUUKRCLjrj6_8gbc_core3cpuNtB6_3Cpu3runNtNtB8_3bus3CtxEB8_`).
The crate hash (`CsdUUKRCLjrj6_`) changes with the compiler and the build,
so the patterns match on the length-prefixed path segments only. A sample
fragment for the Stage 2 front end's `memory.x` (cortex-m-rt; on the H735
the gbc-core lines become one rule for all of gbc-core and `ITCM` is
128K):

```ld
MEMORY {
  FLASH : ORIGIN = 0x08000000, LENGTH = 2M
  RAM   : ORIGIN = 0x20000000, LENGTH = 128K   /* DTCM */
  ITCM  : ORIGIN = 0x00000000, LENGTH = 64K
  AXI   : ORIGIN = 0x24000000, LENGTH = 512K
}
SECTIONS {
  .itcm : ALIGN(8) {
    __sitcm = .;
    *(.text.itcm.*)                                /* front-end ISRs, blit */
    *(.text.__aeabi_memcpy* .text.__aeabi_memclr*)
    /* gbc-core tier A (generated from perf/itcm-sections.txt) */
    *(.text._R*8gbc_core3cpu*3Cpu3run*)
    *(.text._R*8gbc_core3cpu*3Cpu10execute_cb*)
    *(.text._R*8gbc_core3bus9read_high*)
    *(.text._R*8gbc_core3bus*3Ctx12write_mapped*)
    *(.text._R*8gbc_core3apu*9integrate*)          /* 3 channels */
    *(.text._R*8gbc_core3ppu6render12fill_partial*Kb0_*)  /* ::<false> */
    /* ... one line per glob in the file ... */
    . = ALIGN(8);
    __eitcm = .;
  } > ITCM AT > FLASH
  __siitcm = LOADADDR(.itcm);

  .dtcm_rodata : ALIGN(4) {
    __sdtcm_ro = .;
    *(.rodata._R*8gbc_core3ppu6render6SPREAD*)
    *(.rodata._R*8gbc_core3ppu6render10ATTR_FETCH*)
    *(.rodata._R*8gbc_core3apu5noise4POP8*)
    . = ALIGN(4);
    __edtcm_ro = .;
  } > RAM AT > FLASH
  __sidtcm_ro = LOADADDR(.dtcm_rodata);
} INSERT BEFORE .text;
```

`INSERT BEFORE .text` puts these input-section rules ahead of
cortex-m-rt's `*(.text .text.*)`. Both GNU ld and lld assign an input
section to the first rule that matches. Check this in the Stage 2 map file
for the linker actually used. No `KEEP`: unused functions should still be
garbage-collected. The front end copies `.itcm` and `.dtcm_rodata` from
flash before `main` calls into the core (front-end `unsafe` with a
`// SAFETY:` comment, allowed for hardware access). Calls from ITCM to flash
(and back) are beyond Thumb `BL` range (±16 MB), so the linker inserts
veneers. That is fine on cold edges, and the post-link check counts them.

**Checks, so renames and inlining can't silently drop a function from ITCM.**
On the H735 the rule covers all of gbc-core, so only the total
(`check-code-size.sh`) and the post-link check apply. For the H753 list
there is one source of truth, `perf/itcm-sections.txt`, with
`<glob>  # <demangled name>` per line. The front end's `build.rs` generates
the `.itcm` rule list from it, so the script and the check cannot drift.
Then two checks:
- *Pre-link (rlib): `scripts/check-itcm-set.sh`, in `gate-stage1.sh`.*
  Hard failures (exit 1): a glob that matches no `.text.*` section of the
  rlib (MISSING: the function was renamed or inlined away, so fix the
  list; each glob is tested with the shell's `[[ == ]]` pattern, which has
  the same `*` semantics as the linker's), a `FLASH_OK` line without a
  reason, and a relocation parse that finds fewer than 50 calls. Reported
  with a `WARN` line, exit 0 (since 2026-11-24):
  - the matched total over `ITCM_BUDGET_BYTES` (56 KiB);
  - *callee closure*: a direct call or tail call from a function in the set
    (an `R_ARM_THM_CALL` or `R_ARM_THM_JUMP*` relocation in its
    `.rel.text.*` section, read with `readelf -r`) to a gbc-core function
    that is not in the set, not `#[cold]` (`.text.unlikely.*`), and not on
    a `FLASH_OK <glob>  # <reason>` line of the same file; and a
    `FLASH_OK` line that no longer matches such a callee. Not seen by it:
    calls through function pointers (the core has none on these paths) and
    callees outside the rlib (`__aeabi_mem*`, core's panic paths).

  Mutation-checked 2026-11-24 (scratch copies through `ITCM_SET_FILE`):
  a misspelt `Ppu::cpu_blocked` glob exits 1; `ITCM_BUDGET_BYTES=50000`
  exits 0 with "WARN: H753 ITCM set is over its 50000 B budget by
  5072 B"; removing the `cpu_blocked` glob exits 0 with a closure `WARN`
  naming the callee; a stale `FLASH_OK` line exits 0 with a `WARN`; a
  `FLASH_OK` line without a reason exits 1.
- *Post-link (firmware ELF, Stage 2):* every demangled name in the file must
  exist in `nm -C` of the ELF and have an address in
  `[0x0000_0000, 0x0001_0000)`. A name inlined away under LTO fails, so
  update the file. The check also reports `.itcm` size against 64 KiB and the
  number of long-branch thunks (`__*_veneer` with GNU ld,
  `__Thumbv7ABSLongThunk_*` with lld), and fails if `.itcm` overflows (the linker
  also does this). The H735 version checks every gbc-core function against
  `[0x0000_0000, 0x0002_0000)`.

**Is a <= 40 KB target for `execute`/`execute_cb` still needed?** No. They
are 31,404 B now. With the interpreter in ITCM, its I-cache footprint (the
M2 "2.6x the I-cache" concern) no longer matters, because TCM fetches
bypass the cache. On the final chip the whole core is in ITCM and the
binding constraint is the total, 97,136 of 122,880 B, which
`check-code-size.sh` gates. On the H753 the set total (55,072 of
57,344 B) is reported, not gated.

## Instruction-count benchmark

`scripts/check-bench.sh` runs `tools/bench` (release build, one codegen
unit as pinned in `Cargo.toml`) under
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

`bench --fingerprint [--seconds N] [--root DIR] [--audio-buffer N [--hash-samples]]` (also `--clock-offset N` and `--trace RUN`: "Invariance checks" below; N defaults to 3 and must be
at least 1, root to `test-roms`) prints one line per run, `<16-hex FNV-1a
hash> <path>`, sorted by path, so two builds can be compared bit for bit (ADR
0011 "Verification"). Corpus: every `.gb`/`.gbc` under Mooneye `acceptance`,
`misc` and `emulator-only`, `blargg`, `age-test-roms`, `same-suite`,
`cgb-acid2`, `cgb-acid-hell`, `dmg-acid2`, `mealybug-tearoom-tests`,
`rtc3test` and `mbc3-tester` (334 ROMs, each run twice: plain and
`<path>#input`), plus the 3,154 gambatte `*cgb04c*` ROMs and the 198 with a sibling
`<stem>_cgb04c.png` and no `cgb04c` in the name (the manifest's
`gambatte-cgb-png` group), each run once for N/8 seconds (22 frames at N=3;
the checks read frame 15): 4,020 runs, about 8 s wall on 8 threads at N=3. Fewer than 4,000
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
split), and `diff`. 3,832 runs, about 11 s wall. Without the flag the
output is unchanged.

`scripts/diff-fingerprint.sh <rev> [args]` builds `<rev>` in a temporary
worktree and the working tree, runs both fingerprints with the same args,
lists the differing ROMs and exits 0 only if none differ. A ROM run on only one side (the corpus changed since <rev>) fails it unless `--allow-corpus-change` is passed (then only the common ROMs are compared). It warns loudly when
the working tree has uncommitted changes under `crates/`, `tools/` or
`Cargo.*` (the new side is the working tree, not HEAD), when the two sides
have different run counts, and when ROMs fail to load. Use it on commits
that restructure core timing, which must not change behavior.

## Invariance checks

The core is a lazy catch-up design (ADR 0002, 0011, 0014, 0021): a
component's visible state is meant to be a function of the clock and the
writes, not of when it was last synced, and not of the clock's absolute
value. `scripts/check-sync-invariance.sh` checks both over the fingerprint
corpus, against the product build (plain release `bench`), and exits 0 only
if every line is identical. It uses two test-only Cargo features of
`gbc-core` (passed through `tools/bench` features of the same names). No
product build enables them, and with them off the core is byte-identical
(the hooks are `#[cfg]`ed statements and functions: `.text`, every budgeted
symbol and the bench Ir are unchanged).

```
TEST_ROMS=... scripts/check-sync-invariance.sh [--long] [--long-only]
```

About 4.5 minutes on 8 threads (three builds, 2 + 2 + 4 corpus passes), plus
about 1 for `--long`. Heavy: run it alone. The feature builds use the
`checked` profile (overflow checks and `debug_assert!`s on, so every pass
is also a panic check, e.g. `Ppu::sync`'s cursor assertion at every sync)
in their own target dir, `target/invariance`.

### Sync-placement invariance (`eager-sync`)

With the feature, `Ctx::eager_sync` runs at the start of every M-cycle,
before its access (`Bus::read`, `write`, `idle`, the dispatch's
acknowledge), and brings every lazily synced component up to the clock:
the PPU walk (and a running OAM DMA's bytes, as before every PPU sync), the
APU (samples included), the timer's lazy reloads (`Timer::catch_up`) and
the cartridge RTC. The skips (HALT, STOP mode, the speed-switch halt, a
VRAM DMA stall) go one M-cycle at a time, with the same sync in each. It
sets no deadline and raises no event of its own; in particular the PPU
deadline is left where the lazy build put it (`run_ppu_to`, not
`sync_ppu_to`). Serial has no lazy state (each bit is an event).

The script runs the corpus without audio and with `--audio-buffer 7
--hash-samples`, and compares each with the product build's lines. What
this proves: on everything the fingerprint hashes (registers, every I/O
register, frames, samples, events, per-chunk clocks, memory at the end),
no ROM in the corpus can tell a sync at every M-cycle from the syncs the
lazy build happens to make. A change that moves or removes a sync (a
`run_ppu_to` before a register write, an `apu_sync`, a `catch_up`) so that
a later one computes something different shows up as differing ROMs.
`cargo test -p gbc-core --features eager-sync` runs the unit tests in the
same mode (two tests of the skip's length are ignored there).

Known differences are listed in the script (`KNOWN_EAGER`), each with its
diagnosis; a listed ROM that stops differing fails the check too. One
today:

- `gambatte/lcd_offset/offset3_lyc99int_m2irq_count_1`: in single speed,
  line 0's mode-2 pulse is walked at dot 4, but the CPU must see its IF
  request a dot later. `Ppu::next_event` gets that by putting the deadline
  one past the point, so the request is posted late only because nothing
  syncs the PPU on the point itself. This ROM's IF read is at an M-cycle
  boundary on that dot (0:4, CPU grid offset 3): an eager sync there posts
  the request a dot early and the read sees it. So IF bit 1 at that one
  dot depends on sync placement, by design (the doc comment of
  `next_event` names the ROM). A refactor that adds a PPU sync at an
  M-cycle start (for instance on an IF read) changes this ROM in the
  product build; the clean form is a pending bit posted at the point + 1.

Diagnosing a difference: `bench --fingerprint --trace <run>` prints one
line per 456-clock chunk (the running hash, the clock, events, registers
and a few I/O registers); run it with both builds and `diff` for the
first chunk that parts.

### Clock-offset invariance (`clock-offset`)

With the feature, `Emulator::start_clock_at(clock)` (test hook, not
product API; call it right after `load` or `reset`; a multiple of 4,
because the APU's single-speed trigger grid is the clock mod 4) moves the
power-on state to `clock`: the clock, every deadline, the timer's counter
origin, the PPU's frame origin and sync clock, the APU's sync clock and
power-on phase, the RTC's sync clock. Records of past events that have
not happened (an OAM DMA, a speed switch, an HDMA trigger) keep their
power-on values, as on a machine that got there without one.

`bench --fingerprint --clock-offset N` (a build with `--features
clock-offset`) powers every run on at N and takes every hashed or passed
clock relative to it, so its lines must equal the run from 0. The script
runs the corpus, with `--audio-buffer 7 --hash-samples`, at 2^31 - 400,004,
2^32 - 700,000, 2^33 - 1,000,004 and 2^40 - 1,300,000: every run (the
shortest is 1,572,864 clocks) crosses its boundary, and two offsets are 4
mod 8. What this proves: nothing observable depends on the clock's
absolute value, and no truncating cast or wrapped compare of a clock
(`as u32`, `as i32`) misbehaves at those boundaries, on any path the corpus
reaches within its 0.4-3 s per ROM. No difference is known. The unit test
`a_run_from_a_moved_clock_matches_the_run_from_0` (core, with the feature)
is the small version.

### Long run (`--long`)

`bench --long [--minutes N] [--root DIR]` runs ten workloads that keep
working (the six synthetic programs, cgb-acid2, cpu_instrs, cgb_sound and
rtc3test) from power-on for 30 emulated minutes, 7,549,747,200 clocks,
past 2^32 (17 min 4 s), in frame-sized calls with an audio buffer. The
script builds it with the `checked` profile and no feature (the product's
code with overflow checks and debug assertions). It fails on a panic, a
call that makes no progress, a sample count other than one per 128 clocks,
or (for the workloads that keep the LCD on) a frame count more than one off
one per 70,224 clocks. 60.5 s wall for all ten in parallel (2026-11-24, 8
threads; the slowest, `synth:worst-case`, alone). It complements the
offset check: that one covers the whole corpus near four boundaries, this
one covers state that has actually lived for 2^32 clocks.

## Wall-clock benchmark (Stage 2 reference)

`bench --wall` measures emulated frames per second on the desktop. It is a
manual tool, not part of the gate: wall time is noisy (min..max spread of
the runs is printed), unlike the Ir benchmark above.

Run (quiet machine; no valgrind or build alongside):

```
cargo build --release -p bench
./target/release/bench --wall [--frames 1200] [--runs 7] [--ghz 4.0] [--root test-roms] [--rom PATH]...
```

Each workload starts from power-on and runs a fixed 1,200 emulated frames
(20 s) with the same call pattern as `run_emulation` (one frame per call,
1,024-slot audio buffer). One warm-up run is discarded, then the median of
7 timed runs is reported. Host cycles per frame is ns/frame x the nominal
clock (`--ghz`, default 4.0): it is **not** a counter reading, because turbo
and the TSC rate make a counter no more exact. Real-time speed is 59.73
frames/s (16.74 ms per frame, 70,224 clocks), so frames/s / 59.73 is the
margin over real time.

Workloads (ROMs from `scripts/fetch-test-roms.sh`, hash-pinned; `synth:*`
are built in memory): cpu_instrs (CPU), cgb_sound (APU), cgb-acid2 (PPU,
mostly halted), cgb-acid-hell (PPU, objects), mealybug hdma_timing-C and
same-suite hdma_mode0 (HDMA), and the synthetic worst-case, dma-storm,
cgb-raster and apu-writes. Test ROMs that finish early idle (halted or
spinning) for the rest of the run, so their numbers are a floor on cost,
not a stress figure; synth:worst-case is the upper bound.

Machine: Intel Core i7-4790K, 4.0 GHz base (turbo 4.4), 8 threads, WSL2
(Linux 6.18 microsoft-standard), Windows host, release build
(`opt-level` per workspace profile), commit 8ba9c4c, 2026-11-22.

**Baseline** (Mcycles/frame at the nominal 4.0 GHz):

| workload | stresses | frames/s | ns/frame | Mcycles/frame | min..max ns/frame |
|---|---|---:|---:|---:|---|
| cpu_instrs | CPU | 7554 | 132376 | 0.530 | 126715..173347 |
| cgb_sound | APU | 13708 | 72950 | 0.292 | 69910..78732 |
| cgb-acid2 | PPU, halted idle | 22051 | 45349 | 0.181 | 44188..47916 |
| cgb-acid-hell | PPU, objects | 4884 | 204769 | 0.819 | 201100..220435 |
| hdma_timing-C | HDMA | 10654 | 93857 | 0.375 | 89719..97328 |
| hdma_mode0 | HDMA | 41128 | 24315 | 0.097 | 24252..24432 |
| synth:worst-case | all paths | 2976 | 336075 | 1.344 | 316549..345562 |
| synth:dma-storm | DMA | 5610 | 178258 | 0.713 | 173895..197414 |
| synth:cgb-raster | mid-frame PPU writes | 4925 | 203061 | 0.812 | 189597..226890 |
| synth:apu-writes | APU writes | 14543 | 68764 | 0.275 | 63303..75594 |

This table is the reference for Stage 2: the MCU budget per frame is
(core clock / 59.73), e.g. 480 MHz gives 8.04 Mcycles/frame, so these host
figures scale by the Cortex-M7 vs. x86 IPC ratio, which Stage 2 must measure.

Context only, **not reproducible** (the human's commercial games, copied to
a scratch dir and run read-only from power-on for 1,200 frames, same method;
the ROMs are not in the repo and the title screens/intros they show are not a
fixed workload): Pokemon Crystal 13,835 frames/s (72,280 ns, 0.289
Mcycles/frame), Shantae 6,965 (143,569 ns, 0.574), Wario Land 3 7,445
(134,312 ns, 0.537). Pass them with `--rom PATH`.

## Outside the bound: dense mid-mode-3 register writes

Human decision (2026-11-23, session 10): the 50% bar (README) covers
realistic worst cases, which `synth:worst-case` models (all paths at once,
plus one mid-mode-3 SCX write per line once that workload lands). Programs
that write PPU registers many times per line are outside it. Measured by the
M8 perf review (x86 M Ir/frame, % of the 4.02 M line; test loop `LD (C),A;
XOR D` unrolled, a changed value every 3 M-cycles, ~11.7k writes/frame in
double speed, ~3.8k in mode 3):

| Write | stage1-m7 | M8 (pre-418421c) |
|---|---:|---:|
| SCX, double speed | 6.12 | 7.84 (195%) |
| SCY, double speed | 6.12 | 8.74 |
| LCDC bits 2/3/4, double speed | 6.27 | 10.15 (252%) |
| WX (window on), double speed | 6.13 | 7.11 |
| SCX, single speed | 3.33 (83%) | 4.34 (108%) |
| control (HRAM writes) | 1.83 | 1.81 |

Such a game would run slower than real time on the MCU rather than fail;
frame pacing absorbs it. Stage 2 task: measure these on the M7.

## Code size vs other cores (2026-11-23, session 12)

One-off comparison for context (input to the M10 code-size review). All
`.text` on thumbv7em / Cortex-M7, summed over the objects (no LTO, no
linking); C/C++ with Arm GNU 13.3 `-mcpu=cortex-m7 -mthumb
-mfloat-abi=hard -mfpu=fpv5-d16 -ffunction-sections -fdata-sections`.

| Core | O3 / -O2 | s / -Os | z |
|---|---|---|---|
| gbc-core 9159e03 (opt-level 3 = release profile = check-code-size) | 95,134 | 82,304 | 63,732 |
| gbc-core opt-level 2 | 86,340 | | |
| SameBoy ea667ad, emulation only (9 files, `GB_DISABLE_*`) | 78,924 (-O2) | 61,098 | |
| SameBoy, full `Core/` | 130,256 (-O2) | 104,186 | |
| Gambatte (libretro fork 138bcbb), minus statesaver/initstate | 92,468 (-O2) | 59,696 | |
| Gambatte, full `libgambatte/src` | 114,196 (-O2) | 68,530 | |

Reading: gcc `-Os` is closest to rustc `opt-level = "z"`, where gbc-core
(CGB C only) is ~4-7% above SameBoy/Gambatte's emulation code (which also
cover DMG, and SameBoy SGB/AGB). The release profile's opt-level 3 costs
~31 KB over z; per-module opt levels (cold paths at z, hot set at 3) are a
lever, but any change needs bench numbers (the 4.02 M line). Rough
(+-15%): different languages and optimizers.
