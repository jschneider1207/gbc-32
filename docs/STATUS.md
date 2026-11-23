# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M9 (test-ROM completion), in progress; then M10 (in-depth
  review on Fable, started by the human; plan added 2026-11-23). M8 is `stage1-m8`
  (ad56c23). M1-M7 tagged likewise.
- **Last updated:** 2026-11-23, end of session 12 (log
  `logs/2026-11-23-12-m9-edges-window.md`). Baseline 47f186c (must 86/86,
  should 40/40, stretch 3,301/3,461); full gate passed at 47f186c.

## Next up (session 13): M9

`docs/plan/stage-1.md` M9 + `docs/plan/m9-triage.md` (17 groups). Real-game
checklist findings from the human become the first fixes when they arrive.
Failing by area at 47f186c (169 incl. 5 no-reference, 23 signed off):
gambatte-cgb 108 (dma 32; m1/m2enable/m0enable/lycEnable/lcd_offset ~58),
Mealybug 20, SameSuite 8 fix targets, cgb-acid-hell 1, AGE m3 1,
`scx_0367c0` PNG 4.
1. Line-0 mode-2 IRQ pulse in double speed is likely half a dot later than
   ours (lcd-offset design step 5 leftover; needs a separate walk point in
   the hot path): `window/late_enable_afterVblank_ds_lcdoffset1_2`,
   `lyc153int_m2irq_late_retrigger_ds_1`, and probably
   `lycint152_lyc0irq_late_retrigger_ds_1`. A probe moving line 0's `Ly4`
   to t=5 in ds flipped two but regressed two LYC=0 `ds_lcdoffset1` rows.
   Then lcd-offset design steps 4, 6-9.
2. Window group 4 left (4): the ds row above; Mealybug
   `m3_lcdc_win_map_change`, `m3_lcdc_tile_sel_win_change` (window map/tile
   writes vs object stalls); `m3_wx_4_change_sprites` (CGB WX-match glitch,
   SameBoy `cgb_wx_glitch`).
3. Group 12 remaining: m0enable/disable_*, lycdisable_ff45_* edge positions;
   lycstatwirq ly00 pair.
4. HDMA HALT sub-group leftovers (16), GDMA ds length (4), OAM DMA restart
   `srcchange0000_busyinc`.
5. Mealybug step F leftovers (BGP/OBP0/LCDC.1/LCDC.2 in an object fetch),
   step G, `LCDC_TILES_LATE` lead.
6. Headroom: `.text` 92,152 of 97,500 B, cgb-acid2 +2.43% of its +3% guard
   (after the 10777f8 size pass). Unused size ideas: share the
   `set_stalls`/`set_stalls_retriggered` tail (~60-100 B); de-unroll the
   `set_stalls` copy loops (~500 B, re-check Ir). M8 review lows still open
   (ITCM globs, per-function budgets, `power_cycle`/`eject` stack frames).
7. SameSuite group 15 fix targets (8): `channel_1_freq_change_timing-cgb0BC`
   and the 7 unclear (ch1 sweep x2, volume x2, ch4 align/delay/
   equivalent_frequencies).
After M9: M10, in-depth review (human starts it on Fable).

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
3600 frames) is clean at c8e8626.

Perf watch (`perf/bench-baseline.txt`): `synth:worst-case` is +0.02% vs its
baseline (476.57 M Ir; ~99.4% of the 4.02 M x86 Ir/frame line). cgb-acid2
+2.40% of the +3% guard on main (10777f8). APU 219.5k of 230k Ir/frame; 908.3 of 950 Ir per APU write;
`Ctx::write_apu` 498/508 B. `.text` 92,134 of 97,500 B on main (10777f8). The 50% bar
covers realistic worst cases; dense mid-mode-3 writes are outside it
(docs/perf.md). ITCM set 48,692 of 57,344 B (ADR 0026 Proposed). Gate checks:
code size, ITCM set, byte-wide ROM loads (ADR 0027), bench. Fingerprint corpus
4,020 runs. Wall-clock baseline: `bench --wall` (docs/perf.md).

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

## In flight
- Session 13 (2026-11-23): merged to main, bench/gate/baseline NOT yet run:
  c274be8 (HDMA across a speed switch, +21; ADR 0017/0020 amendments) and
  8e30952..046958f (STAT edges at lines 144/0, +23; ADR 0029). Combined
  tree: must 86, should 40, stretch 3,345/3,461, 0 regressions, tests clean.
- Agents: `m9-hdma-halt` (worktree `../gbc-32-hdma-halt`, HDMA/HALT
  leftovers ~11) and `m9-stat153` (worktree `../gbc-32-stat153`, STAT edges
  at lines 153/0: m2enable, lycEnable, ly0/lyc153 ds retrigger).

## Needs human
Open:
- **Stage 3 ROM memory (hardware repo, human-owned):** independent review
  `docs/reference/stage3-rom-memory-options.md` (session 12) recommends
  3.0 V HyperRAM on OCTOSPI (as on the STM32H735G-DK) over the ADR 0001 FMC
  SDRAM, medium confidence, pending a DK measurement; revisiting ADR 0001 is
  the human's call. Also found: the ADR 0002 pin draft puts SDNE0 on PC2_C,
  which adds 4.5 ns (DS13312 Table 76 note 2, checked) and breaks 95 MHz
  SDRAM timing if SDRAM stays.
- **Real-game checklist** (plan, bottom; M9 task), on native Windows. Extra
  checks: Ctrl+C in the window still saves; change the default audio output
  device mid-game (expect `audio stopped`, then within ~2 s `output device
  back; resuming audio pacing`, no burst); a battery `.sav` survives a
  restart; a gamepad.
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

Baseline 47f186c (`acceptance/results/stage1.json`, session 12 end; M8 tag
`stage1-m8` was stretch 3,126). `scripts/gate-stage1.sh` passed at 47f186c.
(.text 92,152 B, cgb-acid2 +2.43%, worst-case +0.02%.)

| Tier | Pass | Total | Notes |
|---|---|---|---|
| must | 86 | 86 | |
| should | 40 | 40 | |
| stretch | 3,301 | 3,461 | 160 fail, 5 no-reference. Signed off (still run, counted as fail where failing): 7 other-revision SameSuite, 13 in `same-suite-apu-cgbE`, 3 in `gambatte-uninit-palram` |
