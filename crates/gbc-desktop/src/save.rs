//! Save files: cartridge RAM plus, for MBC3+RTC carts, the 48-byte
//! BGB/VBA-M/SameBoy RTC footer, or for HuC3 carts the 17-byte
//! SameBoy/BGB HuC3 clock footer (ADR 0023).
//!
//! Wall-clock time is read only here and in `main`, never in the core.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gbc_core::{CLOCK_HZ, CartInfo, EjectedCart, Emulator, Huc3RtcState, RtcState, RtcTime};

/// Size of the de-facto RTC footer: 10 x u32 LE and a u64 LE unix time.
pub const FOOTER_LEN: usize = 48;
/// The older VBA footer, with a u32 timestamp, accepted on load.
const FOOTER_LEN_32: usize = 44;
/// Size of the HuC3 clock footer, SameBoy's packed `GB_huc3_rtc_time_t`
/// (`Core/gb.h`; its `Core/save_state.c` notes that BGB uses the same
/// layout): a u64 LE unix time, then u16 LE minute of the day, day, alarm
/// minute and alarm day, then the alarm-enabled byte.
pub const HUC3_FOOTER_LEN: usize = 17;

/// Which clock a cartridge has, and so which footer its save file ends in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockKind {
    Mbc3,
    Huc3,
}

/// A cartridge clock's state: the core keeps the two kinds apart (ADR
/// 0032); the save code handles them as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    Mbc3(RtcState),
    Huc3(Huc3RtcState),
}

impl Clock {
    /// The inserted cartridge's clock right now, if it has one.
    pub fn of(emu: &Emulator<'_>) -> Option<Self> {
        emu.rtc_state()
            .map(Self::Mbc3)
            .or_else(|| emu.huc3_rtc_state().map(Self::Huc3))
    }

    /// The clock an ejected cartridge came out with.
    pub fn of_ejected(cart: &EjectedCart<'_>) -> Option<Self> {
        cart.rtc.map(Self::Mbc3).or(cart.huc3_rtc.map(Self::Huc3))
    }

    /// Hands the state to the emulator (after `Emulator::load`).
    pub fn restore(self, emu: &mut Emulator<'_>) {
        match self {
            Self::Mbc3(s) => emu.set_rtc_state(s),
            Self::Huc3(s) => emu.set_huc3_rtc_state(s),
        }
    }

    /// The state to restore after loading: the stored clock advanced by
    /// the wall-clock time since it was saved. `now` is `None` to skip the
    /// sync (tests, and anything that must not depend on the host clock);
    /// a stored time in the future counts as no time passed.
    pub fn sync(self, saved_unix: u64, now: Option<u64>) -> Self {
        let Some(now) = now else { return self };
        let elapsed = now.saturating_sub(saved_unix);
        match self {
            Self::Mbc3(s) => Self::Mbc3(s.advance_seconds(elapsed)),
            Self::Huc3(s) => Self::Huc3(s.advance_seconds(elapsed)),
        }
    }
}

/// The save path for a ROM: `<stem>.sav` in the ROM's directory, or
/// `<file name>.sav` (`game.gbc.sav`) when a sibling ROM with the same stem
/// and the other Game Boy extension exists, so `game.gb` and `game.gbc`
/// side by side don't share a save (ADR 0023 amendment).
pub fn save_path(rom: &Path) -> PathBuf {
    let own = rom.extension();
    let sibling = ["gb", "gbc", "GB", "GBC"]
        .iter()
        .any(|e| Some(std::ffi::OsStr::new(e)) != own && rom.with_extension(e).is_file());
    if sibling {
        let mut name = rom.as_os_str().to_owned();
        name.push(".sav");
        PathBuf::from(name)
    } else {
        rom.with_extension("sav")
    }
}

