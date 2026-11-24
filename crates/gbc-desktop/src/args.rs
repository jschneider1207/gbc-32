//! Command-line parsing (hand-rolled: the front end takes few options and
//! ADR 0023 keeps this step dependency-free).

use std::path::PathBuf;

use crate::compat_palette::Mode;

pub const USAGE: &str = "usage: gbc-desktop <rom.gb|rom.gbc> [--headless] [--frames N] [--scale K]
                  [--screenshot out.png]
                  [--dmg-palette auto|gray]
                  [--record-input FILE | --replay-input FILE]
                  [--frame-hashes FILE] [--no-input]

  --headless     run without a window or audio device (needs --frames,
                 or --replay-input)
  --frames N     emulate N frames, then exit 0 (N >= 1; the window closes
                 itself and the save is written as on Esc)
  --screenshot PATH
                 on exit, write the last complete frame (160x144) as a PNG
  --scale K      window scale, integer 1-32 (default 4)
  --dmg-palette auto|gray
                 colors for DMG-only games: the CGB boot ROM's pick for
                 the title (default), or grayscale
  --record-input FILE
                 record the buttons held each frame, and the starting
                 cartridge RAM and clock, to FILE; key M marks the last
                 frame (prints `mark at frame N`)
  --replay-input FILE
                 play a recording back instead of the keyboard and pads,
                 from its own starting save (the .sav is neither read nor
                 written), then exit at its end (or after --frames)
  --no-input     ignore the keyboard and pads (unattended runs, e.g. a
                 game's title or demo with GBC_FRAME_LOG)
  --frame-hashes FILE
                 write `<frame> <ready> <blank> <hash>` per frame, to
                 compare two runs
  -h, --help     print this text

Environment: GBC_FRAME_STATS=1 prints frame-pacing statistics every 4 s;
GBC_FRAME_LOG=FILE writes one CSV row per frame (window only, see
docs/desktop-diagnostics.md).";

pub const DEFAULT_SCALE: u32 = 4;
const MAX_SCALE: u32 = 32;

#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub rom: PathBuf,
    pub headless: bool,
    /// `Some(n)`: stop after n frames (always set when headless).
    pub frames: Option<u64>,
    /// Write the last frame here as a PNG on exit.
    pub screenshot: Option<PathBuf>,
    /// Integer window scale, for the later window step.
    pub scale: u32,
    /// Colors for DMG-only cartridges.
    pub dmg_palette: Mode,
    /// `--record-input`: write the input recording here (ADR 0030).
    pub record_input: Option<PathBuf>,
    /// `--replay-input`: play this recording back.
    pub replay_input: Option<PathBuf>,
    /// `--frame-hashes`: write per-frame picture hashes here.
    pub frame_hashes: Option<PathBuf>,
    /// `--no-input`: hold no buttons, whatever is pressed.
    pub no_input: bool,
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
    let mut screenshot: Option<PathBuf> = None;
    let mut scale = DEFAULT_SCALE;
    let mut dmg_palette = Mode::Auto;
    let mut record_input: Option<PathBuf> = None;
    let mut replay_input: Option<PathBuf> = None;
    let mut frame_hashes: Option<PathBuf> = None;
    let mut no_input = false;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--headless" => headless = true,
            "--no-input" => no_input = true,
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
            "--screenshot" => {
                let v = it.next().ok_or("--screenshot needs a value")?;
                if screenshot.replace(PathBuf::from(v)).is_some() {
                    return Err("--screenshot given twice".into());
                }
            }
            "--record-input" | "--replay-input" | "--frame-hashes" => {
                let v = it.next().ok_or_else(|| format!("{arg} needs a value"))?;
                let slot = match arg.as_str() {
                    "--record-input" => &mut record_input,
                    "--replay-input" => &mut replay_input,
                    _ => &mut frame_hashes,
                };
                if slot.replace(PathBuf::from(v)).is_some() {
                    return Err(format!("{arg} given twice"));
                }
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
    if headless && frames.is_none() && replay_input.is_none() {
        return Err("--headless needs --frames N or --replay-input (it would never exit)".into());
    }
    if record_input.is_some() && replay_input.is_some() {
        return Err("--record-input and --replay-input can't be combined".into());
    }
    Ok(Command::Run(Args {
        rom,
        headless,
        frames,
        screenshot,
        scale,
        dmg_palette,
        record_input,
        replay_input,
        frame_hashes,
        no_input,
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
        assert!(!a.headless);
        assert_eq!(a.frames, None);
        assert_eq!(a.screenshot, None);
        assert_eq!(a.scale, 4);
        assert_eq!(a.dmg_palette, Mode::Auto);
        assert_eq!(a.record_input, None);
        assert_eq!(a.replay_input, None);
        assert_eq!(a.frame_hashes, None);
        assert!(!a.no_input);
        assert!(run(&["x.gb", "--no-input"]).no_input);
    }

    #[test]
    fn input_recording_options() {
        let a = run(&["x.gb", "--record-input", "r.txt"]);
        assert_eq!(a.record_input, Some(PathBuf::from("r.txt")));
        // A headless replay ends with the recording: no --frames needed.
        let a = run(&[
            "--headless",
            "--replay-input",
            "r.txt",
            "--frame-hashes",
            "h.txt",
            "x.gb",
        ]);
        assert!(a.headless && a.frames.is_none());
        assert_eq!(a.replay_input, Some(PathBuf::from("r.txt")));
        assert_eq!(a.frame_hashes, Some(PathBuf::from("h.txt")));
    }

    #[test]
    fn headless_with_frames_and_scale() {
        let a = run(&["--headless", "--frames", "60", "x.gbc", "--scale", "2"]);
        assert!(a.headless);
        assert_eq!(a.frames, Some(60));
        assert_eq!(a.scale, 2);
    }

    #[test]
    fn window_mode_accepts_frames() {
        let a = run(&["--frames", "5", "x.gb"]);
        assert!(!a.headless);
        assert_eq!(a.frames, Some(5));
    }

    #[test]
    fn screenshot_path_in_either_mode() {
        let a = run(&["x.gb", "--screenshot", "o.png", "--frames", "2"]);
        assert_eq!(a.screenshot, Some(PathBuf::from("o.png")));
        let a = run(&[
            "--headless",
            "--frames",
            "2",
            "--screenshot",
            "p.png",
            "x.gb",
        ]);
        assert_eq!(a.screenshot, Some(PathBuf::from("p.png")));
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
            &["--headless", "--frames", "0", "x.gb"],
            &["--headless", "--frames", "-3", "x.gb"],
            &["--headless", "--frames", "x.gb"],
            &["--screenshot", "x.gb"],
            &["x.gb", "--screenshot"],
            &["--screenshot", "a.png", "--screenshot", "b.png", "x.gb"],
            &["--scale", "0", "x.gb"],
            &["--scale", "33", "x.gb"],
            &["--scale", "x.gb"],
            &["a.gb", "b.gb"],
            &["--bogus", "a.gb"],
            &["--dmg-palette", "grey", "a.gb"],
            &["a.gb", "--dmg-palette"],
            &["--headless", "--frames", "1", "--frames", "2", "a.gb"],
            &["a.gb", "--record-input"],
            &["a.gb", "--record-input", "r", "--replay-input", "r"],
            &["a.gb", "--replay-input", "r", "--replay-input", "s"],
            &["--headless", "--record-input", "r", "a.gb"],
        ] {
            assert!(p(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
