//! Ties it together: manifest → selection → parallel runs → checks →
//! outputs, baseline and exit code.

use std::ffi::OsString;
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::checks::{self, CheckResult, Ctx};
use crate::cli::{self, Args};
use crate::integrity::{self, RomHashes};
use crate::machine::{CoreFailure, End, RunPlan, RunState};
use crate::manifest::{self, Check, Exit, Manifest};
use crate::report::{
    self, Baseline, GateResult, Outcome, Report, RomResult, Scope, Selection, rom_status,
};

/// Runs one ROM on an emulator. Only the calls into the emulator core are
/// inside the panic boundary (`machine::core_call`); `Err` is the core
/// rejecting the ROM or panicking (status `crash`). A panic that escapes
/// is a runner bug (exit 2).
pub type Backend<'a> =
    dyn Fn(&[u8], &RunPlan, &mut RunState) -> Result<(), CoreFailure> + Sync + 'a;

/// The whole program behind `main`: parses `argv`, runs, and maps every
/// failure to an exit code. Usage errors, runner errors and panics outside
/// the emulator core are all exit 2, never 1.
pub fn main_with(
    argv: impl IntoIterator<Item = OsString>,
    root: &Path,
    backend: &Backend<'_>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> u8 {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let args = match cli::parse(argv) {
            Ok(cli::Command::Run(args)) => args,
            Ok(cli::Command::Help) => {
                let _ = writeln!(out, "{}", cli::USAGE);
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        let mut paths = Paths::new(root, &args.manifest)?;
        if let Some(dir) = &args.roms_dir {
            paths.roms_dir = dir.clone();
        }
        run(&args, &paths, backend, out)
    }));
    match result {
        Ok(Ok(code)) => code,
        Ok(Err(e)) => {
            let _ = writeln!(err, "romtest: error: {e}");
            let _ = writeln!(
                err,
                "romtest: runner error, exit 2 (results are not meaningful)"
            );
            2
        }
        Err(_) => {
            let _ = writeln!(err, "romtest: internal error (panic in the runner), exit 2");
            2
        }
    }
}

/// Where the runner reads and writes.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `test-roms/`
    pub roms_dir: PathBuf,
    /// `target/acceptance/`
    pub out_dir: PathBuf,
    /// `acceptance/results/<manifest>.json`, next to the manifest.
    pub baseline: PathBuf,
    /// `acceptance/test-roms.sha256`, next to the manifest.
    pub rom_hashes: PathBuf,
    /// The manifest file name without extension (e.g. `stage1`).
    pub stem: String,
}

impl Paths {
    /// `test-roms/` and `target/` live in the workspace `root`; the
    /// baseline lives in `results/` next to the manifest.
    pub fn new(root: &Path, manifest: &Path) -> Result<Self, String> {
        let stem = manifest
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("bad manifest path {}", manifest.display()))?
            .to_owned();
        let dir = manifest.parent().unwrap_or(Path::new(""));
        Ok(Self {
            roms_dir: root.join("test-roms"),
            out_dir: root.join("target").join("acceptance"),
            baseline: dir.join("results").join(format!("{stem}.json")),
            rom_hashes: dir.join(integrity::FILE_NAME),
            stem,
        })
    }
}

/// One `(group, ROM)` to run.
struct Job<'a> {
    group_index: usize,
    rom: String,
    plan: &'a RunPlan,
    checks: Vec<(usize, &'a Check)>,
}

