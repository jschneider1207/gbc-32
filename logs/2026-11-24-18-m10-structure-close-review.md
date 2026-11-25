# 2026-11-24-18: m10-structure-close-review

- Milestone: M10
- Commits: 63ff596..b6b59e7 (tag `stage1-m10`), plus this log

## Done
- Structure: `Speed` enum, line timeline table, `bus/` split (0 B, 0 Ir),
  test layout, docs/comment sweep, `UNPINNED:` tags with a list and a test.
- Human decisions landed: speed switch/STOP on AGE's model (ADR 0020
  amendment), output filter box 64 + 8-tap FIR (ADR 0035), `--long` frame
  check counted from the first frame.
- Fixes with tests seen failing: line 0 double-speed write view, walk
  points at 0:5 / 153:13, VBlank mode-0 latch, late-LYC-write interrupt
  dot, object-line summary rebuild (new gated bench line).
- Gate made stricter: env overrides unset, eager-sync feature proof,
  `cargo doc` step, `worst-case@0` in the invariance corpus.
- Bench baseline re-recorded (9030669); lows and leftovers are plan tasks.

## Learned / surprises
- STATUS item 3 was stale: an agent found its whole brief already on
  `main`. Verify "left" lists against `docs/perf.md` and the code.
- `check-sync-invariance.sh --long` had failed since `synth:dmg-raster`
  was added (session 17): no per-merge check runs `--long`. Not a core
  bug: the check assumed the LCD on from clock 0.
- L7 (timer lazy whenever IF bit 2 is set) costs 45-80 Ir per serviced
  interrupt and saves nothing: dropped with numbers.
- The filter's negative taps overshoot a step by 7.6%: unity gain and
  "never exceeds i16" cannot both hold; it saturates.
- A test whose first assertion fails hides whether the later ones can:
  the VBlank latch test was split so each was seen failing.
- An agent's `rm` of a `test-roms` symlink in its worktree was denied;
  briefs now say to pass the ROM root as an argument.
- Two agents died on API 529s; resuming them by message lost nothing.

## Review
- Trigger: milestone close, `stage1-m9..2821437`; four reviewers (PPU
  correctness, non-PPU correctness, test integrity, performance).
  Findings and outcomes: `docs/reviews/stage1-m10.md`, "Milestone-close
  review (session 18)". C-V1, C-V2, C-P1, C-T1, C-T2 fixed; C-P2 measured
  on `m10-storm`; C-T3 rejected (the ratchet is integer-exact now); lows
  are plan tasks.
- Gate scripts changed, stricter only, plus the human-approved `--long`
  rewrite (stricter per frame, but dmg-raster now passes: escalated first).

## Left open
- Tagged `stage1-m10` at b6b59e7 after a full gate pass; declaring Stage 1
  complete is the human's.
- After 2821437: the filter and the gate-check edits each got their own
  review (fixes 99f7d4f, d1759e7; one medium each: a weakened envelope
  test, an unbounded first frame in `--long`). The latch and object-line
  fixes were not re-reviewed (AGENTS.md: no re-review of fixes).
