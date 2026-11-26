//! `gbc-pc`: talks to the dev board over the ST-LINK virtual COM port.
//!
//! ```text
//! gbc-pc load <rom> [--port COM5] [--baud 115200] [--retries 5] [--timeout-ms 2000]
//! gbc-pc hello      [--port COM5] [--baud 115200] [--retries 5] [--timeout-ms 2000]
//! ```
//!
//! Exit code 0 on success, 1 on a link or load failure, 2 on a usage or
//! file error. From WSL, run it through `scripts/gbc-pc.sh`.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use gbc_pc::{Client, Config, LoadOutcome, Transport};

const USAGE: &str = "usage:
  gbc-pc load <rom> [--port COM5] [--baud 115200] [--retries 5] [--timeout-ms 2000]
  gbc-pc hello      [--port COM5] [--baud 115200] [--retries 5] [--timeout-ms 2000]";

enum Command {
    Load(String),
    Hello,
}

struct Args {
    cmd: Command,
    port: String,
    baud: u32,
    cfg: Config,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let cmd = match it.next().as_deref() {
        Some("load") => Command::Load(String::new()),
        Some("hello") => Command::Hello,
        Some(c) => return Err(format!("unknown command {c:?}")),
        None => return Err("missing command".into()),
    };
    let mut args = Args {
        cmd,
        port: "COM5".into(),
        baud: 115_200,
        cfg: Config::default(),
    };
    let mut rom = None;
    while let Some(a) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| format!("{name} needs a value"));
        match a.as_str() {
            "--port" => args.port = value("--port")?,
            "--baud" => args.baud = number(&value("--baud")?, "--baud")?,
            "--retries" => args.cfg.retries = number(&value("--retries")?, "--retries")?,
            "--timeout-ms" => {
                let ms: u64 = number(&value("--timeout-ms")?, "--timeout-ms")?;
                args.cfg.timeout = Duration::from_millis(ms);
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s if matches!(args.cmd, Command::Load(_)) && rom.is_none() => {
                rom = Some(s.to_string());
            }
            s => return Err(format!("unexpected argument {s:?}")),
        }
    }
    if let Command::Load(path) = &mut args.cmd {
        *path = rom.ok_or("load needs a ROM path")?;
    }
    if args.baud == 0 {
        return Err("--baud must be > 0".into());
    }
    Ok(args)
}

fn number<N: std::str::FromStr>(s: &str, name: &str) -> Result<N, String> {
    s.parse()
        .map_err(|_| format!("{name}: not a valid number: {s:?}"))
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gbc-pc: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let rom = match &args.cmd {
        Command::Load(path) => match std::fs::read(path) {
            Ok(r) => Some(r),
            Err(e) => {
                eprintln!("gbc-pc: cannot read {path}: {e}");
                return ExitCode::from(2);
            }
        },
        Command::Hello => None,
    };
    let port = match open(&args.port, args.baud) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gbc-pc: {e}");
            return ExitCode::from(2);
        }
    };
    println!("gbc-pc: {} at {} baud, 8N1", args.port, args.baud);
    run(port, &args, rom.as_deref())
}

#[cfg(windows)]
fn open(port: &str, baud: u32) -> std::io::Result<gbc_pc::serial::ComPort> {
    gbc_pc::serial::ComPort::open(port, baud)
}

#[cfg(not(windows))]
fn open(_port: &str, _baud: u32) -> std::io::Result<gbc_pc::sim::SimBoard> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "COM ports are only supported in the Windows build (scripts/gbc-pc.sh)",
    ))
}

fn run<T: Transport>(port: T, args: &Args, rom: Option<&[u8]>) -> ExitCode {
    let mut client = Client::new(port, args.cfg);
    let result = match rom {
        None => client.hello().map(|info| {
            println!(
                "HelloReply: protocol {}, max_chunk {}, flash_len {} ({} KiB)",
                info.protocol,
                info.max_chunk,
                info.flash_len,
                info.flash_len / 1024
            );
        }),
        Some(rom) => load(&mut client, rom),
    };
    let s = client.stats;
    println!(
        "link: resends {}, naks {}, timeouts {}, stale {}, corrupt {}, other {}",
        s.resends, s.naks, s.timeouts, s.stale, s.corrupt, s.other
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gbc-pc: {e}");
            ExitCode::from(1)
        }
    }
}

fn load<T: Transport>(client: &mut Client<T>, rom: &[u8]) -> Result<(), gbc_pc::Error> {
    println!(
        "ROM: {} bytes, FNV-1a 64 {:#018x}",
        rom.len(),
        gbc_pc::fnv::hash(rom)
    );
    let start = Instant::now();
    let mut last_print = start;
    let mut progress = |sent: usize, total: usize| {
        let now = Instant::now();
        if sent == total || now - last_print >= Duration::from_secs(1) {
            last_print = now;
            let secs = (now - start).as_secs_f64();
            println!(
                "  {sent:>9} / {total} bytes  {secs:7.2} s  {:>8.0} B/s",
                rate(sent, secs)
            );
        }
    };
    let outcome = client.load(rom, &mut progress)?;
    let secs = start.elapsed().as_secs_f64();
    match outcome {
        LoadOutcome::AlreadyLoaded => println!("already loaded (length and hash match)"),
        LoadOutcome::Loaded { hash } => println!(
            "loaded {} bytes in {secs:.2} s ({:.0} B/s effective), verified hash {hash:#018x}",
            rom.len(),
            rate(rom.len(), secs)
        ),
    }
    Ok(())
}

fn rate(bytes: usize, secs: f64) -> f64 {
    if secs > 0.0 { bytes as f64 / secs } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        parse_args(s.split_whitespace().map(String::from))
    }

    #[test]
    fn load_with_options() {
        let a =
            parse("load game.gbc --port COM7 --baud 921600 --retries 3 --timeout-ms 500").unwrap();
        assert!(matches!(a.cmd, Command::Load(ref p) if p == "game.gbc"));
        assert_eq!(a.port, "COM7");
        assert_eq!(a.baud, 921_600);
        assert_eq!(a.cfg.retries, 3);
        assert_eq!(a.cfg.timeout, Duration::from_millis(500));
    }

    #[test]
    fn defaults_and_errors() {
        let a = parse("hello").unwrap();
        assert!(matches!(a.cmd, Command::Hello));
        assert_eq!((a.port.as_str(), a.baud), ("COM5", 115_200));
        assert!(parse("load").is_err());
        assert!(parse("hello extra").is_err());
        assert!(parse("load a b").is_err());
        assert!(parse("load a --baud x").is_err());
        assert!(parse("load a --bogus").is_err());
        assert!(parse("load a --port").is_err());
        assert!(parse("flash a").is_err());
    }
}
