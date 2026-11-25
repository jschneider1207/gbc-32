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
| 2 | Runner error: bad manifest, missing ROM, integrity-list mismatch, a recolored reference whose `count` doesn't match, I/O failure, `--gate` without a baseline, a baseline from another `roms_release` (unless `--update-baseline`), a usage error, or an internal bug (a panic in the runner's own code, as opposed to inside the emulator core). The results are meaningless. |

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
| `exclude` | no | Globs removed from the `roms` matches. Each glob must remove at least one ROM that the group's `roms` match, else exit 2 (a stale exclude can't silently let ROMs back in). A ROM may be selected by several groups; it then runs once per group, so use `exclude` to keep groups apart. |
| `lockup` | no | `"crash"` (default) or `"run_on"`: the per-group opt-in for the Gambatte lockup rule (under Statuses). `"run_on"` is valid only when every check is `gambatte = true` and the exit is `run_frames`; anywhere else it is exit 2. |
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
| `screenshot = "path", recolor = { from = "#RRGGBB", to = "#RRGGBB", count = N }` | As `screenshot`, against the same listed upstream PNG with one color replaced in memory: every pixel of color `from` becomes `to`. See "Recolored references" below. The PNG file is never changed. |
| `fibonacci = true` | B=3, C=5, D=8, E=13, H=21, L=34 at exit. |
| `gambatte = true` | Gambatte rules from the file name: `_out<hex>` → on-screen hex pattern, `_outaudio0/1` → silence/sound, sibling `.png` → screenshot. See the upstream `testrunner.cpp` (pokemon-speedrunning/gambatte-core @ d819bad196). In a group with `lockup = "run_on"`, a CPU lockup doesn't end the run (see "Gambatte lockup rule" under Statuses). |

### Colors used for screenshot comparison

- CGB colors: each 5-bit channel `X` becomes `(X << 3) | (X >> 2)`.
- DMG compatibility mode shades: BG `#000000 #0063C6 #7BFF31 #FFFFFF`,
  OBJ `#000000 #943939 #FF8484 #FFFFFF`. These are the "common palette"
  used by the reference screenshots, not the real boot ROM palette.
- Gambatte screenshots use Gambatte's own CGB formula (see
  `test-roms/gambatte/game-boy-test-roms-howto.md`).

#### Recolored references

A screenshot check may carry `recolor = { from = "#RRGGBB", to = "#RRGGBB",
count = N }` for an upstream reference that draws one color differently
from this contract. The rules:

- The reference is still the listed upstream PNG in `test-roms/`, verified
  against `acceptance/test-roms.sha256` like every other file. The recolor
  is applied in memory to the bytes that passed that check, never written
  to disk. Nothing else is loaded.
- The path must be fixed (no `{dir}` or `{stem}`). `recolor` is only
  allowed on a `screenshot` check, never on `gambatte` or any other kind.
- `to` must be one of the contract colors above: the common palette's BG
  shades `#000000 #0063C6 #7BFF31 #FFFFFF` or OBJ shades `#000000 #943939
  #FF8484 #FFFFFF`. Any other `to`, `from == to`, a malformed color or a
  `count` below 1 is a bad manifest (exit 2).
- Exactly `count` pixels of the PNG must have color `from`. Before anything
  runs, the runner checks every recolor in the manifest (whatever the
  selection) and counts again whenever a check loads the PNG. Any other
  count, or a missing or unlisted PNG, is exit 2, never `no-reference`. So
  a new upstream release that changes the reference stops the run instead
  of passing against a stale recolor.
- The PNG must contain no pixel of color `to`: a recolor relabels one color
  and never merges two (a merge could make a blank frame match). A PNG that
  already has a `to` pixel is exit 2.
- The frame is then compared against the recolored image exactly, as for
  any screenshot. There is no other leniency.

Adding or changing a `recolor` in any group is a manifest change and needs
the human, like any other change to `acceptance/*.toml`.

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
| `crash` | Panic, the CPU locked up on an invalid opcode (except under the Gambatte lockup rule below), or an input change could not be applied on its M-cycle | fail |
| `unsupported` | The runner doesn't implement this check kind yet | fail |
| `no-reference` | Expected PNG missing | excluded |

When several apply, precedence is `crash`/`timeout` > `fail` >
`unsupported` > `no-reference` > `pass`. So a missing PNG never hides a
crash, a timeout or another failing or unsupported check: `no-reference`
applies only when every other check passed. A ROM whose `load` is rejected
by the core, or whose emulator stops advancing time, is a `crash`.

