//! End-to-end runner tests on a temporary workspace with a fake backend.
//! Each fake "ROM" is a text file whose words script the fake machine.

use std::ffi::OsString;

use gbc_core::Registers;

use super::*;
use crate::checks::{Rgb, encode_png};
use crate::cli::{self, Command};
use crate::integrity;
use crate::machine::fake::{Action, FakeMachine};
use crate::machine::run as run_machine;
use crate::report::Status;

/// Words: `load-error`, `panic` (inside the fake core), `runner-panic`
/// (in the runner's own code, outside the panic boundary), `lock`, `ldbb`,
/// `lcd-off`, `fib`, `serial:<text>`.
fn fake_backend(rom: &[u8], plan: &RunPlan, st: &mut RunState) -> Result<(), CoreFailure> {
    let spec = std::str::from_utf8(rom).unwrap();
    let mut script = Vec::new();
    let mut regs = Registers::default();
    for word in spec.split_whitespace() {
        match word {
            "load-error" => return Err(CoreFailure::Load("bad header".into())),
            "runner-panic" => panic!("runner bug"),
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
    run_machine(&mut m, plan, st)
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
        ws.write_raw("test-roms/.version", b"v7.0\n");
        ws.write_raw("acceptance/m.toml", MANIFEST.as_bytes());
        ws.write_raw("test-roms/s/pass1.gb", b"ldbb fib");
        ws.write_raw("test-roms/s/pass2.gb", b"ldbb fib");
        ws.write_raw("test-roms/s/timeout.gb", b"");
        ws.write_raw("test-roms/s/lock.gb", b"lock");
        ws.write_raw("test-roms/s/panic.gb", b"panic");
        ws.write_raw("test-roms/s/sub/load.gb", b"load-error");
        ws.write_raw("test-roms/s/fibfail.gb", b"ldbb");
        ws.write_raw("test-roms/s/notes.txt", b"not a ROM");
        ws.write_raw("test-roms/t/white.gb", b"lcd-off serial:OK");
        ws.write_raw("test-roms/t/noref.gbc", b"lcd-off serial:OK");
        ws.write_raw("test-roms/t/noref-fail.gbc", b"lcd-off serial:NO");
        ws.write_raw("test-roms/t/noref-panic.gbc", b"serial:OK panic");
        ws.write_raw("test-roms/t/black.gb", b"lcd-off serial:NO");
        ws.write_raw("test-roms/t/white.png", &encode_png(&img(255)).unwrap());
        ws.write_raw("test-roms/t/black.png", &encode_png(&img(0)).unwrap());
        // Outside every group of MANIFEST; used by other manifests.
        ws.write_raw("test-roms/r/runner.gb", b"ldbb fib runner-panic");
        ws.write_raw("test-roms/r/core.gb", b"ldbb fib panic");
        ws.write_raw("test-roms/root.gb", b"lcd-off");
        ws.write_raw("test-roms/root.png", &encode_png(&img(255)).unwrap());
        ws.rehash();
        ws
    }

    /// Writes a file and, for test-roms/, regenerates the integrity list
    /// (as if upstream had shipped the file).
    fn write(&self, rel: &str, data: &[u8]) {
        self.write_raw(rel, data);
        if rel.starts_with("test-roms/") {
            self.rehash();
        }
    }

    /// Writes a file without touching the integrity list (tampering).
    fn write_raw(&self, rel: &str, data: &[u8]) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }

    fn rehash(&self) {
        let list = integrity::generate(&self.root.join("test-roms"), "v7.0");
        self.write_raw("acceptance/test-roms.sha256", list.render().as_bytes());
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

    /// Runs the whole program as `main` does (argv as given, relative
    /// manifest paths resolved against the workspace); returns (exit code,
    /// stdout, stderr).
    fn main(&self, argv: &[&str]) -> (u8, String, String) {
        let argv: Vec<OsString> = argv
            .iter()
            .map(|a| match a.strip_prefix("ws:") {
                Some(rel) => self.root.join(rel).into_os_string(),
                None => OsString::from(a),
            })
            .collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = main_with(argv, &self.root, &fake_backend, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn json_of(&self, stem: &str) -> serde_json::Value {
        let path = self.root.join(format!("target/acceptance/{stem}.json"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn json(&self) -> serde_json::Value {
        self.json_of("m")
    }

    fn statuses(&self) -> Vec<(String, String, String)> {
        statuses_of(&self.json())
    }

    fn baseline(&self) -> Baseline {
        Baseline::parse(&std::fs::read_to_string(self.paths().baseline).unwrap()).unwrap()
    }
}

fn img(c: u8) -> Rgb {
    Rgb {
        width: 160,
        height: 144,
        data: vec![[c; 3]; 160 * 144],
    }
}

fn statuses_of(json: &serde_json::Value) -> Vec<(String, String, String)> {
    json["results"]
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

fn s(g: &str, r: &str, st: &str) -> (String, String, String) {
    (g.into(), r.into(), st.into())
}

#[test]
fn full_run_statuses_outputs_and_exit_code() {
    let ws = Ws::new();
    // Without a baseline nothing can regress, and there is no gate: exit 0.
    let (r, out) = ws.run(&["--jobs", "3"]);
    assert_eq!(r, Ok(0), "{out}");
    assert_eq!(
        ws.statuses(),
        [
            s("g-pass", "s/pass1.gb", "pass"),
            s("g-pass", "s/pass2.gb", "pass"),
            s("g-mixed", "s/fibfail.gb", "fail"),
            s("g-mixed", "s/lock.gb", "crash"),
            s("g-mixed", "s/panic.gb", "crash"),
            s("g-mixed", "s/sub/load.gb", "crash"),
            s("g-mixed", "s/timeout.gb", "timeout"),
            s("g-shot", "t/black.gb", "fail"),
            // Missing PNG, but the serial check fails: fail, not excluded.
            s("g-shot", "t/noref-fail.gbc", "fail"),
            // Missing PNG and a core panic: crash, not excluded.
            s("g-shot", "t/noref-panic.gbc", "crash"),
            // Missing PNG, everything else passes: no-reference.
            s("g-shot", "t/noref.gbc", "no-reference"),
            s("g-shot", "t/white.gb", "pass"),
            s("g-gambatte", "t/white.gb", "unsupported"),
        ]
    );
    assert!(
        out.contains("romtest: verified 20 test-roms files"),
        "{out}"
    );
    let json = ws.json();
    let results = json["results"].as_array().unwrap();
    let panic = &results[4];
    assert_eq!(panic["end"], "panic");
    assert!(
        panic["detail"]
            .as_str()
            .unwrap()
            .contains("fake emulator panic")
    );
    assert_eq!(panic["checks"][0]["status"], "skipped");
    assert_eq!(results[5]["end"], "load_error");
    assert_eq!(results[5]["detail"], "load error: bad header");
    let black = &results[7];
    assert_eq!(results[8]["checks"][0]["status"], "fail");
    assert_eq!(results[8]["checks"][1]["status"], "no-reference");
    assert_eq!(results[9]["end"], "panic");
    assert_eq!(results[9]["checks"][0]["status"], "skipped");
    assert_eq!(results[9]["checks"][1]["status"], "no-reference");
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
    assert_eq!(results[6]["clock"], 41_943 / 4 * 4 + 4);
    assert_eq!(json["summary"]["tiers"]["must"]["pass"], 2);
    assert_eq!(json["summary"]["tiers"]["should"]["no_reference"], 1);

    let md = std::fs::read_to_string(ws.root.join("target/acceptance/m.md")).unwrap();
    for (_, rom, _) in ws.statuses() {
        assert!(md.contains(&format!("`{rom}`")), "{rom} missing from md");
    }
    assert!(out.contains("slowest ROMs"));

    // With a baseline, the must gate fails on the five g-mixed ROMs.
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));
    let (r, out) = ws.run(&["--gate", "must"]);
    assert_eq!(r, Ok(1), "{out}");
    assert_eq!(ws.json()["gate"]["failures"], 5);
    assert!(out.contains("gate must: FAIL"));
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
    let b = ws.baseline();
    assert_eq!(b.roms_release, "v7.0");
    assert_eq!(b.status("g-pass", "s/pass1.gb"), Some(Status::Pass));
    assert_eq!(b.status("g-mixed", "s/lock.gb"), Some(Status::Crash));
    assert_eq!(b.results.values().map(|m| m.len()).sum::<usize>(), 13);

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

    // Partial runs compare only their selection.
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
fn baseline_keys_without_a_result() {
    let ws = Ws::new();
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));

    // The manifest stops selecting a passing ROM (here an exclude; a
    // deleted ROM is caught earlier by the integrity list): regression.
    let m = MANIFEST.replace(
        "roms = [\"s/pass*.gb\"]",
        "roms = [\"s/pass*.gb\"]\nexclude = [\"s/pass1.gb\"]",
    );
    ws.write("acceptance/m.toml", m.as_bytes());
    let (r, out) = ws.run(&[]);
    assert_eq!(r, Ok(1), "{out}");
    assert!(out.contains("g-pass s/pass1.gb: pass -> missing"), "{out}");
    assert_eq!(ws.json()["regressions"][0]["now"], "missing");
    // In scope of a partial run only if its selection covers the key.
    assert_eq!(ws.run(&["--filter", "s/pass*.gb"]).0, Ok(1));
    assert_eq!(ws.run(&["--group", "g-pass"]).0, Ok(1));
    assert_eq!(ws.run(&["--only-check", "fibonacci"]).0, Ok(1));
    assert_eq!(ws.run(&["--filter", "s/pass2.gb"]).0, Ok(0));
    assert_eq!(ws.run(&["--group", "g-mixed"]).0, Ok(0));
    assert_eq!(ws.run(&["--only-check", "serial"]).0, Ok(0));
    assert!(
        ws.run(&["--update-baseline"])
            .1
            .contains("refusing to update")
    );

    // A renamed group: its passing keys are missing too, whatever the
    // --only-check kind (the old group's checks are unknown).
    let m = MANIFEST.replace("id = \"g-pass\"", "id = \"g-ok\"");
    ws.write("acceptance/m.toml", m.as_bytes());
    let (r, out) = ws.run(&["--only-check", "serial"]);
    assert_eq!(r, Ok(1), "{out}");
    assert!(out.contains("REGRESSIONS (2)"), "{out}");

    // In a full run, a non-passing entry that disappears is a regression
    // too (also under --gate), and --update-baseline refuses it.
    let m = MANIFEST.replace(
        "exclude = [\"s/pass*\"]",
        "exclude = [\"s/pass*\", \"s/lock.gb\"]",
    );
    ws.write("acceptance/m.toml", m.as_bytes());
    let (r, out) = ws.run(&[]);
    assert_eq!(r, Ok(1), "{out}");
    assert!(out.contains("REGRESSIONS (1)"), "{out}");
    assert!(out.contains("g-mixed s/lock.gb: crash -> missing"), "{out}");
    let reg = &ws.json()["regressions"][0];
    assert_eq!(
        (&reg["was"], &reg["now"]),
        (&"crash".into(), &"missing".into())
    );
    assert!(
        ws.run(&["--gate", "must"])
            .1
            .contains("g-mixed s/lock.gb: crash -> missing")
    );
    let (r, out) = ws.run(&["--update-baseline"]);
    assert_eq!(r, Ok(1), "{out}");
    assert!(out.contains("refusing to update"), "{out}");
    assert_eq!(
        ws.baseline().status("g-mixed", "s/lock.gb"),
        Some(Status::Crash)
    );
    // A partial run only answers for in-scope passing entries.
    assert_eq!(ws.run(&["--group", "g-mixed"]).0, Ok(0));
    assert_eq!(ws.run(&["--filter", "s/{lock,timeout}.gb"]).0, Ok(0));
    // Only --allow-removed drops it, listing every removed key.
    let (r, out) = ws.run(&["--update-baseline", "--allow-removed"]);
    assert_eq!(r, Ok(0), "{out}");
    assert!(out.contains("removing 1 baseline entry"), "{out}");
    assert!(out.contains("  g-mixed s/lock.gb (was crash)"), "{out}");
    assert!(out.contains("baseline updated"), "{out}");
    assert_eq!(ws.json()["removed_from_baseline"][0]["rom"], "s/lock.gb");
    assert_eq!(ws.baseline().status("g-mixed", "s/lock.gb"), None);
    assert_eq!(ws.run(&[]).0, Ok(0));
    assert_eq!(
        ws.baseline()
            .results
            .values()
            .map(|m| m.len())
            .sum::<usize>(),
        12
    );
}

#[test]
fn gate_without_a_baseline_is_a_runner_error() {
    let ws = Ws::new();
    let e = ws.run(&["--gate", "must"]).0.unwrap_err();
    assert!(e.contains("no baseline"), "{e}");
    assert!(e.contains("--update-baseline"), "{e}");
    assert!(!ws.paths().baseline.exists());
    // --update-baseline still creates it (and then gates).
    assert_eq!(ws.run(&["--gate", "must", "--update-baseline"]).0, Ok(1));
    assert!(ws.paths().baseline.exists());
    assert_eq!(ws.run(&["--group", "g-pass", "--gate", "must"]).0, Ok(0));
}

#[test]
fn baseline_release_must_match() {
    let ws = Ws::new();
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));
    let path = ws.paths().baseline;
    let old = std::fs::read_to_string(&path)
        .unwrap()
        .replace("\"v7.0\"", "\"v6.0\"");
    std::fs::write(&path, &old).unwrap();
    let e = ws.run(&[]).0.unwrap_err();
    assert!(
        e.contains("\"v6.0\"") && e.contains("--update-baseline"),
        "{e}"
    );
    let (r, out) = ws.run(&["--update-baseline"]);
    assert_eq!(r, Ok(0), "{out}");
    assert!(out.contains("rewriting it"), "{out}");
    assert_eq!(ws.baseline().roms_release, "v7.0");
    assert_eq!(ws.run(&[]).0, Ok(0));
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
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));
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
            s("g-shot", "t/noref-fail.gbc", "fail"),
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

    ws.write_raw("test-roms/.version", b"v6.0\n");
    assert!(ws.run(&[]).0.unwrap_err().contains("v6.0"));
    ws.write_raw("test-roms/.version", b"v7.0\n");

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
fn integrity_list_catches_missing_and_changed_files() {
    let ws = Ws::new();
    assert_eq!(ws.run(&[]).0, Ok(0));
    let roms = ws.root.join("test-roms");
    let expect_err = |needle: &str| {
        let e = ws.run(&[]).0.unwrap_err();
        assert!(e.contains(needle), "{needle:?} not in {e}");
        // Also under a selection that doesn't include the file.
        let e = ws.run(&["--group", "g-gambatte"]).0.unwrap_err();
        assert!(e.contains(needle), "{needle:?} not in {e}");
    };

    // A deleted failing ROM would otherwise just stop matching its glob.
    std::fs::rename(roms.join("s/fibfail.gb"), roms.join("fibfail.bak")).unwrap();
    expect_err("missing from");
    std::fs::rename(roms.join("fibfail.bak"), roms.join("s/fibfail.gb")).unwrap();
    assert_eq!(ws.run(&[]).0, Ok(0));

    // A deleted reference PNG would otherwise turn a failure into
    // no-reference.
    std::fs::remove_file(roms.join("t/black.png")).unwrap();
    expect_err("\"t/black.png\"");
    ws.write_raw("test-roms/t/black.png", &encode_png(&img(0)).unwrap());
    assert_eq!(ws.run(&[]).0, Ok(0));

    // Changed contents.
    ws.write_raw("test-roms/t/black.png", &encode_png(&img(255)).unwrap());
    expect_err("don't match their SHA-256");
    ws.write_raw("test-roms/t/black.png", &encode_png(&img(0)).unwrap());

    // A new ROM matched by a glob but not listed.
    ws.write_raw("test-roms/s/new.gb", b"ldbb fib");
    expect_err("\"s/new.gb\"");
    std::fs::remove_file(roms.join("s/new.gb")).unwrap();

    // A reference PNG that appears but isn't listed is not trusted.
    ws.write_raw("test-roms/t/noref.png", &encode_png(&img(255)).unwrap());
    let e = ws.run(&[]).0.unwrap_err();
    assert!(e.contains("not in the test-roms integrity list"), "{e}");
    std::fs::remove_file(roms.join("t/noref.png")).unwrap();

    // The list belongs to another release.
    let list_path = ws.paths().rom_hashes;
    let list = std::fs::read_to_string(&list_path).unwrap();
    std::fs::write(
        &list_path,
        list.replace("# release: v7.0", "# release: v6.0"),
    )
    .unwrap();
    expect_err("is for release \"v6.0\"");

    // No list at all.
    std::fs::remove_file(&list_path).unwrap();
    expect_err("integrity list");
    std::fs::write(&list_path, list).unwrap();
    assert_eq!(ws.run(&[]).0, Ok(0));
}

#[test]
fn roms_dir_override_is_verified_too() {
    // The --roms-dir copy with one ROM deleted is caught: exit 2.
    let ws = Ws::new();
    let copy = tempfile::tempdir().unwrap();
    for f in crate::manifest::list_files(&ws.root.join("test-roms")).unwrap() {
        let to = copy.path().join(&f);
        std::fs::create_dir_all(to.parent().unwrap()).unwrap();
        std::fs::copy(ws.root.join("test-roms").join(&f), to).unwrap();
    }
    let dir = copy.path().to_str().unwrap();
    let (code, _, err) = ws.main(&["--manifest", "ws:acceptance/m.toml", "--roms-dir", dir]);
    assert_eq!(code, 0, "{err}");
    std::fs::remove_file(copy.path().join("s/fibfail.gb")).unwrap();
    let (code, _, err) = ws.main(&["--manifest", "ws:acceptance/m.toml", "--roms-dir", dir]);
    assert_eq!(code, 2);
    assert!(err.contains("\"s/fibfail.gb\""), "{err}");
}

#[test]
fn root_dir_roms_use_a_relative_reference() {
    let ws = Ws::new();
    let m = r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "root"
tier = "must"
roms = ["*.gb"]
exit = { run_frames = 1 }
checks = [{ screenshot = "{dir}/{stem}.png" }]
"#;
    ws.write("acceptance/root.toml", m.as_bytes());
    let (code, out, err) = ws.main(&["--manifest", "ws:acceptance/root.toml"]);
    assert_eq!(code, 0, "{out}{err}");
    let json = ws.json_of("root");
    assert_eq!(statuses_of(&json), [s("root", "root.gb", "pass")]);
    assert_eq!(json["results"][0]["checks"][0]["reference"], "root.png");
}

#[test]
fn failure_images_are_cleared_between_runs() {
    let ws = Ws::new();
    ws.run(&[]).0.unwrap();
    let failed = ws.root.join("target/acceptance/m/failed");
    assert!(failed.is_dir());
    ws.write("test-roms/t/black.gb", b"lcd-off serial:OK");
    ws.write("test-roms/t/noref-fail.gbc", b"lcd-off serial:OK");
    ws.write(
        "test-roms/t/black.png",
        &std::fs::read(ws.root.join("test-roms/t/white.png")).unwrap(),
    );
    ws.run(&[]).0.unwrap();
    assert!(!failed.exists());
}

const PANIC_MANIFEST: &str = r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "p"
tier = "must"
roms = ["r/ROM.gb"]
exit = { ld_b_b = true, timeout_s = 1 }
checks = [{ fibonacci = true }]
"#;

#[test]
fn main_exit_codes() {
    let ws = Ws::new();
    // Usage errors are runner errors.
    let (code, _, err) = ws.main(&["--manifest", "ws:acceptance/m.toml", "--bogus"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown argument"), "{err}");
    assert_eq!(ws.main(&[]).0, 2);
    let (code, out, _) = ws.main(&["--help"]);
    assert_eq!(code, 0);
    assert!(out.contains("usage: romtest"));
    // Help with anything else is a usage error, not a silent exit 0.
    let (code, out, err) = ws.main(&["--manifest", "ws:acceptance/m.toml", "-h"]);
    assert_eq!(code, 2);
    assert!(out.is_empty() && err.contains("only argument"), "{err}");
    // A runner error (missing manifest).
    assert_eq!(ws.main(&["--manifest", "ws:acceptance/none.toml"]).0, 2);
    // Ran: exit 0 without regressions, 1 with gate failures.
    assert_eq!(ws.main(&["--manifest", "ws:acceptance/m.toml"]).0, 0);
    let (code, _, _) = ws.main(&["--manifest", "ws:acceptance/m.toml", "--update-baseline"]);
    assert_eq!(code, 0);
    assert_eq!(
        ws.main(&["--manifest", "ws:acceptance/m.toml", "--gate", "must"])
            .0,
        1
    );
}

#[test]
fn panics_in_the_core_crash_but_panics_in_the_runner_exit_2() {
    let ws = Ws::new();
    // A panic inside the (fake) core is that ROM's crash.
    ws.write(
        "acceptance/p.toml",
        PANIC_MANIFEST.replace("ROM", "core").as_bytes(),
    );
    let (code, out, err) = ws.main(&["--manifest", "ws:acceptance/p.toml", "--jobs", "2"]);
    assert_eq!(code, 0, "{out}{err}");
    assert_eq!(
        statuses_of(&ws.json_of("p")),
        [s("p", "r/core.gb", "crash")]
    );

    // A panic in the runner's own code, outside the core boundary, is a
    // runner bug: exit 2, never a ROM status.
    ws.write(
        "acceptance/p.toml",
        PANIC_MANIFEST.replace("ROM", "runner").as_bytes(),
    );
    let (code, _, err) = ws.main(&["--manifest", "ws:acceptance/p.toml", "--jobs", "2"]);
    assert_eq!(code, 2);
    assert!(err.contains("internal error"), "{err}");
}

#[test]
fn allow_removed_keeps_real_regressions() {
    // --allow-removed drops missing entries but still refuses a pass that
    // now fails.
    let ws = Ws::new();
    assert_eq!(ws.run(&["--update-baseline"]).0, Ok(0));
    let m = MANIFEST.replace(
        "exclude = [\"s/pass*\"]",
        "exclude = [\"s/pass*\", \"s/lock.gb\"]",
    );
    ws.write("acceptance/m.toml", m.as_bytes());
    ws.write("test-roms/s/pass1.gb", b"ldbb");
    let (r, out) = ws.run(&["--update-baseline", "--allow-removed"]);
    assert_eq!(r, Ok(1), "{out}");
    assert!(out.contains("REGRESSIONS (1)"), "{out}");
    assert!(out.contains("g-pass s/pass1.gb: pass -> fail"), "{out}");
    assert!(out.contains("refusing to update"), "{out}");
    assert_eq!(
        ws.baseline().status("g-mixed", "s/lock.gb"),
        Some(Status::Crash)
    );
}

#[test]
fn missing_reference_does_not_hide_an_unsupported_check() {
    let ws = Ws::new();
    let m = r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "u"
tier = "must"
roms = ["t/noref.gbc"]
exit = { run_frames = 1 }
checks = [{ screenshot = "{dir}/{stem}.png" }, { gambatte = true }]
"#;
    ws.write("acceptance/u.toml", m.as_bytes());
    let (code, out, err) = ws.main(&["--manifest", "ws:acceptance/u.toml"]);
    assert_eq!(code, 0, "{out}{err}");
    let json = ws.json_of("u");
    assert_eq!(statuses_of(&json), [s("u", "t/noref.gbc", "unsupported")]);
    assert_eq!(json["results"][0]["checks"][0]["status"], "no-reference");
    assert_eq!(json["summary"]["tiers"]["must"]["unsupported"], 1);
}

const TAMPER_MANIFEST: &str = r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "rom"
tier = "must"
roms = ["s/pass1.gb", "s/pass2.gb"]
exit = { ld_b_b = true, timeout_s = 1 }
checks = [{ fibonacci = true }]

[[group]]
id = "png"
tier = "should"
roms = ["t/white.gb"]
exit = { run_frames = 2 }
checks = [{ screenshot = "{dir}/{stem}.png" }]
"#;

#[test]
fn files_changed_after_the_upfront_check_are_caught_at_load() {
    let ws = Ws::new();
    ws.write("acceptance/tamper.toml", TAMPER_MANIFEST.as_bytes());
    let manifest = ws.root.join("acceptance/tamper.toml");
    let paths = Paths::new(&ws.root, &manifest).unwrap();
    let rom2 = ws.root.join("test-roms/s/pass2.gb");
    let png = ws.root.join("test-roms/t/white.png");
    let first = AtomicBool::new(true);
    let run_with = |group: &str, backend: &Backend<'_>| {
        let args = cli::Args {
            manifest: manifest.clone(),
            groups: vec![group.into()],
            jobs: Some(1),
            ..Default::default()
        };
        let mut out = Vec::new();
        let r = run(&args, &paths, backend, &mut out);
        (r, String::from_utf8(out).unwrap())
    };

    // Untouched: every loaded file is hashed again and matches.
    let (r, out) = run_with("rom", &fake_backend);
    assert_eq!(r, Ok(0), "{out}");
    assert!(out.contains("re-verified 2 loaded files"), "{out}");
    let (r, out) = run_with("png", &fake_backend);
    assert_eq!(r, Ok(0), "{out}");
    assert!(out.contains("re-verified 2 loaded files"), "{out}");

    // A ROM rewritten while another runs (after the up-front check, before
    // its own load): runner error. Jobs run in order with --jobs 1.
    let tamper_rom = |rom: &[u8], plan: &RunPlan, st: &mut RunState| {
        if first.swap(false, Ordering::Relaxed) {
            std::fs::write(&rom2, b"ldbb fib ").unwrap();
        }
        fake_backend(rom, plan, st)
    };
    let e = run_with("rom", &tamper_rom).0.unwrap_err();
    assert!(
        e.contains("s/pass2.gb changed after the integrity check"),
        "{e}"
    );
    std::fs::write(&rom2, b"ldbb fib").unwrap();

    // A reference PNG rewritten during the run, before the check reads it.
    let tamper_png = |rom: &[u8], plan: &RunPlan, st: &mut RunState| {
        std::fs::write(&png, encode_png(&img(254)).unwrap()).unwrap();
        fake_backend(rom, plan, st)
    };
    let e = run_with("png", &tamper_png).0.unwrap_err();
    assert!(
        e.contains("reference t/white.png changed after the integrity check"),
        "{e}"
    );
}

/// The committed reference overrides of a89713c were removed (ADR 0007,
/// 2026-11-22 amendment): their manifest form is a bad manifest (exit 2),
/// even with a listed PNG at that path that would pass.
#[test]
fn reference_override_form_is_rejected() {
    let ws = Ws::new();
    let white = encode_png(&img(255)).unwrap();
    ws.write("test-roms/references/x/white.png", &white);
    use sha2::{Digest, Sha256};
    let sha = integrity::to_hex(&Sha256::digest(&white).into());
    let m = format!(
        r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "pinned"
tier = "must"
roms = ["t/white.gb"]
exit = {{ run_frames = 1 }}
checks = [{{ screenshot = "references/x/white.png", sha256 = "{sha}" }}]
"#
    );
    ws.write("acceptance/p.toml", m.as_bytes());
    let (code, out, err) = ws.main(&["--manifest", "ws:acceptance/p.toml", "--jobs", "1"]);
    assert_eq!(code, 2, "{out}{err}");
    assert!(err.contains("unknown field `sha256`"), "{err}");
}

/// A counted recolor (acceptance/README.md, "Checks"): the listed upstream
/// PNG is recolored in memory, and anything but the exact count (or a
/// recolor the contract doesn't allow) is a runner error (exit 2).
mod recolor {
    use super::*;

    const UPSTREAM: &str = "x/ref.png";
    /// Pixels of `FROM` in the upstream reference; the rest is white.
    const N: usize = 100;

    /// A manifest with one group (`t/white.gb`, a white frame) checking
    /// against `UPSTREAM` with this recolor, plus an unrelated group.
    fn manifest(recolor: &str) -> String {
        format!(
            r#"
roms_release = "v7.0"
model = "cgb-c"

[[group]]
id = "recolored"
tier = "must"
roms = ["t/white.gb"]
exit = {{ run_frames = 1 }}
checks = [{{ screenshot = "{UPSTREAM}", recolor = {recolor} }}]

[[group]]
id = "other"
tier = "must"
roms = ["t/black.gb"]
exit = {{ run_frames = 1 }}
checks = [{{ serial = "NO" }}]
"#
        )
    }

    /// White, except the first `N` pixels are #7BFF4A.
    fn upstream() -> Vec<u8> {
        let mut i = img(255);
        i.data[..N].fill([0x7B, 0xFF, 0x4A]);
        encode_png(&i).unwrap()
    }

    fn setup(recolor: &str) -> Ws {
        let ws = Ws::new();
        ws.write(&format!("test-roms/{UPSTREAM}"), &upstream());
        ws.write("acceptance/r.toml", manifest(recolor).as_bytes());
        ws
    }

    fn go(ws: &Ws, extra: &[&str]) -> (u8, String, String) {
        let mut argv = vec!["--manifest", "ws:acceptance/r.toml", "--jobs", "1"];
        argv.extend_from_slice(extra);
        ws.main(&argv)
    }

    #[test]
    fn applied_and_passes() {
        // Without the recolor, the N pixels differ.
        let ws = setup(r##"{ from = "#7BFF4A", to = "#FFFFFF", count = 100 }"##);
        let (code, out, err) = go(&ws, &[]);
        assert_eq!(code, 0, "{out}{err}");
        let json = ws.json_of("r");
        assert_eq!(
            statuses_of(&json)[0],
            s("recolored", "t/white.gb", "pass"),
            "{json}"
        );
        let c = &json["results"][0]["checks"][0];
        assert_eq!(c["reference"], UPSTREAM);
        assert_eq!(c["mismatched_pixels"], 0);
        assert_eq!(
            c["spec"],
            r##"screenshot = "x/ref.png", recolor = { from = "#7BFF4A", to = "#FFFFFF", count = 100 }"##
        );
    }

    #[test]
    fn wrong_count_is_exit_2_whatever_the_selection() {
        let ws = setup(r##"{ from = "#7BFF4A", to = "#FFFFFF", count = 99 }"##);
        // Also when the recolored group isn't selected: every recolor in
        // the manifest is counted before anything runs.
        for sel in [&[][..], &["--group", "other"][..]] {
            let (code, out, err) = go(&ws, sel);
            assert_eq!(code, 2, "{sel:?}: {out}{err}");
            assert!(
                err.contains(
                    "checking recolored references before running: recolor of x/ref.png: \
                     100 pixels are #7BFF4A, but the manifest's count is 99"
                ),
                "{sel:?}: {err}"
            );
        }
    }

    #[test]
    fn to_must_be_a_contract_color() {
        let ws = setup(r##"{ from = "#7BFF4A", to = "#FEFEFE", count = 100 }"##);
        let (code, out, err) = go(&ws, &[]);
        assert_eq!(code, 2, "{out}{err}");
        assert!(
            err.contains("recolor: to = #FEFEFE is not a contract color"),
            "{err}"
        );
    }

    #[test]
    fn from_and_to_must_differ() {
        // White is a contract color and the count is right: only the
        // from == to rule rejects this.
        let ws = setup(&format!(
            r##"{{ from = "#FFFFFF", to = "#FFFFFF", count = {} }}"##,
            160 * 144 - N
        ));
        let (code, out, err) = go(&ws, &[]);
        assert_eq!(code, 2, "{out}{err}");
        assert!(
            err.contains("recolor: from and to are the same color (#FFFFFF)"),
            "{err}"
        );
    }

    #[test]
    fn altered_upstream_is_an_integrity_failure() {
        let ws = setup(r##"{ from = "#7BFF4A", to = "#FFFFFF", count = 100 }"##);
        // Altered on disk (not re-listed): the up-front integrity check.
        ws.write_raw(
            &format!("test-roms/{UPSTREAM}"),
            &encode_png(&img(255)).unwrap(),
        );
        let (code, out, err) = go(&ws, &[]);
        assert_eq!(code, 2, "{out}{err}");
        assert!(err.contains(UPSTREAM), "{err}");

        // Altered after the up-front checks, before the check loads it:
        // the loaded bytes are verified again.
        ws.write_raw(&format!("test-roms/{UPSTREAM}"), &upstream());
        let path = ws.root.join("test-roms").join(UPSTREAM);
        let white = encode_png(&img(255)).unwrap();
        let tamper = |rom: &[u8], plan: &RunPlan, st: &mut RunState| {
            std::fs::write(&path, &white).unwrap();
            fake_backend(rom, plan, st)
        };
        let args = Args {
            manifest: ws.root.join("acceptance/r.toml"),
            jobs: Some(1),
            ..Default::default()
        };
        let paths = Paths::new(&ws.root, &args.manifest).unwrap();
        let e = run(&args, &paths, &tamper, &mut Vec::new()).unwrap_err();
        assert!(e.starts_with(&format!("reference {UPSTREAM}")), "{e}");
    }
}
