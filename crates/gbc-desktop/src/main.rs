//! Desktop front end (milestone M7, docs/plan/stage-1.md): CLI, ROM and save
//! handling (ADR 0023), the headless run, and the interactive window
//! (`window`, `video`, `input`), DMG compatibility palettes
//! (`compat_palette`, ADR 0025), with audio output (`audio`, `dsp`) driving
//! frame pacing (`pacing`, ADR 0024).

mod args;
mod audio;
mod compat_palette;
mod dsp;
mod headless;
mod input;
mod pacing;
mod png;
mod save;
mod session;
mod shutdown;
mod video;
mod window;

use std::process::ExitCode;

use args::Command;
use session::Options;

fn main() -> ExitCode {
    let args = match args::parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            println!("{}", args::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(Command::Run(a)) => a,
        Err(msg) => {
            eprintln!("error: {msg}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    shutdown::install();
    let opts = Options {
        rom_path: &args.rom,
        now: session::unix_now,
        sync_rtc_on_load: true,
        dmg_palette: args.dmg_palette,
    };
    if !args.headless {
        let title = format!(
            "gbc-32 \u{2014} {}",
            args.rom
                .file_name()
                .map_or_else(|| args.rom.to_string_lossy(), |n| n.to_string_lossy())
        );
        return match session::run(&opts, |emu, autosave| {
            window::run(
                emu,
                autosave,
                title,
                args.scale,
                args.frames,
                args.screenshot.as_deref(),
            )
        })
        .and_then(|r| r)
        {
            Ok(()) => ExitCode::SUCCESS,
            Err(msg) => {
                eprintln!("error: {msg}");
                ExitCode::from(1)
            }
        };
    }
    let frames = args.frames.expect("parse requires --frames when headless");
    match headless::run(&opts, frames, args.screenshot.as_deref()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
    }
}
