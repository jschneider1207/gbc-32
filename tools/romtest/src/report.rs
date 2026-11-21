//! Statuses, scores, the baseline, regressions and the output files
//! (acceptance/README.md, "Statuses", "Outputs", "Score", "Regressions").

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use gbc_core::Registers;
use serde::{Deserialize, Serialize};

use crate::checks::{CheckResult, CheckStatus};
use crate::machine::End;
use crate::manifest::Tier;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Pass,
    Fail,
    Timeout,
    Crash,
    Unsupported,
    NoReference,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Timeout => "timeout",
            Status::Crash => "crash",
            Status::Unsupported => "unsupported",
            Status::NoReference => "no-reference",
        }
    }
}

/// How a ROM run ended, as far as the runner is concerned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ran(End),
    LoadError(String),
    Panic(String),
}

impl Outcome {
    pub fn name(&self) -> &'static str {
        match self {
            Outcome::Ran(end) => end.name(),
            Outcome::LoadError(_) => "load_error",
            Outcome::Panic(_) => "panic",
        }
    }

    /// Whether the checks can be evaluated (the emulator ran to an exit).
    pub fn ran(&self) -> bool {
        matches!(self, Outcome::Ran(_))
    }
}

/// The ROM's status from how the run ended and its check results.
///
/// Precedence (ADR 0007 amendments): crash/timeout > fail > unsupported >
/// no-reference > pass. `no-reference` (excluded from the score) applies
/// only when every other check passed, so a missing PNG can never hide a
/// crash, a timeout, a failing check or an unsupported (scored as failing)
/// check.
pub fn rom_status(outcome: &Outcome, checks: &[CheckStatus]) -> Status {
    let any = |s: CheckStatus| checks.contains(&s);
    match outcome {
        Outcome::LoadError(_) | Outcome::Panic(_) => return Status::Crash,
        Outcome::Ran(End::Locked | End::NoProgress | End::InputLate { .. }) => {
            return Status::Crash;
        }
        Outcome::Ran(End::Timeout) => return Status::Timeout,
        Outcome::Ran(End::Completed | End::LdBB) => {}
    }
    // `Skipped` only appears when the ROM didn't run (already a crash);
    // count it as a failure in case that ever changes.
    if any(CheckStatus::Fail) || any(CheckStatus::Skipped) {
        Status::Fail
    } else if any(CheckStatus::Unsupported) {
        Status::Unsupported
    } else if any(CheckStatus::NoReference) {
        Status::NoReference
    } else {
        Status::Pass
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct RegistersJson {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
}

impl From<Registers> for RegistersJson {
    fn from(r: Registers) -> Self {
        Self {
            a: r.a,
            f: r.f,
            b: r.b,
            c: r.c,
            d: r.d,
            e: r.e,
            h: r.h,
            l: r.l,
            sp: r.sp,
            pc: r.pc,
        }
    }
}

/// The result of one `(group, ROM)` run.
#[derive(Clone, Debug, Serialize)]
pub struct RomResult {
    pub group: String,
    pub tier: Tier,
    pub rom: String,
    pub status: Status,
    /// How the run ended: completed, ld_b_b, timeout, cpu_locked,
    /// no_progress, load_error or panic.
    pub end: &'static str,
    /// Crash details (panic message, load error, lockup).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Emulated base clocks at exit.
    pub clock: u64,
    pub frames: u64,
    /// Everything sent on the serial port (lossy UTF-8).
    pub serial: String,
    pub registers: Option<RegistersJson>,
    pub checks: Vec<CheckResult>,
    pub wall_time_s: f64,
    #[serde(skip)]
    pub group_index: usize,
    #[serde(skip)]
    pub wall: Duration,
}

impl RomResult {
    /// One-line explanation of a non-passing result.
    pub fn explain(&self) -> String {
        let mut parts = Vec::new();
        if !self.detail.is_empty() {
            parts.push(self.detail.clone());
        }
        for c in &self.checks {
            if !matches!(c.status, CheckStatus::Pass | CheckStatus::Skipped) {
                let what = if c.detail.is_empty() {
                    c.status_name().to_owned()
                } else {
                    c.detail.clone()
                };
                parts.push(format!("{}: {what}", c.kind));
            }
        }
        parts.join("; ")
    }
}

impl CheckResult {
    fn status_name(&self) -> &'static str {
        match self.status {
            CheckStatus::Pass => "pass",
            CheckStatus::Fail => "fail",
            CheckStatus::Unsupported => "unsupported",
            CheckStatus::NoReference => "no-reference",
            CheckStatus::Skipped => "skipped",
        }
    }
}

/// Status counts for one tier (or group).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub total: usize,
    pub pass: usize,
    pub fail: usize,
    pub timeout: usize,
    pub crash: usize,
    pub unsupported: usize,
    pub no_reference: usize,
}

