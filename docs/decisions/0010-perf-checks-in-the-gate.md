# 0010: Instruction-count benchmark and code-size budgets in the gate

- Status: Accepted
- Date: 2026-11-21

## Context
The README's quality bar requires a deterministic benchmark in the gate
from Stage 1 M2 on (a slowdown of more than a few percent fails), and
code-size budgets for hot functions on the MCU build. Wall-clock timing is
too noisy for a pass/fail check; instruction counts under valgrind are
deterministic for a pinned toolchain and valgrind version.

## Decision
- **Benchmark:** `tools/bench` runs each fixed ROM (cpu_instrs, cgb_sound,
  cgb-acid2) for 20 emulated seconds inside an `#[inline(never)]`
  `run_emulation`. `scripts/check-bench.sh` runs it under
  `valgrind --tool=callgrind --toggle-collect='*run_emulation*'`, so ROM
  loading and process startup aren't counted, and compares the Ir totals
  with `perf/bench-baseline.txt`. More than +3% on any ROM fails.
  - Plain callgrind, not iai-callgrind: no extra installed runner binary
    whose version has to match a crate, and no harness to learn.
- **Code size:** `scripts/check-code-size.sh` builds gbc-core for
  `thumbv7em-none-eabihf` with `-C target-cpu=cortex-m7` and checks the
  symbols in `perf/code-size-budgets.txt` with `nm -S`. A budgeted symbol
  that's missing (renamed or inlined away) fails. The initial budgets are
  the M1 sizes plus about 5%.
- **The baselines and budgets are agent-maintained.** New emulation work
  (PPU, APU, timing) legitimately costs instructions and code. A raise is
  committed together with the change that needs it, with the measured
  numbers and the reason in the commit body, so every raise is
  attributable in `git log perf/`. Improvements should lower the
  baseline in the same way, so later regressions are measured from the
  better number.

## Consequences
- The gate takes about 25 s longer (valgrind).
- The counts are tied to the pinned toolchain and valgrind 3.22. Changing
  either rebaselines in a commit of its own.
- Instruction counts on x86-64 are a proxy for Cortex-M7 cycles, not a
  measurement of them. Stage 2 measures cycles per frame on the board.
