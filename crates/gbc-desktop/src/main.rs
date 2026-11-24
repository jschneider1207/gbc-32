//! Desktop front end (milestone M7, docs/plan/stage-1.md): CLI, ROM and save
//! handling (ADR 0023), the headless run, and the interactive window
//! (`window`, `video`, `input`), DMG compatibility palettes
//! (`compat_palette`, ADR 0025), with audio output (`audio`, `dsp`) driving
//! frame pacing (`pacing`, ADR 0024). Input recording and replay
//! (`input_log`, ADR 0030) and the per-frame log (`frame_log`, `dwm`)
//! measure stutter.

mod args;
mod audio;
mod compat_palette;
mod dsp;
mod dwm;
mod frame_log;
mod frame_stats;
mod headless;
mod input;
mod input_log;
mod pacing;
mod png;
mod save;
mod save_writer;
mod session;
mod shutdown;
mod video;
mod window;

use std::process::ExitCode;

use args::Command;
use input_log::{Driver, Inputs, Recording};
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
    let recording = match args
        .replay_input
        .as_deref()
        .map(Recording::load)
        .transpose()
    {
        Ok(r) => r,
        Err(msg) => {
            eprintln!("error: {msg}");
            return ExitCode::from(1);
        }
    };
    let opts = Options {
        rom_path: &args.rom,
        now: session::unix_now,
        sync_rtc_on_load: true,
        dmg_palette: args.dmg_palette,
        replay: recording.as_ref().map(|r| &r.header),
    };
    let inputs = Inputs {
        record: args.record_input.as_deref(),
        replay: recording.as_ref(),
        hashes: args.frame_hashes.as_deref(),
    };
    if !args.headless {
        let title = format!(
            "gbc-32 \u{2014} {}",
            args.rom
                .file_name()
                .map_or_else(|| args.rom.to_string_lossy(), |n| n.to_string_lossy())
        );
        return match session::run(&opts, |emu, autosave, start| {
            let driver = Driver::open(&inputs, start)?;
            let log =
                frame_log::from_env(std::time::Instant::now(), &start.rom_title).transpose()?;
            window::run(
                emu,
                autosave,
                title,
                args.scale,
                args.frames,
                args.screenshot.as_deref(),
                driver,
                log,
                args.no_input,
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
    match headless::run(&opts, args.frames, args.screenshot.as_deref(), &inputs) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
    }
}
