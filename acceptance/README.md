# Acceptance testing

Acceptance is decided by scripts, not judgment. A milestone or stage is done
when its gate script exits 0. Opinions about whether something "looks right"
don't count.

## Ownership

| File | Owner | Agents may |
|---|---|---|
| `acceptance/*.toml` (manifests) | Human | Read only. Propose changes in `docs/STATUS.md` → "Needs human". |
| Expected screenshots in `test-roms/` | Upstream | Never modify. |
| `acceptance/test-roms.sha256` (integrity list) | Human | Read only. Regenerated with `scripts/hash-test-roms.sh` only when the human pins a new test-ROM release, never to make a run pass. |
| `acceptance/results/*.json` (baselines) | Agent | Update only via `--update-baseline`, which refuses if anything regressed. |
| Runner implementation (`tools/romtest`) | Agent | Change freely, as long as it still follows this contract. |

## Runner contract

```
cargo run -p romtest --release -- --manifest acceptance/stage1.toml [--filter <glob>] [--gate <tier>] [--update-baseline] [--jobs N]
```

- `--filter <glob>`: run only the ROMs whose path matches the glob. This selects every group containing that path (e.g. all three rtc3test groups). For development and milestone checks; the gate never uses it.
- `--group <id>`: run only the named group (can be repeated).
- `--only-check <kind>`: evaluate only checks of that kind (`serial`, `screenshot`, ...); ignore the rest. For milestone checks that come before a subsystem exists (e.g. M1 serial output before the PPU). The gate never uses it.
- `--gate <tier>`: fail if any ROM in that tier doesn't pass (`no-reference` counts as not passing). Requires a baseline (exit 2 without one). Combined with `--filter`, `--group` or `--only-check`, only the selected ROMs (and checks) in that tier are scored; milestone acceptance commands rely on this, and the gate script never passes a selection. A gated tier with no selected ROM is exit 2, not a vacuous pass.
- `--jobs N`: run ROMs in parallel. Defaults to the number of CPU cores. Each ROM still runs on its own single-threaded emulator instance.
- `--update-baseline`: write the committed baseline. Refused if any ROM regressed, or if combined with `--filter`, `--group` or `--only-check`. The baseline must always cover the whole manifest. If no baseline exists yet, this creates one.
- `--allow-removed`: only together with `--update-baseline`. Drops baseline entries whose `(group, ROM)` no longer exists in the manifest, printing each one. Still refused on any other regression.
- `--roms-dir <path>`: use another copy of the test ROMs instead of `test-roms/` (for testing the runner). It is verified against the integrity list exactly like `test-roms/`.
- `-h`/`--help`: prints usage and exits 0 only as the sole argument; combined with anything else it is a usage error (exit 2).

### Integrity of `test-roms/`