fn put_time(out: &mut Vec<u8>, t: &RtcTime) {
    let dh = ((t.days >> 8) & 1) as u32 | u32::from(t.halted) << 6 | u32::from(t.day_carry) << 7;
    for v in [
        u32::from(t.seconds),
        u32::from(t.minutes),
        u32::from(t.hours),
        u32::from(t.days & 0xFF),
        dh,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
}

fn get_time(b: &[u8]) -> RtcTime {
    let w = |i: usize| u32::from_le_bytes(b[i * 4..i * 4 + 4].try_into().unwrap());
    RtcTime {
        // Low bits only: other emulators may leave junk in the upper bytes.
        seconds: (w(0) & 0x3F) as u8,
        minutes: (w(1) & 0x3F) as u8,
        hours: (w(2) & 0x1F) as u8,
        days: ((w(3) & 0xFF) | (w(4) & 1) << 8) as u16,
        halted: w(4) & 0x40 != 0,
        day_carry: w(4) & 0x80 != 0,
    }
}

/// Encodes a save file: the RAM, then (clock carts) the footer, stamped
/// `unix`.
///
/// MBC3: the RTC's sub-second count is not stored (the format has no
/// field for it; loading restarts the second, an error under one second
/// per save).
///
/// HuC3: the timestamp is the time of the clock's last minute tick
/// (`unix` minus the seconds into the current minute), which is what
/// SameBoy's `last_rtc_second` holds, so the seconds into the minute
/// survive a save and a synced load; the part second is lost.
pub fn encode(ram: &[u8], clock: Option<(&Clock, u64)>) -> Vec<u8> {
    let mut out = Vec::with_capacity(ram.len() + FOOTER_LEN);
    out.extend_from_slice(ram);
    match clock {
        Some((Clock::Mbc3(rtc), unix)) => {
            put_time(&mut out, &rtc.live);
            put_time(&mut out, &rtc.latched);
            out.extend_from_slice(&unix.to_le_bytes());
        }
        Some((Clock::Huc3(c), unix)) => {
            let into_minute = u64::from(c.subminute_clocks / CLOCK_HZ);
            out.extend_from_slice(&unix.saturating_sub(into_minute).to_le_bytes());
            for v in [c.minutes, c.days, c.alarm_minutes, c.alarm_days] {
                out.extend_from_slice(&v.to_le_bytes());
            }
            out.push(u8::from(c.alarm_enabled));
        }
        None => {}
    }
    out
}

/// The HuC3 footer's clock and its timestamp. The fields go to the core
/// as stored: `Emulator::set_huc3_rtc_state` and `advance_seconds` mask
/// them (ADR 0032).
fn get_huc3(b: &[u8]) -> (Huc3RtcState, u64) {
    let w = |i: usize| u16::from_le_bytes([b[8 + 2 * i], b[9 + 2 * i]]);
    let mut c = Huc3RtcState::default();
    c.minutes = w(0);
    c.days = w(1);
    c.alarm_minutes = w(2);
    c.alarm_days = w(3);
    c.alarm_enabled = b[16] & 1 != 0;
    (c, u64::from_le_bytes(b[..8].try_into().unwrap()))
}

/// What a save file held.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    /// Exactly `ram_size` bytes: the file's RAM, zero-padded if short.
    pub ram: Vec<u8>,
    /// The stored clock and the unix time it was stored at.
    pub rtc: Option<(Clock, u64)>,
    pub warnings: Vec<String>,
}

