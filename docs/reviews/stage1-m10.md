# Stage 1 M10: in-depth review, triage and outcomes

Review of the whole tree at `stage1-m9` (49c0756), 2026-11-24, session 16.
Eight independent read-only reviewers; their unedited reports are in
`docs/reviews/stage1-m10/` (A1 CPU/bus, A2 PPU, A3 APU, A4 DMA/cart, B
performance, C size/MCU fit, D Rust, E holistic design). This file is the
triage: one line per finding with its outcome. Update the Outcome column as
work lands; a finding is closed only with a commit or a recorded reason.

Outcome keys: **fixed** `<commit>` · **wave N** (planned, see Waves) ·
**stage 2** (deferred, needs M7 measurements or hardware) · **human**
(needs or got a human decision) · **rejected** (reason) · **plan** (low:
one-line task in the plan) · **OPEN** (high/medium not yet fixed; blocks
the `stage1-m10` tag). Plan tasks are in `docs/plan/stage-1.md` under M10;
"carried-over task" means its "Carried over from the Stage 1 M10 review"
list (there is no Stage 2 plan file yet).

A correctness finding counts as *confirmed* when its regression test has
been seen failing on the old code; until then it is a reviewer claim.

## Headline

- No reviewer found a wrong CPU instruction, a missed scheduler event in
  normal operation, a clock overflow on long runs, a release-build panic
  reachable from caller data, or an MCU memory-fit problem (state 51.8 KB
  of 128 KB DTCM; ITCM hot set 49.4 KB of 57.3 KB; stack ~1.2 KB).
- One high correctness bug games can hit (A2-01).
- ~42% of the worst-case bench was timer/serial events nothing can observe
  (B1, B2). The x86-Ir line is a weak proxy for M7 cycles (B3, B4).
- The byte/struct budgets were deciding where state lives (E2, D1, A1-05,
  A2-03, C3): reformed by the human (STATUS, "Decided in session 16").
- The mid-line PPU and the STAT/sub-M-cycle timing are patch sets over two
  reference emulators' models (E1, E3, E4, A2-02, A1-06, A1-07).

## Waves

1. Nets and measuring stick: checked profile (done), `codegen-units = 1` +
   budget reform, lazy timer (B1).
2. Correctness fixes, each with a test seen failing, full manifest 0
   regressions.
3. More nets (E6, A3-2) before large refactors.
4. Performance (B2, B5-B8, C6, C7), fingerprint-identical; then the
   worst-case program gains a serviced timer IRQ (human).
5. Structure: state back to its owners, typed IRQ event, timeline table,
   bus.rs split, docs.
6. Test-only dot-stepped mode-3 oracle (E1); audio alias measurement (A3-1).