`acceptance/test-roms.sha256` lists the SHA-256 of every file under
`test-roms/` with a `# release: <version>` header. Before running anything,
the runner checks that the list's release equals `roms_release`, that every
listed file exists and matches, and that every ROM matched by a manifest
glob is listed. Every ROM and reference PNG is hashed again from the exact
bytes loaded. Any mismatch, missing file or unlisted file is exit 2. This
stops a deleted or altered ROM or PNG from silently changing results.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Ran successfully; no regressions; `--gate` tier (if given) all passing |
| 1 | Ran successfully, but there are regressions or the `--gate` tier has failures |
| 2 | Runner error: bad manifest, missing ROM, integrity-list mismatch, I/O failure, `--gate` without a baseline, a baseline from another `roms_release` (unless `--update-baseline`), a usage error, or an internal bug (a panic in the runner's own code, as opposed to inside the emulator core). The results are meaningless. |

The gate treats 1 and 2 differently, so a broken runner can never look like
ROMs simply failing.

## Manifest schema

Top level: `roms_release` (must equal `test-roms/.version`, else exit 2),
`model` (only `"cgb-c"`), and a list of `[[group]]`. Each group has:

| Key | Required | Meaning |
|---|---|---|
| `id` | yes | Unique within the manifest. Results and the baseline are keyed by `(id, rom path)`. |
| `tier` | yes | `must`, `should` or `stretch` |
| `roms` | yes | Globs relative to `test-roms/`, supporting `*`, `**` and `{a,b}` alternation (the same syntax as `--filter`). Only files ending in `.gb` or `.gbc` count; each ROM appears at most once per group. A group matching zero ROMs is exit 2. |
| `exclude` | no | Globs removed from the `roms` matches |
| `input` | no | Button script (below) |
| `exit` | yes | One exit condition (below) |
| `checks` | yes | Non-empty list. A ROM passes only if every check passes. |

`input` is a list of `{ at_s, press, hold_s }`. Each entry presses every
button in `press` (`a`, `b`, `select`, `start`, `up`, `down`, `left`,
`right`) at emulated time `at_s` and releases them at `at_s + hold_s`. Each
time is converted to base clocks and rounded down to a multiple of 4 clocks
(a normal-speed M-cycle), at any CPU speed. The core then lands the change
at the start of the M-cycle that clock falls in, on the M-cycle grid in
effect at that time (2 clocks per M-cycle in double speed). Overlapping
entries combine: a button is held while any entry is holding it. Each change
lands on its M-cycle, wherever run calls end (the core's scheduled button
changes, ADR 0012). The core queues a few changes ahead; if a change's clock
has already passed when the runner can hand it over (more changes inside one
run call than the queue holds), the run stops there and the ROM is a `crash`
("input at clock ... could not be scheduled on its M-cycle"). It is never
applied late. A change at or after the exit limit is not needed and never
fails a run.

For every ROM matched by every group in the manifest, the runner:

1. Boots the ROM on a fresh emulator in the manifest's `model`, starting from
   the documented post-boot register state (no boot ROM).
2. Applies the group's `input` script at the given **emulated** times.
3. Runs until the `exit` condition is met.
4. Evaluates every entry in `checks`. A ROM passes only if all of them pass.

Everything is deterministic: there is no wall clock, no host randomness and no
threads inside an emulator instance. The MBC3 RTC advances only with emulated
time. Running the same build on the same manifest always produces the same
results.

### Exit conditions

| Key | Meaning |
|---|---|
| `run_s = N` | Run exactly N emulated seconds (N × 4,194,304 base clocks, regardless of CPU speed mode). |
| `run_frames = N` | Run exactly N LCD frames (N × 70,224 base clocks). |
| `ld_b_b = true, timeout_s = N` | Stop when the CPU executes opcode `0x40` (`LD B,B`). Hitting the timeout counts as `timeout`, which is a failure. |

### Checks

| Key | Passes when |
|---|---|
| `serial = "text"` | Bytes written to the serial port (SB/SC transfers) contain `text`. |
| `screenshot = "path"` | The final frame matches the PNG exactly. `{dir}` and `{stem}` expand to the ROM's directory and file stem. Missing PNG → `no-reference`. The **final frame** is the last fully drawn 160×144 frame before exit (completed at the start of VBlank). The frame is solid white while the screen is blank: if the LCD is off at exit, if it was turned off since the last completed frame, if no frame has completed, or if the only frame completed since the LCD was turned back on is the first one (hardware doesn't display that frame; SameBoy `frame_skip`). |
| `fibonacci = true` | B=3, C=5, D=8, E=13, H=21, L=34 at exit. |
| `gambatte = true` | Gambatte rules from the file name: `_out<hex>` → on-screen hex pattern, `_outaudio0/1` → silence/sound, sibling `.png` → screenshot. See the upstream `testrunner.cpp` (pokemon-speedrunning/gambatte-core @ d819bad196). |

### Colors used for screenshot comparison

- CGB colors: each 5-bit channel `X` becomes `(X << 3) | (X >> 2)`.
- DMG compatibility mode shades: BG `#000000 #0063C6 #7BFF31 #FFFFFF`,
  OBJ `#000000 #943939 #FF8484 #FFFFFF`. These are the "common palette"
  used by the reference screenshots, not the real boot ROM palette.
- Gambatte screenshots use Gambatte's own CGB formula (see
  `test-roms/gambatte/game-boy-test-roms-howto.md`).