/// Runs the selection and writes every output. `Ok` carries exit code 0
/// or 1; `Err` is a runner error (exit 2).
pub fn run(
    args: &Args,
    paths: &Paths,
    backend: &Backend<'_>,
    out: &mut dyn Write,
) -> Result<u8, String> {
    let wall_start = Instant::now();
    let text = std::fs::read_to_string(&args.manifest)
        .map_err(|e| format!("reading {}: {e}", args.manifest.display()))?;
    let manifest =
        manifest::parse(&text).map_err(|e| format!("{}: {e}", args.manifest.display()))?;
    manifest::check_release(&manifest, &paths.roms_dir)?;
    let files = manifest::list_files(&paths.roms_dir)?;
    let resolved = manifest::resolve(&manifest, &files)
        .map_err(|e| format!("{}: {e}", args.manifest.display()))?;
    let n_jobs = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));

    // Nothing runs until test-roms/ matches the committed integrity list.
    let hash_start = Instant::now();
    let rom_hashes = integrity::load(&paths.rom_hashes)?;
    let verified = integrity::verify(
        &rom_hashes,
        &manifest.roms_release,
        &paths.roms_dir,
        &files,
        resolved.iter().flatten().map(String::as_str),
        n_jobs,
    )?;
    // Every recolored reference is checked (listed, its count exact) up front.
    checks::verify_recolors(&manifest, &paths.roms_dir, &|p, b| {
        rom_hashes.check_bytes(p, b)
    })
    .map_err(|e| format!("checking recolored references before running: {e}"))?;
    let hash_time = hash_start.elapsed();

    let baseline = match load_baseline(&paths.baseline)? {
        Some(b) if b.roms_release != manifest.roms_release => {
            if !args.update_baseline {
                return Err(format!(
                    "{} is for roms_release {:?}, but the manifest is {:?} \
                     (rewrite it with --update-baseline)",
                    paths.baseline.display(),
                    b.roms_release,
                    manifest.roms_release
                ));
            }
            // A baseline from another release can't be compared; it is
            // rewritten below.
            let _ = writeln!(
                out,
                "romtest: baseline is for {:?}; not comparing, rewriting it",
                b.roms_release
            );
            None
        }
        b => b,
    };
    if args.gate.is_some() && !args.update_baseline && baseline.is_none() {
        return Err(format!(
            "no baseline at {}: create one with --update-baseline \
             (--gate needs it to check for regressions)",
            paths.baseline.display()
        ));
    }

    let plans: Vec<RunPlan> = manifest
        .groups
        .iter()
        .map(|g| RunPlan {
            lockup_runs_on: g.lockup_runs_on,
            ..RunPlan::new(g.exit, &g.input)
        })
        .collect();
    let jobs = select(&manifest, &resolved, &plans, args)?;
    if let Some(tier) = args.gate
        && !jobs
            .iter()
            .any(|j| manifest.groups[j.group_index].tier == tier)
    {
        return Err(format!("--gate {tier}: no selected ROM is in that tier"));
    }

    let failed_dir = paths.out_dir.join(&paths.stem).join("failed");
    if failed_dir.exists() {
        std::fs::remove_dir_all(&failed_dir)
            .map_err(|e| format!("clearing {}: {e}", failed_dir.display()))?;
    }
    std::fs::create_dir_all(&paths.out_dir)
        .map_err(|e| format!("creating {}: {e}", paths.out_dir.display()))?;

    let selection = Selection {
        filter: args.filter.as_ref().map(|f| f.as_str().to_owned()),
        groups: args.groups.clone(),
        only_check: args.only_check.map(|k| k.name().to_owned()),
    };
    let _ = writeln!(
        out,
        "romtest: {}: {} ROM runs ({}), jobs {n_jobs}",
        args.manifest.display(),
        jobs.len(),
        selection.describe()
    );
    let _ = writeln!(
        out,
        "romtest: verified {} test-roms files ({:.1} MB) against {} in {:.2} s",
        verified.files,
        verified.bytes as f64 / 1e6,
        paths.rom_hashes.display(),
        hash_time.as_secs_f64()
    );

    let loaded = LoadedHashes::default();
    let mut results = execute(
        &jobs,
        &manifest,
        paths,
        &rom_hashes,
        &loaded,
        backend,
        n_jobs,
    )?;
    results.sort_by(|a, b| (a.group_index, &a.rom).cmp(&(b.group_index, &b.rom)));
    let wall = wall_start.elapsed();
    let _ = writeln!(
        out,
        "romtest: re-verified {} loaded files ({:.1} MB) at load time, {:.2} s hashing (summed over jobs)",
        loaded.files.load(Ordering::Relaxed),
        loaded.bytes.load(Ordering::Relaxed) as f64 / 1e6,
        Duration::from_nanos(loaded.nanos.load(Ordering::Relaxed)).as_secs_f64()
    );

    let summary = report::summarize(&results);
    let in_scope = |group: &str, rom: &str| selected(&manifest, args, group, rom);
    let scope = if selection.is_full() {
        Scope::Full
    } else {
        Scope::Partial(&in_scope)
    };
    let all_regressions = report::regressions(baseline.as_ref(), &results, scope);
    // `--allow-removed` (only with `--update-baseline`, so a full run):
    // entries with no result are dropped from the baseline, not regressions.
    let (regressions, removed_from_baseline) = if args.allow_removed {
        report::split_removed(all_regressions)
    } else {
        (all_regressions, Vec::new())
    };
    let gate = args.gate.map(|tier| {
        let failures = report::gate_failures(&results, tier).len();
        GateResult {
            tier,
            failures,
            passed: failures == 0,
        }
    });
    let rep = Report {
        manifest: paths.stem.clone(),
        roms_release: manifest.roms_release.clone(),
        model: manifest.model.clone(),
        selection,
        jobs: n_jobs,
        wall_time_s: wall.as_secs_f64(),
        summary,
        gate,
        baseline_found: baseline.is_some(),
        regressions,
        removed_from_baseline,
        results: &results,
    };
    let json_path = paths.out_dir.join(format!("{}.json", paths.stem));
    let md_path = paths.out_dir.join(format!("{}.md", paths.stem));
    write_file(&json_path, &rep.to_json())?;
    let group_order: Vec<_> = manifest
        .groups
        .iter()
        .map(|g| (g.id.clone(), g.tier))
        .collect();
    write_file(&md_path, &rep.to_markdown(&group_order))?;

    print_summary(out, &rep, baseline.is_some(), &paths.baseline);
    let _ = writeln!(
        out,
        "wrote {} and {}{}",
        json_path.display(),
        md_path.display(),
        if failed_dir.exists() {
            format!("; failure images in {}", failed_dir.display())
        } else {
            String::new()
        }
    );

    if args.update_baseline {
        match report::may_update_baseline(&rep.regressions) {
            Ok(()) => {
                if !rep.removed_from_baseline.is_empty() {
                    let _ = writeln!(
                        out,
                        "removing {} baseline entr{} with no current result (--allow-removed):",
                        rep.removed_from_baseline.len(),
                        if rep.removed_from_baseline.len() == 1 {
                            "y"
                        } else {
                            "ies"
                        }
                    );
                    for r in &rep.removed_from_baseline {
                        let _ = writeln!(out, "  {} {} (was {})", r.group, r.rom, r.was.name());
                    }
                }
                let b = Baseline::from_results(&manifest.roms_release, &results);
                if let Some(dir) = paths.baseline.parent() {
                    std::fs::create_dir_all(dir)
                        .map_err(|e| format!("creating {}: {e}", dir.display()))?;
                }
                write_file(&paths.baseline, &b.to_json())?;
                let _ = writeln!(out, "baseline updated: {}", paths.baseline.display());
            }
            Err(e) => {
                let _ = writeln!(out, "{e}");
            }
        }
    }

    let gate_failures = rep.gate.as_ref().map_or(0, |g| g.failures);
    let _ = writeln!(out, "total wall time {:.1} s", wall.as_secs_f64());
    Ok(report::exit_code(rep.regressions.len(), gate_failures))
}

