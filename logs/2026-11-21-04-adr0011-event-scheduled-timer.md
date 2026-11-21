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
