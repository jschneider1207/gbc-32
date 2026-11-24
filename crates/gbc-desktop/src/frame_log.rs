//! `GBC_FRAME_LOG=<path>`: one CSV row per emulated frame in the window
//! loop, for measuring stutter (columns: `docs/desktop-diagnostics.md`).
//!
//! The main thread only fills fixed-size [`Row`]s in a preallocated
//! buffer; every [`BATCH`] rows the buffer is handed to a writer thread,
//! which formats and writes them. A row stays in the buffer until the next
//! frame starts, because presents of its picture are added to it.
//!
//! On Windows each present also records where it fell on the display's
//! refresh grid ([`dwm::Refresh`]). The writer turns that into the
//! refresh the picture is estimated to have landed on (the first vertical
//! blank after the present returned) and how many refreshes it stayed on
//! screen: the next present's landing refresh minus this one's (0 = never
//! shown, replaced before a refresh; 2+ = shown again).

use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::dwm::{self, Refresh};

/// Rows handed to the writer at a time (about 10 s of frames).
pub const BATCH: usize = 600;

pub const COLUMNS: &str = "frame,wake_us,wake_late_us,emu_start_us,emu_end_us,ready,blank,changed,motion_dx,motion_dy,motion_match,audio_fill_ms,mark,\
presents,present_start_us,present_end_us,refresh_period_us,refresh_at_present,vblank_phase_us,\
landed_refresh,on_screen_refreshes";

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Row {
    pub frame: u64,
    /// When the event-loop wake-up that ran this frame began.
    pub wake_us: Option<u64>,
    /// How late that wake-up was against the time the loop asked for
    /// (timer wake-ups only; `None` when woken early by an event).
    pub wake_late_us: Option<u64>,
    pub emu_start_us: u64,
    pub emu_end_us: u64,
    pub ready: bool,
    pub blank: bool,
    pub changed: bool,
    /// How the picture moved since the previous one ([`motion`]); `None`
    /// unless both are pictures (not blank).
    pub motion: Option<Motion>,
    pub audio_fill_ms: Option<f64>,
    pub mark: bool,
    /// Presents while this was the newest frame.
    pub presents: u32,
    /// The first of them: before the blit, after `present` returned.
    pub present_us: Option<(u64, u64)>,
    /// Where that present returned on the refresh grid (Windows).
    pub refresh: Option<Refresh>,
}

impl Row {
    fn landed(&self) -> Option<u64> {
        self.refresh.map(|r| r.count + 1)
    }

    /// The CSV line, with `on_screen` (refreshes until the next present
    /// landed) when known.
    pub fn csv(&self, on_screen: Option<u64>) -> String {
        let opt = |v: Option<String>| v.unwrap_or_default();
        let b = |v: bool| u8::from(v);
        format!(
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            self.frame,
            opt(self.wake_us.map(|t| t.to_string())),
            opt(self.wake_late_us.map(|t| t.to_string())),
            self.emu_start_us,
            self.emu_end_us,
            b(self.ready),
            b(self.blank),
            b(self.changed),
            opt(self.motion.map(|m| m.dx.to_string())),
            opt(self.motion.map(|m| m.dy.to_string())),
            opt(self.motion.map(|m| format!("{:.3}", m.matched))),
            opt(self.audio_fill_ms.map(|f| format!("{f:.2}"))),
            b(self.mark),
            self.presents,
            opt(self.present_us.map(|p| p.0.to_string())),
            opt(self.present_us.map(|p| p.1.to_string())),
            opt(self.refresh.map(|r| format!("{:.1}", r.period_us))),
            opt(self.refresh.map(|r| r.count.to_string())),
            opt(self.refresh.map(|r| format!("{:.1}", r.phase_us))),
            opt(self.landed().map(|l| l.to_string())),
            opt(on_screen.map(|n| n.to_string())),
        )
    }
}

/// Formats rows in order, holding back each presented row until the next
/// present shows how long it stayed on screen.
pub struct Formatter {
    held: Vec<Row>,
}

impl Formatter {
    pub fn new() -> Self {
        Formatter { held: Vec::new() }
    }

