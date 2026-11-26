//! `gbc-pc`: the PC companion for the dev board (plan
//! `docs/plan/stage-2.md`, "Crate layout", M2 Loader and M5). It speaks
//! the `gbc-link` protocol over the ST-LINK virtual COM port: ROM loads,
//! save load and dump, and live input (`docs/reference/link-protocol.md`).
//!
//! The portable part (the protocol client, the ROM and save flows, the
//! key mapping and the `play` loop, and a simulated board for tests)
//! builds and tests on any host. Only the COM port, [`serial`], the
//! keyboard, [`keyboard`], and the audio device, [`waveout`], are
//! Windows-specific: the tool ships as a
//! native Windows `.exe` because WSL has poor COM-port access.

pub mod audio;
pub mod client;
pub mod fnv;
pub mod input;
pub mod live;
pub mod play;
pub mod rtc;
pub mod save;
pub mod sim;
pub mod transport;

#[cfg(windows)]
pub mod keyboard;
#[cfg(windows)]
pub mod serial;
#[cfg(windows)]
pub mod waveout;

pub use client::{
    BoardInfo, Client, Config, Error, Failure, Incoming, LoadOutcome, Received, Reply, Stats,
};
pub use transport::Transport;
