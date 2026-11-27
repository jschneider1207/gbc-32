# Performance

ROMs, each run for 20 emulated seconds (83,886,080 clocks): `blargg/cpu_instrs/cpu_instrs.gb`
(CPU), `blargg/cgb_sound/cgb_sound.gb` (APU), `cgb-acid2/cgb-acid2.gbc` (PPU).
The test ROMs are read from `$TEST_ROMS` (default `test-roms/`).

Synthetic ROMs cover paths those miss. `tools/bench` builds them in
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
  **Timer interrupt (M10, 2026-11-24).** Before this nothing in
  worst-case took an interrupt other than STAT: TIMA ran from TMA=FF
  with IE bit 2 clear, and since the lazy reloads (ADR 0011, 2026-11-24
  amendment) a timer nobody takes costs no events at all. Now IE = STAT
  and timer, TMA=E0 at TAC=05 (524 kHz in double speed): an overflow
  every 256 base clocks, **16,384 Hz, 274.3 timer IRQs a frame** (about
  two per line), each serviced by a PCM sample player at 0x50: it
  advances a 16-bit sample counter at C002 (its low byte indexes a
  256-byte table at 4000, a 4-bit triangle wave), loads the sample and
  writes it to NR50, the master volume, the usual way to play 4-bit PCM
  on the Game Boy (as `synth:apu-writes` does); 42 M-cycles with the
  dispatch, about 33% of the CPU. `@0` (no APU at all) plays to HRAM
  FF80 instead. The rate is twice `synth:apu-writes`' 8 kHz, the high
  end for such a driver, affordable in double speed; TMA=FF would keep
  the CPU in the handler. The
  STAT handler moved to 0x68 behind a `JP` at 0x48 (4 M-cycles more per
  STAT IRQ) to free the timer vector. The per-line SCX write now detects
  a new line by LY (mode 3 on an LY other than the last one written)
  instead of a mode change since the last poll: with the handlers
  taking most of a heavy line's modes 0 and 2, the mode poll saw no
  change on ~27 lines a frame (117 SCX writes instead of 144). It still
  writes once per visible line, 18-42 M-cycles into mode 3
  (`hold_fetch` 143 a frame). Everything else is unchanged. APU writes
  per frame: 414 (`Apu::write`, 49,459 in 2 s: 274 samples plus the
  STAT handler's ~142 NR50 toggles). Ir (2 s): 276,111,793 ->
  **289,297,010 (+4.8%)**, 2.311 -> **2.422 M Ir per frame, 60.2% of
  the 4.02 M line**. The same program with IE = STAT only (the new TMA
  and poll, no timer IRQ) is 275,164,860 (2.304 M). A first version
  whose handler only bumped the counter (32 M-cycles) measured
  271,698,253, *below* both: its cheap cycles displaced poll-loop
  iterations whose I/O reads (STAT, LY, SC, HDMA5) cost more per
  M-cycle. A handler that does a sample driver's work (a table load and
  an APU write) costs a net 435 Ir per IRQ over the polling it
  displaces ((289.30 - 275.16) M / 32,499 IRQs), so the timer IRQ now
  raises the bound, as it should.
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
- `synth:dmg-raster` (2 s, M10 2026-11-24): the DMG render path, which no
  other line ran (every other bench ROM is CGB). Header 0143 = 00, so
  DMG compatibility mode, single speed. BG (map 9800, a tile with all
  four colors across each row), window (WY=0, WX=87, map 9C00, a solid
  tile) and 40 8x16 objects in the worst-case layout (10 per line on 64
  of 144 lines, X=8+6i overlapping, so DMG's X-coordinate priority
  decides), with OBP0/OBP1, X/Y flip in every combination and
  BG-over-OBJ priority on three objects of each group. LCDC=F7. Per line:
  in mode 0, rotate the shades (a permutation) into BGP, OBP0 and its
  complement into OBP1, SCY + 1; in mode 3, SCX and the next BGP mid-line
  (the held-fetch path twice a line: `hold_fetch` 33,888 calls in 2 s,
  284 a frame). When added: **185,451,213 Ir = 1.552 M Ir per frame**
  (cgb-raster 2.268 M). A unit test checks that it runs in DMG mode
  (KEY1 locked, the objects drawn in the caller's DMG OBJ colors) and
  that a frame shows objects on exactly the 64 lines, the tenth
  object of each line, a changing BG and a flat window.
- `synth:oam-lines` (2 s, M10 2026-11-24): OAM written on every visible
  line, which no other line did (HBlank sprite multiplexing). An ordinary
  single-speed CGB frame: BG (map 9800, a four-color tile), 40 8x16
  objects in the worst-case layout (10 per line on 64 of 144 lines, none
  on the other 80), LCDC=97. Per line: wait for mode 0, write a counter
  to the X of objects 0, 10, 20 and 30 (14 M-cycles, done at most 104
  dots into an HBlank of 121 or more, so OAM is open), wait for mode 3:
  144 rounds a frame. A unit test checks that all four X bytes take
  every write and advance by 144 a frame. Numbers: "Object-line summary"
  below. Not in `check-bench.sh`'s list until its baseline is recorded.
- `synth:storm-control`, `storm-apu`, `storm-spsw`, `storm-div`,
  `storm-gdma`, `storm-ppu` (M10 review, 2026-11-24): adversarial
  programs, one costly access in the tightest loop for the whole frame.
  Measured, on purpose not benched: "Outside the bound" below.

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

**Re-measured (session 19, 2026-11-25, at c07fb77, check-bench method).**
(a) channels and mixing: 402.8k Ir/frame, `Ctx::apu_sync` inclusive under
`div_apu_event` (48,121,826 Ir / 119.455 frames), on a scratch
`worst-case` variant with no APU register writes (mask F, plain STAT
handler, timer handler writing HRAM, as `worst-case@0` but with the
channels on); 427.0k with the second `apu_sync` entry (from
`Emulator::run`) added. The 224.1k figure's choice between the two is not
recorded. (b) APU register writes: 731.1 Ir per write (`synth:apu-writes`
103,354,637 minus `@hram` 91,355,345, over 16,411 `write_apu` calls): the
control now pays the doubled window cost too. `synth:worst-case` 2.630 M
Ir/frame, APU share 652.7k. **New budgets (human, 2026-11-25; ADR 0021
amendment): (a) 450k Ir/frame, (b) <= 770 Ir per write**, both
report-only; the split-budget paragraph above is superseded.

**Output filter (ADR 0035, 2026-11-24).** The output is now 64-clock
window sums and an 8-tap FIR at half rate instead of 128-clock averages
(`apu/mix.rs`; alias levels in `docs/reference/audio-aliasing.md`). The
human approved its cost beforehand (about +10% on the worst case) and a
re-recorded baseline, so budget (a) above no longer describes the APU;
a new figure is the human's to set. Measured against main at 2821437
(`check-bench.sh`):

| bench | before, Ir | after, Ir | delta | per frame |
|---|---|---|---|---|
| cpu_instrs | 1,264,344,219 | 1,327,813,432 | +5.02% | +53.1k |
| cgb_sound | 890,523,560 | 973,851,265 | +9.36% | +69.8k |
| cgb-acid2 | 649,967,201 | 726,390,252 | +11.76% | +64.0k |
| synth:event-storm | 175,300,349 | 182,973,533 | +4.38% | +64.2k |
| synth:dma-storm | 381,935,612 | 393,398,153 | +3.00% | +64.0k |
| synth:halt-irq | 263,308,262 | 301,522,527 | +14.51% | +64.0k |
| synth:worst-case | 286,911,384 | 314,276,521 | +9.54% | +229.1k |
| synth:cgb-raster | 267,388,360 | 275,061,275 | +2.87% | +64.2k |
| synth:apu-writes | 77,400,848 | 103,388,853 | +33.58% | +217.6k |
| synth:dmg-raster | 185,765,867 | 193,438,927 | +4.13% | +64.2k |

`synth:worst-case` is 2.63 M Ir/frame (2.40 M before), 65% of the 4.02 M
line. thumbv7em `.text` 115,692 -> 115,940 B (+248); `size_of::<Memory>()`
50,984 -> 51,016 B (+32: the 8-window history). Where it goes:

- The first stage, twice the windows (each one a span to list, four
  integrator calls and a mix): measured alone before the decision, by a
  scratch `SAMPLE_PERIOD_LOG2 = 6`, at +201k Ir/frame on worst-case, +44k
  on cgb_sound and +191k on apu-writes. With every channel silent it is
  what the benches without sound pay: about 64k Ir/frame with the FIR.
- The FIR and its history: the remaining 26-28k Ir/frame, about 50 Ir
  per output sample (two sides: 16 loads, 8 multiplies, the shift and
  the saturation, and moving 24 bytes of history).
- The history as a ring read through `(at + i) & 7` was measured first:
  317,069,901 Ir on worst-case and 134 B more than the version that
  landed (314,276,564 at the same base), which moves the history down by
  two entries per sample and reads it at fixed offsets.
- Not measured on the Cortex-M7. By the Ir ratio of "Measured on a
  Cortex-M7" (1.12-1.33 cycles per x86 Ir in ITCM), +229k Ir is roughly
  +0.26-0.30 M cycles a frame on the worst case, 4 to 5 points of a
  frame at 392 MHz: an estimate.

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
size test capped it at 128 B then, a cap dropped on 2026-11-24) or
uninitialized memory (`unsafe`); the
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
division); the division-based search (`next_point`; since 2026-11-24
`Ppu::seek_point`, from the timeline table) only re-seeks after
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

2026-11-24, with the per-function byte caps gone (review F: L3, L4, L8,
L13, L14; B11, B13). Ir against main 66cfb53, measured the same day;
total `.text` 109,330 -> 111,680 B (+2,350):

| step | cgb-acid2 | worst-case | cgb-raster | cpu_instrs | halt-irq | `.text` |
|---|---|---|---|---|---|---|
| one `point_irq`, inlined into the walk; the look-ahead skips `Latch` and calls it | -2.03% | -0.33% | +0.41% | -0.03% | -0.09% | +1,116 |
| `walk_to`'s "no point to run" test inline; the deadline's sync without it, inlined into `sync_ppu` | -0.59% | -0.13% | -4.62% | -0.13% | -0.39% | +276 |
| the walk without a STAT source as its own walk (`step_work`) | -0.22% | -0.03% | -0.19% | -0.23% | -0.70% | +1,058 |
| the seek uses `step` (no `step_vblank`) | 0 | 0 | 0 | 0 | 0 | -58 |
| `vdma_arm`'s "armed for this line" test inline, not `#[cold]` | 0 | -0.22% | 0 | 0 | 0 | +116 |
| LCDC writes test LCDC.5 before `wy_write` | -0.02% | 0 | 0 | 0 | 0 | +8 |
| OAM accesses and `on_line` without a panic path | +0.01% | 0 | +0.01% | +0.02% | +0.06% | -166 |
| all (vs main) | -2.83% | -0.71% | -4.40% | -0.38% | -1.12% | +2,350 |

(Each row against the row before; the others of the nine: cgb_sound
-0.56%, apu-writes -0.78%, event-storm -0.31%, dma-storm -0.30% in all.)
Then line 153's LYC=0 request as a timed one (a behaviour change,
`docs/reference/stat-irq.md`): cgb-acid2 +0.01%, the rest 0.00%; `.text`
111,552 B (-128: `Ppu::early_point` and its case in `write_if` go).

To check the ITCM report after these: `walk_to` is `run_points::<false>`
(2,058 B) and `run_points::<true>` (578 B) now, and `point_irq` is
`point_irq_ahead` (1,054 B).

Tried and not kept:
- `point_irq` inlined into the look-ahead too: LLVM hoists every arm's
  select and LYC tests in front of the loop (about 70 instructions per
  `next_event`, whatever it visits). cgb-acid2 -1.90% instead of -2.03%,
  and with no source selected cpu_instrs +0.21%, halt-irq +0.61%; with the
  two walks separate it is cgb-acid2 +1.1% against the call.
- One function per update point that both applies the STAT part and
  returns the next point (`step` and `point_irq` fused, one `match`):
  cgb-acid2 +2.2%, worst-case +0.5% against main (the `(bool, Cursor)`
  result goes through memory; the look-ahead's call got dearer).
- `cpu_blocked` inlined into the VRAM, OAM and palette checks (L13):
  +470 B for cgb-acid2 -0.06%, and cgb-raster +2.45% (`palette_blocked`'s
  fast path carries the larger frame). The nine benches hardly access
  VRAM or OAM from the CPU (at most 0.12% of cgb-acid2), so they can't
  show a gain there; a VRAM-heavy bench would be needed to decide.
- A clamp in `LineObjects::len` to drop the bounds checks in `set_stalls`
  and `Renderer::last_fetched`: cgb-acid2 -0.25%, worst-case -0.14%, but
  halt-irq +0.21%, apu-writes +0.15%, cgb_sound +0.11%.

Still there (B11): the look-ahead re-walks the points the sync then
runs (about 100 Ir per `next_event` with a STAT source selected, one
call per sync), and every PPU register write recomputes it although
only STAT, LYC, LCDC and a moved mode-3 end can change it.

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

Measurement: `RUSTFLAGS="<.cargo/config.toml flags> -C target-cpu=cortex-m7"
cargo build -p gbc-core --release --target thumbv7em-none-eabihf
--target-dir target/codesize` (the flags from `scripts/lib/rustflags.sh`,
"Build flags" below; release is pinned to `codegen-units = 1` in
`Cargo.toml`, the build the firmware will use; no LTO, there is no
firmware crate to link yet), then
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

Sizes at the end of 2026-11-24 (6f74775; the script prints all of them):
total `.text` 115,866 B, 94% of 122,880 (margin 7,014 B); `Emulator`
952 B, `Memory` 50,984 B.

Historical, the morning of 2026-11-24, when the per-function caps were
dropped (6294f0b, before the M10 restructuring and the move to one
codegen unit): total `.text` 97,136 B, 79% of 122,880 (margin 25,744 B);
`Emulator` 848 B, `Memory` 50,968 B. Largest per-function lines then
(report only; `check-code-size.sh` prints the current ones):

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

### Firmware-link size (Stage 2 M1, C-P5)

Measured 2026-11-25 on `e91faae` (rustc 1.100.0, the release profile with
`codegen-units = 1`, no LTO) on `crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf`
(`build.sh`; the cgb-acid2 image gives the same figures) with
`elixir scripts/report-firmware-size.exs <elf> --rlib <the rlib it was
linked from>` (report only, exit 0; readelf, ar and the Arm GNU objdump
under `~/.local/opt`, or `$ARM_OBJDUMP`). gbc-core code is every function
whose v0 path is rooted in `gbc_core`, the set the H735's
`8gbc_core`-segment ITCM rule would take. The closure is what those
functions reach through branches, `movw`/`movt` address pairs and
4-aligned data words (literal pools, vtables); its "panic/fmt" part is what
lies only behind `core::panicking`, the `*_fail`/unwrap failures, the panic
handler, or gbc-core's own `Debug` impls (formatted by the front end, never
by the emulation).

| Item | Bytes |
|---|---|
| rlib `.text*` (the `check-code-size.sh` gate) | 116,150 |
| of it not in the link (42 functions the headless loop does not use, GC'd) | -5,088 |
| gbc-core in the link (177 functions; 60,150 in ITCM, 50,872 in flash on the H753) | 111,022 |
| of which in-function alignment `nop`s (already inside both figures above) | 6,140 |
| inter-function padding before gbc-core functions, as linked | 494 |
| normal-path extras: compiler_builtins (`memmove` 1,410, `u64_div_rem` 864, `memcpy` 660, `__aeabi_mem*`, `memcmp`, `__aeabi_uldivmod`), 15 functions + 2 B padding | 4,278 |
| panic/fmt-path extras: core::fmt and panicking 5,522, the board's panic handler and UART writer 1,028, `rust_begin_unwind` 36, + 16 B padding | 6,602 |
| veneers if the panic/fmt path stays in flash (6 targets, 10 B each) | 60 |
| gbc-core `.rodata` in the link: `SPREAD`/`ATTR_FETCH`/`POP8` (`.dtcm_rodata`) + 304 B of panic locations and strings (flash) | 2,608 |
| other crates' `.rodata` the core reaches (fmt and panic tables) | 519 |

Against the gate figure (122,880 B) and the whole 128 KiB ITCM (131,072 B):

| H735 ITCM content | Bytes | Gate margin | ITCM margin |
|---|---|---|---|
| A: this link's gbc-core + normal path, panic/fmt in flash | 115,854 | 7,026 | 15,218 |
| B: this link's gbc-core + the whole closure | 122,396 | 484 | 8,676 |
| C: every rlib function + normal path, panic/fmt in flash | 120,488 | 2,392 | 10,584 |
| D: every rlib function + the whole closure | 127,030 | -4,150 | 4,042 |

There is no cross-crate inlining without LTO, so each linked gbc-core
function has its rlib size (rlib = link + dropped, to within one 40 B
generic emitted in gbc-core's unit under core's path); the rlib-to-link
difference is only what the link garbage-collects, and that depends on the
front end: the handheld will use API the headless loop does not (saves,
input, audio). No other crate instantiates gbc-core code. The 6,140 B of
`nop`s are LLVM's loop and branch-target alignment for the cortex-m7; on
zero-wait ITCM they may buy nothing (a later size lever to measure, not a
gate question). The rlib gate stays an upper bound for gbc-core's own code.

**What the numbers say.** The 8 KiB reserve was to hold ISRs, the frame
blit, compiler_builtins and veneers. The builtins the core needs on its
normal path are 4,278 B, so at today's size (C) the platform has 10,584 B
of ITCM left, and a core grown to the gate would leave 8,192 - 4,278 - 60
= 3,854 B for the ISRs and the blit. That holds only with the panic and
fmt code in flash (D does not fit the gate), which costs nothing at run
time: it runs only on a panic or when the front end prints an error.

**Recommendation: (c)** keep the rlib gate as is (conservative for any
front end, no firmware build, ~1 s) and add the firmware-link report
(`scripts/report-firmware-size.exs`) as a report line on the firmware
images. The H735 linker script should take `compiler_builtins`' `mem*` and
division helpers into `.itcm` with the core and leave core::fmt,
core::panicking and the panic handler in flash. The 122,880 B figure does
not need to change for this; whether 3,854 B for ISRs and blit at a full
gate is enough, or the gate should drop by the builtins' 4 KiB (to
118,784 B), is the human's call once the H735 front end exists.

