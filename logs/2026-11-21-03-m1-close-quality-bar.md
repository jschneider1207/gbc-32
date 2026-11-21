# 2026-11-21-03: M1 close, quality-bar tooling, ADR 0011

- Milestone: M1 (closed, tagged `stage1-m1`) → M2
- Commits: 4240ef6..HEAD

## Done
- M1 closed: core API review (ADR 0009), fix, baseline (45 pass:
  must 42, should 2, stretch 1; no regressions), plan ticked, tag
  `stage1-m1` (annotated) on f2c7098.
- Gate is stricter (ADR 0010, `acceptance/README.md` items 5-7):
  code-size budgets (`scripts/check-code-size.sh`), instruction-count
  benchmark under callgrind (`scripts/check-bench.sh`, +3% fails), and a
  deterministic fuzz pass (`scripts/check-fuzz.sh`, 15,057 inputs, ~9 s).
  Each check was verified to fail when it should.
- Fuzzing: also 0.35 h of cargo-fuzz (nightly, `tools/fuzz/fuzz/`, outside
  the workspace) and 180k extra generated inputs. No core panics found.
- ADR 0011 (lazy, event-scheduled timer): a throwaway prototype measured
  −25…−33% Ir and `run` 37,064 → 32,016 B. Its implementation steps are
  now M2 plan tasks.
- AGENTS.md: stage by explicit path, never `add -A`/`commit -a`.

## Review
- Trigger 3 (core API change, ADR 0009), 1 reviewer, `6fd632e^..HEAD`.
  - Medium, fixed (a98ecb1): `reset()` with no cartridge panicked
    (indexed an empty ROM). It is now a no-op, with a regression test.
  - Low, doc fixed in a98ecb1: the `load` doc claimed load-over equals
    eject-then-load. Held buttons, stop events and palettes carry over.
  - Low, to the plan: `load` over MBC3 drops the RtcState (M4); three
    `hard_rules` hardening items (M2).

## Learned / surprises
- A concurrent agent's MIT-license commit bfc25ea swept in the fuzz agent's
  unfinished `tools/fuzz` files. main still built, and the history was left
  as is. That led to the AGENTS.md staging rule.
- cargo-fuzz's `run` target is slow (~9 exec/s) because each input runs
  many emulated cycles. This is fine for exploration.

## Left open
- The M2 must subset: 15/17. `timer/tima_write_reloading` and
  `timer/tma_write_reloading` fail (the reload window, ADR 0011 step 3).