The core always outputs RGB555. For DMG games, the caller chooses the
compatibility palettes (BG, OBJ0, OBJ1) when it creates the emulator; this
stands in for the palette the real boot ROM would pick. The runner passes the
"common palette" above, with OBJ0 = OBJ1. Every one of those colors can be
represented exactly in RGB555. The runner converts RGB555 to RGB888 with
`(X << 3) | (X >> 2)` (or Gambatte's formula for Gambatte screenshots).

### Statuses

| Status | Meaning | Scored as |
|---|---|---|
| `pass` | All checks passed | pass |
| `fail` | At least one check failed | fail |
| `timeout` | The `ld_b_b` exit never happened | fail |
| `crash` | Panic, the CPU locked up on an invalid opcode, or an input change could not be applied on its M-cycle | fail |
| `unsupported` | The runner doesn't implement this check kind yet | fail |
| `no-reference` | Expected PNG missing | excluded |

When several apply, precedence is `crash`/`timeout` > `fail` >
`unsupported` > `no-reference` > `pass`. So a missing PNG never hides a
crash, a timeout or another failing or unsupported check: `no-reference`
applies only when every other check passed. A ROM whose `load` is rejected
by the core, or whose emulator stops advancing time, is a `crash`.

The runner must catch panics per ROM, so host builds need unwinding panics.
Don't set `panic = "abort"` in workspace profiles used by `romtest`.

## Outputs

- `target/acceptance/<manifest>.json`: full results for every ROM and every
  check, including serial output and a mismatched-pixel count.
- `target/acceptance/<manifest>.md`: summary table by tier and group.
- `target/acceptance/<manifest>/failed/*.png`: actual frame next to a diff
  image for each screenshot failure.
- `acceptance/results/<manifest>.json`: the **committed baseline**, i.e. the
  status of every ROM at the last accepted run.

### Score

```
tier score = passed / (total - no_reference)
overall    = (3·must + 2·should + 1·stretch) / 6       (tier scores, 0–100%)
```

If a tier has no scored ROM (a partial run, or a tier that is entirely
`no-reference`), its score is undefined, and `overall` sums `weight × score`
over the tiers that have one, divided by the sum of their weights. With
every tier scored, this is exactly the formula above.

### Regressions

Any `(group id, ROM)` whose baseline status is `pass` and whose current
status is anything else is a **regression**. The runner prints regressions
first and exits 1 if there are any. With `--filter`, `--group` or
`--only-check`, only the ROMs and checks that actually ran are compared.

A baseline entry with no current result also counts as a regression (shown
as `<status> -> missing`):
- In a full run (no selection), every baseline entry is in scope, whatever
  its status. Such entries can only be dropped with
  `--update-baseline --allow-removed`.
- In a partial run, an in-scope entry whose baseline status was `pass`.

### Runtime

The runner records the wall-clock time for each ROM and for the whole run,
and prints the 10 slowest ROMs. Failing `ld_b_b` ROMs run until their
timeout (up to 120 emulated seconds), so the runner must catch CPU lockups
early (`crash`). Keep the full-manifest run time in `docs/STATUS.md` under
"Acceptance snapshot". If it grows past about 10 minutes, raise it under
"Needs human".

## Gate

`scripts/gate-stage1.sh` exits 0 only if **all** of these hold:

1. `cargo fmt --check` is clean
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean
3. `cargo test --workspace` passes
4. `gbc-core` builds for `thumbv7em-none-eabihf`, which enforces `no_std`
   because that target has no `std`. Separately, `alloc` is forbidden
   anywhere in `crates/gbc-core/src` (a whitespace- and line-break-tolerant
   search for `extern crate alloc`, `use alloc` and `alloc::`), and
   `crates/gbc-core/tests/hard_rules.rs` must exist and pass: `std` only
   inside `#[cfg(test)]` modules, no `#[path]` or `include!`, `#![no_std]`
   present, and no dependencies of any kind in the core's `Cargo.toml`.
5. `scripts/check-code-size.sh` passes: every symbol in
   `perf/code-size-budgets.txt` is found in the `thumbv7em-none-eabihf`
   release build (`-C target-cpu=cortex-m7`) and fits its budget. A missing
   symbol fails.
6. `scripts/check-bench.sh` passes: the benchmark ROMs are present with the
   hashes in `acceptance/test-roms.sha256`, and no ROM's instruction count
   under valgrind exceeds `perf/bench-baseline.txt` by more than 3%.
   Raising a budget or a baseline is committed together with the change
   that needs it, with the measured numbers and the reason in the commit
   body (`docs/perf.md`).
7. `scripts/check-fuzz.sh` passes: every input in `tools/fuzz/corpus/` and
   the fixed-seed generated inputs run through the core's public API
   without a panic or a hang (`docs/fuzzing.md`).
8. The runner exits 0 with `--gate must` on the full manifest: every `must`
   ROM passes and there are no regressions against
   `acceptance/results/stage1.json`. Exit code 2 is reported separately as a
   runner error.

`should` and `stretch` scores are reported but don't block the gate.
Finishing Stage 1 additionally requires the human to sign off on the
real-game checklist in `docs/plan/stage-1.md`.
