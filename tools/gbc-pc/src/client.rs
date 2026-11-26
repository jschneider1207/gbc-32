//! The protocol client: stop-and-wait requests over a [`Transport`] and
//! the ROM-load flow (`gbc-link` crate docs; plan M2 Loader).
//!
//! One request is outstanding at a time. Each new request takes the next
//! `seq` (wrapping); a `Nak` (whatever its `seq`, since a frame the board
//! could not decode has no trustworthy one) or a timeout resends the same
//! frame with the same `seq`, up to [`Config::retries`] times. Replies
//! with another `seq` are late answers to an earlier attempt (a resend
//! the board answered twice) and are ignored, as are frames that fail
//! COBS or CRC: their answer is lost and the timeout resends.

use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use gbc_link::{FrameDecoder, MAX_CHUNK, MAX_FRAME, Message, NakCode, PROTOCOL_VERSION};

use crate::fnv;
use crate::transport::Transport;

/// Size of the ROM image header in front of the data (ADR 0037).
pub const IMAGE_HEADER_LEN: u32 = 256;

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Resends allowed per request after the first attempt.
    pub retries: u32,
    /// How long to wait for an answer before resending.
    pub timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            retries: 5,
            timeout: Duration::from_millis(2000),
        }
    }
}

/// Why the last attempt of a request failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Timeout,
    Nak(NakCode),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("no answer (timeout)"),
            Self::Nak(code) => write!(f, "Nak {}", nak_name(*code)),
        }
    }
}

/// A `Nak` code by name (`Other(n)` for an unassigned one).
pub fn nak_name(code: NakCode) -> String {
    match code {
        NakCode::Other(c) => format!("Other({c})"),
        c => format!("{c:?}"),
    }
}

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The request failed `attempts` times in a row; `last` says how.
    GaveUp {
        request: &'static str,
        attempts: u32,
        last: Failure,
    },
    /// The board answered the request's `seq` with the wrong reply type.
    Unexpected {
        request: &'static str,
        reply: &'static str,
    },
    /// The board speaks another protocol version.
    Version {
        board: u16,
        ours: u16,
    },
    /// `HelloReply.max_chunk` is 0.
    NoChunkSize,
    RomEmpty,
    /// The ROM does not fit the board's flash after the image header.
    RomTooLarge {
        len: usize,
        max: u64,
    },
    /// The image the board read back does not hash to the ROM's hash.
    HashMismatch {
        sent: u64,
        board: u64,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::GaveUp {
                request,
                attempts,
                last,
            } => write!(
                f,
                "{request}: gave up after {attempts} attempts (last: {last})"
            ),
            Self::Unexpected { request, reply } => {
                write!(f, "{request}: unexpected reply {reply}")
            }
            Self::Version { board, ours } => write!(
                f,
                "protocol version mismatch: board speaks {board}, gbc-pc speaks {ours}"
            ),
            Self::NoChunkSize => f.write_str("board reported max_chunk = 0"),
            Self::RomEmpty => f.write_str("ROM is empty"),
            Self::RomTooLarge { len, max } => write!(
                f,
                "ROM is {len} bytes; the board's flash holds at most {max} (flash_len - {IMAGE_HEADER_LEN})"
            ),
            Self::HashMismatch { sent, board } => write!(
                f,
                "hash mismatch: ROM {sent:#018x}, board read back {board:#018x}"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// A board -> PC answer, owned (the decoder's borrow ends here).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Ack,
    HelloReply {
        protocol: u16,
        max_chunk: u16,
        flash_len: u32,
    },
    AlreadyLoaded,
    RomDone {
        hash: u64,
    },
}

impl Reply {
    fn name(&self) -> &'static str {
        match self {
            Self::Ack => "Ack",
            Self::HelloReply { .. } => "HelloReply",
            Self::AlreadyLoaded => "AlreadyLoaded",
            Self::RomDone { .. } => "RomDone",
        }
    }
}