impl Counts {
    pub fn add(&mut self, s: Status) {
        self.total += 1;
        *match s {
            Status::Pass => &mut self.pass,
            Status::Fail => &mut self.fail,
            Status::Timeout => &mut self.timeout,
            Status::Crash => &mut self.crash,
            Status::Unsupported => &mut self.unsupported,
            Status::NoReference => &mut self.no_reference,
        } += 1;
    }

    /// `passed / (total - no_reference)`, or `None` if nothing is scored.
    pub fn score(&self) -> Option<f64> {
        let scored = self.total - self.no_reference;
        (scored > 0).then(|| self.pass as f64 / scored as f64)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct TierSummary {
    #[serde(flatten)]
    pub counts: Counts,
    pub score: Option<f64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Summary {
    pub tiers: BTreeMap<Tier, TierSummary>,
    /// `(3·must + 2·should + 1·stretch) / 6`. If a tier has nothing scored
    /// (a partial run), the weights of the remaining tiers are renormalized.
    pub overall: Option<f64>,
}

pub fn summarize(results: &[RomResult]) -> Summary {
    let mut counts: BTreeMap<Tier, Counts> = BTreeMap::new();
    for r in results {
        counts.entry(r.tier).or_default().add(r.status);
    }
    let tiers: BTreeMap<Tier, TierSummary> = counts
        .into_iter()
        .map(|(t, c)| {
            let summary = TierSummary {
                counts: c,
                score: c.score(),
            };
            (t, summary)
        })
        .collect();
    let (mut sum, mut weights) = (0.0, 0.0);
    for (t, s) in &tiers {
        if let Some(score) = s.score {
            sum += t.weight() * score;
            weights += t.weight();
        }
    }
    let overall = (weights > 0.0).then(|| sum / weights);
    Summary { tiers, overall }
}

// ------------------------------------------------------------ baseline ---

/// The committed baseline: the status of every `(group, ROM)` at the last
/// accepted run. Keyed by group id, then ROM path; sorted for stable diffs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Baseline {
    pub roms_release: String,
    pub results: BTreeMap<String, BTreeMap<String, Status>>,
}

impl Baseline {
    pub fn from_results(roms_release: &str, results: &[RomResult]) -> Self {
        let mut map: BTreeMap<String, BTreeMap<String, Status>> = BTreeMap::new();
        for r in results {
            map.entry(r.group.clone())
                .or_default()
                .insert(r.rom.clone(), r.status);
        }
        Self {
            roms_release: roms_release.to_owned(),
            results: map,
        }
    }

    pub fn status(&self, group: &str, rom: &str) -> Option<Status> {
        self.results.get(group)?.get(rom).copied()
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("baseline serializes");
        s.push('\n');
        s
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Regression {
    pub group: String,
    pub rom: String,
    pub was: Status,
    /// `None`: the `(group, ROM)` has no result any more (serialized as
    /// `"missing"`).
    #[serde(serialize_with = "serialize_now")]
    pub now: Option<Status>,
}

impl Regression {
    pub fn now_name(&self) -> &'static str {
        self.now.map_or("missing", Status::name)
    }
}

fn serialize_now<S: serde::Serializer>(now: &Option<Status>, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(now.map_or("missing", Status::name))
}

/// Which baseline entries a run answers for.
#[derive(Clone, Copy)]
pub enum Scope<'a> {
    /// A full run: every baseline entry must still have a result.
    Full,
    /// A partial run (`--filter`, `--group`, `--only-check`): the
    /// function says whether `(group, rom)` is inside the selection.
    Partial(&'a dyn Fn(&str, &str) -> bool),
}

/// Regressions (ADR 0007 amendments):
/// - every baseline entry with status `pass` whose current status isn't
///   `pass`;
/// - in a full run, every baseline entry, whatever its status, with no
///   current result (the ROM was removed, excluded or renamed, or its
///   group was): `<status> -> missing`. Only `--update-baseline
///   --allow-removed` may drop such entries;
/// - in a partial run, every baseline `pass` entry inside the selection
///   with no current result.
pub fn regressions(
    baseline: Option<&Baseline>,
    results: &[RomResult],
    scope: Scope<'_>,
) -> Vec<Regression> {
    let Some(b) = baseline else {
        return Vec::new();
    };
    let mut out: Vec<Regression> = results
        .iter()
        .filter(|r| r.status != Status::Pass && b.status(&r.group, &r.rom) == Some(Status::Pass))
        .map(|r| Regression {
            group: r.group.clone(),
            rom: r.rom.clone(),
            was: Status::Pass,
            now: Some(r.status),
        })
        .collect();
    let ran: std::collections::HashSet<(&str, &str)> = results
        .iter()
        .map(|r| (r.group.as_str(), r.rom.as_str()))
        .collect();
    for (group, roms) in &b.results {
        for (rom, &status) in roms {
            if ran.contains(&(group.as_str(), rom.as_str())) {
                continue;
            }
            let missing = match scope {
                Scope::Full => true,
                Scope::Partial(in_scope) => status == Status::Pass && in_scope(group, rom),
            };
            if missing {
                out.push(Regression {
                    group: group.clone(),
                    rom: rom.clone(),
                    was: status,
                    now: None,
                });
            }
        }
    }
    out
}

/// Splits off the baseline entries with no current result (`now` is
/// `None`), which `--update-baseline --allow-removed` drops instead of
/// counting as regressions. Returns `(regressions, removed)`.
pub fn split_removed(regressions: Vec<Regression>) -> (Vec<Regression>, Vec<Regression>) {
    regressions.into_iter().partition(|r| r.now.is_some())
}

/// Results in the `--gate` tier that don't pass.
pub fn gate_failures(results: &[RomResult], tier: Tier) -> Vec<&RomResult> {
    results
        .iter()
        .filter(|r| r.tier == tier && r.status != Status::Pass)
        .collect()
}

/// Exit code for a run that completed (acceptance/README.md, "Exit codes").
/// Runner errors (2) are decided elsewhere.
pub fn exit_code(regressions: usize, gate_failures: usize) -> u8 {
    if regressions > 0 || gate_failures > 0 {
        1
    } else {
        0
    }
}

/// Whether `--update-baseline` may write after this run.
pub fn may_update_baseline(regressions: &[Regression]) -> Result<(), String> {
    if regressions.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "refusing to update the baseline: {} regression(s)",
            regressions.len()
        ))
    }
}

