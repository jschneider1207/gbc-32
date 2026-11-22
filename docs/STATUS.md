# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M3 (PPU, DMG-compatibility rendering + screenshots):
  acceptance passes, closing. M2 is `stage1-m2`, M1 `stage1-m1`.
- **Last updated:** 2026-11-21, during session 05 (M3)

## Next up

M3 acceptance passes (verified at 745b06a: the seven M3 groups, 52/52 ROMs,
`--gate must` exit 0). Session 05 (in progress) state:
- Done: ADR 0013 (RGB555, stride 160), PPU mode timing (ADR 0014,
  f71dbce; mooneye-acceptance-ppu 7/7, mooneye-cgb-misc 4/4), VRAM/OAM
  blocking (22eccab), renderer (53c0c37..7f53075) wired in (265a078).
  Must 66/86, should 24/40, stretch 12 (before the M3 baseline update).
- Running: OAM DMA as the PPU sees it + gambatte HALT-DMA confirmation
  (plan M3, the two unticked tasks).
- Then: milestone review (hardware correctness, test integrity, and
  performance: the hot path changed), fix confirmed high/medium,
  `--update-baseline`, tag `stage1-m3`, session log 05.

Must failures left after M3 are all later milestones: mbc3-tester and
rtc3test x3 (MBC3/RTC not implemented, M4; the renderer draws them
correctly), mooneye-mbc x14 (M4), cgb-acid2 (M5), cgb_sound (M6).
`blargg-interrupt-time` (should) fails after the speed switch (M5).

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` (3,822 runs incl. frame buffer, VRAM,
  OAM, input pass; ~20 s) proves "no behavior change"; behavior changes
  must list and explain every differing ROM. Fingerprinting the frame
  buffer means every PPU change will diff broadly: explain by group.
- Gambatte ROMs report `unsupported` in romtest (plan task, M5). For
  hex-output checks agents have used a throwaway /tmp harness reading
  the 0x9800 tilemap; with a real PPU that task becomes cheaper.
- The PPU is the next hot path: run `scripts/check-bench.sh` and
  `check-code-size.sh` on each step; the perf review triggers at M3 close.

## Needs human

None open.

Context on earlier human decisions:
- `acceptance/test-roms.sha256` is human-owned.
- Strictly-tightening gate/contract changes and factual README fixes are
  agent decisions.
- README quality bar: the core uses at most 50% of an H753 at 480 MHz in
  the worst case.
- Unsafe-free core (ADR 0009).
- The project is MIT-licensed. AGENTS.md has a rule on dependency licenses
  and on not copying code from copyleft emulators.
- Review I7 (session 04): 8ede259 called exact-M-cycle input timing
  "stricter"; it is a pre-announced reinterpretation with zero status
  changes. Told the human, no objection.
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
- **Human rule (session 04):** a test counts only once it has been seen to
  fail against broken code (AGENTS.md).
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
- **Gate:** `scripts/gate-*.sh` edits prompt the human (`.claude/settings.json`).

## Blockers

None.

## Acceptance snapshot

As of ac36d13 (the M2 baseline). `scripts/gate-stage1.sh` passes every step
except the last (must-tier ROMs owned by M3/M4), with no regressions.

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 60 | 24 | 2 | 0 | 0 | 0 |
| should (40) | 4 | 28 | 8 | 0 | 0 | 0 |
| stretch (3268) | 1 | 107 | 2 | 10 | 3144 | 4 |

Must-tier failures: Blargg ×6, rtc3test ×3, mbc3-tester (screenshot only,
M3); cgb-acid2/dmg-acid2 timeouts (M3); MBC2/MBC5 ×14 (M4).

Performance (`docs/perf.md`, `perf/bench-baseline.txt`, Ir over the listed
emulated time):
- cpu_instrs 1,388.0 M, cgb_sound 607.5 M, cgb-acid2 12.86 M (20 s each;
  M1 was 1,926 M / 923 M / 928 M).
- Synthetic: event-storm 413.7 M (2 s, ~3.46 M Ir/frame, the 50%-of-M7
  line is 4.02 M — x86 proxy only, plan M8), dma-storm ~372 M (3 s),
  halt-irq ~111 M (10 s).
- Code size (thumbv7em): `Cpu::run::<Ctx>` 29,224 B (budget 30,000),
  `execute_cb` ~13.2 KB (13,820); budgets also on boundary, div_reset,
  Bus::stop, DMA paths, timer I/O.