### CPU write path (M10, review B8 / inventory A1)

`Ctx::write_mapped` is still one out-of-line function, but its tests now
run in the order real code takes them (C000-DFFF first, then E000-FFFF,
then VRAM, cartridge RAM, ROM), and FF00-FFFF is dispatched in it
(`write_ff` inlined: HRAM stored there, `write_io` and `write_ie`
called from there) instead of through a second call to `write_high`, which is gone.
The forms measured on 406c60b (check-bench Ir against the baseline of
70284ec; total gbc-core `.text` on thumbv7em, 113,154 B before; each row
is the whole change against that base, not a step on the row above):

| Form | cpu_instrs | cgb_sound | cgb-acid2 | event-storm | dma-storm | halt-irq | worst-case | cgb-raster | apu-writes | `.text` (B) |
|---|---|---|---|---|---|---|---|---|---|---|
| 1: tests reordered, WRAM first | -0.68% | -0.15% | 0.00% | -0.11% | -0.18% | -0.24% | -0.17% | -0.43% | -0.12% | -12 |
| 2: 1 + FF00-FFFF dispatched in `write_mapped` (chosen) | -1.65% | -0.17% | -0.03% | -0.38% | -0.20% | -0.96% | -0.56% | -1.51% | -0.47% | -16 |
| A1 middle form alone: C000-DFFF store inlined in `Bus::write`, the rest a call | -1.11% | -0.39% | +0.01% | -0.05% | +0.70% | +0.36% | +0.05% | -0.21% | +0.10% | +3,296 |
| 2 + the middle form | -2.41% | -0.41% | 0.00% | -0.44% | +0.49% | -0.60% | -0.48% | -1.72% | -0.36% | +3,280 |
| A1 whole: `write_mapped` `#[inline(always)]` | -2.37% | -0.14% | -0.03% | -0.77% | -0.06% | +0.54% | -0.37% | -3.03% | -0.10% | +10,600 (over the gate) |
| B8 for reads: WRAM bank by value in `Hot`, branch-free index (`low13 + bit12 * (bank - 1) * 0x1000`) | +6.74% | +6.59% | +0.01% | 0.00% | +4.31% | +0.17% | +0.06% | +0.01% | +0.05% | -18 |

The middle form adds 3,152 B to `Cpu::run` (25,414 -> 28,566 B; it has
a copy at every write site) and on top of form 2 buys 0.8% on
cpu_instrs and 0.2% on two more lines while three get slower: not taken.
The whole inline is what the middle form plus form 2 already give, for
three times the bytes. The branch-free WRAM index left `Cpu::run` at the
same 25,414 B on thumbv7em (no bytes to win there) and costs two more
x86 instructions on every bank 0 read, which is most of what cpu_instrs
executes; the branch stays.

### CPU read path (M10, review B8)

