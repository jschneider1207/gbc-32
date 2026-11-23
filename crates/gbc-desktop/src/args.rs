//! Command-line parsing (hand-rolled: the front end takes few options and
//! ADR 0023 keeps this step dependency-free).

use std::path::PathBuf;

use crate::compat_palette::Mode;

pub const USAGE: &str = "usage: gbc-desktop <rom.gb|rom.gbc> [--headless --frames N] [--scale K]
                  [--dmg-palette auto|gray]

  --headless     run without a window or audio device (needs --frames)
  --frames N     with --headless: emulate N frames, then exit 0 (N >= 1)
  --scale K      window scale, integer 1-32 (default 4)
  --dmg-palette auto|gray
                 colors for DMG-only games: the CGB boot ROM's pick for
                 the title (default), or grayscale
  -h, --help     print this text";

pub const DEFAULT_SCALE: u32 = 4;
const MAX_SCALE: u32 = 32;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub rom: PathBuf,
    /// `Some(n)` in headless mode: the number of frames to run.
    pub headless_frames: Option<u64>,
    /// Integer window scale, for the later window step.
    pub scale: u32,
    /// Colors for DMG-only cartridges.
    pub dmg_palette: Mode,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Run(Args),
}

/// Parses the arguments after the program name. `Err` is a message for
/// stderr (exit code 2).
pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Result<Command, String> {
    let mut rom: Option<PathBuf> = None;
    let mut headless = false;
    let mut frames: Option<u64> = None;
    let mut scale = DEFAULT_SCALE;
    let mut dmg_palette = Mode::Auto;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--headless" => headless = true,
            "--frames" => {
                let v = it.next().ok_or("--frames needs a value")?;
                if frames.is_some() {
                    return Err("--frames given twice".into());
                }
                frames = Some(match v.parse::<u64>() {
                    Ok(n) if n >= 1 => n,
                    _ => return Err(format!("--frames: expected an integer >= 1, got `{v}`")),
                });
            }
            "--scale" => {
                let v = it.next().ok_or("--scale needs a value")?;
                scale = match v.parse::<u32>() {
                    Ok(n) if (1..=MAX_SCALE).contains(&n) => n,
                    _ => {
                        return Err(format!(
                            "--scale: expected an integer 1-{MAX_SCALE}, got `{v}`"
                        ));
                    }
                };
            }
            "--dmg-palette" => {
                let v = it.next().ok_or("--dmg-palette needs a value")?;
                dmg_palette = match v.as_str() {
                    "auto" => Mode::Auto,
                    "gray" => Mode::Gray,
                    _ => return Err(format!("--dmg-palette: expected auto or gray, got `{v}`")),
                };
            }
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option `{s}`")),
            _ => {
                if rom.replace(PathBuf::from(&arg)).is_some() {
                    return Err(format!("more than one ROM given (extra: `{arg}`)"));
                }
            }
        }
    }
    let rom = rom.ok_or("no ROM given")?;
    let headless_frames = match (headless, frames) {
        (true, Some(n)) => Some(n),
        (true, None) => return Err("--headless needs --frames N (it would never exit)".into()),
        (false, Some(_)) => return Err("--frames is only valid with --headless".into()),
        (false, None) => None,
    };
    Ok(Command::Run(Args {
        rom,
        headless_frames,
        scale,
        dmg_palette,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(a: &[&str]) -> Result<Command, String> {
        parse(a.iter().map(|s| s.to_string()))
    }

    fn run(a: &[&str]) -> Args {
        match p(a) {
            Ok(Command::Run(a)) => a,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn rom_only_defaults() {
        let a = run(&["game.gb"]);
        assert_eq!(a.rom, PathBuf::from("game.gb"));
        assert_eq!(a.headless_frames, None);
        assert_eq!(a.scale, 4);
        assert_eq!(a.dmg_palette, Mode::Auto);
    }

    #[test]
    fn headless_with_frames_and_scale() {
        let a = run(&["--headless", "--frames", "60", "x.gbc", "--scale", "2"]);
        assert_eq!(a.headless_frames, Some(60));
        assert_eq!(a.scale, 2);
    }

    #[test]
    fn dmg_palette_choice() {
        assert_eq!(
            run(&["x.gb", "--dmg-palette", "gray"]).dmg_palette,
            Mode::Gray
        );
        assert_eq!(
            run(&["--dmg-palette", "auto", "x.gb"]).dmg_palette,
            Mode::Auto
        );
    }

    #[test]
    fn help_wins() {
        assert_eq!(p(&["--help"]), Ok(Command::Help));
    }

    #[test]
    fn bad_usage_is_an_error() {
        for bad in [
            &[][..],
            &["--headless", "x.gb"],
            &["--frames", "5", "x.gb"],
            &["--headless", "--frames", "0", "x.gb"],
            &["--headless", "--frames", "-3", "x.gb"],
            &["--headless", "--frames", "x.gb"],
            &["--scale", "0", "x.gb"],
            &["--scale", "33", "x.gb"],
            &["--scale", "x.gb"],
            &["a.gb", "b.gb"],
            &["--bogus", "a.gb"],
            &["--dmg-palette", "grey", "a.gb"],
            &["a.gb", "--dmg-palette"],
            &["--headless", "--frames", "1", "--frames", "2", "a.gb"],
        ] {
            assert!(p(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
