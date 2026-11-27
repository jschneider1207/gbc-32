//! The board side of the PC <-> board link (protocol in `gbc-link`, the
//! board's duties in `docs/reference/link-protocol.md`), with no board in
//! it (ADR 0043): the servers the firmware images run, behind small
//! traits, so the board crate (thumbv7em) and the host tests
//! (`tools/gbc-pc`) build the same code.
//!
//! - [`loader`]: the ROM loader (plan Stage 2 M2 "Loader", ADR 0038),
//!   behind the [`loader::Nor`] trait.
//! - [`link_session`]: the player's link session (plan Stage 2 M5): live
//!   input, save load and dump, audio on/off, behind [`link_session::Game`].
//! - [`responder`]: what both share: frame capture, resend detection by
//!   the raw bytes, the stored reply and the bad-frame `Nak`.
//!
//! The UART is always the caller's: it feeds received bytes in and sends
//! the bytes it gets back. `no_std`, no `alloc`, no `unsafe`
//! (`tests/hard_rules.rs`).
#![no_std]
#![forbid(unsafe_code)]

pub mod link_session;
pub mod loader;
pub mod responder;
