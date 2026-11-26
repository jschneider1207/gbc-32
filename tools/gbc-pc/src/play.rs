//! `play`: live input to the board and save dumps while a game runs
//! (Stage 2 M5). One thread, one loop: poll the keyboard, send an `Input`
//! frame when [`InputSchedule`] says so, start a dump on an F5 press,
//! then wait up to [`PlayOptions::poll`] (10 ms, so about 100 Hz) for a
//! board frame and handle it: `Stats` are reported, dump frames go to the
//! dump in progress. Everything here is host-tested against
//! [`crate::sim`]; the Win32 keyboard is [`crate::keyboard`].
//!
//! `Input` frames are not answered, so a `Nak` here usually answers one
//! the board could not decode: only a `Nak` with the dump request's
//! `seq` fails a dump; the rest are counted and ignored. A dump the board
//! starts on its own (a `SaveBegin` while none was asked for, e.g. its
//! button combo) is received the same way.
//!
//! With [`PlayOptions::audio`] the loop first asks for the audio stream
//! (`AudioStart`, answered `Ack`; ADR 0041), hands each `Audio` message
//! to the caller as [`PlayEvent::Audio`] and sends `AudioStop` at the end.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gbc_link::Message;

use crate::client::{Client, Error, Failure, Incoming, Received};
use crate::input::{InputSchedule, Key, KeySet, REFRESH};
use crate::rtc::WallClock;
use crate::save::{DumpRx, DumpStep, write_atomic};
use crate::transport::Transport;

/// The held keys, polled once per loop.
pub trait Keyboard {
    fn held(&mut self) -> io::Result<KeySet>;
}

#[derive(Clone, Debug)]
pub struct PlayOptions {
    /// Stop after this long (`--seconds`); `None` runs until Esc.
    pub duration: Option<Duration>,
    /// Where F5 (or a board-started dump) writes the save.
    pub sav_out: Option<PathBuf>,
    /// Longest wait for a board frame per loop.
    pub poll: Duration,
    /// `Input` refresh period ([`REFRESH`]).
    pub refresh: Duration,
    /// Restamps a dump's clock footer with this clock's time as it is
    /// written ([`crate::rtc::from_dump`]); `None` (`--raw-save`) writes
    /// it as received.
    pub clock: Option<WallClock>,
    /// Ask for the board's audio stream (`--audio-wav`).
    pub audio: bool,
}

