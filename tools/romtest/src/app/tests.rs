//! End-to-end runner tests on a temporary workspace with a fake backend.
//! Each fake "ROM" is a text file whose words script the fake machine.

use std::ffi::OsString;

use gbc_core::Registers;

use super::*;
use crate::checks::{Rgb, encode_png};
use crate::cli::{self, Command};
use crate::machine::fake::{Action, FakeMachine};
use crate::machine::run as run_machine;
use crate::report::Status;

fn fake_backend(rom: &[u8], plan: &RunPlan, st: &mut RunState) -> Result<(), String> {
    let spec = std::str::from_utf8(rom).unwrap();
    let mut script = Vec::new();
    let mut regs = Registers::default();
    for word in spec.split_whitespace() {
        match word {
            "load-error" => return Err("bad header".into()),
            "panic" => script.push((100, Action::Panic)),
            "lock" => script.push((100, Action::Lock)),
            "ldbb" => script.push((1000, Action::LdBB)),
            "lcd-off" => script.push((10, Action::LcdOff)),
            "fib" => {
                regs = Registers {
                    b: 3,
                    c: 5,
                    d: 8,
                    e: 13,
                    h: 21,
                    l: 34,
                    ..Registers::default()
                }
            }
            w if w.starts_with("serial:") => {
                for (i, b) in w["serial:".len()..].bytes().enumerate() {
                    script.push((200 + 4 * i as u64, Action::Serial(b)));
                }
            }
            w => panic!("unknown fake word {w:?}"),
        }
    }
    let mut m = FakeMachine::new(script);
    m.regs = regs;
    run_machine(&mut m, plan, st);
    Ok(())
}

const MANIFEST: &str = r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "g-pass"
tier = "must"
roms = ["s/pass*.gb"]
exit = { ld_b_b = true, timeout_s = 1 }
checks = [{ fibonacci = true }]

[[group]]
id = "g-mixed"
tier = "must"
roms = ["s/**/*.gb"]
exclude = ["s/pass*"]
exit = { ld_b_b = true, timeout_s = 0.01 }
checks = [{ fibonacci = true }]

[[group]]
id = "g-shot"
tier = "should"
roms = ["t/*.{gb,gbc}"]
input = [{ at_s = 0.001, press = ["a"], hold_s = 0.001 }]
exit = { run_frames = 2 }
checks = [{ serial = "OK" }, { screenshot = "{dir}/{stem}.png" }]

[[group]]
id = "g-gambatte"
tier = "stretch"
roms = ["t/white.gb"]
exit = { run_frames = 1 }
checks = [{ gambatte = true }]
"#;

struct Ws {
    _dir: tempfile::TempDir,
    root: PathBuf,
    manifest: PathBuf,
}

