//! Board-independent firmware (ADR 0036): `no_std`, no `alloc`, no
//! `unsafe`, and no dependency besides `gbc-core`. A board crate (Stage 2:
//! `gbc-nucleo-h753`) does everything with registers or `unsafe` (clocks,
//! UART, DWT, the static buffers in their linker sections) and calls in
//! here; the host counterpart (`tools/headless`) calls the same code, so the
//! two print the same lines.
//!
//! What is here now (M1-M4):
//! - [`CycleClock`], the one board trait M1 needs (ADR 0036, "Board
//!   boundary"), and [`paced::Idle`] (M4). Output goes to any
//!   [`core::fmt::Write`].
//! - [`input`]: scripted button input, a `&[Press]` table and a no-alloc
//!   parser for the text format of `scripts/inputs/*.txt`.
//! - [`audio`]: DC blocker, 12-bit DAC conversion, the sample ring and the
//!   DAC-feed hash (M4).
//! - [`fnv`]: 64-bit FNV-1a, the hash on every output line.
//! - [`headless`]: the headless frame loop and its line format.
//! - [`paced`]: the audio-paced main loop (M4): runs the core while the
//!   sample ring has room, idles (`WFI`) otherwise; its line format.
//! - [`rom_image`]: the QSPI NOR ROM image header (ADR 0037).
//! - [`video`]: the RGB555 to RGB565 1.5x scaler (240x216) for the panel (M3).
//!
//! Button traits arrive with their drivers (M5).

#![no_std]
#![forbid(unsafe_code)]

pub mod audio;
pub mod fnv;
pub mod headless;
pub mod input;
pub mod paced;
pub mod rom_image;
pub mod video;

/// A free-running cycle counter (on the MCU, the DWT's `CYCCNT`).
///
/// Only the statistics lines use it ([`headless`]): they differ between
/// the host and the board and are never compared. Nothing that is compared
/// depends on it.
pub trait CycleClock {
    /// The counter now. It wraps at `u32::MAX`; intervals are taken with
    /// `wrapping_sub`, so any interval shorter than one wrap is exact.
    fn now(&self) -> u32;
    /// Counts per second (the CPU clock on the MCU). 0 means unknown: the
    /// statistics then give raw counts only.
    fn hz(&self) -> u32;
}