/// Decodes a save file for a cartridge with `ram_size` bytes of RAM.
/// Tolerant: a short file fills what it has, a long one is cut (both warn).
pub fn decode(bytes: &[u8], ram_size: usize, clock: Option<ClockKind>) -> Decoded {
    let mut warnings = Vec::new();
    let mut rtc = None;
    let ram_end = bytes.len().min(ram_size);
    let has_rtc = clock.is_some();
    if clock == Some(ClockKind::Huc3) {
        if bytes.len() == ram_size + HUC3_FOOTER_LEN {
            let (state, unix) = get_huc3(&bytes[ram_size..]);
            rtc = Some((Clock::Huc3(state), unix));
        } else if bytes.len() > ram_size {
            warnings.push(format!(
                "save file is {} bytes, expected {} (RAM) + {HUC3_FOOTER_LEN} (HuC3 clock): clock not restored",
                bytes.len(),
                ram_size
            ));
        }
    } else if has_rtc {
        let footer = [FOOTER_LEN, FOOTER_LEN_32]
            .into_iter()
            .find(|f| bytes.len() == ram_size + f);
        if let Some(f) = footer {
            let b = &bytes[ram_size..];
            let unix = if f == FOOTER_LEN {
                u64::from_le_bytes(b[40..48].try_into().unwrap())
            } else {
                u64::from(u32::from_le_bytes(b[40..44].try_into().unwrap()))
            };
            let state = RtcState {
                live: get_time(&b[..20]),
                latched: get_time(&b[20..40]),
                subsecond_clocks: 0,
            };
            rtc = Some((Clock::Mbc3(state), unix));
        } else if bytes.len() > ram_size {
            warnings.push(format!(
                "save file is {} bytes, expected {} (RAM) + {FOOTER_LEN} (RTC): clock not restored",
                bytes.len(),
                ram_size
            ));
        }
    }
    if rtc.is_none() && bytes.len() < ram_size {
        warnings.push(format!(
            "save file is {} bytes, expected {}: the rest starts zeroed",
            bytes.len(),
            ram_size
        ));
    } else if !has_rtc && bytes.len() > ram_size {
        warnings.push(format!(
            "save file is {} bytes, expected {}: extra bytes ignored",
            bytes.len(),
            ram_size
        ));
    }
    let mut ram = vec![0u8; ram_size];
    ram[..ram_end].copy_from_slice(&bytes[..ram_end]);
    Decoded { ram, rtc, warnings }
}

/// Writes `data` to `path` atomically: a temp file in the same directory,
/// synced, then renamed over the target.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let result = (|| {
        let f = fs::File::create(&tmp)?;
        io::Write::write_all(&mut &f, data)?;
        f.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// A cartridge's save file on disk.
pub struct SaveFile {
    pub path: PathBuf,
    pub ram_size: usize,
    pub clock: Option<ClockKind>,
}

impl SaveFile {
    /// `None` for cartridges with nothing to persist.
    pub fn for_rom(rom: &Path, info: &CartInfo) -> Option<Self> {
        info.has_save().then(|| Self {
            path: save_path(rom),
            ram_size: info.ram_size,
            clock: if info.rtc {
                Some(ClockKind::Mbc3)
            } else if info.huc3_rtc {
                Some(ClockKind::Huc3)
            } else {
                None
            },
        })
    }

    /// The file's raw bytes if it exists (decode them with
    /// [`SaveFile::decode`]). `Ok(None)` when it doesn't.
    pub fn load_raw(&self) -> io::Result<Option<Vec<u8>>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// What the bytes of this cartridge's save file hold.
    pub fn decode(&self, bytes: &[u8]) -> Decoded {
        decode(bytes, self.ram_size, self.clock)
    }

    /// The file's bytes for this RAM and clock, stamped `now_unix`.
    pub fn encode(&self, ram: &[u8], clock: Option<Clock>, now_unix: u64) -> Vec<u8> {
        encode(ram, clock.as_ref().map(|c| (c, now_unix)))
    }
}

/// Longest a write may stay owed, in seconds of emulated time, however
/// busy the game keeps the save. Without a cap a game that uses cart SRAM
/// as work RAM writes every frame, never goes quiet and saves only at exit.
pub const MAX_OWED_SECS: u64 = 10;

/// Decides when to write during play. The core's dirty flag stays set
/// until cleared, so the tracker takes each observation of it (the caller
/// clears the flag right after `poll`) and waits for a quiet period of
/// emulated time, because games write a save over several frames. A write
/// owed for `max_owed_clocks` is due even if writes continue.
pub struct SettleTracker {
    quiet_clocks: u64,
    max_owed_clocks: u64,
    /// Emulated clock of the first unsaved write, while a save is owed.
    first_write: Option<u64>,
    /// Emulated clock of the last observed write, while a save is owed.
    last_write: Option<u64>,
}

impl SettleTracker {
    pub fn new(quiet_clocks: u64, max_owed_clocks: u64) -> Self {
        Self {
            quiet_clocks,
            max_owed_clocks,
            first_write: None,
            last_write: None,
        }
    }