The inlined `bus::read` dispatched with a `match` on `(addr | dma) >> 13`,
which LLVM laid out as ROM, VRAM, cartridge RAM, WRAM, `read_high`: a
WRAM read (data, the stack, and every opcode of code running from RAM)
paid two compares for regions that are read rarely. It is now an `if`
chain: below 8000 ROM (unchanged), then C000 and up (below E000 WRAM,
else `read_high`), then cartridge RAM, then VRAM. Measured on the chosen
write form above (check-bench Ir against the same baseline, so the
write form's gain is in every row; `.text` against 113,138 B):

| Form | cpu_instrs | cgb_sound | cgb-acid2 | event-storm | dma-storm | halt-irq | worst-case | cgb-raster | apu-writes | `.text` (B) |
|---|---|---|---|---|---|---|---|---|---|---|
| write form 2 alone | -1.65% | -0.17% | -0.03% | -0.38% | -0.20% | -0.96% | -0.56% | -1.51% | -0.47% | |
| ROM, `>> 13 == 6` WRAM, `>= E000` high, cart RAM, VRAM | -7.59% | -5.23% | -0.04% | -0.39% | -1.58% | -0.84% | -0.59% | -1.50% | -0.43% | +1,126 |
| ROM, `>= C000` (`< E000` WRAM, else high), cart RAM, VRAM (chosen) | -7.52% | -5.22% | -0.04% | -0.39% | -5.24% | -1.14% | -0.67% | -1.50% | -0.53% | +770 |

cpu_instrs, cgb_sound and `synth:dma-storm` run their code from WRAM,
which is where the 5% comes from; code running from ROM gains only on
its data and stack reads (the other lines). `Cpu::run` 25,414 ->
26,128 B.

Measured on top of that (06e3750, `.text` 113,908 B) and not taken:

| Step | cpu_instrs | cgb_sound | cgb-acid2 | event-storm | dma-storm | halt-irq | worst-case | cgb-raster | apu-writes | `.text` (B) |
|---|---|---|---|---|---|---|---|---|---|---|
| 06e3750 | -7.52% | -5.22% | -0.04% | -0.39% | -5.24% | -1.14% | -0.67% | -1.50% | -0.53% | |
| WRAM store inlined for stack pushes only (a `Bus::write_stack` with the C000-DFFF store inline, used by `Cpu::push`) | -8.27% | -5.47% | -0.05% | -0.73% | -5.24% | -1.14% | -0.72% | -1.81% | -0.57% | +1,664 |
| Inventory L1: the 32 CB (HL) forms each in their own arm again | -7.52% | -5.22% | -0.06% | -0.39% | -5.24% | -1.14% | -0.67% | -1.50% | -0.53% | +4,738 |

The push-only inline gets all of what the middle form gave (the stack is
where the WRAM writes of these ROMs go) for half its bytes and without
its regressions, but it is still 1.7 KB for 0.75% on one line and
0.05-0.35% on the rest, and it adds a method to the public `Bus` trait:
left out, the first candidate if `.text` room is left at the end of M10.
L1 moves only cgb-acid2, by 0.02%, for 4.7 KB (`execute_cb` 6,860 ->
11,598 B): the shared (HL) body stays.

### Scheduler and attribute pass (M10, review B5 / inventory themes 4, 10, 11)

Each row is one step measured against the row before it (check-bench Ir;
total gbc-core `.text` on thumbv7em, 113,580 B at the start, baseline
eb31bf6). "all nine" lists cpu_instrs, cgb_sound, cgb-acid2, event-storm,
dma-storm, halt-irq, worst-case, cgb-raster, apu-writes in that order.

**One-pass scheduler (B5).** `run_events` tests the three hot kinds in
place in index order, each handler stores its next deadline with
`Schedule::rearm`, and the minimum is folded once (`hot_min`, `settle`).
The pass starts over wherever a handler leaves its own or an earlier
kind due, so the event order is the old loop's.

| Step | Ir | `.text` (B) | Outcome |
|---|---|---|---|
| Hot kinds tested in place, restart after every handler, one fold at the end; handlers still `set` | all nine +0.09, +0.12, +0.26, +0.05, +0.10, +0.12, +0.04, +0.04, +0.11% | | dropped: the fold was not the cost |
| Single pass, and the Ppu event re-arms in place (no out-of-line `set`, no `reschedule` that `boundary` redoes) | all nine -0.48, -0.68, -1.47, -0.29, -0.12, -1.24, -0.31, -0.22, -0.81% | -324 | kept |

**Timer and scheduler attributes (theme 10).**

| Step | Ir (lines that moved) | `.text` (B) | Outcome |
|---|---|---|---|
| A4 `Timer::reload` not forced out of line (inlines into `boundary`) | halt-irq -0.06%, apu-writes -0.02% | +140 | kept |
| A4 `Timer::catch_up` attribute removed | none | 0 | kept (the compiler keeps it out of line) |
| A4 `Timer::schedule_overflow` unpinned | event-storm -0.27%, halt-irq -0.26%, cgb-acid2 -0.17%, worst-case -0.14%, apu-writes -0.14%, cgb_sound -0.08%, cpu_instrs -0.06% | +1,270 | dropped (bytes) |
| A5 `Timer::sync`, `write_tac`, `write_div` unpinned | none | +460 | dropped; comment now cites this |
| A3 `serial_event` without `#[cold]`, still out of line | none | -34 | kept |
| A3 `serial_event` inlined into `boundary` | event-storm -0.52%, worst-case -0.15%; cgb-acid2 +0.22%, halt-irq +0.32%, apu-writes +0.15%, cgb_sound +0.10%, cpu_instrs +0.07%, dma-storm +0.06% | -74 | dropped |
| A6 `Schedule::set` one `#[inline]` function | event-storm -0.80%, worst-case -0.48%, cgb-acid2 -0.05%, dma-storm -0.07%, cgb-raster -0.07% | +3,006 | dropped (bytes) |
| A6 `set` inlined only in `schedule_timer` and `schedule_serial` | event-storm -0.79%, worst-case -0.32% | +166 | kept: the `set`/`set_inline` pair stays for this |
| A6 `schedule_serial`, `schedule_timer` unpinned | event-storm +0.22%, worst-case +0.09% | +1,450 | dropped |
| A7 `read_io_other` unpinned | none (LLVM keeps it out of line) | 0 | attribute kept, see next row |
| A7 `read_io_other` as arms of `read_io` | dma-storm +4.00%, event-storm +1.97%, cpu_instrs +1.07%, worst-case +1.07%, halt-irq +0.74%, cgb-raster +0.57%, apu-writes +0.37%, cgb_sound +0.04%, cgb-acid2 +0.02% | -78 | dropped |
| L7 timer lazy whenever IF bit 2 is set, IE or not (measured later against 54c9836, 115,792 B, ten lines): the event only sets IF, the dispatch's acknowledge of the timer bit calls `schedule_timer`, the IE write no longer does | halt-irq +4.890%, apu-writes +1.613%, worst-case +0.845%; event-storm -0.335%, cgb-acid2 -0.155%, cgb_sound -0.103%, cpu_instrs -0.073%, dma-storm -0.059%, dmg-raster -0.049%, cgb-raster -0.034% | -198 (`boundary` 1,048 -> 888, `Cpu::run` 26,128 unchanged) | dropped |
| L7 with the event still running its reload and the acknowledge only storing the deadline (an out-of-line `rearm_timer`: `Timer::deadline`, `set_inline`, `reschedule`; `schedule_timer` if a later reload is due) | halt-irq +2.762%, apu-writes +0.951%, worst-case +0.558%, cgb-acid2 +0.062%; others within +-0.001% | +206 (`boundary` 984) | dropped |

L7 cannot pay on these lines: a serviced timer interrupt is acknowledged
before the next overflow, so there is no event to save, and the deadline
that the event re-armed in place (inside the scheduler pass, which folds
the minimum anyway) has to be set again from the acknowledge, with a
call and a `reschedule` of its own (about 80 Ir per interrupt in the
first form, 45 in the second; halt-irq takes 16 thousand a second).
It saves events only while IE bit 2 is set and more than one overflow
goes by before the dispatch (IME off, or a higher-priority handler
running, for longer than a timer period), which no bench line and no
ordinary game does. Both forms passed `cargo test -p gbc-core` with the
differential test's deadline check tightened to "`NEVER` whenever IF
bit 2 is set"; neither was taken further (no fingerprint run).

**OAM DMA read clears (theme 4).** `Memory::dma_read_clears` moved to
`Sys` (still atomics behind `&`): cgb-acid2 +0.03%, worst-case +0.01%,
-32 B. The `&mut` CPU read path (to make it a plain `[u32; 5]`) was
re-measured in the next pass and dropped again.

**Attribute sweep (theme 11).** Only T2 was done: `fmt_flags` loops
plainly instead of through `black_box` (+336 B, `Debug` output only, no
bench line executes it). A14, A8 and A11: the next pass.

### Small-state and attribute pass (M10, inventory themes 1, 4, 11)

Each row is one step measured against the row before it (check-bench
Ir, ten lines: `synth:dmg-raster` is new; total gbc-core `.text` on
thumbv7em, 113,952 B at the start, base cab6e24). Every kept step is
fingerprint-identical to `main` (`--audio-buffer 7 --hash-samples`).

| Step | Ir (lines that moved) | `.text` (B) | Outcome |
|---|---|---|---|
| A4 `Timer::schedule_overflow` unpinned, re-measured with the serviced 16 kHz timer interrupt in `synth:worst-case` | event-storm -0.268%, halt-irq -0.261%, cgb-acid2 -0.172%, apu-writes -0.136%, worst-case -0.128%, cgb_sound -0.082%, cpu_instrs -0.058%, dma-storm -0.047%, dmg-raster -0.039%, cgb-raster -0.027% | +1,304 | dropped again: no line reaches 0.5% |
| S10 FF55 read from `VramDma::read_hdma5`, no mirror in `io[0x55]` | worst-case +0.059% (its loop reads FF55); others identical | +2 | kept |
| S2, S3, S9 `Sys::switch_halt_end`, `VramDma::stall_end` and `Refetch` enum; `apu::Fs::ds_delay`; `Apu::write(.., double: bool, ..)` | all ten within +-0.06% (worst-case -0.050%, apu-writes -0.056%) | +256 | kept |
| S4 `&mut` CPU read path (`read_cpu`, `read_high_cpu` with the body inlined in both copies), `ReadClears` a plain `[u32; 5]`; `read_high_cpu` left to the inliner | cgb_sound +1.871%, dma-storm +1.110%, cpu_instrs +0.913%, cgb-raster +0.656%, dmg-raster +0.483%; halt-irq -0.365% | +10,702 (`Cpu::run` 36,264; over the total) | dropped |
| S4 the same with `read_high_cpu` `#[inline(never)]` | all ten slower: event-storm +0.282%, worst-case +0.277%, cgb-raster +0.191%, dmg-raster +0.182%, cpu_instrs +0.181%, halt-irq +0.123%, dma-storm +0.104%, apu-writes +0.083% | +140 | dropped: the atomics behind `&` stay (ADR 0028) |
| A8 `stop_div_reset`, `set_div_apu_deadline`, `start_audio`, `finish` unpinned; `vdma_turn` kept out of line (inlined, `vram_dma` and `vdma_stall` become callees of `Emulator::run` and the ITCM closure check fails) | all ten within +-0.05% | +138 | kept |
| A11 `LineObjects::consider_cold` removed, `scan_to` calls `consider` | cgb-acid2 -0.163%, dma-storm -0.259%; others identical | +186 | kept |
| A14 every listed bare `#[inline(never)]` removed in `bus.rs` (17), `ppu/mod.rs` (9), `apu/` (10, with `read_apu`) and `cart/` (3) at once | cpu_instrs +4.472%, dma-storm +6.940%, event-storm +3.560%, worst-case +3.511%, dmg-raster +1.919%, halt-irq +0.561%; cgb-acid2 -2.082%, apu-writes -0.803%, cgb_sound -0.685% | +1,348 (`read_high` 956 -> 2,100, `write_io` 710 -> 2,912, `boundary` 1,080 -> 1,736) | dropped as a batch |
| A14 `ppu/mod.rs`, `apu/` (without `read_apu`, which inlines into `read_high`) and `cart/` only | all ten faster: cpu_instrs -0.146%, cgb_sound -0.719%, cgb-acid2 -0.805%, event-storm -0.304%, dma-storm -0.040%, halt-irq -0.884%, worst-case -0.625%, cgb-raster -1.204%, apu-writes -1.683%, dmg-raster -0.352% | +566 | superseded by the next row |
| A14 as the row above with the RTC paths (`read_rtc`, `write_rtc`, `latch_rtc`) pinned again: rare paths, and `write_rtc` inlined into `Cart::write_ram` makes that a flash callee of `write_mapped` (ITCM closure check) | against the A11 row: cpu_instrs -0.421%, cgb_sound -0.762%, cgb-acid2 -0.815%, event-storm -0.304%, dma-storm -0.228%, halt-irq -1.007%, worst-case -0.739%, cgb-raster -1.204%, apu-writes -1.725%, dmg-raster -0.355% | +664 | kept |

Result: 115,198 B (+1,246; 7,682 B under the total).

Open: the `bus/` attributes of A14 (`bus.rs` was one file then) one at a time (the batch says the
read side, `read_during_dma` and `read_oam` into `read_high`, and the
`write_io` and `boundary` callees cost more than they gain; none was
measured alone). L7 (timer lazy whenever IF bit 2 is set) was measured
afterwards and dropped: the rows in the table above.

### `Speed` enum (M10, review D4 first half, fbfdf96)

One `speed::Speed` replaces the `double: bool`, M-cycle-length and
shift-count forms of the CPU speed; `Hot::mcycle` stays the number the
tick reads. Fingerprint-identical to `main` (63ff596). Total gbc-core
`.text` on thumbv7em: 116,358 -> 115,792 B (-566).

| Line | Ir, `main` | Ir, fbfdf96 | Delta |
|---|---|---|---|
| cpu_instrs | 1,264,181,920 | 1,264,344,877 | +0.013% |
| cgb_sound | 890,360,439 | 890,523,572 | +0.018% |
| cgb-acid2 | 650,060,048 | 650,309,926 | +0.038% |
| event-storm | 175,085,826 | 175,300,379 | +0.123% |
| dma-storm | 382,222,944 | 381,935,612 | -0.075% |
| halt-irq | 263,062,919 | 263,308,262 | +0.093% |
| worst-case | 286,755,768 | 286,951,099 | +0.068% |
| cgb-raster | 267,389,069 | 267,388,402 | -0.000% |
| apu-writes | 77,367,946 | 77,400,848 | +0.043% |
| dmg-raster | 185,851,400 | 185,765,879 | -0.046% |

(The commit body's "other nine within +-0.02%" was an intermediate
build's figure; these are the tip's.) Notes:

- With `dma_conflict` and `OamDma::due` taking a `Speed` from
  `Hot::speed()`, dma-storm was +1.74%; with only `Ctx::dma_sync`'s
  `due` left on it, +0.45% (4 Ir per call, 359,511 calls). Both now take
  the shift from the tick's word (`Hot::mcycle_shift`).
- The event-storm and halt-irq rise is one instruction per call in
  `Ctx::schedule_serial` (+99,086 Ir over 99k calls) and in `boundary`
  (+117,526): the functions `set_countdown` is inlined into. It is not
  the timer's field (`Timer::shift: u32` restored: 175,302,438), nor
  `set_countdown`'s text, nor either DMA path taken back one at a time
  (175,300,379 each); an intermediate build of the same branch measured
  175,083,770 (-0.001%). Register allocation moving with the unit
  layout (review D2a, `codegen-units`), as far as it was traced.

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

## ROM reads

The core reads the ROM as a plain `&[u8]`: loads of any width, at any
address. ADR 0027 (byte loads only, behind a `compiler_fence`, for the
FMC erratum ES0491 §2.7.3) was retired on 2026-11-24 because the ROM
memory is HyperRAM on OCTOSPI (ES0491 §2.8.4 Table 5: memory-mapped
HyperBus reads at any address and byte number). Undoing its code shapes,
each step measured on its own (2026-11-24, branch `m10-rom-wide`;
`.text` is thumbv7em, 113,154 B before):

| Step | Ir (`check-bench.sh`) | `.text` |
|---|---|---|
| VRAM DMA takes a ROM block as one `[u8; 16]` chunk of the bank window (was 16 byte loads) | `synth:worst-case` 278,145,324 -> 278,059,754 (-0.03%); others identical | -254 B |
| `Cart::rom_block` (inline) and `Cart::ram_block` instead of one `dma_block` | `synth:worst-case` -> 277,991,298 (-0.025%); others identical | 0 |
| MBC1M logo test as slice compares | identical | -32 B |
| `header` left to the inliner | identical | -114 B (`Emulator::load` 948 -> 892) |
| `rom::Rom` and its fence removed | identical | +72 B (`boot_logo` 264 -> 328, unrolled by two; `cart_info` +8) |

Result: 112,826 B (-328); `synth:worst-case` -0.055%, the other eight
lines identical (`synth:dma-storm` is OAM DMA from WRAM and has no ROM
block copy; only `synth:worst-case` runs VRAM DMA from ROM).

Measured and not taken:
- `Cart::read_rom` as `window.get(i).copied().unwrap_or(0xFF)` instead
  of compare, early return, index: cpu_instrs +0.06%, cgb_sound +0.04%,
  cgb-acid2 +0.05%, event-storm +1.71%, dma-storm +0.03%, halt-irq
  +0.47%, worst-case +0.95%, cgb-raster +1.12%, apu-writes +0.28%, and
  +52 B (`Cpu::run` +48). An `if i < len { window[i] } else { 0xFF }`
  expression measured the same as `get`. The fence itself cost nothing on
  this path: removing it alone left all nine counts unchanged.
- The VRAM DMA block written through an out pointer into its VRAM slot
  instead of returned by value: `synth:worst-case` +17,114 Ir (+0.006%),
  +40 B.

## Measured on a Cortex-M7 (NUCLEO-H755ZI-Q, NUCLEO-H753ZI)

Two boards, the same Cortex-M7 r1p1 core and the same `tools/h7-bench`
placements: a NUCLEO-H755ZI-Q at up to 392 MHz (2026-11-24, below), then
the Stage 2 dev board, a NUCLEO-H753ZI, at 64, 392, 480 (483.86 measured)
and 472 MHz (2026-11-25; 472 MHz is the current clock). The h7-bench README has every table, the 64 and 392 MHz H753
runs included.

### NUCLEO-H753ZI at 472 MHz (2026-11-25, current)

The Stage 2 clock (D11): PLL1 from the HSI, 12.8 MHz reference,
fractional x 73.75, 472 MHz nominal (473.62 MHz measured against the
LSE crystal; at most 479.1 MHz over the HSI's datasheet tolerance), VOS0,
flash 4 wait states, AXI 236 MHz. Images at b4d6c81 (core as at
7d4b026). All fifteen workloads, three placements; every line's calls,
frames, clocks and hash equal `expected-hashes.txt`. `flash` is code in
flash with data in DTCM; `ITCM` is the ITCM set minus the plain tile
path, as below. The `flash-axi` column and the ratio table are in the
h7-bench README, "NUCLEO-H753ZI at 472 MHz".

**Measured: cycles per frame at 472 MHz, mean / max over 120 calls
(cpu_instrs 600), and % of a 472 MHz frame (7,902,557 cycles):**

| Workload | flash | ITCM | % of a frame, flash | % of a frame, ITCM |
|---|---|---|---|---|
| `synth:worst-case` | 5,116,841 / 5,182,062 | 3,296,548 / 3,334,278 | 64.7% / 65.6% | **41.7% / 42.2%** |
| `synth:cgb-raster` | 2,880,291 / 2,925,168 | 2,605,630 / 2,644,564 | 36.4% / 37.0% | 33.0% / 33.5% |
| `synth:dmg-raster` | 2,359,427 / 2,390,740 | 1,924,667 / 1,947,063 | 29.9% / 30.3% | 24.4% / 24.6% |
| `synth:event-storm` | 2,123,478 / 2,140,134 | 2,016,303 / 2,031,279 | 26.9% / 27.1% | 25.5% / 25.7% |
| `blargg/cpu_instrs` | 1,540,351 / 2,146,528 | 1,295,514 / 1,767,935 | 19.5% / 27.2% | 16.4% / 22.4% |
| `synth:dma-storm` | 3,227,889 / 3,235,118 | 3,088,109 / 3,092,401 | 40.8% / 40.9% | 39.1% / 39.1% |
| `synth:halt-irq` | 753,776 / 756,908 | 658,298 / 660,286 | 9.5% / 9.6% | 8.3% / 8.4% |
| `synth:apu-writes` | 1,163,854 / 1,187,010 | 897,195 / 919,289 | 14.7% / 15.0% | 11.4% / 11.6% |
| `synth:oam-lines` | 1,679,072 / 1,698,584 | 1,447,995 / 1,463,898 | 21.2% / 21.5% | 18.3% / 18.5% |
| `synth:storm-control` | 2,521,886 / 2,546,334 | 2,248,086 / 2,266,601 | 31.9% / 32.2% | 28.4% / 28.7% |
| `synth:storm-apu` | 25,743,612 / 25,981,351 | 23,961,284 / 24,174,010 | 325.8% / 328.8% | 303.2% / 305.9% |
| `synth:storm-spsw` | 15,310,679 / 15,464,886 | 14,572,044 / 14,705,973 | 193.7% / 195.7% | 184.4% / 186.1% |
| `synth:storm-div` | 16,902,052 / 17,062,206 | 16,339,897 / 16,483,324 | 213.9% / 215.9% | 206.8% / 208.6% |
| `synth:storm-gdma` | 3,171,715 / 3,203,608 | 2,876,062 / 2,901,220 | 40.1% / 40.5% | 36.4% / 36.7% |
| `synth:storm-ppu` | 30,224,496 / 30,544,076 | 28,561,042 / 28,862,232 | 382.5% / 386.5% | 361.4% / 365.2% |

So at 472 MHz the worst case takes **83.4% / 84.4% of the 50% line**
(3,951,279 cycles) with the hot set in ITCM, and misses it from flash
(129.5% / 131.1%). The storms run 1.84 to 3.65 frame times per frame in
ITCM (`storm-ppu` 28.56 M / 28.86 M cycles, 365.2% at most).

- Against the 483.86 MHz run below (same core, same wait states), cycles
  per frame are 0.984x to 1.036x per workload and placement, not
  unchanged: 1.002x on the worst case in ITCM, 0.985x to 1.018x on
  three workloads in ITCM, up to 1.036x from flash. An integer-PLL 472 MHz
  ITCM build of the previous code differs from this one by 0.979x to
  1.013x at the same clock, so rebuilding the bench crate alone moves
  counts by up to about 2%: treat these figures as +-2%. The cause is
  code layout through the branch predictor (BTAC aliasing on code
  addresses, in flash and ITCM alike), separated in M6: see below.

**Layout noise and the core-first flash layout (ADR 0042, M6).** With
the branch predictor off (diagnostic only) a 32 B shift of flash or ITCM
code moved every workload by at most 0.11%; with it on, flash shifts of
8 to 1000 B moved the storms by 1.7 to 4.2% and ITCM shifts of 4 to 32 B
by 0.3 to 1.7%, while moving only `.rodata` moved at most 0.25% and a
reflash 0.03%. The bound workloads (worst-case, cgb-raster, cpu_instrs,
event-storm) moved by at most 0.61% under every perturbation. Alignment
flags did not help. The ITCM images now put gbc-core's flash code first,
at the fixed address 0x0801_0400 (`.text_core`, `tools/memory-x`), with
the image's and the lib's code after it, so a bench-code change moved
the counts by 0.38% at most instead of 3.29%. A core change still moves
the layout (about +-1.7% via ITCM, up to about +-4.2% on the storms; the
canonical statement, with the margin left for the +3% ratchet, is ADR 0042
"Amendments"), and a baseline must be built in the main checkout (the
bytes, not the addresses, depend on the path).

### LTO and opt-level (M6)

Measured 2026-11-26 on `bench-itcm-dtcm-pll480` (472 MHz, ROMs in
internal flash), each variant built from main at 82458a9 with only
`tools/h7-bench/Cargo.toml`'s `[profile.release]` changed (always
`codegen-units = 1` and the CodeGenPrepare flag of "Build flags"). Every
report passed `check-report.sh`: all 15 hash lines equal
`expected-hashes.txt`. Mean cycles per frame against the baseline (the
noise floor for a changed core-code layout is about +-1.7% on the bound
workloads and up to about +-4.2% on the storms; ADR 0042 "Amendments" has
the canonical statement; each variant is one build, so a delta inside
about +-1.7% on a bound workload is not evidence of a cause):

| Variant | `.itcm` B | `.text_core` B | `.bin` B | ITCM check | worst-case | cgb-raster | dmg-raster | event-storm | cpu_instrs | max \|delta\| of 15 |
|---|---:|---:|---:|---|---:|---:|---:|---:|---:|---|
| baseline (opt 3, no LTO) | 64,032 | 51,352 | 730,856 | OK | 3,306,187 | 2,638,603 | 1,922,353 | 2,036,204 | 1,296,155 | (0.03 reflash) |
| `lto = "fat"` * | 63,344 | 29,264 | 725,888 | cannot run | +2.25% | -0.22% | -0.86% | -0.67% | -0.25% | 4.15 (storm-div -) |
| `lto = "thin"` * | 64,152 | 32,168 | 731,784 | cannot run | -0.16% | +0.13% | +0.06% | -0.72% | -0.21% | 7.31 (storm-spsw +) |
| gbc-core `opt-level = 2` | 54,064 | 47,808 | 727,304 | OK | +0.11% | +4.35% | +1.70% | +2.77% | -0.11% | 4.35 (cgb-raster +) |
| same, no tier dropped | 60,600 | 41,288 | 720,792 | OK (`--drop ''`) | -0.60% | +2.08% | +0.91% | +2.63% | -0.24% | 3.70 (storm-control +) |
| gbc-core `opt-level = "s"` | 47,536 | 40,520 | 720,672 | OK | +10.56% | +22.62% | +25.42% | +13.50% | +12.74% | 28.72 (halt-irq +) |

Baseline max per frame on worst-case 3,343,564 (fat 3,421,235, thin
3,340,606, opt 2 3,347,062 / 3,323,074 without a drop, "s" 3,697,090).

\* **LTO does not link as the tree stands.** Under LTO gbc-core's rlib
holds bitcode, not objects, so `.text_core`'s file pattern
(`*libgbc_core-*.rlib:*(.text .text.*)`) and the `.rodata.cst*` rule of
`.dtcm_rodata` match nothing and the `.text_core` ASSERT fails the link.
The LTO rows were measured with an uncommitted rule matching by symbol
name instead (`*(.text._R*8gbc_core*)`, `*(.rodata.cst*)`). Then
`scripts/check-itcm-elf.sh` cannot run at all: it reads the function
sections of the gbc-core rlib whose crate hash the ELF carries, and the
LTO rlib has none, so it exits with an error (build.sh FAIL). By name,
no function that is in ITCM in the baseline ended up in flash in either
LTO image (only two panic thunks moved), so the placement itself held.

**Result: nothing adopted; the firmware profiles stay as they are.** Fat
LTO is 2.25% slower on worst-case (the gated figure), marginal against
the layout band (one build; the band is about +-1.7%, so a layout effect
is not excluded), and no bound workload gains beyond noise; thin LTO is within noise on the bound set
but 7.2-7.3% slower on storm-spsw and storm-ppu. gbc-core at
`opt-level = 2` frees 10 KB of ITCM, enough to keep the plain tile path
there too, but is slower either way on the bound set (cgb-raster +2.1 to +4.4%,
event-storm +2.6 to +2.8%; the smaller of these are near the layout band); `"s"` costs 10 to 29%. Making LTO usable
would also need `.text_core`, `.dtcm_rodata` and `check-itcm-elf.sh` to
match by symbol rather than by file, for no measured gain. No change is
recommended for the root workspace's release profile either: these are
firmware-link measurements, and none of them beat opt-level 3 without
LTO.

### `headless` image on the board (Stage 2 M1/M2, 472 MHz)

The `headless` images (`crates/gbc-nucleo-h753`, M6-target placement: the
ITCM set minus the plain tile path, data in DTCM), 600 frames, cycles per
`run_frame` from the image's `stats` line (DWT, 472 MHz nominal). Every
run's 602 `headless` lines equal the host's. "M1" is the M1 close run;
"internal" and "QSPI" are one build (0765d62) run back to back on
2026-11-25: the same ITCM set and size (64,080 B; its bytes differ, the flash
layout around it having moved), the ROM either in internal flash
(`.rodata`) or read through the memory-mapped QSPI window (0xEB at
59 MHz, cached; `headless-qspi.bin`, `docs/reference/loader.md`).

| ROM | M1, internal flash | internal flash | QSPI | QSPI vs internal | QSPI, % of a frame |
|---|---|---|---|---|---|
| `cpu_instrs` | 1,296,098 / 1,768,873 | 1,294,935 / 1,767,664 | 1,297,900 / 1,769,785 | +0.23% / +0.12% | 16.42% / 22.39% |
| `cgb-acid2` | 684,112 / 756,317 | 683,081 / 755,648 | 693,063 / 759,572 | +1.46% / +0.52% | 8.77% / 9.61% |

(mean / max; a frame is 7,902,557 cycles, the 50% line 3,951,279.) The
worst frame from QSPI is 44.8% of the 50% line (cpu_instrs). The two
images differ in flash code layout as well as in where the ROM is, and a
rebuild alone moves these counts by up to about 2% (above), so the QSPI
cost here is "within about 1.5%", not a measured miss cost; the
bench placement `itcm-dtcm-qspi` (plan M2) is the controlled figure.
`exit_fallbacks=0`: the image never leaves memory-mapped mode.

### QSPI ROM placement (M2, `itcm-dtcm-qspi`, 472 MHz)

Plan Stage 2 M2: the bench workloads with their ROMs read through the
memory-mapped QSPI NOR (0xEB quad I/O at 59 MHz, window cached;
`docs/reference/qspi-nor.md`) instead of internal flash, the conservative
proxy for the Stage 3 HyperRAM. Method (tools/h7-bench README "QSPI ROM
placement"): the fifteen ROMs go into the NOR as one *bench pack* (one ADR
0037 image, `docs/reference/loader.md`); `bench-itcm-dtcm-pll480` (built at
7458dc5) runs every workload on its ROM in internal flash and then on the
pack's copy, in the same boot, through one instance of the frame loop, so
the ROM pointer is the only difference and the comparison is free of the
+-2% rebuild noise. Every line of both passes equals `expected-hashes.txt`
(calls, frames, clocks, hash), the pack's ROMs equal the embedded ones
byte for byte, and the banner's image CRC is the `.bin`'s
(`check-report.sh`); `exit_fallbacks=0`. Three boots: boots 1 and 2
printed the same sixty figures to the cycle, boot 3 is within 0.054% of
them.

**Measured: cycles per frame at 472 MHz, mean / max over 120 calls
(cpu_instrs 600), boot 1; % of a 472 MHz frame (7,902,557 cycles) from
QSPI:**

| Workload | internal flash | QSPI | QSPI vs internal | % of a frame, QSPI |
|---|---|---|---|---|
| `synth:worst-case` | 3,300,028 / 3,338,391 | 3,378,396 / 3,417,095 | +2.37% / +2.36% | **42.8% / 43.2%** |
| `synth:cgb-raster` | 2,608,239 / 2,647,364 | 2,611,818 / 2,653,032 | +0.14% / +0.21% | 33.1% / 33.6% |
| `synth:dmg-raster` | 1,920,125 / 1,942,291 | 1,923,155 / 1,947,229 | +0.16% / +0.25% | 24.3% / 24.6% |
| `synth:event-storm` | 2,028,773 / 2,042,705 | 2,032,042 / 2,048,093 | +0.16% / +0.26% | 25.7% / 25.9% |
| `blargg/cpu_instrs` | 1,296,003 / 1,768,804 | 1,296,953 / 1,768,804 | +0.07% / +0.00% | 16.4% / 22.4% |
| `synth:dma-storm` | 3,071,740 / 3,077,003 | 3,075,984 / 3,081,493 | +0.14% / +0.15% | 38.9% / 39.0% |
| `synth:halt-irq` | 656,766 / 658,741 | 659,184 / 662,248 | +0.37% / +0.53% | 8.3% / 8.4% |
| `synth:apu-writes` | 895,837 / 918,106 | 897,533 / 920,678 | +0.19% / +0.28% | 11.4% / 11.7% |
| `synth:oam-lines` | 1,446,752 / 1,462,811 | 1,448,369 / 1,464,718 | +0.11% / +0.13% | 18.3% / 18.5% |
| `synth:storm-control` | 2,242,987 / 2,261,261 | 2,246,694 / 2,265,580 | +0.17% / +0.19% | 28.4% / 28.7% |
| `synth:storm-apu` | 23,962,863 / 24,175,615 | 23,963,759 / 24,176,415 | +0.00% / +0.00% | 303.2% / 305.9% |
| `synth:storm-spsw` | 14,468,245 / 14,632,669 | 14,466,317 / 14,618,911 | -0.01% / -0.09% | 183.1% / 185.0% |
| `synth:storm-div` | 16,462,228 / 16,606,192 | 16,462,989 / 16,606,915 | +0.00% / +0.00% | 208.3% / 210.1% |
| `synth:storm-gdma` | 2,905,185 / 2,930,139 | 3,317,581 / 3,756,955 | +14.20% / +28.22% | 42.0% / 47.5% |
| `synth:storm-ppu` | 28,474,636 / 28,772,245 | 28,475,119 / 28,775,851 | +0.00% / +0.01% | 360.3% / 364.1% |

So with its ROM in QSPI the worst case takes **85.5% / 86.5% of the 50%
line** (3,951,279 cycles; report only), against 83.5% / 84.5% from
internal flash in the same boot, and 43.2% of a real-time frame at most:
nowhere near a full frame, so the M6 ROM-placement task stays optional.

- The QSPI cost is the ROM's cache misses: 0.1-0.4% where the program
  runs from a small ROM working set, +2.4% on the worst case (its HBlank
  HDMA copies 2 KiB from ROM every frame) and +14% / +28% on
  `storm-gdma` (outside the bound), whose GDMA source walks the whole
  address space, about 13 KB of ROM a frame, more than the 16 KiB D-cache
  keeps between frames. At ~770 cycles per random 32-byte miss
  (`qspi-nor.md`) those are about 100 and 540 misses a frame (estimates
  from that figure, not counted). A ROM-heavy game (large streamed
  graphics or PCM) will sit between the two. The plan takes this NOR as the
  conservative proxy for the Stage 3 HyperRAM (its miss latency is not
  measured yet).
- Against the `headless-qspi` figures above (different builds, +0.23% /
  +1.46%), this is the controlled figure: cpu_instrs +0.07% here.
- The internal-flash column is within -1.1% to +2.7% of the previous
  `itcm-dtcm` run (h7-bench README "After the switch onto the lib";
  +0.12% on the worst case), the usual rebuild band.

### QSPI tuning (M6, `itcm-dtcm-qspi`, 472 MHz)

Plan Stage 2 M6 "QSPI tuning". What the NOR and the STM32 allow, with
sources: `docs/reference/qspi-nor.md` "Tuning (M6)". In short: 0xEB at
CR.DC = 0 goes to 104 MHz; DTR and QPI don't exist on the MX25L6433F;
10 dummy cycles (133 MHz) would need a status-register write every boot
(rejected); HCLK3 with an even divider gives only 59 or 118 MHz, so the
kernel clock moved to PLL2 (200 MHz, /2 = 100 MHz). Adopted: **100 MHz
plus SIOO with the 0xA5 enhance-mode byte**.

Miss cost (`qspi-mmap`, DWT cycles, median of 64) and the bench's
within-boot QSPI-vs-internal figures (`bench-itcm-dtcm-pll480`,
`run-qspi.sh`, every run `check-report.sh` OK: 30 hashes, pack ROMs
equal the embedded ones, `exit_fallbacks=0`), mean / max:

| Setting | cold line miss | worst-case | storm-gdma | cgb-raster | dmg-raster | event-storm | cpu_instrs | pack validate |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 59 MHz, mode 0xFF (M2 setting, rebuilt at main 9ab334d) | 770 | +2.38% / +2.42% | +14.38% / +28.54% | | | | | 8,408,308 |
| 100 MHz, mode 0xFF | 463 | +1.41% / +1.39% | +8.67% / +17.27% | +0.08% / +0.13% | +0.09% / +0.18% | +0.09% / +0.15% | +0.04% / +0.00% | 4,961,980 |
| **100 MHz, SIOO + 0xA5 (adopted)** | **419** | **+1.23% / +1.27%** | **+7.56% / +15.04%** | +0.07% / +0.12% | +0.08% / +0.12% | +0.08% / +0.13% | +0.03% / +0.00% | 4,961,520 |

The adopted row is run 2 of three boots of one binary. Across the three,
the worst case's QSPI mean was 3,345,137 / 3,345,076 / 3,345,059 (0.002%
spread), storm-gdma's 3,089,295 / 3,089,346 / 3,089,402. The other bound
workloads moved by at most 0.01 points. The rest stay at 0.0-0.2% (halt-irq
+0.16-0.18%). `storm-spsw`'s internal figure itself jumped between boots
(14.89M vs 14.99M, both passes alike), as in earlier runs, so it says
nothing about the QSPI.

