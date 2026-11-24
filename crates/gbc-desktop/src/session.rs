//! Loading a ROM and its save, and writing the save back (ADR 0023). The
//! window and headless loops both run inside [`run`].

use std::path::Path;

use gbc_core::{CLOCK_HZ, Config, Emulator, Memory, cart_info};

use crate::compat_palette;
use crate::input_log::{Header, RomId, fnv1a64};
use crate::save::{Clock, MAX_OWED_SECS, SaveFile, SettleTracker};
use crate::save_writer::{SaveWriter, WriteStats};

/// Unix time now, in seconds. The only wall clock the front end reads.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub struct Options<'a> {
    pub rom_path: &'a Path,
    /// Wall-clock source: the time stamped on saves, and the "now" the
    /// stored RTC is advanced to on load. Tests pass a fixed one.
    pub now: fn() -> u64,
    /// Advance a stored RTC by the wall-clock time since it was saved.
    pub sync_rtc_on_load: bool,
    /// Colors for DMG-only cartridges (ADR 0025).
    pub dmg_palette: compat_palette::Mode,
    /// `--replay-input`: start from the recording's cartridge RAM, RTC
    /// and palette instead of the `.sav`, and write no save (ADR 0030).
    pub replay: Option<&'a Header>,
}

/// Writes the save during play. The main loop calls
/// [`Autosave::after_frame`] once per emulated frame; the file is written
/// on a background thread ([`SaveWriter`]), so the loop never waits for
/// the disk.
pub struct Autosave {
    file: Option<(SaveFile, SaveWriter)>,
    tracker: SettleTracker,
    now: fn() -> u64,
}

impl Autosave {
    /// Saves once the game has written to its save and then left it alone
    /// for one second of emulated time, or after `MAX_OWED_SECS` of
    /// continuous writes. Failures warn (on the writer thread) and are
    /// retried one second later.
    pub fn after_frame(&mut self, emu: &mut Emulator<'_>) {
        let Some((file, writer)) = &self.file else {
            return;
        };
        if writer.take_failed() {
            self.tracker.retry(emu.clock());
        }
        let dirty = emu.save_dirty();
        let due = self.tracker.poll(dirty, emu.clock());
        if dirty {
            emu.clear_save_dirty();
        }
        if !due {
            return;
        }
        // A snapshot: the RAM copy is microseconds; the write is not ours.
        writer.submit(file.encode(emu.cart_ram(), Clock::of(emu), (self.now)()));
    }

    /// Writes finished since the last call (for `GBC_FRAME_STATS`).
    pub fn take_write_stats(&mut self) -> WriteStats {
        self.file
            .as_ref()
            .map_or_else(WriteStats::default, |(_, w)| w.take_stats())
    }

    /// Waits until every queued write has finished (tests).
    #[cfg(test)]
    pub fn flush(&self) {
        if let Some((_, w)) = &self.file {
            w.wait_idle();
        }
    }
}

