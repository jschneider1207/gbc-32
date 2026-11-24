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
one-line task in the plan).

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
| A2-02 | med | STAT IRQ model = two reference models spliced; walk a dot early at 0:4 and 153:12, five compensations | wave 5 (with E3/E4) |
| A2-03 | med | Mid-line state in three structs, invariants held by `% 11` masks | wave 5 (after budget reform) |
| A2-04 | low | Delayed LYC IRQ in IF bits 5-6 can be stranded/wiped/leaked | wave 5 (typed `Irq` event, E4) |
| A2-05 | low | LCDC.5 clear early on the WY line loses the WY latch for the frame | fixed 65f00bd (confirmed; SameBoy `wy_check` + Pan Docs) |
| A2-06 | low | `obj_size_fetch` patches VRAM to pass a parameter (= E10) | wave 5 |
| A2-07 | low | Write landing at `now - 1` on the mode-3 end dot keeps the old value for pixel 159 | plan |
| A2-08 | low | Window zero pixel on CGB rests on one Mealybug capture, against SameBoy | rejected: the capture is CGB C hardware, our target (ADR 0003); tagged as single-source (E7) |
| A2-09 | low | `Memory::clear` skips `obj_rank.hold`, `win_zero`, `dma_read_clears` (= D3) | wave 5 |
| A2-10 | low | TILE_SEL set-glitch falls back to the old calibration when the latch is unknown | plan (oracle, wave 6) |
| A2-11 | low | `stat_during_dma`, `fix_attrs` `d - 5`, DMG `POST_BOOT_POS` single-source | plan (tag unpinned, E7) |
| A3-1 | med | 128-clock box filter leaves aliasing on high notes | wave 6: measure first; a fix that costs Ir goes to the human with numbers |
| A3-2 | med | No independent check of produced samples | wave 3 |
| A3-3 | low | `Square::since_step` uses the next-reload period after an NRx3 write | wave 2 |
| A3-4 | low | Five fitted trigger windows; the two square trigger rules are one rule | plan |
| A3-5 | low | Output headroom (±2x full scale after high-pass) not in the API contract | wave 2 (doc) |
| A3-6 | low | Stale sweep overflow check survives a CH1 retrigger | wave 2 |
| A3-7 | low | Desktop DC blocker starts from zero (click) | plan (desktop is throwaway) |
| A4-1 | med | Unsupported mappers run as ROM-only with battery-flagged RAM (= D12) | fixed 35ff843 (ADR 0031; independent API review: no high/medium). HuC1 + HuC3 (human): wave 2, open |
| A4-2 | med | HDMA cancel with a block pending copies `value + 1` blocks | wave 2 |
| A4-3 | med | Fuzz pass built without overflow checks / debug asserts (= D5) | fixed fc92df4 |
| A4-4 | low | `dma_retime` underflow on a switch to single speed right after reset | fixed df7a7a9 (confirmed: test failed before) |
| A4-5 | low | `RtcState::advance_seconds` underflow on an unmasked state | fixed f29659a (confirmed) |
| A4-6 | low | Re-arming an active HDMA inside HBlank starts a second block (references disagree) | plan |
| A4-7 | low | RP (FF56) unimplemented (= A1-03) | wave 2 |
| A4-8 | low | MBC1M detection accepts two blank logo areas | fixed 2de0ffb (confirmed) |
| A4-9 | low | MBC3 latch edge rule unverified | plan (already a task) |
| A4-10 | low | MBC5 rumble bit has no way out of the core | plan (Stage 3 board has no motor decision yet) |
| A4-11 | low | Test gaps (list in report) | wave 3 where cheap |
| A1-01 | med | Plain STOP mode is a stub (machine keeps running; any button wakes) | wave 2 |
| A1-02 | low | Speed switch runs `boundary()` with stale deadlines; serial bit can shift twice | wave 2 (with E8) |
| A1-04 | low | `VramDma::switch_end` 32-bit, two meanings; stale `RAN` after 8.5-17 min | wave 5 (own field after reform) |
| A1-05 | low | State in unused bits/padding because of the `Hw` cap | wave 5 |
| A1-06 | med | Speed switch/STOP timing is a composite of three emulators | wave 5: branch experiment (DIV reset after STOP's 2nd byte) |
| A1-07 | low | Sub-M-cycle access offsets decided per site (= E4) | wave 5 |
| A1-08 | low | `idle_speed_switch` spins if the halt end is off the M-cycle grid | wave 2 (`n.max(1)`) |
| A1-09 | low | Stale comments | wave 5 (docs sweep) |
| E8 | low | Speed switch re-seeks the PPU walk without syncing first | wave 2 |

## B. Performance

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| B1 | high | Unserviced TIMA overflows each run a scheduler event: 23.7% of worst-case (measured) | fixed 69247dd: worst-case -23.7%, event-storm -28.0%; lazy only while IF.2 is set and IE.2 clear (re-arming from the acknowledge path cost 720 B in `Cpu::run`) |
| B2 | high | Serial: one event per bit (~11%, estimate) | wave 4 |
| B3 | high | 4.02 M x86-Ir line is not a stand-in for 50% of the M7 | human: line stays until an M7 measurement; relative +3% guard is the gate. Better proxies (QEMU thumb counts, i686 valgrind) need an install: Needs human |
| B4 | high (M7) | u64 time arithmetic on every event/IO path | stage 2 (u32 epoch-relative stamps; needs the clock-offset net, E6) |
| B5 | med | Scheduler bookkeeping 9-12% inside `boundary` | wave 4 |
| B6 | med | Four checks before every instruction (= C6) | wave 4 |
| B7 | med | Tick is load-sub-store-test in memory per M-cycle | wave 4 |
| B8 | med | Plain memory access 14-27 Ir before the tick | wave 4 |
| B9 | med (M7) | `Cpu::run` register pressure, single dispatch branch | stage 2 |
| B10 | med (M7) | ITCM/DTCM placement still Proposed | stage 2 (ADR 0026 at link time) |
| B11 | low | PPU sync fixed cost ~300-380 Ir, 3.6x per line | wave 5 (timeline table, E3) |
| B12 | low | Full four-channel APU sync per register write | plan |
| B13 | low | Bounds-check panics in per-line PPU/OAM code | plan |

## C. Code size and MCU fit

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| C1 | med | ITCM list stale, no callee-closure check | fixed 0a424bc (closure check added; margin 2,324 B) |
| C2 | med | Gate measures 16 codegen units; firmware will use 1 | fixed 4db1d02 (+0.6..2.4% Ir, .text 98,204 -> 94,960 B) |
| C3 | med | Total `.text` cap is not an MCU constraint | fixed be036b7 |
| C4 | low | 17 budgets with large slack; `LineObjects::scan` line measures a dead copy | fixed be036b7 (hot caps = measured +5%) |
| C5 | med | Frame buffer kept out of DTCM for a reason that does not hold | stage 2 (placement plan in docs/perf.md corrected in wave 5) |
| C6 | med | 40 instructions from loop top to dispatch (= B6) | wave 4 |
| C7 | low | Register pairs stored big-endian (-214 B, 47 fewer `rev`) | wave 4 |
| C8 | low | `Memory` field order costs ~500 B of large-offset setup | wave 5 |
| C9 | low | One save-dirty bit for 128 KiB | plan (Stage 2 API, needs an ADR) |
| C10 | low | Three one-level mutual recursions block a static stack bound | plan |
| C11 | low | Hardware contract says ~200 KiB state; measured 51.8 KiB | Needs human (hardware repo) |

## D. Rust quality

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| D1 | high | `Hw` cap drives state into wrong owners; cap inconsistent and measured on x86 | wave 1 (cap dropped, human) then wave 5 (moves) |
| D2 | high | `bus.rs`/`Ctx` god-module, nine seams | wave 5 (after `codegen-units = 1`) |
| D3 | med | `Memory` as overflow area; `ObjRank` misnamed; `clear()` != `new()` | wave 5 |
| D4 | med | Speed represented six ways; time units bare integers | wave 5 (`Speed` enum first) |
| D5 | med | Overflow checks/debug asserts never run on the corpus | fixed fc92df4 (`checked` profile; fuzz gate uses it) |
| D6 | med | PPU mid-line code shape (184-line fn, packed bytes, tuples) | wave 5/6 (follows E1's decision) |
| D7 | med | `docs/architecture.md` stale in nine places (= E11) | wave 5 |
| D8 | med | Stale/roadmap/history comments; broken doc link; no `cargo doc` gate | wave 5 |
| D9 | med | Test layout (inline 2.4k-line modules, duplicated helpers, test-only field in `Serial`) | wave 5 (helpers + the `Serial` field); file moves: plan |
| D10 | low | Raw register/LCDC literals | plan |
| D11 | low | `Bus` trait accretion, `LATCHED` sentinel | plan (with E5, stage 2) |
| D12 | low | = A4-1 | fixed 35ff843 |
| D13 | low | Inlining conventions undocumented | wave 5 (architecture.md) |
| D14 | low | API nits (`Config` non_exhaustive, `run_until(u64::MAX)`) | wave 5 |
| D15 | low | Lint suggestions | plan |
| D16 | low | Style drift | plan |

## E. Holistic design

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| E1 | high | Mid-line PPU is a closed-form image of a FIFO machine; per-write cost unbounded | wave 6: test-only dot-stepped mode-3 oracle; promotion into the core is stage 2 (needs M7 numbers) |
| E2 | high | Budgets decide design | fixed be036b7 (gates), state moves in wave 5 |
| E3 | med | Line timeline encoded 3-4 times | wave 5 |
| E4 | med | Sub-M-cycle timing per-site; line-0 pulse reconciled at three observers | wave 5 (`Irq` event, landing table) |
| E5 | med | CPU fetch-overlap modes port gambatte's `prefetched_` | stage 2 (touches `Cpu::run`; wants the real-game net) |
| E6 | med | Net gaps: audio samples, long runs, sync placement, real-game frames | wave 3 |
| E7 | med | Unpinned / single-ROM behaviours | wave 5 (tag in code + list in docs/reference) |
| E9 | low | Five hand-written stall-list loops | wave 5 |
| E10 | low | = A2-06 | wave 5 |
| E11 | low | = D7 | wave 5 |

## Log of outcomes

- f29659a, df7a7a9, fc92df4: checked profile, RTC mask, DMA retime clamp
  (A4-3, A4-4, A4-5, D5). The checked run of the fingerprint corpus, core
  tests and the full manifest found no other panic.
