# M10 review B: performance of gbc-core

Reviewer B, tree at 49c0756 (`main`). Read-only; nothing tracked was changed.

## Method

- x86 profiles: `target/release/bench` under callgrind (`--toggle-collect='*run_emulation*'`),
  aggregated per symbol (self, inclusive, calls) and per source line. Runs:
  `synth:worst-case` 2 s (478,444,047 Ir, 119.455 frames, **4.005 M Ir/frame**),
  `cpu_instrs` 5 s (334,784,796 Ir, 298.6 frames, 1.121 M/frame; the 20 s baseline run is 1.42 M/frame),
  `cgb-acid2` 4 s (139.4 M), `cgb_sound` 5 s (294.0 M), `synth:halt-irq` 3 s, `synth:cgb-raster` 2 s.
- thumbv7em: the assembly `check-rom-loads.sh` emitted today (`target/romloads/.../gbc_core-*.s`,
  release, `-C target-cpu=cortex-m7`), hand-read for the dispatch, tick, bus read, `boundary`
  and `fill_plain` paths, plus static instruction counts per function against the x86 build.
- Two experiments, both removed afterwards (scratch target dirs / worktree deleted):
  1. Build profile: `codegen-units=1`, and `codegen-units=1 + lto=fat + panic=abort` (env overrides, own target dir).
  2. Scratch worktree, one byte of `tools/bench/src/synth.rs` changed: `WORST_CASE` writes TAC=00 instead of 05
     (the unserviced timer off; program flow is otherwise identical).
- Raw outputs are in the scratchpad `review/` directory: `cg-*.out`, `ann-*.txt`, `core-thumb.s`, `cgsum.awk`.

## Where the time goes (measured)

### `synth:worst-case`, 4.005 M Ir/frame (the gate metric)

| Part | M Ir (2 s) | Share | Ir/frame | Notes |
|---|---:|---:|---:|---|
| CPU interpreter + inlined bus (`Cpu::run` self) | 129.4 | 27.0% | 1.08 M | 1.67 M instructions, 77 Ir each |
| Event machinery: `boundary` self 126.6 + `serial_event` 26.6 + `Timer::schedule_overflow` 17.7 | 170.9 | 35.7% | 1.43 M | 1,551,710 `boundary` calls (12,990/frame); 1,039,999 timer reloads; 487,864 serial shifts |
| Timer/serial register I/O (`write_timer_io` 8.0 incl, `read_high` 20.9 incl) | ~29 | ~6% | 0.24 M | the SC/STAT/HDMA5 polling loop |
| PPU (`run_ppu_to_call` inclusive 76.6, + `next_event`, `stat`) | ~90 | ~19% | 0.76 M | render 52.7 M of it; 519 syncs/frame (3.6 per line) |
| APU (`Ctx::apu_sync` inclusive) | 40.3 | 8.4% | 0.34 M | 2,194 Ir per sync, 154 syncs/frame |
| HDMA (`vdma_turn` inclusive) | 18.9 | 4.0% | 0.16 M | 1,107 Ir per 16-byte block, of which 6 `boundary` calls per stall |

So **about 42% of the gate metric is timer and serial event traffic**, not CPU, pixels or sound.

### `cpu_instrs` (5 s), 1.121 M Ir/frame

| Part | M Ir | Share |
|---|---:|---:|
| `Cpu::run` self | 222.3 | 66.4% |
| `execute_cb` inclusive | 16.0 | 4.8% |
| PPU (`sync_ppu` inclusive; BG only) | 68.7 | 20.5% (230 k/frame; `fill_plain` 1,042 Ir per line = 6.5 Ir/pixel, plus ~560 Ir fixed per line) |
| APU (idle, post-boot CH1) | 10.5 | 3.1% (35 k/frame) |

3,094,528 instructions in 5,242,880 M-cycles: **77 Ir per SM83 instruction, 45 Ir per M-cycle** for the CPU alone.
By source line, per instruction: inlined bus code 35.9 (read dispatch + tick), `exec.rs` 16.9 (5.3 of it the opcode
jump, 6.8 the arm), loop prologue in `cpu/mod.rs` 14.1, `core` integer helpers ~4, cart/ROM 1.6, `execute_cb` 5.2.

