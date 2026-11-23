# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M8 (game compatibility + perf baseline), not started. M7
  is `stage1-m7`, M6 `stage1-m6`, M5 `stage1-m5`, M4 `stage1-m4`, M3
  `stage1-m3`, M2 `stage1-m2`, M1 `stage1-m1`.
- **Last updated:** 2026-11-22, end of session 09 (M7 close; log
  `logs/2026-11-22-09-m7-desktop.md`)

## Next up

Start M8 (`docs/plan/stage-1.md` "M8") in a fresh session. The first M8
task, "fix issues found in the real-game checklist", needs the human's
play-testing on the new front end (Needs human). Until then, take the
unblocked M8 items: the headless frames/second benchmark, the M6/M7
review lows, code-size/ITCM planning, unaligned ROM reads.

Desktop front end (`crates/gbc-desktop`, M7): `gbc-desktop <rom>
[--scale K] [--headless --frames N]`. winit 0.30 + softbuffer (CPU blit,
integer scale), cpal 0.18 audio with audio-driven pacing (ADR 0024: 20 Hz
DC blocker, Catmull-Rom resampler, ±0.5% DRC, 50 ms target, falls back to
clock pacing if no device or the callback stalls 500 ms), gilrs 0.11
gamepad, `.sav` next to the ROM with the 48-byte BGB/VBA-M RTC footer and
a 1 s settle / 10 s cap autosave (ADR 0023). Keys: arrows, Z = B, X = A,
Enter = Start, Right Shift/Backspace = Select, Esc quits. Env:
`GBC_NO_AUDIO=1` (silent clock pacing), `GBC_AUDIO_STATS=1` (underrun /
fill stats). The gate now builds it and runs `cpu_instrs.gb` headless for
600 frames (contract item 8).

Perf watch (unchanged in M7; the core was not touched): `synth:worst-case`
3.908 M x86 Ir/frame, **97.2% of the 4.02 M line** (~110k headroom). APU
budget (human): 230k channels+mixing (224.1k), <= 950 Ir per APU write
(884.6). Code size: `.text` 94,754 of 96,000 B (1.2 KB left). Run
`check-bench.sh` and `check-code-size.sh` on every hot-path step. `cargo
doc -D warnings` fails on 3 links in `cpu/mod.rs` (not in the gate).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change";
  behavior changes must list and explain every differing ROM.
- Resources: at most 2 concurrent builds, heavy steps (bench, fingerprint,
  gate) alone; scratch worktrees get their own target dir and are deleted
  when done (AGENTS.md "Machine resources").
- Agents with one theme and a ~15-45 tool-call cap stay on budget.

## Needs human
Open (session 09, M7):
- **Interactive play check** (M7 acceptance): run `cargo run -p
  gbc-desktop --release -- <rom>` on WSLg and on native Windows (only a
  `cargo check --target x86_64-pc-windows-gnu` was done here); check
  picture, keyboard, a gamepad (untested: no pad in WSL), sound, and that
  a battery game's `.sav` survives a restart. WSLg's weston crashed twice
  (SIGSEGV) during the agent's early audio runs; not reproduced since.
  Then the real-game checklist (plan M8).


Open: `../gbc-32-hardware/docs/firmware-interface.md` says the audio rate is "decided in firmware M6"; ADR 0021 fixed it at 32,768 Hz (agents may not edit that repo).

Decided in session 08 (2026-11-22), human:
- APU perf budget. The exact optimizations brought the APU to 224.1k; the
  human accepted that over the output-changing fallbacks ("Accept, budget
  230k"). After the perf review showed the per-write cost, the budget was
  split ("Split it"): 230k Ir/frame for channels and mixing, plus <= 950
  Ir per APU register write. It stays recorded as over the original 200k
  estimate and is an optimization target (human request; plan M8).
- Total gbc-core `.text` budget = 96,000 B (was 85,100), from the measured full-APU estimate 94,336 B + margin. Per-function budgets stay. Over it later: come back with numbers.

Decided in session 07 (2026-11-22), human: Stage 1 is done only when every
manifest test ROM passes, except groups the human signs off with a recorded
reason (README "Done when", plan M9). Both manifest drafts approved in
advance ("You have my ok") and committed after validation matched
(8543723: PNG-reference group, 198 ROMs; undef_ops lockup opt-in group).

Decided in session 07 (2026-11-22), human, after the M5 test-integrity
review (medium: the lockup rule covered all 3,154 gambatte ROMs, broader
than "those specific tests"): "Narrow to undef_ops". Plan: an explicit
per-group manifest opt-in (e.g. `lockup = "run_on"`), the 10
`gambatte/undef_ops` ROMs in their own group with it, every other group
treats a lockup as `crash` again; contract text + ADR 0018 amendment.
Manifest change drafted for the human's OK; re-review (runner pass/fail
logic changes again).

Decided in session 07 (2026-11-22), human: "Both those sound fine" to
(a) a new stretch-tier group selecting exactly the Gambatte ROMs with a
sibling `_cgb04c.png` (198 ROMs whose names lack `cgb04c`, so
`gambatte-cgb` never selects them; a directory glob would also take ROMs
with no CGB rule). The agent drafts it, the human looks it over before
commit. (b) The total `.text` budget (85,100 B; 84,942 used after M5)
will be raised once for M6, to a figure measured after the first APU
channel lands: come back to the human with the number. Per-function
budgets stay.

