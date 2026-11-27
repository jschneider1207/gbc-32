//! The RTC sync on `load --sav`, the restamp on `dump`, and `--raw-save`
//! (ADR 0040, M5 review T-2), on synthetic saves. The desktop front end's
//! sync on load is `gbc_firmware::save::Clock::sync` (it delegates to
//! it), and its file decoding equals `gbc_firmware::save::check`
//! (`crates/gbc-desktop/src/save.rs` tests), so "the desktop loads it" is
//! `check` + `Clock::sync` here.

use std::path::PathBuf;
use std::time::Duration;

use gbc_core::{Config, Emulator, Memory, RtcState, RtcTime};
use gbc_firmware::save::{self, Clock, Layout, SaveError};
use gbc_pc::rtc::{for_load, from_dump};
use gbc_pc::save::dump_to_file;
use gbc_pc::sim::SimBoard;
use gbc_pc::{Client, Config as LinkConfig};

const MBC3_RTC: u8 = 0x10; // MBC3+TIMER+RAM+BATTERY
const MBC5_BATT: u8 = 0x1B;
const HUC3: u8 = 0xFE;
const RAM_32K: u8 = 0x03;
const T0: u64 = 1_790_000_000;

fn rom(cart_type: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0x18, 0xFE, 0x00]); // nop; jr -2
    rom[0x147] = cart_type;
    rom[0x149] = RAM_32K;
    rom
}

fn ram() -> Vec<u8> {
    (0..0x8000).map(|i| (i * 7 + 3) as u8).collect()
}

fn time(s: u8, m: u8, h: u8, days: u16, halted: bool, day_carry: bool) -> RtcTime {
    RtcTime {
        seconds: s,
        minutes: m,
        hours: h,
        days,
        halted,
        day_carry,
    }
}

fn latched() -> RtcTime {
    time(1, 2, 3, 4, true, false)
}

/// RAM + 48-byte footer with `live`, [`latched`] and the stamp.
fn mbc3_sav(live: RtcTime, unix: u64) -> Vec<u8> {
    let clock = Clock::Mbc3(RtcState {
        live,
        latched: latched(),
        subsecond_clocks: 0,
    });
    let mut out = vec![0; 0x8000 + save::MBC3_FOOTER_LEN];
    save::encode(&ram(), Some(clock), unix, &mut out).unwrap();
    out
}

/// What the desktop (or the board) reads back: the clock and the stamp.
fn read(cart: u8, sav: &[u8]) -> (Clock, u64) {
    let image = save::check(Layout::of_rom(&rom(cart)).unwrap(), sav).unwrap();
    assert_eq!(image.ram(), &ram()[..], "RAM bytes untouched");
    (image.clock().unwrap(), image.unix_time())
}

fn live(c: Clock) -> RtcTime {
    match c {
        Clock::Mbc3(s) => {
            assert_eq!(s.latched, latched(), "latched registers untouched");
            s.live
        }
        Clock::Huc3(_) => panic!("not MBC3"),
    }
}

#[test]
fn load_advances_a_running_clock_by_the_time_since_the_stamp() {
    let sav = mbc3_sav(time(10, 20, 3, 5, false, false), T0);
    let now = T0 + 2 * 3600 + 61;
    let out = for_load(&rom(MBC3_RTC), &sav, Some(now)).unwrap();
    let (clock, stamp) = read(MBC3_RTC, &out);
    assert_eq!(live(clock), time(11, 21, 5, 5, false, false));
    assert_eq!(stamp, now);
    // A stamp in the future: no time passed, but stamped now.
    let out = for_load(
        &rom(MBC3_RTC),
        &mbc3_sav(time(10, 20, 3, 5, false, false), now + 99),
        Some(now),
    )
    .unwrap();
    assert_eq!(
        read(MBC3_RTC, &out),
        read(MBC3_RTC, &mbc3_sav(time(10, 20, 3, 5, false, false), now))
    );
    // The 44-byte VBA footer goes to the board as the 48-byte one.
    let mut vba = sav.clone();
    vba.truncate(0x8000 + 44);
    vba[0x8000 + 40..].copy_from_slice(&(T0 as u32).to_le_bytes());
    let out = for_load(&rom(MBC3_RTC), &vba, Some(now)).unwrap();
    assert_eq!(out.len(), 0x8000 + 48);
    assert_eq!(
        live(read(MBC3_RTC, &out).0),
        time(11, 21, 5, 5, false, false)
    );
}