## A. Correctness

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| A2-01 | high | CGB palette writes dropped while the LCD is off if it was turned off inside mode 3 (`Ppu::drawing` ignores LCD enable) | fixed 86c2f98 (confirmed: read back FF/00 instead of 1F; nothing else shared the hole) |
| A2-02 | med | STAT IRQ model = two reference models spliced; walk a dot early at 0:4 and 153:12, five compensations | OPEN: timed requests (ADR 0033) put IF at the right dots and E3's table landed; the walk points and their compensations remain (`docs/reference/stat-irq.md` "Open"): plan task |
| A2-03 | med | Mid-line state in three structs, invariants held by `% 11` masks | fixed (m10-ppu-state): renderer owns the fetch state; `Stalls` type replaces the masks |
| A2-04 | low | Delayed LYC IRQ in IF bits 5-6 can be stranded/wiped/leaked | fixed (ADR 0033, timed requests in `Ppu::irq`; three scenarios were real behaviour bugs) |
| A2-05 | low | LCDC.5 clear early on the WY line loses the WY latch for the frame | fixed 65f00bd (confirmed; SameBoy `wy_check` + Pan Docs) |
| A2-06 | low | `obj_size_fetch` patches VRAM to pass a parameter (= E10) | fixed (m10-ppu-state) |
| A2-07 | low | Write landing at `now - 1` on the mode-3 end dot keeps the old value for pixel 159 | plan (already the M8 "PPU edge cases" task) |
| A2-08 | low | Window zero pixel on CGB rests on one Mealybug capture, against SameBoy | rejected: the capture is CGB C hardware, our target (ADR 0003); tagged as single-source (E7) |
| A2-09 | low | `Memory::clear` skips `obj_rank.hold`, `win_zero`, `dma_read_clears` (= D3) | fixed (m10-ppu-state; test seen failing) |
| A2-10 | low | TILE_SEL set-glitch falls back to the old calibration when the latch is unknown | plan (M10 task; the oracle exists, 0e674bc) |
| A2-11 | low | `stat_during_dma`, `fix_attrs` `d - 5`, DMG `POST_BOOT_POS` single-source | plan (with the E7 task) |
| A3-1 | med | 128-clock box filter leaves aliasing on high notes | human: measured 6753609 (`docs/reference/audio-aliasing.md`, ratchet test `tests/apu_aliasing.rs`); every fix costs Ir (about +10% for box 64 + 8-tap FIR), so it is a budget decision in STATUS "Needs human" |
| A3-2 | med | No independent check of produced samples | fixed: `tests/apu_output.rs`, 14 Pan Docs-derived tests, each mutation-checked; all passed on the unmodified core |
| A3-3 | low | `Square::since_step` uses the next-reload period after an NRx3 write | fixed (confirmed: countdown 1522 vs 498) |
| A3-4 | low | Five fitted trigger windows; the two square trigger rules are one rule | plan (M10 task) |
| A3-5 | low | Output headroom (±2x full scale after high-pass) not in the API contract | fixed: bound is exactly +-61,440 (2x full scale, 17 bits) in the API docs and ADR 0021 |
| A3-6 | low | Stale sweep overflow check survives a CH1 retrigger | fixed (confirmed against SameBoy apu.c) |
| A3-7 | low | Desktop DC blocker starts from zero (click) | plan (M10 task; desktop is throwaway) |
| A4-1 | med | Unsupported mappers run as ROM-only with battery-flagged RAM (= D12) | fixed 35ff843 (ADR 0031; independent API review: no high/medium). HuC1 + HuC3: done (ADR 0032; independent review: no high/medium; unverified-on-hardware list in `docs/reference/hudson-mappers.md`) |
| A4-2 | med | HDMA cancel with a block pending copies `value + 1` blocks | fixed (branch m10-hdma-cancel): the triggered block runs as one block (gambatte; SameBoy does value+1 as we did). Non-zero cancel values are single-source: Needs human FYI |
| A4-3 | med | Fuzz pass built without overflow checks / debug asserts (= D5) | fixed fc92df4 |
| A4-4 | low | `dma_retime` underflow on a switch to single speed right after reset | fixed df7a7a9 (confirmed: test failed before) |
| A4-5 | low | `RtcState::advance_seconds` underflow on an unmasked state | fixed f29659a (confirmed) |
| A4-6 | low | Re-arming an active HDMA inside HBlank starts a second block (references disagree) | pinned + commented (we follow SameBoy); no behaviour change |
| A4-7 | low | RP (FF56) unimplemented (= A1-03) | fixed (RP reads 3E \| written C1 bits in CGB mode) |
| A4-8 | low | MBC1M detection accepts two blank logo areas | fixed 2de0ffb (confirmed) |
| A4-9 | low | MBC3 latch edge rule unverified | plan (already a task: M4 hardware review, MBC3 latch) |
| A4-10 | low | MBC5 rumble bit has no way out of the core | plan (carried-over list, Stage 3: the board has no motor decision yet) |
| A4-11 | low | Test gaps (list in report) | A4-2/A4-4/A4-5/A4-6 got tests with their fixes; the rest: plan (M10 task) |
| A1-01 | med | Plain STOP mode is a stub (machine keeps running; any button wakes) | fixed (branch m10-stop-mode; ADR 0020 amendment). APU channels keep playing in STOP (SameBoy CGB path): accepted by the human, session 16 ("Thats fine for stop.") |
| A1-02 | low | Speed switch runs `boundary()` with stale deadlines; serial bit can shift twice | fixed e8d960e (confirmed: 2 shifts; also a late scheduled input) |
| A1-04 | low | `VramDma::switch_end` 32-bit, two meanings; stale `RAN` after 8.5-17 min | fixed: `switch_end` is a u64 (confirmed: block dropped 2^31 clocks later) |
| A1-05 | low | State in unused bits/padding because of the `Hw` cap | fixed: IF bits (ADR 0033), `switch_end` and `apu_ds_delay` (3d9a9e8), PPU state (m10-ppu-state), small scalars (inventory theme 1, docs/perf.md "Small-state and attribute pass") |
| A1-06 | med | Speed switch/STOP timing is a composite of three emulators | human: experiment done, session 18 (`docs/reference/speed-switch.md`, 6ee59df); code parked on branch `m10-spsw-div-code` (1437f18, not on `main`): 0 status changes, 4 of 4,031 fingerprints differ. Needs an ADR 0020 decision (STATUS "Needs human") |
| A1-07 | low | Sub-M-cycle access offsets decided per site (= E4) | plan (with the E4 task) |
| A1-08 | low | `idle_speed_switch` spins if the halt end is off the M-cycle grid | fixed 7cc2345 |
| A1-09 | low | Stale comments | plan (with the D8 task) |
| E8 | low | Speed switch re-seeks the PPU walk without syncing first | fixed e4c4713 (confirmed via a late-WY write before the switch) |

