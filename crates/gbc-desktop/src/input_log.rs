//! `--record-input` / `--replay-input`: per-frame joypad recordings that
//! replay bit-exactly (ADR 0030).
//!
//! The core is deterministic given the ROM, the cartridge RAM and RTC it
//! starts from, its config and the buttons held during each `run_frame`
//! call. A recording stores all of these except the ROM (only checksums and
//! a hash of it, to refuse the wrong ROM), so a replay does not depend on
//! the `.sav` next to the ROM, nor on the wall clock the RTC was synced to.
//!
//! Frame `n` is the `n`-th `run_frame` call since load, counting from 0
//! (the first call ends early at the first frame boundary, see
//! `headless::run`). Both the window and the headless loop set the buttons
//! once, right before each call, so a recording made in one replays
//! identically in the other.
//!
//! # Format (version 1)
//!
//! UTF-8 text, one record per line (`\n`), so a recording can be read and
//! diffed. The header lines come first, in this order:
//!
//! ```text
//! gbc-32 input 1
//! rom-title WARIOLAND3            informational only
//! rom-header-checksum 3c          0x14D, hex
//! rom-global-checksum a1b2        0x14E-0x14F, hex
//! rom-fnv1a64 0123456789abcdef    FNV-1a 64 of the whole ROM
//! dmg-palette auto                auto | gray (affects the picture only)
//! sav none                        or: sav loaded <len> <fnv1a64 of the file>
//! cart-ram <len> <hex bytes>      the cartridge RAM the run started from
//! rtc none                        or: rtc <live> <latched> <subsecond clocks>
//! events
//! ```
//!
//! where an RTC time is `s:m:h:days:halted:day_carry` in decimal. Events
//! follow, in non-decreasing frame order:
//!
//! ```text
//! <frame> b <hex>    from this frame on the held buttons are <hex>
//!                    (gbc_core::Buttons bits); none before the first
//! <frame> m          mark: the human flagged this frame (key M)
//! end <frames>       the recording ran frames 0..<frames>; last line
//! ```
//!
//! A replay checks the ROM against the header, starts from the header's
//! cartridge RAM and RTC (never the `.sav`), writes no save, and stops
//! after `<frames>` frames.

use std::fmt::Write as _;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use gbc_core::{Buttons, RtcState, RtcTime};

use crate::compat_palette::Mode;

pub const MAGIC: &str = "gbc-32 input";
pub const VERSION: u32 = 1;

/// FNV-1a, 64-bit: a fast, dependency-free fingerprint (not a security
/// hash; it only has to tell different ROMs, saves and frames apart).
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// What a recording must match, and the state a replay starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    pub rom_title: String,
    pub header_checksum: u8,
    pub global_checksum: u16,
    pub rom_hash: u64,
    pub dmg_palette: Mode,
    /// The `.sav` the recorded run loaded: its length and hash.
    pub sav: Option<(usize, u64)>,
    /// The cartridge RAM right after load.
    pub cart_ram: Vec<u8>,
    /// The RTC right after load (after the wall-clock sync).
    pub rtc: Option<RtcState>,
}

/// The ROM fields of a [`Header`]: title, checksums and hash.
pub struct RomId {
    pub title: String,
    pub header_checksum: u8,
    pub global_checksum: u16,
    pub hash: u64,
}

impl RomId {
    pub fn of(rom: &[u8]) -> Self {
        let at = |i: usize| rom.get(i).copied().unwrap_or(0);
        let title = (0x134..0x144)
            .map(at)
            .take_while(|&b| b != 0)
            .map(|b| {
                if b.is_ascii_graphic() || b == b' ' {
                    char::from(b)
                } else {
                    '?'
                }
            })
            .collect::<String>()
            .trim()
            .to_owned();
        RomId {
            title,
            header_checksum: at(0x14D),
            global_checksum: u16::from_be_bytes([at(0x14E), at(0x14F)]),
            hash: fnv1a64(rom),
        }
    }
}

