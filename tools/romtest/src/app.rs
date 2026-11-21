//! Ties it together: manifest → selection → parallel runs → checks →
//! outputs, baseline and exit code.

use std::cell::{Cell, RefCell};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use crate::checks::{self, CheckResult, Ctx};
use crate::cli::Args;
use crate::machine::{End, RunPlan, RunState};
use crate::manifest::{self, Check, Exit, Manifest};
use crate::report::{
    self, Baseline, GateResult, Outcome, Report, RomResult, Selection, rom_status,
};

/// Runs one ROM on an emulator. `Err` is a load error (status `crash`).
pub type Backend = dyn Fn(&[u8], &RunPlan, &mut RunState) -> Result<(), String> + Sync;

/// Where the runner reads and writes.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `test-roms/`
    pub roms_dir: PathBuf,
    /// `target/acceptance/`
    pub out_dir: PathBuf,
    /// `acceptance/results/<manifest>.json`, next to the manifest.
    pub baseline: PathBuf,
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
    backend: &Backend,
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
    let baseline = load_baseline(&paths.baseline)?;

    let plans: Vec<RunPlan> = manifest
        .groups
        .iter()
        .map(|g| RunPlan::new(g.exit, &g.input))
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

    let n_jobs = args
        .jobs
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get()));
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

    let mut results = execute(&jobs, &manifest, paths, backend, n_jobs)?;
    results.sort_by(|a, b| (a.group_index, &a.rom).cmp(&(b.group_index, &b.rom)));
    let wall = wall_start.elapsed();

    let summary = report::summarize(&results);
    let regressions = report::regressions(baseline.as_ref(), &results);
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

/// Runs every job on `n_jobs` threads. Each ROM gets its own emulator.
/// Longest runs start first; the result order is restored by the caller.
fn execute(
    jobs: &[Job<'_>],
    manifest: &Manifest,
    paths: &Paths,
    backend: &Backend,
    n_jobs: usize,
) -> Result<Vec<RomResult>, String> {
    let mut order: Vec<usize> = (0..jobs.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(jobs[i].plan.exit.limit_clocks()));
    let next = AtomicUsize::new(0);
    let abort = AtomicBool::new(false);
    let results = Mutex::new(Vec::with_capacity(jobs.len()));
    let first_error = Mutex::new(None::<String>);
    std::thread::scope(|s| {
        for _ in 0..n_jobs.min(jobs.len()) {
            s.spawn(|| {
                while !abort.load(Ordering::Relaxed) {
                    let k = next.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = order.get(k) else { break };
                    match run_job(&jobs[i], manifest, paths, backend) {
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
    backend: &Backend,
) -> Result<RomResult, String> {
    let start = Instant::now();
    let group = &manifest.groups[job.group_index];
    let rom_path = paths.roms_dir.join(&job.rom);
    let rom =
        std::fs::read(&rom_path).map_err(|e| format!("reading {}: {e}", rom_path.display()))?;

    let mut st = RunState::default();
    let outcome =
        match catch_panic(|| backend(&rom, job.plan, &mut st)) {
            Ok(Ok(())) => Outcome::Ran(st.end.ok_or_else(|| {
                format!("internal error: {} ended without an exit reason", job.rom)
            })?),
            Ok(Err(e)) => Outcome::LoadError(e),
            Err(msg) => Outcome::Panic(msg),
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
        Outcome::Ran(End::Timeout) => match group.exit {
            Exit::LdBB { timeout_s } => format!("no LD B,B within {timeout_s} s"),
            _ => String::new(),
        },
        Outcome::Ran(End::Completed | End::LdBB) => String::new(),
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
        };
        job.checks
            .iter()
            .map(|&(i, c)| checks::evaluate(c, i, &ctx))
            .collect::<Result<_, _>>()?
    } else {
        // The emulator never reached an exit, but a missing reference is
        // still reported (it decides the status; see `rom_status`).
        job.checks
            .iter()
            .map(|&(_, c)| checks::not_run(c, &paths.roms_dir, &job.rom))
            .collect()
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
                r.now.name()
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

// ------------------------------------------------------- panic capture ---

thread_local! {
    static CAPTURING: Cell<bool> = const { Cell::new(false) };
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Installs a panic hook that records panics inside [`catch_panic`]
/// (message and location) instead of printing them; other panics go to the
/// default hook.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if CAPTURING.with(Cell::get) {
            LAST_PANIC.with(|p| *p.borrow_mut() = Some(info.to_string().replace('\n', " ")));
        } else {
            default(info);
        }
    }));
}

/// Runs `f`, turning a panic into `Err(message)`.
fn catch_panic<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    LAST_PANIC.with(|p| p.borrow_mut().take());
    CAPTURING.with(|c| c.set(true));
    let r = catch_unwind(AssertUnwindSafe(f));
    CAPTURING.with(|c| c.set(false));
    r.map_err(|payload| {
        LAST_PANIC
            .with(|p| p.borrow_mut().take())
            .or_else(|| payload.downcast_ref::<&str>().map(|s| s.to_string()))
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic with a non-string payload".into())
    })
}

/// The real backend.
pub fn core_backend(rom: &[u8], plan: &RunPlan, st: &mut RunState) -> Result<(), String> {
    crate::machine::run_core(rom, plan, st)
}

#[cfg(test)]
mod tests;
