//! The committed board targets in `tools/headless/expected/` are what this
//! tool prints now: the compared (`headless `) lines of a 600-frame run,
//! byte for byte. A core change that alters emulation shows up here; the
//! files are then regenerated (`headless <rom> --frames 600 | grep '^headless '
//! > expected/<name>.txt`) and the board must match the new ones.
//!
//! The ROMs come from `test-roms/` (`GBC_TEST_ROMS` overrides the
//! directory, for scratch worktrees); absent, the test prints SKIPPED and
//! passes, unless `GBC_REQUIRE_TEST_DATA=1`.

use std::path::PathBuf;
use std::process::Command;

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

fn check(rel: &str, expected: &str) {
    let Some(rom) = rom_path(rel) else {
        return;
    };
    let out = Command::new(env!("CARGO_BIN_EXE_headless"))
        .arg(&rom)
        .args(["--frames", "600"])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    let got: String = stdout
        .lines()
        .filter(|l| l.starts_with("headless "))
        .map(|l| format!("{l}\n"))
        .collect();
    let want = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("expected")
            .join(expected),
    )
    .unwrap();
    assert_eq!(want.lines().count(), 602);
    if got != want {
        let first = got
            .lines()
            .zip(want.lines())
            .find(|(g, w)| g != w)
            .map(|(g, w)| format!("got  {g}\nwant {w}"));
        panic!("{rel}: output differs from expected/{expected}\n{first:?}");
    }
    // Nothing but the compared lines and the stats lines.
    assert!(
        stdout
            .lines()
            .all(|l| l.starts_with("headless ") || l.starts_with("stats ")),
        "{stdout}"
    );
}

#[test]
fn cpu_instrs_matches_expected() {
    check("blargg/cpu_instrs/cpu_instrs.gb", "cpu_instrs.txt");
}

#[test]
fn cgb_acid2_matches_expected() {
    check("cgb-acid2/cgb-acid2.gbc", "cgb-acid2.txt");
}

#[test]
fn exit_codes() {
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_headless"))
            .args(args)
            .output()
            .unwrap()
            .status
            .code()
    };
    // Usage errors: 2, before any file is read.
    for args in [
        &[][..],
        &["x.gb"],
        &["x.gb", "--frames", "-1"],
        &["x.gb", "--frames"],
        &["x.gb", "--bogus"],
        &["x.gb", "y.gb", "--frames", "1"],
    ] {
        assert_eq!(run(args), Some(2), "{args:?}");
    }
    // An unreadable ROM: 1.
    assert_eq!(run(&["/nonexistent/x.gb", "--frames", "1"]), Some(1));
}
