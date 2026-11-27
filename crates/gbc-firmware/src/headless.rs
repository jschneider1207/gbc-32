//! The headless loop (Stage 2 M1): runs a ROM for a fixed number of frames
//! with scripted input and prints one hash line per frame and an
//! audio-stream hash, so a board run can be compared with a host run of
//! the same ROM and script, byte for byte.
//!
//! # Line format (version 1)
//!
//! Every line ends with `\n` alone (a UART sink may send `\r\n`; a
//! comparison strips a trailing `\r`). Numbers are decimal; hashes are
//! 64-bit FNV-1a ([`crate::fnv`]) as 16 lower-case, zero-padded hex digits.
//!
//! ```text
//! headless v1 rom_bytes=<len> rom_fnv=<hash> input_fnv=<hash> frames=<N>
//! headless frame=<n> clock=<clock> ev=<events> fb=<hash>        (n = 0 .. N-1)
//! headless audio frames=<N> samples=<count> fnv=<hash>
//! stats ...                                                     (not compared)
//! ```
//!
//! - Header: the ROM's length and FNV-1a, the input table's
//!   ([`Script::fnv`]), and the frame count asked for.
//! - `frame=<n>`: the `n`th `Emulator::run_frame` call (from 0).
//!   `clock` is `Emulator::clock` after it (emulated base clocks since
//!   power-on), `ev` the call's `Events` bits as 4 hex digits (bit 0 is
//!   `FRAME_READY`: with the LCD off a call ends without a frame), `fb`
//!   the hash of the whole frame buffer after it ([`Fnv1a::frame`]).
//! - `audio`: the hash of every sample of the run, in order
//!   ([`Fnv1a::samples`], one stream across all calls), and their count.
//! - If the run cannot start, one `headless error <reason>` line follows
//!   the header instead of the frame and audio lines.
//!
//! **Compared lines** are exactly the lines starting with `headless `:
//! everything in them is emulated state, the same on every target. Lines
//! starting with `stats ` carry the [`CycleClock`] figures, which differ
//! between host and board, and are never compared; nor is anything else a
//! board prints (its banner).
//!
//! ```text
//! stats run_frame min=<c> mean=<c> max=<c> max_frame=<n> hz=<hz>
//! stats realtime mean=<pct>% max=<pct>%                         (only if hz != 0)
//! ```
//!
//! `c` are counts of the [`CycleClock`] around each `run_frame` call alone
//! (input, hashing and printing are outside); `realtime` is that as a
//! percentage, two decimals, of one real-time frame (`hz * FRAME_CLOCKS /
//! CLOCK_HZ` counts, 7,902,557 cycles at 472 MHz).
//!
//! # Board hook
//!
//! [`run_with_sink`] also hands every completed frame to a [`FrameSink`]
//! (the board's display, M3): after each `run_frame` call whose events
//! contain `FRAME_READY`, after that call's line is written, outside the
//! `run_frame` cycle count. The sink only reads the frame, and it writes
//! no line, so the lines are the same with any sink ([`run`] is
//! `run_with_sink` with [`NoSink`]). A board prints its own figures after
//! the run, on lines of its own that start with `stats ` (not compared).
//!
//! # Run conditions
//!
//! A fresh emulator: power-on post-boot state (no boot ROM), the default
//! [`Config`], cartridge RAM zeroed (no save), the frame buffer filled with
//! [`WHITE`]. So the lines depend only on the ROM, the script and `N`.
//!
//! # Memory (for the MCU caller)
//!
//! The caller lends every large buffer ([`Buffers`]); on the board they
//! are statics in `.axisram`/DTCM, placed by the board crate (ADR 0036):
//! [`Memory`] (~50 KiB; `Memory::new()` is not all zeros, so a static
//! one is `.data` and its initializer costs ~51 KB of flash), the [`Frame`] (46,080 B),
//! the audio buffer (at least [`MIN_AUDIO_SLOTS`] samples of 4 B), and
//! cartridge RAM (`gbc_core::cart_ram_size(rom)` bytes; 0 for cpu_instrs
//! and cgb-acid2). The `Emulator` itself (952 B on thumbv7em,
//! `scripts/check-code-size.sh`) lives on [`run`]'s stack frame.

use core::fmt::Write;

use gbc_core::{
    Buttons, CLOCK_HZ, Config, Emulator, Events, FRAME_CLOCKS, Frame, LoadErrorKind, Memory,
    StereoSample, WHITE, cart_ram_size,
};

use crate::CycleClock;
use crate::fnv::{self, Fnv1a};
use crate::input::Script;

