//! Command line (acceptance/README.md, "Runner contract"). Usage errors are
//! runner errors (exit 2).

use std::ffi::OsString;
use std::path::PathBuf;

use crate::glob::Glob;
use crate::manifest::{CheckKind, Tier};

pub const USAGE: &str = "\
usage: romtest --manifest <path> [options]

  --manifest <path>     acceptance manifest (e.g. acceptance/stage1.toml)
  --filter <glob>       run only ROMs whose path matches (selects every group containing them)
  --group <id>          run only this group (repeatable)
  --only-check <kind>   evaluate only checks of this kind (serial, screenshot, fibonacci, gambatte)
  --gate <tier>         exit 1 unless every selected ROM in this tier passes (must, should, stretch)
  --jobs <n>            parallel ROM runs (default: number of CPU cores)
  --update-baseline     write acceptance/results/<manifest>.json (full runs only; refused on regressions)
  --allow-removed       with --update-baseline: drop baseline entries that no longer have a result
                        (each one is listed); without it they are regressions
  --roms-dir <path>     read test ROMs from here instead of test-roms/ (for testing the runner;
                        still verified against acceptance/test-roms.sha256)
  -h, --help            show this help (only as the sole argument)

Exit codes: 0 ok, 1 regressions or gate failures, 2 runner error.";

#[derive(Debug, Default)]
pub struct Args {
    pub manifest: PathBuf,
    pub filter: Option<Glob>,
    pub groups: Vec<String>,
    pub only_check: Option<CheckKind>,
    pub gate: Option<Tier>,
    pub jobs: Option<usize>,
    pub update_baseline: bool,
    /// Lets `--update-baseline` drop baseline entries with no current
    /// result (ADR 0007 amendments).
    pub allow_removed: bool,
    pub roms_dir: Option<PathBuf>,
}

#[derive(Debug)]
pub enum Command {
    Run(Args),
    Help,
}

pub fn parse<I: IntoIterator<Item = OsString>>(args: I) -> Result<Command, String> {
    let mut out = Args::default();
    let mut manifest = None;
    let args: Vec<OsString> = args.into_iter().collect();
    // Help only on its own: combined with anything else it is a usage
    // error, so a typo'd run can't exit 0 without running.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        if args.len() == 1 {
            return Ok(Command::Help);
        }
        return Err(format!("-h/--help must be the only argument\n\n{USAGE}"));
    }
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        let arg = arg
            .into_string()
            .map_err(|a| format!("argument is not UTF-8: {a:?}"))?;
        if arg == "--update-baseline" {
            out.update_baseline = true;
            continue;
        }
        if arg == "--allow-removed" {
            out.allow_removed = true;
            continue;
        }
        // `--flag value` or `--flag=value`.
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f.to_owned(), Some(v.to_owned())),
            _ => (arg.clone(), None),
        };
        let mut value = || -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => it
                    .next()
                    .ok_or_else(|| format!("{flag} needs a value"))?
                    .into_string()
                    .map_err(|a| format!("argument is not UTF-8: {a:?}")),
            }
        };
        match flag.as_str() {
            "--manifest" => set_once(&mut manifest, PathBuf::from(value()?), &flag)?,
            "--roms-dir" => set_once(&mut out.roms_dir, PathBuf::from(value()?), &flag)?,
            "--filter" => set_once(&mut out.filter, Glob::new(&value()?)?, &flag)?,
            "--group" => out.groups.push(value()?),
            "--only-check" => {
                let v = value()?;
                let kind = CheckKind::parse(&v).ok_or_else(|| {
                    format!(
                        "--only-check {v:?}: not one of serial, screenshot, fibonacci, gambatte"
                    )
                })?;
                set_once(&mut out.only_check, kind, &flag)?;
            }
            "--gate" => {
                let v = value()?;
                let tier = Tier::parse(&v)
                    .ok_or_else(|| format!("--gate {v:?}: not one of must, should, stretch"))?;
                set_once(&mut out.gate, tier, &flag)?;
            }
            "--jobs" => {
                let v = value()?;
                let n = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n >= 1)
                    .ok_or_else(|| format!("--jobs {v:?}: expected a positive integer"))?;
                set_once(&mut out.jobs, n, &flag)?;
            }
            _ => return Err(format!("unknown argument {arg:?}\n\n{USAGE}")),
        }
    }
    out.manifest = manifest.ok_or_else(|| format!("--manifest is required\n\n{USAGE}"))?;
    if out.allow_removed && !out.update_baseline {
        return Err("--allow-removed only makes sense with --update-baseline".into());
    }
    if out.update_baseline
        && (out.filter.is_some() || !out.groups.is_empty() || out.only_check.is_some())
    {
        return Err(
            "--update-baseline is refused with --filter, --group or --only-check: \
             the baseline must cover the whole manifest"
                .into(),
        );
    }
    Ok(Command::Run(out))
}

