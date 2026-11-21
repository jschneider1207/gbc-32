# Acceptance testing

Acceptance is decided by scripts, not judgment. A milestone or stage is done
when its gate script exits 0. Opinions about whether something "looks right"
don't count.

## Ownership

| File | Owner | Agents may |
|---|---|---|
| `acceptance/*.toml` (manifests) | Human | Read only. Propose changes in `docs/STATUS.md` → "Needs human". |
| Expected screenshots in `test-roms/` | Upstream | Never modify. |
| `acceptance/results/*.json` (baselines) | Agent | Update only via `--update-baseline`, which refuses if anything regressed. |
| Runner implementation (`tools/romtest`) | Agent | Change freely, as long as it still follows this contract. |

## Runner contract

```
cargo run -p romtest --release -- --manifest acceptance/stage1.toml [--filter <glob>] [--gate <tier>] [--update-baseline] [--jobs N]
```

- `--filter <glob>`: run only the ROMs whose path matches the glob. This selects every group containing that path (e.g. all three rtc3test groups). For development and milestone checks; the gate never uses it.
- `--group <id>`: run only the named group (can be repeated).
- `--only-check <kind>`: evaluate only checks of that kind (`serial`, `screenshot`, ...); ignore the rest. For milestone checks that come before a subsystem exists (e.g. M1 serial output before the PPU). The gate never uses it.
- `--gate <tier>`: fail if any ROM in that tier doesn't pass.
- `--jobs N`: run ROMs in parallel. Defaults to the number of CPU cores. Each ROM still runs on its own single-threaded emulator instance.
- `--update-baseline`: write the committed baseline. Refused if any ROM regressed, or if combined with `--filter`, `--group` or `--only-check`. The baseline must always cover the whole manifest. If no baseline exists yet, this creates one.

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Ran successfully; no regressions; `--gate` tier (if given) all passing |
| 1 | Ran successfully, but there are regressions or the `--gate` tier has failures |
| 2 | Runner error: bad manifest, missing ROM, I/O failure, or an internal bug. The results are meaningless. |

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
`right`) at emulated time `at_s` and releases them at `at_s + hold_s`. Times
are rounded down to the nearest M-cycle. Overlapping entries combine: a
button is held while any entry is holding it.

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
| `screenshot = "path"` | The final frame matches the PNG exactly. `{dir}` and `{stem}` expand to the ROM's directory and file stem. Missing PNG → `no-reference`. The **final frame** is the last fully drawn 160×144 frame before exit (completed at the start of VBlank). If the LCD is off at exit, the frame is solid white. |
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
| `crash` | Panic, or the CPU locked up on an invalid opcode | fail |
| `unsupported` | The runner doesn't implement this check kind yet | fail |
| `no-reference` | Expected PNG missing | excluded |

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

### Regressions

Any `(group id, ROM)` whose baseline status is `pass` and whose current
status is anything else is a **regression**. The runner prints regressions
first and exits 1 if there are any. With `--filter`, `--group` or
`--only-check`, only the ROMs and checks that actually ran are compared.

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
   because that target has no `std`. Separately, `alloc` is forbidden:
   no `extern crate alloc` anywhere in `crates/gbc-core/src`. Test code may
   use `std` as long as it's inside `#[cfg(test)]`.
5. The runner exits 0 with `--gate must` on the full manifest: every `must`
   ROM passes and there are no regressions against
   `acceptance/results/stage1.json`. Exit code 2 is reported separately as a
   runner error.

`should` and `stretch` scores are reported but don't block the gate.
Finishing Stage 1 additionally requires the human to sign off on the
real-game checklist in `docs/plan/stage-1.md`.
