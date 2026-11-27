//! Save images (Stage 2 M5, decision D6): checking a `.sav` sent by the
//! PC for the loaded ROM, putting it into the core, and taking the current
//! save out of the core in the same format.
//!
//! # Format
//!
//! The desktop front end's `.sav` (ADR 0023 and its HuC3 amendment, code
//! in `crates/gbc-desktop/src/save.rs`), so files move between the two:
//!
//! - the cartridge RAM, exactly `CartInfo::ram_size` bytes (MBC2: the
//!   core's one byte per cell form), then
//! - MBC3 with RTC: a 48-byte footer, ten u32 LE (live S, M, H, DL, DH,
//!   then the latched S, M, H, DL, DH; DH = day bit 8 | halt << 6 | day
//!   carry << 7) and a u64 LE unix time. The 44-byte VBA form (u32 time)
//!   is accepted too;
//! - HuC3: a 17-byte footer, a u64 LE unix time (of the clock's last
//!   minute tick), then u16 LE minute of the day, day, alarm minute and
//!   alarm day, then one alarm-enabled byte.
//!
//! # Checks (stricter than the desktop's tolerant load)
//!
//! [`check`] accepts exactly these lengths, for a cartridge with a save
//! (`CartInfo::has_save`): RAM alone (a clock cart then keeps the clock it
//! starts with), or RAM plus the cartridge's footer (48 or 44 for MBC3,
//! 17 for HuC3). Anything else is [`SaveError::WrongSize`]; the desktop
//! pads, cuts or ignores instead. A footer field with bits beyond its
//! register width (MBC3 S/M over 63, H over 31, DL over 255, DH bits other
//! than 0, 6, 7; HuC3 minutes over 0xFFF, alarm-enabled over 1) is
//! [`SaveError::BadFooter`], where the desktop masks. Nothing is written
//! until the whole image has passed, so a refused save is never half
//! applied.
//!
//! # Use
//!
//! The core takes cartridge RAM as a buffer lent to `Emulator::load`, so a
//! save goes in in two steps: [`Image::write_ram`] into the buffer before
//! `load`, [`Image::restore_clock`] after it. Between frames of a running
//! game, [`reload`] does both around an eject and a load (a power cycle,
//! as if the cartridge were reinserted with that save). [`take`] writes
//! the current save into a buffer of [`Layout::len`] bytes.
//!
//! The firmware has no wall clock (determinism: the core never reads one
//! either), so [`take`] stamps the footer with the `unix_time` it is
//! given, and [`Image::restore_clock`] does not advance the clock by the
//! time since the stamp. Passing the loaded image's [`Image::unix_time`]
//! back makes load → take byte-identical for the canonical lengths (RAM,
//! RAM + 48, RAM + 17); a 44-byte footer, or a clock cart's RAM-only
//! image, comes back as RAM + 48 / RAM + 17. Syncing to real time is the
//! PC's job (`tools/gbc-pc` knows the time, ADR 0040): it sends
//! [`Image::encode_synced`] (the clock advanced by the wall time since
//! the stamp, as the desktop does on load, [`Clock::sync`]) and restamps
//! dumps with [`Image::encode_restamped`]. Both take `now` as a
//! parameter; nothing here reads a clock.

use gbc_core::{
    CLOCK_HZ, CartInfo, Config, Emulator, Huc3RtcState, LoadErrorKind, RtcState, RtcTime, cart_info,
};

/// The MBC3 RTC footer (BGB, VBA-M, SameBoy).
pub const MBC3_FOOTER_LEN: usize = 48;
/// The older VBA MBC3 footer (u32 time), accepted by [`check`].
pub const VBA_FOOTER_LEN: usize = 44;
/// The HuC3 clock footer (SameBoy, BGB).
pub const HUC3_FOOTER_LEN: usize = 17;

/// Which clock a cartridge has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockKind {
    Mbc3,
    Huc3,
}

/// A cartridge clock's state, as stored in a footer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Mbc3(RtcState),
    Huc3(Huc3RtcState),
}