    /// Lines ready to write after `rows`.
    pub fn push(&mut self, rows: &[Row], out: &mut Vec<String>) {
        for &r in rows {
            if r.presents > 0
                && let Some(first) = self.held.first()
            {
                let on = match (first.landed(), r.landed()) {
                    (Some(a), Some(b)) => Some(b.saturating_sub(a)),
                    _ => None,
                };
                for (i, h) in self.held.drain(..).enumerate() {
                    out.push(h.csv(if i == 0 { on } else { None }));
                }
            }
            if r.presents > 0 || !self.held.is_empty() {
                self.held.push(r);
            } else {
                out.push(r.csv(None));
            }
        }
    }

    /// The rest, at the end of the log (the last present's time on
    /// screen is unknown).
    pub fn finish(&mut self, out: &mut Vec<String>) {
        out.extend(self.held.drain(..).map(|h| h.csv(None)));
    }
}

/// The header: comment lines, then the column names.
pub fn header(rom_title: &str, rate: Option<(u32, u32)>) -> String {
    let rate = match rate {
        Some((n, d)) => format!("{n}/{d} Hz = {:.3} Hz", f64::from(n) / f64::from(d)),
        None => "unavailable".into(),
    };
    format!(
        "# gbc-32 frame log 1\n# rom: {rom_title}\n# display refresh (DWM rateRefresh): {rate}\n\
         # columns: docs/desktop-diagnostics.md\n{COLUMNS}\n"
    )
}

/// One emulated frame, for [`FrameLog::frame`].
pub struct Emulated {
    pub frame: u64,
    /// The wake-up that ran it, and how late it was (see [`Row`]).
    pub wake: Option<Instant>,
    pub wake_late: Option<Duration>,
    /// `run_frame` began and returned.
    pub start: Instant,
    pub end: Instant,
    /// It completed a picture (`FRAME_READY`).
    pub ready: bool,
    /// The screen is blank (white) after it.
    pub blank: bool,
    /// What the screen shows differs from before it.
    pub changed: bool,
    pub motion: Option<Motion>,
    pub audio_fill_ms: Option<f64>,
}

/// The shift of the picture's content since the previous picture: the
/// game-side motion (e.g. a scroll step), derived from the frame buffers
/// because the core's public API does not expose SCX/SCY. Content that
/// moved right by 2 has `dx = 2` (the camera moved left: SCX fell by 2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Motion {
    pub dx: i8,
    pub dy: i8,
    /// Share of the compared pixels that match at that shift (1.0 =
    /// a pure shift; lower when sprites move or new tiles appear).
    pub matched: f32,
}

/// What a frame changed on screen, given the previous picture `prev`, the
/// frame buffer `cur` it just drew into (a new picture if `ready`), `same`
/// (`ready` and `cur == prev`) and the blank state before and after:
/// whether what the screen shows changed (white <-> picture, or a new
/// picture that differs), and the picture's [`motion`] when a picture
/// followed a picture.
pub fn picture_change(
    prev: &gbc_core::Frame,
    cur: &gbc_core::Frame,
    ready: bool,
    same: bool,
    was_blank: bool,
    blank: bool,
) -> (bool, Option<Motion>) {
    let changed = was_blank != blank || (ready && !blank && !same);
    let still = Motion {
        dx: 0,
        dy: 0,
        matched: 1.0,
    };
    let motion =
        (ready && !blank && !was_blank).then(|| if same { still } else { motion(prev, cur) });
    (changed, motion)
}

/// Largest shift [`motion`] tries, per axis, in pixels.
pub const MAX_SHIFT: i32 = 8;