#[test]
fn load_leaves_a_halted_clock_and_stamps_it_now() {
    // As the desktop: a halted clock does not advance (the core's
    // advance_seconds); the next save is stamped with the save time.
    let halted = time(10, 20, 3, 5, true, false);
    let out = for_load(&rom(MBC3_RTC), &mbc3_sav(halted, T0), Some(T0 + 86_400)).unwrap();
    assert_eq!(
        read(MBC3_RTC, &out),
        read(MBC3_RTC, &mbc3_sav(halted, T0 + 86_400))
    );
}

#[test]
fn load_day_overflow_wraps_and_sets_the_carry() {
    let at = |live, secs| live_after(live, secs);
    assert_eq!(
        at(time(59, 59, 23, 511, false, false), 1),
        time(0, 0, 0, 0, false, true)
    );
    assert_eq!(
        at(time(0, 0, 0, 3, false, true), 86_400),
        time(0, 0, 0, 4, false, true)
    );
    assert_eq!(
        at(time(0, 0, 0, 0, false, false), 400 * 86_400 + 5),
        time(5, 0, 0, 400, false, false)
    );
}

fn live_after(start: RtcTime, secs: u64) -> RtcTime {
    let out = for_load(&rom(MBC3_RTC), &mbc3_sav(start, T0), Some(T0 + secs)).unwrap();
    let got = live(read(MBC3_RTC, &out).0);
    // The desktop's rule on the same file gives the same clock.
    let desktop = match read(MBC3_RTC, &mbc3_sav(start, T0)).0.sync(T0, T0 + secs) {
        Clock::Mbc3(s) => s.live,
        Clock::Huc3(_) => unreachable!(),
    };
    assert_eq!(got, desktop);
    got
}

fn huc3_sav(minutes: u16, days: u16, unix: u64) -> Vec<u8> {
    let mut f = ram();
    f.extend_from_slice(&unix.to_le_bytes());
    for v in [minutes, days, 0x321, 0x2001] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.push(1);
    f
}

#[test]
fn load_syncs_huc3_as_the_desktop() {
    // Stamped at its last minute tick; 3 min 25 s later the clock is 3
    // minutes on (over midnight) and 25 s into the next one, so the image
    // for the board is stamped 25 s before now.
    let now = T0 + 3 * 60 + 25;
    let out = for_load(&rom(HUC3), &huc3_sav(1439, 9, T0), Some(now)).unwrap();
    assert_eq!(out, huc3_sav(2, 10, now - 25));
    // Loaded on the desktop at `now`, it is the clock the desktop gets
    // from the original file.
    let (c, stamp) = read(HUC3, &out);
    let (orig, orig_stamp) = read(HUC3, &huc3_sav(1439, 9, T0));
    assert_eq!(c.sync(stamp, now), orig.sync(orig_stamp, now));
}

#[test]
fn load_refuses_what_the_board_refuses_unless_raw() {
    let rom = rom(MBC3_RTC);
    let mut bad = mbc3_sav(time(0, 0, 0, 0, false, false), T0);
    bad[0x8000] = 64; // seconds over 63
    assert_eq!(
        for_load(&rom, &bad, Some(T0)),
        Err(SaveError::BadFooter { offset: 0x8000 })
    );
    assert!(matches!(
        for_load(&rom, &bad[..100], Some(T0)),
        Err(SaveError::WrongSize { .. })
    ));
    assert_eq!(for_load(&rom, &bad, None).unwrap(), bad);
    // RAM only, or no clock: nothing to sync, bytes as given.
    assert_eq!(for_load(&rom, &ram(), Some(T0 + 5)).unwrap(), ram());
    assert_eq!(
        for_load(&self::rom(MBC5_BATT), &ram(), Some(T0 + 5)).unwrap(),
        ram()
    );
}

#[test]
fn raw_save_is_byte_exact() {
    let sav = mbc3_sav(time(10, 20, 3, 5, false, false), T0);
    assert_eq!(for_load(&rom(MBC3_RTC), &sav, None).unwrap(), sav);
    assert_eq!(from_dump(sav.clone(), None), (sav, false));
}