impl Header {
    /// `Err` (a message) if `rom` is not the recorded ROM.
    pub fn check_rom(&self, rom: &RomId) -> Result<(), String> {
        if (rom.header_checksum, rom.global_checksum, rom.hash)
            != (self.header_checksum, self.global_checksum, self.rom_hash)
        {
            return Err(format!(
                "the recording was made with a different ROM (recorded `{}`, checksums {:02x}/{:04x}, \
                 hash {:016x}; this ROM `{}`, {:02x}/{:04x}, {:016x})",
                self.rom_title,
                self.header_checksum,
                self.global_checksum,
                self.rom_hash,
                rom.title,
                rom.header_checksum,
                rom.global_checksum,
                rom.hash
            ));
        }
        Ok(())
    }

    fn write(&self, out: &mut String) {
        let _ = writeln!(out, "{MAGIC} {VERSION}");
        let _ = writeln!(out, "rom-title {}", self.rom_title);
        let _ = writeln!(out, "rom-header-checksum {:02x}", self.header_checksum);
        let _ = writeln!(out, "rom-global-checksum {:04x}", self.global_checksum);
        let _ = writeln!(out, "rom-fnv1a64 {:016x}", self.rom_hash);
        let pal = match self.dmg_palette {
            Mode::Auto => "auto",
            Mode::Gray => "gray",
        };
        let _ = writeln!(out, "dmg-palette {pal}");
        match self.sav {
            Some((len, h)) => {
                let _ = writeln!(out, "sav loaded {len} {h:016x}");
            }
            None => out.push_str("sav none\n"),
        }
        let _ = write!(out, "cart-ram {}", self.cart_ram.len());
        if !self.cart_ram.is_empty() {
            out.push(' ');
            for b in &self.cart_ram {
                let _ = write!(out, "{b:02x}");
            }
        }
        out.push('\n');
        match &self.rtc {
            Some(r) => {
                let _ = writeln!(
                    out,
                    "rtc {} {} {}",
                    fmt_time(&r.live),
                    fmt_time(&r.latched),
                    r.subsecond_clocks
                );
            }
            None => out.push_str("rtc none\n"),
        }
        out.push_str("events\n");
    }
}

fn fmt_time(t: &RtcTime) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        t.seconds,
        t.minutes,
        t.hours,
        t.days,
        u8::from(t.halted),
        u8::from(t.day_carry)
    )
}

