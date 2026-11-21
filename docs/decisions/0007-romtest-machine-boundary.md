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

## Amendments

### 2026-11-21: test integrity review

An independent review of romtest's test integrity found ways for a broken
or tampered setup to look like a pass. Changes:

- **Integrity list for `test-roms/`.** `acceptance/test-roms.sha256` holds
  the SHA-256 of every file under `test-roms/` (`sha256sum` format, sorted,
  with a `# release: <version>` header line), written by
  `scripts/hash-test-roms.sh` from the pinned release. Before anything
  runs, romtest checks that the list's release equals the manifest's
  `roms_release` (and so `.version`), that every listed file exists, that
  every listed file hashes to its entry, and that every ROM matched by any
  manifest glob is listed. A screenshot check may only load a listed PNG.
  Any failure is exit 2. Without this, deleting a failing ROM made it stop
  matching its glob, and deleting a reference PNG turned a failure into
  `no-reference`; either way the gate could pass. Everything listed is
  hashed on `--jobs` threads (5101 files, 177 MB for v7.0: 0.27 s on 8
  threads), which is simpler than tracking what a selection would load and
  still cheap. Regenerating the list is only for a new pinned release,
  never to make a run pass. `--roms-dir <path>` points the runner at
  another copy of the ROMs (still verified), for testing the runner.
- **Status precedence: crash/timeout > fail > no-reference > unsupported >
  pass.** Previously `no-reference` came first, so that the score's
  denominator depended only on the files in `test-roms/`. That let a
  missing PNG hide a crash, a timeout or another failing check, so it is
  reversed: `no-reference` (excluded from the score) applies only when
  every other check that could be evaluated passed or was also missing its
  reference. A consequence is that the denominator can move as the
  emulator improves (e.g. the five v7.0 mealybug ROMs without a CGB
  reference count as `timeout` until they reach `LD B,B`, then become
  `no-reference`). That is the price of never hiding a failure.
- **Narrow panic boundary.** Only calls into the emulator core are inside
  `catch_unwind` (`machine::core_call`: `cart_ram_size`, `init`, `load`,
  and every `Machine` method in the run loop). A panic there becomes
  `crash`, as does a load error (the ROM file is hash-verified, so
  rejecting it is the emulator's fault). A panic in romtest's own code (the
  input timeline, checks, allocation, the run loop's bookkeeping) is not
  caught per ROM: it propagates out of the worker (which stops the other
  workers) to `app::main_with`, which maps it to exit 2.
  `#[cfg(not(panic = "unwind"))] compile_error!` stops a `panic = "abort"`
  build of romtest.
- **Baseline rules.**
  - A baseline entry with status `pass` whose `(group, ROM)` is inside the
    current selection (`--group`, `--filter`, `--only-check`; all of them
    in a full run) but has no current result is a regression (`pass ->
    missing`). This catches a ROM dropped by a manifest edit or a renamed
    group. A group that is no longer in the manifest counts as selected
    under `--only-check`, since its checks are unknown. Entries that were
    not `pass` and have disappeared are just dropped by the next
    `--update-baseline`.
  - `--gate` without a baseline is exit 2 (create one with
    `--update-baseline`); otherwise a missing file would silently disable
    regression checking in the gate. `--update-baseline` still creates the
    first baseline, also when combined with `--gate`.
  - A baseline whose `roms_release` differs from the manifest is exit 2,
    unless `--update-baseline` is given; then it is not compared and is
    rewritten.
- **Final frame after the LCD goes off.** The core documents that after
  the LCD is turned off the screen shows white until the next
  `FRAME_READY`, even if the LCD is back on by then. Polling
  `lcd_enabled()` only at the runner's stop points missed an off/on cycle
  within one run call, so the runner could report a frame from before the
  LCD went off. The core now has `Emulator::screen_blank()` (a flag set
  when the LCD is turned off and cleared when a frame completes; see the
  ADR 0006 amendment), the `Machine` trait uses it instead of
  `lcd_enabled`, and the final frame is white if the screen is blank at
  exit or no frame completed.
- **Root-directory ROMs.** `{dir}` is empty for a ROM directly in
  `test-roms/`; `{dir}/` then expands to nothing, so `{dir}/{stem}.png` is
  `stem.png` rather than the absolute `/stem.png`. Any expanded reference
  that is empty, absolute, or has an empty or `..` segment is exit 2, and
  manifest parsing probes each template for both a subdirectory ROM and a
  root ROM.
