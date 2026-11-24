# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M9 (test-ROM completion), in progress; then M10 (in-depth
  review on Fable, started by the human). M8 is `stage1-m8` (ad56c23).
  M1-M7 tagged likewise.
- **Last updated:** 2026-11-23, session 15 (log
  `logs/2026-11-23-15-m9-apu-mealybug-lcdoffset.md`). Baseline 6def429
  (must 86/86, should 40/40, stretch 3,434/3,461).

## Next up: M9

3 open fix targets (27 stretch fails minus 24 signed off), all Mealybug,
agents in flight at session-15 log time (see In flight):
1. `m3_lcdc_tile_sel_change`, `m3_lcdc_tile_sel_win_change` (8 px, one
   line each): the TILE_SEL glitch latch carries over from the previous
   line; a correct model must also track BG reads with LCDC.4 set (a
   naive carry broke 6 gambatte `bgtiledata_spx*`). Branch
   `m9-tile-sel-latch`.
2. `m3_wx_4_change_sprites` (10 px): CGB WX-match fetch glitch (SameBoy
   `cgb_wx_glitch`). Branch `m9-cgb-wx-glitch`.
Whatever still fails after these goes to the human for sign-off with
evidence (human, session 15). Then M9's remaining tasks: M8 review lows
(plan M9 bullets) and the real-game checklist (human). Then M9 milestone
review and tag `stage1-m9`, then M10 (in-depth review, human starts it on
Fable). Items for M10: `obj_size_fetch` temporarily swaps one VRAM byte
for one object fetch (unobservable, but unusual); boot-tile setup
(`bus.rs` `boot_logo`); delayed LYC request kept in IF bits 5-6
(`LYC_IRQ_PENDING`, reads OR 0xE0, interrupt checks mask with
`INT_MASK`); line-0 pulse scheduler deadline one dot after its walk
point (`Ppu::next_event`); bbad441's 1 MHz phase shift under the ds APU
delay is inferred from one ROM pair; the TILE_SEL set-glitch latch lives
in `Memory` (`Hw` was at its size limit). Also open: single-speed
line-153/0 walk points a dot early vs gambatte ("Open" in
`docs/reference/stat-irq.md`).

**Budgets (main c6c4ea7):** `.text` 97,378 of 99,000 B (human raised it
from 97,500 in session 15 for the last 3 ROMs; per-function budgets
unchanged). `next_event` 586 of 590 B. Worst-case ~4.003 M Ir/frame,
99.6% of the 4.02 M line: the hot render path has no room left.
cgb-acid2 ~+1% vs bench baseline (guard +3%). APU per-write 916.4 of 950
Ir. `Hw` at its 864 B limit. Biggest remaining size lever (needs the
human, Stage 2 measurement): ~5.8 KB of M7 alignment `nop` padding
(`docs/perf.md` code size).

**Present-phase fix landed (session 14 end):** LADX slide route
irregularities beyond the beat 54.1 -> 1.1/min (slides 115 -> 0), Wario
Land 3 no-input 51 -> 1.6/min; costs ~6% of a core (spin waits) and ~1 ms
mean present delay; `GBC_PRESENT_PHASE=0 GBC_PRECISE_WAIT=0` restores the
old behaviour (ADR 0024 amendment). Desktop smoothness is done.

**Human (session 14): the desktop client is a throwaway used to validate
the core; spend minimal time on it.** Fix only what blocks core validation
(crashes, lost saves, wrong output). No more smoothness/pacing polish after
the in-flight present-phase fix. The SameBoy differential harness stays
worthwhile (it validates the core), at lower priority than M9 ROM fixes.