/// The `limit` slowest results by wall time (ties by group and ROM).
pub fn slowest(results: &[RomResult], limit: usize) -> Vec<&RomResult> {
    let mut v: Vec<&RomResult> = results.iter().collect();
    v.sort_by(|a, b| {
        b.wall
            .cmp(&a.wall)
            .then_with(|| (a.group_index, &a.rom).cmp(&(b.group_index, &b.rom)))
    });
    v.truncate(limit);
    v
}

// ------------------------------------------------------------- outputs ---

/// What was selected for this run.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Selection {
    pub filter: Option<String>,
    pub groups: Vec<String>,
    pub only_check: Option<String>,
}

impl Selection {
    pub fn is_full(&self) -> bool {
        self.filter.is_none() && self.groups.is_empty() && self.only_check.is_none()
    }

    pub fn describe(&self) -> String {
        if self.is_full() {
            return "full manifest".into();
        }
        let mut parts = Vec::new();
        if let Some(f) = &self.filter {
            parts.push(format!("--filter '{f}'"));
        }
        for g in &self.groups {
            parts.push(format!("--group {g}"));
        }
        if let Some(k) = &self.only_check {
            parts.push(format!("--only-check {k}"));
        }
        parts.join(" ")
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct GateResult {
    pub tier: Tier,
    pub failures: usize,
    pub passed: bool,
}

/// The full results file (`target/acceptance/<manifest>.json`).
#[derive(Serialize)]
pub struct Report<'a> {
    pub manifest: String,
    pub roms_release: String,
    pub model: String,
    pub selection: Selection,
    pub jobs: usize,
    pub wall_time_s: f64,
    pub summary: Summary,
    pub gate: Option<GateResult>,
    pub baseline_found: bool,
    pub regressions: Vec<Regression>,
    /// Baseline entries with no current result that `--update-baseline
    /// --allow-removed` dropped (otherwise they are in `regressions`).
    pub removed_from_baseline: Vec<Regression>,
    pub results: &'a [RomResult],
}

impl Report<'_> {
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("report serializes");
        s.push('\n');
        s
    }

    /// The Markdown summary (`target/acceptance/<manifest>.md`). Lists
    /// every ROM that ran, grouped by tier and group.
    pub fn to_markdown(&self, group_order: &[(String, Tier)]) -> String {
        let mut md = String::new();
        let pct = |s: Option<f64>| s.map_or("n/a".to_owned(), |s| format!("{:.1}%", s * 100.0));
        let _ = writeln!(md, "# Acceptance results: {}\n", self.manifest);
        let _ = writeln!(
            md,
            "- roms_release `{}`, model `{}`\n- Selection: {}\n- ROM runs: {} (jobs {}), wall time {:.1} s",
            self.roms_release,
            self.model,
            self.selection.describe(),
            self.results.len(),
            self.jobs,
            self.wall_time_s
        );
        if let Some(g) = &self.gate {
            let verdict = if g.passed { "PASS" } else { "FAIL" };
            let _ = writeln!(
                md,
                "- Gate `{}`: **{verdict}** ({} not passing)",
                g.tier, g.failures
            );
        }
        let baseline = if self.baseline_found {
            format!("{} regression(s)", self.regressions.len())
        } else {
            "no baseline yet".into()
        };
        let _ = writeln!(md, "- Baseline: {baseline}\n");

        if !self.regressions.is_empty() {
            let _ = writeln!(
                md,
                "## Regressions\n\n| Group | ROM | Was | Now |\n|---|---|---|---|"
            );
            for r in &self.regressions {
                let _ = writeln!(
                    md,
                    "| {} | `{}` | {} | {} |",
                    r.group,
                    r.rom,
                    r.was.name(),
                    r.now_name()
                );
            }
            md.push('\n');
        }
        if !self.removed_from_baseline.is_empty() {
            let _ = writeln!(
                md,
                "## Removed from the baseline (--allow-removed)\n\n| Group | ROM | Was |\n|---|---|---|"
            );
            for r in &self.removed_from_baseline {
                let _ = writeln!(md, "| {} | `{}` | {} |", r.group, r.rom, r.was.name());
            }
            md.push('\n');
        }

        let _ = writeln!(md, "## Score\n");
        let _ = writeln!(
            md,
            "| Tier | Pass | Fail | Timeout | Crash | Unsupported | No reference | Total | Score |"
        );
        let _ = writeln!(md, "|---|---|---|---|---|---|---|---|---|");
        for (t, s) in &self.summary.tiers {
            let c = &s.counts;
            let _ = writeln!(
                md,
                "| {t} | {} | {} | {} | {} | {} | {} | {} | {} |",
                c.pass,
                c.fail,
                c.timeout,
                c.crash,
                c.unsupported,
                c.no_reference,
                c.total,
                pct(s.score)
            );
        }
        let _ = writeln!(
            md,
            "\n**Overall: {}**\n\n`tier score = passed / (total - no_reference)`; \
             `overall = (3·must + 2·should + 1·stretch) / 6` \
             (weights renormalized over the tiers present in a partial run).\n",
            pct(self.summary.overall)
        );

        // Per-group summary, then every ROM.
        let mut by_group: BTreeMap<&str, Vec<&RomResult>> = BTreeMap::new();
        for r in self.results {
            by_group.entry(&r.group).or_default().push(r);
        }
        let _ = writeln!(md, "## Groups\n");
        for tier in Tier::ALL {
            let groups: Vec<_> = group_order
                .iter()
                .filter(|(id, t)| *t == tier && by_group.contains_key(id.as_str()))
                .collect();
            if groups.is_empty() {
                continue;
            }
            let _ = writeln!(
                md,
                "### {tier}\n\n| Group | Pass | Total | Score |\n|---|---|---|---|"
            );
            for (id, _) in &groups {
                let mut c = Counts::default();
                for r in &by_group[id.as_str()] {
                    c.add(r.status);
                }
                let _ = writeln!(
                    md,
                    "| {id} | {} | {} | {} |",
                    c.pass,
                    c.total,
                    pct(c.score())
                );
            }
            md.push('\n');
        }

        let _ = writeln!(md, "## Results\n");
        for (id, tier) in group_order {
            let Some(rs) = by_group.get(id.as_str()) else {
                continue;
            };
            let _ = writeln!(
                md,
                "### {id} ({tier})\n\n| ROM | Status | Time (s) | Details |\n|---|---|---|---|"
            );
            for r in rs {
                let _ = writeln!(
                    md,
                    "| `{}` | {} | {:.2} | {} |",
                    r.rom,
                    r.status.name(),
                    r.wall_time_s,
                    r.explain().replace('|', "\\|").replace('\n', " ")
                );
            }
            md.push('\n');
        }
        md
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn result(group: &str, tier: Tier, rom: &str, status: Status) -> RomResult {
        RomResult {
            group: group.into(),
            tier,
            rom: rom.into(),
            status,
            end: "completed",
            detail: String::new(),
            clock: 0,
            frames: 0,
            serial: String::new(),
            registers: None,
            checks: Vec::new(),
            wall_time_s: 0.0,
            group_index: 0,
            wall: Duration::ZERO,
        }
    }

    #[test]
    fn status_precedence() {
        use CheckStatus as C;
        let ran = Outcome::Ran(End::Completed);
        assert_eq!(rom_status(&ran, &[C::Pass, C::Pass]), Status::Pass);
        assert_eq!(rom_status(&ran, &[C::Pass, C::Fail]), Status::Fail);
        assert_eq!(rom_status(&ran, &[C::Unsupported, C::Fail]), Status::Fail);
        assert_eq!(
            rom_status(&ran, &[C::Pass, C::Unsupported]),
            Status::Unsupported
        );
        // A missing PNG doesn't exclude a ROM whose other check is
        // unsupported (scored as a failure).
        assert_eq!(
            rom_status(&ran, &[C::NoReference, C::Unsupported]),
            Status::Unsupported
        );
        assert_eq!(
            rom_status(&ran, &[C::Unsupported, C::NoReference]),
            Status::Unsupported
        );
        assert_eq!(
            rom_status(&ran, &[C::Pass, C::NoReference]),
            Status::NoReference
        );
        // A missing reference never hides a failure, timeout or crash:
        // crash/timeout > fail > unsupported > no-reference > pass.
        assert_eq!(rom_status(&ran, &[C::Fail, C::NoReference]), Status::Fail);
        assert_eq!(rom_status(&ran, &[C::NoReference, C::Fail]), Status::Fail);
        let timeout = Outcome::Ran(End::Timeout);
        assert_eq!(rom_status(&timeout, &[C::NoReference]), Status::Timeout);
        assert_eq!(
            rom_status(&timeout, &[C::Fail, C::NoReference]),
            Status::Timeout
        );
        let panic = Outcome::Panic("x".into());
        assert_eq!(
            rom_status(&panic, &[C::Skipped, C::NoReference]),
            Status::Crash
        );
        let locked = Outcome::Ran(End::Locked);
        assert_eq!(rom_status(&locked, &[C::NoReference]), Status::Crash);
        assert_eq!(
            rom_status(&Outcome::LoadError("x".into()), &[C::NoReference]),
            Status::Crash
        );
        assert_eq!(
            rom_status(&Outcome::Ran(End::LdBB), &[C::Pass]),
            Status::Pass
        );
        assert_eq!(
            rom_status(&Outcome::Ran(End::Timeout), &[C::Pass]),
            Status::Timeout
        );
        assert_eq!(
            rom_status(&Outcome::Ran(End::Locked), &[C::Pass]),
            Status::Crash
        );
        assert_eq!(
            rom_status(&Outcome::Ran(End::NoProgress), &[C::Pass]),
            Status::Crash
        );
        let late = End::InputLate { at: 2, now: 4 };
        assert_eq!(rom_status(&Outcome::Ran(late), &[C::Pass]), Status::Crash);
        assert_eq!(
            rom_status(&Outcome::Panic("x".into()), &[C::Skipped]),
            Status::Crash
        );
        assert_eq!(
            rom_status(&Outcome::LoadError("x".into()), &[C::Skipped]),
            Status::Crash
        );
    }

    #[test]
    fn scores() {
        let rs = [
            result("a", Tier::Must, "1", Status::Pass),
            result("a", Tier::Must, "2", Status::Fail),
            result("a", Tier::Must, "3", Status::NoReference),
            result("b", Tier::Should, "1", Status::Unsupported),
            result("b", Tier::Should, "2", Status::Pass),
            result("b", Tier::Should, "3", Status::Timeout),
            result("b", Tier::Should, "4", Status::Crash),
            result("c", Tier::Stretch, "1", Status::NoReference),
        ];
        let s = summarize(&rs);
        let must = &s.tiers[&Tier::Must];
        assert_eq!(must.counts.total, 3);
        assert_eq!(must.score, Some(0.5));
        assert_eq!(s.tiers[&Tier::Should].score, Some(0.25));
        // Stretch has only an excluded ROM: nothing scored.
        assert_eq!(s.tiers[&Tier::Stretch].score, None);
        // (3·0.5 + 2·0.25) / 5 with stretch absent.
        assert!((s.overall.unwrap() - 0.4).abs() < 1e-12);

        let rs = [
            result("a", Tier::Must, "1", Status::Pass),
            result("b", Tier::Should, "1", Status::Fail),
            result("c", Tier::Stretch, "1", Status::Pass),
        ];
        // Full formula: (3·1 + 2·0 + 1·1) / 6.
        assert!((summarize(&rs).overall.unwrap() - 4.0 / 6.0).abs() < 1e-12);
        assert_eq!(summarize(&[]).overall, None);
    }

    #[test]
    fn regression_detection() {
        let mut b = Baseline::from_results(
            "v7.0",
            &[
                result("g", Tier::Must, "a.gb", Status::Pass),
                result("g", Tier::Must, "b.gb", Status::Pass),
                result("g", Tier::Must, "c.gb", Status::Fail),
                result("h", Tier::Must, "a.gb", Status::Pass),
            ],
        );
        let now = [
            result("g", Tier::Must, "a.gb", Status::Pass),
            result("g", Tier::Must, "b.gb", Status::NoReference),
            result("g", Tier::Must, "c.gb", Status::Crash),
            // h/a.gb didn't run (partial run): not compared.
            result("new", Tier::Must, "a.gb", Status::Fail),
        ];
        // h/a.gb is out of this run's scope.
        let in_scope = |g: &str, _: &str| g != "h";
        let scope = Scope::Partial(&in_scope);
        let r = regressions(Some(&b), &now, scope);
        assert_eq!(
            r,
            [Regression {
                group: "g".into(),
                rom: "b.gb".into(),
                was: Status::Pass,
                now: Some(Status::NoReference)
            }]
        );
        assert!(regressions(None, &now, scope).is_empty());

        // In a partial run's scope, a passing key with no result is a
        // regression; a missing non-passing key (g/c.gb below) is not.
        let all = |_: &str, _: &str| true;
        let names = |r: &[Regression]| -> Vec<(String, String, &str, &str)> {
            r.iter()
                .map(|r| (r.group.clone(), r.rom.clone(), r.was.name(), r.now_name()))
                .collect()
        };
        let r = regressions(Some(&b), &now[..2], Scope::Partial(&all));
        assert_eq!(
            names(&r),
            [
                ("g".into(), "b.gb".into(), "pass", "no-reference"),
                ("h".into(), "a.gb".into(), "pass", "missing")
            ]
        );
        let json = serde_json::to_value(&r[1]).unwrap();
        assert_eq!(json["now"], "missing");
        assert_eq!(json["was"], "pass");

        // In a full run every missing key counts, whatever its status.
        let r = regressions(Some(&b), &now[..2], Scope::Full);
        assert_eq!(
            names(&r),
            [
                ("g".into(), "b.gb".into(), "pass", "no-reference"),
                ("g".into(), "c.gb".into(), "fail", "missing"),
                ("h".into(), "a.gb".into(), "pass", "missing")
            ]
        );
        let (changed, removed) = split_removed(r);
        assert_eq!(names(&changed).len(), 1);
        assert_eq!(names(&removed).len(), 2);
        assert!(removed.iter().all(|r| r.now.is_none()));

        b.results.clear();
        assert!(regressions(Some(&b), &now, Scope::Full).is_empty());
    }

    #[test]
    fn baseline_round_trip_and_strictness() {
        let b = Baseline::from_results(
            "v7.0",
            &[
                result("z", Tier::Must, "b.gb", Status::NoReference),
                result("z", Tier::Must, "a.gb", Status::Pass),
                result("a", Tier::Stretch, "a.gb", Status::Unsupported),
            ],
        );
        let json = b.to_json();
        assert!(json.find("\"a\"").unwrap() < json.find("\"z\"").unwrap());
        assert!(json.contains("\"no-reference\""));
        assert_eq!(Baseline::parse(&json).unwrap(), b);
        assert!(Baseline::parse("{\"roms_release\":\"v7.0\",\"results\":{},\"x\":1}").is_err());
        assert!(
            Baseline::parse("{\"roms_release\":\"v7.0\",\"results\":{\"g\":{\"a\":\"maybe\"}}}")
                .is_err()
        );
        assert!(Baseline::parse("not json").is_err());
    }

    #[test]
    fn gate_and_exit_codes() {
        let rs = [
            result("a", Tier::Must, "1", Status::Pass),
            result("a", Tier::Must, "2", Status::NoReference),
            result("b", Tier::Should, "1", Status::Fail),
        ];
        assert_eq!(gate_failures(&rs, Tier::Must).len(), 1);
        assert_eq!(gate_failures(&rs, Tier::Should).len(), 1);
        assert_eq!(gate_failures(&rs, Tier::Stretch).len(), 0);
        assert_eq!(exit_code(0, 0), 0);
        assert_eq!(exit_code(1, 0), 1);
        assert_eq!(exit_code(0, 3), 1);
        assert_eq!(exit_code(2, 3), 1);
    }

    #[test]
    fn baseline_update_refused_on_regressions() {
        assert!(may_update_baseline(&[]).is_ok());
        let r = Regression {
            group: "g".into(),
            rom: "a".into(),
            was: Status::Pass,
            now: Some(Status::Fail),
        };
        assert!(
            may_update_baseline(&[r])
                .unwrap_err()
                .contains("1 regression")
        );
    }

    #[test]
    fn slowest_is_sorted_and_stable() {
        let mut rs: Vec<RomResult> = (0..15)
            .map(|i| {
                let mut r = result("g", Tier::Must, &format!("{i:02}.gb"), Status::Pass);
                r.wall = Duration::from_millis((i % 5) as u64);
                r
            })
            .collect();
        rs.reverse();
        let s: Vec<&str> = slowest(&rs, 10).iter().map(|r| r.rom.as_str()).collect();
        assert_eq!(
            s,
            [
                "04.gb", "09.gb", "14.gb", "03.gb", "08.gb", "13.gb", "02.gb", "07.gb", "12.gb",
                "01.gb"
            ]
        );
    }

    #[test]
    fn markdown_lists_every_rom() {
        let mut rs = vec![
            result("g1", Tier::Must, "x/a.gb", Status::Pass),
            result("g2", Tier::Stretch, "x/b.gb", Status::Fail),
        ];
        rs[1].detail = "a|b".into();
        let report = Report {
            manifest: "m".into(),
            roms_release: "v7.0".into(),
            model: "cgb-c".into(),
            selection: Selection::default(),
            jobs: 1,
            wall_time_s: 1.0,
            summary: summarize(&rs),
            gate: None,
            baseline_found: false,
            regressions: vec![],
            removed_from_baseline: vec![],
            results: &rs,
        };
        let md = report.to_markdown(&[("g1".into(), Tier::Must), ("g2".into(), Tier::Stretch)]);
        assert!(md.contains("| `x/a.gb` | pass |"));
        assert!(md.contains("| `x/b.gb` | fail |"));
        assert!(md.contains("a\\|b"));
        assert!(md.contains("### g2 (stretch)"));
        assert!(md.contains("Overall: 75.0%"));
    }
}