Answer to "is 80 Ir per M-cycle high": yes, by roughly 2x for the CPU part. A tight M-cycle-accurate interpreter
with the same safety rules should land near 40-45 Ir per instruction (B6-B8); the PPU and APU parts are near a
reasonable floor for this design.

### Context that matters for the "zero headroom" worry

`docs/perf.md` wall-clock table: Shantae 0.574, Wario Land 3 0.537, Pokemon Crystal 0.289 host Mcycles/frame
against `synth:worst-case` 1.344. The heaviest real game measured is **43% of the synthetic worst case**. The bar
has no headroom against a program whose largest single cost (B1) is something no game does.

## Findings

### B1 Unserviced TIMA overflows run a full scheduler event each: 23.7% of the gate metric   [severity: high] [confidence: high]
- Where: `crates/gbc-core/src/bus.rs:754-764` (`run_events`, Timer arm), `timer/mod.rs` `reload`/`schedule_overflow`/`deadline`,
  `schedule.rs:104-119` (`take_first_due`), `tools/bench/src/synth.rs:103-190` (`WORST_CASE`: TMA=FF, TAC=05, IE=STAT only).
- What: the timer is already lazy (TIMA is a function of the clock) except for one thing: every overflow is a scheduled
  event that reloads `tima`/`tima_from`, stores `reloaded_at`, recomputes `overflow_at` and ORs bit 2 into IF. When IF
  bit 2 is already set, the only observable effect of that event is state the model could derive later. In the worst-case
  program nobody ever clears IF.2 (IE is STAT only), so 1,039,999 events in 2 s (8,706 per frame, one every 4 M-cycles) do nothing.
- Measurement: experiment 2 (TAC=00 in `WORST_CASE`, nothing else changed): 478,444,047 -> **364,842,860 Ir (-23.7%)**,
  4.005 -> 3.054 M Ir/frame. That is 109 Ir per overflow. `boundary` calls fall from 1,551,710 to 520,303. HDMA blocks get
  cheaper as a side effect (6 of the `boundary` calls per stall were timer events).
- M7 (estimate): the same path is ~250 thumb instructions per event (`boundary` 346 static vs 221 on x86,
  `schedule_overflow` 67 vs 20, all u64), so roughly 2.2 M cycles/frame, over half of the 4.02 M budget on its own.
- Proposed change: make the Timer deadline `NEVER` while IF bit 2 is set. Add `Timer::catch_up(now)` that applies the
  elapsed reloads in closed form (period = (256 - TMA) falls of `tima_bit`; yields `tima`, `tima_from`, `reloaded_at`,
  `overflow_at` exactly as the last eager reload would have left them). Call it at the start of every timer register
  access (`Timer::read`, `write_tima/tma/tac/div`, STOP, speed switch) and wherever IF.2 is cleared (`write_if`,
  `push_acknowledge`, `push_acknowledge_if`, `set_interrupt_flags`), then set the deadline again.
  Effort M, risk medium (cycle A/B rules around `reloaded_at`). Test: the existing eager `timer::reference` differential
  test extended with random IF clears; `diff-fingerprint.sh` identical; `synth:event-storm`/`worst-case` Ir.
- Follow-up that belongs with it: afterwards the bench no longer stresses timer interrupts at all. Add IE=timer and a
  RETI handler to the worst-case program (a decision for the human: it changes what the gate measures), so the
  realistic maximum (one serviced timer IRQ per ~9 M-cycles) is what the bar covers.

### B2 Serial transfers cost one event per bit: about 11% of the gate metric   [severity: high] [confidence: medium]
- Where: `bus.rs:786-797` (`serial_event`), `bus.rs:452-475`, `serial.rs:124` (`next_shift`), `serial.rs:168` (`shift`).
- What: a running transfer schedules each of its 8 shifts. Only the eighth has an effect outside the serial port
  (IF bit 3, `Events::SERIAL_TX`); SB mid-transfer and the bit count are functions of the system counter, which the
  module already computes edges from.