**Desktop front end (session 14):** audio-driven pacing now reads a
smoothed fill (WASAPI's 10 ms chunks made frames land 10/20 ms apart;
ADR 0024 amendments). `GBC_FRAME_STATS=1` prints per-4 s frame stats and
a `hitch:` line per gap (blank frames = game turned the LCD off; busy/step
= our code blocked; wake late = OS; high fill + few frames = pacer).
Saves are written on a `SaveWriter` thread (ADR 0023 amendment). Human
still finds Wario Land 3 slightly less smooth than SameBoy (vsync off in
SameBoy too). A long Shantae run (town + levels, build e2c908e, before the
hitch line) was clean: frame interval p1 ~16.0 / p99 ~17.5 ms, no gaps
after startup. Wario Land 3 gaps (67-469 ms) confirmed on bc07793: every `hitch:` line
is LCD-off blank frames (e.g. 28 frames, 27 blank), busy/wake late <2 ms.
Present step ~2 ms adds +-1 ms jitter (present p1 15.0 / p99 18.5 vs
frame p1 16.1 / p99 17.4); occasional `step max` 5-9 ms (one frame's
emulation; normal ~0.3 ms) is unexplained but within budget. Human: hitches don't match the stutter
felt. Link's Awakening DX (84858f7): hitches = 2-3 LCD-off frames; a
~40 s stretch with `wake late` mean 1.0 / max 4.1 ms (elsewhere 0.35 / ~1)
spread frame intervals to 13-20.5 ms: queued fix = sleep to ~2 ms before
due, then precise wait (after the replay tooling, `desktop-replay`, same
files). Stutter work: automatic detection from a per-frame log + DWM refresh
counts on agent-run native replays (human: noticing stutters and pressing
a mark key is not a good strategy; marks optional).
Tooling landed (ADR 0030, `docs/desktop-diagnostics.md`): record/replay,
`GBC_FRAME_LOG`, `scripts/analyze-frame-log.exs`, `scripts/synth-input.exs`
(LADX slide route). Human recording of a Wario Land 3 level:
`/mnt/x/gbc-32/wario-land-{session.txt,frames.csv}` (+ `.sav`); display
irregularities 70/min (beat 15), all 0-1 ms after vblank.
**Queued for a future session (human):** SameBoy differential harness:
SameBoy v1.0.5 core (MIT) built from source in scratch, boot ROMs from a
copy of `X:\sameboy_winsdl_v1.0.5` (no RGBDS needed), CGB C model, replay
our recordings, compare frames from the game's start (SameBoy runs its
boot ROM, we don't). Settles game-side questions (e.g. Wario Land 3 camera
steps) objectively. Next
candidates: present path jitter (softbuffer, +-1-2 ms) or display sync
(human: "decide after the stall fix").

**Safety (read AGENTS.md "Machine resources"):** never open the Linux
window under WSLg and never use the GPU from WSL. Window/audio/input smoke
tests use `scripts/run-windows.sh <rom> --frames N [--out shot.png]` on a
scratch ROM copy (it forwards `GBC_FRAME_STATS`/`GBC_AUDIO_STATS` via
WSLENV); short, and only for front-end changes. Windows Update can shut WSL
down mid-session: commit often.

