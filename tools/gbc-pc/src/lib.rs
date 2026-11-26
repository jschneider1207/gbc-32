//! `gbc-pc`: the PC companion for the dev board (plan
//! `docs/plan/stage-2.md`, "Crate layout" and M2 Loader). It speaks the
//! `gbc-link` protocol over the ST-LINK virtual COM port.
//!
//! The portable part (the protocol client, the ROM-load flow and a
//! simulated board for tests) builds and tests on any host. Only the COM
//! port itself, [`serial`], is Windows-specific: the tool ships as a
//! native Windows `.exe` because WSL has poor COM-port access.

pub mod client;
pub mod fnv;
pub mod sim;
pub mod transport;

#[cfg(windows)]
pub mod serial;

pub use client::{BoardInfo, Client, Config, Error, Failure, LoadOutcome, Reply, Stats};
pub use transport::Transport;
