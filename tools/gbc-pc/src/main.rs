//! `gbc-pc`: talks to the dev board over the ST-LINK virtual COM port.
//!
//! ```text
//! gbc-pc load <rom> [--sav <file.sav>] [--raw-save] [link options]
//! gbc-pc dump <out.sav>                [--raw-save] [link options]
//! gbc-pc play [--sav-out <file.sav>] [--seconds N] [--raw-save] [--audio-wav <file.wav>] [link options]
//! gbc-pc hello                         [link options]
//! link options: [--port COM5|null] [--baud 2000000] [--retries 5] [--timeout-ms 2000]
//! ```
//!
//! `load --sav` sends the save after the ROM, its clock footer (MBC3 RTC,
//! HuC3) first synced to the PC's clock as the desktop does on load, and
//! refused here if the board would refuse it. `dump` and `play`'s dumps
//! restamp a clock footer with the PC's time (ADR 0040, `gbc_pc::rtc`).
//! `--raw-save` turns both off: the bytes go and come exactly as they are
//! (transport checks). `dump` writes the board's
//! save only when complete and verified (via a `.part` file and a
//! rename). `play` sends the keys as `Input` frames (arrows, Z = B,
//! X = A, Enter = Start, Right Shift/Backspace = Select) until Esc or
//! `--seconds`; F5 dumps the save to `--sav-out`; board `Stats` are
//! printed. `--port null` runs without a board (writes dropped, nothing
//! read).
//!
//! Exit code 0 on success, 1 on a link, load or dump failure, 2 on a
//! usage or file error. From WSL, run it through `scripts/gbc-pc.sh`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use gbc_pc::play::{Exit, Keyboard, PlayEvent, PlayOptions};
use gbc_pc::{Client, Config, LoadOutcome, Transport};

const USAGE: &str = "usage:
  gbc-pc load <rom> [--sav <file.sav>]                [--raw-save] [link options]
  gbc-pc dump <out.sav>                               [--raw-save] [link options]
  gbc-pc play [--sav-out <file.sav>] [--seconds N]    [--raw-save] [--audio-wav <file.wav>] [link options]
  gbc-pc hello                                        [link options]
--raw-save: no RTC sync on load, no restamp on dump (byte-exact saves)
link options: [--port COM5|null] [--baud 2000000] [--retries 5] [--timeout-ms 2000]";

enum Command {
    Load {
        rom: String,
        sav: Option<String>,
    },
    Dump(String),
    Play {
        sav_out: Option<String>,
        seconds: Option<u64>,
        audio_wav: Option<String>,
    },
    Hello,
}

