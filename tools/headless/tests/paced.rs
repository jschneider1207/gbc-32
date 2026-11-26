//! `headless --paced`: the paced loop's compared lines agree with the
//! committed headless targets in `expected/` (same clock, the `fbs` chain
//! of their `fb=` values on FRAME_READY lines, the sample count), its
//! producer and consumer feed hashes agree, and nothing but `paced ` and
//! `stats ` lines is printed. ROMs as in `expected.rs`.

use std::path::PathBuf;
use std::process::Command;

use gbc_firmware::fnv;

fn rom_path(rel: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("GBC_TEST_ROMS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-roms"),
        PathBuf::from,
    );
    let path = dir.join(rel);
    if path.is_file() {
        return Some(path);
    }
    assert!(
        std::env::var("GBC_REQUIRE_TEST_DATA").as_deref() != Ok("1"),
        "{} missing (GBC_REQUIRE_TEST_DATA=1)",
        path.display()
    );
    eprintln!("SKIPPED: {} missing", path.display());
    None
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    line.split(' ')
        .find_map(|f| f.strip_prefix(key))
        .unwrap_or_else(|| panic!("no {key} in {line:?}"))
}

fn check(rel: &str, expected: &str) {
    let Some(rom) = rom_path(rel) else {
        return;
    };
    let out = Command::new(env!("CARGO_BIN_EXE_headless"))
        .arg(&rom)
        .args(["--frames", "600", "--paced"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(
        stdout
            .lines()
            .all(|l| l.starts_with("paced ") || l.starts_with("stats paced ")),
        "{stdout}"
    );
    let paced: Vec<&str> = stdout.lines().filter(|l| l.starts_with("paced ")).collect();
    assert_eq!(paced.len(), 3, "{stdout}");

    let want = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("expected")
            .join(expected),
    )
    .unwrap();
    let want: Vec<&str> = want.lines().collect();
    // Same header after the prefix and version.
    assert_eq!(
        paced[0].strip_prefix("paced v1 "),
        want[0].strip_prefix("headless v1 ")
    );
    let frames = &want[1..want.len() - 1];
    let mut chain = Vec::new();
    let mut ready = 0;
    for l in frames {
        if u16::from_str_radix(field(l, "ev="), 16).unwrap() & 1 != 0 {
            let fb = u64::from_str_radix(field(l, "fb="), 16).unwrap();
            chain.extend_from_slice(&fb.to_le_bytes());
            ready += 1;
        }
    }
    let end = paced[1];
    assert_eq!(field(end, "frames="), "600");
    assert_eq!(field(end, "clock="), field(frames[599], "clock="));
    assert_eq!(field(end, "ready="), ready.to_string());
    assert_eq!(field(end, "fbs="), format!("{:016x}", fnv::hash(&chain)));
    let audio = want[want.len() - 1];
    assert_eq!(field(end, "feed_frames="), field(audio, "samples="));
    // The consumer got exactly what the producer pushed.
    assert_eq!(
        paced[2].strip_prefix("paced drain "),
        Some(&end[end.find("feed_frames=").unwrap()..])
    );
}

#[test]
fn cpu_instrs_agrees_with_expected() {
    check("blargg/cpu_instrs/cpu_instrs.gb", "cpu_instrs.txt");
}

#[test]
fn cgb_acid2_agrees_with_expected() {
    check("cgb-acid2/cgb-acid2.gbc", "cgb-acid2.txt");
}
