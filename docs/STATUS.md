# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M1 (CPU + memory bus): acceptance passes, closing. M0 tagged `stage1-m0`.
- **Last updated:** 2026-11-21 (session 01; M0 log `logs/2026-11-21-01-m0-workspace-runner.md`)

## Next up

1. **Unsafe-free core (ADR 0009, approved by the human):** split `Memory` out of
   `Emulator`, `#![forbid(unsafe_code)]` in gbc-core, hard_rules check. In progress.
2. **M1 close:** one independent review of the API change (AGENTS.md trigger 3),
   then `--update-baseline`, tick the M1 boxes, session log, tag `stage1-m1`.
   The three M1 milestone reviews are already done and triaged (fixes in
   6fd632e..853f66f; deferred items are plan tasks).
3. **Quality bar tooling (README "Quality and performance bar", approved):**
   - a deterministic instruction-count benchmark under valgrind (e.g. iai-callgrind) with a
     committed baseline; the gate fails on a regression of more than a few percent;
   - code-size budgets for `run`/`execute_cb` on thumbv7em;
   - a short fuzz pass over untrusted inputs (ROM header/load, save RAM, RTC state) in the gate;
   - Miri only for `unsafe` outside the core (none yet).

   Nightly via rustup (user-level) is fine if a tool needs it.
4. **M2:** start with the ADR on a lazy, event-scheduled timer (plan task), then the
   timing work.

## Needs human

None open.

Decisions the human made 2026-11-21, for context:
- Integrity list human-owned.
- Gate hardened, including the SM83 data fetch with `GBC_REQUIRE_TEST_DATA=1`.
- Runner rulings folded into `acceptance/README.md`.
- AGENTS.md: agents may make strictly-tightening gate/contract changes and factual README fixes.
- README quality bar (core uses at most 50% of an H753 at 480 MHz in the worst case).
- Unsafe-free core (ADR 0009).
- Valgrind installed.
- The `.claude/settings.json` ask rule on `scripts/gate-*.sh` is still in place, so gate edits prompt.

## Environment

- Rust via **rustup** (user-level, `~/.rustup` and `~/.cargo`), not asdf.
  `cargo` is **not on PATH in non-interactive shells**: prefix commands with
  `source ~/.cargo/env &&`. Toolchain pinned to 1.100.0 in `rust-toolchain.toml`.
  No nightly installed yet.
- valgrind 3.22.0 at `/usr/bin/valgrind`.
- SM83 single-step data: `scripts/fetch-sm83-tests.sh` → `test-data/sm83/` (gitignored,
  hash-verified per file). The gate fetches it; locally set `GBC_REQUIRE_TEST_DATA=1`
  to make missing data fail instead of skip.
- Desktop system libraries installed 2026-11-21 (`./scripts/install-system-deps.sh --check` passes).
  `ldconfig` warnings about `/usr/lib/wsl/lib/libnvoptix*` are harmless WSL noise.
- Host C compiler: gcc 13.3 (`cc`). WSL2 Ubuntu 24.04. No `jq`.
- Use bash or Elixir for scripts (AGENTS.md); `/usr/bin/python3` exists but is not to be used.
- Hardware datasheets (DS13312, RM0468, ES0491, RM0433, ...) are in
  `../gbc-32-hardware/datasheets/`; `pdftotext` works on them.
- Commit only your own paths (`git add <paths>`): the human sometimes commits
  concurrently, and `commit -a` once swept in half-written files (d2b9658).

## Blockers

None.

## Acceptance snapshot

After M1 (CPU, bus, timer, serial, MBC1), as of 853f66f. The baseline is still the
M0 one until M1 closes. Full manifest ~4 s wall with 8 jobs. Blargg cpu_instrs runs
31 emulated s in 0.275 s host release (~110× real time).

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 42 | 42 | 2 | 0 | 0 | 0 |
| should (40) | 2 | 30 | 8 | 0 | 0 | 0 |
| stretch (3268) | 1 | 107 | 2 | 10 | 3144 | 4 |