Also decided in session 07 (2026-11-22): STOP is 2 bytes unless IE&IF is
pending (SameBoy `stop()`, which encodes the Pan Docs flowchart). The
human first chose "Keep pending cases only", but no SST 0x10 case sets
IF, so that kept 0 of 1000. The human then chose: run all 1000 0x10
cases and compare everything except PC and the bus-cycle list
(registers, flags, SP, IME, RAM), where SST's 1-byte expectation
contradicts hardware. The 5 SameBoy STOP branches have dedicated,
mutation-checked unit tests (ADR 0020). The human also approved loosening the
lockup rule for `gambatte/undef_ops` ("Those specific tests should
probably have looser restrictions then right? If the whole point is to
crash."). Scope chosen: for the `gambatte` check only, an invalid-opcode
CPU lockup is not `crash`; the machine runs on to `run_frames` and the
screen check decides (as upstream testrunner). Panics, stalled time and
lockups elsewhere stay `crash`. The romtest `gambatte`
check may re-encode the hex-digit glyphs clean-room (read GPL
testrunner.cpp for the rules only; own data; ADR 0018). Decided in
session 06 (2026-11-22): the mbc3-tester reference
uses a counted, contract-constrained in-memory recolor (ADR 0007
amendment; a recolor may only relabel a color absent from the reference);
mbc3-tester `run_frames` 41; ADR 0015 extends ADR 0006 and doesn't
change it. Any new `recolor` or manifest change still needs the human.

FYI (session 05): the human saw "a lot of compile errors" (session 05).
build, clippy `-D warnings`, tests, the thumbv7em core build and the
nightly fuzz crate are all clean with the pinned toolchain, so this is
editor-side (likely rust-analyzer using a different toolchain, e.g. a
Windows editor on WSL files, or loading `tools/fuzz/fuzz`, which needs
nightly). Waiting on an example error if it persists.

Context on earlier human decisions:
- Acceptance screenshot fixes: a counted `recolor` on the pinned upstream
  PNG, never a committed replacement image (session 06, ADR 0007).
- `acceptance/test-roms.sha256` is human-owned.
- Strictly-tightening gate/contract changes and factual README fixes are
  agent decisions.
- README quality bar: the core uses at most 50% of an H753 at 480 MHz in
  the worst case.
- Unsafe-free core (ADR 0009).
- The project is MIT-licensed. AGENTS.md has a rule on dependency licenses
  and on not copying code from copyleft emulators.
- Token cost (session 05): agents must be small, one behavior each.
- Remote `origin` = `git@github.com:jschneider1207/gbc-32.git`. The human
  pushes; agents never do.

## Environment

- **Rust:** via rustup (user-level, `~/.rustup` and `~/.cargo`).
  - `cargo` is **not on PATH in non-interactive shells**: prefix commands
    with `source ~/.cargo/env &&`.
  - The toolchain is pinned to 1.100.0. Nightly is installed, but only for
    cargo-fuzz (`tools/fuzz/fuzz/`, outside the workspace; see
    `docs/fuzzing.md`).
- **valgrind** 3.22.0. Benchmark Ir counts are tied to it and the pinned
  toolchain; a change to either is a rebaseline commit.
- **SM83 single-step data:** `scripts/fetch-sm83-tests.sh` →
  `test-data/sm83/`. Set `GBC_REQUIRE_TEST_DATA=1` locally.
- **Desktop system libraries:** installed. Windows target `x86_64-pc-windows-gnu` added to rustup (user-level, session 09) for `cargo check`. WSL2 Ubuntu 24.04, no `jq`.
  Scripts are bash or Elixir, not Python.
- **Datasheets:** `../gbc-32-hardware/datasheets/` (read-only for agents).
- **Commits:** stage by explicit path only (AGENTS.md Git). Other agents
  and the human commit concurrently.
- **Worktrees:** never point a worktree at another tree's `target/` (`CARGO_TARGET_DIR`): cargo then reuses stale binaries (session 07). Agents in `git worktree`s must not symlink `test-roms`
  (a deny rule blocks removing it); use `romtest --roms-dir`,
  `bench --root`, `TEST_ROMS=` for check-bench. Agent worktrees are
  "locked": removal needs `--force --force` (ask the human). Cherry-picked
  branches need `git branch -D` after `git cherry` shows all `-`.
- **romtest filters** match ROM paths, not group names (e.g.
  `--filter 'mooneye-test-suite/acceptance/**'`).
- **Gate:** `scripts/gate-*.sh` edits prompt the human (`.claude/settings.json`).

## Blockers

None.

## Acceptance snapshot

M7 baseline (`stage1-m7`; identical to M6, the core is unchanged). `scripts/gate-stage1.sh` passes.

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 33 | 40 | same-suite-cgb 6/6, age-cgb 14/21, `interrupt_time` fails |
| stretch | 2,765 | 3,461 | 696 fail, 0 unsupported (131 audio ROMs judged), 5 no-reference |

Performance: see Perf watch above and `perf/bench-baseline.txt` (x86 Ir).
