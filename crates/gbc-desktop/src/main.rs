//! Desktop front end (milestone M7, docs/plan/stage-1.md). This step is the
//! part that needs no window, audio or gamepad library: CLI, ROM and save
//! handling (ADR 0023) and the headless run. Video, audio and input modules
//! come next and call `session::run` the way `headless` does.

mod args;
mod headless;
mod save;
mod session;

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
    let opts = Options {
        rom_path: &args.rom,
        now: session::unix_now,
        sync_rtc_on_load: true,
    };
    let Some(frames) = args.headless_frames else {
        eprintln!("the window front end is not implemented yet (M7 next step); use --headless");
        return ExitCode::from(2);
    };
    match headless::run(&opts, frames) {
        Ok(_) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("error: {msg}");
            ExitCode::from(1)
        }
    }
}
