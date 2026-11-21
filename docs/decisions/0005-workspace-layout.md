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
