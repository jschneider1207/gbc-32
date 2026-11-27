//! `gbc_firmware::save` against a real core: round trips, refusals, and
//! reloading into a running emulator. Synthetic ROMs (a header and a
//! `jr -2` loop), so no test data is needed. Equality with the desktop's
//! encoder is checked in `crates/gbc-desktop/src/save.rs`, which sees
//! both.

use gbc_core::{
    CLOCK_HZ, Config, Emulator, Frame, Huc3RtcState, Memory, RtcState, RtcTime, StereoSample,
};
use gbc_firmware::save::{
    self, Clock, ClockKind, HUC3_FOOTER_LEN, Layout, MBC3_FOOTER_LEN, SaveError,
};

const MBC3_RTC: u8 = 0x10; // MBC3+TIMER+RAM+BATTERY
const MBC5_BATT: u8 = 0x1B; // MBC5+RAM+BATTERY
const MBC5_NO_BATT: u8 = 0x1A; // MBC5+RAM
const MBC2_BATT: u8 = 0x06;
const HUC3: u8 = 0xFE;

fn rom(cart_type: u8, ram_code: u8) -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0x18, 0xFE, 0x00]); // nop; jr -2
    rom[0x147] = cart_type;
    rom[0x149] = ram_code;
    rom
}

fn ram(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 + 3) as u8).collect()
}

fn rtc_footer(unix: u64) -> Vec<u8> {
    // live 59 s 59 m 23 h day 0x1FF with carry; latched 1 2 3 day 4, halted.
    let mut f = Vec::new();
    for v in [59u32, 59, 23, 0xFF, 0x81, 1, 2, 3, 4, 0x40] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&unix.to_le_bytes());
    f
}

fn huc3_footer(unix: u64) -> Vec<u8> {
    let mut f = unix.to_le_bytes().to_vec();
    for v in [754u16, 0x1234, 0x321, 0x2001] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.push(1);
    f
}

/// Checks `image` for `rom`, loads it (RAM before `load`, clock after),
/// and takes the save straight back with the image's own timestamp.
fn load_then_take(rom: &[u8], image: &[u8]) -> Result<Vec<u8>, SaveError> {
    let layout = Layout::of_rom(rom)?;
    let img = save::check(layout, image)?;
    let mut buf = vec![0xAAu8; layout.ram_size + 100];
    img.write_ram(&mut buf)?;
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert!(emu.load(rom, &mut buf, Config::default()).is_ok());
    img.restore_clock(&mut emu);
    let mut out = vec![0u8; layout.len()];
    let n = save::take(&emu, img.unix_time(), &mut out)?;
    out.truncate(n);
    Ok(out)
}

#[test]
fn round_trips_are_byte_identical() {
    let cases: [(u8, u8, Vec<u8>); 5] = [
        (
            MBC3_RTC,
            0x03,
            [ram(0x8000), rtc_footer(1_700_000_000)].concat(),
        ),
        (MBC3_RTC, 0x00, rtc_footer(u64::MAX)), // RTC only, no RAM
        (MBC5_BATT, 0x02, ram(0x2000)),
        (
            MBC2_BATT,
            0x00,
            (0..512).map(|i| 0xF0 | (i as u8 & 0xF)).collect(),
        ),
        (
            HUC3,
            0x03,
            [ram(0x8000), huc3_footer(0x0102_0304_0506_0708)].concat(),
        ),
    ];
    for (ty, code, image) in cases {
        assert_eq!(
            load_then_take(&rom(ty, code), &image),
            Ok(image),
            "type {ty:02x}"
        );
    }
}

#[test]
fn footer_fields_decode_as_documented() {
    let layout = Layout::of_rom(&rom(MBC3_RTC, 0x02)).unwrap();
    assert_eq!(
        layout,
        Layout {
            ram_size: 0x2000,
            clock: Some(ClockKind::Mbc3)
        }
    );
    let image = [ram(0x2000), rtc_footer(42)].concat();
    let img = save::check(layout, &image).unwrap();
    let t = |seconds, minutes, hours, days, halted, day_carry| RtcTime {
        seconds,
        minutes,
        hours,
        days,
        halted,
        day_carry,
    };
    assert_eq!(
        img.clock(),
        Some(Clock::Mbc3(RtcState {
            live: t(59, 59, 23, 0x1FF, false, true),
            latched: t(1, 2, 3, 4, true, false),
            subsecond_clocks: 0,
        }))
    );
    assert_eq!((img.unix_time(), img.ram()), (42, &image[..0x2000]));

    let layout = Layout::of_rom(&rom(HUC3, 0x03)).unwrap();
    let image = [ram(0x8000), huc3_footer(7)].concat();
    let Some(Clock::Huc3(c)) = save::check(layout, &image).unwrap().clock() else {
        panic!("no HuC3 clock");
    };
    assert_eq!(
        (
            c.minutes,
            c.days,
            c.alarm_minutes,
            c.alarm_days,
            c.alarm_enabled
        ),
        (754, 0x1234, 0x321, 0x2001, true)
    );
}