**Gambatte lockup rule** (approved by the human 2026-11-22 for the
`gambatte/undef_ops` ROMs, and narrowed by the human the same day to an
explicit opt-in; ADR 0018, amendments). Only in a group with
`lockup = "run_on"` is a CPU lockup on an invalid opcode not a `crash`.
That key is valid only on a group whose checks are all `gambatte = true`
and whose exit is `run_frames = N`; anywhere else the manifest is bad
(exit 2). In such a group, as on hardware, the CPU stays stuck
(interrupts don't wake it) while the LCD, timers and the rest keep
running; the run goes on to frame N, and the checks decide on the final
frame as usual, so a wrong screen is `fail` and a lockup is never a
`pass` by itself. The lockup is recorded in the ROM's `detail` ("CPU
locked up (PC=...) at clock ...; ran on to frame N"). Everything else is
unchanged: a lockup in any group without the opt-in (including other
all-`gambatte`, `run_frames` groups), a panic, a rejected load, stalled
emulated time and a late input change are still `crash`.

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

The script runs these steps in this order. Each stops the gate at the first
failure.

1. Preconditions: none of `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`,
   `CARGO_BUILD_RUSTFLAGS` or `CARGO_TARGET_*_RUSTFLAGS` may be set (they
   replace the flags of `.cargo/config.toml`, so the gate would judge
   another build; set but empty counts as set), and `cargo` and a Cargo
   workspace must exist. The overrides `CODE_SIZE_BUDGETS_FILE`,
   `ITCM_SET_FILE` and `TEST_ROMS` are unset: the gate judges the committed
   files and the ROMs it fetches.
2. `scripts/fetch-test-roms.sh` succeeds (pinned zip hash).
3. `scripts/fetch-sm83-tests.sh` succeeds (SM83 single-step CPU test data,
   pinned commit and per-file hashes), and the gate exports
   `GBC_REQUIRE_TEST_DATA=1` so `tools/sm83-tests` fails instead of skipping
   when the data is missing (step 7 then runs those cases).
4. `cargo fmt --check` is clean
5. `cargo clippy --workspace --all-targets -- -D warnings` is clean
6. `RUSTDOCFLAGS="-D warnings" cargo doc -p gbc-core --no-deps
   --document-private-items` is clean (no rustdoc warning, such as a
   broken intra-doc link, in the core's documentation).
7. `cargo test --workspace` passes (including the SM83 single-step cases
   with the data required, see step 3).
8. `gbc-core` builds for `thumbv7em-none-eabihf`, which enforces `no_std`
   because that target has no `std`. Separately, `alloc` is forbidden
   anywhere in `crates/gbc-core/src` (a whitespace- and line-break-tolerant
   search for `extern crate alloc`, `use alloc` and `alloc::`), and
   `crates/gbc-core/tests/hard_rules.rs` must exist and pass: `std` only
   inside `#[cfg(test)]` modules, no `#[path]` or `include!`, `#![no_std]`
   present, and no dependencies of any kind in the core's `Cargo.toml`.
9. `scripts/check-code-size.sh` passes: the total `.text` of `gbc-core` in
   the `thumbv7em-none-eabihf` release build (`-C target-cpu=cortex-m7`)
   fits the `(total .text)` figure in `perf/code-size-budgets.txt` (sized
   for the STM32H735's ITCM, `docs/perf.md` "Code-size budgets"). The
   per-function lines in that file are reported, not gated (human,
   2026-11-24).
10. `scripts/check-itcm-set.sh` passes: every glob in
    `perf/itcm-sections.txt` (the dev-board ITCM placement plan,
    `docs/perf.md`) matches a function section, every `FLASH_OK` line has a
    reason, and the build and relocations are understood. Fails on a stale
    glob (`MISSING`), a `FLASH_OK` without a reason, or a build or
    relocation error. Report-only (WARN, exit 0): the set's summed size
    over the ITCM budget, and callee-closure leaks out of the set.
11. `scripts/check-bench.sh` passes: the benchmark ROMs are present with the
    hashes in `acceptance/test-roms.sha256`, and no ROM's instruction count
    under valgrind exceeds `perf/bench-baseline.txt` by more than 3%.
    Raising a budget or a baseline is committed together with the change
    that needs it, with the measured numbers and the reason in the commit
    body (`docs/perf.md`).
12. `scripts/check-fuzz.sh` passes: every input in `tools/fuzz/corpus/` and
    the fixed-seed generated inputs run through the core's public API
    without a panic or a hang (`docs/fuzzing.md`).
13. `scripts/check-sync-invariance.sh --long` passes (`docs/perf.md`
    "Invariance checks"): the `eager-sync` build (every lazy component
    caught up at every M-cycle) and the `clock-offset` build (powered on
    just below 2^31, 2^32, 2^33 and 2^40 clocks) print the same
    `--fingerprint` lines as the product build, apart from the listed known
    differences (a stale entry fails too), and `bench --long` runs 30
    emulated minutes without a panic. Any differing line fails.
14. The desktop front end builds (`cargo build -p gbc-desktop --release`)
    and the binary cargo just built runs
    `test-roms/blargg/cpu_instrs/cpu_instrs.gb --headless --frames 600`
    and exits 0 (M7 acceptance).
15. The runner exits 0 with `--gate must` on the full manifest: every `must`
    ROM passes and there are no regressions against
    `acceptance/results/stage1.json`. Exit code 2 is reported separately as a
    runner error.

`should` and `stretch` scores are reported but don't block the gate.
Finishing Stage 1 additionally requires the human to sign off on the
real-game checklist in `docs/plan/stage-1.md`.
