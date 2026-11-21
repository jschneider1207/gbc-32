# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M2 (timing, interrupts, timers, OAM DMA). M1 is tagged `stage1-m1`.
- **Last updated:** 2026-11-21, end of session 03 (`logs/2026-11-21-03-m1-close-quality-bar.md`)

## Next up

1. **ADR 0011 implementation**, in the order listed in the M2 plan:
   - event table + PPU on it;
   - fingerprint tooling;
   - the reload window in the eager timer, kept afterwards as a test-only
     reference;
   - the lazy timer and serial clock with a differential test;
   - halted/idle jumps to the next deadline.

   Read `docs/decisions/0011-event-scheduled-timer.md` first. It is
   detailed, with measured numbers from a throwaway prototype. Each step
   is new emulation/timing logic, so use the most capable model.
   - When it lands, rebaseline the benchmark and size budgets in the same
     commit, with the numbers in the commit body (ADR 0010).
   - The prototype went over the `write_high`/`read_high` size budgets.
     Keep the timer I/O out of line.
2. The rest of M2: HALT tasks, OAM DMA, scheduled inputs at M-cycle
   resolution, and `hard_rules` hardening (M1 API review, low).
3. **M2 acceptance subset:** 15/17 must pass. Failing:
   `timer/tima_write_reloading` and `timer/tma_write_reloading` (the TIMA
   reload window, ADR 0011 step 3).

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

As of 8e9bae8. The committed baseline is the M1 one (f2c7098). The gate
fails only at the last step (must-tier ROMs), as expected mid-stage:
fmt, clippy, tests, no-alloc, code size, the benchmark and fuzzing all
pass. The full gate takes about 1 min with the valgrind benchmark.

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 42 | 42 | 2 | 0 | 0 | 0 |
| should (40) | 2 | 30 | 8 | 0 | 0 | 0 |
| stretch (3268) | 1 | 107 | 2 | 10 | 3144 | 4 |

Performance (`docs/perf.md`):
- Benchmark Ir over 20 emulated s: cpu_instrs 1,926,303,053, cgb_sound
  922,727,228, cgb-acid2 928,451,269.
- Code size: `Cpu::run::<Ctx>` 37,064 B (budget 39,000), `execute_cb`
  15,702 B (budget 16,500).
