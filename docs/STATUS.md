# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M8 (game compatibility + perf baseline), in progress. M7
  is `stage1-m7` (M1-M6 tagged likewise).
- **Last updated:** 2026-11-23, session 10 (log
  `logs/2026-11-22-10-m8-start.md`)

## Next up

1. **Real-game checklist results from the human** (plan, bottom) feed the
   first M8 task. Headless smoke of all 6 games (scratch copies, 3600
   frames) is clean after this session's core changes.
2. **Mid-line PPU fidelity** (`docs/design/mid-line-ppu.md`, section 7 has
   per-step notes): steps 1 (map select/SCX fetch hold), 2 (SCX low-bit
   carry), 3 (per-plane SCY/LCDC.4) and D (double-speed write dot) are on
   main: +159 ROMs, 0 regressions. Steps E (window stall), F (objects), G
   (CGB tile-select glitch) are blocked on code size (270 B left; Needs
   human). Mealybug `m3_scx_low_3_bits` (pixels 152-159) is not the column
   carry: separate diagnosis.
2b. **M9 triage** (`docs/plan/m9-triage.md`, 17 groups): done this session:
   CGB palette RAM blocked around mode 3 (6110717, +31), LCDC.2 per OAM scan
   entry (3f7b00d, +20), live-SCX fine-scroll discard (e549dd7, +21). Next
   by ROMs per cost: step E window stall (97, needs .text), OAM DMA write
   conflicts (58, needs the human's reference pick), HDMA timing (75, L),
   STAT IRQ edges (47, L).
3. Remaining M8 items: `spsw-interrupts-cgbBC` (AGE `caution/`, low);
   M6 hardware lows; PPU edge cases from the M3 review; APU cost
   (optional); event-storm and ROM placement need Stage 2 hardware.
4. M8 close: gate passes now; then the milestone review (hardware
   correctness, test integrity incl. the romtest audio rule change
   23154a2/b74079a, perf/portability: this milestone changed hot paths:
   ROM reads, APU, PPU), update the baseline, tag `stage1-m8`. Not before
   the human's checklist findings are fixed.

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
device reopen with 2-16 s backoff after a stream loss (amendment; real
device loss untested: manual check below). Keys: arrows, Z = B, X = A,
Enter = Start, Right Shift/Backspace = Select, Esc quits.

Perf watch: `check-bench.sh` OK vs the committed baseline: `synth:worst-case`
-0.32% (cold-code cuts), `synth:cgb-raster` **+2.68% of the 3% guard** (one
mid-line SCX write per line now takes the held-fetch slow path, ~500-565 x86
Ir per write; reset at the M8 baseline update). APU: 883.4 x86 Ir per APU
write (budget <= 950). `.text` 95,836 of 96,000 B (164 B left) after two
cold-code cuts freed 1,530 B. ITCM set 54,942 B of a 57,344 B core budget
(`scripts/check-itcm-set.sh`, ADR 0026 Proposed). New gate checks this
session: `check-rom-loads.sh` (ADR 0027), `check-itcm-set.sh`. Fingerprint
corpus now includes the 198 PNG-reference gambatte ROMs (4,020 runs) and
fails on a corpus mismatch (`--allow-corpus-change` for the first run
against an older rev). Wall-clock baseline: `bench --wall` (docs/perf.md).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change".
- Resources: at most 2 concurrent builds, heavy steps alone; core
  experiments in scratch worktrees with their own target dir.
- Parallel agents can collide on ADR numbers: check `docs/decisions/`
  right before committing an ADR.

## Needs human
Open (session 10):
- **Sign-off candidates (M9):** 7 SameSuite APU ROMs whose names target
  other revisions (`*_extra_length_clocking-cgb0B` x3, `channel_3_*-cgb0`
  and `-cgbB`, `channel_1_freq_change_timing-A`, `-cgbDE`; a `-cgb0BC`
  variant exists for ours); possibly AGE `spsw-interrupts-cgbBC` (AGE
  `caution/`: breaks the halt Nintendo requires; its source still needs a
  read before asking).
- **Benchmark coverage:** should `synth:worst-case` gain one mid-line SCX
  write per line so the new slow path is measured (~+45k Ir/frame,
  estimate)? It makes the perf check stricter but moves the baseline.
- **Real-game checklist** (plan, bottom), on native Windows. Extra checks
  this session: Ctrl+C in the window still saves; change the default audio
  output device mid-game (expect `audio stopped`, then within ~2 s
  `output device back; resuming audio pacing`, no burst); a battery `.sav`
  survives a restart; a gamepad.
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

Current HEAD (e549dd7), not yet the committed baseline (`stage1-m7`
baseline is older; update at M8 close). `scripts/gate-stage1.sh` passes at
e549dd7.

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 39 | 40 | only age `spsw-interrupts-cgbBC` (AGE caution/) fails |
| stretch | 3,028 | 3,466 | 5 no-reference; M7 baseline was 2,765 |

Session 10 gains (vs the M7 baseline): WX=0 stall (8ba9c4c), line-0 mode 0
+ post-boot position 171 (740154c), frame-sequencer double-speed delay
(0a83ae5), LY read timing (d2b6e56), STOP DIV-reset TIMA edge (eb019ba),
mid-line PPU steps 1-3 + D (6cfe52b..f507248), palette RAM blocking
(6110717), LCDC.2 per scan entry (3f7b00d), live-SCX discard (e549dd7).