- Measurement: 487,864 `serial_event` calls in 2 s, 53.6 Ir self each plus 68.1 Ir of `boundary` per call in the
  timer-off run (experiment 2 profile): 59.4 M Ir. Seven in eight are avoidable: **about 52 M Ir, 10.9% of 478.4 M**.
  Not measured by experiment (removing the transfer changes the program's flow), so medium confidence on the exact figure.
- Proposed change: schedule only the completion (the clock of the 8th 1->0 flip-flop edge). `read SB`, `write SB`,
  `write SC`, DIV reset and STOP first bring `sb`/`out`/`bits` up to the clock in closed form (count of edges since the
  last sync, from `Timer::sc`). Effort M, risk medium (SC-write and DIV-reset edge rules). Test: the existing
  `lazy_next_shift_matches_the_eager_flip_flop` differential test extended to mid-transfer SB reads and DIV writes;
  fingerprint identical.
- With B1: 478.4 M -> about 313 M Ir, **4.005 -> ~2.6 M Ir/frame (-35%)** (B1 measured, B2 estimated).

### B3 The 4.02 M x86-Ir line is not a trustworthy stand-in for "50% of the M7"   [severity: high] [confidence: high]
- Where: `README.md:18`, `docs/perf.md` (all "% of the 4.02 M line" statements), `scripts/check-bench.sh`,
  `docs/plan/stage-1.md:135` and `:298` (already-tracked open items saying the same).
- What, with numbers:
  1. Build sensitivity exceeds the headroom. Same source, `codegen-units=1`: worst-case 488,432,817 Ir (**+2.09%**);
     `codegen-units=1 + lto=fat + panic=abort`: 487,270,705 (+1.84%); cpu_instrs +1.71% / +1.92%; cgb-acid2 +1.21% / +1.70%.
     The bar currently has 0.4% headroom (4.005 of 4.02 M). The MCU firmware will not be built with the bench's profile
     (16 CGUs, no LTO, unwinding), so the last 2% of this number says nothing about the target.
  2. Thumb needs more instructions exactly where the bench spends its time. Static thumb/x86 instruction ratios per
     function: `Timer::schedule_overflow` 3.35, `schedule_serial` 2.56, `serial_event` 2.28, `Ppu::latch` 1.71,
     `boundary` 1.57, `write_timer_io` 1.57, `read_high` 1.48, `Ppu::stat` 1.34, `Noise::integrate` 1.30,
     `Cpu::run` 1.07, `Apu::sync` 1.02, `fill_mixed` 0.95, `Square/Wave::integrate` 0.92.
     Weighting each function's measured self Ir by its ratio gives **~5.7 M thumb instructions per worst-case frame**
     (estimate; static ratios are not dynamic path ratios), i.e. about 140% of the 4.02 M line before any
     cycles-per-instruction factor, branch mispredicts or memory wait states.
  3. x86 hides register pressure: thumb `Cpu::run` has 611 stack-slot references in 7,791 instructions (B9).
- Proposed change (ranked by cost):
  - Now, no install: keep the Ir check as a regression detector only, and say so in README/perf.md (a factual
    correction). Stop quoting it as a percentage of the M7.
  - Add a thumb instruction count: run a `thumbv7em` build of the bench loop under an instruction-counting emulator
    (`qemu-system-arm -M mps2-an500 -icount`, or a TCG plugin for per-function counts). Needs QEMU installed: human.
    This removes points 2 and 3; it still has no cache or branch model.
  - Cheaper stopgap: an `i686-unknown-linux-gnu` bench under valgrind (32-bit u64 costs show up; needs the rustup
    target and multilib: human).
  - Add two or three fixed real-game workloads (input script + frame range) next to the synthetics, so the bar is
    anchored to what games do. `bench --wall --rom` already runs them.
  - Stage 2: one DWT cycle-counter measurement of `worst-case` and of a real game calibrates all of this.
  Effort S-M. No behavior risk.

### B4 Time is u64 on every event and I/O path   [severity: high on the M7, low on x86] [confidence: high for the instruction counts, medium for the gain]
- Where: `schedule.rs` (8 x u64 deadlines + `next` + `slow`), `bus.rs:296-349` (`Hot::deadline`, `clock()`),
  `bus.rs:714-721` (`reschedule_at`), `timer/mod.rs` (`base`, `base_clock`, `tima_from`, `tima_bit`, `overflow_at`,
  `reloaded_at`), `serial.rs` (`clock_bit`, `phase`), PPU line clocks, `push_acknowledge` (`bus.rs:2040-2043`).
- What: the per-M-cycle countdown is 32-bit (good), but everything behind it compares, mins, shifts and saturates
  u64 values. On thumbv7em: `boundary` has 33 carry-chain instructions (`sbcs`/`adc`) and 65 loads in 346; every
  deadline compare is `subs` + `sbcs` + branch; `Timer::sc`/`clock_of` are 64-bit variable shifts (~8 instructions
  each); `reschedule_at` is ~25 instructions of saturating 64-bit arithmetic; even interrupt dispatch
  (`push_acknowledge`) does a 64-bit clock reconstruction and compare inline in `Cpu::run`.
- Measurement: static thumb/x86 ratios above (1.57 to 3.35 on the event path). Estimate: a timer event is ~250 thumb
  instructions against 109 x86 Ir.
- Proposed change: a u32 clock relative to an epoch. `Sys` keeps `epoch: u64`; all deadlines and stored clocks become
  u32 offsets, `NEVER = u32::MAX`; `Emulator::run` rebases at the start of every call (subtract the elapsed offset from
  each stored clock, saturating for `NEVER`), so rebasing is exercised every frame rather than every few minutes.
  The system counter can be a wrapping u32 (the hardware one is 16 bits; edge counts are differences).
  Effort L (15-20 fields in 6 modules, each needing a `rebase`), risk medium. Test: fingerprint identical over the full
  manifest; a unit test that rebases at random points and compares with a never-rebased run.
  Expected: x86 Ir a few percent; thumb event/I-O paths -35 to -40% instructions (estimate). Do B1/B2 first: they
  remove most of the events this would speed up, and the remaining gain should then be re-estimated on a thumb count (B3).

### B5 Scheduler bookkeeping costs more than the work it schedules   [severity: medium] [confidence: medium]
- Where: `schedule.rs:73-119`, `bus.rs:736-782`.
- What: per event, `take_first_due` scans, stores `NEVER`, and recomputes the minimum; the handler then calls
  `set_inline`, which compares again and may recompute again; `boundary` then calls `reschedule_at` (min with target,
  saturating subtract, clamp). Three passes over the same few deadlines for one timer reload.
- Measurement: lines attributed inside `boundary` in the worst-case profile: `schedule.rs` 17.4 M, `cmp.rs` 17.4 M,
  `uint_macros.rs` 13.5 M, slice iteration 9.7 M: **45-58 M Ir (9-12%)**, about 30-37 Ir per `boundary` call
  (some of the `uint_macros` share is timer arithmetic). `boundary` self is 81.6 Ir per call.
- Proposed change: in `run_events`, test the four hot kinds in fixed order (`if at[k] <= now { handler }`), let each
  handler write its next deadline straight into `at[k]`, and compute `next` once at the end, merged with the target
  clamp of `reschedule_at`. Same event order, no `NEVER` round trip. Effort S, risk low. Expected about -20 Ir per
  `boundary` call: -31 M Ir (-6.5%) today, about -1% once B1/B2 land. Test: `schedule.rs` unit tests, fingerprint.

### B6 Four separate checks before every instruction   [severity: medium] [confidence: high for the cost, medium for the gain]
- Where: `crates/gbc-core/src/cpu/mod.rs` `run`/`step_inline` (`run_done`, `mode != Running`, `ime && IE & IF`, `ei_pending`).
- What: every instruction tests `done` (2 Ir), `mode` (2), `ime` (2, or 6 with the IE & IF load when IME is set) and
  `ei_pending` (2), each a byte load and a branch, through two pointers (`Cpu` and `Ctx -> Sys`).
- Measurement: `cpu/mod.rs` lines in `Cpu::run`: 43.8 M Ir over 3.09 M instructions = **14.1 Ir per instruction**
  (13% of cpu_instrs); 26-30 M in worst-case (IME is on there). Thumb: 11 instructions, 4 loads, 4 branches before the fetch.
- Proposed change: one `attention: bool` in `Cpu` (or fold it into `Ctx::done`), true when any of: mode is not Running,
  `ei_pending`, the run is done, or `ime && IE & IF & 0x1F != 0`. Recompute it where those inputs change: EI/DI/RETI,
  HALT/STOP entry and exit, `raise`, IE and IF writes, and the end of `run_events` (every IF-setting event is there or in
  `Ppu::sync`). The loop tests one byte; the slow path does today's checks. Effort S-M, risk medium (a missed
  recompute is a missed interrupt; the SM83 single-step suite and the interrupt-timing ROMs cover it).
  Expected: -8 Ir per instruction: cpu_instrs -24.7 M of 334.8 M (**-7.4%**), worst-case -13.4 M (-2.8%).