impl Default for PlayOptions {
    fn default() -> Self {
        Self {
            duration: None,
            sav_out: None,
            poll: Duration::from_millis(10),
            refresh: REFRESH,
            clock: None,
            audio: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayEvent {
    Stats {
        frames: u32,
        cycles_mean: u32,
        cycles_max: u32,
        /// The board's extra fields as text (empty if none).
        extra: String,
    },
    /// A dump was asked for (F5) or a board-started one began.
    DumpStarted,
    /// A dump attempt failed and is being retried.
    DumpRetry(Failure),
    /// A dump gave up after its retries.
    DumpFailed(Failure),
    /// A complete, verified save was written.
    Saved { path: PathBuf, len: usize },
    /// A verified save could not be written (the play goes on).
    WriteFailed(String),
    /// F5 or a board dump without `--sav-out`.
    NoSavOut,
    /// An `Audio` message (only with [`PlayOptions::audio`]).
    Audio {
        first: u32,
        held: u16,
        data: Vec<u8>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Exit {
    Esc,
    Time,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlayReport {
    pub exit: Exit,
    /// `Input` frames sent, the final release included.
    pub inputs: u32,
    pub saves: u32,
    pub dumps_failed: u32,
    /// `Nak`s that did not answer the dump request, ignored.
    pub ignored_naks: u32,
    /// `Audio` messages received.
    pub audio: u32,
}

struct Job {
    rx: DumpRx,
    attempts: u32,
    deadline: Instant,
}

/// Runs the loop until Esc or `opts.duration`; `now` is the clock (the
/// real one in the tool, a stepped one in tests). On exit sends one last
/// `Input { buttons: 0 }` so the board does not keep buttons held.
pub fn play<T: Transport>(
    client: &mut Client<T>,
    kb: &mut dyn Keyboard,
    opts: &PlayOptions,
    now: &mut dyn FnMut() -> Instant,
    event: &mut dyn FnMut(PlayEvent),
) -> Result<PlayReport, Error> {
    let cfg = client.config();
    let start = now();
    let mut sched = InputSchedule::new(opts.refresh);
    let mut prev = KeySet::default();
    let mut job: Option<Job> = None;
    let mut report = PlayReport {
        exit: Exit::Time,
        inputs: 0,
        saves: 0,
        dumps_failed: 0,
        ignored_naks: 0,
        audio: 0,
    };
    if opts.audio {
        match client.request(&Message::AudioStart)? {
            crate::client::Reply::Ack => {}
            r => {
                return Err(Error::Io(io::Error::other(format!(
                    "AudioStart answered {r:?}, not Ack"
                ))));
            }
        }
    }
    loop {
        let t = now();
        if opts
            .duration
            .is_some_and(|d| t.saturating_duration_since(start) >= d)
        {
            report.exit = Exit::Time;
            break;
        }
        let keys = kb.held()?;
        if keys.contains(Key::Esc) {
            report.exit = Exit::Esc;
            break;
        }
        let buttons = keys.buttons();
        if sched.due(buttons, t) {
            client.send(&Message::Input { buttons })?;
            report.inputs += 1;
        }
        if keys.contains(Key::F5) && !prev.contains(Key::F5) {
            if opts.sav_out.is_none() {
                event(PlayEvent::NoSavOut);
            } else if job.is_none() {
                let seq = client.send(&Message::SaveDumpRequest)?;
                job = Some(Job {
                    rx: DumpRx::new(Some(seq)),
                    attempts: 1,
                    deadline: t + cfg.timeout,
                });
                event(PlayEvent::DumpStarted);
            }
        }
        prev = keys;

        let r = client.recv(opts.poll)?;
        let t = now();
        let mut step = DumpStep::Pending;
        match &r {
            Received::Frame { msg, seq } => match msg {
                Incoming::Stats {
                    frames,
                    cycles_mean,
                    cycles_max,
                    extra,
                } => event(PlayEvent::Stats {
                    frames: *frames,
                    cycles_mean: *cycles_mean,
                    cycles_max: *cycles_max,
                    extra: extra.clone(),
                }),
                Incoming::Audio { first, held, data } => {
                    if opts.audio {
                        report.audio += 1;
                        event(PlayEvent::Audio {
                            first: *first,
                            held: *held,
                            data: data.clone(),
                        });
                    }
                }
                Incoming::Nak(_) => match &mut job {
                    Some(j) if j.rx.seq() == Some(*seq) => step = j.rx.on_received(&r),
                    _ => report.ignored_naks += 1,
                },
                _ => {
                    if job.is_none() && matches!(msg, Incoming::SaveBegin { .. }) {
                        job = Some(Job {
                            rx: DumpRx::new(None),
                            attempts: 1,
                            deadline: t + cfg.timeout,
                        });
                        event(PlayEvent::DumpStarted);
                    }
                    if let Some(j) = &mut job {
                        let before = (j.rx.started(), j.rx.received());
                        step = j.rx.on_received(&r);
                        if (j.rx.started(), j.rx.received()) != before {
                            j.deadline = t + cfg.timeout;
                        }
                    }
                }
            },
            Received::Corrupt => {
                if let Some(j) = &mut job {
                    step = j.rx.on_received(&r);
                }
            }
            Received::Timeout => {}
        }
        if let (DumpStep::Pending, Some(j)) = (&step, &job)
            && t >= j.deadline
        {
            step = DumpStep::Failed(Failure::Timeout);
        }
        match step {
            DumpStep::Pending => {}
            DumpStep::Done(data) => {
                job = None;
                let (data, _) = crate::rtc::from_dump(data, opts.clock.map(|f| f()));
                match &opts.sav_out {
                    None => event(PlayEvent::NoSavOut),
                    Some(path) => match write_atomic(path, &data) {
                        Ok(()) => {
                            report.saves += 1;
                            event(PlayEvent::Saved {
                                path: path.clone(),
                                len: data.len(),
                            });
                        }
                        Err(e) => event(PlayEvent::WriteFailed(format!("{}: {e}", path.display()))),
                    },
                }
            }
            DumpStep::Failed(f) => {
                let attempts = job.as_ref().map_or(0, |j| j.attempts);
                if attempts > cfg.retries {
                    job = None;
                    report.dumps_failed += 1;
                    event(PlayEvent::DumpFailed(f));
                } else {
                    event(PlayEvent::DumpRetry(f));
                    let seq = client.send(&Message::SaveDumpRequest)?;
                    job = Some(Job {
                        rx: DumpRx::new(Some(seq)),
                        attempts: attempts + 1,
                        deadline: t + cfg.timeout,
                    });
                }
            }
        }
    }
    if opts.audio {
        // Not waited for: the board also stops 1.5 s after the last frame.
        client.send(&Message::AudioStop)?;
    }
    client.send(&Message::Input { buttons: 0 })?;
    report.inputs += 1;
    Ok(report)
}
