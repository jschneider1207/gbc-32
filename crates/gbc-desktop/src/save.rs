//! Save files: cartridge RAM plus, for MBC3+RTC carts, the 48-byte
//! BGB/VBA-M/SameBoy RTC footer (ADR 0023).
//!
//! Wall-clock time is read only here and in `main`, never in the core.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gbc_core::{CartInfo, RtcState, RtcTime};

/// Size of the de-facto RTC footer: 10 x u32 LE and a u64 LE unix time.
pub const FOOTER_LEN: usize = 48;
/// The older VBA footer, with a u32 timestamp, accepted on load.
const FOOTER_LEN_32: usize = 44;

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

/// Encodes a save file: the RAM, then (RTC carts) the footer. The RTC's
/// sub-second count is not stored (the format has no field for it; loading
/// restarts the second, an error under one second per save).
pub fn encode(ram: &[u8], rtc: Option<(&RtcState, u64)>) -> Vec<u8> {
    let mut out = Vec::with_capacity(ram.len() + FOOTER_LEN);
    out.extend_from_slice(ram);
    if let Some((rtc, unix)) = rtc {
        put_time(&mut out, &rtc.live);
        put_time(&mut out, &rtc.latched);
        out.extend_from_slice(&unix.to_le_bytes());
    }
    out
}

/// What a save file held.
#[derive(Debug, PartialEq, Eq)]
pub struct Decoded {
    /// Exactly `ram_size` bytes: the file's RAM, zero-padded if short.
    pub ram: Vec<u8>,
    /// The stored clock and the unix time it was stored at.
    pub rtc: Option<(RtcState, u64)>,
    pub warnings: Vec<String>,
}

/// Decodes a save file for a cartridge with `ram_size` bytes of RAM.
/// Tolerant: a short file fills what it has, a long one is cut (both warn).
pub fn decode(bytes: &[u8], ram_size: usize, has_rtc: bool) -> Decoded {
    let mut warnings = Vec::new();
    let mut rtc = None;
    let ram_end = bytes.len().min(ram_size);
    if has_rtc {
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
            rtc = Some((state, unix));
        } else if bytes.len() >= ram_size {
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

/// Clock state to hand to `Emulator::set_rtc_state` after loading: the
/// stored clock advanced by the wall-clock time since it was saved. `now`
/// is `None` to skip the sync (tests, and anything that must not depend on
/// the host clock); a stored time in the future counts as no time passed.
pub fn sync_rtc(saved: RtcState, saved_unix: u64, now: Option<u64>) -> RtcState {
    match now {
        Some(now) => saved.advance_seconds(now.saturating_sub(saved_unix)),
        None => saved,
    }
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
    pub has_rtc: bool,
}

impl SaveFile {
    /// `None` for cartridges with nothing to persist.
    pub fn for_rom(rom: &Path, info: &CartInfo) -> Option<Self> {
        info.has_save().then(|| Self {
            path: save_path(rom),
            ram_size: info.ram_size,
            has_rtc: info.rtc,
        })
    }

    /// Reads the save if the file exists. `Ok(None)` when it doesn't.
    pub fn load(&self) -> io::Result<Option<Decoded>> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(decode(&bytes, self.ram_size, self.has_rtc))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn store(&self, ram: &[u8], rtc: Option<RtcState>, now_unix: u64) -> io::Result<()> {
        let bytes = encode(ram, rtc.as_ref().map(|r| (r, now_unix)));
        write_atomic(&self.path, &bytes)
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
        let bytes = encode(&ram, Some((&sample_rtc(), 1_700_000_000)));
        assert_eq!(bytes.len(), 32 + FOOTER_LEN);
        let d = decode(&bytes, 32, true);
        assert_eq!(d.ram, ram);
        assert_eq!(d.rtc, Some((sample_rtc(), 1_700_000_000)));
        assert!(d.warnings.is_empty());
    }

    #[test]
    fn footer_layout_matches_bgb() {
        let bytes = encode(&[], Some((&sample_rtc(), 0x0102_0304_0506_0708)));
        assert_eq!(&bytes[..4], &59u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &0xFFu32.to_le_bytes()); // DL
        assert_eq!(&bytes[16..20], &0x81u32.to_le_bytes()); // DH: carry + day bit 8
        assert_eq!(&bytes[36..40], &0x40u32.to_le_bytes()); // latched DH: halt
        assert_eq!(&bytes[40..], &0x0102_0304_0506_0708u64.to_le_bytes());
    }

    #[test]
    fn round_trip_without_rtc() {
        let ram = vec![7u8; 8];
        let d = decode(&encode(&ram, None), 8, false);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (ram, None, 0));
    }

    #[test]
    fn rtc_only_cart() {
        let bytes = encode(&[], Some((&sample_rtc(), 5)));
        assert_eq!(decode(&bytes, 0, true).rtc, Some((sample_rtc(), 5)));
    }

    #[test]
    fn short_file_is_padded_with_a_warning() {
        let d = decode(&[1, 2, 3], 8, false);
        assert_eq!(d.ram, [1, 2, 3, 0, 0, 0, 0, 0]);
        assert_eq!(d.warnings.len(), 1);
        let d = decode(&[1, 2, 3], 8, true);
        assert_eq!((d.ram.len(), d.rtc, d.warnings.len()), (8, None, 1));
    }

    #[test]
    fn long_file_is_cut_with_a_warning() {
        let d = decode(&[9; 12], 8, false);
        assert_eq!(d.ram, [9; 8]);
        assert_eq!(d.warnings.len(), 1);
        // An RTC cart with a wrong-sized tail: RAM kept, clock not restored.
        let d = decode(&[9; 12], 8, true);
        assert_eq!((d.ram, d.rtc, d.warnings.len()), (vec![9; 8], None, 1));
    }

    #[test]
    fn rtc_cart_file_without_footer_loads_ram_quietly() {
        let d = decode(&[4; 8], 8, true);
        assert_eq!(d.ram, [4; 8]);
        assert_eq!(d.rtc, None);
        assert_eq!(d.warnings.len(), 1, "says the clock is missing");
    }

    #[test]
    fn vba_44_byte_footer_loads() {
        let mut bytes = encode(&[1; 4], Some((&sample_rtc(), 77)));
        bytes.truncate(4 + 44); // drop the high half of the timestamp
        let d = decode(&bytes, 4, true);
        assert_eq!(d.rtc, Some((sample_rtc(), 77)));
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
        let t = sync_rtc(saved, 1000, Some(1000 + 3600 + 15)).live;
        assert_eq!((t.hours, t.minutes, t.seconds), (1, 1, 5));
        assert_eq!(sync_rtc(saved, 1000, None), saved, "no sync without now");
        assert_eq!(sync_rtc(saved, 2000, Some(1000)), saved, "clock went back");
        let mut halted = saved;
        halted.live.halted = true;
        assert_eq!(sync_rtc(halted, 0, Some(999)), halted, "halted stays put");
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