### B7 The clock countdown is a load-subtract-store-test in memory on every M-cycle   [severity: medium] [confidence: high for the cost]
- Where: `bus.rs:432-439` (`tick`), every inlined site in `Cpu::run` (140 `bl boundary` sites on thumb).
- What: `budget` and `mcycle` live in `Ctx` behind the bus pointer. x86: `lea; mov; sub; mov; test; jg`. Thumb:
  `ldr r2,[sp,#24]; ldr r0,[r9]; ldr r1,[r2]; subs; str; cmp; bgt`: 7 instructions, 3 loads (one reloads the *address*
  of `budget` from a stack slot), 1 store, per M-cycle.
- Measurement: the two `tick` lines are 20.1 M Ir in cpu_instrs (6.0%) and 11.8 M in worst-case (2.5%).
  M7 estimate: 7-8 cycles x 35,112 M-cycles in double speed = **~260 k cycles/frame, 6.5% of the 4.02 M budget** for
  counting time.
- Proposed change (structural, with B8): pending-cycle accounting, as SameBoy does (`pending_cycles`): ROM/WRAM/HRAM
  accesses and idle cycles add to a local counter; the counter is committed (and events due are run, each at its own
  deadline clock) before any access that can observe time (FE00-FF7F, VRAM, cart RAM/MBC writes, anything while
  `hot.dma != 0` or a DMA start is pending) and at the end of the instruction. One compare per instruction instead of
  per M-cycle. Handlers must take the event's clock as `now` instead of `self.clock()`; most already receive `now`.
  Effort L, risk high (interrupt and DMA edge timing); the SM83 per-cycle tests and the full manifest are the net.
  Expected: -4 to -5 Ir per M-cycle, about -75 k (single speed) to -150 k (double speed) Ir/frame.
  A smaller step with most of the thumb gain: restructure so `budget`/`mcycle` are addressed off the `Ctx` base
  register directly (no `let h = &mut self.hot` field pointer for LLVM to hoist and spill); check the result in the
  thumb assembly. Effort S, risk none, gain 2 loads per M-cycle on thumb (estimate), nothing on x86.

