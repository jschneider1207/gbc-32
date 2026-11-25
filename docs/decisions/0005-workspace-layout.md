# 0005: Cargo workspace layout

- Status: Accepted
- Date: 2026-11-21

## Context
We need separate build targets: a `no_std` core, a std desktop front end,
std tooling, and later an embedded binary.

## Decision
One Cargo workspace: `crates/gbc-core`, `crates/gbc-desktop`,
`tools/romtest`, later `crates/gbc-stm32`. Layout and boundaries are
described in `docs/architecture.md`.

## Consequences
`cargo test --workspace` covers everything that builds on the host. The
embedded crate (Stage 2) will need its own target configuration and may be
excluded from host workspace builds.

## Amendment (human, 2026-11-25)
No generic `crates/gbc-stm32`. Firmware crates are per board:
`crates/gbc-nucleo-h753` (Stage 2) and later a Stage 3 board crate
(provisionally `gbc-handheld`). Chip-independent front-end logic goes in a
host-tested `no_std` crate, `crates/gbc-platform`. All Cortex-M7 parts share
the `thumbv7em-none-eabihf` target; the PAC, memory map and peripherals
differ per chip, hence the per-board split. See `docs/plan/stage-2.md`.
