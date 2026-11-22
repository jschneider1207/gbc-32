# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M5 (CGB features): acceptance passes, close review not
  run yet. M4 is `stage1-m4` (0bf708d), M3 `stage1-m3`, M2 `stage1-m2`,
  M1 `stage1-m1`.
- **Last updated:** 2026-11-22, session 07 (log `logs/2026-11-22-07-m5-cgb.md`)

## Next up

1. **M5 close** (AGENTS.md "Independent review"): 3 reviewers in parallel
   on `stage1-m4..HEAD` (hardware correctness, test integrity incl. the
   gambatte check 78c6443 and the lockup rule 75902f9 which had no
   separate review, perf/portability for the PPU/bus/CPU hot-path
   changes). Only the perf reviewer builds (bench, code size); the other
   two read and may run `cargo test`. Triage, fix confirmed high/medium,
   then `--update-baseline` (the committed baseline still lists every
   gambatte ROM `unsupported`), commit, tag `stage1-m5`.
2. Unfinished M5 plan items roll forward as plan tasks (STOP-mode
   accuracy and screen blank in STOP, palette mode-3 blocking, post-boot
   CGB handover, OAM DMA conflicts vs gambatte, interrupt_time, the
   other-clock-phase PPU thresholds, cgb-acid-hell 2 px).
3. Code size: total `.text` is 84,942 of 85,100 B (158 B left). M6 (APU)
   can't fit without a budget decision or big cuts: raise it with the
   human before M6 (Needs human).

Perf watch: `synth:worst-case` ~3.69 M x86 Ir/frame (92% of the 4.02 M
line, which assumes 1 Ir = 1 M7 cycle; thumbv7em code is 1-2x). Bench vs
baseline (e22721c): cgb-acid2 +2.28%, cgb-raster +1.86%, the rest under
+0.5% or negative. Run `check-bench.sh` and `check-code-size.sh` on every
hot-path step.

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change";
  behavior changes must list and explain every differing ROM. A STOP
  counter attributed all 1,368 speed-switch diffs (session 07).
- Resources: at most 2 concurrent builds, heavy steps (bench, fingerprint,
  gate) alone; scratch worktrees get their own target dir and are deleted
  when done (AGENTS.md "Machine resources").
- Agents with one theme and a ~20-45 tool-call cap stay on budget.

## Needs human

Open (session 07):
1. 198 Gambatte ROMs with `*_cgb04c.png` references (`scy/`, `bgen/`,
   `bgtilemap/`, `scx_during_m3/`, ...) have no `cgb04c` in the ROM name,
   so `gambatte-cgb` never selects them. Add a group or glob for them?
   (Manifest change. A directory glob would also select ROMs with no CGB
   rule, which is a runner error; ADR 0018.)
2. Total `.text` budget (85,100 B, `check-code-size.sh`): 84,942 B used
   after M5. The APU (M6) will need several KB. Raise the budget for M6
   (and to what), or require cuts first? Raising is a loosening, so it's
   the human's call. Context: the core is already over the H753's 64 KB
   ITCM (plan task), so the budget guards growth, not a hardware limit.

Decided in session 07 (2026-11-22): STOP is 2 bytes unless IE&IF is
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
- **Desktop system libraries:** installed. WSL2 Ubuntu 24.04, no `jq`.
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

Session 07, main 58c87a4 (full-manifest run, not yet a committed
baseline; the committed baseline is still `stage1-m4`'s):
- must 85/86: only `cgb_sound` fails (M6). cgb-acid2 and
  mooneye-cgb-boot-regs pass (M5 acceptance).
- should 32/40 (M4: 26): same-suite-cgb 6/6, age-cgb 14/21;
  `blargg-interrupt-time` still fails.
- stretch: about 2,555/3,263 with the gambatte check live (M4: 12, when
  gambatte was `unsupported`); 131 gambatte audio ROMs `unsupported`
  until M6.

Performance (`docs/perf.md`, `perf/bench-baseline.txt` rebaselined at
e22721c; x86 Ir): worst-case ~443 M (3.69 M/frame, 92% of the 4.02 M
line); cgb-acid2 +2.28% and cgb-raster +1.86% over the baseline.
Code size (thumbv7em): gbc-core `.text` 84,942 of 85,100 B. `Hw` within
its 864 B assert.