impl Clock {
    /// The clock as it should be at unix time `now`, given it was stored
    /// at `saved_unix`: advanced by `now - saved_unix` seconds by the
    /// core's `advance_seconds` (a halted MBC3 clock stays put; the day
    /// counter wraps and sets the carry flag as the hardware would). A
    /// stored time in the future counts as no time passed, and so does a
    /// stamp of 0: that means "unknown" (the board has no wall clock, so a
    /// game started without a loaded save dumps a footer stamped 0), not
    /// 1970, and syncing from it would add ~20,000 days. This is the
    /// desktop front end's sync on load (ADR 0023), shared with
    /// `tools/gbc-pc` (ADR 0040).
    #[must_use]
    pub fn sync(self, saved_unix: u64, now: u64) -> Self {
        // `advance_seconds(0)` rather than returning `self`: it still
        // normalises (masks) a hostile footer's fields.
        let elapsed = if saved_unix == 0 {
            0
        } else {
            now.saturating_sub(saved_unix)
        };
        match self {
            Self::Mbc3(s) => Self::Mbc3(s.advance_seconds(elapsed)),
            Self::Huc3(s) => Self::Huc3(s.advance_seconds(elapsed)),
        }
    }
}

/// What a cartridge's save holds: its RAM size and clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub ram_size: usize,
    pub clock: Option<ClockKind>,
}

impl Layout {
    /// From the ROM header. [`SaveError::NoSave`] for a cartridge with
    /// nothing to persist (no battery, or nothing behind it).
    pub fn of_rom(rom: &[u8]) -> Result<Self, SaveError> {
        Self::of_info(cart_info(rom).map_err(SaveError::Header)?)
    }

    /// For the cartridge in `emu`.
    pub fn of_emulator(emu: &Emulator<'_>) -> Result<Self, SaveError> {
        Self::of_info(emu.cart_info().ok_or(SaveError::NoCartridge)?)
    }

    fn of_info(info: CartInfo) -> Result<Self, SaveError> {
        if !info.has_save() {
            return Err(SaveError::NoSave);
        }
        let clock = if info.rtc {
            Some(ClockKind::Mbc3)
        } else if info.huc3_rtc {
            Some(ClockKind::Huc3)
        } else {
            None
        };
        Ok(Self {
            ram_size: info.ram_size,
            clock,
        })
    }

    /// The footer [`take`] writes: 48 (MBC3), 17 (HuC3) or 0.
    pub const fn footer_len(&self) -> usize {
        match self.clock {
            Some(ClockKind::Mbc3) => MBC3_FOOTER_LEN,
            Some(ClockKind::Huc3) => HUC3_FOOTER_LEN,
            None => 0,
        }
    }

    /// The length of the image [`take`] writes.
    pub const fn len(&self) -> usize {
        self.ram_size + self.footer_len()
    }

    /// Never: a cartridge with a save has RAM or a clock.
    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Why a save was refused. Nothing was changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The ROM header is not one the core loads.
    Header(LoadErrorKind),
    /// No cartridge is inserted.
    NoCartridge,
    /// The cartridge has no save (no battery, or no RAM and no clock).
    NoSave,
    /// The image is `len` bytes; the cartridge takes `ram_size`, or
    /// `ram_size` plus its footer (`footer`: 48 for MBC3, also 44; 17 for
    /// HuC3; 0 for none).
    WrongSize {
        len: usize,
        ram_size: usize,
        footer: usize,
    },
    /// A footer field has bits beyond its register width; `offset` is the
    /// field's first byte in the image.
    BadFooter { offset: usize },
    /// The output or RAM buffer has `have` bytes, `need` are needed.
    BufferTooSmall { need: usize, have: usize },
    /// [`reload`]: the image is for another cartridge layout than the one
    /// inserted.
    Mismatch,
}

/// A checked save image: borrowed RAM and the decoded clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Image<'s> {
    layout: Layout,
    ram: &'s [u8],
    clock: Option<Clock>,
    unix_time: u64,
}

impl<'s> Image<'s> {
    pub const fn layout(&self) -> Layout {
        self.layout
    }