/// The smallest audio buffer [`run`] takes: what one `run_frame` call can
/// produce (gbc-core `AUDIO_SAMPLE_RATE` docs), so `AUDIO_BUFFER_FULL`
/// never splits a frame.
pub const MIN_AUDIO_SLOTS: usize = 550;

/// The caller's buffers ("Memory" in the module docs).
pub struct Buffers<'b> {
    pub mem: &'b mut Memory,
    /// At least `cart_ram_size(rom)` bytes; only that prefix is used (and
    /// zeroed).
    pub save_ram: &'b mut [u8],
    pub frame: &'b mut Frame,
    /// At least [`MIN_AUDIO_SLOTS`] samples.
    pub audio: &'b mut [StereoSample],
}

/// Why a run did not start (also printed as a `headless error` line), or
/// stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// `cart_ram_size` refused the header (too short, unsupported mapper).
    RomHeader(LoadErrorKind),
    /// `save_ram` is smaller than the cartridge needs.
    SaveRam { need: usize, have: usize },
    /// `audio` is smaller than [`MIN_AUDIO_SLOTS`].
    AudioBuffer { have: usize },
    /// `Emulator::load` refused the ROM.
    Load(LoadErrorKind),
    /// The output sink failed; the lines are incomplete.
    Write,
}

impl From<core::fmt::Error> for Error {
    fn from(_: core::fmt::Error) -> Self {
        Self::Write
    }
}

/// [`CycleClock`] counts per `run_frame` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CycleStats {
    pub min: u32,
    pub max: u32,
    /// The frame with the first maximum.
    pub max_frame: u32,
    pub sum: u64,
}

/// What a finished run printed, for callers that want it as values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Summary {
    pub frames: u32,
    /// `Emulator::clock` at the end.
    pub clock: u64,
    /// Hash of the last frame line (`fb=`).
    pub last_fb: u64,
    pub samples: u64,
    pub audio_fnv: u64,
    pub cycles: CycleStats,
}

/// The board's per-frame hook ("Board hook" in the module docs).
pub trait FrameSink {
    /// `frame` is the frame buffer right after `run_frame` call `n` (the
    /// `frame=<n>` line's) returned `FRAME_READY`. Not called for a call
    /// that ended without a frame (LCD off). The time spent here is not in
    /// the `run_frame` statistics.
    fn frame(&mut self, n: u32, frame: &Frame);
}

/// A [`FrameSink`] that does nothing (the host, and the board without a
/// display).
pub struct NoSink;

impl FrameSink for NoSink {
    #[inline(always)]
    fn frame(&mut self, _n: u32, _frame: &Frame) {}
}

/// Runs `rom` for `frames` frames with `input` and prints the lines (module
/// docs) to `out`. Returns the summary, or the error that ended the run.
pub fn run<C, W>(
    rom: &[u8],
    input: Script<'_>,
    frames: u32,
    bufs: Buffers<'_>,
    clock: &C,
    out: &mut W,
) -> Result<Summary, Error>
where
    C: CycleClock + ?Sized,
    W: Write + ?Sized,
{
    run_with_sink(rom, input, frames, bufs, clock, out, &mut NoSink)
}

/// [`run`], handing each completed frame to `sink` ("Board hook" in the
/// module docs). The lines are the same as `run`'s.
pub fn run_with_sink<C, W, S>(
    rom: &[u8],
    input: Script<'_>,
    frames: u32,
    bufs: Buffers<'_>,
    clock: &C,
    out: &mut W,
    sink: &mut S,
) -> Result<Summary, Error>
where
    C: CycleClock + ?Sized,
    W: Write + ?Sized,
    S: FrameSink + ?Sized,
{
    writeln!(
        out,
        "headless v1 rom_bytes={} rom_fnv={:016x} input_fnv={:016x} frames={frames}",
        rom.len(),
        fnv::hash(rom),
        input.fnv(),
    )?;
    let started = start(rom, bufs);
    let (mut emu, frame, audio) = match started {
        Ok(s) => s,
        Err(e) => {
            error_line(out, "headless", e)?;
            return Err(e);
        }
    };

    let mut held = Buttons::EMPTY;
    let mut audio_h = Fnv1a::new();
    let mut samples = 0u64;
    let mut last_fb = 0;
    let mut cy = CycleStats {
        min: u32::MAX,
        max: 0,
        max_frame: 0,
        sum: 0,
    };
    for n in 0..frames {
        let b = input.held(n);
        if b != held {
            emu.set_buttons(b);
            held = b;
        }
        let t0 = clock.now();
        let r = emu.run_frame(frame, audio);
        let dt = clock.now().wrapping_sub(t0);
        cy.min = cy.min.min(dt);
        if dt > cy.max {
            cy.max = dt;
            cy.max_frame = n;
        }
        cy.sum += u64::from(dt);

        let got = &audio[..r.audio_samples];
        audio_h.samples(got);
        samples += got.len() as u64;
        let mut fb = Fnv1a::new();
        fb.frame(frame);
        last_fb = fb.get();
        writeln!(
            out,
            "headless frame={n} clock={} ev={:04x} fb={last_fb:016x}",
            emu.clock(),
            r.events.bits(),
        )?;
        if r.events.contains(Events::FRAME_READY) {
            sink.frame(n, frame);
        }
    }
    writeln!(
        out,
        "headless audio frames={frames} samples={samples} fnv={:016x}",
        audio_h.get()
    )?;
    if frames == 0 {
        cy.min = 0;
    }
    stats_lines(out, &cy, frames, clock.hz())?;
    Ok(Summary {
        frames,
        clock: emu.clock(),
        last_fb,
        samples,
        audio_fnv: audio_h.get(),
        cycles: cy,
    })
}