    /// `dirty` is `Emulator::save_dirty()`, `clock` the emulated clock.
    /// Returns true when the save should be written now.
    pub fn poll(&mut self, dirty: bool, clock: u64) -> bool {
        if dirty {
            self.last_write = Some(clock);
            self.first_write.get_or_insert(clock);
        }
        let (Some(first), Some(last)) = (self.first_write, self.last_write) else {
            return false;
        };
        let quiet = !dirty && clock.saturating_sub(last) >= self.quiet_clocks;
        if quiet || clock.saturating_sub(first) >= self.max_owed_clocks {
            self.first_write = None;
            self.last_write = None;
            return true;
        }
        false
    }

    /// A failed write is owed again.
    pub fn retry(&mut self, clock: u64) {
        self.first_write = Some(clock);
        self.last_write = Some(clock);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MBC3: Option<ClockKind> = Some(ClockKind::Mbc3);
    const HUC3: Option<ClockKind> = Some(ClockKind::Huc3);

    fn sample_rtc() -> RtcState {
        RtcState {
            live: RtcTime {
                seconds: 59,
                minutes: 59,
                hours: 23,
                days: 0x1FF,
                halted: false,
                day_carry: true,
            },
            latched: RtcTime {
                seconds: 1,
                minutes: 2,
                hours: 3,
                days: 4,
                halted: true,
                day_carry: false,
            },
            subsecond_clocks: 0,
        }
    }

    #[test]
    fn round_trip_with_rtc() {
        let ram: Vec<u8> = (0..32).collect();
        let bytes = encode(&ram, Some((&Clock::Mbc3(sample_rtc()), 1_700_000_000)));
        assert_eq!(bytes.len(), 32 + FOOTER_LEN);
        let d = decode(&bytes, 32, MBC3);
        assert_eq!(d.ram, ram);
        assert_eq!(d.rtc, Some((Clock::Mbc3(sample_rtc()), 1_700_000_000)));
        assert!(d.warnings.is_empty());
    }

    #[test]
    fn footer_layout_matches_bgb() {
        let bytes = encode(
            &[],
            Some((&Clock::Mbc3(sample_rtc()), 0x0102_0304_0506_0708)),
        );
        assert_eq!(&bytes[..4], &59u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &0xFFu32.to_le_bytes()); // DL
        assert_eq!(&bytes[16..20], &0x81u32.to_le_bytes()); // DH: carry + day bit 8
        assert_eq!(&bytes[36..40], &0x40u32.to_le_bytes()); // latched DH: halt
        assert_eq!(&bytes[40..], &0x0102_0304_0506_0708u64.to_le_bytes());
    }

    #[test]
    fn round_trip_without_rtc() {
        let ram = vec![7u8; 8];
        let d = decode(&encode(&ram, None), 8, None);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (ram, None, 0));
    }

    #[test]
    fn rtc_only_cart() {
        let bytes = encode(&[], Some((&Clock::Mbc3(sample_rtc()), 5)));
        assert_eq!(
            decode(&bytes, 0, MBC3).rtc,
            Some((Clock::Mbc3(sample_rtc()), 5))
        );
    }

    #[test]
    fn short_file_is_padded_with_a_warning() {
        let d = decode(&[1, 2, 3], 8, None);
        assert_eq!(d.ram, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(d.warnings.len(), 1);
        let d = decode(&[1, 2, 3], 8, MBC3);
        assert_eq!((d.ram.len(), d.rtc, d.warnings.len()), (8, None, 1));
    }

    #[test]
    fn long_file_is_cut_with_a_warning() {
        let d = decode(&[9; 12], 8, None);
        assert_eq!(d.ram, [9; 8]);
        assert_eq!(d.warnings.len(), 1);
        // An RTC cart with a wrong-sized tail: RAM kept, clock not restored.
        let d = decode(&[9; 12], 8, MBC3);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (vec![9; 8], None, 1));
    }

    #[test]
    fn rtc_cart_file_without_footer_loads_ram_quietly() {
        let d = decode(&[4; 8], 8, MBC3);
        assert_eq!(d.ram, [4; 8]);
        assert_eq!(d.rtc, None);
        assert!(
            d.warnings.is_empty(),
            "plain RAM dump: no warning, {:?}",
            d.warnings
        );
    }