struct Args {
    cmd: Command,
    port: String,
    baud: u32,
    cfg: Config,
    /// `--raw-save`: saves byte-exact, no clock sync or restamp.
    raw_save: bool,
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let cmd = match it.next().as_deref() {
        Some("load") => Command::Load {
            rom: String::new(),
            sav: None,
        },
        Some("dump") => Command::Dump(String::new()),
        Some("play") => Command::Play {
            sav_out: None,
            seconds: None,
            audio_wav: None,
        },
        Some("hello") => Command::Hello,
        Some(c) => return Err(format!("unknown command {c:?}")),
        None => return Err("missing command".into()),
    };
    let mut args = Args {
        cmd,
        port: "COM5".into(),
        // The loader image's rate (crates/gbc-nucleo-h753 src/bin/loader.rs,
        // docs/reference/loader.md).
        baud: 2_000_000,
        cfg: Config::default(),
        raw_save: false,
    };
    let mut path = None;
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
            "--sav" => match &mut args.cmd {
                Command::Load { sav, .. } => *sav = Some(value("--sav")?),
                _ => return Err("--sav is a load option".into()),
            },
            "--sav-out" => match &mut args.cmd {
                Command::Play { sav_out, .. } => *sav_out = Some(value("--sav-out")?),
                _ => return Err("--sav-out is a play option".into()),
            },
            "--audio-wav" => match &mut args.cmd {
                Command::Play { audio_wav, .. } => *audio_wav = Some(value("--audio-wav")?),
                _ => return Err("--audio-wav is a play option".into()),
            },
            "--raw-save" => match args.cmd {
                Command::Hello => return Err("--raw-save is a load, dump or play option".into()),
                _ => args.raw_save = true,
            },
            "--seconds" => {
                let n = number(&value("--seconds")?, "--seconds")?;
                match &mut args.cmd {
                    Command::Play { seconds, .. } => *seconds = Some(n),
                    _ => return Err("--seconds is a play option".into()),
                }
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s if matches!(args.cmd, Command::Load { .. } | Command::Dump(_)) && path.is_none() => {
                path = Some(s.to_string());
            }
            s => return Err(format!("unexpected argument {s:?}")),
        }
    }
    match &mut args.cmd {
        Command::Load { rom, .. } => *rom = path.ok_or("load needs a ROM path")?,
        Command::Dump(out) => *out = path.ok_or("dump needs an output path")?,
        Command::Play { .. } | Command::Hello => {}
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
    let (rom, sav) = match &args.cmd {
        Command::Load { rom, sav } => {
            let rom = match read(rom) {
                Ok(r) => r,
                Err(code) => return code,
            };
            let sav = match sav.as_deref().map(read).transpose() {
                Ok(s) => s,
                Err(code) => return code,
            };
            let now = (!args.raw_save).then(gbc_pc::rtc::unix_now);
            let sav = match sav
                .map(|s| gbc_pc::rtc::for_load(&rom, &s, now))
                .transpose()
            {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "gbc-pc: save refused here, as the board would refuse it: {e:?} (--raw-save sends it unchecked)"
                    );
                    return ExitCode::from(1);
                }
            };
            if sav.is_some() && now.is_some() {
                println!("save: clock footer (if any) synced to the PC's clock");
            }
            (Some(rom), sav)
        }
        _ => (None, None),
    };
    let port = match open(&args.port, args.baud) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("gbc-pc: {e}");
            return ExitCode::from(2);
        }
    };
    println!("gbc-pc: {} at {} baud, 8N1", args.port, args.baud);
    run(port, &args, rom.as_deref(), sav.as_deref())
}

fn read(path: &str) -> Result<Vec<u8>, ExitCode> {
    std::fs::read(path).map_err(|e| {
        eprintln!("gbc-pc: cannot read {path}: {e}");
        ExitCode::from(2)
    })
}

fn open(port: &str, baud: u32) -> std::io::Result<Box<dyn Transport>> {
    if port.eq_ignore_ascii_case("null") {
        return Ok(Box::new(gbc_pc::transport::NullPort));
    }
    open_com(port, baud)
}

#[cfg(windows)]
fn open_com(port: &str, baud: u32) -> std::io::Result<Box<dyn Transport>> {
    Ok(Box::new(gbc_pc::serial::ComPort::open(port, baud)?))
}

#[cfg(not(windows))]
fn open_com(_port: &str, _baud: u32) -> std::io::Result<Box<dyn Transport>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "COM ports are only supported in the Windows build (scripts/gbc-pc.sh)",
    ))
}

#[cfg(windows)]
fn keyboard() -> Box<dyn Keyboard> {
    Box::new(gbc_pc::keyboard::AsyncKeyboard)
}

/// No keyboard off Windows: nothing is ever held (`--seconds` ends it).
#[cfg(not(windows))]
fn keyboard() -> Box<dyn Keyboard> {
    struct NoKeys;
    impl Keyboard for NoKeys {
        fn held(&mut self) -> std::io::Result<gbc_pc::input::KeySet> {
            Ok(Default::default())
        }
    }
    Box::new(NoKeys)
}

