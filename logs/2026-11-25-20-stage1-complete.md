# 2026-11-25-20: stage1-complete

- Milestone: Stage 1 close
- Commits: this commit (docs only)

## Done
- Human declared Stage 1 complete. Recorded in `docs/STATUS.md` and at the
  top of `docs/plan/stage-1.md`. Last full gate pass: `stage1-m10` (b6b59e7).
- Human decision: the Stage 2 dev board (NUCLEO-H753ZI) targets 480 MHz
  (VOS0), not 392 MHz. Checked first for an obvious reason not to: none.
  The board runs from the LDO (VOS0 is only barred under direct SMPS, the
  H755's case) and the part is rev V. Caveats are recorded in STATUS:
  the 480 MHz figures are still scaled from 392 MHz cycles, and flash
  wait states rise.

## Left open
- No Stage 2 plan yet. Next: draft `docs/plan/stage-2.md` for the human.
- Stage 2 plan drafted (`docs/plan/stage-2.md`) and approved by the human
  with changes: dev-board perf report-only (README target moved to the
  H735 at 550 MHz), joystick + PC-serial input, saves over serial,
  per-board crates (`gbc-nucleo-h753`, `gbc-platform`; ADR 0005
  amendment), AGENTS.md dependency rule extended to `gbc-nucleo-h753` (D9).
