//! Loading a ROM and its save, and writing the save back (ADR 0023). The
//! window and headless loops both run inside [`run`].

use std::path::Path;

use gbc_core::{CLOCK_HZ, Config, Emulator, Memory, cart_info};

use crate::compat_palette;
use crate::save::{MAX_OWED_SECS, SaveFile, SettleTracker, sync_rtc};

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
}

/// Writes the save during play. The main loop calls
/// [`Autosave::after_frame`] once per emulated frame.
pub struct Autosave {
    file: Option<SaveFile>,
    tracker: SettleTracker,
    now: fn() -> u64,
}

impl Autosave {
    /// Saves once the game has written to its save and then left it alone
    /// for one second of emulated time, or after `MAX_OWED_SECS` of
    /// continuous writes. Failures warn and are retried.
    pub fn after_frame(&mut self, emu: &mut Emulator<'_>) {
        let Some(file) = &self.file else { return };
        let dirty = emu.save_dirty();
        let due = self.tracker.poll(dirty, emu.clock());
        if dirty {
            emu.clear_save_dirty();
        }
        if due && let Err(e) = file.store(emu.cart_ram(), emu.rtc_state(), (self.now)()) {
            eprintln!("warning: can't write {}: {e}", file.path.display());
            self.tracker.retry(emu.clock());
        }
    }
}

/// Loads the ROM at `opts.rom_path` (and its `.sav`), calls `body`, then
/// ejects the cartridge and writes the save: always, for a cartridge that
/// has one, since the RTC counts without marking the save dirty (ADR
/// 0015). `Err` is a message for stderr (exit 1).
pub fn run<R>(
    opts: &Options<'_>,
    body: impl FnOnce(&mut Emulator<'_>, &mut Autosave) -> R,
) -> Result<R, String> {
    let path = opts.rom_path;
    let rom = std::fs::read(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let info = cart_info(&rom).map_err(|e| format!("{}: {e}", path.display()))?;
    let save = SaveFile::for_rom(path, &info);
    let mut cart_ram = vec![0u8; info.ram_size];
    let mut stored_rtc = None;
    if let Some(save) = &save {
        let loaded = save
            .load()
            .map_err(|e| format!("can't read {}: {e}", save.path.display()))?;
        if let Some(d) = loaded {
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
        dmg_palettes: compat_palette::select(opts.dmg_palette, &rom),
    };
    emu.load(&rom, &mut cart_ram, config)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some((rtc, saved_at)) = stored_rtc {
        let now = opts.sync_rtc_on_load.then(opts.now);
        emu.set_rtc_state(sync_rtc(rtc, saved_at, now));
    }

    let mut autosave = Autosave {
        file: save,
        tracker: SettleTracker::new(u64::from(CLOCK_HZ), u64::from(CLOCK_HZ) * MAX_OWED_SECS),
        now: opts.now,
    };
    // A panic in the loop still gets the exit save, then keeps unwinding.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        body(&mut emu, &mut autosave)
    }));

    let ejected = emu.eject().expect("a cartridge is loaded");
    if let Some(file) = &autosave.file {
        file.store(ejected.save_ram(), ejected.rtc, (opts.now)())
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
        };
        let sav = dir.join("a.sav");
        let (early, late) = run(&opts, |emu, autosave| {
            let mut frame: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
            for _ in 0..30 {
                let _ = emu.run_frame(&mut frame, &mut []);
                autosave.after_frame(emu);
            }
            let early = sav.exists();
            for _ in 0..60 {
                let _ = emu.run_frame(&mut frame, &mut []);
                autosave.after_frame(emu);
            }
            (early, std::fs::read(&sav).ok())
        })
        .unwrap();
        assert!(!early, "still settling after half a second");
        assert_eq!(late.map(|s| s[0]), Some(0x42), "written during play");
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
        };
        let r = std::panic::catch_unwind(|| {
            let _ = run(&opts, |_, _| panic!("boom"));
        });
        assert!(r.is_err(), "the panic keeps unwinding");
        assert!(dir.join("p.sav").exists(), "exit save written");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