## B. Performance

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| B1 | high | Unserviced TIMA overflows each run a scheduler event: 23.7% of worst-case (measured) | fixed 69247dd: worst-case -23.7%, event-storm -28.0%; lazy only while IF.2 is set and IE.2 clear (re-arming from the acknowledge path cost 720 B in `Cpu::run`). Lazy whenever IF.2 is set (inventory L7): measured and dropped, a863b4b (halt-irq +2.8..+4.9%) |
| B2 | high | Serial: one event per bit (~11%, estimate) | fixed 4e13c14 (m10-serial-lazy; ADR 0011 amendment 16faa5b): worst-case -13.9%, event-storm -28.5% |
| B3 | high | 4.02 M x86-Ir line is not a stand-in for 50% of the M7 | human: line stays until an M7 measurement; relative +3% guard is the gate. M7 cycles measured on a NUCLEO-H755ZI-Q in session 17 (20b91b5, docs/perf.md "Measured on a Cortex-M7"); whether that replaces the x86 line is the human's |
| B4 | high (M7) | u64 time arithmetic on every event/IO path | stage 2 (u32 epoch-relative stamps; carried-over task; the clock-offset net is in the gate) |
| B5 | med | Scheduler bookkeeping 9-12% inside `boundary` | fixed 8730f7b (one-pass scheduler dispatch; docs/perf.md "Scheduler and attribute pass") |
| B6 | med | Four checks before every instruction (= C6) | fixed (m10-cpu-loop): one attention test per instruction |
| B7 | med | Tick is load-sub-store-test in memory per M-cycle | fixed: `Hot::t_minus` at offset 0, 4 thumb instructions per tick (was 7) |
| B8 | med | Plain memory access 14-27 Ir before the tick | fixed 61b2ed4, 06e3750: write dispatch reordered with FF00-FFFF in `write_mapped`, read dispatch tests C000-FFFF first (cpu_instrs -7.52%, dma-storm -5.24%); the branch-free WRAM index measured +6.74% and dropped (docs/perf.md "CPU write path", "CPU read path") |
| B9 | med (M7) | `Cpu::run` register pressure, single dispatch branch | stage 2 (carried-over task) |
| B10 | med (M7) | ITCM/DTCM placement still Proposed | stage 2 (ADR 0026 at link time; carried-over task, with C5) |
| B11 | low | PPU sync fixed cost ~300-380 Ir, 3.6x per line | mostly fixed (m10-ppu-walk: inlined no-work test, quiet walk; cgb-raster -5.1%); `next_event` caching left as a lead: plan (M10 task) |
| B12 | low | Full four-channel APU sync per register write | fixed (m10-undo-apu): NR50/NR51 writes append a mixer span instead of syncing four channels; 917 -> 749 Ir per write. Per-channel sync for channel registers: plan (M10 task, needs a bench that shows it) |
| B13 | low | Bounds-check panics in per-line PPU/OAM code | partly fixed (OAM paths panic-free); `latch`, `set_stalls`, `last_fetched`, `mix_planes`, `write_during_dma` keep 1-2 sites: plan (M10 task) |

