# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M1 (CPU + memory bus), not started. M0 closed and tagged `stage1-m0`.
- **Last updated:** 2026-11-21 (session 01, log `logs/2026-11-21-01-m0-workspace-runner.md`)

## Next up

1. M1, before fixing the bus signature: the per-run-call context struct and the ROM
   size/mask decision (both tasks in the M1 plan section, from the API review).
2. M1: SM83 decode/execute with unit tests (consider the SM83 JSON single-step tests),
   memory map, MBC1, serial, post-boot registers (CGB and DMG-compat values).
3. M1 acceptance: `romtest --group blargg-cpu-instrs --group blargg-instr-timing --only-check serial --gate must`.

## Needs human

None. (2026-11-21: the human approved the three M0 items: integrity list is
human-owned, gate alloc/hard-rules step strengthened, runner rulings folded into
`acceptance/README.md`; commits 35f8cba, adf17a7.)

## Environment

- Rust via **rustup** (user-level, `~/.rustup` and `~/.cargo`), not asdf.
  `cargo` is **not on PATH in non-interactive shells**: prefix commands with
  `source ~/.cargo/env &&`. Toolchain pinned to 1.100.0 in `rust-toolchain.toml`.
- Desktop system libraries installed 2026-11-21 (`./scripts/install-system-deps.sh --check` passes).
  `ldconfig` warnings about `/usr/lib/wsl/lib/libnvoptix*` are harmless WSL noise.
- Host C compiler: gcc 13.3 (`cc`). WSL2 Ubuntu 24.04. No `jq`.
- `python3` through asdf has no version set; `/usr/bin/python3` (3.12) exists, but
  use bash or Elixir for scripts (AGENTS.md).
- Commit only your own paths (`git add <paths>`): the human sometimes commits
  concurrently, and `commit -a` once swept in half-written files (d2b9658).

## Blockers

None.

## Acceptance snapshot

Stub core (no CPU), baseline `acceptance/results/stage1.json` at `stage1-m0`.
Full manifest: 3394 group/ROM runs, ~9.7 s wall time with 8 jobs (includes ~0.4 s
of SHA-256 verification of `test-roms/`).

| Tier | Pass | Fail | Timeout | Unsupported | No reference | Score |
|---|---|---|---|---|---|---|
| must (86) | 0 | 10 | 76 | 0 | 0 | 0% |
| should (40) | 0 | 1 | 39 | 0 | 0 | 0% |
| stretch (3268) | 0 | 0 | 114 | 3154 | 0 | 0% |