- A random miss costs 46% fewer cycles than at 59 MHz (770 to 419). The
  QSPI overhead on the worst case roughly halves (+2.38% to +1.23%), and
  on storm-gdma it does too (+14.4% to +7.6% mean, +28.5% to +15.0% max).
  The worst case's QSPI max is 3,384,337 cycles, 85.7% of 3,951,279.
- SIOO adds about 0.2 points on the worst case and 1.1 on storm-gdma
  over the clock alone, for a lib-side cost: `Qspi::init` and
  `Mapped::exit` take the NOR out of enhance mode. Images built before
  this change panic in `init` ("QUADSPI busy after the ES0392 2.7.4
  sequence") once a current image has run, whatever the NOR's mode: the
  M6 review (T8) reproduced it with the NOR in normal mode, so enhance
  mode is not the cause; the mechanism is unknown (`qspi-nor.md` "Old
  images panic at boot").
- Also run on this build: `run-replay.sh` on cgb-acid2 (600 frames,
  `player-script`): PASS, hashes equal, 0 over budget; the `player` image
  with Pokemon Red (reloaded, hash 0x0bfa5d71919acbbc) for 60 s: 0 over
  budget, 0 display errors, core max 1,348,228 cycles.

**Clock after the M6 review (T6).** PLL2 now runs from a 12.8 MHz input
(DIVM2 5 x DIVN2 62 / DIVR2 4 = 198.4 MHz; the 16 MHz input of the first
set sat on the PLL's maximum), so memory-mapped reads run at 99.2 MHz,
and CSHT is sized for the fastest HSI (6 cycles, was 5). `qspi-mmap`:
cold 32-byte line 429 cycles median (was 419), cold u32 200 (194), 64 KiB
copy 47.3 MB/s (47.7). `bench-itcm-dtcm-pll480` with the pack in the NOR
(one boot, worktree build; check-report and check-cycles OK, the last
two of 78 reports agree): worst-case QSPI +1.26% mean / +1.34% max over
internal (was +1.23% mean), storm-gdma +7.73% / +15.39% (was +7.56% /
+15.04%), every other workload at most +0.18%; against
expected-cycles.txt the QSPI lines moved -0.05% to +0.21% mean (storm-gdma
+0.18% mean, +0.35% max), all within the +3% ratchet.

### M6 worst-case report (h7-bench ratchet measurement; shipped-image figure below)

Plan Stage 2 M6 "Report". `bench-itcm-dtcm-pll480` (the core placement
of the shipped image: ADR 0042 core-first layout, the ITCM tier default with the
plain tile path dropped, QSPI 100 MHz + SIOO, no LTO, opt-level
unchanged) built in the main checkout at main a6a2995, NUCLEO-H753ZI at
472 MHz, bench pack in the NOR, two independent boots of one `.bin`
(re-flashed in between), 2026-11-26. Both captures: `check-report.sh
--qspi` OK (30 hashes equal `expected-hashes.txt`, `same_as_flash=1`,
`qspi_hz=100000000`, `exit_fallbacks=0`). Boot 1 is the committed
baseline (`tools/h7-bench/expected-cycles.txt`); boot 2 passes
`check-cycles.sh` against it.

**`synth:worst-case`, ROMs in QSPI (boot 1; boot 2 in brackets):**

| | cycles per frame | % of 3,951,279 (50% line, report only) | % of 7,902,557 (real-time frame) |
|---|---:|---:|---:|
| mean | 3,345,102 (3,345,049) | 84.66% | 42.33% |
| max | 3,382,280 (3,383,692) | 85.60% (85.64%) | 42.80% (42.82%) |

The worst frame is 7.17 ms of a 16.74 ms frame. With ROMs in internal
flash the same boot gives 3,304,254 / 3,342,450 (83.6% / 84.6% of the
50% line): the QSPI costs +1.24% / +1.19%. The 50% figure is not met
on the H753 at 472 MHz (85.6% of it) and is reported, not gated (plan
M6). Real time is met with 57% of the frame to spare.

**What this is, and the shipped-image figure (M6 review P5).** The bench
image keeps `FRAME` in cached AXI SRAM and has no front end; the shipped
`player` keeps `FRAME` in DTCM (ADR 0026 2026-11-26 amendment). So the
figures in this section are the ratchet's controlled measurement, not
literally the shipped placement. The plan's M6 acceptance wording
("measured on the board with ROMs in QSPI and the shipped placement") is
met by the scripted `player-script` run of `synth:worst-case` with the ROM
in the NOR, 10 minutes, 0 frames over budget
(`docs/reference/m6-evidence.md`): core max 3,390,856 (42.9% of 7,902,557;
85.8% of 3,951,279), mean 3,350,505, busy max 3,846,037. The live `player`
(`gbc-pc play`, same ROM, 10 minutes) read core max 3,385,726, busy max
3,902,609.

**H735 at 550 MHz: a projection, not a measurement.** 50% line at
550 MHz = 550e6 / 59.7275 / 2 = 4,604,244 cycles (a frame 9,208,489).
Assuming the same cycle count as measured here (cycle counts did not
change with frequency between 64, 392 and 480 MHz on the H753), the
worst-case max 3,382,280 is **73.5% of the 50% line** (36.7% of a
frame), mean 72.7%. What this leaves out: the H735 puts all of
gbc-core in its 128 KiB ITCM (the plain tile path and the other dropped
tiers too, so it should be faster than this); the Stage 3 ROM sits in
HyperRAM over OCTOSPI, whose miss latency differs from this NOR's (419
cycles a cold line here); and no front-end cost is in these figures.

**Baseline (boot 1), cycles per frame, mean / max:**

| Workload | internal flash | QSPI | QSPI vs internal |
|---|---:|---:|---:|
| worst-case | 3,304,254 / 3,342,450 | 3,345,102 / 3,382,280 | +1.24% / +1.19% |
| cgb-raster | 2,635,867 / 2,675,407 | 2,637,614 / 2,677,953 | +0.07% / +0.10% |
| dmg-raster | 1,919,939 / 1,942,189 | 1,921,382 / 1,944,534 | +0.08% / +0.12% |
| event-storm | 2,033,459 / 2,047,485 | 2,035,011 / 2,049,749 | +0.08% / +0.11% |
| cpu_instrs | 1,292,293 / 1,766,469 | 1,292,701 / 1,766,469 | +0.03% / +0.00% |
| dma-storm | 3,109,414 / 3,116,796 | 3,111,419 / 3,119,581 | +0.06% / +0.09% |
| halt-irq | 658,721 / 661,370 | 659,848 / 663,419 | +0.17% / +0.31% |
| apu-writes | 897,106 / 919,017 | 897,965 / 919,759 | +0.10% / +0.08% |
| oam-lines | 1,447,372 / 1,463,731 | 1,448,065 / 1,464,072 | +0.05% / +0.02% |
| storm-control | 2,244,315 / 2,262,568 | 2,246,140 / 2,264,959 | +0.08% / +0.11% |
| storm-apu | 23,871,038 / 24,082,808 | 23,871,482 / 24,083,232 | +0.00% / +0.00% |
| storm-spsw | 14,892,990 / 15,028,915 | 14,891,391 / 15,027,829 | -0.01% / -0.01% |
| storm-div | 16,727,299 / 16,873,763 | 16,727,675 / 16,874,179 | +0.00% / +0.00% |
| storm-gdma | 2,872,118 / 2,897,745 | 3,089,290 / 3,332,817 | +7.56% / +15.01% |
| storm-ppu | 28,678,019 / 28,977,195 | 28,678,407 / 28,978,104 | +0.00% / +0.00% |

Boot to boot (all 60 mean and max figures): largest |delta| 0.047%
(`qspi apu-writes` max, 430 cycles); worst-case QSPI max +0.04% (1,412
cycles), every mean within 0.03%. That is the reflash spread only, not the
margin of the +3% ratchet: a core layout change moves the bound
workloads by up to about +-1.7% and `storm-spsw` moved 0.67% between two
boots of one binary (ADR 0042 "Amendments", the canonical statement; no
gate step runs `check-cycles.sh` yet, gate B2 is M7). The storms (`storm-*`) are outside the bound by human
decision; `storm-gdma` is the one QSPI-bound workload (+15% max), still
under half a frame.

After the runs the board went back to the `player` image (same commit)
with Pokemon Red (loader hash 0x0bfa5d71919acbbc): 30 s, frames
advancing, 0 over budget, 0 display errors, core max 1,350,123 cycles,
busy max 1,804,527.

### Display in the loop (M3, `headless-display`, 472 MHz)

Plan Stage 2 M3 "Measure": the `headless` loop on `cgb-acid2`, 600
frames, with the panel running (`headless-display-cgb-acid2`,
`--features display`, src/bin/headless.rs `display`). Per completed
frame, outside the core's cycle count (the `FrameSink` hook,
`gbc_firmware::headless`): wait for the previous transfer, read the
window back and compare (RAMRD), `scale_frame` the new frame into the
AXI buffer, queue it (`present`). `GBC_DISPLAY_START` picks the start:
`te-wait` (TE, and the sink waits until TE has started it, so the 8.1 ms
transfer of frame n always runs while the core emulates frame n + 1),
`te` (TE, the sink returns at once: the overlap depends on where TE
falls), `now` (started by the sink, no TE), `off` (no display; the same
code: the images differ only in about 300 bytes of `main` around the
mode constant, every other function and the 64,080-B ITCM image are
byte-identical). One build (6d696e9), one boot each, 2026-11-25; every
run's 602 `headless` lines equal `tools/headless/expected/cgb-acid2.txt`
(run-headless.sh). The `run_frame` figures are DWT counts around each
`run_frame` call (the run's `stats run_frame` line).

| Image / mode | core cycles per frame, mean / max | vs `off` |
|---|---|---|
| display build, `off` | 681,007 / 753,470 | |
| display build, `te-wait` | 681,453 / 753,464 | +0.07% / -0.00% |
| display build, `te` | 681,315 / 753,422 | +0.05% / -0.01% |
| display build, `now` | 681,324 / 753,464 | +0.05% / -0.00% |
| plain `headless-cgb-acid2`, same tree | 683,879 / 756,185 | +0.42% / +0.36% (a different build) |
| M2 (0765d62), internal flash | 683,081 / 755,648 | |
| M1 close | 684,112 / 756,317 | |

**The display costs the core nothing measurable:** +0.07% at most with
the transfer overlapping every frame, at the level of boot-to-boot noise
(M2 above: 0.054%); against M2's figure the display run is -0.24% /
-0.29% (a different build: the usual layout band). The worst frame with
the display is 9.53% of a 472 MHz frame. The core runs from ITCM and
DTCM with the ROM and frame buffer cached; the MDMA moves 12.8 MB/s from
AXI SRAM to the FMC, a small share of the AXI bus, and the core does not
touch the FMC.

The CPU's display work per frame (587 frames with `FRAME_READY` out of
600 calls; plan M3 acceptance: conversion plus DMA setup at most 5% of a
frame, 395,127 cycles at 472 MHz; the plan's "about 402k" is 5% at 480
MHz):

| | cycles (min / mean / max; min and max over the three display modes) |
|---|---|
| `scale_frame` (flash; frame in cached AXI SRAM, output in the non-cacheable buffer) | 195,927 / 196,671-196,682 / 197,247 (415-418 µs) |
| `present` (window commands, 8 MDMA registers), `te`/`te-wait` | 298 / 335 / 386 |
| `present` + `start_now`, `now` | 399 / 430 / 512 |
| **sum of the maxima** | **197,759 = 2.50% of a frame** (50% of the budget) |

`scale_frame` is about 10% slower here than in `lcd-dma-probe`
(178-183k), whose frame is in DTCM; the headless image keeps the frame
in AXI SRAM (`.axisram.frame`, cached).

The transfer: 3,822,714 to 3,822,792 cycles from SWRQ to CTCIF (8,099
µs) in every mode, the same as the idle probe's: the core running beside
it does not slow the MDMA either (in all three modes the sink was
already waiting when it finished, `late=0`, so the figure is exact). TE
to SWRQ 59 to 88 cycles. 0 transfer errors.

The image rebuilt at 6af43aa (one panic-location byte different: a doc
comment moved a line in `lcd_dma.rs`), `te-wait`, run once more: lines
equal, 587 readbacks, 0 mismatches, control 2, core 681,404 / 753,518,
`scale_frame` + `present` at most 197,375, the transfer 3,822,714 to
3,822,740.

`dmg-acid2` (`headless-display-dmg-acid2`, `te-wait`, the same build):
its 602 lines equal the host tool's (`tools/headless`, 600 frames; no
committed expected file, so a scratch file), 592 frames, 0 mismatching
pixels, control 2; core 683,114 / 752,341; `scale_frame` 196,253 to
197,112, `present` 297 to 386, the transfer 3,822,714 to 3,822,740.

Not part of a real frame loop, measured for scale: the readback of the
window (51,840 pixels, 155,520 bytes) is 42.63 M cycles, 90.3 ms (274
cycles per byte); the whole self-test loop takes 100 ms a frame (six TE
periods), so 600 frames take about 59 s. In `te` mode the loop locks to
that phase: TE came on average 0.77 M cycles after `present`, after the
core's frame (0.68 M) had mostly finished, so `te` overlaps little; that
is why `te-wait` is the measured mode.

**Stricter self-test (M3 review T-1, T-2; branch `m3-review-fixes`).**
Before, the GRAM was cleared only at init, and 586 of cgb-acid2's 600
frames equal the one before, so a dropped, short or misplaced transfer
left the right picture and read back 0 mismatches: with one transfer cut
to half its length (BNDT halved, the 301st `present`) the old self-test
still reported `mismatches=0`. Now, between `scale_frame` and `present`,
the CPU writes the bitwise inverse of the frame about to be sent into the
window (`lcd_dma::write_inverse`; every readback byte of an inverted pixel
differs under the 0xFC mask, a const assertion over all 65,536 values), so
every pixel the transfer misses reads back wrong: the same cut transfer
gave `mismatches=25920` (exactly half the window, first at row 108) and
run-headless.sh exit 1. The sentinel takes 3.83 M cycles (8.1 ms, the
FMC's write speed, as the MDMA) plus a 20 µs drain, timed on its own
(`stats display waits ... sentinel_*`) and kept out of `setup` and the 5%
figure (with a DSB alone `present` measured 1,586 cycles, the write FIFO
still emptying). And run-headless.sh now checks the verdict for a display
image (detected from the file name, the `stats display start=` text in
the .bin, or a `stats display` line; any one): exactly one
`stats display start=` and one `stats display cpu` line, `frames`,
`transfers` and `readbacks` each equal to the compared lines' FRAME_READY
count, `mismatches=0`, `control_mismatches=2`, `transfer_errors=0`,
`start_timeouts=0`, `dead=0`, `ok=1`; otherwise exit 1 (so an `off`
image now exits 1 too). Runs with it (`te-wait`, 2026-11-25): cgb-acid2
587 readbacks, 0 mismatches, control 2, core 680,822 / 753,476,
`scale_frame` + `present` at most 196,480 (`present` 292 to 368);
dmg-acid2 592 readbacks, 0 mismatches, core 683,981 / 752,329. The
readback is unchanged (42.63 M cycles); the loop is about 8 ms a frame
longer (run_ms 68,856 instead of 59,070).

### Paced loop (M4)

Plan Stage 2 M4 acceptance: the audio-paced main loop
(`gbc_firmware::paced`) on the board with the panel and the DAC on, the
`paced-<rom>` images (src/bin/paced.rs, `--features paced`, the
`itcm-dtcm` placement of the headless images: `.itcm` 64,080 B, 1,200 B
free, check-itcm-elf.sh OK; the front end in flash). 35,837 frames
(10 minutes) each, 2026-11-26, commit 6485873, checked by
`run-paced.sh` (exit 0 for both): the `paced v1/end/drain` lines equal
the host's `headless <rom> --frames 35837 --paced` byte for byte (ROM
fnv, emulated clock, frame-hash chain, DAC-feed hash at the producer and,
at the ISR, as the ISR wrote it into the DMA half buffer: the consumer
side, not a read of the DAC output). Evidence: docs/reference/m4-evidence.md.