    #[test]
    fn vba_44_byte_footer_loads() {
        let mut bytes = encode(&[1; 4], Some((&Clock::Mbc3(sample_rtc()), 77)));
        bytes.truncate(4 + 44); // drop the high half of the timestamp
        let d = decode(&bytes, 4, MBC3);
        assert_eq!(d.rtc, Some((Clock::Mbc3(sample_rtc()), 77)));
    }

    #[test]
    fn sync_advances_by_wall_clock() {
        let saved = RtcState {
            live: RtcTime {
                seconds: 50,
                ..RtcTime::default()
            },
            ..RtcState::default()
        };
        let sync = |s: RtcState, at, now| match Clock::Mbc3(s).sync(at, now) {
            Clock::Mbc3(s) => s,
            other => panic!("kind changed: {other:?}"),
        };
        let t = sync(saved, 1000, Some(1000 + 3600 + 15)).live;
        assert_eq!((t.hours, t.minutes, t.seconds), (1, 1, 5));
        assert_eq!(sync(saved, 1000, None), saved, "no sync without now");
        assert_eq!(sync(saved, 2000, Some(1000)), saved, "clock went back");
        let mut halted = saved;
        halted.live.halted = true;
        assert_eq!(sync(halted, 0, Some(999)), halted, "halted stays put");
    }

    fn sample_huc3() -> Huc3RtcState {
        let mut c = Huc3RtcState::default();
        c.minutes = 754;
        c.days = 0x1234;
        c.alarm_minutes = 0x321;
        c.alarm_days = 0x2001;
        c.alarm_enabled = true;
        c
    }

    fn huc3(c: Clock) -> Huc3RtcState {
        match c {
            Clock::Huc3(s) => s,
            other => panic!("not a HuC3 clock: {other:?}"),
        }
    }

    #[test]
    fn clock_of_finds_either_kind_in_the_emulator() {
        // The autosave during play reads the clock this way.
        for (cart_type, want) in [(0xFEu8, HUC3), (0x10, MBC3), (0x03, None)] {
            let mut rom = vec![0u8; 0x8000];
            rom[0x147] = cart_type;
            let mut mem = Box::new(gbc_core::Memory::new());
            let mut emu = Emulator::new(&mut mem);
            emu.load(&rom, &mut [], gbc_core::Config::default())
                .unwrap();
            let kind = Clock::of(&emu).map(|c| match c {
                Clock::Mbc3(_) => ClockKind::Mbc3,
                Clock::Huc3(_) => ClockKind::Huc3,
            });
            assert_eq!(kind, want, "type {cart_type:02X}");
            let ejected = emu.eject().unwrap();
            assert_eq!(Clock::of_ejected(&ejected).is_some(), want.is_some());
        }
    }

    #[test]
    fn round_trip_with_huc3_clock() {
        let ram: Vec<u8> = (0..32).collect();
        let bytes = encode(&ram, Some((&Clock::Huc3(sample_huc3()), 1_700_000_000)));
        assert_eq!(bytes.len(), 32 + HUC3_FOOTER_LEN);
        let d = decode(&bytes, 32, HUC3);
        assert_eq!(d.ram, ram);
        assert_eq!(d.rtc, Some((Clock::Huc3(sample_huc3()), 1_700_000_000)));
        assert!(d.warnings.is_empty());
        // Clock only, no RAM.
        let bytes = encode(&[], Some((&Clock::Huc3(sample_huc3()), 5)));
        assert_eq!(
            decode(&bytes, 0, HUC3).rtc,
            Some((Clock::Huc3(sample_huc3()), 5))
        );
    }

    #[test]
    fn huc3_footer_layout_matches_sameboy() {
        // GB_huc3_rtc_time_t, packed: u64 time, u16 minutes, days,
        // alarm_minutes, alarm_days, u8 alarm_enabled.
        let bytes = encode(
            &[0xEE; 2],
            Some((&Clock::Huc3(sample_huc3()), 0x0102_0304_0506_0708)),
        );
        assert_eq!(&bytes[2..10], &0x0102_0304_0506_0708u64.to_le_bytes());
        assert_eq!(&bytes[10..12], &754u16.to_le_bytes());
        assert_eq!(&bytes[12..14], &[0x34, 0x12]);
        assert_eq!(&bytes[14..16], &[0x21, 0x03]);
        assert_eq!(&bytes[16..18], &[0x01, 0x20]);
        assert_eq!(bytes[18], 1);
        assert_eq!(bytes.len(), 19);
    }