#[test]
fn wrong_sizes_are_refused() {
    let mbc3 = Layout::of_rom(&rom(MBC3_RTC, 0x02)).unwrap();
    let mbc5 = Layout::of_rom(&rom(MBC5_BATT, 0x02)).unwrap();
    let huc3 = Layout::of_rom(&rom(HUC3, 0x02)).unwrap();
    for (layout, len) in [
        (mbc3, 0x2000 - 1),
        (mbc3, 0x2000 + 1),
        (mbc3, 0x2000 + 47),
        (mbc3, 0x2000 + 49),
        (mbc3, 0x2000 + HUC3_FOOTER_LEN),
        (mbc5, 0),
        (mbc5, 0x2000 + MBC3_FOOTER_LEN),
        (mbc5, 0x2000 + HUC3_FOOTER_LEN),
        (huc3, 0x2000 + MBC3_FOOTER_LEN),
        (huc3, 0x2000 + 16),
    ] {
        let bytes = vec![0u8; len];
        assert_eq!(
            save::check(layout, &bytes),
            Err(SaveError::WrongSize {
                len,
                ram_size: 0x2000,
                footer: layout.footer_len()
            }),
            "{layout:?} {len}"
        );
    }
    // RAM alone is a valid save for a clock cart (the clock starts fresh),
    // and the 44-byte VBA footer is accepted (u32 time).
    assert_eq!(save::check(mbc3, &[0; 0x2000]).unwrap().clock(), None);
    let mut vba = [ram(0x2000), rtc_footer(0)].concat();
    vba.truncate(0x2000 + 44);
    vba[0x2000 + 40..].copy_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    let img = save::check(mbc3, &vba).unwrap();
    assert_eq!(img.unix_time(), 0xDEAD_BEEF);
    assert!(img.clock().is_some());
}

#[test]
fn carts_without_a_save_are_refused() {
    for (ty, code) in [(0x00, 0x00), (MBC5_NO_BATT, 0x02), (MBC5_BATT, 0x00)] {
        assert_eq!(
            Layout::of_rom(&rom(ty, code)),
            Err(SaveError::NoSave),
            "{ty:02x}"
        );
    }
}

#[test]
fn bad_footers_are_refused_at_the_field() {
    let layout = Layout::of_rom(&rom(MBC3_RTC, 0x02)).unwrap();
    let good = [ram(0x2000), rtc_footer(9)].concat();
    assert!(save::check(layout, &good).is_ok());
    // One past each field's width: S, M, H, DL, then DH's undefined bits.
    for (i, bad) in [64u32, 64, 32, 0x100, 0x02, 64, 64, 32, 0x100, 0x3E]
        .into_iter()
        .enumerate()
    {
        let mut b = good.clone();
        let at = 0x2000 + 4 * i;
        b[at..at + 4].copy_from_slice(&bad.to_le_bytes());
        assert_eq!(
            save::check(layout, &b),
            Err(SaveError::BadFooter { offset: at }),
            "field {i}"
        );
    }
    let layout = Layout::of_rom(&rom(HUC3, 0x02)).unwrap();
    let good = [ram(0x2000), huc3_footer(9)].concat();
    assert!(save::check(layout, &good).is_ok());
    for (at, bytes) in [(8, &[0x00, 0x10][..]), (12, &[0x00, 0x10]), (16, &[2])] {
        let mut b = good.clone();
        let at = 0x2000 + at;
        b[at..at + bytes.len()].copy_from_slice(bytes);
        assert_eq!(
            save::check(layout, &b),
            Err(SaveError::BadFooter { offset: at }),
            "{at}"
        );
    }
}

#[test]
fn short_buffers_are_refused_without_writing() {
    let r = rom(MBC3_RTC, 0x02);
    let layout = Layout::of_rom(&r).unwrap();
    let image = [ram(0x2000), rtc_footer(1)].concat();
    let img = save::check(layout, &image).unwrap();
    let mut short = vec![0x55u8; 0x1FFF];
    assert_eq!(
        img.write_ram(&mut short),
        Err(SaveError::BufferTooSmall {
            need: 0x2000,
            have: 0x1FFF
        })
    );
    assert!(short.iter().all(|&b| b == 0x55));

    let mut buf = vec![0u8; 0x2000];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert_eq!(
        save::take(&emu, 0, &mut [0; 64]),
        Err(SaveError::NoCartridge)
    );
    assert!(emu.load(&r, &mut buf, Config::default()).is_ok());
    let mut out = vec![0x55u8; layout.len() - 1];
    assert_eq!(
        save::take(&emu, 0, &mut out),
        Err(SaveError::BufferTooSmall {
            need: layout.len(),
            have: layout.len() - 1
        })
    );
    assert!(out.iter().all(|&b| b == 0x55));
}