Wiring: the DMA ISR pops the ring through the `FeedTap`; `Idle` is `WFI`
with DBGMCU_CR DBGSLEEP_D1 set (without it the cycle counter stops in
`WFI`: 163 counts per TE period instead of 7.9 M, measured at every
boot, docs/reference/audio-dac.md); the display sink scales and presents
a frame only if the previous transfer has finished, else skips it; the
run starts four refills before the end of the DAC's start ramp.

| | `cgb_sound` | `synth:worst-case` |
|---|---|---|
| core cycles per call, mean / max | 741,536 / 2,458,648 (frame 54) | 3,325,532 / 3,347,461 |
| as a share of a 472 MHz frame (7,902,556) | 9.4% / 31.1% | 42.1% / 42.4% |
| ring push (conditioning + hash), mean / max | 34,236 / 35,549 | 35,162 / 35,707 |
| frame hash (`fbs`, per `FRAME_READY`), mean / max | 184,926 / 186,306 | 184,920 / 186,058 |
| display sink, mean / max (skips included) | 139,505 / 195,265 | 148,663 / 204,706 |
| `scale_frame` / `present` mean (presented frames) | 193,289 / 339 | 202,312 / 362 |
| busy per frame (sum of the means), idle share | 1,100,203, **86.1% idle** | 3,694,277, **53.3% idle** |
| `WFI` calls (run / drain) | 107,720 / 10 | 70,871 / 8 |
| ring fill min / max (of 2,048) | 1,039 / 2,047 | 1,243 / 2,047 |
| underruns / overruns during the run | 0 / 0 | 0 / 0 |
| `ppm` (whole run) / `steady_ppm` | +82 / +9 | +72 / +18 |
| display presented / skipped | 25,788 / 10,046 (28.0%) | 26,231 / 9,605 (26.8%) |
| display transfer errors / stuck | 0 / 0 | 0 / 0 |
| DAC ISR cost min / max (pop + tap fold of 256 frames) | 552 / 8,110 | 556 / 8,290 |
| DAC errors, missed refills | 0 | 0 |

- **Speed.** `steady_ppm` (from the first `WFI`, the ring full at both
  ends) is the ÷7,202 divider's +20.6 ppm within the measurement (the
  fill at the two ends can differ by up to one call's 550 samples, 16.8 ms
  over 600 s: ±28 ppm). The whole-run `ppm` adds the initial fill
  (2,048 frames ahead, +104 ppm) less the wait before the first pop
  (about 30 ms, -50 ppm: five halves, 39 ms, from the run start, less the ~9 ms the `paced v1` line takes on the UART before the first call; paced.rs `RAMP_LEFT`). The refills against the
  cycle counter give 32,768.28 Hz (`dac_rate_millihz`, nominal 32,768.675;
  ±1 refill over the run is ±13 ppm). All against the HSI: the LSE puts
  the CPU at 473.6047 MHz during these runs, so against real time the
  emulator runs +3,400 ppm fast (STATUS "Needs human").
- **Display skips (front-end policy, report only).** About 27% of the
  frames find the previous transfer still pending or running: a frame
  presented at a random point of the panel's TE period waits up to
  16.7 ms for TE, then 8.1 ms for the transfer, so it is often not done
  when the next frame comes 16.7 ms later; the panel then shows about
  44 frames a second, in runs of every other frame as the two 60 Hz
  clocks drift. The core never waits for the panel.
- **The cost of the stand-ins.** The frame hash (`fbs`, 2.3% of a frame)
  and the tap's FNV fold in the ISR are acceptance instruments, not
  product cost; `scale_frame` (2.4 to 2.6%) is the display's.
- `cgb_sound`'s slowest call is frame 54, early in the ROM (31% of a
  frame), the reason for its lower `min_fill`.

#### After M4 review P-3/P-4/P-7 (2026-11-26, commit eb2e432)

Same two 10-minute runs, `run-paced.sh` exit 0 for both, the `paced`
lines equal the host's with the same hashes as above (cgb_sound `fbs`
ffe7f333d13ab726, feed a42b9bdd2e459ea5; worst-case 329470c6bc19a93f,
4b3d013ebbd78714). Changes: the display shows the newest frame at each TE
(a pending transfer is taken back and replaced; a frame that comes while
MDMA reads the buffer is copied aside and shown from the idle hook when
the transfer is done; ADR 0039 amendment), the idle hook re-checks the
wait condition with interrupts masked before `WFI` (P-4), and the DAC ISR
counts a half-late single-flag entry as `late` and skips the second
refill of the same half (P-3).

| | `cgb_sound` before → after | `synth:worst-case` before → after |
|---|---|---|
| display presented / skipped | 25,788 / 10,046 → **33,276 / 2,558 (7.1%)** | 26,231 / 9,605 → **33,117 / 2,719 (7.6%)** |
| TE edges | 35,920 → 35,916 | 35,915 → 35,923 |
| of the skips: replaced while pending / superseded while held | – → 2,558 / 0 | – → 2,198 / 521 |
| shown from the copy (`deferred`), copy cost mean / max | – → 16,410, 42,072 / 44,983 | – → 15,277, 42,802 / 45,085 |
| display sink mean / max | 139,505 / 195,265 → 124,983 / 196,648 | 148,663 / 204,706 → 132,738 / 206,934 |
| busy per frame (sum of the means), idle share | 1,100,203, 86.1% → 1,089,837, **86.2%** | 3,694,277, 53.3% → 3,673,387, **53.5%** |
| `ppm` / `steady_ppm` | +82 / +9 → +82 / +9 | +72 / +18 → +72 / +18 |
| underruns, DAC late / errors / restarts | 0, 0 / 0 / 0 → same | 0, 0 / 0 / 0 → same |

- The panel now shows about 55.6 new frames a second (presented over
  600 s) of the core's 59.73, and every frame the core makes in a TE
  period with no other one reaches the panel: the remaining skips are
  two frames completing within one TE period (`replaced`), which the
  loop's DMA-half grid makes common (frames 15.6 or 23.4 ms apart: two
  in one 16.7 ms TE period, then a TE period with none).
- The idle share leaves out the conversions of the copied frames, which
  run in the idle hook: 16,410 × 195,245 cycles over 35,837 frames is
  about 89k cycles per frame (1.1% of a frame), so about 85.0% and
  52.4% idle.
- P-3 forced on the board (a scratch build spinning once in the DAC ISR
  between the LISR and NDTR reads until the DMA wrapped, 1,800 frames):
  `late=1` and the pending second interrupt's refill skipped once; the
  same forcing on the old guard-less code: `late=0` with the half refilled
  twice (`run-paced.sh` would have passed it). Found on the way: `halt`
  clearing the stream's EN sets TCIF and enters the ISR once more with
  NDTR anywhere (seen 512 and 256); that entry (phase STOPPED) is not
  checked.

### NUCLEO-H753ZI at 480 MHz (2026-11-25)

STM32H753ZIT6 rev V on the LDO supply, so VOS0 and 480 MHz are allowed
(DS12117 Table 111); flash 4 wait states, AXI 240 MHz. Images at 7d4b026.
All fifteen h7-bench workloads, three placements; every line's calls,
frames, clocks and hash equal `expected-hashes.txt` (the PC and QEMU).
`flash` is code in flash with data in DTCM; `ITCM` is the ITCM set minus
the plain tile path, as below.

**Measured: cycles per frame at 480 MHz, mean / max over 120 calls
(cpu_instrs 600), and % of a 480 MHz frame (8,036,499 cycles):**

| Workload | flash | ITCM | % of a frame, flash | % of a frame, ITCM |
|---|---|---|---|---|
| `synth:worst-case` | 5,079,014 / 5,142,534 | 3,291,153 / 3,328,005 | 63.2% / 64.0% | **41.0% / 41.4%** |
| `synth:cgb-raster` | 2,894,227 / 2,942,521 | 2,607,183 / 2,646,327 | 36.0% / 36.6% | 32.4% / 32.9% |
| `synth:dmg-raster` | 2,335,260 / 2,367,324 | 1,927,981 / 1,950,733 | 29.1% / 29.5% | 24.0% / 24.3% |
| `synth:event-storm` | 2,127,321 / 2,143,998 | 2,014,882 / 2,028,825 | 26.5% / 26.7% | 25.1% / 25.2% |
| `blargg/cpu_instrs` | 1,531,800 / 2,137,526 | 1,294,743 / 1,767,346 | 19.1% / 26.6% | 16.1% / 22.0% |
| `synth:dma-storm` | 3,211,272 / 3,219,188 | 3,033,551 / 3,039,197 | 40.0% / 40.1% | 37.7% / 37.8% |
| `synth:halt-irq` | 739,531 / 742,716 | 656,976 / 658,854 | 9.2% / 9.2% | 8.2% / 8.2% |
| `synth:apu-writes` | 1,137,090 / 1,163,168 | 897,762 / 919,997 | 14.1% / 14.5% | 11.2% / 11.4% |
| `synth:oam-lines` | 1,673,067 / 1,691,083 | 1,447,229 / 1,463,331 | 20.8% / 21.0% | 18.0% / 18.2% |

So on the dev board at 480 MHz the worst case takes **81.9% / 82.8% of
the 50% bar** (4,018,249 cycles) with the hot set in ITCM, and misses it
from flash (126% / 128% of the bar). Nothing in the bound or in `BENCHES`
comes near a whole frame. The storms ("Outside the bound") are measured
there.

- ITCM costs the same cycles at 480 as at 392 MHz (ratio 1.000 to 1.003
  on the five workloads both ran) and at 64 MHz. Flash costs more: 4 wait
  states make a miss 10 CPU cycles against 6 at 392 MHz, and the flash
  placements take 1.00x to 1.11x the 392 MHz cycles (the worst case
  1.10x). ITCM against flash is now 1.54x on the worst case (1.36x to
  1.41x at 392 MHz), 1.06x to 1.27x on the rest.
- Against the H755 figures below (core bcd881e) the H753 worst case in
  ITCM is 6% dearer at any clock: the core's code changed between the two
  (not separated, the H755 is gone).
- Measured with the 480 MHz MCO clock, 483.9 MHz actual (the PC timed
  `selftest-pll480`'s DWT lines at 484.3 MHz, 0.9% fast; h7-bench README,
  "NUCLEO-H753ZI at 480 MHz"): the % above are against the nominal 480 MHz.
  Stage 2 now runs 472 MHz from the HSI (D11): frame 7,902,557 cycles, 50%
  line 3,951,279. Measured there (above), the counts are 0.984x to 1.036x
  of these, not unchanged; the 472 MHz section has the current figures.

### NUCLEO-H755ZI-Q (2026-11-24)

Measured 2026-11-24 with `tools/h7-bench` (its README has the method, the
board facts and all six result tables) on an STM32H755ZIT6: Cortex-M7 r1p1,
16 KiB I- and D-cache, 64 KiB ITCM, 128 KiB DTCM. CPU cycles from the DWT
cycle counter around each frame-sized `run_until` call, no interrupts;
every run reproduced the PC's checksums of frames and audio. Core at `main`
bcd881e. The board supplies VCORE from the SMPS, which limits it to VOS1
and 400 MHz (DS12919 Table 14), so 392 MHz is the fastest clock measured.

Placements: `flash` is all code in flash behind the I-cache (data in DTCM;
with the data in AXI SRAM the figures differ by under 3%); `ITCM` is the
set of `perf/itcm-sections.txt` in ITCM **minus the plain tile path**
(`Renderer::render`, `fill_tiles`), because the whole set no longer fits
64 KiB (below).

**Measured: cycles per frame at 392 MHz, mean / max over 120 calls
(cpu_instrs 600):**

| Workload | flash | ITCM | ITCM share of a frame at 392 MHz (mean / max) |
|---|---|---|---|
| `synth:worst-case` | 4,383,420 / 4,435,230 | 3,100,670 / 3,137,337 | 47.2% / 47.8% |
| `synth:cgb-raster` | 2,804,392 / 2,850,104 | 2,530,564 / 2,569,295 | 38.6% / 39.1% |
| `synth:dmg-raster` | 2,150,011 / 2,179,232 | 1,815,665 / 1,837,309 | 27.7% / 28.0% |
| `synth:event-storm` | 2,195,649 / 2,213,436 | 1,952,834 / 1,967,362 | 29.8% / 30.0% |
| `blargg/cpu_instrs` | 1,398,842 / 1,951,152 | 1,210,549 / 1,683,911 | 18.4% / 25.7% |

A frame is 6,563,140 cycles at 392 MHz (59.7275 Hz). So the worst case
meets the 50% bar on this board at 392 MHz with the hot set in ITCM, by
2.2 points at its slowest frame, and misses it from flash (66.8% / 67.6%).
At the 64 MHz HSI the ITCM placement costs the same cycles within 0.3%
(worst case 3,093,762 / 3,130,103); the flash placement costs up to 3% fewer
there (the AXI clock is then the CPU clock).

**x86 Ir against M7 cycles.** Ir per frame is the baseline's Ir
(`perf/bench-baseline.txt` at 0270845) divided by the frames the bench
counts in the same emulated seconds (118, 118, 117, 119; 1,191 for
cpu_instrs, which the bench runs for 20 s while the board ran its first
10 s, so that row compares different windows):

| Workload | Ir / frame | cycles / Ir, ITCM | cycles / Ir, flash |
|---|---|---|---|
| `synth:worst-case` | 2,423,362 | 1.28 | 1.81 |
| `synth:cgb-raster` | 2,263,989 | 1.12 | 1.24 |
| `synth:dmg-raster` | 1,577,114 | 1.15 | 1.36 |
| `synth:event-storm` | 1,471,309 | 1.33 | 1.49 |
| `blargg/cpu_instrs` | 1,061,445 | 1.14 | 1.32 |

The proxy "one x86 instruction is one M7 cycle" was optimistic by 12 to
33% with the hot code in ITCM and by 24 to 81% from flash. The "4.02 M
line" in this file is 50% of a frame at 480 MHz in *cycles*; in Ir the
same bar is about 3.1 M Ir/frame for the worst case at this ratio (4.02 M /
1.28), which the baseline's 2.42 M meets.

**Extrapolated to 550 MHz, not measured** (the H753's measured ITCM cycle
counts at 480 MHz, above, over a 550 MHz frame of 9,208,488 cycles,
assuming the cycle count does not change with frequency, as it did not
between 64, 392 and 480 MHz). The 480 MHz column this table used to
extrapolate from the H755 (worst case 39.0% at most) is replaced by the
measurement above (41.4%; the core changed in between):

| Workload (ITCM, max) | 480 MHz (H753, measured) | 550 MHz |
|---|---|---|
| `synth:worst-case` | 41.4% | 36.1% |
| `synth:cgb-raster` | 32.9% | 28.7% |
| `synth:dmg-raster` | 24.3% | 21.2% |
| `synth:event-storm` | 25.2% | 22.0% |
| `blargg/cpu_instrs` | 22.0% | 19.2% |

What the extrapolation leaves out: 550 MHz is the STM32H735, a different
chip, where all of gbc-core is in a 128 KiB ITCM (so the plain tile path
is too, which should make it faster than these figures, not slower). No
front-end work (LCD, audio, input) is in any of these numbers.

**The ITCM set does not fit 64 KiB.** In the firmware ELF its functions
are 68,832 B (`check-itcm-set.sh` reports 69,078 B on the rlib, against a
57,344 B budget); the 55 KB figures under "Dev board: NUCLEO-H753ZI" below
are older than the current set. `tools/h7-bench` drops the
tier the list names first, the plain tile path (8,078 B), and then fills
64,664 of the 65,280 B it uses with 61,284 B of gbc-core, the memory
routines and 89 long-branch thunks: nothing is left for front-end ISRs. A
Stage 2 front end on an H753/H755 must drop more.

## Player (M5)

The `player` image (`docs/reference/player.md`): the paced loop with the
ROM in the QSPI NOR, the display (newest frame at TE), the DAC, live
input and the link session, the M6-target ITCM set (`check-itcm-elf.sh`
OK: 64,080 B of 65,280 B) and, in this image, the frame buffer in DTCM.
Its statistics (every 300 frames, `gbc_firmware::player`): `core` is the
`run_frame` call alone; `busy` is the whole loop iteration awake (one
service-hook call to the next minus the cycles asleep in `WFI`: link,
input, core, audio push, frame hash, display hand-off, idle-hook checks,
interrupts); **over budget** = `busy` > 7,902,557 cycles (1/59.7275 s at
472 MHz). Measured 2026-11-26 at 472 MHz, no input (title and attract
screens), branch `m5-player` (commits f3187ad, 82449fb):

| Run | Frames | core mean | core p99 (max window) | core max | front end mean / max | busy max | slack min | over budget | underruns | presented / skipped |
|---|---|---|---|---|---|---|---|---|---|---|
| cgb-acid2, 20 s via the wrapper | 2,461 | 683,666 | 688,876 | 757,929 | 415,185 / 455,465 | 1,156,129 (14.6%) | 6,762,972 | 0 | 0 | 2,364 / 84 |
| Wario Land 3, 150 s | 11,071 | 1,790,006 (heaviest window) | 1,859,948 | 1,897,911 (24.0%) | 443,761 / 500,382 | 2,350,158 (29.7%) | 5,592,506 | 0 | 0 | 10,339 / 649 |

