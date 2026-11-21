# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M2 (timing, interrupts, timers, OAM DMA). M1 is tagged `stage1-m1`.
- **Last updated:** 2026-11-21, session 04 (`logs/2026-11-21-04-adr0011-event-scheduled-timer.md`)

## Next up

ADR 0011 steps 1-5 are done (lazy event-scheduled timer/serial, HALT idles
to the next deadline). The M2 acceptance subset now passes 17/17, but M2
plan tasks remain; finish them before the milestone review and tag.

1. **HALT tasks** (`cpu/mod.rs`): IME=1 with an interrupt pending at HALT's
   check re-executes HALT (`Cpu::halt()`, rewind PC, `TODO(M2)` there);
   `EI; HALT`; CGB +1 M-cycle wake-up latency (one `bus.idle()` in the
   wake-up branch of `step_not_running`'s `Mode::Halted` arm). Changes
   behavior: expect fingerprint diffs, explain each.
2. **OAM DMA** timing and bus conflicts, on a `Dma` deadline with lazy
   progress (ADR 0011 "OAM DMA and scheduled inputs"); `peek` stays
   conflict-free; measure any `read` cost with `check-bench`.
3. **`set_buttons_at`** (Input deadline), romtest switched to it.
4. ADR 0011 open hardware questions (CGB timer-disable tick; serial phase
   after DIV reset). Also check the two unticked top M2 items (memory
   access timing, interrupt dispatch/HALT bug/IE-IF) against what's done.
5. **Milestone close:** reviews per AGENTS.md (hardware correctness, test
   integrity, performance/portability: the hot path changed), fix, then
   `--update-baseline` and tag `stage1-m2`.

Every new test is mutation-checked (AGENTS.md): break the code, see it fail.
Verification tools: `scripts/diff-fingerprint.sh <rev>` (295 ROMs, ~7 s).

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
- **Gate:** `scripts/gate-*.sh` edits prompt the human (`.claude/settings.json`).

## Blockers

None.

## Acceptance snapshot

As of 84c7744. The committed romtest baseline is still M1's (f2c7098); the
only status changes against it are `timer/tima_write_reloading` and
`timer/tma_write_reloading` (fail → pass).

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 44 | 40 | 2 | 0 | 0 | 0 |
| should (40) | 2 | 30 | 8 | 0 | 0 | 0 |
| stretch (3268) | 1 | 107 | 2 | 10 | 3144 | 4 |

Performance (`docs/perf.md`, baseline updated in 84c7744):
- Benchmark Ir over 20 emulated s: cpu_instrs 1,382,484,838, cgb_sound
  622,018,571, cgb-acid2 13,094,335 (was 1,926 M / 923 M / 928 M at M1).
- Code size: `Cpu::run::<Ctx>` 30,896 B (budget 32,500), `execute_cb`
  13,870 B (budget 14,600).