fn write_file(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| format!("writing {}: {e}", path.display()))
}

fn load_baseline(path: &Path) -> Result<Option<Baseline>, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Baseline::parse(&text)
            .map(Some)
            .map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("reading {}: {e}", path.display())),
    }
}

/// Whether `(group, rom)` is inside the `--group`/`--filter`/`--only-check`
/// selection. Used for baseline entries, whose ROM or group may no longer
/// exist: a group that is not in the manifest counts as selected unless
/// `--group` leaves it out.
fn selected(manifest: &Manifest, args: &Args, group: &str, rom: &str) -> bool {
    let group_ok = args.groups.is_empty() || args.groups.iter().any(|g| g == group);
    let rom_ok = args.filter.as_ref().is_none_or(|f| f.matches(rom));
    let checks_ok = args.only_check.is_none_or(|kind| {
        manifest
            .groups
            .iter()
            .find(|g| g.id == group)
            .is_none_or(|g| g.checks.iter().any(|c| c.kind() == kind))
    });
    group_ok && rom_ok && checks_ok
}

/// Applies `--group`, `--filter` and `--only-check`.
fn select<'a>(
    manifest: &'a Manifest,
    resolved: &[Vec<String>],
    plans: &'a [RunPlan],
    args: &Args,
) -> Result<Vec<Job<'a>>, String> {
    for id in &args.groups {
        if !manifest.groups.iter().any(|g| &g.id == id) {
            return Err(format!("--group {id:?}: no such group in the manifest"));
        }
    }
    let mut jobs = Vec::new();
    for (gi, g) in manifest.groups.iter().enumerate() {
        if !args.groups.is_empty() && !args.groups.contains(&g.id) {
            continue;
        }
        let checks = checks::selected(&g.checks, args.only_check);
        if checks.is_empty() {
            continue;
        }
        for rom in &resolved[gi] {
            if args.filter.as_ref().is_none_or(|f| f.matches(rom)) {
                jobs.push(Job {
                    group_index: gi,
                    rom: rom.clone(),
                    plan: &plans[gi],
                    checks: checks.clone(),
                });
            }
        }
    }
    if jobs.is_empty() {
        return Err("the selection (--filter/--group/--only-check) matches no ROMs".into());
    }
    Ok(jobs)
}