    /// The cartridge RAM, exactly `layout().ram_size` bytes.
    pub const fn ram(&self) -> &'s [u8] {
        self.ram
    }

    /// The footer's clock, if the image had one.
    pub const fn clock(&self) -> Option<Clock> {
        self.clock
    }

    /// The footer's timestamp (0 without a footer).
    pub const fn unix_time(&self) -> u64 {
        self.unix_time
    }

    /// Copies the RAM into the buffer for `Emulator::load` (its first
    /// `ram_size` bytes; the rest is untouched). Refused, writing nothing,
    /// if `buf` is shorter.
    pub fn write_ram(&self, buf: &mut [u8]) -> Result<(), SaveError> {
        let need = self.ram.len();
        let have = buf.len();
        let dst = buf
            .get_mut(..need)
            .ok_or(SaveError::BufferTooSmall { need, have })?;
        dst.copy_from_slice(self.ram);
        Ok(())
    }

    /// This image re-encoded for a load at unix time `now`: the RAM as
    /// is, the clock [`Clock::sync`]ed from the stamp to `now`, stamped
    /// `now` (HuC3: minus the seconds into the minute, as [`take`]). A
    /// 44-byte footer comes out as 48 bytes; an image without a footer
    /// comes out unchanged. Returns the length written to `out`.
    pub fn encode_synced(&self, now: u64, out: &mut [u8]) -> Result<usize, SaveError> {
        let clock = self.clock.map(|c| c.sync(self.unix_time, now));
        encode(self.ram, clock, now, out)
    }

    /// This image with its footer stamped `now` and the clock as stored
    /// (for a dump: the registers were current at `now`). The RAM is
    /// unchanged; an image without a footer comes out unchanged.
    pub fn encode_restamped(&self, now: u64, out: &mut [u8]) -> Result<usize, SaveError> {
        encode(self.ram, self.clock, now, out)
    }

    /// Hands the clock to the core, after `Emulator::load` (which starts
    /// the clock from zero). Not advanced by real time (module docs).
    /// Without a clock in the image the core's clock is left alone.
    pub fn restore_clock(&self, emu: &mut Emulator<'_>) {
        match self.clock {
            Some(Clock::Mbc3(s)) => emu.set_rtc_state(s),
            Some(Clock::Huc3(s)) => emu.set_huc3_rtc_state(s),
            None => {}
        }
    }
}

/// Checks `bytes` as a save for a cartridge with `layout` (module docs,
/// checks).
pub fn check(layout: Layout, bytes: &[u8]) -> Result<Image<'_>, SaveError> {
    let ram_size = layout.ram_size;
    let wrong = SaveError::WrongSize {
        len: bytes.len(),
        ram_size,
        footer: layout.footer_len(),
    };
    let (ram, footer) = bytes.split_at_checked(ram_size).ok_or(wrong)?;
    let (clock, unix_time) = match (layout.clock, footer.len()) {
        (_, 0) => (None, 0),
        (Some(ClockKind::Mbc3), MBC3_FOOTER_LEN | VBA_FOOTER_LEN) => {
            let field = |i: usize| u32_at(footer, 4 * i);
            for (i, max) in [0x3F, 0x3F, 0x1F, 0xFF, 0xC1]
                .into_iter()
                .cycle()
                .take(10)
                .enumerate()
            {
                if field(i) & !max != 0 {
                    return Err(SaveError::BadFooter {
                        offset: ram_size + 4 * i,
                    });
                }
            }
            let unix = if footer.len() == MBC3_FOOTER_LEN {
                u64::from(u32_at(footer, 40)) | u64::from(u32_at(footer, 44)) << 32
            } else {
                u64::from(u32_at(footer, 40))
            };
            let state = RtcState {
                live: rtc_time(field(0), field(1), field(2), field(3), field(4)),
                latched: rtc_time(field(5), field(6), field(7), field(8), field(9)),
                subsecond_clocks: 0,
            };
            (Some(Clock::Mbc3(state)), unix)
        }
        (Some(ClockKind::Huc3), HUC3_FOOTER_LEN) => {
            let w = |i: usize| u16::from_le_bytes([footer[8 + 2 * i], footer[9 + 2 * i]]);
            for (i, max) in [(0, 0xFFF), (2, 0xFFF)] {
                if w(i) > max {
                    return Err(SaveError::BadFooter {
                        offset: ram_size + 8 + 2 * i,
                    });
                }
            }
            if footer[16] > 1 {
                return Err(SaveError::BadFooter {
                    offset: ram_size + 16,
                });
            }
            let mut c = Huc3RtcState::default();
            c.minutes = w(0);
            c.days = w(1);
            c.alarm_minutes = w(2);
            c.alarm_days = w(3);
            c.alarm_enabled = footer[16] == 1;
            let unix = u64::from(u32_at(footer, 0)) | u64::from(u32_at(footer, 4)) << 32;
            (Some(Clock::Huc3(c)), unix)
        }
        _ => return Err(wrong),
    };
    Ok(Image {
        layout,
        ram,
        clock,
        unix_time,
    })
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// Fields already checked against their widths.
fn rtc_time(s: u32, m: u32, h: u32, dl: u32, dh: u32) -> RtcTime {
    RtcTime {
        seconds: s as u8,
        minutes: m as u8,
        hours: h as u8,
        days: (dl | (dh & 1) << 8) as u16,
        halted: dh & 0x40 != 0,
        day_carry: dh & 0x80 != 0,
    }
}