/// Loads the ROM at `opts.rom_path` (and its `.sav`), calls `body`, then
/// ejects the cartridge and writes the save: always, for a cartridge that
/// has one, since the RTC counts without marking the save dirty (ADR
/// 0015). `body` also gets the state the run started from, as a recording
/// header (ADR 0030). With `opts.replay` the run starts from that header
/// instead and no save is read or written. `Err` is a message for stderr
/// (exit 1).
pub fn run<R>(
    opts: &Options<'_>,
    body: impl FnOnce(&mut Emulator<'_>, &mut Autosave, &Header) -> R,
) -> Result<R, String> {
    let path = opts.rom_path;
    let rom = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let info = cart_info(&rom).map_err(|e| format!("{}: {e}", path.display()))?;
    let rom_id = RomId::of(&rom);
    let mut cart_ram = vec![0u8; info.ram_size];
    let mut stored_rtc = None;
    let mut sav = None;
    let mut dmg_palette = opts.dmg_palette;
    let save = match opts.replay {
        Some(h) => {
            h.check_rom(&rom_id)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if h.cart_ram.len() != info.ram_size || h.rtc.is_some() != info.rtc {
                return Err(format!(
                    "{}: the recording's cartridge RAM or RTC does not fit this cartridge",
                    path.display()
                ));
            }
            cart_ram.copy_from_slice(&h.cart_ram);
            dmg_palette = h.dmg_palette;
            None
        }
        None => SaveFile::for_rom(path, &info),
    };
    if let Some(save) = &save {
        let loaded = save
            .load_raw()
            .map_err(|e| format!("can't read {}: {e}", save.path.display()))?;
        if let Some(bytes) = loaded {
            sav = Some((bytes.len(), fnv1a64(&bytes)));
            let d = save.decode(&bytes);
            for w in &d.warnings {
                eprintln!("warning: {}: {w}", save.path.display());
            }
            cart_ram.copy_from_slice(&d.ram);
            stored_rtc = d.rtc;
        }
    }

    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    let config = Config {
        dmg_palettes: compat_palette::select(dmg_palette, &rom),
    };
    emu.load(&rom, &mut cart_ram, config)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some((rtc, saved_at)) = stored_rtc {
        let now = opts.sync_rtc_on_load.then(opts.now);
        rtc.sync(saved_at, now).restore(&mut emu);
    }
    if let Some(rtc) = opts.replay.and_then(|h| h.rtc) {
        emu.set_rtc_state(rtc);
    }
    let start = Header {
        rom_title: rom_id.title,
        header_checksum: rom_id.header_checksum,
        global_checksum: rom_id.global_checksum,
        rom_hash: rom_id.hash,
        dmg_palette,
        sav: opts.replay.map_or(sav, |h| h.sav),
        cart_ram: emu.cart_ram().to_vec(),
        rtc: emu.rtc_state(),
    };

    let file = match save {
        Some(f) => {
            let w = SaveWriter::spawn(f.path.clone())
                .map_err(|e| format!("can't start the save writer: {e}"))?;
            Some((f, w))
        }
        None => None,
    };
    let mut autosave = Autosave {
        file,
        tracker: SettleTracker::new(u64::from(CLOCK_HZ), u64::from(CLOCK_HZ) * MAX_OWED_SECS),
        now: opts.now,
    };
    // A panic in the loop still gets the exit save, then keeps unwinding.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        body(&mut emu, &mut autosave, &start)
    }));

    // The exit save goes after any autosave still queued or in flight;
    // `finish` returns once it is on disk (or failed).
    let ejected = emu.eject().expect("a cartridge is loaded");
    if let Some((file, writer)) = autosave.file.take() {
        let last = file.encode(
            ejected.save_ram(),
            Clock::of_ejected(&ejected),
            (opts.now)(),
        );
        writer
            .finish(Some(last))
            .map_err(|e| format!("can't write {}: {e}", file.path.display()))?;
    }
    match result {
        Ok(r) => Ok(r),
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gbc_core::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH, WHITE};

    #[test]
    fn autosave_writes_after_a_quiet_second_not_before() {
        let dir = std::env::temp_dir().join(format!("gbc-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("a.gb");
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]);
        rom[0x147] = 0x03; // MBC1+RAM+BATTERY
        rom[0x149] = 0x02;
        rom[0x14D] = rom[0x134..=0x14C]
            .iter()
            .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
        // ld a,0A; ld (0000),a; ld a,42; ld (A000),a; jr $
        rom[0x150..0x15B]
            .copy_from_slice(&[0x3E, 0x0A, 0xEA, 0, 0, 0x3E, 0x42, 0xEA, 0, 0xA0, 0x18]);
        rom[0x15B] = 0xFE;
        std::fs::write(&rom_path, &rom).unwrap();
        let opts = Options {
            rom_path: &rom_path,
            now: || 1,
            sync_rtc_on_load: false,
            dmg_palette: compat_palette::Mode::Auto,
            replay: None,
        };
        let sav = dir.join("a.sav");
        let (early, late) = run(&opts, |emu, autosave, _| {
            let mut frame: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
            for _ in 0..30 {
                let _ = emu.run_frame(&mut frame, &mut []);
                autosave.after_frame(emu);
            }
            autosave.flush();
            let early = sav.exists();
            for _ in 0..60 {
                let _ = emu.run_frame(&mut frame, &mut []);
                autosave.after_frame(emu);
            }
            autosave.flush();
            (early, std::fs::read(&sav).ok())
        })
        .unwrap();
        assert!(!early, "still settling after half a second");
        assert_eq!(late.map(|s| s[0]), Some(0x42), "written during play");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_failed_autosave_is_retried() {
        let dir = std::env::temp_dir().join(format!("gbc-retry-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("r.gb");
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]);
        rom[0x147] = 0x03; // MBC1+RAM+BATTERY
        rom[0x149] = 0x02;
        rom[0x14D] = rom[0x134..=0x14C]
            .iter()
            .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
        // ld a,0A; ld (0000),a; ld a,42; ld (A000),a; jr $ (one write only)
        rom[0x150..0x15B]
            .copy_from_slice(&[0x3E, 0x0A, 0xEA, 0, 0, 0x3E, 0x42, 0xEA, 0, 0xA0, 0x18]);
        rom[0x15B] = 0xFE;
        std::fs::write(&rom_path, &rom).unwrap();
        let opts = Options {
            rom_path: &rom_path,
            now: || 1,
            sync_rtc_on_load: false,
            dmg_palette: compat_palette::Mode::Auto,
            replay: None,
        };
        let sav = dir.join("r.sav");
        // A directory in the temp file's place makes the write fail.
        let block = dir.join("r.sav.tmp");
        std::fs::create_dir(&block).unwrap();
        let (first, retried) = run(&opts, |emu, autosave, _| {
            let mut frame: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
            let mut frames = |n| {
                for _ in 0..n {
                    let _ = emu.run_frame(&mut frame, &mut []);
                    autosave.after_frame(emu);
                    autosave.flush();
                }
            };
            frames(90); // the write is due after a quiet second, and fails
            let first = sav.exists();
            std::fs::remove_dir(&block).unwrap();
            frames(90); // the game wrote nothing new: only a retry saves
            (first, std::fs::read(&sav).ok())
        })
        .unwrap();
        assert!(!first, "the blocked write failed");
        assert_eq!(retried.map(|s| s[0]), Some(0x42), "retried during play");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A HuC3 cartridge's clock goes through the save file like an MBC3
    /// RTC: read from the footer, advanced by the wall-clock time since
    /// the save, counted during the run, written back at exit.
    #[test]
    fn huc3_clock_round_trips_through_the_save_file() {
        use crate::save::{self, ClockKind, HUC3_FOOTER_LEN, decode};
        use gbc_core::Huc3RtcState;
        let dir = std::env::temp_dir().join(format!("gbc-huc3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("h.gb");
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]);
        rom[0x147] = 0xFE; // HuC3
        rom[0x149] = 0x02;
        rom[0x14D] = rom[0x134..=0x14C]
            .iter()
            .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
        rom[0x150..0x152].copy_from_slice(&[0x18, 0xFE]); // jr $
        std::fs::write(&rom_path, &rom).unwrap();
        // A save from an earlier session: day 7, 23:58, stamped t = 1000.
        let mut old = Huc3RtcState::default();
        old.days = 7;
        old.minutes = 1438;
        old.alarm_minutes = 0x123;
        let mut ram = vec![0u8; 0x2000];
        ram[5] = 0x99;
        let sav = dir.join("h.sav");
        std::fs::write(&sav, save::encode(&ram, Some((&Clock::Huc3(old), 1000)))).unwrap();
        // This session starts 3 minutes and 30 seconds later.
        let opts = Options {
            rom_path: &rom_path,
            now: || 1210,
            sync_rtc_on_load: true,
            dmg_palette: compat_palette::Mode::Auto,
            replay: None,
        };
        let (at_start, ram5) = run(&opts, |emu, _, _| {
            let at_start = emu.huc3_rtc_state().unwrap();
            let mut frame: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
            // A little over 30 s of emulated time: the next minute.
            let target = emu.clock() + 31 * u64::from(CLOCK_HZ);
            while emu.clock() < target {
                let _ = emu.run_until(target, &mut frame, &mut []);
            }
            (at_start, emu.cart_ram()[5])
        })
        .unwrap();
        assert_eq!((at_start.days, at_start.minutes), (8, 1));
        assert_eq!(at_start.subminute_clocks, 30 * CLOCK_HZ);
        assert_eq!((at_start.alarm_minutes, ram5), (0x123, 0x99));
        // The exit save: the RAM and the clock as it left, 00:02 and 1 s.
        let bytes = std::fs::read(&sav).unwrap();
        assert_eq!(bytes.len(), 0x2000 + HUC3_FOOTER_LEN);
        let d = decode(&bytes, 0x2000, Some(ClockKind::Huc3));
        assert_eq!(d.ram[5], 0x99);
        let mut want = Huc3RtcState::default();
        want.days = 8;
        want.minutes = 2;
        want.alarm_minutes = 0x123;
        assert_eq!(d.rtc, Some((Clock::Huc3(want), 1210 - 1)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_panic_in_the_loop_still_writes_the_exit_save() {
        let dir = std::env::temp_dir().join(format!("gbc-panic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("p.gb");
        let mut rom = vec![0u8; 0x8000];
        rom[0x147] = 0x03; // MBC1+RAM+BATTERY
        rom[0x149] = 0x02;
        rom[0x14D] = rom[0x134..=0x14C]
            .iter()
            .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
        std::fs::write(&rom_path, &rom).unwrap();
        let opts = Options {
            rom_path: &rom_path,
            now: || 1,
            sync_rtc_on_load: false,
            dmg_palette: compat_palette::Mode::Auto,
            replay: None,
        };
        let r = std::panic::catch_unwind(|| {
            let _ = run(&opts, |_, _, _| panic!("boom"));
        });
        assert!(r.is_err(), "the panic keeps unwinding");
        assert!(dir.join("p.sav").exists(), "exit save written");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