/// Totals for the files hashed again as they were loaded for a run.
#[derive(Default)]
struct LoadedHashes {
    files: AtomicUsize,
    bytes: AtomicU64,
    nanos: AtomicU64,
}

impl LoadedHashes {
    /// Checks bytes just read from `test-roms/` against the integrity
    /// list, so a file changed after the up-front check can't be used
    /// unverified. `Err` is a runner error (exit 2).
    fn verify(&self, list: &RomHashes, path: &str, bytes: &[u8]) -> Result<(), String> {
        let start = Instant::now();
        let r = list.check_bytes(path, bytes);
        self.files.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes.len() as u64, Ordering::Relaxed);
        self.nanos
            .fetch_add(start.elapsed().as_nanos() as u64, Ordering::Relaxed);
        r
    }
}

/// Runs every job on `n_jobs` threads. Each ROM gets its own emulator.
/// Longest runs start first; the result order is restored by the caller.
fn execute(
    jobs: &[Job<'_>],
    manifest: &Manifest,
    paths: &Paths,
    rom_hashes: &RomHashes,
    loaded: &LoadedHashes,
    backend: &Backend<'_>,
    n_jobs: usize,
) -> Result<Vec<RomResult>, String> {
    /// Stops the other workers if this one unwinds (a runner bug; the
    /// panic itself reaches `main_with` when the scope joins).
    struct AbortOnPanic<'a>(&'a AtomicBool);
    impl Drop for AbortOnPanic<'_> {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.0.store(true, Ordering::Relaxed);
            }
        }
    }
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].plan.exit.limit_clocks()));
    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let results = Mutex::new(Vec::with_capacity(jobs.len()));
    let first_error = Mutex::new(None::<String>);
    std::thread::scope(|s| {
        for _ in 0..n_jobs.min(jobs.len()) {
            s.spawn(|| {
                let _guard = AbortOnPanic(&abort);
                while !abort.load(Ordering::Relaxed) {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = order.get(k) else { break };
                    match run_job(&jobs[i], manifest, paths, rom_hashes, loaded, backend) {
                        Ok(r) => results.lock().expect("results lock").push(r),
                        Err(e) => {
                            abort.store(true, Ordering::Relaxed);
                            first_error.lock().expect("error lock").get_or_insert(e);
                        }
                    }
                }
            });
        }
    });
    if let Some(e) = first_error.into_inner().expect("error lock") {
        return Err(e);
    }
    Ok(results.into_inner().expect("results lock"))
}