/// Checks the buffers, resets them to the run conditions and loads the
/// cartridge.
pub(crate) fn start<'a, 'b: 'a>(
    rom: &'a [u8],
    bufs: Buffers<'b>,
) -> Result<(Emulator<'a>, &'b mut Frame, &'b mut [StereoSample]), Error> {
    let need = cart_ram_size(rom).map_err(Error::RomHeader)?;
    let have = bufs.save_ram.len();
    if need > have {
        return Err(Error::SaveRam { need, have });
    }
    if bufs.audio.len() < MIN_AUDIO_SLOTS {
        return Err(Error::AudioBuffer {
            have: bufs.audio.len(),
        });
    }
    let cart_ram = &mut bufs.save_ram[..need];
    cart_ram.fill(0);
    bufs.frame.fill(WHITE);
    let mut emu = Emulator::new(bufs.mem);
    match emu.load(rom, cart_ram, Config::default()) {
        // A fresh emulator had no cartridge to hand back.
        Ok(_) => Ok((emu, bufs.frame, bufs.audio)),
        Err(e) => Err(Error::Load(e.kind)),
    }
}

/// The `<prefix> error ...` line for `e` (`headless`, or `paced` for
/// [`crate::paced`]).
pub(crate) fn error_line<W: Write + ?Sized>(
    out: &mut W,
    prefix: &str,
    e: Error,
) -> Result<(), Error> {
    match e {
        Error::RomHeader(k) => writeln!(out, "{prefix} error rom_header {k:?}")?,
        Error::SaveRam { need, have } => {
            writeln!(out, "{prefix} error save_ram need={need} have={have}")?;
        }
        Error::AudioBuffer { have } => writeln!(
            out,
            "{prefix} error audio_buffer need={MIN_AUDIO_SLOTS} have={have}"
        )?,
        Error::Load(k) => writeln!(out, "{prefix} error load {k:?}")?,
        Error::Write => {}
    }
    Ok(())
}

