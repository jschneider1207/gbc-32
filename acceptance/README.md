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

- `--filter`: run only the ROMs whose path matches the glob (for development; never used by the gate).
- `--gate <tier>`: exit non-zero if any ROM in that tier fails or any ROM regresses.
- Without `--gate`, the exit code is non-zero only on regressions.
- `--jobs`: run ROMs in parallel. Each ROM still runs on its own single-threaded emulator instance.

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
| `screenshot = "path"` | The final 160×144 frame matches the PNG exactly. `{dir}` and `{stem}` expand to the ROM's directory and file stem. Missing PNG → `no-reference`. |
| `fibonacci = true` | B=3, C=5, D=8, E=13, H=21, L=34 at exit. |
| `gambatte = true` | Gambatte rules from the file name: `_out<hex>` → on-screen hex pattern, `_outaudio0/1` → silence/sound, sibling `.png` → screenshot. See the upstream `testrunner.cpp` (pokemon-speedrunning/gambatte-core @ d819bad196). |

### Colors used for screenshot comparison

- CGB colors: each 5-bit channel `X` becomes `(X << 3) | (X >> 2)`.
- DMG compatibility mode shades: BG `#000000 #0063C6 #7BFF31 #FFFFFF`,
  OBJ `#000000 #943939 #FF8484 #FFFFFF`. These are the "common palette"
  used by the reference screenshots, not the real boot ROM palette.
- Gambatte screenshots use Gambatte's own CGB formula (see
  `test-roms/gambatte/game-boy-test-roms-howto.md`).

This color conversion lives in the runner, not in `gbc-core`. The core
outputs raw RGB555 or shade indices, and the front end decides how to display
them.

### Statuses

`pass`, `fail`, `timeout`, `crash` (panic, or an invalid opcode that halts the
CPU), `no-reference` (an expected PNG is missing; excluded from scoring).

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

Any ROM whose baseline status is `pass` and whose current status is anything
else is a **regression**. The runner prints regressions first and exits
non-zero if there are any.

## Gate

`scripts/gate-stage1.sh` exits 0 only if **all** of these hold:

1. `cargo fmt --check` is clean
2. `cargo clippy --workspace --all-targets -- -D warnings` is clean
3. `cargo test --workspace` passes
4. `gbc-core` builds for `thumbv7em-none-eabihf` (it must stay `no_std` and
   must not use `alloc`)
5. Every `must` ROM in `acceptance/stage1.toml` passes
6. There are no regressions against `acceptance/results/stage1.json`

`should` and `stretch` scores are reported but don't block the gate.
Finishing Stage 1 additionally requires the human to sign off on the
real-game checklist in `docs/plan/stage-1.md`.