fn put_time(out: &mut [u8], t: &RtcTime) {
    let dh = u32::from((t.days >> 8) & 1) | u32::from(t.halted) << 6 | u32::from(t.day_carry) << 7;
    let fields = [
        u32::from(t.seconds),
        u32::from(t.minutes),
        u32::from(t.hours),
        u32::from(t.days & 0xFF),
        dh,
    ];
    for (o, v) in out.as_chunks_mut::<4>().0.iter_mut().zip(fields) {
        o.copy_from_slice(&v.to_le_bytes());
    }
}

/// Writes the inserted cartridge's save (RAM, then its footer stamped
/// `unix_time`) to the start of `out` and returns its length
/// ([`Layout::len`]). The format and its losses are the desktop's: MBC3
/// drops the RTC's part second; HuC3 stamps `unix_time` minus the seconds
/// into the current minute. Refused, writing nothing, if `out` is short.
pub fn take(emu: &Emulator<'_>, unix_time: u64, out: &mut [u8]) -> Result<usize, SaveError> {
    let info = emu.cart_info().ok_or(SaveError::NoCartridge)?;
    if !info.has_save() {
        return Err(SaveError::NoSave);
    }
    let clock = emu
        .rtc_state()
        .map(Clock::Mbc3)
        .or_else(|| emu.huc3_rtc_state().map(Clock::Huc3));
    encode(emu.cart_ram(), clock, unix_time, out)
}

/// Writes `ram`, then `clock`'s footer (if any) stamped `unix_time`, to
/// the start of `out`; returns the length. The encoding [`take`]
/// documents. Refused, writing nothing, if `out` is short.
pub fn encode(
    ram: &[u8],
    clock: Option<Clock>,
    unix_time: u64,
    out: &mut [u8],
) -> Result<usize, SaveError> {
    let footer = match clock {
        Some(Clock::Mbc3(_)) => MBC3_FOOTER_LEN,
        Some(Clock::Huc3(_)) => HUC3_FOOTER_LEN,
        None => 0,
    };
    let need = ram.len() + footer;
    let have = out.len();
    let out = out
        .get_mut(..need)
        .ok_or(SaveError::BufferTooSmall { need, have })?;
    let (o_ram, o_footer) = out.split_at_mut(ram.len());
    o_ram.copy_from_slice(ram);
    match clock {
        Some(Clock::Mbc3(r)) => {
            put_time(&mut o_footer[..20], &r.live);
            put_time(&mut o_footer[20..40], &r.latched);
            o_footer[40..48].copy_from_slice(&unix_time.to_le_bytes());
        }
        Some(Clock::Huc3(c)) => {
            let into_minute = u64::from(c.subminute_clocks / CLOCK_HZ);
            o_footer[..8].copy_from_slice(&unix_time.saturating_sub(into_minute).to_le_bytes());
            for (o, v) in o_footer[8..16].as_chunks_mut::<2>().0.iter_mut().zip([
                c.minutes,
                c.days,
                c.alarm_minutes,
                c.alarm_days,
            ]) {
                o.copy_from_slice(&v.to_le_bytes());
            }
            o_footer[16] = u8::from(c.alarm_enabled);
        }
        None => {}
    }
    Ok(need)
}

/// Puts `image` into the running emulator between frames: ejects the
/// cartridge, writes the RAM into its buffer, loads it again with `config`
/// (a power cycle: clock 0, held buttons and stop events cleared, as
/// `Emulator::eject` documents) and restores the clock. Refused, changing
/// nothing, unless `image` was checked for the inserted cartridge's
/// layout.
pub fn reload(emu: &mut Emulator<'_>, image: &Image<'_>, config: Config) -> Result<(), SaveError> {
    if Layout::of_emulator(emu)? != image.layout {
        return Err(SaveError::Mismatch);
    }
    let Some(cart) = emu.eject() else {
        return Err(SaveError::NoCartridge);
    };
    // The buffer held this cartridge before (`load` needs at least
    // `ram_size` bytes), so this cannot be short; if it were, the RAM is
    // left as it was and the cartridge goes back in unchanged.
    let wrote = image.write_ram(cart.cart_ram);
    // The same ROM and buffer `load` accepted before, so this cannot fail;
    // if it did, the emulator is left empty and the error says why.
    if let Err(e) = emu.load(cart.rom, cart.cart_ram, config) {
        return Err(SaveError::Header(e.kind));
    }
    match wrote {
        Ok(()) => image.restore_clock(emu),
        Err(_) => {
            if let Some(s) = cart.rtc {
                emu.set_rtc_state(s);
            }
            if let Some(s) = cart.huc3_rtc {
                emu.set_huc3_rtc_state(s);
            }
        }
    }
    wrote
}