Desktop front end (`crates/gbc-desktop`): `gbc-desktop <rom> [--scale K]
[--frames N] [--screenshot out.png] [--dmg-palette auto|gray]
[--headless]`. Default scale 4. Blank LCD shows white. Raw RGB555->888
colours, no colour correction (human decision, session 14). Save:
`<stem>.sav` (ADR 0023). Keys: arrows, Z = B, X = A, Enter = Start, Right
Shift/Backspace = Select, Esc quits (same game keys as SameBoy).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` proves "no behavior change" (in a
  worktree pass `--root /home/jess/gbc-32/test-roms`).
- Resources: at most 2 concurrent builds; heavy steps (check-bench,
  diff-fingerprint, gate) alone. Session 14 used a shared flock
  (`flock -s` for builds, `flock -x` for heavy steps) on one lock file in
  the session scratchpad; it serialises heavy steps but doesn't cap builds
  at 2, so launch at most 2 building agents.
- Core work in scratch worktrees, merged by rebase + `--ff-only`.
- ADR numbers: check `docs/decisions/` before committing one (next free 0030).
- PPU design size estimates ran ~2-3x low; measure early.

## In flight
- Agents (session 15) in worktrees under
  `/tmp/claude-1000/-home-jess-gbc-32/bf774c1c-1090-4ecf-b84f-19e3688e420c/scratchpad/`:
  `wt-latch` (branch `m9-tile-sel-latch`), `wt-wx` (`m9-cgb-wx-glitch`).
  If the session ended before they were merged, check the branches with
  `git log main..<branch>` and rebase + `--ff-only` merge them.

## Needs human

Open:
- **Stage 3 ROM memory (hardware repo, human-owned):** independent review
  `docs/reference/stage3-rom-memory-options.md` (session 12) recommends
  3.0 V HyperRAM on OCTOSPI (as on the STM32H735G-DK) over the ADR 0001 FMC
  SDRAM, medium confidence, pending a DK measurement; revisiting ADR 0001 is
  the human's call. Also found: the ADR 0002 pin draft puts SDNE0 on PC2_C,
  which adds 4.5 ns (DS13312 Table 76 note 2, checked) and breaks 95 MHz
  SDRAM timing if SDRAM stays.
- **Real-game checklist** (plan, bottom; M9 task), on native Windows.
  Session 14: Wario Land 3 and Shantae played (look and sound right);
  Ctrl+C and Esc saves, `.sav` across restart and the audio device switch
  all passed. Open: smoothness vs SameBoy (see Next up, desktop), a
  gamepad, Pokémon Crystal (RTC across a restart), Alone in the Dark, Link's
  Awakening DX, and Pokémon Red (DMG compat palette; not in the plan table).
- **Branches to delete** (agent `git branch -D` is denied). Stale/superseded:
  `ppu-edges`, `edges-hdma-scratch`, `lcd-offset-step1`, `lcd-offset-phase`,
  `ppu-edges-step1b` and `hdma-trigger` (landed as rebased copies in
  session 12). Merged in session 12: `m9-spsw`, `m9-step1b-hdma`, `m9-late-wy`.
  Fully merged (`git cherry main <b>` shows no `+`): `bench-worst-case-midline-scx`,
  `ppu-objmix`, `wip/speed-switch-h3`, `worktree-agent-a62aec9bd37d37e1c`,
  `worktree-agent-ab9ff6f40452030b3`, `worktree-agent-ae119d2f1346ad041`.
  Not fully merged, keep until checked: `ppu-bg-attributes` (1 `+`),
  `worktree-agent-a46dc3e70b2125ffb` (1 `+`). Worktree-agent branches may be
  locked worktrees (`git worktree remove --force --force`).
- Optional, strongest guard against a repeat of the BSOD: set
  `guiApplications=false` in `%UserProfile%\.wslconfig` (disables WSLg).
- FYI: `wsl-pro.service` restarts every 2 s because a cmd.exe AutoRun entry
  points at a missing `E:\Tools\ansicon\x64\ansicon`; harmless noise.
- Optional: disable or schedule WSL auto-updates (Windows Update shut WSL down
  mid-session on 2026-11-22).

Open: `../gbc-32-hardware/docs/firmware-interface.md` says the audio rate is "decided in firmware M6"; ADR 0021 fixed it at 32,768 Hz (agents may not edit that repo).

Decided in session 15 (2026-11-23), human:
- gbc-core total `.text` budget 97,500 -> 99,000 B ("Raise .text to
  99,000, try") for the last 3 Mealybug ROMs; leftovers come back for
  sign-off (c6c4ea7).
- SameSuite `channel_{1,2}_volume`, `channel_4_align`,
  `channel_1_sweep_restart`: "Sign off all 4" as CGB E results (each
  failing cell is SameBoy's CGB C value). Moved into
  `same-suite-apu-cgbE` (0be5600). FYI: 4 earlier-signed-off cgbE ROMs
  (`channel_4_lfsr_7_15`, `_lfsr_restart{,_fast}`,
  `_frequency_alignment`) now pass on our CGB C model; they stay in that
  group (moving them back would be a manifest change: stricter, the
  human's call).

Decided in session 14 (2026-11-23), human:

- SameSuite `channel_1_freq_change_timing-cgb0BC`: sign off ("Sign off
  target"): it needs the CGB C PCM12 read glitch; a model fitted to it broke
  14 passing SameSuite ROMs that expect CGB E reads. Own stretch group
  `same-suite-apu-pcm-glitch` (22fe162). The frequency-timing half landed
  (91ff0de).
- Mealybug `m3_lcdc_win_en_change_multiple_wx`, `m3_wx_{4,5,6}_change`,
  `win_without_bg`: sign off, no CGB C reference upstream ("yes do #1").
  Own group `mealybug-no-cgb-reference` (d801a6c), still run.
- Desktop: no colour correction; raw colours stay ("dont do that color
  correction"). Copied SameBoy (R) glyph bytes with notice: "fine".
- Display sync (vsync, would supersede ADR 0024's audio-driven pacing):
  "Decide after stall fix".

Decided in session 13 (2026-11-23), human:
- cgb-acid2 bench at +3.37% after the STAT 144/153/0 correctness fixes
  (perf pass recovered what it could, fingerprint identical; worst-case
  4.007 M Ir/frame stays under the 4.02 M line): **rebaseline**
  `perf/bench-baseline.txt` ("Rebaseline (Recommended)"), over raising the
  `point_irq` budget, raising the `Hw` size limit, or backing fixes out.

Decided in session 12 (2026-11-23), human:
- SameSuite group 15: sign off the 9 E-only ROMs (ch1/2 `nrx2_glitch`,
  `nrx2_speed_change`; ch4 `lfsr_7_15`, `lfsr_restart{,_fast}`,
  `freq_change`, `frequency_alignment`) as "CGB E result". "Sign off the
  9". Moved into stretch group `same-suite-apu-cgbE` (cde49e4, baseline
  303bc6d). The other 8 stay fix targets.

Decided in session 11 (2026-11-23), human:
- APU double speed: follow gambatte's CGB C linear trigger rule; sign off
  SameSuite `channel_{1,2}_align{,_cpu}` (4) as "CGB E result" (SameSuite
  apu/README: CPU-CGB-C fails most ch1/2 tests). "Yes". Manifest change
  approved: exclude them from `same-suite-apu`, own stretch group
  `same-suite-apu-cgbE` (keeps running, reported), baseline refresh with
  `--allow-removed`.

- Power-on OBJ palette RAM: sign off gambatte `scx_during_m3_spx2`,
  `scx_during_m3_spx2_ds`, `scx_attrib_during_m3_spx2_ds` as "depends on
  uninitialised palette RAM" (they draw OBJ palette 0 colour 2, never
  written; the cgb04c capture shows that console's power-on 0x21926C; we
  start at 0, deterministic). "Sign off". Manifest change approved: own
  stretch group (keeps running, reported), baseline with `--allow-removed`.

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

Baseline 6def429 (`acceptance/results/stage1.json`, session 15; session 14
ended at 3,410; M8 tag `stage1-m8` was stretch 3,126). `scripts/gate-stage1.sh`
passed at 0be1ad0 (session 15).

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 40 | 40 | |
| stretch | 3,434 | 3,461 | 27 fail, 5 no-reference. Signed off (still run, counted as fail where failing): 7 other-revision SameSuite, 13 failing of 17 in `same-suite-apu-cgbE`, 1 in `same-suite-apu-pcm-glitch`, 3 in `gambatte-uninit-palram`, 5 no-reference in `mealybug-no-cgb-reference`. 3 open fix targets |