    #[test]
    fn huc3_seconds_into_the_minute_survive_through_the_timestamp() {
        // Saved 25.5 s into a minute, loaded 40 s of wall clock later: the
        // minute has ticked once and is 5 s old (the half second is lost).
        let mut c = sample_huc3();
        c.subminute_clocks = 25 * CLOCK_HZ + CLOCK_HZ / 2;
        let bytes = encode(&[], Some((&Clock::Huc3(c), 10_000)));
        assert_eq!(&bytes[..8], &(10_000u64 - 25).to_le_bytes());
        let (stored, at) = decode(&bytes, 0, HUC3).rtc.unwrap();
        assert_eq!((huc3(stored).subminute_clocks, at), (0, 9_975));
        let s = huc3(stored.sync(at, Some(10_040)));
        assert_eq!((s.days, s.minutes), (0x1234, 755));
        assert_eq!(s.subminute_clocks, 5 * CLOCK_HZ);
        // Days pass too; the alarm is carried as stored.
        let s = huc3(stored.sync(at, Some(9_975 + 3 * 86_400 + 700 * 60)));
        assert_eq!((s.days, s.minutes), (0x1238, 14));
        assert_eq!((s.alarm_minutes, s.alarm_days), (0x321, 0x2001));
        assert!(s.alarm_enabled);
        // No sync, or a timestamp in the future: as stored.
        assert_eq!(stored.sync(at, None), stored);
        assert_eq!(stored.sync(at, Some(5)), stored);
    }

