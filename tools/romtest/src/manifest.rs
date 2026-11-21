//! Manifest parsing, validation and ROM resolution
//! (acceptance/README.md, "Manifest schema"). Every problem here is a
//! runner error (exit 2): a bad manifest makes the results meaningless.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt;
use std::path::Path;

use gbc_core::{Buttons, CLOCK_HZ, FRAME_CLOCKS};
use serde::{Deserialize, Serialize};

use crate::glob::Glob;

/// The only supported model (ADR 0003).
pub const MODEL: &str = "cgb-c";

/// Base clocks per M-cycle at normal speed. Input times are rounded down to
/// a multiple of this.
const MCYCLE_CLOCKS: u64 = 4;

/// Sanity bound on any time in the manifest, so conversions can't overflow.
const MAX_SECONDS: f64 = 1.0e6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Must,
    Should,
    Stretch,
}

impl Tier {
    pub const ALL: [Tier; 3] = [Tier::Must, Tier::Should, Tier::Stretch];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Must => "must",
            Tier::Should => "should",
            Tier::Stretch => "stretch",
        }
    }

    /// Weight in the overall score (acceptance/README.md, "Score").
    pub fn weight(self) -> f64 {
        match self {
            Tier::Must => 3.0,
            Tier::Should => 2.0,
            Tier::Stretch => 1.0,
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// When a run ends. Limits are in base clocks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Exit {
    /// `run_s = N`
    RunS { seconds: f64 },
    /// `run_frames = N`
    RunFrames { frames: u64 },
    /// `ld_b_b = true, timeout_s = N`
    LdBB { timeout_s: f64 },
}

impl Exit {
    /// The emulated time at which the run ends (or times out).
    pub fn limit_clocks(&self) -> u64 {
        match *self {
            Exit::RunS { seconds } => seconds_to_clocks(seconds),
            Exit::RunFrames { frames } => frames * u64::from(FRAME_CLOCKS),
            Exit::LdBB { timeout_s } => seconds_to_clocks(timeout_s),
        }
    }

    pub fn is_ld_b_b(&self) -> bool {
        matches!(self, Exit::LdBB { .. })
    }
}

impl fmt::Display for Exit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exit::RunS { seconds } => write!(f, "run_s = {seconds}"),
            Exit::RunFrames { frames } => write!(f, "run_frames = {frames}"),
            Exit::LdBB { timeout_s } => write!(f, "ld_b_b, timeout_s = {timeout_s}"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CheckKind {
    Serial,
    Screenshot,
    Fibonacci,
    Gambatte,
}

impl CheckKind {
    pub const ALL: [CheckKind; 4] = [
        CheckKind::Serial,
        CheckKind::Screenshot,
        CheckKind::Fibonacci,
        CheckKind::Gambatte,
    ];

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.name() == s)
    }

    pub fn name(self) -> &'static str {
        match self {
            CheckKind::Serial => "serial",
            CheckKind::Screenshot => "screenshot",
            CheckKind::Fibonacci => "fibonacci",
            CheckKind::Gambatte => "gambatte",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    /// Serial output must contain this text.
    Serial(String),
    /// Path template of the expected PNG (`{dir}`, `{stem}`).
    Screenshot(String),
    Fibonacci,
    Gambatte,
}

impl Check {
    pub fn kind(&self) -> CheckKind {
        match self {
            Check::Serial(_) => CheckKind::Serial,
            Check::Screenshot(_) => CheckKind::Screenshot,
            Check::Fibonacci => CheckKind::Fibonacci,
            Check::Gambatte => CheckKind::Gambatte,
        }
    }
}

impl fmt::Display for Check {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Check::Serial(s) => write!(f, "serial = {s:?}"),
            Check::Screenshot(s) => write!(f, "screenshot = {s:?}"),
            Check::Fibonacci => f.write_str("fibonacci = true"),
            Check::Gambatte => f.write_str("gambatte = true"),
        }
    }
}