#[test]
fn dump_restamp_changes_only_the_stamp() {
    let sav = mbc3_sav(time(10, 20, 3, 5, false, true), T0);
    let (out, restamped) = from_dump(sav.clone(), Some(T0 + 777));
    assert!(restamped);
    assert_eq!(out[..0x8000 + 40], sav[..0x8000 + 40]);
    assert_eq!(out[0x8000 + 40..], (T0 + 777).to_le_bytes());
    let huc3 = huc3_sav(700, 3, T0);
    let (out, restamped) = from_dump(huc3.clone(), Some(T0 + 777));
    assert!(restamped);
    assert_eq!(out, huc3_sav(700, 3, T0 + 777));
    // No footer, or one the board would not have written: as received.
    assert_eq!(from_dump(ram(), Some(T0)), (ram(), false));
    let mut bad = sav;
    bad[0x8000] = 64;
    assert_eq!(from_dump(bad.clone(), Some(T0)), (bad, false));
}

fn later() -> u64 {
    T0 + 1234
}

#[test]
fn dump_to_file_restamps_unless_raw() {
    let sav = mbc3_sav(time(10, 20, 3, 5, false, false), T0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rtc-dump");
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("g.sav");
    let cfg = LinkConfig {
        timeout: Duration::from_millis(50),
        ..LinkConfig::default()
    };
    let mut board = SimBoard::player(1 << 20);
    board.game.save = Some(sav.clone());
    let mut c = Client::new(board, cfg);
    assert_eq!(
        dump_to_file(&mut c, &out, Some(later)).unwrap(),
        (sav.len(), true)
    );
    assert_eq!(
        std::fs::read(&out).unwrap(),
        mbc3_sav(time(10, 20, 3, 5, false, false), later())
    );
    assert_eq!(
        dump_to_file(&mut c, &out, None).unwrap(),
        (sav.len(), false)
    );
    assert_eq!(std::fs::read(&out).unwrap(), sav);
}

/// The M5 review T-2 scenario (Pokémon Crystal): saved on the desktop at
/// T0, loaded onto the board 10 minutes later, played 2 h there, dumped,
/// loaded on the desktop 5 minutes after the dump. The clock must be the
/// saved one plus the wall time that passed, each second counted once.
#[test]
fn desktop_to_board_to_desktop_counts_real_time_once() {
    let rom = rom(MBC3_RTC);
    let start = time(0, 0, 12, 100, false, false);
    let desktop_file = mbc3_sav(start, T0);
    let board_load = T0 + 600;
    let played = 2 * 3600;
    let dump_at = board_load + played;
    let desktop_load = dump_at + 300;

    // gbc-pc load --sav, then the player (crates/gbc-nucleo-h753
    // src/bin/player.rs): check, RAM into the buffer, load, clock in,
    // and it keeps the image's stamp for its dumps.
    let sent = for_load(&rom, &desktop_file, Some(board_load)).unwrap();
    let image = save::check(Layout::of_rom(&rom).unwrap(), &sent).unwrap();
    let mut cart_ram = vec![0; 0x8000];
    image.write_ram(&mut cart_ram).unwrap();
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    emu.load(&rom, &mut cart_ram, Config::default()).unwrap();
    image.restore_clock(&mut emu);
    // 2 h of play: the emulated clock runs them (stood in for by
    // advance_seconds; running 432 000 frames is too slow for a test).
    let s = emu.rtc_state().unwrap();
    emu.set_rtc_state(s.advance_seconds(played));
    let mut dumped = vec![0; image.layout().len()];
    save::take(&emu, image.unix_time(), &mut dumped).unwrap();

    // gbc-pc dump, then the desktop's load.
    let (file, _) = from_dump(dumped, Some(dump_at));
    let (clock, stamp) = read(MBC3_RTC, &file);
    let loaded = live(clock.sync(stamp, desktop_load));
    let (orig, _) = read(MBC3_RTC, &desktop_file);
    assert_eq!(
        loaded,
        live(orig.sync(T0, desktop_load)),
        "saved clock + wall time since T0"
    );
    assert_eq!(loaded, time(0, 15, 14, 100, false, false));
}
