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

### 2026-11-21: M0 milestone-close review

The M0 milestone-close reviews found more gaps. Changes:

- **Vanished baseline entries are regressions in a full run.** In a run
  without `--filter`, `--group` or `--only-check` (with or without
  `--gate`), every baseline `(group, ROM)` with no current result is a
  regression whatever its baseline status, printed as `<status> ->
  missing` (e.g. `timeout -> missing`), and the run exits 1. Previously
  only `pass` entries were checked, so a failing ROM could leave the
  baseline silently through a manifest edit. `--update-baseline` is
  refused while any such entry exists. The only way to drop entries is
  `--update-baseline --allow-removed` (`--allow-removed` alone is a usage
  error), which prints every removed key, records them in the results JSON
  (`removed_from_baseline`) and Markdown, and still refuses on any other
  regression. Partial runs are unchanged: they compare only what is in
  scope, and an in-scope `pass` entry with no result is a regression
  (`pass -> missing`).
- **Status precedence is now crash/timeout > fail > unsupported >
  no-reference > pass.** `unsupported` is scored as a failure, so a ROM
  with a missing PNG and an unsupported check (e.g. a Gambatte ROM before
  M5) counts as `unsupported`, not as excluded. `no-reference` applies only
  when every other check passed.
- **`-h`/`--help` exits 0 only as the sole argument.** With any other
  argument it is a usage error (exit 2), so a command line with a stray
  `--help` can't look like a successful run.
- **Loaded bytes are hashed.** The up-front check could pass and the file
  change before a job read it. Now every ROM and reference PNG a run reads
  is hashed again from exactly the bytes read and compared with the
  in-memory integrity list (exit 2 on a mismatch or an unlisted file). The
  up-front pass still hashes every listed file, which keeps
  "every listed file exists and matches" a once-per-run guarantee
  independent of the selection. The extra cost is small, so both passes
  are kept rather than skipping files that will be loaded: on the full
  stage 1 manifest (v7.0, 8 jobs) the up-front pass hashes 5101 files
  (176.9 MB) in 0.29 s, and the load-time checks hash 3439 loads
  (137.8 MB) in 1.09 s of CPU summed over the 8 workers (about 0.14 s of
  wall time) out of a 9.7 s run.
- **No-progress guard catches stalled time with events.** A run call that
  returns without advancing the clock and with no event was already
  `crash` (`no_progress`). Now `MAX_STALLED_CALLS` (1000) consecutive run
  calls without a clock advance are also `crash`, even if every call
  reports events (e.g. a core stuck reporting `SERIAL_TX`), so such a core
  can't spin until the ld_b_b timeout or forever. Every real run call
  executes at least one instruction, so the bound is generous.
- **Final frame after the LCD is turned on.** The core no longer displays
  the first frame after an LCD enable (ADR 0006 amendment, "M0 hardware
  review"): `screen_blank()` stays true through that `FRAME_READY`, so the
  runner's final frame stays white until the second frame. The fake
  machine mirrors this, and fires `FRAME_READY` at the start of VBlank
  (65,664 clocks into each 70,224-clock frame) like the core.
- **Hard rules are tested beyond the gate's grep.**
  `crates/gbc-core/tests/hard_rules.rs` tokenizes every file under
  `crates/gbc-core/src` (comments and literals removed, so whitespace and
  line breaks don't matter) and fails on `extern crate alloc|std` or a
  path rooted at `std`/`alloc` outside test code, on any `#[path]` (also
  in `cfg_attr`) or `include!`, on a missing `#![no_std]`, and on any
  entry in a dependency table of `crates/gbc-core/Cargo.toml` (dev and
  build dependencies included). Test code is, conservatively, only the
  body of an inline `#[cfg(test)] mod name { ... }`; `#[cfg(test)]` on
  any other item does not exempt it.

Deliberate interpretations of acceptance/README.md, recorded so they are
not mistaken for bugs:

- **`--gate` with a selection scores only the selection.** Combined with
  `--filter`, `--group` or `--only-check`, `--gate <tier>` fails only on
  selected ROMs (and, with `--only-check`, on the selected checks) in
  that tier. The plan's milestone acceptance commands rely on this (e.g.
  M1: `--group … --only-check serial --gate must`). The gate script never
  passes a selection, so the stage gate always scores the whole manifest.
  A `--gate` tier with no selected ROM is a usage error, not a vacuous
  pass.
- **The overall score renormalizes the weights.** The README's `overall =
  (3·must + 2·should + 1·stretch) / 6` is undefined when a tier has no
  scored ROM (a partial run, or a tier whose ROMs are all `no-reference`).
  The runner sums `weight × score` over the tiers that have a score and
  divides by the sum of their weights; with every tier scored this is
  exactly the README formula.