fn set_once<T>(slot: &mut Option<T>, value: T, flag: &str) -> Result<(), String> {
    if slot.is_some() {
        return Err(format!("{flag} given more than once"));
    }
    *slot = Some(value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(args: &[&str]) -> Result<Args, String> {
        match parse(args.iter().map(OsString::from))? {
            Command::Run(a) => Ok(a),
            Command::Help => Err("help".into()),
        }
    }

    #[test]
    fn parses_all_flags() {
        let a = p(&[
            "--manifest",
            "m.toml",
            "--filter=mooneye/**",
            "--group",
            "a",
            "--group=b",
            "--only-check",
            "serial",
            "--gate",
            "must",
            "--jobs",
            "3",
            "--roms-dir",
            "/tmp/r",
        ])
        .unwrap();
        assert_eq!(a.roms_dir, Some(PathBuf::from("/tmp/r")));
        assert_eq!(a.manifest, PathBuf::from("m.toml"));
        assert_eq!(a.filter.unwrap().as_str(), "mooneye/**");
        assert_eq!(a.groups, ["a", "b"]);
        assert_eq!(a.only_check, Some(CheckKind::Serial));
        assert_eq!(a.gate, Some(Tier::Must));
        assert_eq!(a.jobs, Some(3));
        assert!(!a.update_baseline);
        assert!(
            p(&["--manifest", "m", "--update-baseline"])
                .unwrap()
                .update_baseline
        );
        assert_eq!(p(&["--help"]).unwrap_err(), "help");
        assert_eq!(p(&["-h"]).unwrap_err(), "help");
        let a = p(&["--manifest", "m", "--update-baseline", "--allow-removed"]).unwrap();
        assert!(a.update_baseline && a.allow_removed);
    }

    #[test]
    fn help_must_be_the_only_argument() {
        for args in [
            &["--help", "--manifest", "m"][..],
            &["--manifest", "m", "-h"][..],
            &["--help", "--help"][..],
            &["-h", "--bogus"][..],
        ] {
            let e = p(args).unwrap_err();
            assert!(e.contains("only argument"), "{args:?} => {e}");
        }
    }

    #[test]
    fn rejects_bad_usage() {
        let cases: &[(&[&str], &str)] = &[
            (&[], "--manifest is required"),
            (&["--manifest"], "needs a value"),
            (&["--manifest", "m", "--bogus"], "unknown argument"),
            (&["--manifest", "m", "--gate", "maybe"], "--gate"),
            (
                &["--manifest", "m", "--only-check", "audio"],
                "--only-check",
            ),
            (&["--manifest", "m", "--jobs", "0"], "--jobs"),
            (&["--manifest", "m", "--jobs", "x"], "--jobs"),
            (&["--manifest", "m", "--filter", "a/{b"], "bad glob"),
            (&["--manifest", "m", "--manifest", "n"], "more than once"),
            (&["--manifest", "m", "--allow-removed"], "--update-baseline"),
        ];
        for (args, needle) in cases {
            let e = p(args).unwrap_err();
            assert!(e.contains(needle), "{args:?} => {e}");
        }
    }

    #[test]
    fn update_baseline_refused_with_selection() {
        for extra in [
            &["--filter", "a/*"][..],
            &["--group", "g"][..],
            &["--only-check", "serial"][..],
        ] {
            let mut args = vec!["--manifest", "m", "--update-baseline"];
            args.extend_from_slice(extra);
            assert!(p(&args).unwrap_err().contains("refused"), "{extra:?}");
        }
        // Allowed with --gate and --jobs.
        p(&[
            "--manifest",
            "m",
            "--update-baseline",
            "--gate",
            "must",
            "--jobs",
            "2",
        ])
        .unwrap();
    }
}
