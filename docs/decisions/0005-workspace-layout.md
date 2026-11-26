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
host-tested `no_std` crate, `crates/gbc-firmware`. All Cortex-M7 parts share
the `thumbv7em-none-eabihf` target; the PAC, memory map and peripherals
differ per chip, hence the per-board split. See `docs/plan/stage-2.md`.

Crates (human-approved 2026-11-25), each with one role:
- `gbc-firmware`: board-independent firmware (main loop and pacing, video
  conversion, audio conditioning, input, stats, ROM image header). `no_std`,
  `forbid(unsafe_code)`, host-tested, depends on `gbc-core` only.
- `gbc-link`: the serial protocol (framing and checksums for ROM load, save
  load/dump, PC input, stats). `no_std`, no dependencies, no unsafe,
  transport-agnostic. Stage 3 may reuse it over USB serial or skip it.
- `tools/gbc-pc`: the PC companion, a native Windows `.exe` that speaks
  `gbc-link` over a COM port.
- `gbc-nucleo-h753` (lib + bin) depends on `gbc-firmware`, `gbc-core` and
  `gbc-link`; `gbc-handheld` uses `gbc-link` only if Stage 3 picks USB serial.

Rule: `gbc-firmware` does not depend on `gbc-link`. The board crate wires
the two together.

Build details, the board boundary and when each crate is created: ADR 0036.
