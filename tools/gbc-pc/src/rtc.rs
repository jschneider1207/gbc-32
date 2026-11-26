//! Real-time sync of save clocks for the board (ADR 0040, M5 review T-2).
//!
//! The board has no wall clock (determinism, and the firmware follows the
//! core), so it neither advances a loaded clock by the time since the
//! save's stamp nor knows the time when it dumps: it stamps a dump with
//! the stamp of the save it loaded. The desktop front end syncs on load
//! (ADR 0023: live registers += now - stamp). Without the PC doing the
//! same, a save moved desktop -> board -> desktop counts the board's play
//! time twice (the board's emulated clock ran it, then the desktop adds
//! now - old stamp again) and the board loses the wall time between the
//! stamp and the board load. So, PC side:
//!
//! - `load --sav`: [`for_load`] syncs the clock to now with the desktop's
//!   rule (`gbc_firmware::save::Clock::sync`, which the desktop calls
//!   too) and stamps the image now, so the board starts from real time;
//! - `dump` and `play`'s dumps: [`from_dump`] restamps the footer with now
//!   (the board's clock has run in emulated real time since the load, so
//!   its registers are the time at the dump). RAM bytes are never touched;
//! - `--raw-save`: neither (`now` = `None`), byte-exact transport.
//!
//! Known losses, as in the format: MBC3 drops the RTC's part second;
//! HuC3 (stamped at its last minute tick) loses the seconds into the
//! current minute on each board load and each dump, since the board
//! restarts the minute and cannot report it. The board clock is only as
//! true as its emulation speed is real time.

use std::time::{SystemTime, UNIX_EPOCH};

use gbc_firmware::save::{self, ClockKind, Layout, SaveError};

/// Reads the host's wall clock: whole seconds since the unix epoch (0
/// for a clock set before 1970).
pub type WallClock = fn() -> u64;

/// The host's wall clock ([`WallClock`]).
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The save to send after `rom`: with `now`, checked exactly as the board
/// checks it (`gbc_firmware::save::check`; an error here is what the
/// board would refuse) and re-encoded with its clock synced to `now`
/// ([`save::Image::encode_synced`]); with `None` (`--raw-save`), `sav`
/// as given, unchecked.
pub fn for_load(rom: &[u8], sav: &[u8], now: Option<u64>) -> Result<Vec<u8>, SaveError> {
    let Some(now) = now else {
        return Ok(sav.to_vec());
    };
    let layout = Layout::of_rom(rom)?;
    let image = save::check(layout, sav)?;
    let mut out = vec![0; layout.len()];
    let n = image.encode_synced(now, &mut out)?;
    out.truncate(n);
    Ok(out)
}

/// The cartridge layout of a dump, from its length alone (`dump` has no
/// ROM): the board writes RAM + 48 (MBC3 clock), RAM + 17 (HuC3) or RAM
/// only, and every RAM size the core reports is 0 or a power of two of
/// at least 512 (`gbc_core::cart_ram_size`), so the length mod 512 tells
/// the footer. `None`: no clock footer.
fn dump_layout(len: usize) -> Option<Layout> {
    let clock = match len % 512 {
        save::MBC3_FOOTER_LEN => ClockKind::Mbc3,
        save::HUC3_FOOTER_LEN => ClockKind::Huc3,
        _ => return None,
    };
    Some(Layout {
        ram_size: len - len % 512,
        clock: Some(clock),
    })
}

/// A received dump as it should be written: with `now`, a clock footer
/// restamped `now` ([`save::Image::encode_restamped`]; the RAM and the
/// clock registers as dumped); a dump without a recognisable footer, or
/// with `None` (`--raw-save`), as received. The flag says whether it was
/// restamped.
pub fn from_dump(dump: Vec<u8>, now: Option<u64>) -> (Vec<u8>, bool) {
    let Some((now, layout)) = now.zip(dump_layout(dump.len())) else {
        return (dump, false);
    };
    let Ok(image) = save::check(layout, &dump) else {
        return (dump, false);
    };
    let mut out = vec![0; layout.len()];
    match image.encode_restamped(now, &mut out) {
        Ok(n) if n == dump.len() => {
            out.truncate(n);
            (out, true)
        }
        _ => (dump, false),
    }
}