fn parse_time(s: &str) -> Result<RtcTime, String> {
    let f: Vec<&str> = s.split(':').collect();
    let [s, m, h, d, halted, carry] = f[..] else {
        return Err(format!("bad RTC time `{s}`"));
    };
    let num = |v: &str, max: u16| -> Result<u16, String> {
        match v.parse::<u16>() {
            Ok(n) if n <= max => Ok(n),
            _ => Err(format!("bad RTC field `{v}`")),
        }
    };
    Ok(RtcTime {
        seconds: num(s, 63)? as u8,
        minutes: num(m, 63)? as u8,
        hours: num(h, 31)? as u8,
        days: num(d, 511)?,
        halted: num(halted, 1)? == 1,
        day_carry: num(carry, 1)? == 1,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Buttons(u64, Buttons),
    Mark(u64),
}

/// A parsed recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    pub header: Header,
    pub events: Vec<Event>,
    /// Frames `0..frames` were recorded.
    pub frames: u64,
}

fn hex_byte(s: &str) -> Result<u8, String> {
    if s.len() != 2 {
        return Err(format!("bad hex byte `{s}`"));
    }
    u8::from_str_radix(s, 16).map_err(|_| format!("bad hex byte `{s}`"))
}

impl Recording {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("can't read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Parses a recording; `Err` names the line and what is wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lines = text.split('\n').enumerate().peekable();
        let mut next = |key: &str| -> Result<(usize, String), String> {
            let (i, l) = lines
                .next()
                .ok_or_else(|| format!("ends before the `{key}` line"))?;
            let l = l.strip_suffix('\r').unwrap_or(l);
            let rest = if key == MAGIC {
                l.strip_prefix(MAGIC).and_then(|r| r.strip_prefix(' '))
            } else {
                l.strip_prefix(key).and_then(|r| {
                    if r.is_empty() {
                        Some(r)
                    } else {
                        r.strip_prefix(' ')
                    }
                })
            };
            rest.map(|r| (i + 1, r.to_owned()))
                .ok_or_else(|| format!("line {}: expected `{key} ...`, got `{l}`", i + 1))
        };
        let err = |n: usize, m: String| format!("line {n}: {m}");

        let (n, v) = next(MAGIC).map_err(|_| "not a gbc-32 input recording".to_owned())?;
        match v.parse::<u32>() {
            Ok(VERSION) => {}
            _ => return Err(err(n, format!("unsupported format version `{v}`"))),
        }
        let (_, rom_title) = next("rom-title")?;
        let (n, v) = next("rom-header-checksum")?;
        let header_checksum = hex_byte(&v).map_err(|e| err(n, e))?;
        let (n, v) = next("rom-global-checksum")?;
        let global_checksum = (v.len() == 4)
            .then(|| u16::from_str_radix(&v, 16).ok())
            .flatten()
            .ok_or_else(|| err(n, format!("bad checksum `{v}`")))?;
        let (n, v) = next("rom-fnv1a64")?;
        let rom_hash = (v.len() == 16)
            .then(|| u64::from_str_radix(&v, 16).ok())
            .flatten()
            .ok_or_else(|| err(n, format!("bad hash `{v}`")))?;
        let (n, v) = next("dmg-palette")?;
        let dmg_palette = match v.as_str() {
            "auto" => Mode::Auto,
            "gray" => Mode::Gray,
            _ => return Err(err(n, format!("bad palette `{v}`"))),
        };
        let (n, v) = next("sav")?;
        let sav = match v.split(' ').collect::<Vec<_>>()[..] {
            ["none"] => None,
            ["loaded", len, h] if h.len() == 16 => {
                match (len.parse::<usize>(), u64::from_str_radix(h, 16)) {
                    (Ok(len), Ok(h)) => Some((len, h)),
                    _ => return Err(err(n, format!("bad sav line `{v}`"))),
                }
            }
            _ => return Err(err(n, format!("bad sav line `{v}`"))),
        };
        let (n, v) = next("cart-ram")?;
        let (len, hex) = v.split_once(' ').unwrap_or((&v, ""));
        let len: usize = len
            .parse()
            .map_err(|_| err(n, format!("bad cart-ram length `{len}`")))?;
        if hex.len() != len * 2 {
            return Err(err(
                n,
                format!(
                    "cart-ram has {} hex digits, expected {}",
                    hex.len(),
                    len * 2
                ),
            ));
        }
        let cart_ram = (0..len)
            .map(|i| {
                hex.get(i * 2..i * 2 + 2)
                    .map_or(Err(String::new()), hex_byte)
            })
            .collect::<Result<Vec<u8>, String>>()
            .map_err(|_| err(n, "bad cart-ram hex".into()))?;
        let (n, v) = next("rtc")?;
        let rtc = match v.split(' ').collect::<Vec<_>>()[..] {
            ["none"] => None,
            [live, latched, sub] => {
                let sub: u32 = sub
                    .parse()
                    .ok()
                    .filter(|&s| s < gbc_core::CLOCK_HZ)
                    .ok_or_else(|| err(n, format!("bad RTC subsecond `{sub}`")))?;
                Some(RtcState {
                    live: parse_time(live).map_err(|e| err(n, e))?,
                    latched: parse_time(latched).map_err(|e| err(n, e))?,
                    subsecond_clocks: sub,
                })
            }
            _ => return Err(err(n, format!("bad rtc line `{v}`"))),
        };
        let (n, v) = next("events")?;
        if !v.is_empty() {
            return Err(err(n, "`events` takes no value".into()));
        }
        let header = Header {
            rom_title,
            header_checksum,
            global_checksum,
            rom_hash,
            dmg_palette,
            sav,
            cart_ram,
            rtc,
        };

        let mut events = Vec::new();
        let mut last = 0u64;
        let mut frames = None;
        for (i, l) in lines {
            let n = i + 1;
            let l = l.strip_suffix('\r').unwrap_or(l);
            if l.is_empty() {
                continue; // the final newline
            }
            if frames.is_some() {
                return Err(err(n, "text after the `end` line".into()));
            }
            let f: Vec<&str> = l.split(' ').collect();
            if let ["end", count] = f[..] {
                let count: u64 = count
                    .parse()
                    .map_err(|_| err(n, format!("bad frame count `{count}`")))?;
                if events.last().is_some() && last >= count {
                    return Err(err(
                        n,
                        format!("an event at frame {last} is past the end ({count} frames)"),
                    ));
                }
                frames = Some(count);
                continue;
            }
            let (frame, ev) = match f[..] {
                [frame, "b", hex] => (frame, Some(hex)),
                [frame, "m"] => (frame, None),
                _ => return Err(err(n, format!("bad event `{l}`"))),
            };
            let frame: u64 = frame
                .parse()
                .map_err(|_| err(n, format!("bad frame number `{frame}`")))?;
            if frame < last {
                return Err(err(
                    n,
                    format!("frame {frame} comes after frame {last} (out of order)"),
                ));
            }
            last = frame;
            events.push(match ev {
                Some(hex) => {
                    let b = hex_byte(hex).map_err(|e| err(n, e))?;
                    Event::Buttons(frame, Buttons::from_bits_truncate(b))
                }
                None => Event::Mark(frame),
            });
        }
        let frames = frames.ok_or("no `end` line (the recording is incomplete)")?;
        Ok(Recording {
            header,
            events,
            frames,
        })
    }
}

/// Writes a recording as the run goes. The events are buffered in memory
/// (a few bytes per button change) and the file is written by
/// [`Recorder::finish`]; a mark also writes everything so far, so a crash
/// keeps the marks.
pub struct Recorder {
    out: BufWriter<std::fs::File>,
    held: Option<Buttons>,
    /// Frames passed to [`Recorder::frame`].
    frames: u64,
}

impl Recorder {
    pub fn create(path: &Path, header: &Header) -> Result<Self, String> {
        let f = std::fs::File::create(path)
            .map_err(|e| format!("can't create {}: {e}", path.display()))?;
        let mut out = BufWriter::new(f);
        let mut text = String::new();
        header.write(&mut text);
        out.write_all(text.as_bytes())
            .map_err(|e| format!("can't write {}: {e}", path.display()))?;
        Ok(Recorder {
            out,
            held: None,
            frames: 0,
        })
    }

    /// Frame `self.frames` is about to run with `buttons` held.
    pub fn frame(&mut self, buttons: Buttons) -> io::Result<()> {
        if self.held != Some(buttons) && (self.held.is_some() || !buttons.is_empty()) {
            writeln!(self.out, "{} b {:02x}", self.frames, buttons.bits())?;
        }
        self.held = Some(buttons);
        self.frames += 1;
        Ok(())
    }

    /// Marks the last frame run (frame 0 if none ran yet); returns it.
    pub fn mark(&mut self) -> io::Result<u64> {
        let f = self.frames.saturating_sub(1);
        writeln!(self.out, "{f} m")?;
        self.out.flush()?;
        Ok(f)
    }

    /// Writes the `end` line and flushes. Returns the frame count.
    pub fn finish(mut self) -> io::Result<u64> {
        // A mark before any frame ran names frame 0: the end must cover it.
        writeln!(self.out, "end {}", self.frames.max(1))?;
        self.out.flush()?;
        Ok(self.frames)
    }
}

/// Feeds a [`Recording`] back frame by frame.
pub struct Player {
    events: Vec<Event>,
    next: usize,
    held: Buttons,
    frames: u64,
    /// The frame about to run.
    frame: u64,
}

impl Player {
    pub fn new(r: &Recording) -> Self {
        Player {
            events: r.events.clone(),
            next: 0,
            held: Buttons::default(),
            frames: r.frames,
            frame: 0,
        }
    }

    /// Frames in the recording.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Whether every recorded frame has run.
    pub fn done(&self) -> bool {
        self.frame >= self.frames
    }

    /// The buttons for the frame about to run, and whether it is marked.
    /// Advances to the next frame.
    pub fn frame(&mut self) -> (Buttons, bool) {
        let mut mark = false;
        while let Some(&ev) = self.events.get(self.next) {
            match ev {
                Event::Buttons(f, b) if f == self.frame => self.held = b,
                Event::Mark(f) if f == self.frame => mark = true,
                _ => break,
            }
            self.next += 1;
        }
        self.frame += 1;
        (self.held, mark)
    }
}

/// `--frame-hashes`: one line per frame, `<frame> <ready> <blank>
/// <fnv1a64 of the visible picture>` (ready and blank 0/1), written as the
/// run goes (buffered). Two runs of the same recording must give
/// identical files.
pub struct FrameHashes {
    out: BufWriter<std::fs::File>,
    frame: u64,
}

impl FrameHashes {
    pub fn create(path: &Path) -> Result<Self, String> {
        let f = std::fs::File::create(path)
            .map_err(|e| format!("can't create {}: {e}", path.display()))?;
        Ok(FrameHashes {
            out: BufWriter::new(f),
            frame: 0,
        })
    }

    /// One frame ran; `visible` is what the screen shows after it.
    pub fn frame(&mut self, ready: bool, blank: bool, visible: &gbc_core::Frame) -> io::Result<()> {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for p in visible {
            for b in p.to_le_bytes() {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        writeln!(
            self.out,
            "{} {} {} {h:016x}",
            self.frame,
            u8::from(ready),
            u8::from(blank)
        )?;
        self.frame += 1;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        self.out.flush()
    }
}

/// The input options of a run (`--record-input`, `--replay-input`,
/// `--frame-hashes`).
#[derive(Default, Clone, Copy)]
pub struct Inputs<'a> {
    pub record: Option<&'a Path>,
    pub replay: Option<&'a Recording>,
    pub hashes: Option<&'a Path>,
}

/// Where each frame's buttons come from, plus the optional recording and
/// hash outputs: the one place the window and headless loops share, so
/// both set buttons the same way.
pub struct Driver {
    replay: Option<Player>,
    recorder: Option<Recorder>,
    hashes: Option<FrameHashes>,
    /// The frame about to run.
    frame: u64,
    error: Option<String>,
}

impl Driver {
    pub fn new(
        replay: Option<Player>,
        recorder: Option<Recorder>,
        hashes: Option<FrameHashes>,
    ) -> Self {
        Driver {
            replay,
            recorder,
            hashes,
            frame: 0,
            error: None,
        }
    }

    /// Opens the outputs; `start` is the state the run started from.
    pub fn open(inputs: &Inputs<'_>, start: &Header) -> Result<Self, String> {
        let recorder = inputs
            .record
            .map(|p| Recorder::create(p, start))
            .transpose()?;
        let hashes = inputs.hashes.map(FrameHashes::create).transpose()?;
        Ok(Driver::new(
            inputs.replay.map(Player::new),
            recorder,
            hashes,
        ))
    }

    /// Frames a replay has in all (`None` when not replaying).
    pub fn replay_frames(&self) -> Option<u64> {
        self.replay.as_ref().map(Player::frames)
    }

    /// Whether marks (key M) mean anything: recording, or `logging`.
    pub fn marks_enabled(&self, logging: bool) -> bool {
        self.recorder.is_some() || logging
    }

    /// A replay has run every recorded frame.
    pub fn replay_done(&self) -> bool {
        self.replay.as_ref().is_some_and(Player::done)
    }

    /// The buttons for the frame about to run: the recording's when
    /// replaying (`live` is ignored), else `live`. Also returns whether a
    /// replayed mark falls on this frame (printed to stderr here).
    pub fn begin_frame(&mut self, live: Buttons) -> (Buttons, bool) {
        let (b, mark) = match &mut self.replay {
            Some(p) => p.frame(),
            None => (live, false),
        };
        if mark {
            eprintln!("mark at frame {}", self.frame);
        }
        if let Some(r) = &mut self.recorder
            && let Err(e) = r.frame(b)
        {
            self.fail(format!("can't write the input recording: {e}"));
        }
        self.frame += 1;
        (b, mark)
    }

    /// The frame begun last finished; `visible` is the picture showing.
    pub fn end_frame(&mut self, ready: bool, blank: bool, visible: &gbc_core::Frame) {
        if let Some(h) = &mut self.hashes
            && let Err(e) = h.frame(ready, blank, visible)
        {
            self.fail(format!("can't write the frame hashes: {e}"));
        }
    }

    /// Key M: marks the last frame run. Returns its number.
    pub fn mark(&mut self) -> u64 {
        let f = match &mut self.recorder {
            Some(r) => match r.mark() {
                Ok(f) => f,
                Err(e) => {
                    self.fail(format!("can't write the input recording: {e}"));
                    self.frame.saturating_sub(1)
                }
            },
            None => self.frame.saturating_sub(1),
        };
        eprintln!("mark at frame {f}");
        f
    }

    fn fail(&mut self, msg: String) {
        if self.error.is_none() {
            eprintln!("error: {msg}");
            self.error = Some(msg);
        }
        // Stop writing: a half-written recording is refused on replay.
        self.recorder = None;
    }

    /// Finishes the outputs. `Err` if any write failed.
    pub fn finish(mut self) -> Result<(), String> {
        if let Some(r) = self.recorder.take() {
            let n = r
                .finish()
                .map_err(|e| format!("can't write the input recording: {e}"))?;
            eprintln!("recorded {n} frames");
        }
        if let Some(h) = self.hashes.take() {
            h.finish()
                .map_err(|e| format!("can't write the frame hashes: {e}"))?;
        }
        self.error.map_or(Ok(()), Err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> Header {
        Header {
            rom_title: "TEST GAME".into(),
            header_checksum: 0x3c,
            global_checksum: 0xa1b2,
            rom_hash: 0x0123_4567_89ab_cdef,
            dmg_palette: Mode::Gray,
            sav: Some((4, 0xfeed)),
            cart_ram: vec![0, 0x42, 0xff, 7],
            rtc: Some(RtcState {
                live: RtcTime {
                    seconds: 59,
                    minutes: 1,
                    hours: 23,
                    days: 300,
                    halted: false,
                    day_carry: true,
                },
                latched: RtcTime {
                    seconds: 3,
                    minutes: 0,
                    hours: 0,
                    days: 0,
                    halted: true,
                    day_carry: false,
                },
                subsecond_clocks: 12345,
            }),
        }
    }

    fn text(h: &Header, events: &str) -> String {
        let mut s = String::new();
        h.write(&mut s);
        s.push_str(events);
        s
    }

    #[test]
    fn recorder_output_parses_back() {
        let dir = std::env::temp_dir().join(format!("gbc-inlog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("r.txt");
        let h = header();
        let mut r = Recorder::create(&p, &h).unwrap();
        let seq = [
            Buttons::default(),
            Buttons::default(),
            Buttons::A,
            Buttons::A,
            Buttons::A | Buttons::UP,
            Buttons::default(),
        ];
        for (i, &b) in seq.iter().enumerate() {
            r.frame(b).unwrap();
            if i == 3 {
                assert_eq!(r.mark().unwrap(), 3);
            }
        }
        assert_eq!(r.finish().unwrap(), 6);
        let rec = Recording::load(&p).unwrap();
        assert_eq!(rec.header, h);
        assert_eq!(rec.frames, 6);
        assert_eq!(
            rec.events,
            vec![
                Event::Buttons(2, Buttons::A),
                Event::Mark(3),
                Event::Buttons(4, Buttons::A | Buttons::UP),
                Event::Buttons(5, Buttons::default()),
            ]
        );
        let mut p = Player::new(&rec);
        let got: Vec<_> = (0..6).map(|_| p.frame()).collect();
        assert!(p.done());
        let want: Vec<_> = seq.iter().enumerate().map(|(i, &b)| (b, i == 3)).collect();
        assert_eq!(got, want);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_save_no_rtc_round_trips() {
        let h = Header {
            sav: None,
            cart_ram: Vec::new(),
            rtc: None,
            dmg_palette: Mode::Auto,
            ..header()
        };
        let r = Recording::parse(&text(&h, "0 b 08\nend 1\n")).unwrap();
        assert_eq!(r.header, h);
        assert_eq!(r.events, vec![Event::Buttons(0, Buttons::START)]);
    }

    #[test]
    fn format_errors_are_rejected() {
        let good = text(&header(), "1 b 01\n2 m\nend 3\n");
        assert!(Recording::parse(&good).is_ok());
        let cases: &[(&str, &str, &str)] = &[
            (
                "gbc-32 input 1",
                "gbc-32 input 2",
                "unsupported format version",
            ),
            (
                "gbc-32 input 1",
                "something else",
                "not a gbc-32 input recording",
            ),
            (
                "rom-header-checksum 3c",
                "rom-header-checksum 3",
                "bad hex byte",
            ),
            (
                "rom-fnv1a64 0123456789abcdef",
                "rom-fnv1a64 xyz",
                "bad hash",
            ),
            ("dmg-palette gray", "dmg-palette blue", "bad palette"),
            ("cart-ram 4 0042ff07", "cart-ram 4 0042ff", "hex digits"),
            (
                "cart-ram 4 0042ff07",
                "cart-ram 4 0042fz07",
                "bad cart-ram hex",
            ),
            (
                "rtc 59:1:23:300:0:1",
                "rtc 64:1:23:300:0:1",
                "bad RTC field",
            ),
            ("12345\n", "99999999\n", "bad RTC subsecond"),
            ("sav loaded 4", "sav maybe 4", "bad sav line"),
            ("1 b 01", "1 b 1", "bad hex byte"),
            ("1 b 01", "1 x 01", "bad event"),
            ("2 m", "0 m", "out of order"),
            ("end 3", "end 2", "past the end"),
            ("end 3\n", "end 3\n4 m\n", "after the `end` line"),
            ("end 3\n", "", "no `end` line"),
        ];
        for &(from, to, msg) in cases {
            assert!(good.contains(from), "{from}");
            let bad = good.replacen(from, to, 1);
            match Recording::parse(&bad) {
                Err(e) => assert!(e.contains(msg), "{from} -> {to}: {e}"),
                Ok(_) => panic!("{from} -> {to} was accepted"),
            }
        }
        assert!(Recording::parse("").is_err());
        let truncated = &good[..good.find("rtc").unwrap()];
        assert!(
            Recording::parse(truncated)
                .unwrap_err()
                .contains("expected `rtc ...`")
        );
    }

    #[test]
    fn rom_check_compares_checksums_and_hash() {
        let h = header();
        let mut id = RomId {
            title: "OTHER".into(),
            header_checksum: 0x3c,
            global_checksum: 0xa1b2,
            hash: 0x0123_4567_89ab_cdef,
        };
        assert!(h.check_rom(&id).is_ok(), "the title is informational");
        id.hash ^= 1;
        assert!(h.check_rom(&id).is_err());
        id.hash ^= 1;
        id.global_checksum ^= 1;
        assert!(h.check_rom(&id).is_err());
    }

    #[test]
    fn rom_id_reads_the_header() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x134..0x13E].copy_from_slice(b"WARIOLAND3");
        rom[0x14D] = 0x3c;
        rom[0x14E] = 0xa1;
        rom[0x14F] = 0xb2;
        let id = RomId::of(&rom);
        assert_eq!(id.title, "WARIOLAND3");
        assert_eq!((id.header_checksum, id.global_checksum), (0x3c, 0xa1b2));
        assert_eq!(id.hash, fnv1a64(&rom));
        assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