### B8 A plain memory access costs 14-27 Ir before the tick   [severity: medium] [confidence: high for the cost]
- Where: `bus.rs:2185-2196` (`read`), `bus.rs:1065-1079` (`write_mapped`, always out of line), `bus.rs:2289-2307`
  (`wram`), `cart/mod.rs:596-602` (`read_rom`), `rom.rs` (`Rom::get`).
- What:
  - ROM read (x86, 14 Ir): reload `sys` pointer, `or` with `hot.dma`, sign test, window select, bounds compare, load
    window pointer, load byte. Three dependent loads (`Ctx -> Sys -> rom_win[i] -> byte`); on thumb 19 instructions and
    5 loads because the `sys` pointer and the `dma` field address come from stack slots.
  - Anything else falls into a compare chain: VRAM, then cart RAM, then WRAM. A WRAM read is ~17-20 Ir.
  - HRAM and I/O reads call `read_high` out of line: 15.1 Ir inclusive per call in cpu_instrs, 44 in worst-case.
  - Every write calls `write_mapped`: 26.7 Ir inclusive per write in cpu_instrs (488,565 calls), including stack pushes
    to WRAM/HRAM, the most common writes in real code.
- Measurement: bus lines inlined into `Cpu::run` are 111.1 M Ir in cpu_instrs: **33.2% of the whole run, 35.9 Ir per
  instruction**; `match (addr | hot.dma) >> 13` alone is 46.6 M. (cpu_instrs executes mostly from WRAM: only 489,537
  ROM byte loads in 5 s. A game executing from ROM pays the cheaper 14 Ir path per fetch.)
