# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M0 (toolchain, workspace, runner), not started
- **Last updated:** 2026-11-21 (project infrastructure set up)

## Next up

1. M0: install the Rust toolchain (blocked, see Needs human), then create the
   workspace per `docs/architecture.md`
2. M0: `gbc-core` skeleton building for host and `thumbv7em-none-eabihf`
3. M0: `romtest` per `acceptance/README.md`, then commit the initial baseline

## Needs human

- **Rust toolchain:** `cargo`/`rustc` are not installed. Either install
  rustup yourself (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`)
  or tell the agent it may install it (user-level, `~/.cargo`).
- **ADR 0003 (target CGB revision C) is Proposed.** Work proceeds on this
  assumption unless you override it.

## Blockers

None.

## Acceptance snapshot

No runner yet. Once M0 is done, this section holds the latest
must/should/stretch scores from `target/acceptance/stage1.md`.
