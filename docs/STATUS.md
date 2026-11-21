# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M1 (CPU + memory bus): acceptance passes; close-out remaining (below). M0 tagged `stage1-m0`.
- **Last updated:** 2026-11-21, end of session 01 (logs `logs/2026-11-21-01-*.md`, `logs/2026-11-21-02-*.md`)

## Next up

1. **Close M1:**
   - The core API changed in 262e011 (ADR 0009: caller-provided `Memory`,
     `#![forbid(unsafe_code)]`), so run **one** API reviewer scoped to
     `6fd632e^..HEAD` core changes (AGENTS.md trigger 3).
   - Triage under the new rules: fix high and medium, send low to the plan,
     no re-review.
   - The three M1 milestone-close reviews are already done and triaged
     (fixes 6fd632e..853f66f; deferred items are plan tasks), so don't
     repeat them.
   - Then:
     - `cargo run -p romtest --release -- --manifest acceptance/stage1.toml --update-baseline`
       (no regressions expected; must 42/86);
     - tick the M1 boxes in the plan;
     - add a "Review" section to the session-02 log;
     - `git tag -a stage1-m1 -m "..."` (tags must be annotated here).
2. **Quality-bar tooling** (README "Quality and performance bar", approved):
   - **Benchmark:** a deterministic instruction-count benchmark under
     valgrind (e.g. iai-callgrind) on fixed ROMs, with a committed baseline.
     The gate fails on a regression of more than a few percent.
   - **Code size:** budgets for `Cpu::run::<Ctx>` (37,064 B now) and
     `execute_cb` (15,702 B) on thumbv7em with `-C target-cpu=cortex-m7`.
   - **Fuzzing:** a short fuzz pass in the gate over untrusted inputs: ROM
     header/`load`, save RAM, `set_rtc_state`. It must stay deterministic in
     the gate, e.g. fixed-seed or bounded corpus runs. Nightly via rustup is
     fine if needed (user-level).
   - All of these only make the gate stricter, so they are agent decisions.
     Note that the `.claude/settings.json` ask rule on `scripts/gate-*.sh`
     will still prompt the human.
3. **M2:** first the ADR on a lazy, event-scheduled timer vs the eager tick
   (plan task), then interrupt, HALT, timer and OAM DMA timing. The M2 plan
   section lists the review findings already queued.

## Needs human

None open.

Decisions the human made on 2026-11-21, for context:
- `acceptance/test-roms.sha256` is human-owned.
- The gate was hardened, including the SM83 data fetch with
  `GBC_REQUIRE_TEST_DATA=1`.
- The runner rulings were folded into `acceptance/README.md`.
- Strictly-tightening gate/contract changes and factual README fixes are
  agent decisions.
- README quality bar: the core uses at most 50% of an H753 at 480 MHz in
  the worst case.
- Unsafe-free core (ADR 0009).
- Valgrind is installed.
- Leaner review rules (AGENTS.md "Independent review", f9b9126).

## Environment

- **Rust:** via rustup (user-level, `~/.rustup` and `~/.cargo`), not asdf.
  - `cargo` is **not on PATH in non-interactive shells**: prefix commands
    with `source ~/.cargo/env &&`.
  - The toolchain is pinned to 1.100.0 in `rust-toolchain.toml`. No nightly
    is installed yet.
  - `-C target-feature=+strict-align` is unstable on 1.100 (it warns), so
    the unaligned-ROM-read constraint is handled in code (plan task).
- **valgrind** 3.22.0 at `/usr/bin/valgrind`.
- **SM83 single-step data:** `scripts/fetch-sm83-tests.sh` →
  `test-data/sm83/` (gitignored, hash-verified per file via
  `tools/sm83-tests/sm83-v1.sha256`). Locally, set
  `GBC_REQUIRE_TEST_DATA=1` so missing data fails.
- **Desktop system libraries:** installed
  (`./scripts/install-system-deps.sh --check` passes). WSL2 Ubuntu 24.04,
  gcc 13.3, no `jq`. Scripts are written in bash or Elixir, not Python.
- **Hardware datasheets** (DS13312, RM0468, ES0491, RM0433, ...) are in
  `../gbc-32-hardware/datasheets/`, and `pdftotext` works on them. Don't
  modify that repo without the human.
- **Commits:** stage only your own paths (`git add <paths>`). The human
  sometimes commits concurrently.

## Blockers

None.

## Acceptance snapshot

As of 6d67a02 (M1 CPU/bus/timer/serial/MBC1, unsafe-free core). The
committed baseline is still the M0 one until M1 closes. The full manifest
takes about 4 s wall with 8 jobs.

Blargg cpu_instrs runs 31 emulated s in about 0.28 s on the host release
build, roughly 110× real time. On thumbv7em, `run` is 37 KB and
`execute_cb` 15.7 KB.

| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference |
|---|---|---|---|---|---|---|
| must (86) | 42 | 42 | 2 | 0 | 0 | 0 |
| should (40) | 2 | 30 | 8 | 0 | 0 | 0 |
| stretch (3268) | 1 | 107 | 2 | 10 | 3144 | 4 |
