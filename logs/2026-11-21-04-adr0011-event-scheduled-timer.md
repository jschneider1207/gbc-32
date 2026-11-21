# 2026-11-21-04: ADR 0011 event-scheduled timer

- Milestone: M2
- Commits: 7ebc7a5..HEAD

## Done
- ADR 0011 steps 1-5, all fingerprint-identical where they should be
  (`scripts/diff-fingerprint.sh`, 295 ROMs):
  - step 2 ran first (7ebc7a5): `bench --fingerprint` + `diff-fingerprint.sh`,
    because step 1's check needs the tool in its parent commit;
  - step 1 (f74cceb): `schedule.rs` event table, PPU on it; `Sys` made
    `repr(C)` with bus-path fields first (field order moved `run` ~1 KB);
  - step 3 (0f3d68a): cycle A/B reload window in the eager timer. M2
    acceptance subset 15/17 → 17/17;
  - step 4 (4b81a49): lazy timer + serial clock, eager timer kept as
    `timer/reference.rs` (test-only), differential test (2000 sequences).
    Ir −28%/−33%/−25%, `run` 36,416 → 30,896 B; budgets tightened;
  - step 5 (84c7744): halted/stopped/locked/off/speed-switch pause idle to
    the next deadline. acid2 697 M → 13.1 M Ir.
- `hard_rules` test hardening (a2a781f).
- AGENTS.md rule (1917568, from the human): a test counts only once it has
  been seen to fail; mutation-check every new test.

## Learned / surprises
- A sub-agent reported a test "would fail" from reading the code. The human
  rejected that; the orchestrator ran the mutations (all 5 failed as
  expected). Every later brief required a mutation table.
- Glitch increment during cycle A is impossible (selected bit and all below
  are 0 right after the fall); ADR 0011 text clarified.
- Equivalent mutant: DIV rebase `old & !0xFFFF` instead of `(old|0xFFFF)+1`
  is unobservable today (all counter-relative state is reset at the
  rebase); `+1` kept for monotonicity.
- The fingerprint's differing path was verified (VBlank +1 M-cycle: 272/295
  differ, exit 1).

## Left open
- HALT tasks, OAM DMA, `set_buttons_at`, ADR 0011 open hardware questions.
- romtest baseline is still M1's (two timer ROMs show fail→pass).

## Later in the session
- HALT tasks (3ee75c7), scheduled inputs ADR 0012 (acffc11..aadd26b, one
  API reviewer: 2 medium doc/runner findings fixed in 1b79d45/aadd26b; late
  inputs now `crash`), OAM DMA (7bdaee2, must 44 → 60/86), DMA hot-path
  layout (75b12ad, bench under the old baseline), gate tightened (586ca1e).

## Review (M2 close, stage1-m1..HEAD; 3 reviewers)
Hardware correctness:
- H1 medium (verified) serial clock: SameBoy master flip-flop on bit 7 falls,
  reset by SC writes; gambatte serial 39/46 → fix (also closes ADR 0011 Q2).
- H2 medium (verified) dispatch clears IF too early (SameBoy clears 2 T
  into the 5th M-cycle) → fix.
- H3 medium (verified) speed switch: ~0x20008-clock halt with DIV/TIMA
  running, DMA paused, not a 2050 M-cycle freeze → M5 plan task (speed
  switch is M5 scope; ADR 0011 already defers the pause length to M5).
- H4 medium (by reading) OAM DMA must pause during HALT (SameBoy
  `GB_dma_run` returns while halted; gambatte oamdmasrc80_halt_*) → fix,
  plus correct docs/reference/oam-dma.md.
- H5 low: STOP's second byte → M5 plan task.
- Q1 (CGB timer-disable glitch): keep SameBoy behavior; rapid_toggle and
  gambatte tima need it. Record in ADR 0011 → with the H1 fix.
Test integrity:
- I1 medium (verified) check-bench/check-fuzz run a stale binary under
  CARGO_TARGET_DIR → fix.
- I2 medium fingerprint coverage too narrow (no VRAM/OAM/HRAM/cart RAM/
  PPU-APU regs/frame, missing suites, no inputs) → fix.
- I3 low `--seconds 0`, I4 low dirty-tree / partial root, I8 low perf.md
  stale text → fixed with I2 (same files).
- I5 low: 586ca1e body says "stricter" but cpu_instrs baseline rose
  +0.40%. Accepted: the wording was wrong for that ROM (the gate as a whole
  got tighter); can't rewrite history; noted here.
- I6 low: 7bdaee2 committed with check-bench red until 75b12ad. Accepted as
  a process miss; agents must not commit a red perf check.
- I7 low: 8ede259 called exact-M-cycle input timing "stricter"; it is a
  reinterpretation the README had pre-announced for M2, with zero status
  changes. Recorded; told the human.
Performance/portability:
- P1 medium (measured) ~186 Ir per event (linear rescans); DS timer+fast
  serial storm ≈ 54% of an M7 frame → fix (incremental min).
- P3 medium (measured) bench has no DMA/double-speed/event-dense ROM → fix
  (synthetic ROMs built by tools/bench, no ROM files in git).
- P2 low u64 division on DMA paths (`__aeabi_uldivmod`), P4 low missing
  budgets (boundary, read/write_during_dma, div_reset, stop), P6 low double
  reschedule in div_reset → fixed with P1 (same code).
- P5 low `run`+`execute_cb` 41.7 KB vs 16 KiB I-cache / 40 KB plan target
  → plan task (MCU stage).
