# 0043: The board's link servers get their own crate (`gbc-link-board`)

- Status: Accepted (human, 2026-11-27: "go ahead with all of them", on the
  Stage 2 comprehensive review's Q2)
- Date: 2026-11-27
- Amends: ADR 0036 (firmware crate layout; its human-approved crate list)
- Builds on: ADR 0038 (board ROM loader), ADR 0041 (audio to the PC),
  `docs/reference/link-protocol.md`; review `docs/reviews/stage2-comprehensive.md`
  rows Q2, Q11, Q16

## Context
The ROM loader server (`Loader`, behind a `Nor` trait) and the player's
link session (`Session`, behind a `Game` trait) were board-independent:
they used only `core`, `gbc_link` and `gbc_firmware`. Yet they lived in
`crates/gbc-nucleo-h753`, which is its own thumbv7em workspace, so the
only way to host-test them was to `#[path]`-include the two files into
`tools/gbc-pc/tests/{board,link_session}.rs` under `#[allow(dead_code)]`.
Their stop-and-wait half (frame capture, resend detection by the raw
bytes, the stored reply, the separate bad-frame `Nak`) was copied between
the two files, and a third copy lived in `tools/gbc-pc/src/sim.rs`. K1 (a
corrupt resend replacing the stored reply, fixed in a77fca6) existed in
one copy only.

## Decision
- A new root-workspace crate, **`crates/gbc-link-board`**: `#![no_std]`,
  no `alloc`, `#![forbid(unsafe_code)]`, normal dependencies on
  `gbc-link` and `gbc-firmware` only, no build script.
  `tests/hard_rules.rs` enforces this; it is a copy of
  `crates/gbc-link/tests/hard_rules.rs` with the dependency rule changed
  (the checkers can't share a file without `#[path]`, which they ban).
- It holds `loader` (`Loader`, `Nor`, the NOR erase units
  `SECTOR_BYTES`/`BLOCK_BYTES`), `link_session` (`Session`, `Game`) and
  `responder`: one `Responder` both servers use for the stop-and-wait
  half. The servers' behaviour, wire bytes, counters and their field
  names are unchanged; each server still counts `frames`, `bad_frames`,
  `duplicates` and `replies_dropped` itself, from what `Responder`
  returns. `MAX_CHUNK` and the reply buffer sizes (`REPLY_MAX`, with the
  Q14 const assertion) are defined once there and re-exported by both
  server modules.
- `gbc-nucleo-h753` depends on it by path and re-exports it
  (`pub use gbc_link_board::{link_session, loader}`), so the images name
  `gbc_nucleo_h753::loader::...` as before. Its QSPI driver re-exports the
  erase units instead of defining them again (Q11).
- `tools/gbc-pc` tests depend on it as a normal dev-dependency (no
  `#[path]`). `gbc_pc::sim` (the `sim` feature, test support) takes its
  frame capture and resend detection from `Responder` too; its message
  handling stays its own (see Consequences).
- ADR 0036 said each bin "supplies only `#[entry]` and its mode logic".
  That had drifted: the bins also hold their board glue (the `loader`
  image's `Nor` impl over the QSPI driver, the status lines, the
  player's `Game` impl and loop wiring). That is the rule from now on:
  board-independent logic goes in `gbc-firmware` or `gbc-link-board`;
  a bin holds `#[entry]`, its mode logic and the glue between those
  crates and the lib's drivers (Q16).

## Consequences
- The servers are host-tested as ordinary code, under the root
  workspace's `cargo clippy` and `cargo test`, and a fix to the
  resend logic reaches both servers and the simulated board at once.
  `scripts/gate-stage2.sh` H7 names the new crate's tests too.
- The session's state grows by one 32-byte buffer (the bad-frame `Nak`
  is now encoded into the responder's own buffer, as the loader's was,
  instead of a stack temporary).
- `gbc_pc::sim::SimBoard` still answered messages with its own code (one
  board that took both a ROM and saves, with knobs the real servers
  don't have: a fake protocol version and chunk size, any save length,
  dump faults, scripted `Stats` and audio). **Done since 54db549** (Q2
  rest): it runs the real `Loader` (over an in-memory NOR) and `Session`
  (over an in-memory `Game`), one image at a time as on the board, with
  the faults on the wire around them and the version and chunk size as
  labelled `HelloReply` rewrites; it has no message handling of its own.
- The new crate's hard-rules checker is a fourth copy; keep the copies in
  step (a shared checker crate would be a new dependency of `gbc-core`).