fn run_job(
    job: &Job<'_>,
    manifest: &Manifest,
    paths: &Paths,
    rom_hashes: &RomHashes,
    loaded: &LoadedHashes,
    backend: &Backend<'_>,
) -> Result<RomResult, String> {
    let start = Instant::now();
    let group = &manifest.groups[job.group_index];
    let rom_path = paths.roms_dir.join(&job.rom);
    let rom =
        std::fs::read(&rom_path).map_err(|e| format!("reading {}: {e}", rom_path.display()))?;
    loaded.verify(rom_hashes, &job.rom, &rom)?;

    let mut st = RunState::default();
    let outcome =
        match backend(&rom, job.plan, &mut st) {
            Ok(()) => Outcome::Ran(st.end.ok_or_else(|| {
                format!("internal error: {} ended without an exit reason", job.rom)
            })?),
            Err(CoreFailure::Load(e)) => Outcome::LoadError(e),
            Err(CoreFailure::Panic(msg)) => Outcome::Panic(msg),
        };

    let detail = match &outcome {
        Outcome::LoadError(e) => format!("load error: {e}"),
        Outcome::Panic(m) => format!("panic: {m}"),
        Outcome::Ran(End::Locked) => format!(
            "CPU locked up (PC={:#06x})",
            st.registers.map_or(0, |r| r.pc)
        ),
        Outcome::Ran(End::NoProgress) => {
            format!("emulator stopped advancing at clock {}", st.clock)
        }
        Outcome::Ran(End::InputLate { at, now }) => format!(
            "input at clock {at} could not be scheduled on its M-cycle \
             (queue limit): handed over at clock {now}"
        ),
        Outcome::Ran(End::Timeout) => match group.exit {
            Exit::LdBB { timeout_s } => format!("no LD B,B within {timeout_s} s"),
            _ => String::new(),
        },
        Outcome::Ran(End::Completed) => match (st.lockup, group.exit) {
            (Some(l), Exit::RunFrames { frames }) => format!(
                "CPU locked up (PC={:#06x}) at clock {}; ran on to frame {frames} \
                 (gambatte lockup rule)",
                l.pc, l.clock
            ),
            _ => String::new(),
        },
        Outcome::Ran(End::LdBB) => String::new(),
    };

    let check_results: Vec<CheckResult> = if outcome.ran() {
        let frame = st.final_frame();
        let flat = job.rom.replace('/', "__");
        let failed = |i: usize| {
            let base = format!("{}/failed/{}/{flat}.{i}", paths.stem, group.id);
            Some((
                paths.out_dir.clone(),
                format!("{base}.actual.png"),
                format!("{base}.diff.png"),
            ))
        };
        let ctx = Ctx {
            roms_dir: &paths.roms_dir,
            rom: &job.rom,
            serial: &st.serial,
            registers: st.registers,
            final_frame: &frame,
            failure_pngs: &failed,
            verify: &|p, bytes| loaded.verify(rom_hashes, p, bytes),
        };
        job.checks
            .iter()
            .map(|&(i, c)| checks::evaluate(c, i, &ctx))
            .collect::<Result<_, _>>()?
    } else {
        // The emulator never reached an exit (status crash); a missing
        // reference is still reported in the check results.
        job.checks
            .iter()
            .map(|&(_, c)| checks::not_run(c, &paths.roms_dir, &job.rom))
            .collect::<Result<_, _>>()?
    };
    let statuses: Vec<_> = check_results.iter().map(|c| c.status).collect();
    let status = rom_status(&outcome, &statuses);
    let wall = start.elapsed();
    Ok(RomResult {
        group: group.id.clone(),
        tier: group.tier,
        rom: job.rom.clone(),
        status,
        end: outcome.name(),
        detail,
        clock: st.clock,
        frames: st.frames,
        serial: String::from_utf8_lossy(&st.serial).into_owned(),
        registers: st.registers.map(Into::into),
        checks: check_results,
        wall_time_s: (wall.as_secs_f64() * 1000.0).round() / 1000.0,
        group_index: job.group_index,
        wall,
    })
}