/// Finds the horizontal or vertical shift (up to [`MAX_SHIFT`]; screen
/// slides and scrolling are one axis at a time) that best maps `prev`
/// onto `cur`, over every third row. Ties go to the smaller shift, so a
/// still or flat picture reads as 0. About 0.25 M pixel compares; only run
/// when the frame log is on.
pub fn motion(prev: &gbc_core::Frame, cur: &gbc_core::Frame) -> Motion {
    use gbc_core::{SCREEN_HEIGHT as H, SCREEN_WIDTH as W};
    let score = |dx: i32, dy: i32| -> (u32, u32) {
        let (mut hit, mut n) = (0, 0);
        let (x0, x1) = (dx.max(0), (W as i32 + dx).min(W as i32));
        let (y0, y1) = (dy.max(0), (H as i32 + dy).min(H as i32));
        for y in (y0..y1).step_by(3) {
            let c = &cur[y as usize * W..][x0 as usize..x1 as usize];
            let p = &prev[(y - dy) as usize * W..][(x0 - dx) as usize..(x1 - dx) as usize];
            hit += c.iter().zip(p).filter(|(a, b)| a == b).count() as u32;
            n += c.len() as u32;
        }
        (hit, n)
    };
    let (h, n) = score(0, 0);
    let mut best = (0i32, 0i32, f64::from(h) / f64::from(n.max(1)));
    for d in 1..=MAX_SHIFT {
        for (dx, dy) in [(-d, 0), (d, 0), (0, -d), (0, d)] {
            let (h, n) = score(dx, dy);
            let f = f64::from(h) / f64::from(n.max(1));
            if f > best.2 {
                best = (dx, dy, f);
            }
        }
    }
    Motion {
        dx: best.0 as i8,
        dy: best.1 as i8,
        matched: best.2 as f32,
    }
}

pub struct FrameLog {
    t0: Instant,
    rows: Vec<Row>,
    tx: Option<mpsc::Sender<Vec<Row>>>,
    writer: Option<JoinHandle<io::Result<u64>>>,
    path: PathBuf,
}

/// `Some` when `GBC_FRAME_LOG` names a file; `Err` if it can't be created.
pub fn from_env(t0: Instant, rom_title: &str) -> Option<Result<FrameLog, String>> {
    let p = std::env::var_os("GBC_FRAME_LOG").filter(|p| !p.is_empty())?;
    Some(FrameLog::create(Path::new(&p), t0, rom_title))
}

impl FrameLog {
    pub fn create(path: &Path, t0: Instant, rom_title: &str) -> Result<Self, String> {
        let f = std::fs::File::create(path)
            .map_err(|e| format!("can't create {}: {e}", path.display()))?;
        let head = header(rom_title, dwm::refresh_rate());
        let (tx, rx) = mpsc::channel::<Vec<Row>>();
        let writer = std::thread::Builder::new()
            .name("frame-log".into())
            .spawn(move || {
                let mut out = BufWriter::new(f);
                out.write_all(head.as_bytes())?;
                let mut fmt = Formatter::new();
                let mut lines = Vec::new();
                let mut n = 0u64;
                let mut write = |lines: &mut Vec<String>, out: &mut BufWriter<_>| {
                    for l in lines.drain(..) {
                        writeln!(out, "{l}")?;
                        n += 1;
                    }
                    io::Result::Ok(n)
                };
                for rows in rx {
                    fmt.push(&rows, &mut lines);
                    write(&mut lines, &mut out)?;
                }
                fmt.finish(&mut lines);
                let n = write(&mut lines, &mut out)?;
                out.flush()?;
                Ok(n)
            })
            .map_err(|e| format!("can't start the frame log writer: {e}"))?;
        Ok(FrameLog {
            t0,
            rows: Vec::with_capacity(BATCH + 1),
            tx: Some(tx),
            writer: Some(writer),
            path: path.to_owned(),
        })
    }

    fn us(&self, t: Instant) -> u64 {
        t.saturating_duration_since(self.t0).as_micros() as u64
    }

    /// A frame was emulated.
    pub fn frame(&mut self, e: &Emulated) {
        if self.rows.len() >= BATCH
            && let Some(tx) = &self.tx
        {
            let full = std::mem::replace(&mut self.rows, Vec::with_capacity(BATCH + 1));
            if tx.send(full).is_err() {
                self.tx = None; // the writer failed; `finish` reports it
            }
        }
        let row = Row {
            frame: e.frame,
            wake_us: e.wake.map(|t| self.us(t)),
            wake_late_us: e.wake_late.map(|d| d.as_micros() as u64),
            emu_start_us: self.us(e.start),
            emu_end_us: self.us(e.end),
            ready: e.ready,
            blank: e.blank,
            changed: e.changed,
            motion: e.motion,
            audio_fill_ms: e.audio_fill_ms,
            mark: false,
            presents: 0,
            present_us: None,
            refresh: None,
        };
        self.rows.push(row);
    }