impl Ws {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let ws = Ws {
            manifest: root.join("acceptance/m.toml"),
            root,
            _dir: dir,
        };
        ws.write("test-roms/.version", b"v7.0\n");
        ws.write("acceptance/m.toml", MANIFEST.as_bytes());
        ws.write("test-roms/s/pass1.gb", b"ldbb fib");
        ws.write("test-roms/s/timeout.gb", b"");
        ws.write("test-roms/s/lock.gb", b"lock");
        ws.write("test-roms/s/panic.gb", b"panic");
        ws.write("test-roms/s/sub/load.gb", b"load-error");
        ws.write("test-roms/s/fibfail.gb", b"ldbb");
        ws.write("test-roms/s/notes.txt", b"not a ROM");
        ws.write("test-roms/t/white.gb", b"lcd-off serial:OK");
        ws.write("test-roms/t/noref.gbc", b"lcd-off serial:OK");
        ws.write("test-roms/t/noref-panic.gbc", b"serial:OK panic");
        ws.write("test-roms/t/black.gb", b"lcd-off serial:NO");
        let img = |c: u8| Rgb {
            width: 160,
            height: 144,
            data: vec![[c; 3]; 160 * 144],
        };
        ws.write("test-roms/t/white.png", &encode_png(&img(255)).unwrap());
        ws.write("test-roms/t/black.png", &encode_png(&img(0)).unwrap());
        ws
    }

    fn write(&self, rel: &str, data: &[u8]) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn paths(&self) -> Paths {
        Paths::new(&self.root, &self.manifest).unwrap()
    }

    /// Runs with extra CLI arguments; returns (result, stdout).
    fn run(&self, extra: &[&str]) -> (Result<u8, String>, String) {
        let mut argv: Vec<OsString> = vec!["--manifest".into(), self.manifest.clone().into()];
        argv.extend(extra.iter().map(OsString::from));
        let args = match cli::parse(argv) {
            Ok(Command::Run(a)) => a,
            Ok(Command::Help) => unreachable!(),
            Err(e) => return (Err(e), String::new()),
        };
        let mut out = Vec::new();
        let r = run(&args, &self.paths(), &fake_backend, &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    fn json(&self) -> serde_json::Value {
        let text = std::fs::read_to_string(self.root.join("target/acceptance/m.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn statuses(&self) -> Vec<(String, String, String)> {
        self.json()["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r["group"].as_str().unwrap().to_owned(),
                    r["rom"].as_str().unwrap().to_owned(),
                    r["status"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

fn s(g: &str, r: &str, st: &str) -> (String, String, String) {
    (g.into(), r.into(), st.into())
}

#[test]
fn full_run_statuses_outputs_and_exit_code() {
    let ws = Ws::new();
    let (r, out) = ws.run(&["--gate", "must", "--jobs", "3"]);
    assert_eq!(r, Ok(1), "{out}");
    assert_eq!(
        ws.statuses(),
        [
            s("g-pass", "s/pass1.gb", "pass"),
            s("g-mixed", "s/fibfail.gb", "fail"),
            s("g-mixed", "s/lock.gb", "crash"),
            s("g-mixed", "s/panic.gb", "crash"),
            s("g-mixed", "s/sub/load.gb", "crash"),
            s("g-mixed", "s/timeout.gb", "timeout"),
            s("g-shot", "t/black.gb", "fail"),
            s("g-shot", "t/noref-panic.gbc", "no-reference"),
            s("g-shot", "t/noref.gbc", "no-reference"),
            s("g-shot", "t/white.gb", "pass"),
            s("g-gambatte", "t/white.gb", "unsupported"),
        ]
    );
    let json = ws.json();
    let results = json["results"].as_array().unwrap();
    let panic = &results[3];
    assert_eq!(panic["end"], "panic");
    assert!(
        panic["detail"]
            .as_str()
            .unwrap()
            .contains("fake emulator panic")
    );
    assert_eq!(panic["checks"][0]["status"], "skipped");
    let black = &results[6];
    assert_eq!(results[7]["end"], "panic");
    assert_eq!(results[7]["checks"][0]["status"], "skipped");
    assert_eq!(results[7]["checks"][1]["status"], "no-reference");
    assert_eq!(black["serial"], "NO");
    let shot = &black["checks"][1];
    assert_eq!(shot["mismatched_pixels"], 160 * 144);
    let actual = shot["actual_png"].as_str().unwrap();
    assert!(ws.root.join("target/acceptance").join(actual).is_file());
    assert!(
        ws.root
            .join("target/acceptance")
            .join(shot["diff_png"].as_str().unwrap())
            .is_file()
    );
    assert_eq!(results[5]["clock"], 41_943 / 4 * 4 + 4);
    assert_eq!(json["summary"]["tiers"]["must"]["pass"], 1);
    assert_eq!(json["gate"]["failures"], 5);

    let md = std::fs::read_to_string(ws.root.join("target/acceptance/m.md")).unwrap();
    for (_, rom, _) in ws.statuses() {
        assert!(md.contains(&format!("`{rom}`")), "{rom} missing from md");
    }
    assert!(out.contains("gate must: FAIL"));
    assert!(out.contains("slowest ROMs"));

    // Without --gate and without a baseline, nothing can regress: exit 0.
    assert_eq!(ws.run(&[]).0, Ok(0));
}

#[test]
fn results_do_not_depend_on_jobs() {
    let ws = Ws::new();
    let strip = |v: &mut serde_json::Value| {
        v["wall_time_s"] = 0.into();
        v["jobs"] = 0.into();
        for r in v["results"].as_array_mut().unwrap() {
            r["wall_time_s"] = 0.into();
        }
    };
    ws.run(&["--jobs", "1"]).0.unwrap();
    let mut a = ws.json();
    ws.run(&["--jobs", "8"]).0.unwrap();
    let mut b = ws.json();
    strip(&mut a);
    strip(&mut b);
    assert_eq!(a, b);
}

#[test]
fn baseline_create_regress_and_refuse() {
    let ws = Ws::new();
    let paths = ws.paths();
    assert!(!paths.baseline.exists());
    let (r, out) = ws.run(&["--update-baseline"]);
    assert_eq!(r, Ok(0), "{out}");
    let b = Baseline::parse(&std::fs::read_to_string(&paths.baseline).unwrap()).unwrap();
    assert_eq!(b.status("g-pass", "s/pass1.gb"), Some(Status::Pass));
    assert_eq!(b.status("g-mixed", "s/lock.gb"), Some(Status::Crash));
    assert_eq!(b.results.values().map(|m| m.len()).sum::<usize>(), 11);

    // Break a passing ROM: regression, exit 1, printed first.
    ws.write("test-roms/s/pass1.gb", b"ldbb");
    let (r, out) = ws.run(&[]);
    assert_eq!(r, Ok(1));
    let reg = out.find("REGRESSIONS (1)").expect(&out);
    assert!(reg < out.find("score:").unwrap());
    assert!(out.contains("g-pass s/pass1.gb: pass -> fail"));
    assert_eq!(ws.json()["regressions"][0]["now"], "fail");

    // Updating is refused while anything regressed; the baseline is kept.
    let before = std::fs::read_to_string(&paths.baseline).unwrap();
    let (r, out) = ws.run(&["--update-baseline"]);
    assert_eq!(r, Ok(1));
    assert!(out.contains("refusing to update the baseline"));
    assert_eq!(std::fs::read_to_string(&paths.baseline).unwrap(), before);

    // Partial runs compare only what ran.
    assert_eq!(ws.run(&["--group", "g-mixed"]).0, Ok(0));
    assert_eq!(ws.run(&["--group", "g-pass"]).0, Ok(1));

    // Fixed again: no regression; a fresh update succeeds.
    ws.write("test-roms/s/pass1.gb", b"ldbb fib");
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));

    // A corrupt baseline is a runner error.
    std::fs::write(&paths.baseline, "{").unwrap();
    assert!(ws.run(&[]).0.is_err());
}

#[test]
fn update_baseline_with_selection_is_a_usage_error() {
    let ws = Ws::new();
    for extra in [
        &["--update-baseline", "--group", "g-pass"][..],
        &["--update-baseline", "--filter", "s/**"][..],
        &["--update-baseline", "--only-check", "serial"][..],
    ] {
        assert!(ws.run(extra).0.unwrap_err().contains("refused"));
    }
    assert!(!ws.paths().baseline.exists());
}

#[test]
fn gate_passes_on_a_passing_selection() {
    let ws = Ws::new();
    assert_eq!(ws.run(&["--group", "g-pass", "--gate", "must"]).0, Ok(0));
    // A gate tier with nothing selected is a usage error, not a vacuous pass.
    assert!(
        ws.run(&["--group", "g-pass", "--gate", "should"])
            .0
            .is_err()
    );
}

#[test]
fn filter_selects_every_group_containing_the_path() {
    let ws = Ws::new();
    let (r, _) = ws.run(&["--filter", "t/white.gb"]);
    assert_eq!(r, Ok(0));
    assert_eq!(
        ws.statuses(),
        [
            s("g-shot", "t/white.gb", "pass"),
            s("g-gambatte", "t/white.gb", "unsupported")
        ]
    );
    ws.run(&["--filter", "s/{pass1,sub/load}.gb", "--group", "g-mixed"])
        .0
        .unwrap();
    assert_eq!(ws.statuses(), [s("g-mixed", "s/sub/load.gb", "crash")]);
    assert!(
        ws.run(&["--filter", "nothing/**"])
            .0
            .unwrap_err()
            .contains("matches no ROMs")
    );
}

#[test]
fn only_check_evaluates_one_kind() {
    let ws = Ws::new();
    ws.run(&["--only-check", "serial"]).0.unwrap();
    // Only g-shot has a serial check; the missing PNG no longer matters.
    assert_eq!(
        ws.statuses(),
        [
            s("g-shot", "t/black.gb", "fail"),
            s("g-shot", "t/noref-panic.gbc", "crash"),
            s("g-shot", "t/noref.gbc", "pass"),
            s("g-shot", "t/white.gb", "pass"),
        ]
    );
    let json = ws.json();
    assert_eq!(json["results"][0]["checks"].as_array().unwrap().len(), 1);
}

#[test]
fn runner_errors() {
    let ws = Ws::new();
    assert!(
        ws.run(&["--group", "nope"])
            .0
            .unwrap_err()
            .contains("no such group")
    );

    ws.write("test-roms/.version", b"v6.0\n");
    assert!(ws.run(&[]).0.unwrap_err().contains("v6.0"));
    ws.write("test-roms/.version", b"v7.0\n");

    let bad = MANIFEST.replace("roms = [\"t/white.gb\"]", "roms = [\"t/missing.gb\"]");
    ws.write("acceptance/m.toml", bad.as_bytes());
    assert!(ws.run(&[]).0.unwrap_err().contains("not found"));

    let bad = MANIFEST.replace("id = \"g-gambatte\"", "id = \"g-shot\"");
    ws.write("acceptance/m.toml", bad.as_bytes());
    assert!(ws.run(&[]).0.unwrap_err().contains("duplicate"));

    let bad = MANIFEST.replace("roms = [\"s/pass*.gb\"]", "roms = [\"q/*.gb\"]");
    ws.write("acceptance/m.toml", bad.as_bytes());
    assert!(ws.run(&[]).0.unwrap_err().contains("matches no ROMs"));

    let missing = ws.root.join("acceptance/none.toml");
    let args = cli::Args {
        manifest: missing.clone(),
        ..Default::default()
    };
    let paths = Paths::new(&ws.root, &missing).unwrap();
    assert!(run(&args, &paths, &fake_backend, &mut Vec::new()).is_err());
}

#[test]
fn failure_images_are_cleared_between_runs() {
    let ws = Ws::new();
    ws.run(&[]).0.unwrap();
    let failed = ws.root.join("target/acceptance/m/failed");
    assert!(failed.is_dir());
    ws.write("test-roms/t/black.gb", b"lcd-off serial:OK");
    ws.write(
        "test-roms/t/black.png",
        &std::fs::read(ws.root.join("test-roms/t/white.png")).unwrap(),
    );
    ws.run(&[]).0.unwrap();
    assert!(!failed.exists());
}

#[test]
fn panic_capture_returns_message() {
    let r: Result<(), String> = catch_panic(|| panic!("boom {}", 7));
    assert!(r.unwrap_err().contains("boom 7"));
    assert_eq!(catch_panic(|| 5), Ok(5));
}