fn print_summary(out: &mut dyn Write, rep: &Report<'_>, have_baseline: bool, baseline: &Path) {
    // Regressions first (acceptance/README.md, "Regressions").
    if !rep.regressions.is_empty() {
        let _ = writeln!(out, "\nREGRESSIONS ({}):", rep.regressions.len());
        for r in &rep.regressions {
            let _ = writeln!(
                out,
                "  {} {}: {} -> {}",
                r.group,
                r.rom,
                r.was.name(),
                r.now_name()
            );
        }
    } else if have_baseline {
        let _ = writeln!(out, "\nno regressions against {}", baseline.display());
    } else {
        let _ = writeln!(
            out,
            "\nno baseline at {} (nothing to compare)",
            baseline.display()
        );
    }

    let _ = writeln!(out, "\nscore:");
    for (tier, s) in &rep.summary.tiers {
        let c = &s.counts;
        let _ = writeln!(
            out,
            "  {:<8} {:>5} / {:<5} pass  {}  (fail {}, timeout {}, crash {}, unsupported {}, no-reference {})",
            tier.name(),
            c.pass,
            c.total - c.no_reference,
            fmt_pct(s.score),
            c.fail,
            c.timeout,
            c.crash,
            c.unsupported,
            c.no_reference
        );
    }
    let _ = writeln!(out, "  overall  {}", fmt_pct(rep.summary.overall));

    let slow = report::slowest(rep.results, 10);
    let _ = writeln!(out, "\nslowest ROMs:");
    for r in slow {
        let _ = writeln!(
            out,
            "  {:>7.2} s  {:<12} {} ({})",
            r.wall.as_secs_f64(),
            r.status.name(),
            r.rom,
            r.group
        );
    }

    if let Some(g) = &rep.gate {
        let fails = report::gate_failures(rep.results, g.tier);
        let _ = writeln!(
            out,
            "\ngate {}: {} ({} of {} not passing)",
            g.tier,
            if g.passed { "PASS" } else { "FAIL" },
            g.failures,
            rep.results.iter().filter(|r| r.tier == g.tier).count()
        );
        const SHOWN: usize = 20;
        for r in fails.iter().take(SHOWN) {
            let _ = writeln!(
                out,
                "  {:<12} {} ({}) {}",
                r.status.name(),
                r.rom,
                r.group,
                r.explain()
            );
        }
        if fails.len() > SHOWN {
            let _ = writeln!(out, "  ... and {} more", fails.len() - SHOWN);
        }
    }
}

fn fmt_pct(s: Option<f64>) -> String {
    s.map_or("n/a".into(), |s| format!("{:.1}%", s * 100.0))
}

/// The real backend.
pub fn core_backend(rom: &[u8], plan: &RunPlan, st: &mut RunState) -> Result<(), CoreFailure> {
    crate::machine::run_core(rom, plan, st)
}

#[cfg(test)]
mod tests;