/// One `input` entry, converted to base clocks (rounded down to an M-cycle).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEntry {
    pub press_at: u64,
    pub release_at: u64,
    pub buttons: Buttons,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub id: String,
    pub tier: Tier,
    pub roms: Vec<Glob>,
    pub exclude: Vec<Glob>,
    pub input: Vec<InputEntry>,
    pub exit: Exit,
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub roms_release: String,
    pub model: String,
    pub groups: Vec<Group>,
}

/// `seconds × CLOCK_HZ`, rounded down. `CLOCK_HZ` is 2^22, so the product
/// is exact in `f64` and a time that is a whole number of clocks never
/// rounds down by one.
pub fn seconds_to_clocks(seconds: f64) -> u64 {
    (seconds * f64::from(CLOCK_HZ)).floor() as u64
}

fn input_clocks(seconds: f64) -> u64 {
    seconds_to_clocks(seconds) / MCYCLE_CLOCKS * MCYCLE_CLOCKS
}

// ---------------------------------------------------------------- raw ---

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    roms_release: String,
    model: String,
    group: Vec<RawGroup>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawGroup {
    id: String,
    tier: String,
    roms: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    input: Vec<RawInput>,
    exit: RawExit,
    checks: Vec<RawCheck>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInput {
    at_s: Num,
    press: Vec<String>,
    hold_s: Num,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExit {
    run_s: Option<Num>,
    run_frames: Option<i64>,
    ld_b_b: Option<bool>,
    timeout_s: Option<Num>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCheck {
    serial: Option<String>,
    screenshot: Option<String>,
    fibonacci: Option<bool>,
    gambatte: Option<bool>,
}

/// TOML integers and floats are both accepted for times.
#[derive(Deserialize, Clone, Copy)]
#[serde(untagged)]
enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    fn get(self) -> f64 {
        match self {
            Num::Int(i) => i as f64,
            Num::Float(f) => f,
        }
    }
}

fn seconds(n: Num, what: &str, allow_zero: bool) -> Result<f64, String> {
    let s = n.get();
    let ok = s.is_finite() && s <= MAX_SECONDS && if allow_zero { s >= 0.0 } else { s > 0.0 };
    if ok {
        Ok(s)
    } else {
        let range = if allow_zero { ">= 0" } else { "> 0" };
        Err(format!(
            "{what} = {s} is out of range (must be {range} and <= {MAX_SECONDS})"
        ))
    }
}

fn button(name: &str) -> Option<Buttons> {
    Some(match name {
        "a" => Buttons::A,
        "b" => Buttons::B,
        "select" => Buttons::SELECT,
        "start" => Buttons::START,
        "up" => Buttons::UP,
        "down" => Buttons::DOWN,
        "left" => Buttons::LEFT,
        "right" => Buttons::RIGHT,
        _ => return None,
    })
}

// ------------------------------------------------------------- parsing ---

/// Parses and validates a manifest. Does not touch the file system.
pub fn parse(text: &str) -> Result<Manifest, String> {
    let raw: RawManifest = toml::from_str(text).map_err(|e| e.to_string())?;
    if raw.model != MODEL {
        return Err(format!(
            "model = {:?} is not supported (only {MODEL:?})",
            raw.model
        ));
    }
    if raw.group.is_empty() {
        return Err("the manifest has no [[group]]".into());
    }
    let mut ids = HashSet::new();
    let mut groups = Vec::with_capacity(raw.group.len());
    for (i, g) in raw.group.into_iter().enumerate() {
        let label = if g.id.is_empty() {
            format!("group #{}", i + 1)
        } else {
            format!("group {:?}", g.id)
        };
        let group = parse_group(g).map_err(|e| format!("{label}: {e}"))?;
        if !ids.insert(group.id.clone()) {
            return Err(format!("{label}: duplicate group id"));
        }
        groups.push(group);
    }
    Ok(Manifest {
        roms_release: raw.roms_release,
        model: raw.model,
        groups,
    })
}

fn parse_group(g: RawGroup) -> Result<Group, String> {
    if g.id.is_empty() {
        return Err("empty id".into());
    }
    let tier = Tier::parse(&g.tier).ok_or_else(|| {
        format!(
            "tier = {:?} is not one of \"must\", \"should\", \"stretch\"",
            g.tier
        )
    })?;
    if g.roms.is_empty() {
        return Err("roms is empty".into());
    }
    let roms = g
        .roms
        .iter()
        .map(|p| Glob::new(p))
        .collect::<Result<_, _>>()?;
    let exclude = g
        .exclude
        .iter()
        .map(|p| Glob::new(p))
        .collect::<Result<_, _>>()?;

    let mut input = Vec::with_capacity(g.input.len());
    for (i, e) in g.input.iter().enumerate() {
        let at = seconds(e.at_s, "at_s", true).map_err(|m| format!("input #{}: {m}", i + 1))?;
        let hold =
            seconds(e.hold_s, "hold_s", false).map_err(|m| format!("input #{}: {m}", i + 1))?;
        if e.press.is_empty() {
            return Err(format!("input #{}: press is empty", i + 1));
        }
        let mut buttons = Buttons::EMPTY;
        for name in &e.press {
            buttons |=
                button(name).ok_or_else(|| format!("input #{}: unknown button {name:?}", i + 1))?;
        }
        let press_at = input_clocks(at);
        let release_at = input_clocks(at + hold);
        if release_at <= press_at {
            return Err(format!(
                "input #{}: hold_s = {hold} is shorter than one M-cycle",
                i + 1
            ));
        }
        input.push(InputEntry {
            press_at,
            release_at,
            buttons,
        });
    }

    let exit = parse_exit(&g.exit)?;

    if g.checks.is_empty() {
        return Err("checks is empty".into());
    }
    let checks = g
        .checks
        .into_iter()
        .enumerate()
        .map(|(i, c)| parse_check(c).map_err(|m| format!("check #{}: {m}", i + 1)))
        .collect::<Result<_, _>>()?;

    Ok(Group {
        id: g.id,
        tier,
        roms,
        exclude,
        input,
        exit,
        checks,
    })
}

fn parse_exit(e: &RawExit) -> Result<Exit, String> {
    let exit = match (e.run_s, e.run_frames, e.ld_b_b, e.timeout_s) {
        (Some(s), None, None, None) => Exit::RunS {
            seconds: seconds(s, "run_s", false)?,
        },
        (None, Some(n), None, None) => {
            if !(1..=1_000_000_000).contains(&n) {
                return Err(format!("exit: run_frames = {n} is out of range"));
            }
            Exit::RunFrames { frames: n as u64 }
        }
        (None, None, Some(true), Some(t)) => Exit::LdBB {
            timeout_s: seconds(t, "timeout_s", false)?,
        },
        (None, None, Some(false), _) => return Err("exit: ld_b_b must be true".into()),
        (None, None, Some(true), None) => return Err("exit: ld_b_b needs timeout_s".into()),
        (None, None, None, None) => return Err("exit: no exit condition".into()),
        _ => {
            return Err(
                "exit: give exactly one of run_s, run_frames, or ld_b_b with timeout_s".into(),
            );
        }
    };
    if exit.limit_clocks() == 0 {
        return Err(format!("exit: {exit} is shorter than one clock"));
    }
    Ok(exit)
}

fn parse_check(c: RawCheck) -> Result<Check, String> {
    let given = [
        c.serial.is_some(),
        c.screenshot.is_some(),
        c.fibonacci.is_some(),
        c.gambatte.is_some(),
    ];
    if given.iter().filter(|&&g| g).count() != 1 {
        return Err("each check needs exactly one key".into());
    }
    Ok(match c {
        RawCheck {
            serial: Some(s), ..
        } => {
            if s.is_empty() {
                return Err("serial text is empty".into());
            }
            Check::Serial(s)
        }
        RawCheck {
            screenshot: Some(s),
            ..
        } => {
            // Validate the template now; ROM-specific values come later.
            let probe = expand_template(&s, "d/e", "x");
            if s.is_empty() || probe.starts_with('/') || probe.split('/').any(|p| p == "..") {
                return Err(format!("bad screenshot path {s:?}"));
            }
            if s.replace("{dir}", "")
                .replace("{stem}", "")
                .contains(['{', '}'])
            {
                return Err(format!(
                    "screenshot path {s:?}: only {{dir}} and {{stem}} are expanded"
                ));
            }
            Check::Screenshot(s)
        }
        RawCheck {
            fibonacci: Some(f), ..
        } => {
            if !f {
                return Err("fibonacci must be true".into());
            }
            Check::Fibonacci
        }
        RawCheck {
            gambatte: Some(g), ..
        } => {
            if !g {
                return Err("gambatte must be true".into());
            }
            Check::Gambatte
        }
        _ => unreachable!("exactly one key is set"),
    })
}

/// Expands `{dir}` and `{stem}` for a ROM at `dir/stem.ext`.
pub fn expand_template(template: &str, dir: &str, stem: &str) -> String {
    template.replace("{dir}", dir).replace("{stem}", stem)
}

// ----------------------------------------------------------- resolution ---

/// Whether a path counts as a ROM.
pub fn is_rom(path: &str) -> bool {
    path.ends_with(".gb") || path.ends_with(".gbc")
}

/// Lists every file under `dir` as a relative `/`-separated path.
pub fn list_files(dir: &Path) -> Result<BTreeSet<String>, String> {
    fn walk(base: &Path, rel: &str, out: &mut BTreeSet<String>) -> Result<(), String> {
        let path = if rel.is_empty() {
            base.to_path_buf()
        } else {
            base.join(rel)
        };
        let entries =
            std::fs::read_dir(&path).map_err(|e| format!("reading {}: {e}", path.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading {}: {e}", path.display()))?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 file name in {}", path.display()))?;
            let child = if rel.is_empty() {
                name.to_owned()
            } else {
                format!("{rel}/{name}")
            };
            // `metadata` follows symlinks.
            let meta = std::fs::metadata(entry.path())
                .map_err(|e| format!("reading {}: {e}", entry.path().display()))?;
            if meta.is_dir() {
                walk(base, &child, out)?;
            } else if meta.is_file() {
                out.insert(child);
            }
        }
        Ok(())
    }
    let mut out = BTreeSet::new();
    walk(dir, "", &mut out)?;
    Ok(out)
}

/// Checks `roms_release` against `test-roms/.version`.
pub fn check_release(manifest: &Manifest, roms_dir: &Path) -> Result<(), String> {
    let path = roms_dir.join(".version");
    let version = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "reading {}: {e} (run scripts/fetch-test-roms.sh)",
            path.display()
        )
    })?;
    let version = version.trim();
    if version != manifest.roms_release {
        return Err(format!(
            "roms_release = {:?} but {} is {version:?}",
            manifest.roms_release,
            path.display()
        ));
    }
    Ok(())
}