- Proposed change: keep the two ROM windows and the WRAM bank offset by value in `Hot` (updated by the MBC and SVBK
  writes), so a ROM or WRAM access is one base register and one or two loads; give WRAM and HRAM an inlined fast path
  for reads *and* writes (HRAM and WRAM have no side effects when `hot.dma == 0`); dispatch the rest out of line.
  `Rom<'a>` in `Hot` is a plain borrowed slice, so this needs no `unsafe`. Effort M, risk low-medium (bank-switch
  invalidation; `check-rom-loads.sh` still applies). Expected: -3 Ir per ROM read, -8 per WRAM/HRAM read, -10 to -15
  per WRAM/HRAM write: about -25 to -30 M Ir on cpu_instrs (-8%), 2-4% on ROM-resident code.

### B9 `Cpu::run` on thumb: register pressure and a single dispatch branch   [severity: medium, M7 only] [confidence: medium]
- Where: thumb `Cpu::run` (7,791 instructions, ~25 KB), `cpu/exec.rs` `dispatch_all!`.
- What:
  - 611 instructions reference `[sp, ...]`. Loop-invariant *addresses* of `Ctx` fields (`&hot.budget`, `&hot.dma`,
    `&ctx.sys`) are materialized at entry and spilled, then reloaded at each use (see B7, B8).
  - One `tbh [pc, r0, lsl #1]` for all 256 opcodes, and 137 handler exits branch back to the loop head. The M7
    predicts an indirect branch from its last target, so this branch mispredicts on nearly every instruction.
    Estimate: ~8 cycles x 10-20 k instructions/frame = **80-160 k cycles/frame (2-4% of the budget)**.
  - Opcode fetch to dispatch for a ROM-resident NOP is 44 thumb instructions with 14 loads and 2 stores (counted
    from the assembly): estimate 45-50 cycles including the mispredict.
- Proposed change: B6-B8 remove most of the loads. For the dispatch there is no stable-Rust guaranteed tail call, so
  threaded dispatch is not available; do not restructure for it. Measure the mispredict rate on hardware in Stage 2
  (DWT counters) before spending effort. Effort: none now.

