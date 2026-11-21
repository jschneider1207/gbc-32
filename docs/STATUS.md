# Status

> Read this first every session. Rewrite it (don't append) at the end of
> each session so a fresh session can pick up from here.

- **Stage:** 1 (portable core + desktop front end), see `docs/plan/stage-1.md`
- **Milestone:** M0 (toolchain, workspace, runner), not started
- **Last updated:** 2026-11-21 (project infrastructure set up)

## Next up

1. M0: create the workspace per `docs/architecture.md` and pin the toolchain in
   `rust-toolchain.toml` (rustup is installed, see below)
2. M0: `gbc-core` skeleton building for host and `thumbv7em-none-eabihf`
3. M0: `romtest` per `acceptance/README.md`, then commit the initial baseline

## Needs human

- **Before M7 (desktop front end), not urgent yet:** the window and audio
  libraries need system packages that need `sudo`:
  `sudo apt install libasound2-dev libudev-dev libwayland-dev libxkbcommon-dev`
  (missing as of 2026-11-21; `libx11-dev` and `pkg-config` are present).
  Until then, keep `gbc-desktop` a dependency-free stub so the workspace
  builds without them.

## Environment

- Rust via **rustup** (user-level, `~/.rustup` and `~/.cargo`), not asdf:
  rust 1.100.0 stable, components clippy and rustfmt, target
  `thumbv7em-none-eabihf`. Pin the exact version in `rust-toolchain.toml`.
- Host C compiler: gcc 13.3 (`cc`). The machine is WSL2 Ubuntu 24.04.
- `python3` through asdf has no version set. Use `/usr/bin/python3` (3.12) for one-off scripts.

## Blockers

None.

## Acceptance snapshot

No runner yet. Once M0 is done, this section holds the latest
must/should/stretch scores from `target/acceptance/stage1.md`.