    /// Marks the newest frame.
    pub fn mark(&mut self) {
        if let Some(r) = self.rows.last_mut() {
            r.mark = true;
        }
    }

    /// A present ran from `start` to `end`; `refresh` is where `end` fell
    /// on the refresh grid. Counted against the newest frame.
    pub fn presented(&mut self, start: Instant, end: Instant, refresh: Option<Refresh>) {
        let (s, e) = (self.us(start), self.us(end));
        if let Some(r) = self.rows.last_mut() {
            r.presents += 1;
            if r.present_us.is_none() {
                r.present_us = Some((s, e));
                r.refresh = refresh;
            }
        }
    }

    /// Writes the rest and waits for the writer. Returns the row count.
    pub fn finish(mut self) -> Result<u64, String> {
        let err = |e: String| format!("can't write {}: {e}", self.path.display());
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(std::mem::take(&mut self.rows));
        }
        match self.writer.take().map(JoinHandle::join) {
            Some(Ok(Ok(n))) => Ok(n),
            Some(Ok(Err(e))) => Err(err(e.to_string())),
            Some(Err(_)) => Err(err("the writer thread panicked".into())),
            None => Ok(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refresh(count: u64, phase_us: f64) -> Option<Refresh> {
        Some(Refresh {
            period_us: 16_666.7,
            count,
            phase_us,
        })
    }

    #[test]
    fn row_csv_columns() {
        let r = Row {
            frame: 7,
            wake_us: Some(90),
            wake_late_us: Some(35),
            emu_start_us: 100,
            emu_end_us: 420,
            ready: true,
            blank: false,
            changed: true,
            motion: Some(Motion {
                dx: -4,
                dy: 0,
                matched: 0.9876,
            }),
            audio_fill_ms: Some(51.234),
            mark: true,
            presents: 1,
            present_us: Some((500, 2500)),
            refresh: refresh(1000, 1234.56),
        };
        assert_eq!(COLUMNS.split(',').count(), r.csv(None).split(',').count());
        assert_eq!(
            r.csv(Some(2)),
            "7,90,35,100,420,1,0,1,-4,0,0.988,51.23,1,1,500,2500,16666.7,1000,1234.6,1001,2"
        );
        let r = Row {
            wake_us: None,
            wake_late_us: None,
            motion: None,
            audio_fill_ms: None,
            presents: 0,
            present_us: None,
            refresh: None,
            mark: false,
            ready: false,
            blank: true,
            changed: false,
            ..r
        };
        assert_eq!(r.csv(None), "7,,,100,420,0,1,0,,,,,0,0,,,,,,,");
    }

    fn row(frame: u64, presents: u32, refresh: Option<Refresh>) -> Row {
        Row {
            frame,
            wake_us: None,
            wake_late_us: None,
            emu_start_us: 0,
            emu_end_us: 0,
            ready: true,
            blank: false,
            changed: true,
            motion: None,
            audio_fill_ms: None,
            mark: false,
            presents,
            present_us: (presents > 0).then_some((1, 2)),
            refresh,
        }
    }

    fn on_screen(line: &str) -> &str {
        line.rsplit(',').next().unwrap()
    }

    #[test]
    fn on_screen_is_the_gap_to_the_next_landing() {
        let mut f = Formatter::new();
        let mut out = Vec::new();
        f.push(
            &[
                row(0, 1, refresh(10, 0.0)),
                row(1, 1, refresh(11, 0.0)), // shown 1 refresh
                row(2, 0, None),             // never presented
                row(3, 1, refresh(13, 0.0)), // frame 1 stayed 2
            ],
            &mut out,
        );
        // Frame 3 waits for the next present, in the next batch.
        assert_eq!(out.len(), 3);
        f.push(&[row(4, 1, refresh(13, 0.0))], &mut out);
        assert_eq!(out.len(), 4);
        f.finish(&mut out);
        let got: Vec<_> = out.iter().map(|l| on_screen(l)).collect();
        assert_eq!(got, ["1", "2", "", "0", ""]);
        let frames: Vec<_> = out
            .iter()
            .map(|l| l.split(',').next().unwrap().to_owned())
            .collect();
        assert_eq!(frames, ["0", "1", "2", "3", "4"], "in order");
    }

    #[test]
    fn log_file_round_trip() {
        let dir = std::env::temp_dir().join(format!("gbc-flog-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("f.csv");
        let t0 = Instant::now();
        let us = |n| t0 + Duration::from_micros(n);
        let mut log = FrameLog::create(&p, t0, "TITLE").unwrap();
        let n = BATCH as u64 + 5; // crosses a batch boundary
        for i in 0..n {
            log.frame(&Emulated {
                frame: i,
                wake: Some(us(i * 1000)),
                wake_late: (i == 1).then_some(Duration::from_micros(250)),
                start: us(i * 1000),
                end: us(i * 1000 + 300),
                ready: true,
                blank: false,
                changed: true,
                motion: None,
                audio_fill_ms: Some(50.0),
            });
            if i == 3 {
                log.mark();
            }
            if i % 2 == 0 {
                log.presented(us(i * 1000 + 400), us(i * 1000 + 900), refresh(i, 0.0));
                log.presented(us(i * 1000 + 950), us(i * 1000 + 990), refresh(i + 1, 0.0));
            }
        }
        assert_eq!(log.finish().unwrap(), n);
        let text = std::fs::read_to_string(&p).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("# gbc-32 frame log 1"));
        assert_eq!(lines.next(), Some("# rom: TITLE"));
        let rows: Vec<_> = lines.filter(|l| !l.starts_with('#')).collect();
        assert_eq!(rows[0], COLUMNS);
        assert_eq!(rows.len() as u64, n + 1);
        assert_eq!(
            rows[1],
            "0,0,,0,300,1,0,1,,,,50.00,0,2,400,900,16666.7,0,0.0,1,2"
        );
        assert_eq!(rows[2], "1,1000,250,1000,1300,1,0,1,,,,50.00,0,0,,,,,,,");
        assert!(
            rows[4].starts_with("3,3000,,3000,3300,1,0,1,,,,50.00,1,0,"),
            "{}",
            rows[4]
        );
        assert!(rows[n as usize].starts_with(&format!("{},", n - 1)));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn motion_finds_the_scroll_step() {
        use gbc_core::{Frame, SCREEN_HEIGHT as H, SCREEN_WIDTH as W};
        // A busy, non-repeating pattern.
        let pat = |x: i32, y: i32| ((x * 7 + y * 13 + (x * y) % 11) & 0x7FFF) as u16;
        let make = |ox: i32, oy: i32| -> Box<Frame> {
            let mut f: Box<Frame> = Box::new([0; W * H]);
            for y in 0..H {
                for x in 0..W {
                    f[y * W + x] = pat(x as i32 - ox, y as i32 - oy);
                }
            }
            f
        };
        let base = make(0, 0);
        assert_eq!(
            motion(&base, &base),
            Motion {
                dx: 0,
                dy: 0,
                matched: 1.0
            }
        );
        for (dx, dy) in [(-4, 0), (3, 0), (0, 8), (0, -1)] {
            let m = motion(&base, &make(dx, dy));
            assert_eq!((m.dx, m.dy), (dx as i8, dy as i8), "{dx},{dy}");
            assert!(m.matched > 0.99, "{m:?}");
        }
        // A flat picture: every shift matches; reads as still.
        let flat: Box<Frame> = Box::new([5; W * H]);
        let m = motion(&flat, &flat);
        assert_eq!((m.dx, m.dy), (0, 0));
    }

    #[test]
    fn header_names_the_refresh_rate() {
        let h = header("X", Some((60000, 1001)));
        assert!(h.contains("60000/1001 Hz = 59.940 Hz"), "{h}");
        assert!(header("X", None).contains("unavailable"));
        assert!(h.ends_with(&format!("{COLUMNS}\n")));
    }
}