/// What the board said in `HelloReply`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoardInfo {
    pub protocol: u16,
    pub max_chunk: u16,
    pub flash_len: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadOutcome {
    /// The board already held this ROM (same length and hash).
    AlreadyLoaded,
    /// Programmed and verified; the board read back `hash`.
    Loaded { hash: u64 },
}

/// Link health counters, for the report.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Frames resent after a `Nak` or timeout.
    pub resends: u32,
    pub naks: u32,
    pub timeouts: u32,
    /// Replies for an older `seq`, ignored.
    pub stale: u32,
    /// Received frames that failed COBS/CRC/length checks, ignored.
    pub corrupt: u32,
    /// Well-formed frames that are not a reply (e.g. `Stats`), ignored.
    pub other: u32,
}

enum Wait {
    Reply(Reply),
    Nak(NakCode),
    Timeout,
}

pub struct Client<T: Transport> {
    t: T,
    cfg: Config,
    next_seq: u8,
    dec: FrameDecoder,
    frame: [u8; MAX_FRAME],
    rx: [u8; 4096],
    rx_pos: usize,
    rx_len: usize,
    pub stats: Stats,
}

impl<T: Transport> Client<T> {
    pub fn new(t: T, cfg: Config) -> Self {
        Self {
            t,
            cfg,
            next_seq: 0,
            dec: FrameDecoder::new(),
            frame: [0; MAX_FRAME],
            rx: [0; 4096],
            rx_pos: 0,
            rx_len: 0,
            stats: Stats::default(),
        }
    }

