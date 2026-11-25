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
| A2-02 | med | STAT IRQ model = two reference models spliced; walk a dot early at 0:4 and 153:12, five compensations | partly fixed c598882: the single-speed walk points sit at their true dots (0:5, 153:13) and their delayed-request compensations are gone; gambatte's select/LYC latch is still modelled window by window (`docs/reference/stat-irq.md` "Open"): carried over as a plan task (human, session 18) |
| A2-03 | med | Mid-line state in three structs, invariants held by `% 11` masks | fixed (m10-ppu-state): renderer owns the fetch state; `Stalls` type replaces the masks |
| A2-04 | low | Delayed LYC IRQ in IF bits 5-6 can be stranded/wiped/leaked | fixed (ADR 0033, timed requests in `Ppu::irq`; three scenarios were real behaviour bugs) |
| A2-05 | low | LCDC.5 clear early on the WY line loses the WY latch for the frame | fixed 65f00bd (confirmed; SameBoy `wy_check` + Pan Docs) |
| A2-06 | low | `obj_size_fetch` patches VRAM to pass a parameter (= E10) | fixed (m10-ppu-state) |
| A2-07 | low | Write landing at `now - 1` on the mode-3 end dot keeps the old value for pixel 159 | plan (already the M8 "PPU edge cases" task) |
| A2-08 | low | Window zero pixel on CGB rests on one Mealybug capture, against SameBoy | rejected: the capture is CGB C hardware, our target (ADR 0003); tagged as single-source (E7) |
| A2-09 | low | `Memory::clear` skips `obj_rank.hold`, `win_zero`, `dma_read_clears` (= D3) | fixed (m10-ppu-state; test seen failing) |
| A2-10 | low | TILE_SEL set-glitch falls back to the old calibration when the latch is unknown | plan (M10 task; the oracle exists, 0e674bc) |
| A2-11 | low | `stat_during_dma`, `fix_attrs` `d - 5`, DMG `POST_BOOT_POS` single-source | fixed with E7 d900f7b (`stat-during-dma`, `dma-fetch-read-dot`, `post-boot-pos-dmg-mode` in `docs/reference/unpinned.md`) |
| A3-1 | med | 128-clock box filter leaves aliasing on high notes | fixed 7d389ce: box 64 + 8-tap FIR (ADR 0035, 2202bd3; the human chose it, session 18), measured 6753609 (`docs/reference/audio-aliasing.md`, ratchet `tests/apu_aliasing.rs` re-recorded). docs/perf.md's 230k "channels and mixing" budget no longer describes the APU: a new figure is the human's (plan task) |
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
| A1-06 | med | Speed switch/STOP timing is a composite of three emulators | fixed 2821437: AGE model for two-byte STOPs (DIV resets after STOP's last byte), with the ADR 0020 amendment the human approved (session 18); experiment in `docs/reference/speed-switch.md` (6ee59df) |
| A1-07 | low | Sub-M-cycle access offsets decided per site (= E4) | plan (with the E4 task) |
| A1-08 | low | `idle_speed_switch` spins if the halt end is off the M-cycle grid | fixed 7cc2345 |
| A1-09 | low | Stale comments | fixed with D8 (8dce1b6) |
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
| D2 | high | `bus.rs`/`Ctx` god-module, nine seams | fixed: `bus/` split merged, f8ab2a5..6f74775 (session 18) |
| D3 | med | `Memory` as overflow area; `ObjRank` misnamed; `clear()` != `new()` | fixed (m10-ppu-state) |
| D4 | med | Speed represented six ways; time units bare integers | partly fixed 5e116e5 (one `Speed` enum; Ir and size in docs/perf.md "`Speed` enum", 54c9836). Rest: three M10 plan tasks (`edges.rs` per-speed idiom, `al` argument, `Counter` newtype), carried over (human, session 18) |
| D5 | med | Overflow checks/debug asserts never run on the corpus | fixed fc92df4 (`checked` profile; fuzz gate uses it) |
| D6 | med | PPU mid-line code shape (184-line fn, packed bytes, tuples) | partly fixed (`Planes`, `TileLead`, `Refetch`, `Stalls` types); E1 keeps the closed form in Stage 1, so the rest (`hold_fetch`/`write_ppu` split, mode-source enums, renderer scratch) is an M10 plan task, carried over (human, session 18) |
| D7 | med | `docs/architecture.md` stale in nine places (= E11) | fixed a3be8f3 (after D2); stale identifiers and paths in ADRs, perf.md and reference notes a1a499d |
| D8 | med | Stale/roadmap/history comments; broken doc link; no `cargo doc` gate | fixed 8dce1b6 (comment sweep), a1a499d (docs), ac37969 (gate denies rustdoc warnings in gbc-core) |
| D9 | med | Test layout (inline 2.4k-line modules, duplicated helpers, test-only field in `Serial`) | fixed b2c5b23 (one crate-level `testkit`), cdc329c (eager serial reference in a test-only `EagerSerial` wrapper), 77b64be (`ppu` tests split in three); `lib.rs`'s two inline test modules (~2,600 lines) are not split: plan task |
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
| E4 | med | Sub-M-cycle timing per-site; line-0 pulse reconciled at three observers | partly fixed: timed requests owned by the PPU (ADR 0033, f5d9f94 line; line 153's LYC=0 c08c22a) instead of IF bits 5-6. Walk points re-derived c598882 (A2-02). The `LAND` landing table is not done: carried over as a plan task (human, session 18; with A1-07) |
| E5 | med | CPU fetch-overlap modes port gambatte's `prefetched_` | stage 2 (touches `Cpu::run`; wants the real-game net; carried-over task with D11) |
| E6 | med | Net gaps: audio samples, long runs, sync placement, real-game frames | fixed: `check-sync-invariance.sh` (eager-sync and clock-offset features, 30-minute checked run), in the gate. Corpus identical except one known ROM (line-0 mode-2 pulse depends on its deadline placement; goes with E4). Real-game frames: plan (M10 task, needs the human's recordings) |
| E7 | med | Unpinned / single-ROM behaviours | fixed d900f7b: `UNPINNED:` tags in code, `docs/reference/unpinned.md`, `tests/unpinned.rs` keeps the two in step |
| E9 | low | Five hand-written stall-list loops | fixed (`ppu/stalls.rs`) |
| E10 | low | = A2-06 | fixed |
| E11 | low | = D7 | fixed with D7 (a3be8f3) |

## Milestone-close review (session 18)

AGENTS.md "Independent review" at milestone close: the diff
`stage1-m9..2821437`, 2026-11-24, session 18, four fresh read-only
reviewers in parallel: T test integrity, P performance and portability,
V PPU correctness, H non-PPU correctness (CPU, bus, timer, APU, DMA,
cart). Triage by the orchestrator; outcome keys as above. No high
finding; the mediums are C-P1, C-P2 and C-V1 (C-T4 was A3-1, open at
review time).

| ID | Sev | Finding | Outcome |
|---|---|---|---|
| C-T1 | low | The gate did not unset `CODE_SIZE_BUDGETS_FILE`, `ITCM_SET_FILE` or `TEST_ROMS`, so an exported override could point a check at other inputs | fixed 3b9db31 (the gate unsets them) |
| C-T2 | low | `check-sync-invariance.sh` never proved the eager-sync build had the feature on | fixed 78f4c06 (checks gbc-core's feature set per build; seen failing) |
| C-T3 | low | `tests/apu_aliasing.rs` ratchet compares host f64 dB at ±0.05 dB; could flap on another libm | rejected: since 7d389ce the input is the core's integer samples, checked bit-exact against the integer model (`model_matches_the_core`); libm differences in the f64 DFT/`log10` are ~1e-12 dB, and no recorded level sits near the 1e-12 clamp floor (all are above -38 dB or `INF`) |
| C-T4 | med | A3-1 aliasing still open | fixed 7d389ce (box 64 + 8-tap FIR, ADR 0035, human approved) |
| C-T5 | low | The `synth:worst-case` workload changed in the range (a serviced 16 kHz timer) | accepted: wave 4 planned the serviced timer IRQ with the human, the 16 kHz rate was the agent's choice; the perf reviewer measured the program as it is |
| C-T6 | low | `tools/h7-bench` is its own workspace, outside the gate's fmt/clippy/test | plan (M10 task) |
| C-T7 | low | The gate-step list in `acceptance/README.md` omits the SM83, ITCM-set and invariance steps the script runs | plan (M10 task; stricter) |
| C-P1 | med | `ObjectLines::build` reran bit by bit on every line after an OAM write; no bench covered it | fixed 86d01ef (word-wide build, one line late; new program `synth:oam-lines` 377b8ec: 2.016 M -> 1.282 M Ir/frame; tests seen failing) |
| C-P2 | med | docs/perf.md "Outside the bound" names only PPU writes; dense APU writes, KEY1+STOP loops, DIV/TAC loops and back-to-back GDMA also exceed the benched worst case (reviewer estimates); the table is stale | in progress, session 18 (branch `m10-storm`: measure and document; mitigations are a later decision for the human) |
| C-P3 | low | Per-function size report stale: `Ctx::div_reset` is gone, 32 of 61 lines are "over", `Apu::flush` and `hold_fetch` have no line | plan (M10 task) |
| C-P4 | low | Bench baseline behind the tip | done at close (baseline re-recorded, session 18) |
| C-P5 | low | `.text` grew 95,652 -> ~116 KB in the range, ~5.5% margin; the gate is provisional (it excludes compiler_builtins and core panic code); 5.8 KB of nop padding unmeasured on the M7; fmt code (2,218 B) counts against the gate | plan (carried-over task, Stage 2) |
| C-P6 | low | Panic sites left in release: `panic_bounds_check` in `latch`, `write_during_dma`; `slice_index_fail` in `set_stalls`, `last_fetched`, `mix_planes` (x2), `apply_inputs`, `set_buttons_at`; `panic_fmt` in `set_buttons_at`; a divide-by-zero check in `speed_switch_halt` (`bus/stop.rs`) | plan (the B13 task, extended) |
| C-P7 | low | ~1.7 KB of anonymous `.rodata` not matched by the `.dtcm_rodata` globs | plan (carried-over task, Stage 2) |
| C-P8 | low | ~50 `#[inline(never)]` in `bus/` without an individual measurement | plan (already the M10 "inventory A14 attributes one at a time" task) |
| C-V1 | med | Spurious STAT interrupt, or a held STAT line, for STAT/LYC writes at dots ~450-455 of VBlank lines: the mode-0 latch applied where no mode-0 interrupt exists | fixed d614c28 (confirmed; tests split, each seen failing, f0a96ec) |
| C-V2 | low-med | The walk's LY=LYC interrupt reached IF 4 dots late when a LYC write lands a dot before the comparison | fixed 4619272 (gambatte-backed); line 153's same gap is left, `UNPINNED` `l153-lyc-write-before-cmp` |
| C-V3 | low | LYC write at 152:455: comment and code seemed to disagree | comment clarified efd4d32 (they agree) |
| C-V4 | low | The line-0 mode-0 read quirk is one dot wide; the comment said one M-cycle | comment fixed efd4d32; pinned by two `lcd_offset` ROMs |
| C-V5 | low | Double speed: the STAT phase-2 rule at a line's dot 0 does not look back to the previous line's dot 455 (benign today) | plan (M10 task) |
| C-H1 | low | Double speed with `ds_delay`: a DIV reset on the edge clock can drop a delayed frame-sequencer step (`bus/apu_io.rs` `apu_div_reset`, `set_div_apu_deadline`) | plan (M10 task) |
| C-H2 | low | STOP mode defers a pending sweep event until wake (`bus/stop.rs`) | plan (M10 task) |
| C-H3 | low | VRAM DMA ignores VRAM write blocking in mode 3 (SameBoy writes the other bank); not stated at the site, no `UNPINNED:` tag | plan (M10 task) |
| C-H4 | low | Stale comment `hdma.rs` ~15: "an interrupt dispatch due then goes first", but the code runs the block first | plan (M10 task) |
| C-H5 | low | HDMA re-arm can drop an already requested block in a one-M-cycle window (SameBoy agrees with the code, gambatte does not) | plan (M10 task) |

The PPU reviewer did not read `mix_planes`, `tile_sel_glitch`,
`carry_sel`, `ppu/mode3_oracle.rs` or the OAM-DMA scan paths.

Found while closing, not by a reviewer: `check-sync-invariance.sh --long`
had failed for `synth:dmg-raster` since the program was added (2bb845e):
the check assumed the LCD on from clock 0. Fixed d017dd5 (frames counted
from the first one, each within 23 clocks of its due clock; human
approved; seen failing two ways). Lesson: `--long` was not part of the
per-merge checks, so a full gate run was the first to see it.

## Log of outcomes

- f29659a, df7a7a9, fc92df4: checked profile, RTC mask, DMA retime clamp
  (A4-3, A4-4, A4-5, D5). The checked run of the fingerprint corpus, core
  tests and the full manifest found no other panic.
- Session 18: A3-1 measured (6753609), D4 first half (5e116e5), E3
  timeline table (2529ce6, 574572f) and the line-0 double-speed fix it
  found (ab37d1f), L7 dropped (a863b4b), A1-06 experiment parked for the
  human (`m10-spsw-div-code`), D2 in progress (`m10-bus-split`). Lows and
  leftovers turned into plan tasks.
- Session 18, later: D2 merged (f8ab2a5..6f74775), D7/D8 (8dce1b6,
  a3be8f3, a1a499d; `cargo doc` gate step ac37969), E7 (d900f7b), D9
  (b2c5b23, cdc329c, 77b64be; `lib.rs` tests left), A2-02 walk points
  (c598882), A1-06 landed (2821437, ADR 0020 amendment) and A3-1 fixed
  (7d389ce, ADR 0035), both on the human's decision; A2-02's latch, E4's
  `LAND` table and the D4/D6 rests carried over as plan tasks (human).
  Milestone-close review of `stage1-m9..2821437` run and triaged (section
  above): C-V1, C-P1 fixed, C-P2 in progress; the `--long` invariance
  failure fixed (d017dd5); bench baseline re-recorded.