## C. Code size and MCU fit

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| C1 | med | ITCM list stale, no callee-closure check | fixed 0a424bc (closure check added; margin 2,324 B) |
| C2 | med | Gate measures 16 codegen units; firmware will use 1 | fixed 4db1d02 (+0.6..2.4% Ir, .text 98,204 -> 94,960 B) |
| C3 | med | Total `.text` cap is not an MCU constraint | fixed be036b7 |
| C4 | low | 17 budgets with large slack; `LineObjects::scan` line measures a dead copy | fixed be036b7 (hot caps = measured +5%) |
| C5 | med | Frame buffer kept out of DTCM for a reason that does not hold | stage 2 (carried-over task with B10; docs/perf.md still says AXI SRAM, not corrected in M10) |
| C6 | med | 40 instructions from loop top to dispatch (= B6) | fixed with B6: 28 thumb instructions handler-end to dispatch (was 40) |
| C7 | low | Register pairs stored big-endian (-214 B, 47 fewer `rev`) | fixed: `rev` in `Cpu::run` 71 -> 28 |
| C8 | low | `Memory` field order costs ~500 B of large-offset setup | plan (M10 task; not done) |
| C9 | low | One save-dirty bit for 128 KiB | plan (carried-over task, Stage 2 API, needs an ADR) |
| C10 | low | Three one-level mutual recursions block a static stack bound | plan (carried-over task, Stage 2) |
| C11 | low | Hardware contract says ~200 KiB state; measured 51.8 KiB | Needs human (hardware repo) |

## D. Rust quality

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| D1 | high | `Hw` cap drives state into wrong owners; cap inconsistent and measured on x86 | fixed: cap dropped (human, be036b7 gates); moves landed: IF bits (ADR 0033), PPU fetch state (m10-ppu-state), `switch_end` / `apu_ds_delay` (3d9a9e8), OAM DMA read clears into `Sys` (f79c866), theme 1 scalars. The inventory's other S items were not re-audited one by one |
| D2 | high | `bus.rs`/`Ctx` god-module, nine seams | in progress, session 18 (branch `m10-bus-split`, not on `main`) |
| D3 | med | `Memory` as overflow area; `ObjRank` misnamed; `clear()` != `new()` | fixed (m10-ppu-state) |
| D4 | med | Speed represented six ways; time units bare integers | partly fixed 5e116e5 (one `Speed` enum; Ir and size in docs/perf.md "`Speed` enum", 54c9836). Rest: three M10 plan tasks (`edges.rs` per-speed idiom, `al` argument, `Counter` newtype) |
| D5 | med | Overflow checks/debug asserts never run on the corpus | fixed fc92df4 (`checked` profile; fuzz gate uses it) |
| D6 | med | PPU mid-line code shape (184-line fn, packed bytes, tuples) | partly fixed (`Planes`, `TileLead`, `Refetch`, `Stalls` types); E1 keeps the closed form in Stage 1, so the rest (`hold_fetch`/`write_ppu` split, mode-source enums, renderer scratch) is an M10 plan task |
| D7 | med | `docs/architecture.md` stale in nine places (= E11) | OPEN: M10 plan task (after D2) |
| D8 | med | Stale/roadmap/history comments; broken doc link; no `cargo doc` gate | OPEN: M10 plan task (with A1-09) |
| D9 | med | Test layout (inline 2.4k-line modules, duplicated helpers, test-only field in `Serial`) | OPEN: M10 plan task (helpers, the `Serial` field, file moves; none done) |
| D10 | low | Raw register/LCDC literals | plan (M10 task) |
| D11 | low | `Bus` trait accretion, `LATCHED` sentinel | plan (carried-over task with E5, stage 2) |
| D12 | low | = A4-1 | fixed 35ff843 |
| D13 | low | Inlining conventions undocumented | plan (M10 task) |
| D14 | low | API nits (`Config` non_exhaustive, `run_until(u64::MAX)`) | plan (M10 task; not done) |
| D15 | low | Lint suggestions | plan (M10 task) |
| D16 | low | Style drift | plan (M10 task) |