Percentages are of the 7,902,557-cycle budget. The front end (about
0.41-0.50 M cycles, 0.9-1.1 ms per frame) in these runs was mostly the
frame hash (`fbs`, about 185 k cycles per completed frame, "Paced loop
(M4)") and the 1.5x scaler into the panel buffer (about 195 k per
presented frame), then the audio push (about 35 k); the live image no
longer hashes (below). `skipped` frames are superseded
before the panel's TE took them (the panel runs at about 60 Hz against
59.73 Hz). The first Wario Land 3 run had 5 underruns (1,280 frames),
all at the start: the run hashes the 2 MiB ROM from the NOR for its
header line before the first frame, longer than the paced image's
4-refill margin before the DAC's start ramp ends; live mode now starts 24
refills (187 ms) before the end (commit 82449fb), and the second run
above had 0.

Scripted mode (`player-script`, `GBC_HEADLESS_FRAMES=2400`, no input,
cgb_sound in the NOR; `run-paced.sh out/player-script.bin cgb_sound.gb`,
`RUN_PACED_FRAMES=2400`): **PASS**, the three `paced` lines equal the
host's `headless --paced` (fbs `bf833017780c5a60`, feed
`a47140640767c765`, 1,316,677 DAC frames), ring underruns 0 / overruns
0 (fill 1,243..2,047), speed +856 ppm (steady -238), display 2,194
presented / 203 skipped, 0 transfer errors, DAC no errors. Core min /
mean / max 672,430 / 959,192 / 2,460,484 (frame 54); `stats player`
busy max 2,903,956 (36.7%), over budget 0. (Its `underruns=6
underrun_frames=1467` are since boot and include the drain's last pop and
the DAC stop ramp after the run, which pop an empty ring; the run's own
figure is `stats paced ring`: 0.)

**No hashing in the live image (M7 review, branch `m7-fe-perf`).** The
live `player` only discards its `paced` lines, so it no longer computes
the frame hash and the DAC feed hash (`paced::Service::HASHES` false
without `player-script`; the ISR pops with `FeedTap::pop_unhashed`). The
scripted image (`player-script`, gate B3/B4) and every other image hash
as before. Pokemon Red, no input, 80 s via `play-on-board.sh`, 16 Stats
windows each, same ROM and NOR, 472 MHz, 2026-11-26:

| Image | fe mean (mean of windows) | fe_mean range | fe max | busy max (last window) | core max | over budget / underruns / dac_restarts |
|---|---|---|---|---|---|---|
| main (383ed4f), hashing | 427,482 | 410,983-444,657 | 516,128 | 1,770,366 | 1,341,699 | 0 / 0 / 0 |
| no hashing in live mode | 244,589 | 217,603-253,741 | 321,577 | 1,605,867 | 1,349,528 | 0 / 0 / 0 |

The front end drops by about 183 k cycles a frame (2.3% of the budget),
the frame hash's measured cost; what is left is mostly the scaler.

### Display stash cache attribute (M7 P2)

M7 review P2 estimated (no measurement) that the display stash, the
46,080 B copy of a frame taken while MDMA still reads the panel buffer
(`display.rs`, `.axisram.stash`, region 4's Normal write-back
write-allocate), might evict the core's QSPI ROM and cart-RAM lines from
the 16 KiB D-cache on deferred frames. Measured 2026-11-26, 472 MHz,
`player` (live), Wario Land 3 (2 MiB, MBC5) in the NOR, no input, 120 s
via `play-on-board.sh --seconds 120`, each run freshly flashed. A
temporary (uncommitted) build added a split of the core cycles: frames
whose previous iteration stashed ("after") against the rest, and the
variants re-attributed the stash on a spare MPU region (12, 64 KiB at
0x2401_0000 with subregions 0, 6 and 7 disabled, so 40 of the stash's
45 KiB, 0x2401_2000-0x2401_C000; the rest of it shares 8 KiB subregions
with `AUDIO` and `CART_RAM`). Since-boot figures from the last `Stats`
line with `total_frames` <= 7,500 (7,270-7,365 frames); fe from the 19-20
windows of the session:

| Variant | core mean | core max | after: n / mean | other: n / mean | fe mean | fe max | busy max | deferred / skipped |
|---|---|---|---|---|---|---|---|---|
| WBWA (main), run 1 | 1,260,983 | 1,886,378 | 3,386 / 1,222,835 | 3,979 / 1,293,446 | 231,208 | 330,076 | 2,139,309 | 3,386 / 613 |
| WBWA (main), run 2 | 1,264,834 | 1,877,559 | 3,095 / 1,333,571 | 4,188 / 1,214,037 | 230,492 | 331,956 | 2,126,960 | 3,095 / 315 |
| WB, no write-allocate | 1,264,547 | 1,887,287 | 2,829 / 1,276,803 | 4,441 / 1,256,741 | 228,979 | 330,382 | 2,140,712 | 2,830 / 525 |
| non-cacheable | 1,254,400 | 1,875,456 | 3,377 / 1,250,691 | 3,936 / 1,257,584 | 308,608 | 499,985 | 2,286,072 | 3,378 / 467 |

All runs: 0 underruns, 0 over budget, 0 display errors, 0 DAC restarts.
Noise: the two identical baselines differ by 3.9 k (0.3%) in core mean,
9 k in core max and 13 k in busy max. The after/other split carries no
signal: which frames are deferred depends on the TE phase, so each
bucket holds different game content, and its sign flips between the two
baselines (-71 k, +120 k); a stash eviction cost would show as "after"
consistently above "other" in every run.

Decision: **keep write-back write-allocate**. No write-allocate is
identical to the baseline within noise. Non-cacheable saves about 8.5 k
(0.7%) of core mean, about two noise widths, but costs +78 k of fe mean
and +168 k of fe max (the copy into, and the scaler's reads out of,
uncached AXI SRAM) and +150 k of busy max, so the loop is worse overall.
The estimate's 220 k a deferred frame does not happen; in this image the
frame buffer is in DTCM, so the stash is the only frame-sized AXI
traffic, and whatever it evicts is refetched at a cost below the noise
(not broken down further). A full-size mapping would also need a layout
change: AXI SRAM has about 750 B left before the 128 KiB-aligned panel
buffer moves up and `.fault_stack` overflows (a 64 KiB-aligned stash
overflowed it by 2,048 B).

### Session 30 review fixes (scaler, DAC ISR)

`docs/reviews/stage2-comprehensive.md` P1 (scaler two pixels per u32, no
stack row, f4b11d1) and P5 (DAC ISR: neutral `PcmFrame` ring, the 12-bit
conversion fused into the pop, ea6e9dc/4ac67b1; P4 ba6b626 and ADR 0044
with it). Measured 2026-11-27, HEAD e8b8efd, 472 MHz, NUCLEO-H753ZI.

**Scaler, isolated** (`headless-display-cgb-acid2`, `te-wait`, 587
`FRAME_READY` frames of 600; `cd crates/gbc-nucleo-h753 && ./build.sh
headless-display-cgb-acid2 && RUN_HEADLESS_READ_SECS=120
./run-headless.sh out/headless-display-cgb-acid2.bin
../../tools/headless/expected/cgb-acid2.txt`: EQUAL, 602 lines, display
self-test passed, 0 mismatches, 0 transfer errors):

| `stats display cpu` | before ("Display in the loop (M3)") | after | change |
|---|---|---|---|
| `scale_frame` min / mean / max | 195,927 / 196,671-196,682 / 197,247 | 117,510 / 118,674 / 120,169 | -78.0 k mean (-39.7%) |
| `present` (setup) min / mean / max | 298 / 335 / 386 | 302 / 319 / 348 | noise |
| worst_sum | 197,759 (2.50% of a frame) | 120,517 (1.53%) | |
| `run_frame` mean / max | 681,453 / 753,464 | 682,443 / 753,509 | +0.15% / +0.01% (another build) |

The review estimated 85-100 k; the measured 118.7 k is above it, still
-40%.