fn run<T: Transport>(port: T, args: &Args, rom: Option<&[u8]>, sav: Option<&[u8]>) -> ExitCode {
    let mut client = Client::new(port, args.cfg);
    let clock: Option<gbc_pc::rtc::WallClock> = (!args.raw_save).then_some(gbc_pc::rtc::unix_now);
    let result = match (&args.cmd, rom) {
        (Command::Load { .. }, Some(rom)) => load(&mut client, rom, sav),
        (Command::Dump(out), _) => dump(&mut client, Path::new(out), clock),
        (
            Command::Play {
                sav_out,
                seconds,
                audio_wav,
            },
            _,
        ) => play(
            &mut client,
            sav_out.as_deref(),
            *seconds,
            audio_wav.as_deref(),
            clock,
        ),
        _ => client.hello().map(|info| {
            println!(
                "HelloReply: protocol {}, max_chunk {}, flash_len {} ({} KiB)",
                info.protocol,
                info.max_chunk,
                info.flash_len,
                info.flash_len / 1024
            );
        }),
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

fn dump<T: Transport>(
    client: &mut Client<T>,
    out: &Path,
    clock: Option<gbc_pc::rtc::WallClock>,
) -> Result<(), gbc_pc::Error> {
    let start = Instant::now();
    let (len, restamped) = gbc_pc::save::dump_to_file(client, out, clock)?;
    println!(
        "dumped {len} bytes in {:.2} s to {} (hash verified{})",
        start.elapsed().as_secs_f64(),
        out.display(),
        if restamped {
            "; clock footer stamped with the PC's time"
        } else {
            ""
        }
    );
    Ok(())
}

fn play<T: Transport>(
    client: &mut Client<T>,
    sav_out: Option<&str>,
    seconds: Option<u64>,
    audio_wav: Option<&str>,
    clock: Option<gbc_pc::rtc::WallClock>,
) -> Result<(), gbc_pc::Error> {
    let mut capture = gbc_pc::audio::Capture::new();
    let mut audio_span: Option<(Instant, Instant)> = None;
    let mut last_extra = String::new();
    let opts = PlayOptions {
        audio: audio_wav.is_some(),
        duration: seconds.map(Duration::from_secs),
        sav_out: sav_out.map(PathBuf::from),
        clock,
        ..PlayOptions::default()
    };
    println!(
        "play: arrows, Z = B, X = A, Enter = Start, Right Shift/Backspace = Select; F5 dumps the save{}; Esc quits",
        sav_out.map_or(" (needs --sav-out)".to_string(), |p| format!(" to {p}"))
    );
    let mut kb = keyboard();
    let mut event = |e: PlayEvent| match e {
        PlayEvent::Stats {
            frames,
            cycles_mean,
            cycles_max,
            extra,
        } if extra.is_empty() => {
            println!("stats: frames {frames}, cycles mean {cycles_mean}, max {cycles_max}")
        }
        PlayEvent::Stats { extra, .. } => {
            println!("stats: {extra}");
            last_extra = extra;
        }
        PlayEvent::Audio { first, held, data } => {
            let t = Instant::now();
            audio_span = Some(audio_span.map_or((t, t), |(a, _)| (a, t)));
            capture.on_audio(first, held, &data);
        }
        PlayEvent::DumpStarted => println!("dump: started"),
        PlayEvent::DumpRetry(f) => println!("dump: retrying ({f})"),
        PlayEvent::DumpFailed(f) => eprintln!("dump: failed ({f})"),
        PlayEvent::Saved { path, len } => {
            println!("dump: {len} bytes to {} (hash verified)", path.display())
        }
        PlayEvent::WriteFailed(e) => eprintln!("dump: cannot write {e}"),
        PlayEvent::NoSavOut => eprintln!("dump: no --sav-out given, nothing written"),
    };
    let (t0, rx0) = (Instant::now(), client.rx_bytes);
    let r = gbc_pc::play::play(client, &mut *kb, &opts, &mut Instant::now, &mut event)?;
    let rx_rate = (client.rx_bytes - rx0) as f64 / t0.elapsed().as_secs_f64().max(1e-9);
    println!(
        "play: {} ({} Input frames, {} saves, {} failed dumps, {} other Naks ignored)",
        match r.exit {
            Exit::Esc => "Esc",
            Exit::Time => "time up",
        },
        r.inputs,
        r.saves,
        r.dumps_failed,
        r.ignored_naks
    );
    if let Some(path) = audio_wav {
        println!("audio: link received {rx_rate:.0} B/s over the whole play");
        audio_summary(&capture, audio_span, &last_extra);
        let wav = capture.wav();
        match std::fs::write(path, &wav) {
            Ok(()) => println!("audio: wrote {path} ({} B)", wav.len()),
            Err(e) => {
                return Err(gbc_pc::Error::Io(std::io::Error::new(
                    e.kind(),
                    format!("{path}: {e}"),
                )));
            }
        }
    }
    Ok(())
}

/// The `--audio-wav` report: frames, gaps, rate, and the board's own
/// `audio_*` counters from its last `Stats`.
fn audio_summary(c: &gbc_pc::audio::Capture, span: Option<(Instant, Instant)>, board_extra: &str) {
    let s = c.stats;
    let secs = span.map_or(0.0, |(a, b)| b.duration_since(a).as_secs_f64());
    let fps = if secs > 0.0 {
        s.frames as f64 / secs
    } else {
        0.0
    };
    println!(
        "audio: first_frame={} frames={} messages={} gaps={} missing={} filled={} held={} span={secs:.2}s frames_per_s={fps:.1} data_bytes_per_s={:.0}",
        s.first_frame.map_or("-".to_string(), |f| f.to_string()),
        s.frames,
        s.messages,
        s.gaps,
        s.missing,
        s.filled,
        s.held,
        fps * 3.0,
    );
    let board: Vec<&str> = board_extra
        .split(' ')
        .filter(|f| f.starts_with("audio_"))
        .collect();
    println!(
        "audio: board {}",
        if board.is_empty() {
            "-".to_string()
        } else {
            board.join(" ")
        }
    );
}

fn load<T: Transport>(
    client: &mut Client<T>,
    rom: &[u8],
    sav: Option<&[u8]>,
) -> Result<(), gbc_pc::Error> {
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
    if let Some(sav) = sav {
        println!(
            "save: {} bytes, FNV-1a 64 {:#018x}",
            sav.len(),
            gbc_pc::fnv::hash(sav)
        );
        let start = Instant::now();
        client.send_save(sav, &mut |_, _| {})?;
        println!(
            "save sent in {:.2} s; the board verified the hash and took it",
            start.elapsed().as_secs_f64()
        );
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
        assert!(matches!(a.cmd, Command::Load { ref rom, sav: None } if rom == "game.gbc"));
        assert_eq!(a.port, "COM7");
        assert_eq!(a.baud, 921_600);
        assert_eq!(a.cfg.retries, 3);
        assert_eq!(a.cfg.timeout, Duration::from_millis(500));
    }

    #[test]
    fn defaults_and_errors() {
        let a = parse("hello").unwrap();
        assert!(matches!(a.cmd, Command::Hello));
        assert_eq!((a.port.as_str(), a.baud), ("COM5", 2_000_000));
        assert!(parse("load").is_err());
        assert!(parse("hello extra").is_err());
        assert!(parse("load a b").is_err());
        assert!(parse("load a --baud x").is_err());
        assert!(parse("load a --bogus").is_err());
        assert!(parse("load a --port").is_err());
        assert!(parse("flash a").is_err());
        assert!(parse("dump").is_err());
        assert!(parse("dump a b").is_err());
        assert!(parse("play x").is_err());
        assert!(parse("play --sav x").is_err());
        assert!(parse("load a --sav-out x").is_err());
        assert!(parse("hello --seconds 2").is_err());
        assert!(parse("play --seconds two").is_err());
    }

    #[test]
    fn raw_save_flag() {
        assert!(!parse("load g.gbc --sav g.sav").unwrap().raw_save);
        assert!(parse("load g.gbc --sav g.sav --raw-save").unwrap().raw_save);
        assert!(parse("dump out.sav --raw-save").unwrap().raw_save);
        assert!(parse("play --raw-save --sav-out g.sav").unwrap().raw_save);
        assert!(!parse("dump out.sav").unwrap().raw_save);
        assert!(parse("hello --raw-save").is_err());
    }

    #[test]
    fn save_commands() {
        let a = parse("load g.gbc --sav g.sav --port null").unwrap();
        assert!(
            matches!(a.cmd, Command::Load { ref rom, sav: Some(ref s) } if rom == "g.gbc" && s == "g.sav")
        );
        assert_eq!(a.port, "null");
        let a = parse("dump out.sav").unwrap();
        assert!(matches!(a.cmd, Command::Dump(ref p) if p == "out.sav"));
        let a = parse("play --sav-out g.sav --seconds 2").unwrap();
        assert!(matches!(
            a.cmd,
            Command::Play { sav_out: Some(ref p), seconds: Some(2), audio_wav: None } if p == "g.sav"
        ));
        let a = parse("play --audio-wav a.wav --seconds 3").unwrap();
        assert!(matches!(
            a.cmd,
            Command::Play { audio_wav: Some(ref w), seconds: Some(3), sav_out: None } if w == "a.wav"
        ));
        assert!(parse("dump x --audio-wav a.wav").is_err());
        assert!(parse("play --audio-wav").is_err());
        let a = parse("play").unwrap();
        assert!(matches!(
            a.cmd,
            Command::Play {
                sav_out: None,
                seconds: None,
                audio_wav: None
            }
        ));
    }
}
