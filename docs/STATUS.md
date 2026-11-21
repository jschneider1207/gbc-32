# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M0 (toolchain, workspace, runner): implementation done, closing
- **Last updated:** 2026-11-21 (session 01: workspace, core API, romtest)

## Next up

1. M0 close: triage the romtest pass/fail review (required before trusting
   results, AGENTS.md "Independent review") and land the core API review fixes
   (eject/reload, RAM trim, `set_dmg_palettes`, no `Default`, unknown RAM code).
2. M0 close: run `./scripts/gate-stage1.sh` (expect exit 1 at the ROM step),
   create the initial baseline with `--update-baseline`, tick the M0 boxes,
   tag `stage1-m0`.
3. M1: CPU + bus. Note the new pre-bus task (per-call context struct) in the plan.

## Needs human

None.

## Environment

- Rust via **rustup** (user-level, `~/.rustup` and `~/.cargo`), not asdf.
  `cargo` is **not on PATH in non-interactive shells**: prefix commands with
  `source ~/.cargo/env &&`. Toolchain pinned to 1.100.0 in `rust-toolchain.toml`
  (rustup installed a `1.100.0-x86_64-unknown-linux-gnu` toolchain alongside
  `stable`, same version, user-level).
- Desktop system libraries installed 2026-11-21 (`./scripts/install-system-deps.sh --check` passes):
  ALSA 1.2.11, libudev 255, wayland-client 1.22.0, xkbcommon 1.6.0, x11 1.8.7.
  `ldconfig` warnings about `/usr/lib/wsl/lib/libnvoptix*` are harmless WSL noise.
- Host C compiler: gcc 13.3 (`cc`). The machine is WSL2 Ubuntu 24.04. No `jq`.
- `python3` through asdf has no version set. Use `/usr/bin/python3` (3.12) for one-off scripts.
- Commit only your own paths (`git add <paths>`): the human sometimes commits
  concurrently, and `commit -a` once swept in half-written files (d2b9658).

## Blockers

None.

## Acceptance snapshot

Stub core (no CPU): runner works end to end, everything fails as expected.
Full manifest: 3394 group/ROM runs, ~3.2 s wall time with 8 jobs.

| Tier | Pass | Fail | Timeout | Unsupported | No reference |
|---|---|---|---|---|---|
| must (86) | 0 | 10 | 76 | 0 | 0 |
| should (40) | 0 | 1 | 39 | 0 | 0 |
| stretch (3268) | 0 | 0 | 109 | 3154 | 5 |

No baseline committed yet (pending the runner review).
