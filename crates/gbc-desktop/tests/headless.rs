//! Runs the built binary headless on small synthesized ROMs (no test-ROM
//! data needed) and checks exit codes and the `.sav` files it leaves.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const FOOTER: usize = 48;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A scratch directory, removed on drop.
struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Self {
        let p = std::env::temp_dir().join(format!("gbc-desktop-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Dir(p)
    }
    fn path(&self, f: &str) -> PathBuf {
        self.0.join(f)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A 32 KiB ROM with a valid header (8 KiB cart RAM) and `code` at 0x150
/// (the entry point at 0x100 jumps there).
fn rom(cart_type: u8, code: &[u8]) -> Vec<u8> {
    let mut r = vec![0u8; 0x8000];
    r[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // jp 0150
    r[0x134..0x138].copy_from_slice(b"TEST");
    r[0x147] = cart_type;
    r[0x149] = 0x02;
    let sum = r[0x134..=0x14C]
        .iter()
        .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
    r[0x14D] = sum;
    r[0x150..0x150 + code.len()].copy_from_slice(code);
    r
}

/// Enables RAM, writes 0x42 to A000, selects RTC seconds and writes 30,
/// then loops.
const WRITER: &[u8] = &[
    0x3E, 0x0A, 0xEA, 0x00, 0x00, // ld a,0A; ld (0000),a
    0x3E, 0x42, 0xEA, 0x00, 0xA0, // ld a,42; ld (A000),a
    0x3E, 0x08, 0xEA, 0x00, 0x40, // ld a,08; ld (4000),a
    0x3E, 0x1E, 0xEA, 0x00, 0xA0, // ld a,1E; ld (A000),a
    0x18, 0xFE, // jr $
];
/// Enables RAM and loops, writing nothing.
const IDLE: &[u8] = &[0x3E, 0x0A, 0xEA, 0x00, 0x00, 0x18, 0xFE];

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gbc-desktop"))
        .args(args)
        .output()
        .unwrap()
}

fn headless(rom: &Path, frames: &str) -> Output {
    run(&["--headless", "--frames", frames, rom.to_str().unwrap()])
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes(b[..4].try_into().unwrap())
}

#[test]
fn battery_rtc_cart_saves_ram_and_clock() {
    let dir = Dir::new("save");
    let rom_path = dir.path("game.gbc");
    fs::write(&rom_path, rom(0x10, WRITER)).unwrap();
    let before = now();
    let out = headless(&rom_path, "5");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sav = fs::read(dir.path("game.sav")).expect("save written next to the ROM");
    assert_eq!(sav.len(), 0x2000 + FOOTER);
    assert_eq!(sav[0], 0x42);
    let f = &sav[0x2000..];
    assert_eq!(le32(f), 30, "live seconds");
    assert_eq!(le32(&f[20..]), 0, "latched seconds: the game never latched");
    let stamp = u64::from_le_bytes(f[40..48].try_into().unwrap());
    assert!(stamp >= before && stamp <= now(), "unix time stamp");
    assert!(!dir.path("game.sav.tmp").exists());
}

#[test]
fn existing_save_is_loaded_and_the_clock_advanced_by_wall_time() {
    let dir = Dir::new("load");
    let rom_path = dir.path("game.gb");
    fs::write(&rom_path, rom(0x10, IDLE)).unwrap();
    let mut sav = vec![0u8; 0x2000 + FOOTER];
    sav[1] = 0x55;
    // Live and latched clock 00:00:00 day 0, saved an hour ago.
    sav[0x2000 + 40..].copy_from_slice(&(now() - 3600).to_le_bytes());
    fs::write(dir.path("game.sav"), &sav).unwrap();
    let out = headless(&rom_path, "3");
    assert!(out.status.success());
    let back = fs::read(dir.path("game.sav")).unwrap();
    assert_eq!(back[1], 0x55, "RAM from the old save survives");
    let f = &back[0x2000..];
    assert_eq!((le32(&f[8..]), le32(&f[4..])), (1, 0), "live H, M");
    assert_eq!(le32(&f[28..]), 0, "latched copy is untouched");
}

#[test]
fn short_save_warns_and_is_rewritten_full_size() {
    let dir = Dir::new("short");
    let rom_path = dir.path("g.gbc");
    fs::write(&rom_path, rom(0x10, IDLE)).unwrap();
    fs::write(dir.path("g.sav"), [9u8; 10]).unwrap();
    let out = headless(&rom_path, "2");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning"));
    let back = fs::read(dir.path("g.sav")).unwrap();
    assert_eq!(back.len(), 0x2000 + FOOTER);
    assert_eq!(&back[..10], &[9u8; 10]);
}

#[test]
fn cart_without_battery_gets_no_save() {
    let dir = Dir::new("nobatt");
    let rom_path = dir.path("p.gb");
    fs::write(&rom_path, rom(0x00, IDLE)).unwrap();
    assert!(headless(&rom_path, "2").status.success());
    assert!(!dir.path("p.sav").exists());
}

#[test]
fn bad_usage_exits_2() {
    for args in [&[][..], &["--screenshot"], &["--headless", "x.gb"]] {
        assert_eq!(run(args).status.code(), Some(2), "{args:?}");
    }
}

#[test]
fn unreadable_and_invalid_roms_exit_1() {
    let dir = Dir::new("bad");
    let missing = dir.path("nope.gb");
    let out = headless(&missing, "1");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("nope.gb"));
    let tiny = dir.path("tiny.gb");
    fs::write(&tiny, [0u8; 16]).unwrap();
    assert_eq!(headless(&tiny, "1").status.code(), Some(1));
}

/// A cartridge whose mapper the core does not emulate (review A4-1) is
/// refused with a message that names the mapper, and its `.sav` is
/// neither read into a run nor written: it used to run as "no MBC" with
/// always-enabled battery RAM and overwrite the save at exit.
#[test]
fn unsupported_mapper_exits_1_and_leaves_the_save_alone() {
    // HuC3, MBC7 (both "+BATTERY" types) and an unassigned code.
    for (cart_type, name) in [(0xFEu8, "HuC3"), (0x22, "MBC7"), (0x16, "unknown mapper")] {
        let dir = Dir::new(&format!("unsupported-{cart_type:02x}"));
        let rom_path = dir.path("game.gbc");
        // No enable register on a mapper-less cartridge: the write to
        // A000 would land in the save.
        fs::write(&rom_path, rom(cart_type, WRITER)).unwrap();
        let sav: Vec<u8> = (0..0x2000u32).map(|i| (i * 7) as u8).collect();
        fs::write(dir.path("game.sav"), &sav).unwrap();
        let out = headless(&rom_path, "5");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{cart_type:02X}: {stderr}");
        assert!(
            stderr.contains("game.gbc") && stderr.contains("unsupported cartridge"),
            "{stderr}"
        );
        assert!(stderr.contains(name), "{stderr}");
        assert!(stderr.contains(&format!("{cart_type:02X}")), "{stderr}");
        assert_eq!(fs::read(dir.path("game.sav")).unwrap(), sav);
        let mut files: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        files.sort();
        assert_eq!(files, ["game.gbc", "game.sav"]);
        // Without a save none is created either.
        fs::remove_file(dir.path("game.sav")).unwrap();
        assert_eq!(headless(&rom_path, "5").status.code(), Some(1));
        assert!(!dir.path("game.sav").exists());
    }
}

#[test]
fn locked_cpu_still_exits_0_with_a_warning() {
    let dir = Dir::new("lock");
    let rom_path = dir.path("l.gb");
    fs::write(&rom_path, rom(0x00, &[0xDD])).unwrap(); // undefined opcode
    let out = headless(&rom_path, "3");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("locked"));
}

#[test]
fn screenshot_writes_a_160x144_png() {
    let dir = Dir::new("shot");
    let rom_path = dir.path("s.gb");
    fs::write(&rom_path, rom(0x00, IDLE)).unwrap();
    let png_path = dir.path("out.png");
    let out = run(&[
        "--headless",
        "--frames",
        "3",
        "--screenshot",
        png_path.to_str().unwrap(),
        rom_path.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let png = fs::read(&png_path).expect("screenshot written");
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[8..16], &[0, 0, 0, 13, b'I', b'H', b'D', b'R']);
    assert_eq!(&png[16..24], &[0, 0, 0, 160, 0, 0, 0, 144]);
    assert_eq!(&png[24..29], &[8, 2, 0, 0, 0], "8-bit RGB");
    assert_eq!(&png[png.len() - 12..], b"\0\0\0\0IEND\xAE\x42\x60\x82");
    assert!(png.len() > 144 * (160 * 3 + 1), "raw pixel data present");
}

/// Live RTC seconds of the save a `--frames N` run leaves (RTC cart, no
/// prior save, so the clock starts at zero).
fn rtc_seconds_after(name: &str, frames: &str) -> u32 {
    let dir = Dir::new(name);
    let rom_path = dir.path("t.gbc");
    fs::write(&rom_path, rom(0x10, IDLE)).unwrap();
    assert!(headless(&rom_path, frames).status.success());
    let sav = fs::read(dir.path("t.sav")).unwrap();
    le32(&sav[0x2000..])
}

#[test]
fn frames_means_complete_frames() {
    // 70,224 clocks per frame, 4,194,304 per second, and an unnamed first
    // run of about 70,000 clocks to the first frame boundary: 58 frames
    // end under one second, 59 over. If that first run counted as a frame,
    // 59 would fall short.
    assert_eq!(rtc_seconds_after("f58", "58"), 0);
    assert_eq!(rtc_seconds_after("f59", "59"), 1);
    assert_eq!(rtc_seconds_after("f120", "120"), 2);
}

#[cfg(unix)]
#[test]
fn sigterm_still_writes_the_exit_save() {
    let dir = Dir::new("sigterm");
    let rom_path = dir.path("g.gbc");
    fs::write(&rom_path, rom(0x10, IDLE)).unwrap();
    // A save from an hour ago. The idle game never dirties it, so only the
    // exit save can refresh its time stamp.
    let mut sav = vec![0u8; 0x2000 + FOOTER];
    let old = now() - 3600;
    sav[0x2000 + 40..].copy_from_slice(&old.to_le_bytes());
    fs::write(dir.path("g.sav"), &sav).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_gbc-desktop"))
        .args(["--headless", "--frames", "1000000000"])
        .arg(&rom_path)
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .unwrap();
    assert!(status.success());
    // Bounded wait: a run that ignores the signal must fail, not hang.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let exit = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("still running 10 s after SIGTERM");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert_eq!(exit.code(), Some(0), "clean exit, not killed: {exit:?}");
    let back = fs::read(dir.path("g.sav")).unwrap();
    let stamp = u64::from_le_bytes(back[0x2000 + 40..].try_into().unwrap());
    assert!(stamp > old + 3000, "exit save rewrote the time stamp");
}
