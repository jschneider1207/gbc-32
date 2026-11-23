# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M8 (game compatibility + perf baseline): all work items
  are done or deferred to Stage 2; **ready to close** (steps below). The
  real-game checklist fixes moved to M9 (human, 2026-11-23). M7 is
  `stage1-m7` (M1-M6 tagged likewise).
- **Last updated:** 2026-11-23, end of session 10 (log
  `logs/2026-11-22-10-m8-start.md`). main = daec83c, gate passes, tree clean,
  no worktrees or agents running.

## Next up (session 11)

1. **Worst-case benchmark gains mid-line SCX writes** (human decision,
   session 10): add one mid-line SCX write per line to `synth:worst-case`
   (tools/bench/src/synth.rs) so the held-fetch slow path counts against
   the 4.02 M x86 Ir/frame line. Do this BEFORE the M8 baseline update.
   If the worst case then crosses the line, stop and bring numbers/options
   to the human; don't loosen anything. Mutation-check: the new writes must
   actually hit mode 3 (e.g. show the slow path's Ir appears).
2. **Close M8** (AGENTS.md "Independent review", "Git"): range
   `stage1-m7..HEAD` (big: ~90 commits; this session touched CPU/bus ROM
   reads, timer, APU, PPU, DMA, runner, bench, desktop). Run 3 fresh
   reviewers in parallel: hardware correctness, test integrity (includes
   the romtest changes 23154a2 audio capture-length rule and b74079a
   audio0 report split, the fingerprint corpus change f200ab4, and the new
   gate checks), performance/portability (hot paths changed: ROM reads
   ADR 0027, APU, PPU held fetch, timer). Fix confirmed high/medium;
   lows become plan tasks. Then `--update-baseline` (acceptance) and the
   bench baseline (`synth:cgb-raster` is +2.70% of the 3% guard against
   the old one), commit, tag `stage1-m8`. Rewrite STATUS.
3. **M9** (`docs/plan/stage-1.md` M9, `docs/plan/m9-triage.md`): first the
   human's real-game checklist findings (they play on native Windows),
   then triage groups by ROMs per cost. Open threads:
   - Window step E2+ (56 ROMs left in triage group 4; leads in the design
     doc section 7 "Step E1" and the triage doc).
   - OAM DMA group 13 (6 `src{8000,9F00}_busypop*`): conflicting CPU
     reads with a VRAM source clear OAM byte `i` (gambatte); needs a
     design for a side effect on the read path (ADR 0028).
   - `oamdma_src0000_busyint0002` (interrupt-dispatch pushes during DMA).
   - Steps F (objects), G (CGB tile-select glitch); Mealybug
     `m3_scx_low_3_bits` pixels 152-159 (not the column carry).
   - HDMA timing (75), STAT IRQ edges (47), `lcd_offset` (33), double-speed
     STAT/LY (31), APU square timing (27).

**Safety (read AGENTS.md "Machine resources"):** never open the Linux
window under WSLg and never use the GPU from WSL. Window/audio/input smoke
tests use `scripts/run-windows.sh <rom> --frames N [--out shot.png]` on a
scratch ROM copy; short, and only for front-end changes. Windows Update
can shut WSL down mid-session (it did at 22:57 on 2026-11-22, installing
WSL 3.0.5.0): commit often; agents resume from their transcripts.

Desktop front end (`crates/gbc-desktop`): `gbc-desktop <rom> [--scale K]
[--frames N] [--screenshot out.png] [--dmg-palette auto|gray]
[--headless]`. `--frames N` = N complete frames. Blank LCD shows white.
Save: `<stem>.sav`, or `<file name>.sav` when `game.gb` and `game.gbc` sit
side by side (ADR 0023 amendment). Ctrl+C/SIGTERM/panic still write the
exit save (`ctrlc` crate). Audio: cpal, audio-driven pacing (ADR 0024),
device reopen with 2-16 s backoff after a stream loss (amendment). Keys:
arrows, Z = B, X = A, Enter = Start, Right Shift/Backspace = Select, Esc
quits. Real-game headless smoke (scratch copies of `/mnt/x/gbc-roms`,
3600 frames) is clean at daec83c.

Perf watch: `check-bench.sh` OK vs the committed (M7) baseline:
`synth:worst-case` -0.32%, `synth:cgb-raster` **+2.70% of the 3% guard**
(mid-line writes take the held-fetch slow path), `synth:dma-storm` +~0.8%.
APU: 883.4 x86 Ir per APU write (budget <= 950). `.text` **96,420 of
97,500 B** (human raised the total from 96,000 in session 10;
per-function budgets unchanged). ITCM set 54,896 B of a 57,344 B core
budget (`scripts/check-itcm-set.sh`, ADR 0026 Proposed). Gate checks
added in session 10: `check-rom-loads.sh` (ADR 0027), `check-itcm-set.sh`.
Fingerprint corpus: 4,020 runs incl. the 198 PNG-reference gambatte ROMs;
a corpus mismatch fails unless `--allow-corpus-change`. Wall-clock
baseline: `bench --wall` (docs/perf.md).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change"; every
  changed ROM is listed and explained.
- Resources: at most 2 concurrent builds, heavy steps (check-bench,
  diff-fingerprint, gate) alone; core work in scratch worktrees with their
  own target dir, merged by `--ff-only` after a rebase.
- Parallel agents can collide on ADR numbers: check `docs/decisions/`
  right before committing an ADR (next free: 0029).
- PPU design size estimates ran ~2-3x low; measure early.

## Needs human
Open (session 10):
- **Real-game checklist** (plan, bottom; now an M9 task), on native
  Windows. Extra checks: Ctrl+C in the window still saves; change the
  default audio output device mid-game (expect `audio stopped`, then within
  ~2 s `output device back; resuming audio pacing`, no burst); a battery
  `.sav` survives a restart; a gamepad.
- **Sign-off candidate (M9):** possibly AGE `spsw-interrupts-cgbBC` (AGE
  `caution/`: breaks the halt Nintendo requires; its source still needs a
  read before asking).
- Optional, strongest guard against a repeat of the BSOD: set
  `guiApplications=false` in `%UserProfile%\.wslconfig` (disables WSLg).
- FYI: `wsl-pro.service` restarts every 2 s because a cmd.exe AutoRun
  entry points at a missing `E:\Tools\ansicon\x64\ansicon`; harmless
  noise.
- Optional: disable or schedule WSL auto-updates (Windows Update shut WSL
  down mid-session on 2026-11-22).

Open: `../gbc-32-hardware/docs/firmware-interface.md` says the audio rate is "decided in firmware M6"; ADR 0021 fixed it at 32,768 Hz (agents may not edit that repo).

Decided in session 10 (2026-11-23), human:
- gbc-core total `.text` budget 96,000 -> 97,500 B (for mid-line PPU
  steps E/F/G and the M9 fixes; per-function budgets unchanged). "That's
  fine."
- OAM DMA CPU writes during a conflicting transfer (M9 triage groups 2 and
  13, ~65 ROMs): follow gambatte's CGB C hardware captures over SameBoy's
  rule. "gambatte is fine."
- M9 sign-off: the 7 SameSuite APU ROMs that target other CGB revisions
  (`channel_{1,2,4}_extra_length_clocking-cgb0B`,
  `channel_3_extra_length_clocking-cgb0`, `-cgbB`,
  `channel_1_freq_change_timing-A`, `-cgbDE`) are signed off as "targets
  another CGB revision" (ADR 0003: we emulate CPU CGB C). They stay in the
  manifest and keep running; they don't block Stage 1. "Sign off on the 7."
- The 50% bar covers realistic worst cases; dense mid-mode-3 register
  writes (many per line) are outside it, documented in docs/perf.md
  "Outside the bound" with a Stage 2 measurement task. "Yes that is fine."
- `synth:worst-case` gains one mid-line SCX write per line so the
  held-fetch slow path counts against the 4.02 M line (stricter; moves the
  bench baseline). "Do the other thing." Queued after the OAM DMA and
  window-timing work lands.

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
- **Real-game ROMs (human):** `X:\gbc-roms` = `/mnt/x/gbc-roms` (all 6 checklist games). Read-only for agents: copy to a scratch dir before any run, never write `.sav` files there, never commit ROMs.
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

HEAD daec83c, not yet the committed baseline (that is M7's; update at the
M8 close). `scripts/gate-stage1.sh` passes at daec83c.

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 39 | 40 | only age `spsw-interrupts-cgbBC` (AGE caution/) fails |
| stretch | 3,126 | 3,466 | 5 no-reference; 7 signed off (other CGB revisions); M7 baseline was 2,765 |

Session 10 gains (vs the M7 baseline): should-tier timing fixes (8ba9c4c,
740154c, 0a83ae5, d2b6e56, eb019ba), mid-line PPU steps 1-3, D, E1
(6cfe52b..daec83c), palette RAM blocking (6110717), LCDC.2 per scan entry
(3f7b00d), live-SCX discard (e549dd7), OAM DMA write conflicts per
gambatte (3031f7f, ADR 0028).