    pub fn transport(&self) -> &T {
        &self.t
    }

    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.t
    }

    /// Sends `msg` as a new request and returns the board's answer for its
    /// `seq` (any type; the caller checks it is the right one).
    pub fn request(&mut self, msg: &Message<'_>) -> Result<Reply, Error> {
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        let n = gbc_link::encode(msg, seq, &mut self.frame).map_err(|e| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot encode {}: {e:?}", msg_name(msg)),
            ))
        })?;
        let mut attempts = 0;
        loop {
            if attempts > 0 {
                self.stats.resends += 1;
            }
            attempts += 1;
            self.t.write_all(&self.frame[..n])?;
            let last = match self.wait(seq)? {
                Wait::Reply(r) => return Ok(r),
                Wait::Nak(code) => {
                    self.stats.naks += 1;
                    Failure::Nak(code)
                }
                Wait::Timeout => {
                    self.stats.timeouts += 1;
                    Failure::Timeout
                }
            };
            if attempts > self.cfg.retries {
                return Err(Error::GaveUp {
                    request: msg_name(msg),
                    attempts,
                    last,
                });
            }
        }
    }

    /// Waits up to the timeout for the answer to `seq`.
    fn wait(&mut self, seq: u8) -> Result<Wait, Error> {
        let deadline = Instant::now() + self.cfg.timeout;
        loop {
            while self.rx_pos < self.rx_len {
                let (used, frame) = self.dec.feed_slice(&self.rx[self.rx_pos..self.rx_len]);
                self.rx_pos += used;
                let Some(frame) = frame else { continue };
                let (s, m) = match frame {
                    Ok(f) => f,
                    Err(_) => {
                        self.stats.corrupt += 1;
                        continue;
                    }
                };
                let reply = match m {
                    // Any seq: see the module docs.
                    Message::Nak { code } => return Ok(Wait::Nak(code)),
                    Message::Ack => Reply::Ack,
                    Message::HelloReply {
                        protocol,
                        max_chunk,
                        flash_len,
                    } => Reply::HelloReply {
                        protocol,
                        max_chunk,
                        flash_len,
                    },
                    Message::AlreadyLoaded => Reply::AlreadyLoaded,
                    Message::RomDone { hash } => Reply::RomDone { hash },
                    _ => {
                        self.stats.other += 1;
                        continue;
                    }
                };
                if s != seq {
                    self.stats.stale += 1;
                    continue;
                }
                return Ok(Wait::Reply(reply));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Wait::Timeout);
            }
            let n = self.t.read_timeout(&mut self.rx, left)?;
            if n == 0 {
                return Ok(Wait::Timeout);
            }
            self.rx_pos = 0;
            self.rx_len = n;
        }
    }

    /// Opens a session: a lone 0x00 to flush any half-received frame at
    /// the board, then `Hello`. Checks the board's protocol version.
    pub fn hello(&mut self) -> Result<BoardInfo, Error> {
        self.t.write_all(&[0])?;
        let request = "Hello";
        match self.request(&Message::Hello {
            protocol: PROTOCOL_VERSION,
        })? {
            Reply::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            } => {
                if protocol != PROTOCOL_VERSION {
                    return Err(Error::Version {
                        board: protocol,
                        ours: PROTOCOL_VERSION,
                    });
                }
                Ok(BoardInfo {
                    protocol,
                    max_chunk,
                    flash_len,
                })
            }
            r => Err(Error::Unexpected {
                request,
                reply: r.name(),
            }),
        }
    }

    /// Loads `rom` into the board's flash: `Hello`, `RomBegin` (skipped
    /// when the board answers `AlreadyLoaded`), the chunks in order, then
    /// `RomEnd`, whose `RomDone` hash must equal the ROM's. `progress` is
    /// called with the bytes acknowledged so far after each chunk.
    pub fn load(
        &mut self,
        rom: &[u8],
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<LoadOutcome, Error> {
        let info = self.hello()?;
        if rom.is_empty() {
            return Err(Error::RomEmpty);
        }
        let max = u64::from(info.flash_len).saturating_sub(u64::from(IMAGE_HEADER_LEN));
        let len = match u32::try_from(rom.len()) {
            Ok(len) if u64::from(len) <= max => len,
            _ => {
                return Err(Error::RomTooLarge {
                    len: rom.len(),
                    max,
                });
            }
        };
        let chunk = usize::from(info.max_chunk).min(MAX_CHUNK);
        if chunk == 0 {
            return Err(Error::NoChunkSize);
        }
        let hash = fnv::hash(rom);

        match self.request(&Message::RomBegin { len, hash })? {
            Reply::AlreadyLoaded => return Ok(LoadOutcome::AlreadyLoaded),
            Reply::Ack => {}
            r => return Err(unexpected("RomBegin", r)),
        }
        let mut offset = 0usize;
        for data in rom.chunks(chunk) {
            // `offset < len <= u32::MAX`, checked above.
            let msg = Message::RomChunk {
                offset: offset as u32,
                data,
            };
            match self.request(&msg)? {
                Reply::Ack => {}
                r => return Err(unexpected("RomChunk", r)),
            }
            offset += data.len();
            progress(offset, rom.len());
        }
        match self.request(&Message::RomEnd)? {
            Reply::RomDone { hash: board } => {
                if board != hash {
                    return Err(Error::HashMismatch { sent: hash, board });
                }
                Ok(LoadOutcome::Loaded { hash: board })
            }
            r => Err(unexpected("RomEnd", r)),
        }
    }
}

fn unexpected(request: &'static str, r: Reply) -> Error {
    Error::Unexpected {
        request,
        reply: r.name(),
    }
}

fn msg_name(msg: &Message<'_>) -> &'static str {
    match msg {
        Message::Hello { .. } => "Hello",
        Message::RomBegin { .. } => "RomBegin",
        Message::RomChunk { .. } => "RomChunk",
        Message::RomEnd => "RomEnd",
        Message::SaveBegin { .. } => "SaveBegin",
        Message::SaveChunk { .. } => "SaveChunk",
        Message::SaveDumpRequest => "SaveDumpRequest",
        Message::Input { .. } => "Input",
        Message::Ack => "Ack",
        Message::Nak { .. } => "Nak",
        Message::HelloReply { .. } => "HelloReply",
        Message::AlreadyLoaded => "AlreadyLoaded",
        Message::RomDone { .. } => "RomDone",
        Message::Stats { .. } => "Stats",
    }
}