### B10 ITCM/DTCM placement is a precondition of every number here, and it is still "Proposed"   [severity: medium, M7 only] [confidence: medium]
- Where: `docs/decisions/0026-itcm-placement-at-link-time.md` (Status: Proposed), `docs/perf.md` "ITCM placement plan".
- What: the Ir bar assumes every instruction costs the same. From flash at 4 wait states behind a 16 KiB I-cache, a
  25 KB interpreter plus bus, scheduler, PPU and APU code will not stay cached; perf.md puts a miss at 10+ cycles.
  The H753 plan leaves 1.5 KB of ITCM margin and keeps ~14 KB of per-line PPU code in flash. Data is not covered at
  all: `Hw` (~1 KB) and `Memory` (WRAM 32 K, VRAM 16 K, tables) belong in DTCM (128 KB, zero wait, uncached); the
  46 KB frame buffer (160 x 144 x 2 B, the caller's) and the ROM (SDRAM/QSPI) are the D-cache's job.
- Proposed change: accept ADR 0026 or replace it before Stage 2 starts; add the data placement (core state in DTCM)
  to it; prefer the H735 (128 KB+ ITCM) if the board choice is still open, since B1-B8 do not shrink code. No core change.

### B11 PPU sync has ~300-380 Ir of fixed cost and runs 3.6 times per line in the worst case   [severity: low] [confidence: medium]
- Where: `bus.rs:812-907` (`sync_ppu`, `run_ppu_to_call`, `schedule_ppu`), `ppu/mod.rs` `sync`, `next_event`, `point_irq`.
- Measurement (worst-case): 61,972 `Ppu::sync` calls in 2 s (519/frame). Per call: `sync` self 110, `next_event` 112
  inclusive, `sync_ppu` 46, `run_ppu_to_call` 29, `Schedule::set` 11.5, plus `boundary` for the 27,666 that are
  events: about **160-200 k Ir/frame (4-5%)** before any pixel is drawn. cgb-acid2: `point_irq` is called 221,876
  times in 4 s (928/frame, 20 Ir each).
- Proposed change: perf.md already names the pair of events one dot apart (the `End` draw, then the HBlank STAT edge).
  Draw lazily at the HBlank event and make the few accesses that can land in that one-dot gap (`write_vram`,
  palette and OAM writes) flush a pending line first. Effort M, risk medium (it touches access blocking). Expected
  -144 events/frame: about -50 k Ir/frame (-1.3%). Low priority.

### B12 APU: every register write pays a full four-channel sync   [severity: low] [confidence: medium]
- Where: `bus.rs:639-663` (`write_apu`), `apu/mix.rs` `Apu::sync`.
- Measurement: worst-case `write_apu` 16,983 calls, 2,301 Ir inclusive each (39.1 M, 8.2%, most of it sample
  generation that would be paid anyway); perf.md's own split is 885 Ir per write, about half fixed setup. APU total
  338 k Ir/frame worst-case (617 Ir per stereo sample), 103 k in cgb_sound, 35 k idle.
- Proposed change: NR50/NR51 writes change only mixer weights, not channel waveforms. Queue them as (clock, weights)
  change points and let the channels integrate whole batches; apply the weights per window at mix time. Removes the
  per-write channel setup (~400 Ir x 146 writes = ~60 k Ir/frame, 1.5%). Also skip channels with DAC off or not
  routed in NR51 (no gain on the worst case, real gain on games). Effort M, risk medium (sample-exact fingerprints).

### B13 Bounds-check panics remain in per-line and per-access code   [severity: low] [confidence: high]
- Where (thumb build, `bl panic_bounds_check`): `Ppu::latch` x2 (one at `ppu/mod.rs:3073`), `Ppu::set_stalls` x2,
  `Ppu::hold_fetch`, `Ppu::flush`, `Ppu::tile_lead`, `Ppu::fix_attrs`, `bus::read_oam` x2, `Ctx::write_oam` x2,
  `write_during_dma`, `Renderer::mix_planes` x2, `Renderer::last_fetched`.
- What: README states "no hidden panics or bounds checks in hot paths (verified in the generated assembly)". `latch`
  and `set_stalls` run every visible line; `read_oam`/`write_oam` on every CPU OAM access. The cost is a compare and
  an untaken branch each (a few Ir per line), so this is about the stated standard, not speed. `Cpu::run`,
  `boundary`, `fill_plain`, `fill_mixed`, `render`, `Apu::sync` and the `integrate` functions have none.
- Proposed change: mask the indices (as the renderer does) or narrow the README claim to the functions it holds for.
  Effort S.

## Ranking: (expected gain) / (effort + risk)

### 1. Quick wins
| # | Change | Gain | Effort / risk |
|---|---|---|---|
| B5 | One-pass scheduler in `run_events` | -6.5% worst-case now (~-1% after B1/B2) | S / low |
| B6 | Single `attention` flag in the CPU loop | -7.4% cpu_instrs, -2.8% worst-case | S-M / medium |
| B7 (small step) | Address `budget`/`mcycle` off the `Ctx` base | thumb only: ~2 loads per M-cycle (estimate) | S / none |
| B3 (docs) | Call the Ir line a regression detector, not 50% of the M7 | correctness of the claim | S / none |
| B13 | Remove or own up to the remaining bounds checks | none (standard) | S / none |

### 2. Structural changes with large gains
| # | Change | Gain | Effort / risk |
|---|---|---|---|
| B1 | No timer event while IF.2 is set (closed-form catch-up) | **-23.7% worst-case, measured** | M / medium |
| B2 | One serial event per byte | ~-11% worst-case (estimate from profile) | M / medium |
| B8 | ROM windows and WRAM offset in `Hot`; inline WRAM/HRAM read and write | -8% CPU-bound, 2-4% ROM-resident | M / low-medium |
| B7 | Pending-cycle accounting (one time check per instruction) | -75 k to -150 k Ir/frame; ~260 k cycles/frame on the M7 (estimate) | L / high |
| B3 | Thumb instruction-count bench + real-game workloads | makes the bar mean something | M / none (needs installs: human) |

B1 + B2 take the gate metric from 4.005 to about 2.6 M Ir/frame. B6 + B7 + B8 together should take the interpreter
from 77 to roughly 45 Ir per instruction (estimate), another 0.4 M Ir/frame on the worst case and 25-30% on
CPU-bound frames.

### 3. Things that only matter on the M7
| # | Item | Size (estimates) |
|---|---|---|
| B4 | u32 epoch-relative time | event and I/O paths -35 to -40% thumb instructions; re-estimate after B1/B2 |
| B10 | ITCM for code, DTCM for `Hw` + `Memory` | without it, I-cache misses at 10+ cycles dominate everything above |
| B9 | Stack traffic in `Cpu::run` (611 `[sp]` references) | folded into B7/B8 |
| B9 | Single `tbh` dispatch mispredict | 80-160 k cycles/frame; no stable-Rust fix, measure first |
| B3 | Worst case in thumb instructions | ~5.7 M/frame today vs the 4.02 M line; ~3.4 M after B1/B2 |

## Bigger-picture recommendations

1. **Make work proportional to observable effects, then re-derive the worst case.** The design principle of ADR 0002
   and 0011 (lazy state, events only for effects) was stopped one step short for the timer and serial port. Finishing
   it (B1, B2) is worth more than all the micro-tuning recorded in perf.md combined, and it is the only change here
   that moves the gate metric by a third. After it, redefine `synth:worst-case` with the human so it stresses things a
   demanding game does: serviced timer interrupts, HDMA, 10-object lines, PCM writes.
2. **Stop steering by x86 Ir to 0.1%.** A 2% swing from codegen-unit partitioning and a 0.9-3.35x per-function
   thumb/x86 ratio mean the last several percent of the bar are noise and its absolute level is unknown. Keep Ir for
   regressions. Get a thumb instruction count (emulated, no hardware) and fixed real-game workloads, and get one
   hardware cycle measurement as early in Stage 2 as possible. The heaviest real game on record is 43% of the
   synthetic worst case, so the situation is likely better for games and worse for the synthetic than the bar says.
3. **Treat the CPU/bus fast path as one design, not 140 inlined ticks.** B6, B7 and B8 are the same idea: the common
   instruction touches only registers, ROM, WRAM and HRAM and should need one "anything unusual?" test and no clock
   arithmetic. Do B6 and B8 first (contained, low risk); decide on full pending-cycle accounting (B7) only with a
   thumb count in hand, because its risk is the highest in this report.
4. **Do B4 (u32 time) after B1/B2 and after a thumb count exists.** It is the largest M7-only item but touches every
   module; its payoff shrinks once the event storm is gone, and x86 Ir cannot show it.
5. **Leave the pixel pipeline and the channel integrators alone.** They are near the floor for this design
   (below); further tuning there is the 1% work the owner wants to stop doing.

## Looked at and found fine

- **BG/window tile loop** (`fill_tiles`, pair tables, `SPREAD`, `ATTR_FETCH`): 52 Ir per 8-pixel tile on x86,
  40-48 thumb instructions; no panics, no per-pixel branches. BG-only rendering is 150 k Ir/frame (3.7% of the line).
- **Object path** (`fetch_objects` 837 Ir per 10-object line, `fill_mixed` 768 Ir per call): reasonable; the 64-bit
  object merge and whole-tile skip are sound. Thumb/x86 ratio 0.95-1.07.
- **APU integrators**: closed-form per window, 76-114 Ir per channel-window; one `udiv` per batch per channel on thumb
  (2-12 cycles, negligible); no 64-bit division or library division calls anywhere in the hot set
  (`__aeabi_uldivmod` is called only from `noise::long_steps`, `apu::rem_long` and `oam_dma_during_vdma`, none of which ran in these profiles).
- **HALT**: `idle_halted` consumes the whole countdown in one step; `synth:halt-irq` spends 32% in pixels, 20% in the CPU.
- **PPU event rate without STAT sources**: one sync per visible line (143.6/frame in cpu_instrs).
- **Timer and serial register reads/writes**: already computed from the clock; only their events are eager (B1, B2).
- **ADR 0027 byte-wide ROM reads / `compiler_fence`**: no measurable cost on the CPU path (`Rom::get` is 3 Ir per load).
- **Release profile**: LTO and `codegen-units=1` do not help (they cost 1-2% Ir); no finding beyond the noise point in B3.
- **Struct copies**: `vram_dma` copies `VramDma` in and out per block and `Apu::sync` clears 128 B per sync; both are
  small next to the event costs above.
