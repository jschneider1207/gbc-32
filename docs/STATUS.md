# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M5 (CGB features), not started. M4 is `stage1-m4`
  (0bf708d), M3 `stage1-m3`, M2 `stage1-m2`, M1 `stage1-m1`.
- **Last updated:** 2026-11-22, end of session 06 (M4 close; log
  `logs/2026-11-22-06-m4-carts.md`)

## Next up

Start M5 (`docs/plan/stage-1.md` "M5") in a fresh session. Acceptance:
`cgb-acid2` and `mooneye-cgb-boot-regs` passing; `same-suite-cgb` and
`age-cgb` tracked. Already in place from M4: CGB palette RAM colors for BG
(41a47b2: BCPS/BCPD/OCPS/OCPD storage, auto-increment, read-back, 8 BG
pair tables in `Memory`); CGB-mode objects still use OBP0/OBP1.

Suggested order (one behavior per agent; chain agents that share files):
1. VRAM bank 1 + BG map attributes (palette, bank, flips, priority), with the
   LCDC.0 master-priority meaning in CGB mode (plan task from the M4 review).
2. CGB object attributes (palette 0-7 from OCPD, VRAM bank) and CGB
   priority (OPRI). Keep a separate X sort for mode-3 stall timing (plan
   task: `sorted_object_stalls` depends on `priority_order()` being an X sort).
3. WRAM banking, then GDMA/HDMA timing.
4. Double speed / STOP fixes (H3, H5), then post-boot CGB-mode handover.
5. romtest `gambatte` check, so the stretch-tier gambatte suite reports.

Perf watch (must stay honest): `synth:worst-case` is 3.66 M x86 Ir/frame
(91% of the 4.02 M line), but the line assumes 1 Ir = 1 M7 cycle, and the
M4 perf review measured thumbv7em code at 1-2x (u64 maths). Treat the
headroom as thin. Run `check-bench.sh` and `check-code-size.sh` (now with
a total `.text` budget, 85,100 B; 83.4 KB used) on every hot-path step,
and add M5 features to `synth:worst-case`/`synth:cgb-raster` as they land.
No hardware yet (human, 2026-11-22), so the M7 measurement waits for Stage 2.

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` (3,822 runs incl. frame buffer, VRAM,
  OAM, input pass; ~20 s) proves "no behavior change"; behavior changes
  must list and explain every differing ROM. It doesn't cover the synth
  benches; check those with frame hashes.
- Gambatte ROMs report `unsupported` in romtest (plan task, M5). The two
  PPU-side OAM DMA deviations from SameBoy are provisional until then
  (`docs/reference/oam-dma.md`).
- Agents spawned with a fixed tool-call cap (~20-45) and one theme stayed
  on budget this session; the uncapped ones ran 50-85 calls.

## Needs human

Open (session 07):
1. 198 Gambatte ROMs with `*_cgb04c.png` references (`scy/`, `bgen/`,
   `bgtilemap/`, `scx_during_m3/`, ...) have no `cgb04c` in the ROM name,
   so `gambatte-cgb` never selects them. Add a group or glob for them?
   (Manifest change. A directory glob would also select ROMs with no CGB
   rule, which is a runner error; ADR 0018.)

Decided in session 07 (2026-11-22): the human approved loosening the
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
- **Worktrees:** agents in `git worktree`s must not symlink `test-roms`
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

As of the M4 baseline (`stage1-m4`). `scripts/gate-stage1.sh` passes every
step except the last (must-tier ROMs owned by M5-M6), with no regressions.

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 84 | 2 | 0 | 0 | 0 | 0 |
| should (40) | 26 | 14 | 0 | 0 | 0 | 0 |
| stretch (3268) | 12 | 97 | 0 | 10 | 3144 | 5 |

Must-tier failures: cgb-acid2 (M5), cgb_sound (M6). `blargg-interrupt-time`
(should) fails after the speed switch (M5).

Performance (`docs/perf.md`, `perf/bench-baseline.txt` at cf95ba4; x86 Ir):
- cpu_instrs 1,595 M (20 s), cgb_sound 841 M, cgb-acid2 512 M (was 790 M
  at M3).
- Synthetic: worst-case 437 M (3.66 M/frame, 91% of the 4.02 M line),
  cgb-raster 294 M (2.46 M/frame), event-storm 396 M, dma-storm 418 M,
  halt-irq 222 M.
- Code size (thumbv7em): gbc-core `.text` 83.4 KB (total budget 85,100 B;
  over the H753's 64 KB ITCM, plan task). `Hw` 832 B (assert <= 864).