    #[test]
    fn huc3_file_with_the_wrong_tail_keeps_the_ram() {
        // A plain RAM dump: no clock, no warning.
        let d = decode(&[4; 8], 8, HUC3);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (vec![4; 8], None, 0));
        // A tail that is not the 17-byte footer (here an MBC3 one): the
        // RAM is kept, the clock is not restored, with a warning.
        let bytes = encode(&[9; 8], Some((&Clock::Mbc3(sample_rtc()), 1)));
        let d = decode(&bytes, 8, HUC3);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (vec![9; 8], None, 1));
        // And a HuC3 footer on an MBC3 cart is not taken for an RTC.
        let bytes = encode(&[9; 8], Some((&Clock::Huc3(sample_huc3()), 1)));
        let d = decode(&bytes, 8, MBC3);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (vec![9; 8], None, 1));
        // Short file.
        let d = decode(&[1, 2, 3], 8, HUC3);
        assert_eq!((d.ram.len(), d.rtc, d.warnings.len()), (8, None, 1));
    }

    #[test]
    fn hostile_huc3_footer_is_safe_to_sync() {
        let mut bytes = vec![7u8; 4];
        bytes.extend_from_slice(&[0xFF; HUC3_FOOTER_LEN]);
        let (stored, at) = decode(&bytes, 4, HUC3).rtc.unwrap();
        assert_eq!(at, u64::MAX);
        let raw = huc3(stored);
        assert_eq!((raw.minutes, raw.days), (0xFFFF, 0xFFFF));
        // The core masks when it counts (and when the state is set).
        for (at, now) in [(at, u64::MAX), (0, u64::MAX), (0, 1), (5, 0)] {
            let s = huc3(stored.sync(at, Some(now)));
            assert!(s.minutes <= 0xFFF && s.subminute_clocks < 60 * CLOCK_HZ);
        }
    }

    #[test]
    fn settle_waits_for_quiet() {
        let mut t = SettleTracker::new(100, 100_000);
        assert!(!t.poll(false, 0), "nothing owed");
        assert!(!t.poll(true, 10));
        assert!(!t.poll(false, 50));
        assert!(!t.poll(true, 60), "another write restarts the wait");
        assert!(!t.poll(false, 159));
        assert!(t.poll(false, 160));
        assert!(!t.poll(false, 1000), "written once only");
        t.retry(1000);
        assert!(t.poll(false, 1100));
    }
    #[test]
    fn constant_writes_save_at_the_cap() {
        let mut t = SettleTracker::new(100, 1000);
        for clock in (0..1000).step_by(10) {
            assert!(!t.poll(true, clock), "not due at {clock}");
        }
        assert!(t.poll(true, 1000), "due once owed for the cap");
        // The timer restarts with the next write and the cap applies again.
        for clock in (1010..2000).step_by(10) {
            assert!(!t.poll(true, clock), "not due at {clock}");
        }
        assert!(t.poll(true, 2010), "next save at most one cap later");
    }

    #[test]
    fn quiet_second_still_saves_before_the_cap() {
        let mut t = SettleTracker::new(100, 1000);
        assert!(!t.poll(true, 500));
        assert!(!t.poll(false, 599));
        assert!(t.poll(false, 600));
        assert!(!t.poll(false, 5000), "nothing owed after saving");
    }

    #[test]
    fn retry_after_forced_save_restarts_the_cap() {
        let mut t = SettleTracker::new(100, 1000);
        assert!(!t.poll(true, 0));
        assert!(t.poll(true, 1000));
        t.retry(1000);
        assert!(!t.poll(true, 1500));
        assert!(!t.poll(true, 1999));
        assert!(t.poll(true, 2000), "cap counts from the retry");
    }

    #[test]
    fn atomic_write_replaces_and_leaves_no_temp() {
        let dir = std::env::temp_dir().join(format!("gbc-save-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.sav");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(fs::read(&p).unwrap(), b"two");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);

        // A plain `fs::write` would clobber the target; the temp-file path
        // fails before touching it. Block the temp name with a directory.
        fs::create_dir(dir.join("a.sav.tmp")).unwrap();
        assert!(write_atomic(&p, b"three").is_err());
        assert_eq!(fs::read(&p).unwrap(), b"two", "old save intact on failure");
        fs::remove_dir_all(&dir).unwrap();
    }

    /// Not a check: how long `write_atomic` blocks on this machine, for
    /// attributing frame stalls (`cargo test -- --ignored --nocapture
    /// write_atomic_latency`; `GBC_SAVE_BENCH_DIR` picks the directory,
    /// default the temp dir). 32 KiB + footer, like an MBC3 RTC cart.
    #[test]
    #[ignore]
    fn write_atomic_latency() {
        let base =
            std::env::var_os("GBC_SAVE_BENCH_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
        let dir = base.join(format!("gbc-save-bench-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bench.sav");
        let data = vec![0x5Au8; 32 * 1024 + FOOTER_LEN];
        let mut ms: Vec<f64> = (0..50)
            .map(|i| {
                let mut d = data.clone();
                d[0] = i as u8;
                let t = std::time::Instant::now();
                write_atomic(&p, &d).unwrap();
                t.elapsed().as_secs_f64() * 1e3
            })
            .collect();
        ms.sort_by(f64::total_cmp);
        println!(
            "write_atomic in {}: min {:.2} p50 {:.2} p90 {:.2} max {:.2} ms",
            dir.display(),
            ms[0],
            ms[25],
            ms[45],
            ms[49]
        );
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod path_tests {
    use super::*;

    #[test]
    fn sibling_roms_with_the_same_stem_get_separate_saves() {
        let dir = std::env::temp_dir().join(format!("gbc-savepath-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let gb = dir.join("game.gb");
        let gbc = dir.join("game.gbc");
        fs::write(&gb, b"").unwrap();
        assert_eq!(save_path(&gb), dir.join("game.sav"), "alone: stem.sav");
        fs::write(&gbc, b"").unwrap();
        assert_eq!(save_path(&gb), dir.join("game.gb.sav"));
        assert_eq!(save_path(&gbc), dir.join("game.gbc.sav"));
        // An unrelated file with the same stem doesn't count.
        let other = dir.join("zelda.gbc");
        fs::write(&other, b"").unwrap();
        fs::write(dir.join("zelda.txt"), b"").unwrap();
        assert_eq!(save_path(&other), dir.join("zelda.sav"));
        fs::remove_dir_all(&dir).unwrap();
    }
}