## E. Holistic design

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| E1 | high | Mid-line PPU is a closed-form image of a FIFO machine; per-write cost unbounded | fixed for Stage 1: test-only dot-stepped mode-3 oracle 0e674bc (`docs/reference/mode3-oracle.md`; its disagreements (a)-(c) settled, open edges (d), (e) and its coverage gaps are M10 plan tasks); promotion into the core: stage 2 (carried-over task) |
| E2 | high | Budgets decide design | fixed be036b7 (gates); state moves landed (see D1) |
| E3 | med | Line timeline encoded 3-4 times | fixed 2529ce6, 574572f (`ppu/timeline.rs`, one row per update point; reads and write views use its dots), session 18. It uncovered a line-0 double-speed write-view bug, fixed ab37d1f (tests seen failing). Leftovers (runtime `state_at` lookups, the mode-3 group with different ends, two dot-4 differences from gambatte): M10 plan tasks |
| E4 | med | Sub-M-cycle timing per-site; line-0 pulse reconciled at three observers | partly fixed: timed requests owned by the PPU (ADR 0033, f5d9f94 line; line 153's LYC=0 c08c22a) instead of IF bits 5-6. OPEN: the `LAND` landing table (M10 plan task, with A1-07) and re-deriving the walk points (A2-02) |
| E5 | med | CPU fetch-overlap modes port gambatte's `prefetched_` | stage 2 (touches `Cpu::run`; wants the real-game net; carried-over task with D11) |
| E6 | med | Net gaps: audio samples, long runs, sync placement, real-game frames | fixed: `check-sync-invariance.sh` (eager-sync and clock-offset features, 30-minute checked run), in the gate. Corpus identical except one known ROM (line-0 mode-2 pulse depends on its deadline placement; goes with E4). Real-game frames: plan (M10 task, needs the human's recordings) |
| E7 | med | Unpinned / single-ROM behaviours | OPEN: M10 plan task (no `UNPINNED:` tag or reference list exists yet) |
| E9 | low | Five hand-written stall-list loops | fixed (`ppu/stalls.rs`) |
| E10 | low | = A2-06 | fixed |
| E11 | low | = D7 | plan (with the D7 task) |

## Log of outcomes

- f29659a, df7a7a9, fc92df4: checked profile, RTC mask, DMA retime clamp
  (A4-3, A4-4, A4-5, D5). The checked run of the fingerprint corpus, core
  tests and the full manifest found no other panic.
- Session 18: A3-1 measured (6753609), D4 first half (5e116e5), E3
  timeline table (2529ce6, 574572f) and the line-0 double-speed fix it
  found (ab37d1f), L7 dropped (a863b4b), A1-06 experiment parked for the
  human (`m10-spsw-div-code`), D2 in progress (`m10-bus-split`). Lows and
  leftovers turned into plan tasks.