/// The ROMs each group matches, sorted, in manifest group order. Errors on a
/// literal ROM path that doesn't exist and on a group matching nothing.
pub fn resolve(manifest: &Manifest, files: &BTreeSet<String>) -> Result<Vec<Vec<String>>, String> {
    let roms: Vec<&String> = files.iter().filter(|f| is_rom(f)).collect();
    let mut out = Vec::with_capacity(manifest.groups.len());
    for g in &manifest.groups {
        for glob in &g.roms {
            if let Some(paths) = glob.literal_paths() {
                for p in paths {
                    if !files.contains(&p) {
                        return Err(format!("group {:?}: ROM {p:?} not found", g.id));
                    }
                    if !is_rom(&p) {
                        return Err(format!(
                            "group {:?}: {p:?} is not a ROM (.gb or .gbc)",
                            g.id
                        ));
                    }
                }
            }
        }
        // A BTreeMap keeps each ROM once and sorted.
        let matched: BTreeMap<&str, ()> = roms
            .iter()
            .filter(|r| g.roms.iter().any(|glob| glob.matches(r)))
            .filter(|r| !g.exclude.iter().any(|glob| glob.matches(r)))
            .map(|r| (r.as_str(), ()))
            .collect();
        if matched.is_empty() {
            return Err(format!("group {:?} matches no ROMs", g.id));
        }
        out.push(matched.into_keys().map(str::to_owned).collect());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "roms_release = \"v7.0\"\nmodel = \"cgb-c\"\n";

    fn group(body: &str) -> String {
        format!("{HEAD}[[group]]\n{body}\n")
    }

    const MIN: &str = r#"id = "g"
tier = "must"
roms = ["a/*.gb"]
exit = { run_s = 1 }
checks = [{ fibonacci = true }]"#;

    fn err(text: &str) -> String {
        parse(text).expect_err("should be rejected")
    }

    #[test]
    fn parses_the_real_manifest() {
        let text = include_str!("../../../acceptance/stage1.toml");
        let m = parse(text).unwrap();
        assert_eq!(m.model, "cgb-c");
        assert!(m.groups.len() > 20);
        let sub = m
            .groups
            .iter()
            .find(|g| g.id == "rtc3test-sub-second")
            .unwrap();
        assert_eq!(sub.input.len(), 3);
        assert_eq!(sub.exit, Exit::RunS { seconds: 30.0 });
    }

    #[test]
    fn parses_a_full_group() {
        let m = parse(&group(
            r#"id = "g"
tier = "should"
roms = ["a/*.gb", "b/{x,y}.gb"]
exclude = ["a/z.gb"]
input = [{ at_s = 1, press = ["a", "down"], hold_s = 0.1 }]
exit = { ld_b_b = true, timeout_s = 10 }
checks = [{ serial = "Passed" }, { screenshot = "{dir}/{stem}.png" }, { fibonacci = true }, { gambatte = true }]"#,
        ))
        .unwrap();
        let g = &m.groups[0];
        assert_eq!(g.tier, Tier::Should);
        assert_eq!(g.roms.len(), 2);
        assert_eq!(g.exclude.len(), 1);
        assert_eq!(g.exit, Exit::LdBB { timeout_s: 10.0 });
        assert_eq!(g.exit.limit_clocks(), 10 * u64::from(CLOCK_HZ));
        assert_eq!(
            g.checks,
            [
                Check::Serial("Passed".into()),
                Check::Screenshot("{dir}/{stem}.png".into()),
                Check::Fibonacci,
                Check::Gambatte
            ]
        );
        assert_eq!(g.input[0].buttons, Buttons::A | Buttons::DOWN);
    }

    #[test]
    fn exit_limits_in_base_clocks() {
        assert_eq!(Exit::RunS { seconds: 31.0 }.limit_clocks(), 31 * 4_194_304);
        assert_eq!(Exit::RunFrames { frames: 40 }.limit_clocks(), 40 * 70_224);
        assert_eq!(Exit::RunS { seconds: 0.5 }.limit_clocks(), 2_097_152);
    }

    #[test]
    fn input_times_round_down_to_an_m_cycle() {
        let m = parse(&group(&format!(
            "{MIN}\ninput = [{{ at_s = 1.3, press = [\"a\"], hold_s = 0.1 }}, {{ at_s = 1, press = [\"b\"], hold_s = 0.5 }}]"
        )))
        .unwrap();
        let i = &m.groups[0].input;
        // 1.3 s = 5,452,595.2 clocks -> 5,452,595 -> M-cycle 5,452,592.
        assert_eq!(i[0].press_at, 5_452_592);
        // 1.4 s = 5,872,025.6 clocks -> 5,872,024.
        assert_eq!(i[0].release_at, 5_872_024);
        assert_eq!(i[1].press_at, 4_194_304);
        assert_eq!(i[1].release_at, 6_291_456);
        for e in i {
            assert_eq!(e.press_at % 4, 0);
            assert_eq!(e.release_at % 4, 0);
        }
    }

    #[test]
    fn rejects_unknown_and_missing_keys() {
        assert!(err(&format!("{HEAD}bogus = 1\n[[group]]\n{MIN}")).contains("bogus"));
        assert!(err(&group(&format!("{MIN}\nextra = 1"))).contains("extra"));
        assert!(err(&format!("model = \"cgb-c\"\n[[group]]\n{MIN}")).contains("roms_release"));
        assert!(
            err(&group(
                "id = \"g\"\ntier = \"must\"\nexit = { run_s = 1 }\nchecks = [{ fibonacci = true }]"
            ))
            .contains("roms")
        );
        assert!(err(&group(&MIN.replace("exit = { run_s = 1 }", ""))).contains("exit"));
        assert!(
            err(&group(&MIN.replace("checks = [{ fibonacci = true }]", ""))).contains("checks")
        );
        assert!(
            err(&group(
                &MIN.replace("{ run_s = 1 }", "{ run_s = 1, frames = 2 }")
            ))
            .contains("frames")
        );
        assert!(err(&group(&MIN.replace("fibonacci", "fibonaci"))).contains("fibonaci"));
        assert!(
            err(&group(&format!(
                "{MIN}\ninput = [{{ at_s = 1, press = [\"a\"], hold_s = 1, x = 1 }}]"
            )))
            .contains('x')
        );
        assert!(err(HEAD).contains("group"));
    }

    #[test]
    fn rejects_bad_values() {
        let cases = [
            (MIN.replace("\"must\"", "\"maybe\""), "tier"),
            (MIN.replace("[\"a/*.gb\"]", "[]"), "roms is empty"),
            (MIN.replace("[\"a/*.gb\"]", "[\"a/{x\"]"), "bad glob"),
            (
                MIN.replace("[{ fibonacci = true }]", "[]"),
                "checks is empty",
            ),
            (MIN.replace("fibonacci = true", "fibonacci = false"), "true"),
            (MIN.replace("fibonacci = true", "gambatte = false"), "true"),
            (MIN.replace("fibonacci = true", "serial = \"\""), "empty"),
            (
                MIN.replace("fibonacci = true", "serial = \"a\", fibonacci = true"),
                "exactly one",
            ),
            (
                MIN.replace("fibonacci = true", "screenshot = \"/abs.png\""),
                "screenshot",
            ),
            (
                MIN.replace("fibonacci = true", "screenshot = \"{name}.png\""),
                "expanded",
            ),
            (MIN.replace("run_s = 1", "run_s = 0"), "run_s"),
            (MIN.replace("run_s = 1", "run_s = -2.5"), "run_s"),
            (MIN.replace("run_s = 1", "run_frames = 0"), "run_frames"),
            (
                MIN.replace("run_s = 1", "run_s = 1, run_frames = 2"),
                "exactly one",
            ),
            (MIN.replace("run_s = 1", "ld_b_b = true"), "timeout_s"),
            (
                MIN.replace("run_s = 1", "ld_b_b = false, timeout_s = 1"),
                "true",
            ),
            (MIN.replace("run_s = 1", "timeout_s = 1"), "exactly one"),
            (
                MIN.replace("run_s = 1", "ld_b_b = true, timeout_s = 1, run_s = 2"),
                "exactly one",
            ),
            (MIN.replace("{ run_s = 1 }", "{}"), "no exit"),
            (
                format!("{MIN}\ninput = [{{ at_s = 1, press = [\"x\"], hold_s = 1 }}]"),
                "unknown button",
            ),
            (
                format!("{MIN}\ninput = [{{ at_s = 1, press = [], hold_s = 1 }}]"),
                "press is empty",
            ),
            (
                format!("{MIN}\ninput = [{{ at_s = -1, press = [\"a\"], hold_s = 1 }}]"),
                "at_s",
            ),
            (
                format!("{MIN}\ninput = [{{ at_s = 1, press = [\"a\"], hold_s = 0 }}]"),
                "hold_s",
            ),
            (
                format!("{MIN}\ninput = [{{ at_s = 1, press = [\"a\"], hold_s = 1e-9 }}]"),
                "M-cycle",
            ),
            (MIN.replace("id = \"g\"", "id = \"\""), "empty id"),
        ];
        for (body, needle) in cases {
            let e = err(&group(&body));
            assert!(e.contains(needle), "{body}\n=> {e}\n(expected {needle:?})");
        }
    }

    #[test]
    fn rejects_bad_model_and_duplicate_ids() {
        let text = format!("roms_release = \"v7.0\"\nmodel = \"dmg\"\n[[group]]\n{MIN}");
        assert!(err(&text).contains("model"));
        let text = format!("{HEAD}[[group]]\n{MIN}\n[[group]]\n{MIN}");
        assert!(err(&text).contains("duplicate"));
    }

    fn files(list: &[&str]) -> BTreeSet<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn resolve_one(body: &str, fs: &[&str]) -> Result<Vec<String>, String> {
        let m = parse(&group(body))?;
        resolve(&m, &files(fs)).map(|mut v| v.remove(0))
    }

    #[test]
    fn resolve_globs_excludes_and_extensions() {
        let fs = [
            "a/x.gb",
            "a/y.gbc",
            "a/y.png",
            "a/z-dmg.gb",
            "a/sub/w.gb",
            "a/readme.md",
        ];
        let body = MIN.replace("[\"a/*.gb\"]", "[\"a/**\", \"a/x.gb\"]");
        let body = format!("{body}\nexclude = [\"a/*-*.gb\"]");
        assert_eq!(
            resolve_one(&body, &fs).unwrap(),
            ["a/sub/w.gb", "a/x.gb", "a/y.gbc"]
        );
        let body = MIN.replace("[\"a/*.gb\"]", "[\"a/{x,sub/w}.gb\"]");
        assert_eq!(resolve_one(&body, &fs).unwrap(), ["a/sub/w.gb", "a/x.gb"]);
    }

    #[test]
    fn resolve_errors() {
        let fs = ["a/x.gb", "a/y.png"];
        let e = resolve_one(&MIN.replace("a/*.gb", "a/missing.gb"), &fs).unwrap_err();
        assert!(e.contains("not found"), "{e}");
        let e = resolve_one(&MIN.replace("a/*.gb", "a/{x,q}.gb"), &fs).unwrap_err();
        assert!(e.contains("a/q.gb"), "{e}");
        let e = resolve_one(&MIN.replace("a/*.gb", "a/y.png"), &fs).unwrap_err();
        assert!(e.contains("not a ROM"), "{e}");
        let e = resolve_one(&MIN.replace("a/*.gb", "b/*.gb"), &fs).unwrap_err();
        assert!(e.contains("matches no ROMs"), "{e}");
        let body = format!("{MIN}\nexclude = [\"a/**\"]");
        assert!(resolve_one(&body, &fs).unwrap_err().contains("no ROMs"));
    }

    #[test]
    fn release_check() {
        let dir = tempfile::tempdir().unwrap();
        let m = parse(&group(MIN)).unwrap();
        assert!(
            check_release(&m, dir.path())
                .unwrap_err()
                .contains(".version")
        );
        std::fs::write(dir.path().join(".version"), "v7.0\n").unwrap();
        check_release(&m, dir.path()).unwrap();
        std::fs::write(dir.path().join(".version"), "v6.0\n").unwrap();
        assert!(check_release(&m, dir.path()).unwrap_err().contains("v6.0"));
    }

    #[test]
    fn list_files_is_relative_and_recursive() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/x.gb"), b"").unwrap();
        std::fs::write(dir.path().join("y.gbc"), b"").unwrap();
        assert_eq!(
            list_files(dir.path()).unwrap(),
            files(&["a/b/x.gb", "y.gbc"])
        );
    }

    #[test]
    fn template_expansion() {
        assert_eq!(
            expand_template("{dir}/{stem}-cgbBCE.png", "age/m3", "m3-bg"),
            "age/m3/m3-bg-cgbBCE.png"
        );
    }
}
