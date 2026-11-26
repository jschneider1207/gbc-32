//! Host counterpart of the board's `headless` image (Stage 2 M1, ADR
//! 0036): runs a ROM for N frames with an optional input script and prints
//! the lines of `gbc_firmware::headless` (format in its module docs). The
//! loop and the line format are the firmware's own code, so the board and
//! this tool cannot print different lines for the same ROM and script.
//!
//! ```text
//! headless <rom> [--input <program.txt>] [--frames <N>] [--paced]
//! ```
//!
//! `--paced` runs `gbc_firmware::paced` instead (the audio-paced main
//! loop, Stage 2 M4) and prints its lines (`paced ` compared, `stats `
//! not; format in its module docs): the board's `paced` lines must equal
//! these. The consumer is simulated: the idle hook pops one DMA half
//! (256 frames) through a `FeedTap`, as the board's DMA ISR would during
//! `WFI`. The `stats paced speed` line is meaningless here (no real-time
//! consumer).
//!
//! `--frames` defaults to the script's `end <frames>` line. The `stats`
//! lines time each `run_frame` call with the host's monotonic clock in
//! nanoseconds (`hz=1000000000`); compare runs with `grep '^headless '`.
//! The committed board targets are in `tools/headless/expected/`.
//!
//! Exit status: 0 on a full run, 1 if the run could not start (after a
//! `headless error` line) or the ROM or script could not be read, 2 for a
//! usage error.

use std::io::Write as _;
use std::process::ExitCode;
use std::time::Instant;

use gbc_core::{Frame, Memory, StereoSample, WHITE, cart_ram_size};
use gbc_firmware::CycleClock;
use gbc_firmware::audio::{DMA_HALF_FRAMES, FeedRing, FeedTap};
use gbc_firmware::headless::NoSink;
use gbc_firmware::headless::{self, Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};
use gbc_firmware::paced::{self, Feed, Hooks, Idle};

/// Nanoseconds since start, wrapping (about 4.3 s per wrap; a `run_frame`
/// call takes milliseconds).
struct HostClock(Instant);

impl CycleClock for HostClock {
    fn now(&self) -> u32 {
        // Truncation is the wrap.
        self.0.elapsed().as_nanos() as u32
    }
    fn hz(&self) -> u32 {
        1_000_000_000
    }
}

/// `core::fmt::Write` over a buffered stdout, keeping the first I/O error.
struct Out<W: std::io::Write> {
    w: W,
    err: Option<std::io::Error>,
}

impl<W: std::io::Write> std::fmt::Write for Out<W> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.w.write_all(s.as_bytes()).map_err(|e| {
            self.err.get_or_insert(e);
            std::fmt::Error
        })
    }
}

/// The host's stand-in for the board's DMA ISR during `WFI`: each idle
/// call pops one DMA half through the tap.
struct HostConsumer<'a> {
    ring: &'a FeedRing,
    tap: &'a FeedTap,
    half: [u32; DMA_HALF_FRAMES],
}

impl Idle for HostConsumer<'_> {
    // The host never sleeps: it pops at once, whatever the condition.
    fn idle(&mut self, _waiting: &dyn Fn() -> bool) {
        self.tap.pop_into(self.ring, &mut self.half);
    }
}

struct Args {
    rom: String,
    input: Option<String>,
    frames: Option<u32>,
    paced: bool,
}

const USAGE: &str = "usage: headless <rom> [--input <program.txt>] [--frames <N>] [--paced]";

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut rom = None;
    let mut input = None;
    let mut frames = None;
    let mut paced = false;
    while let Some(a) = it.next() {
        match a.as_str() {
            "--paced" => paced = true,
            "--input" => input = Some(it.next().ok_or("--input needs a file")?),
            "--frames" => {
                let n = it.next().ok_or("--frames needs a number")?;
                frames = Some(n.parse().map_err(|_| format!("bad --frames {n:?}"))?);
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            _ if rom.is_none() => rom = Some(a),
            _ => return Err(format!("unexpected argument {a:?}")),
        }
    }
    Ok(Args {
        rom: rom.ok_or("no ROM given")?,
        input,
        frames,
        paced,
    })
}

fn main() -> ExitCode {
    let args = match parse_args(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("headless: {e}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let (presses, end) = match &args.input {
        None => (Vec::new(), None),
        Some(path) => match read_script(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("headless: {path}: {e}");
                return ExitCode::from(1);
            }
        },
    };
    let Some(frames) = args.frames.or(end) else {
        eprintln!("headless: give --frames (or an input script with an `end` line)\n{USAGE}");
        return ExitCode::from(2);
    };
    let rom = match std::fs::read(&args.rom) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("headless: {}: {e}", args.rom);
            return ExitCode::from(1);
        }
    };

    let mut mem = Box::new(Memory::new());
    // Sized from the header when it parses; `run` reports a bad header.
    let mut save_ram = vec![0u8; cart_ram_size(&rom).unwrap_or(0)];
    let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
    let mut audio = vec![StereoSample::default(); MIN_AUDIO_SLOTS];
    let bufs = Buffers {
        mem: &mut mem,
        save_ram: &mut save_ram,
        frame: &mut frame,
        audio: &mut audio,
    };
    let clock = HostClock(Instant::now());
    let mut out = Out {
        w: std::io::BufWriter::new(std::io::stdout().lock()),
        err: None,
    };
    let script = Script::new(&presses);
    let r = if args.paced {
        let ring = Box::new(FeedRing::new());
        let tap = FeedTap::new();
        let mut consumer = HostConsumer {
            ring: &ring,
            tap: &tap,
            half: [0; DMA_HALF_FRAMES],
        };
        let hooks = Hooks {
            clock: &clock,
            sink: &mut NoSink,
            idle: &mut consumer,
        };
        let feed = Feed {
            ring: &ring,
            tap: &tap,
        };
        paced::run(&rom, script, frames, bufs, feed, hooks, &mut out)
            .map(drop)
            .map_err(|e| format!("{e:?}"))
    } else {
        headless::run(&rom, script, frames, bufs, &clock, &mut out)
            .map(drop)
            .map_err(|e| format!("{e:?}"))
    };
    let flushed = out.w.flush();
    if let Some(e) = out.err.or(flushed.err()) {
        eprintln!("headless: stdout: {e}");
        return ExitCode::from(1);
    }
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("headless: {e}");
            ExitCode::from(1)
        }
    }
}

/// Reads and parses an input program (`gbc_firmware::input` docs).
fn read_script(path: &str) -> Result<(Vec<Press>, Option<u32>), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    // One press per line at most.
    let mut presses = vec![Press::NONE; text.lines().count()];
    let p =
        input::parse(&text, &mut presses).map_err(|e| format!("line {}: {:?}", e.line, e.kind))?;
    presses.truncate(p.presses);
    Ok((presses, p.end))
}