fn stats_lines<W: Write + ?Sized>(
    out: &mut W,
    cy: &CycleStats,
    frames: u32,
    hz: u32,
) -> Result<(), Error> {
    let mean = cy.sum / u64::from(frames.max(1));
    writeln!(
        out,
        "stats run_frame min={} mean={mean} max={} max_frame={} hz={hz}",
        cy.min, cy.max, cy.max_frame
    )?;
    // One real-time frame in clock counts.
    let budget = u64::from(hz) * u64::from(FRAME_CLOCKS) / u64::from(CLOCK_HZ);
    if budget != 0 {
        let pct = |c: u64| {
            let bp = c * 10_000 / budget;
            (bp / 100, bp % 100)
        };
        let (mi, mf) = pct(mean);
        let (xi, xf) = pct(u64::from(cy.max));
        writeln!(out, "stats realtime mean={mi}.{mf:02}% max={xi}.{xf:02}%")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    extern crate std;
    use std::boxed::Box;
    use std::string::String;
    use std::vec;
    use std::vec::Vec;

    /// Every `frame` call: the frame number and the frame's hash.
    struct Recorder(Vec<(u32, u64)>);
    impl FrameSink for Recorder {
        fn frame(&mut self, n: u32, frame: &Frame) {
            let mut h = Fnv1a::new();
            h.frame(frame);
            self.0.push((n, h.get()));
        }
    }

    struct Zero;
    impl CycleClock for Zero {
        fn now(&self) -> u32 {
            0
        }
        fn hz(&self) -> u32 {
            0
        }
    }

    /// The lines of a `frames`-frame run of `rom`, with `sink`.
    fn lines<S: FrameSink>(rom: &[u8], frames: u32, sink: &mut S) -> String {
        let mut mem = Box::new(Memory::new());
        let mut frame: Box<Frame> = Box::new([0; gbc_core::SCREEN_WIDTH * gbc_core::SCREEN_HEIGHT]);
        let mut audio = vec![StereoSample::default(); MIN_AUDIO_SLOTS];
        let bufs = Buffers {
            mem: &mut mem,
            save_ram: &mut [],
            frame: &mut frame,
            audio: &mut audio,
        };
        let mut s = String::new();
        run_with_sink(rom, Script::new(&[]), frames, bufs, &Zero, &mut s, sink).unwrap();
        s
    }

    /// The sink sees exactly the frames whose line has FRAME_READY (ev bit
    /// 0), with that line's number and hash, and changes no line.
    #[test]
    fn sink_gets_each_completed_frame() {
        // A 32 KiB ROM-only cartridge (type 0) whose program at 0100h
        // waits about four frames (B x 256 x DEC C/JR NZ: 16 clocks per
        // round, 262k clocks), turns the LCD off (LDH (40h),A with A = 0) and loops: so
        // the run has frames with FRAME_READY, then calls without.
        let mut rom = vec![0u8; 0x8000];
        let prog = [
            0x06, 0x40, // LD B,40h
            0x0E, 0x00, // LD C,0     <- outer (0102h)
            0x0D, // DEC C            <- inner (0104h)
            0x20, 0xFD, // JR NZ,inner
            0x05, // DEC B
            0x20, 0xF8, // JR NZ,outer
            0xAF, // XOR A
            0xE0, 0x40, // LDH (40h),A
            0x18, 0xFE, // JR -2
        ];
        rom[0x100..0x100 + prog.len()].copy_from_slice(&prog);
        let frames = 12;
        let mut rec = Recorder(Vec::new());
        let with = lines(&rom, frames, &mut rec);
        assert_eq!(with, lines(&rom, frames, &mut NoSink));
        let mut want = Vec::new();
        for l in with.lines().filter(|l| l.starts_with("headless frame=")) {
            let field = |k: &str| {
                l.split(' ')
                    .find_map(|f| f.strip_prefix(k))
                    .map(String::from)
            };
            let ev = u16::from_str_radix(&field("ev=").unwrap(), 16).unwrap();
            if ev & 1 != 0 {
                let n: u32 = field("frame=").unwrap().parse().unwrap();
                let fb = u64::from_str_radix(&field("fb=").unwrap(), 16).unwrap();
                want.push((n, fb));
            }
        }
        // Both kinds of call happened.
        assert!(!want.is_empty() && want.len() < frames as usize, "{with}");
        assert_eq!(rec.0, want);
    }

    #[test]
    fn stats_lines_format() {
        let mut s = String::new();
        let cy = CycleStats {
            min: 3_000_000,
            max: 3_951_279,
            max_frame: 7,
            sum: 3 * 3_500_000,
        };
        stats_lines(&mut s, &cy, 3, 472_000_000).unwrap();
        assert_eq!(
            s,
            "stats run_frame min=3000000 mean=3500000 max=3951279 max_frame=7 hz=472000000\n\
             stats realtime mean=44.28% max=50.00%\n"
        );
        let mut s = String::new();
        stats_lines(&mut s, &cy, 3, 0).unwrap();
        assert_eq!(
            s,
            "stats run_frame min=3000000 mean=3500000 max=3951279 max_frame=7 hz=0\n"
        );
    }

    #[test]
    fn error_lines_format() {
        for (e, line) in [
            (
                Error::SaveRam {
                    need: 8192,
                    have: 0,
                },
                "headless error save_ram need=8192 have=0\n",
            ),
            (
                Error::AudioBuffer { have: 549 },
                "headless error audio_buffer need=550 have=549\n",
            ),
            (
                Error::RomHeader(LoadErrorKind::RomTooSmall { len: 3 }),
                "headless error rom_header RomTooSmall { len: 3 }\n",
            ),
        ] {
            let mut s = String::new();
            error_line(&mut s, "headless", e).unwrap();
            assert_eq!(s, line);
        }
    }
}