#[test]
fn take_drops_the_part_second_and_stamps_huc3_at_the_minute() {
    let r = rom(HUC3, 0x00);
    let mut buf = Vec::new();
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert!(emu.load(&r, &mut buf, Config::default()).is_ok());
    let mut c = Huc3RtcState::default();
    c.minutes = 5;
    c.subminute_clocks = 17 * CLOCK_HZ + 99;
    emu.set_huc3_rtc_state(c);
    let mut out = [0u8; HUC3_FOOTER_LEN];
    assert_eq!(save::take(&emu, 1000, &mut out), Ok(HUC3_FOOTER_LEN));
    assert_eq!(&out[..8], &983u64.to_le_bytes());
    assert_eq!(&out[8..10], &5u16.to_le_bytes());
}

#[test]
fn reload_replaces_ram_and_clock_of_a_running_game() {
    let r = rom(MBC3_RTC, 0x02);
    let layout = Layout::of_rom(&r).unwrap();
    let mut buf = vec![0u8; 0x2000];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert!(emu.load(&r, &mut buf, Config::default()).is_ok());
    let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
    let mut audio = vec![StereoSample::default(); 1100];
    for _ in 0..3 {
        let _ = emu.run_frame(&mut frame, &mut audio);
    }
    assert!(emu.clock() > 0);

    let image = [ram(0x2000), rtc_footer(77)].concat();
    let img = save::check(layout, &image).unwrap();
    assert_eq!(save::reload(&mut emu, &img, Config::default()), Ok(()));
    assert_eq!(emu.clock(), 0);
    let mut out = vec![0u8; layout.len()];
    assert_eq!(save::take(&emu, 77, &mut out), Ok(layout.len()));
    assert_eq!(out, image);

    // An image checked for another layout changes nothing.
    let other = Layout {
        ram_size: 0x2000,
        clock: None,
    };
    let plain = vec![9u8; 0x2000];
    let img = save::check(other, &plain).unwrap();
    assert_eq!(
        save::reload(&mut emu, &img, Config::default()),
        Err(SaveError::Mismatch)
    );
    assert_eq!(emu.cart_ram(), &image[..0x2000]);
}

/// The `link-probe` image's ROM, byte for byte as
/// `crates/gbc-nucleo-h753/build.sh` (`link_probe_rom`) writes it: 32 KiB
/// of zeros, `nop; jr -2` at 0x100, MBC3+TIMER+RAM+BATTERY, 32 KiB RAM.
fn link_probe_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0x18, 0xFE, 0x00]);
    rom[0x147] = MBC3_RTC;
    rom[0x149] = 0x03;
    rom
}

/// The board round trip's save (`run-save-roundtrip.sh`): a RAM pattern
/// and an MBC3 footer with the clock **halted** (DH bit 6) and latched
/// equal to live, so neither emulated time nor a latch can change it; a
/// non-zero unix time, so the stamp on the dump is checked too.
fn link_probe_sav() -> Vec<u8> {
    let mut s: Vec<u8> = (0..0x8000usize).map(|i| (i * 7 + i / 251) as u8).collect();
    for v in [12u32, 34, 5, 0x89, 0x41, 12, 34, 5, 0x89, 0x41] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    s.extend_from_slice(&1_700_000_000u64.to_le_bytes());
    s
}

const LINK_PROBE_FIXTURE: &str = "../../tools/gbc-pc/tests/fixtures/link-probe.sav";

/// The committed fixture is `link_probe_sav()` (`GBC_WRITE_FIXTURE=1`
/// rewrites it), and on the probe's ROM it survives 600 frames: load,
/// run, take with the image's own timestamp gives the same bytes.
#[test]
fn link_probe_fixture_round_trips_across_frames() {
    let want = link_probe_sav();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(LINK_PROBE_FIXTURE);
    if std::env::var_os("GBC_WRITE_FIXTURE").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    assert_eq!(std::fs::read(&path).unwrap(), want, "{}", path.display());

    let rom = link_probe_rom();
    let layout = Layout::of_rom(&rom).unwrap();
    assert_eq!(layout.len(), want.len());
    let img = save::check(layout, &want).unwrap();
    let mut buf = vec![0u8; layout.ram_size];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert!(emu.load(&rom, &mut buf, Config::default()).is_ok());
    // As the board applies it: into the running emulator.
    save::reload(&mut emu, &img, Config::default()).unwrap();
    let mut fb: Frame = [0; gbc_core::SCREEN_WIDTH * gbc_core::SCREEN_HEIGHT];
    let mut audio = [StereoSample { left: 0, right: 0 }; 0];
    for _ in 0..600 {
        let _ = emu.run_frame(&mut fb, &mut audio);
    }
    let mut out = vec![0u8; want.len()];
    let n = save::take(&emu, img.unix_time(), &mut out).unwrap();
    assert_eq!(n, want.len());
    assert_eq!(out, want);
}

/// K3: a footer stamped 0 means "unknown" (the board has no wall clock),
/// so syncing from it must not advance the clock by ~55 years.
#[test]
fn sync_from_a_zero_stamp_does_not_advance() {
    let clock = Clock::Mbc3(RtcState::default());
    let now = 1_760_000_000;
    assert_eq!(clock.sync(0, now), clock);
    let huc = Clock::Huc3(Huc3RtcState::default());
    assert_eq!(huc.sync(0, now), huc);
    // A real stamp still advances.
    assert_ne!(clock.sync(now - 3600, now), clock);
}
