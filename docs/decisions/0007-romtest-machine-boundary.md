# 0007: Testing romtest before a CPU exists (Machine trait + fake)

- Status: Accepted
- Date: 2026-11-21

## Context
`romtest` decides every milestone and stage (acceptance/README.md), so its
own logic must be trustworthy before the core can run a real ROM. At M0 the
core is a skeleton: every step is one idle M-cycle, a white frame completes
every 70,224 clocks, and `LD B,B`, serial output and CPU lockups never
happen. A bug in the run loop (input timing, the ld_b_b exit, timeout
versus crash, final-frame selection), in status precedence, or in
regression detection would otherwise only show up once the CPU and PPU
exist, and might then look like an emulator bug, or worse, a pass.

Options considered:
1. Hand-assembled test ROMs. They exercise the real core, but they can't
   run until the CPU exists, which is exactly the period in question.
2. Test the runner only through the real core. At M0 that covers only the
   "run N clocks, white frames" path.
3. A small trait boundary in `romtest`, with the real emulator behind it
   in production and a scriptable fake in tests.

## Decision
Option 3. `tools/romtest/src/machine.rs` defines a `Machine` trait that
mirrors the parts of the core API the runner uses: `set_stop_events`,
`set_buttons`, `run_until(target, frame) -> Events`, `clock`, `registers`,
`serial_byte` and `lcd_enabled`. It is implemented for
`gbc_core::Emulator` with one-line forwards. The run loop
(`machine::run`) is generic over `M: Machine`, so the production path is
monomorphized and has no dynamic dispatch. Nothing changes in `gbc-core`:
the trait lives only in `romtest`.

Tests use `machine::fake::FakeMachine`. It advances in fixed-size steps
(configurable, to simulate instruction overshoot), completes numbered
frames while its LCD is on, and fires scripted actions at chosen clocks:
serial bytes, `LD B,B`, a CPU lockup, a panic, LCD off/on, or a stall.
Above the run loop, the runner takes the backend as a
`dyn Fn(&[u8], &RunPlan, &mut RunState) -> Result<(), String>`. Main
passes the real core. The end-to-end tests (`app/tests.rs`) build a
temporary workspace (manifest, `.version`, ROM files, reference PNGs) in
which each "ROM" is a short text script for the fake. This covers the
whole pipeline: selection, parallel execution, panic catching, checks,
statuses, the JSON/Markdown outputs, failure PNGs, the baseline,
regressions and exit codes.

One test (`real_core_runs_a_blank_rom`) also runs the real core, to keep
the trait implementation honest.

## Consequences
- The runner's pass/fail logic is tested now, independently of emulator
  progress. When the CPU lands, ROM results reflect the core, not runner
  bugs.
- The fake must keep matching the core's documented run semantics (runs
  end at the first instruction boundary at or after the target; ALWAYS_STOP
  events end a call; LD_B_B/SERIAL_TX stop only if enabled). If ADR 0006's
  API changes, update the trait, its `Emulator` impl and the fake together.
- Hand-assembled ROM tests can still be added later for core features. They
  complement this, but don't replace it.
