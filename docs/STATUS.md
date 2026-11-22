# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M4 (cartridges + saves), in progress. M3 is `stage1-m3`,
  M2 `stage1-m2`, M1 `stage1-m1`.
- **Last updated:** 2026-11-22, mid session 06 (log: `logs/2026-11-22-06-m4-carts.md`)

## Next up

Cartridge work is done: MBC1 (+ multicart, ADR 0016), MBC2, MBC3 + RTC on
emulated time, MBC5, `RtcState::advance_seconds`, save persistence API
(ADR 0015, reviewed, fixes in c2ab199). Mooneye MBC ROMs all pass; rtc3test
passes every subtest in-ROM.

Remaining M4 work, in order (plan "M4"):
1. Perf: `synth:worst-case` is 4.04 M Ir/frame after e15d85f (100.4% of the
   4.02 M line). Target <= 3.82 M (95%). In flight at this writing: a
   render palette-pair cut (`ppu/render.rs`) and an event-boundary /
   scheduler cut. Check `git log` for their commits.
2. `Hw` is 1,000/1,024 B: move per-line buffers into `Memory` (plan task).
3. CGB palette colors (BCPS/BCPD/OCPS/OCPD -> colors only), so the rtc3test
   screenshots (green PASS) match.
4. Milestone close: reviews DONE (see log), fixes landed; waiting on the
   human (Needs human 1-2), then baseline + tag `stage1-m4`.

Working rules that matter here:
- Every new test is mutation-checked: break the code, see it fail (AGENTS.md).
- `scripts/diff-fingerprint.sh <rev>` (3,822 runs incl. frame buffer, VRAM,
  OAM, input pass; ~20 s) proves "no behavior change"; behavior changes
  must list and explain every differing ROM.
- Gambatte ROMs report `unsupported` in romtest (plan task, M5). The two
  PPU-side OAM DMA deviations from SameBoy are provisional until then
  (`docs/reference/oam-dma.md`).
- Run `scripts/check-bench.sh` and `check-code-size.sh` on hot-path steps.

## Needs human

Approved 2026-11-22, being implemented (then tag `stage1-m4`):
1. **mbc3-tester `run_frames`**: the ROM's result first appears at 40.59
   frames (counted as 70,224-clock units from the post-boot handover). That's
   41,368 clocks after the 40-frame cutoff. No core or runner bug was found,
   and upstream probably counts frames drawn. Proposal: `run_frames = 41`
   for this group only (it passes with 0.41 frame of margin). Alternatives:
   redefine `run_frames` everywhere, or leave it failing.
2. **Reference-override mechanism** (test-integrity review M1/M2): a89713c
   and 3da7107 added a committed, pinned PNG override plus a new ownership
   class. The human approved the recolor, not this mechanism, and ADR 0007
   wasn't amended. Proposal: replace it with a manifest-described in-memory
   recolor of the pinned upstream PNG, `recolor = { from, to, count }`,
   where `to` must be a contract palette color, plus a dated ADR 0007
   amendment. Then remove `acceptance/references/` and its README rows.
3. NUCLEO-H753ZI: not available yet (human, 2026-11-22). M7 cycle
   measurement waits for Stage 2 (perf review HIGH, plan task).

Resolved in session 06: the mbc3-tester recolor itself was approved, and
ADR 0015 was agreed to extend ADR 0006, not change it.

FYI (session 05): the human saw "a lot of compile errors" (session 05).
build, clippy `-D warnings`, tests, the thumbv7em core build and the
nightly fuzz crate are all clean with the pinned toolchain, so this is
editor-side (likely rust-analyzer using a different toolchain, e.g. a
Windows editor on WSL files, or loading `tools/fuzz/fuzz`, which needs
nightly). Waiting on an example error if it persists.

Context on earlier human decisions:
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

As of the M3 baseline (`stage1-m3`). `scripts/gate-stage1.sh` passes every
step except the last (must-tier ROMs owned by M4-M6), with no regressions.

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 66 | 20 | 0 | 0 | 0 | 0 |
| should (40) | 24 | 16 | 0 | 0 | 0 | 0 |
| stretch (3268) | 12 | 97 | 0 | 10 | 3144 | 5 |

Must-tier failures: mooneye-mbc ×14, rtc3test ×3, mbc3-tester (M4);
cgb-acid2 (M5); cgb_sound (M6). `blargg-interrupt-time` (should) fails
after the speed switch (M5).

Performance (`docs/perf.md`, `perf/bench-baseline.txt`; Ir/frame from the
M3 perf review):
- cpu_instrs 1,729 M (1.45 M/frame), cgb_sound 974 M (0.82 M), cgb-acid2
  789 M (0.66 M; PPU ~0.53 M of it), each 20 s.
- Synthetic: event-storm 446 M (3.73 M/frame, 93% of the 4.02 M line),
  dma-storm 437 M, halt-irq 292 M.
- Code size (thumbv7em): gbc-core `.text` 79.6 KB (no longer fits a 64 KB
  ITCM; plan task). `Cpu::run` 27,760 B (budget 29,150), `execute_cb`
  12,622, `Ppu::sync` 2,918, `render` 2,684, `start_line` 2,208.