**Live `player`** (no input, `scripts/play-on-board.sh <copy of the ROM>
--seconds N [--audio]`; fe = mean of the windows' `fe_mean`, fe max and
busy max = largest window value, core max = `total_core_max`; ISR = the
range of the windows' `audio_isr_max`). Pokemon Red 80 s, freshly loaded
and flashed, 16 windows, against "No hashing in the live image":

| Pokemon Red, 80 s | before (no audio) | after, no audio | after, `--audio` (next run, same boot) |
|---|---|---|---|
| fe mean | 244,589 | 167,126 (-77.5 k, -31.7%) | 231,321 |
| fe_mean range | 217,603-253,741 | 160,302-175,748 | 185,832-238,185 |
| fe max | 321,577 | 247,554 (-74.0 k) | 323,048 |
| busy max (last window) | 1,605,867 | 1,518,411 (-87 k) | 1,590,351 |
| core max | 1,349,528 | 1,372,254 (+22.7 k, +1.7%) | 1,356,540 (window max) |
| core mean (mean of windows) | - | 880,410 | 879,994 |
| `audio_isr_max` | 8,356 off / 10,496 tee on (ADR 0041, this game's intro) | 2,726-2,802 (-66%) | 5,208-5,290 (-50%) |
| `audio_cyc_per_frame` (tee) | 107 (ADR 0041) | - | 116 |
| over budget / underruns / dac_restarts / display_errors | 0 / 0 / 0 / - | 0 / 0 / 0 / 0 | 0 / 0 / 0 / 0 |
| presented / skipped (since boot) | - | 5,958 / 189 | 11,877 / 191 |
| audio dropped / timeouts | - | - | 0 / 0 (PC: 0 underruns, 0 gaps) |
| stack_used | 12,968 (M5) | 13,088 / 22,912 | 13,088 |

Wario Land 3 120 s, freshly loaded and flashed, 24 windows, against the
M7 P2 WBWA runs 1 / 2 (their core mean was a temporary build's since-boot
figure, not reproducible from `Stats`):

| Wario Land 3, 120 s | before (WBWA runs 1 / 2) | after, no audio | after, `--audio` (next run, same boot) |
|---|---|---|---|
| fe mean | 231,208 / 230,492 | 166,785 (-64 k, -27.8%) | 233,829 |
| fe max | 330,076 / 331,956 | 247,162 | 321,692 |
| busy max | 2,139,309 / 2,126,960 | 2,062,144 (-3.3%) | 2,142,274 |
| core max | 1,886,378 / 1,877,559 | 1,886,758 | 1,876,027 (window max) |
| core mean (mean of windows) | - | 1,120,298 | 1,229,879 |
| `audio_isr_max` | - | 2,767-2,936 | 5,230-5,410 |
| skipped (presented), since boot | 613 / 315 | 343 (8,147) | 877 (16,966) |
| over budget / underruns / dac_restarts | 0 / 0 / 0 | 0 / 0 / 0 | 0 / 0 / 0; audio dropped 0, 117 cyc/frame |

Reading: the scaler's -78 k per presented frame shows as -64 to -77 k of
live fe mean (about 95-97% of frames are presented); fe max drops by
74-85 k. The DAC ISR's worst case falls by 5.2-5.6 k cycles per half
(`audio_dac::HALF` = 256 frames, about 2.1 halves per video frame, so
about 11-12 k cycles per video frame, counted in `core`). The
tee's main-loop pack/encode is 116-117 cycles per frame against ADR 0041's
107 (+9, about 5 k per video frame while streaming: it now converts from
`PcmFrame`), less than the ISR saves. Core max: Wario Land 3 equal; Pokemon
Red +1.7% (one frame since boot; neither change adds work to `core`, the
ISR part shrank, so a layout/single-frame effect, not checked further).
Stack +120 B against the M5 figure (other changes since M5 too). The
`--audio` columns are the second run of the same boot (no reflash, later
game content), so they are not a clean audio-on/off pair.

#### Session 31: stash swap (P2), stats cost (M4)

`docs/reviews/stage2-comprehensive.md` P2 (16f88a3: a deferred frame is
kept by swapping the loop's frame buffer with the display stash,
`paced::SwapSink`, instead of the 46,080 B `copy_from_slice`) and M4
(bd38750: `stats_max`, `fe_max_ex_stats`). Measured 2026-11-27, A/B of
the `player` image at ce95a80 (before) and 16f88a3 (after, with M4), 472
MHz, NUCLEO-H753ZI. Two runs each, alternated (before, after, before,
after for Wario Land 3 then Pokemon Red, each freshly flashed: loader
flashed first, so play-on-board never took its fast path), no input,
no audio, same method as above (fe = mean of the windows' `fe_mean`;
fe/busy max = largest window value; core mean = mean of the windows'
`core_mean`; core max = `total_core_max`; presented/skipped since boot).

With the swap the core draws into either buffer: the `player` frame in
DTCM `.bss` or the stash in AXI SRAM (NORMAL_WBWA), whichever the last
deferred frame left it.

| Wario Land 3, 120 s | before 1 / 2 | after 1 / 2 | change (means) |
|---|---|---|---|
| fe mean | 170,406 / 166,593 | 153,555 / 154,639 | -14.4 k (-8.6%) |
| fe max | 248,491 / 248,316 | 234,374 / 234,561 | -14.0 k |
| busy max | 2,034,244 / 2,061,514 | 2,028,419 / 2,046,866 | -10 k |
| core mean | 1,090,132 / 1,117,357 | 1,123,573 / 1,121,162 | +1.7% (before runs differ by 2.5%) |
| core max | 1,874,976 / 1,886,596 | 1,878,688 / 1,887,843 | +0.1% |
| presented / skipped | 8,412 / 303, 8,226 / 297 | 7,654 / 847, 7,965 / 536 | see below |
| `stats_max` / `fe_max_ex_stats` (largest) | - | 71,628 / 163,643, 71,668 / 163,602 | |

| Pokemon Red, 80 s | before 1 / 2 | after 1 / 2 | change (means) |
|---|---|---|---|
| fe mean | 168,896 / 169,205 | 155,068 / 155,144 | -13.9 k (-8.2%) |
| fe max | 248,571 / 249,196 | 232,477 / 235,193 | -15.0 k |
| busy max | 1,547,420 / 1,547,571 | 1,525,894 / 1,539,749 | -15 k |
| core mean | 882,627 / 885,607 | 881,721 / 883,622 | -0.2% |
| core max | 1,371,892 / 1,372,384 | 1,376,342 / 1,373,115 | +0.2% |
| presented / skipped | 5,936 / 198, 5,532 / 579 | 5,677 / 485, 5,888 / 244 | see below |
| `stats_max` / `fe_max_ex_stats` (largest) | - | 72,090 / 163,807, 71,905 / 163,929 | |

All eight runs: 0 underruns, 0 over budget, 0 display errors;
stack_used 13,120 before, 13,144 after (of 22,912). `.text` of `player`
+312 B, `.itcm` 64,088 B unchanged, ITCM checks OK.

Reading: the swap takes 4-9 cycles (`copy_mean` of the scripted
`stats display paced` line in the replay runs, max 29) instead of
42-45 k; fe mean falls by 14 k, close to the review's ~20 k estimate
for about 46% of frames (deferred share of presented frames in the
replays: 13-86%
by ROM). The core cost of drawing into AXI half the time is not seen:
Pokemon Red -0.2%, Wario Land 3's two before runs are 27 k (2.5%) apart
and the after runs sit beside the second (+0.3% against it), core max
equal. Kept. Skipped frames vary from run to run with either image
(Pokemon Red before 198 and 579; Session 30 Wario Land 3: 315-613);
Wario Land 3's after runs (847, 536) are on the high side, which a few
runs cannot separate from that spread: nothing in the change alters
when a frame is deferred or superseded, only the 43 k cycles it no
longer spends. Watch it in the next live measurements.

**M4:** the report frame (`queue_stats`: about 0.9 kB of fields, u64
formatting, COBS/CRC) costs 71-72 k cycles, and it was every window's
`fe_max`: `fe_max` 232-235 k against `fe_max_ex_stats` 162-164 k. Its
cycles stay in `busy`, `fe` and `over_budget` (the gates' check).

**Replay set** (`cd crates/gbc-nucleo-h753 && ./run-replay.sh`, gate
B3's invocation, at 16f88a3's tree): all six PASS equal=yes,
underruns 0, over budget 0, mismatches 0 (cpu_instrs core_max 1,773,152,
readbacks 12; cgb-acid2 756,851, 2; dmg-acid2 753,593, 2; cgb_sound
2,468,349, 8; hdma_mode0 1,052,557, 2; rtc3test-sub-second 1,446,131,
6, input 6/6, late 0). Host (gate H5): regenerated lists equal
`tools/headless/expected/` (and the replay lists), no diff.

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

*Note 2026-11-26 (M6 review P8):* the "AXI left" column is for the Stage 2
plan's 231,424 B of buffers and does not hold for the real front end. The
shipped `player` uses 394,240 B of AXI SRAM (ADR 0026 2026-11-26 amendment,
open item), above the 262,144 B of AXI at 128 KiB of ITCM. The Stage 3
front end needs its own AXI budget, and the 128 KiB ITCM recommendation
below must be re-checked against it. The analysis here is kept as written.

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
line stays as the stricter figure (the H753 now runs 472 MHz, whose 50% is 3.95 M cycles, D11); no Ir gate changes.

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
(Since then the set has outgrown the budget: 69,078 B in 53 globs on
2026-11-24, 11,734 B over 57,344 B and over the 64 KiB ITCM itself.
`check-itcm-set.sh` prints the current figures; "Measured on a Cortex-M7"
above has what the H755 build did about it. The table and the placement
below are the 2026-11-24 morning state and are kept for the tiers and the
method, not for the sizes.)

**The ITCM set** (`perf/itcm-sections.txt`, 63 globs, 68 functions at the
time of the table), by tier:

| Tier | Contents | Size (B) |
|---|---|---|
| A: per instruction/access | `Cpu::run`, `execute_cb`, `checked_fetch`; `read_high`, `read_vram`, `read_oam`, `write_mapped`, `write_io`, `write_vram`, `write_oam`, `sync_ppu`, `boundary`, `run_ppu_to_call`; `Ppu::cpu_blocked`; `Cart::{read_ram, read_rtc}`; `halt_begin/end`, `idle_halted`; `Schedule::set`, `Timer::schedule_overflow` | 36,834 |
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
   (`read/write_during_dma`, `dma_sync`, `vdma_turn`, `vdma_arm_at`).
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
DMA1/2): the stack, `Hw`/`Emulator` (848 B when this plan was written;
952 B at the end of 2026-11-24), `Memory` (50,968 B then, 50,984 B now: WRAM,
VRAM, OAM) and the read-only tables `SPREAD` 1,024 B, `ATTR_FETCH` 1,024 B
and `POP8` 256 B (2,304 B, copied from flash at boot like `.itcm`). Frame
and audio buffers go in AXI SRAM, because the LCD and SAI DMA must reach
them. Cart RAM goes in AXI SRAM too (it can be up to 128 KB with MBC5).
`check-code-size.sh` prints both sizes for thumbv7em. The same split
applies on the H735 (above).

#### ITCM tier A/B (M6)

The default drop, the plain tile path, was never measured against the
alternative of keeping it in ITCM. It cannot simply be added back:
`.itcm` is 64,032 B with it dropped (h7-bench; 64,088 B in `player`), so
it fits only if a different tier of at least about 6,830 B leaves instead.
The tier sizes in the h7-bench ELF (FUNC symbols matched by the globs,
2026-11-26) are: A 39,094 B; B per event 7,402 B; B plain tile path
8,078 B; B per line 3,196 B; C APU 9,556 B; D 844 B. So the per-line
tier the plan suggested (3,196 B), or per line plus D (4,040 B), does not
make room. Two single-tier swaps do:

| Build (`H7_BENCH_ITCM_DROP`) | `.itcm` | `check-itcm-elf.sh` |
|---|---|---|
| default: `plain tile path` | 64,032 B | OK |
| `cheap, per-event` (Tier B per event in flash, plain tile path in ITCM) | 64,632 B | OK |
| `APU, per sync` (Tier C in flash, plain tile path in ITCM) | 62,544 B | OK (\*) |

(\*) Only 39 globs are kept with this drop, and `tools/memory-x`'s
`MIN_GLOBS` guard (40) refuses it. It was lowered to 30 for this
measurement only and then reverted.

Measured on `bench-itcm-dtcm-pll480` (472 MHz, internal-flash ROMs), built
in one worktree path. Each capture passed `check-report.sh` (15 lines equal
to `expected-hashes.txt`). Two runs of the default image agreed within
0.01%, so run-to-run noise is small, but a core layout change moves bound
workloads by up to about +-1.7% (ADR 0042 "Amendments"), and each variant
is one build. Deltas against the default, mean / max:

| Workload | drop per-event | drop APU |
|---|---|---|
| `synth:worst-case` | +1.42% / +1.42% | +2.35% / +2.46% |
| `synth:cgb-raster` | -0.21% / -0.22% | -0.07% / -0.07% |
| `synth:dmg-raster` | +2.55% / +2.56% | +0.34% / +0.33% |
| `synth:event-storm` | +1.14% / +1.13% | +1.86% / +1.86% |
| `blargg/cpu_instrs` | +0.64% / +0.50% | +0.40% / +0.32% |
| max \|delta\| over 15 | 4.97% (storm-div, faster) | 2.46% (worst-case max) |

Neither swap is faster on the bound set. Both read slower on `worst-case`
and `event-storm`, and the per-event swap on `dmg-raster` as well, but only
the APU swap's +2.35% worst-case clearly exceeds the +-1.7% layout band; the
per-event swap's +1.42% is inside it and may be layout, not the swap. The workload that should gain most from the plain tile path,
`cgb-raster`, the closest to cgb-acid2-like content, gains at most 0.2%,
which is noise. The plain tile path runs once per line per run of plain
tiles and stays warm in the 16 KiB I-cache, while the per-event PPU
functions and the APU set run at every event or sync. **The default stays
`plain tile path`** for both h7-bench and gbc-nucleo-h753. The `player`
image built from main runs Pokemon Red with 0 over budget (busy max
1.83 M cycles, 23% of the budget). The earlier figure "no tier dropped,
opt-level 2: +2.1% cgb-raster" ("LTO and opt-level (M6)") measured a
different code generation, not this placement.

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
    /* gbc-core's merged constant pools: read_high's IO read mask, the APU
       duty and volume tables (review C-P7; tools/memory-x). Its panic
       strings and Locations stay in flash. */
    *libgbc_core-*.rlib:*(.rodata.cst*)
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
- *Post-link (firmware ELF, Stage 2): `scripts/check-itcm-elf.sh
  [--drop <tiers>] [--rlib <file|dir>] <elf>`; h7-bench's `build.sh` runs
  it on every `itcm` image.* The functions a glob names are the sections of
  the gbc-core rlib the ELF was linked from that it matches (as the linker
  matches them, so a `#[cold]` `.text.unlikely.*` copy is never taken for a
  listed function), plus ELF symbols matching it that the rlib lacks. Fails
  (exit 1): a function of a kept tier that the link kept outside
  `[0x0000_0000, 0x0001_0000)`; a function of a `--drop` tier inside it
  (the drop list disagrees with the link); a glob matching no section of
  the rlib (stale); no `.itcm`, or one over 64 KiB. Reported only: a listed
  function the rlib has and the ELF lacks (unused and garbage-collected, or
  inlined under LTO; the plan's "that the link kept"), the dropped tiers,
  `.itcm` size against 64 KiB, and the long-branch thunks by direction
  (`__*LongThunk_*` with lld, `__*_veneer` with GNU ld). Exit 2: a tool
  error. The dropped tiers come from the caller (`--drop`, the syntax of
  `H7_BENCH_ITCM_DROP`; default none), not from the ELF: a check that
  inferred them from where the functions landed would pass any
  misplacement. The H735 version will check every gbc-core function
  against `[0x0000_0000, 0x0002_0000)`.

  Mutation-checked 2026-11-25 on `bench-itcm-dtcm-pll480`: built with
  `H7_BENCH_ITCM_DROP="plain tile path,frame driver"` and checked with
  `--drop "plain tile path"` exits 1 naming `Emulator::run` and
  `Emulator::run_until` in flash; a misspelt `Ppu::cpu_blocked` glob
  (`ITCM_SET_FILE`) exits 1 as stale; the normal build checked with an
  extra `--drop "frame driver"` exits 1; the normal build exits 0.

**Is a <= 40 KB target for `execute`/`execute_cb` still needed?** No. They
are 31,404 B now. With the interpreter in ITCM, its I-cache footprint (the
M2 "2.6x the I-cache" concern) no longer matters, because TCM fetches
bypass the cache. On the final chip the whole core is in ITCM and the
binding constraint is the total, 97,136 of 122,880 B, which
`check-code-size.sh` gates. On the H753 the set total (69,078 B against a
57,344 B budget on 2026-11-24: over) is reported, not gated.

## Build flags

Every build of the workspace gets one rustc flag beyond the profiles in
`Cargo.toml` (human, 2026-11-24; ADR 0008, amendment "the LLVM
address-folding limit, applied"):

```
-C llvm-args=-cgp-max-address-users-to-scan=100000
```

LLVM's CodeGenPrepare stops folding an address into its memory accesses
once it has more than 100 uses in one function (default of this hidden
option); `Cpu::run` has far more on the `Ctx` fields, and paid a reload
per access. With the flag: cpu_instrs -6.70% Ir, cgb_sound -5.67%, every
bench line lower, thumb `.text` -1,130 B (ADR 0008 has the table).

Where it is set, and how it can't be lost unnoticed:

- `.cargo/config.toml`, `[build] rustflags`: the one definition. It covers
  every cargo build run inside the repository: tests, bench, romtest, the
  desktop front end (also the `x86_64-pc-windows-gnu` cross build of
  `scripts/run-windows.sh`), the gate's thumbv7em build and, later, the
  firmware. Keep it a one-line array.
- A `RUSTFLAGS` (or `CARGO_ENCODED_RUSTFLAGS`) variable *replaces* the
  config's flags. The scripts that set their own (`check-code-size.sh`,
  `check-itcm-set.sh`: `-C target-cpu=cortex-m7`)
  source `scripts/lib/rustflags.sh`, which reads the config line and adds
  to it, so the two cannot drift; it fails (exit 1) if the line is
  missing or lacks the option.
- `scripts/check-bench.sh` asks the binary it is about to measure what it
  was built with (`bench --rustflags`, recorded by `tools/bench/build.rs`
  from `CARGO_ENCODED_RUSTFLAGS`) and fails unless the config's flags are
  among them: a `RUSTFLAGS` left in the environment would otherwise
  produce another build's numbers.
- Not covered, on purpose: `cargo fuzz` (`tools/fuzz/fuzz`, nightly) sets
  `RUSTFLAGS` itself for its sanitizer and coverage flags, which replaces
  the config. It is a bug-finding build on another LLVM, never measured.
  `scripts/diff-fingerprint.sh` builds the old revision with that
  revision's own config (before 2026-11-24: without the flag); the
  fingerprints must match either way, since the flag changes no behaviour.

**On a toolchain bump** (`rust-toolchain.toml`): the option belongs to the
pinned LLVM. If LLVM drops it, every build fails with an unknown-option
error; if it renames or changes it, builds pass and the numbers move. So
re-validate on the new toolchain: run `scripts/check-bench.sh` and
`scripts/check-code-size.sh` as committed, then again with the value in
`.cargo/config.toml` temporarily set to `=100`, LLVM's default (the
"without" build; the guards accept it, so nothing else needs editing).
Keep the flag only if it still lowers Ir and `.text`; record both sets of
numbers in ADR 0008 and restore `=100000`.

## Object-line summary (M10 review, 2026-11-24)

`Ppu::latch` skips the OAM scan of a line no object covers, from a
160-bit summary of OAM (`ObjectLines`, `ppu/mod.rs`) that every OAM
write (CPU, OAM DMA) makes stale. The rebuild at the next line set one
bit per object row: 40 objects, up to 16 rows each. A program that writes
OAM on every line (HBlank sprite multiplexing) paid it 144 times a frame,
and no bench line measured it. `synth:oam-lines` (above) does. Two
changes:

- **Word-wide build.** The bits are indexed by OAM Y (bit `ly + 16`), so
  an object is one bit at Y and the screen's top and bottom need no
  clipping; the five words are then smeared up by 1, 2, 4 (8 rows) and 8
  (16 rows). 40 single-bit ORs and 15 or 20 word steps instead of up to
  640 single-bit ORs.
- **One line's delay.** The first line after a write is scanned without
  the summary (the scan itself finds an empty line empty), and the bits
  are rebuilt at the next line only if OAM was not written in between. A
  program that writes OAM on every line never rebuilds; one that writes
  it once a frame (DMA in VBlank) pays one direct scan of a line that
  might have had no object.

Ir, each variant on the same base (2 s; 119.455 frames):

| Variant | `synth:oam-lines` | per frame | cgb-acid2 (20 s) | `synth:worst-case` |
|---|---|---|---|---|
| before (2821437 + the program) | 233,214,859 | 1.952 M | 649,967,201 | 286,911,384 |
| word-wide build | 152,119,282 | 1.273 M | 642,136,703 | 286,847,468 |
| and one line's delay | 145,605,876 | 1.219 M | 642,250,198 | 286,863,036 |

The build alone is 679k Ir a frame (4.7k per rebuild, more than the
review's 3.4k estimate), 17% of the 4.02 M line; the delay takes another
55k for 0.018% on cgb-acid2 and 0.005% on worst-case (one scan of a
line without the summary per OAM write that is followed by a clean line). Both stay: the delay
alone would leave a program that writes OAM every other line with 72 of
the old rebuilds a frame.

As landed (`build` out of line and compact, see the size below), every
line against `main` (6367b5b, with the ADR 0035 audio filter) measured
the same way:

| Line | main | with the change | delta |
|---|---|---|---|
| cpu_instrs.gb | 1,327,813,432 | 1,327,470,396 | -0.026% |
| cgb_sound.gb | 973,851,265 | 973,508,274 | -0.035% |
| cgb-acid2.gbc | 726,390,252 | 717,976,794 | -1.158% |
| synth:event-storm | 182,973,533 | 182,939,314 | -0.019% |
| synth:dma-storm | 393,398,153 | 393,269,184 | -0.033% |
| synth:halt-irq | 301,522,527 | 301,350,701 | -0.057% |
| synth:worst-case | 314,276,521 | 314,195,604 | -0.026% |
| synth:cgb-raster | 275,061,275 | 275,027,138 | -0.012% |
| synth:apu-writes | 103,388,853 | 103,354,637 | -0.033% |
| synth:dmg-raster | 193,438,927 | 193,358,653 | -0.041% |
| synth:oam-lines | 240,856,971 | 153,181,406 | -36.40% |

`synth:oam-lines` is 2.016 M -> **1.282 M Ir per frame** (-734k).

Size: total `.text` 115,940 -> 116,076 B (+136). Written the plain way
(a loop over a step table, inlined into `latch`) the build cost +1,004 B:
the 40-object loop and every smear step come out unrolled. What brought
it down: `build` out of line (`latch` 1,720 ->
1,406 B, `build` 466 B), the smear as one loop over the step with a
carry between words, eight local words so the object loop has no bounds
check, and the stale state as the all-zero value (`Ppu::reset` and
`Sys::new` clear the PPU in one block; with a non-zero stale state they
grew by 52 B). The object loop is still unrolled (306 B of it when
built as a function of its own).

Behaviour: `diff-fingerprint.sh` against the commit that adds the
program (audio buffer 7, samples hashed) is identical, the invariance
checks pass with the new program in their corpus, and romtest shows no
status change. A unit test compares `build` with the scan's own
per-line test (`LineObjects::covers`) for every Y alone, every Y with a
second object 0-40 rows below, and 2,000 pseudo-random OAMs, in both
heights, on lines 0-255.

## Instruction-count benchmark

`scripts/check-bench.sh` runs `tools/bench` (release build, one codegen
unit as pinned in `Cargo.toml`, the flags of "Build flags" above, which it
checks first) under
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
every program in `synth::NAMES`, the storms of "Outside the bound"
included, plus `worst-case@0/1/2/4/8`, all for N seconds), so an
exact APU optimization can be shown sample-identical: run it before and
after, at two buffer sizes (e.g. 7 and 64; the hashes do not depend on the
split), and `diff`. 4,039 runs (2026-11-24), about 11 s wall. Without the flag the
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
APU (samples included), the timer's lazy reloads (`Timer::catch_up`), the
serial port's lazy shifts (`Serial::catch_up`) and the cartridge RTC. The skips (HALT, STOP mode, the speed-switch halt, a
VRAM DMA stall) go one M-cycle at a time, with the same sync in each. It
sets no deadline and raises no event of its own; in particular the PPU
deadline is left where the lazy build put it (`run_ppu_to`, not
`sync_ppu_to`).

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
diagnosis; a listed ROM that stops differing fails the check too. None
today. The one there was,
`gambatte/lcd_offset/offset3_lyc99int_m2irq_count_1`, came from line 0's
single-speed mode-2 pulse: its IF request was a dot late only because the
PPU deadline was one past the walk point, so a sync on the point posted
it early. The request is now a timed one (`irq::PendingIrq`, ADR 0033),
posted at the point + 1 whenever the PPU is synced.

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

`bench --long [--minutes N] [--root DIR]` runs twelve workloads that keep
working (the eight synthetic programs, cgb-acid2, cpu_instrs, cgb_sound and
rtc3test) from power-on for 30 emulated minutes, 7,549,747,200 clocks,
past 2^32 (17 min 4 s), in frame-sized calls with an audio buffer. The
script builds it with the `checked` profile, no feature and exactly the
rustc flags of `.cargo/config.toml` (every build of the script is checked
with `bench --rustflags`: a RUSTFLAGS-style variable could turn on
`--cfg feature="eager-sync"` while cargo reports no feature). It fails on
a panic, a call that makes no progress, a sample count other than one per
128 clocks, or a frame that is not where one per 70,224 clocks puts it
(for cgb-acid2, cpu_instrs, cgb_sound and rtc3test, which turn the LCD off
during their setup, from the first frame after the last LCD-off gap,
below).

That frame check (`FrameCheck` in `tools/bench/src/long.rs`) counts from
the first completed frame, so setup with the LCD off costs nothing, but
the first frame must end its call within three frames of power-on
(210,672 clocks; seen: 70,060 to 194,844, `synth:dmg-raster` the latest).
Frame k's offset is `clock - (first + k * 70,224)`, checked as each frame
arrives (the failure names the frame, its clock, the due clock, the
offsets so far and the previous frame's clock), so a lost, extra or late
frame fails at once and nothing can drift. The core does not report the
boundary's own clock: the `FRAME_READY` call ends at the first point where
the CPU looks at the run's end, up to `OVERSHOOT` = 31 clocks after the
boundary (3 clocks of M-cycle rounding, then STOP mode's second wake-up
idle and a 6 M-cycle instruction; the derivation, with the functions, is
at the constant). Frame 0's own overshoot cancels in the spread: the
offsets must span at most 31 clocks, and (kept from the earlier check, so
that nothing it rejected passes) each must be within 23 of 0. Seen: spread
0 to 16. At the end the count must equal `1 + (last - first + 31) /
70,224`, the run must not reach the next frame's boundary (`first + n *
70,224`; frames that stop fail), and a workload with no frame fails.

The four ROM workloads that turn the LCD off while they set up
(`Lcd::SetupGaps`; checked since 2026-11-25, unchecked from fe99394 until
then) may have LCD-off gaps only before a per-workload setup deadline, and exactly
a per-workload number of them: a frame whose call ends more than 70,224 +
31 clocks after the previous one (or the first frame) restarts the check
from that frame, and must end its call by the deadline (inclusive), which
replaces the three-frame first-frame limit for them. Every other frame,
before the deadline too, and every frame after it, is checked by the rules
above; a gap after the deadline, no frame by it, or a gap count other than
the expected one (the first frame is not counted) fails. The deadline is
the measured re-anchor clock + 35,112 (half a frame), so no further frame
is due before it and a frame lost or shifted in the setup shows as a
different count, not as another gap. Measured (30 minutes, `checked`): cgb-acid2 0 gaps,
first frame at 959,452 (deadline 994,564); cpu_instrs 1 gap of 278,224
clocks, re-anchored at 348,288 (383,400); cgb_sound 1 of 278,212, at
348,276 (383,388); rtc3test 2, 299,672 then 99,424, at 469,156 (504,268).
Offsets after: 0..0, -10..11, -4..16, 0..0.
Before 2026-11-24 the check was a count within one of `clocks / 70,224`,
which failed dmg-raster's setup (two short) and hid a lost frame
elsewhere.

60.5 s wall for the ten before dmg-raster in parallel (2026-11-24, 8
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
cgb-raster, apu-writes and dmg-raster (added after the baseline below;
not measured in it). Test ROMs that finish early idle (halted or
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

## Outside the bound: register-write storms

Human decision (2026-11-23, session 10): the 50% bar (README) covers
realistic worst cases, which `synth:worst-case` models (all paths at once,
with one mid-mode-3 SCX write per line and a 16 kHz sample player).
Programs that write expensive registers many times per line are outside
it. This section says how far outside, measured (M10 review, 2026-11-24;
the section used to name only mid-mode-3 PPU writes, with a table from
M8).

**What the benched worst case does and does not bound.** The emulator's
cost per frame is not bounded by the frame: most of it is per *access*.
`synth:worst-case` fixes the access counts at what a demanding game does
(per frame: 144 mid-mode-3 SCX writes, 414 APU writes, 274 timer and 146
STAT interrupts, 144 HDMA blocks, a serial transfer and timer overflows
back to back). It does not bound a program that spends the whole frame
writing one costly register: the SM83 can write an I/O register every 2
M-cycles, 17,154 times a frame in double speed, and several registers
cost 400 to 1,200 Ir per write here. Nothing breaks or crashes in that
case; the frame just costs more host time.

**Method.** `synth:storm-*` (`tools/bench/src/synth.rs`, "Storms"): each
runs one kind of access in the tightest loop there is (`LD (C),A` and `LD
(HL),B` alternating, two values, 32 pairs and a `JR`: 64 writes per 131
M-cycles) for the whole frame, in double speed, IME off. All share one
background: the picture of `synth:worst-case` (BG, window, 10 objects on
64 lines) and its four sound channels at their worst periods.
`synth:storm-control` is that background with the loop writing HRAM, so a
storm's own cost is its figure minus the control's; "Ir per event" below
is that difference over the events per frame (net of the HRAM write each
one displaces). Ir is measured as `scripts/check-bench.sh` does (same
build and flags check, callgrind, `--toggle-collect='*run_emulation*'`),
2 emulated seconds, divided by the frames the bench counts (118, 119 for
`storm-spsw`), as in "Measured on a Cortex-M7". Core at 86d01ef (`main`,
2026-11-24), programs at e6aa427. The storms are
**not** in `BENCHES` or `perf/bench-baseline.txt`: they are measurements
of what is outside the bar, and a gate on them would be a gate on
adversarial input. They are in `synth::NAMES`, so `bench --fingerprint
--hash-samples` and `scripts/check-sync-invariance.sh` run them (6 more
runs; all identical under `eager-sync` and the four clock offsets when
added).

**Measured (x86 Ir), with the M7 share as an estimate.** "x line" is Ir
per frame over the 4.02 M line. The last two columns are **estimates, not
measurements**: Ir per frame times the 1.12 to 1.33 M7 cycles per Ir
measured for other workloads with the hot set in ITCM ("Measured on a
Cortex-M7"), over a frame's cycles at 392 MHz (6,563,140; the NUCLEO) and
550 MHz (9,208,488; the STM32H735). No storm has run on the board, and
their mix of code (and so the ratio) may differ from the measured
workloads'. Over 100% means slower than real time by that factor.
They have since run on the board: see "Measured on the M7" after the
table.

| Program | What the loop does, per frame | Ir (2 s) | M Ir / frame | x line | Ir per event | est. % of a frame, 392 MHz | est. %, 550 MHz |
|---|---|---:|---:|---:|---:|---:|---:|
| `synth:worst-case` | (reference: the benched bound) | 314,195,642 | 2.66 | 0.66 | - | 45-54% (the board measured 47.8% at bcd881e, when this was 2.42 M Ir) | 32-38% |
| `synth:storm-control` | 17,154 HRAM writes | 234,571,753 | 1.99 | 0.49 | - | 34-40% | 24-29% |
| `synth:storm-apu` | 17,154 NR32 writes (a) | 2,361,761,435 | 20.01 | 4.98 | 1,051 | 342-406% | 243-289% |
| `synth:storm-spsw` | 6,769 speed switches (b) | 1,013,637,629 | 8.52 | 2.12 | 965 | 145-173% | 104-123% |
| `synth:storm-div` | 17,154 DIV writes (c) | 1,139,033,945 | 9.65 | 2.40 | 447 | 165-196% | 117-139% |
| `synth:storm-gdma` | 1,665 one-block GDMAs (d) | 276,970,510 | 2.35 | 0.58 | 216 | 40-48% | 29-34% |
| `synth:storm-ppu` | 17,154 LCDC writes (e) | 2,597,774,395 | 22.02 | 5.48 | 1,167 | 376-446% | 268-318% |

**Measured on the M7** (2026-11-25, NUCLEO-H753ZI at 480 MHz,
`tools/h7-bench` at 7d4b026, the default variant of each storm, 120
frame-sized calls; hashes equal the PC's). Cycles per frame with the hot
set in ITCM, mean / max; % of a 480 MHz frame (8,036,499) at most; the
550 MHz column scales the same cycles to the STM32H735's frame
(9,208,488), an extrapolation; cycles per Ir uses the M Ir / frame above,
measured on an older core (86d01ef), so it is approximate:

| Program | cycles / frame, ITCM | % of a frame, 480 MHz | est. %, 550 MHz | cycles / Ir | flash (data in DTCM) |
|---|---:|---:|---:|---:|---:|
| `synth:storm-control` | 2,243,418 / 2,261,753 | 28.1% | 24.6% | 1.13 | 2,504,796 / 2,528,921 |
| `synth:storm-apu` | 23,957,661 / 24,170,419 | 300.8% | 262.5% | 1.20 | 25,614,716 / 25,848,430 |
| `synth:storm-spsw` | 14,791,973 / 14,934,687 | 185.8% | 162.2% | 1.74 | 15,353,951 / 15,500,482 |
| `synth:storm-div` | 16,062,214 / 16,212,035 | 201.7% | 176.1% | 1.66 | 16,649,536 / 16,832,276 |
| `synth:storm-gdma` | 2,882,474 / 2,919,847 | 36.3% | 31.7% | 1.23 | 3,193,040 / 3,234,018 |
| `synth:storm-ppu` | 28,457,135 / 28,756,285 | 357.8% | 312.3% | 1.29 | 30,503,428 / 30,852,328 |

At the current 472 MHz clock (b4d6c81, "NUCLEO-H753ZI at 472 MHz"
above), ITCM, mean / max and % of a 472 MHz frame at most:
`storm-control` 2,248,086 / 2,266,601 (28.7%), `storm-apu` 23,961,284 /
24,174,010 (305.9%), `storm-spsw` 14,572,044 / 14,705,973 (186.1%),
`storm-div` 16,339,897 / 16,483,324 (208.6%), `storm-gdma` 2,876,062 /
2,901,220 (36.7%), `storm-ppu` 28,561,042 / 28,862,232 (365.2%).

So the dearest storm, `storm-ppu`, runs 3.6 frame times per frame on the
dev board and an estimated 3.1 on the final chip (438% of a 392 MHz frame, inside the 392 MHz
estimate's range above); `storm-spsw` and `storm-div` cost more cycles per
Ir (1.66 to 1.74) than any workload of the bound (1.12 to 1.33), so their
estimates were low. Recorded for context only: these are outside the bound
by decision (human, 2026-11-23).

So the most expensive input found costs **22.0 M Ir per frame, 5.5 times
the 4.02 M line and 8.3 times `synth:worst-case`**: on the M7 an estimated
3.8 to 4.5 frame times per frame at 392 MHz, 2.7 to 3.2 at 550 MHz. This
is the highest figure found, not a proven maximum: the cost of a mixed
loop is bounded by its write slots times the dearest write, so a mix of
these should not exceed the dearest single one by much, but that is an
argument, not a measurement.

Variants (`synth:<name>@<variant>`, same method; `:1x` is single speed,
8,577 writes a frame, against `synth:storm-control@hram:1x` at 1.48 M):

| Variant | Loop | M Ir / frame | x line | Ir per event |
|---|---|---:|---:|---:|
| `storm-apu@12` | NR12 (envelope), F7 / E7 | 20.34 | 5.06 | 1,070 |
| `storm-apu@22` | NR43 (noise rate) | 20.23 | 5.03 | 1,064 |
| `storm-apu@16` | NR21 (duty) | 19.97 | 4.97 | 1,048 |
| `storm-apu@30` | wave RAM FF30, CH3 playing | 19.86 | 4.94 | 1,042 |
| `storm-apu@27` | unused FF27 | 18.93 | 4.71 | 988 |
| `storm-apu@24` | NR50 (the mixer-only path) | 11.94 | 2.97 | 580 |
| `storm-apu@off` | NR32 with the APU powered off (ignored writes) | 8.99 | 2.24 | 408 |
| `storm-apu@1c:1x` | NR32, single speed | 10.59 | 2.64 | 1,063 |
| `storm-div@tac` | TAC 05 / 04 (on / off) | 5.90 | 1.47 | 228 |
| `storm-div@tac-rate` | TAC 05 / 07 (two rates) | 5.90 | 1.47 | 228 |
| `storm-div@div:1x` | DIV, single speed | 5.30 | 1.32 | 446 |
| `storm-gdma@free` | 1,839 GDMAs, destination not rewritten | 2.20 | 0.55 | 114 |
| `storm-gdma@pinned:1x` | 1,341 GDMAs, single speed | 2.08 | 0.52 | 448 |
| `storm-ppu@scy` | SCY 00 / 04 | 17.82 | 4.43 | 923 |
| `storm-ppu@stat` | STAT 00 / 78 | 16.80 | 4.18 | 863 |
| `storm-ppu@scx` | SCX 00 / 05 | 15.73 | 3.91 | 801 |
| `storm-ppu@wx` | WX 87 / 95 | 13.38 | 3.33 | 664 |
| `storm-ppu@lyc` | LYC 00 / 48 | 8.99 | 2.24 | 408 |
| `storm-ppu@bgp` | BGP (no effect on a CGB-mode picture) | 8.87 | 2.21 | 401 |
| `storm-ppu@bcpd` | BCPD, auto-increment | 3.84 | 0.95 | 108 |
| `storm-ppu@lcdc:1x` | LCDC, single speed | 12.34 | 3.07 | 1,266 |
| `storm-ppu@scx:1x` | SCX, single speed | 8.03 | 2.00 | 763 |

A sweep of the other registers (`synth:storm-io@<hex>`: 00 / FF to
FF00-FF21, FF23, FF25, FF26, FF40-FF7F and IE, 1 s each, Ir per 70,224
clocks) found nothing dearer than the classes above: the APU registers
15.9 to 19.8 M, SCY 17.5, STAT 16.5, SCX 14.8, WY 14.7, WX 11.9, NR51
11.8, NR52 (power off / on) 10.5, LCDC 00 / FF (LCD off / on) 9.95, LYC
8.8, DIV 8.8, BGP/OBP0/OBP1 8.7, TAC 5.4, TIMA 4.7, LY 4.6, SC 4.6, TMA
4.4, IF 4.0, IE 3.9, OCPD/BCPD 3.6 to 3.7, HDMA5 3.4; every other one is
under 3.0 M (KEY1, VBK, SVBK, P1, HDMA1-4 included; FF46 restarts an OAM
DMA whose bus conflicts send this loop astray, so it measures nothing).
The sweep's values are not chosen per register, so it is a screen, not a
bound.

**Case by case.** Reachability is from what games and demos are known to
do, not from a survey of the library (none was run); the mitigation gains
are **estimates** from the callgrind profile of the storm (inclusive Ir of
the function named, 1 s runs), and none is implemented.

- **(a) APU writes: 20.0 M at storm density, adversarial only; 1,051 Ir
  per write is what matters.** The review estimated 5-10 M; measured is
  twice that. Every write to FF10-FF3F except a powered NR50/NR51 brings
  the four channels up to the write (`Apu::sync_write` -> `sync`): that
  is 79% of the storm, `Apu::flush` alone 71% (825 Ir per write). Real
  software writes a sound register once per sample when it plays PCM (4
  to about 22 kHz: 70 to 370 writes a frame) plus a music driver's few
  dozen per frame; `synth:worst-case` has 414 a frame and
  `synth:apu-writes` 137 (8 kHz through NR50: a realistic player, not a
  storm). A program would need a write every 2 M-cycles for the whole
  frame, about 1 MHz, to reach the storm: none is known. Scaling: each
  1,000 writes a frame cost about 1.05 M Ir (0.58 M through NR50/NR51).
  Two paths cost although they cannot change the output: writes with the
  APU off (408 Ir each, 9.0 M: FF10-FF25 are ignored then, but the sync
  still runs) and writes to unused FF27-FF2F (988 Ir).
  Cheapest mitigation: return before the sync for a write that cannot
  change what the channels do (APU off and not NR52 or wave RAM; FF27-
  FF2F; a value equal to the stored one where the register has no write
  side effect). Estimated gain: those storms fall to about the control's
  2 M; **no gain for (a) itself**, whose writes do change the output.
  For (a), coalescing (apply the write to the channel state at once and
  let `flush` mix the listed spans later, as NR50/NR51 do now) would
  bring it towards NR50's 580 Ir per write, about 12 M (estimate); it is
  a redesign of the write path's sync invariant (ADR 0021 section 3),
  not a small change.
- **(b) Speed-switch loop: 8.5 M, adversarial only.** With `IE & IF != 0`
  and IME off, STOP is one byte and skips the 0x20000-clock halt, so a
  switch takes 3 M-cycles (plus one idle to double speed): 6,769 a frame,
  far denser than the review's "every ~10 M-cycles", and 965 Ir each
  (against the double-speed control, so approximate: this loop runs at
  both speeds; the profile has 963 in `Cpu::stop`, `Ctx::speed_switch`
  825 of it, its DIV reset 322). The review's
  ~3 M was low. Games switch speed once at start-up, a few again around
  link or infrared code: single digits per session. No mitigation is
  worth its size; if one were wanted, the DIV-reset part is (c)'s.
- **(c) DIV and TAC writes: 9.7 M and 5.9 M, adversarial only.** 447 Ir
  per DIV write (`div_reset_as` 406: the timer's reset 129, the APU's
  frame-sequencer part 83, re-scheduling the timer 74 and the APU 65),
  228 per TAC write. Games write DIV to seed a random number or restart
  a timeout, a few times a frame at most; TAC a handful of times when a
  sample player starts and stops. Cheapest mitigation: none needed; a
  reset cannot simply be skipped when the counter is already near 0,
  because the timer, serial and frame-sequencer deadlines still move.
- **(d) Back-to-back one-block GDMA: 2.35 M, inside the bound.** The
  review's concern does not hold: a block stalls the CPU for 17 M-cycles
  (double speed), so the loop makes 1,665 transfers a frame (1,839 with
  the destination left to run), and the time the CPU is stalled is time
  it executes nothing. Both variants are below `synth:worst-case` (2.66
  M). A real loader does this in VBlank with far fewer, larger transfers.
  No mitigation.
- **(e) PPU register writes: 22.0 M (LCDC), reachable in part.** 1,167 Ir
  per LCDC write averaged over the frame; the writes that land in mode 3
  (not counted separately) each flush the line drawn so far and re-plan the
  rest (`Ppu::flush` 28% of the storm, `Renderer::render` 24%,
  `render_held` 23%, `hold_fetch` 19%, overlapping). SCY, SCX, WX, WY
  are the same path at 660 to 920 Ir; STAT is a different one (863 Ir:
  `Ppu::write_stat` runs twice per write, one dot apart, 52% of that
  storm), as is LYC (408). Real software: one to a few mid-mode-3 writes
  per line (parallax, a status-bar split, a wobble effect) is common and
  benched (`synth:worst-case` 144 a frame, `synth:cgb-raster` 288 plus
  4,880 BCPD, `synth:dmg-raster` 288); a demo can write SCX or LCDC
  every few M-cycles through mode 3 for a line-doubling or mid-line
  effect, which is tens of writes per line: at 16 mode-3 writes a line
  (2,304 a frame) the storm's average predicts about +2.7 M Ir, an
  **estimate**, and likely low because every one of them lands in mode
  3. So a real demo can plausibly pass the 4.02 M line; no commercial
  game is known to. Two paths cost without changing the picture: BGP,
  OBP0 and OBP1 in CGB mode (401 Ir each: the write syncs and flushes
  the PPU, 33% in `flush`, though CGB mode ignores the DMG palettes) and
  a write of the value already there.
  Cheapest mitigation: return early for BGP/OBP0/OBP1 in CGB mode and
  for same-value writes to SCX, SCY, WX, WY, LYC and the DMG palettes
  (estimated gain: those storms fall to about the control's 2 M; no gain
  for writes that do change the value). For the general case the
  proposal on file is a dot-stepped fetcher for the rest of a line after
  the first mid-mode-3 write (`docs/reviews/stage1-m10/E-holistic.md`),
  estimated there at 2.5-3.5 M for the M8 programs: a large change, and
  it costs the common case a few percent.

**Consequence on the MCU.** A frame that costs more than its 16.74 ms is
late: the front end cannot present 59.73 frames a second, so emulation
runs slower than real time for as long as the program keeps it up, and
an audio buffer fed per frame underruns (a gap or a repeated buffer,
whichever the front end chooses). The core does not fail: no state is
lost, no access is skipped, the frames and samples it does produce are
the same ones (the invariance checks cover the storms). Stage 2 has to
decide what the front end does with a late frame (skip presenting it,
stretch audio, or both); the core gives it nothing but the time. The
plan's Stage 2 task still stands: run the storms on the M7 and replace
the estimate columns.

**Historical (M8 perf review; superseded by the table above).** x86 M Ir
per frame and % of the 4.02 M line, loop `LD (C),A; XOR D` unrolled (a
changed value every 3 M-cycles, about 11.7k writes a frame in double
speed, 3.8k in mode 3), on a different picture and an older core
(`hold_fetch` and the PPU walk have been rewritten since), so the rows
are not comparable with today's:

| Write | stage1-m7 | M8 (pre-418421c) |
|---|---:|---:|
| SCX, double speed | 6.12 | 7.84 (195%) |
| SCY, double speed | 6.12 | 8.74 |
| LCDC bits 2/3/4, double speed | 6.27 | 10.15 (252%) |
| WX (window on), double speed | 6.13 | 7.11 |
| SCX, single speed | 3.33 (83%) | 4.34 (108%) |
| control (HRAM writes) | 1.83 | 1.81 |

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
