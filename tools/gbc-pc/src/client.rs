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
    /// A save dump arrived broken ([`crate::save::DumpError`]).
    Dump(crate::save::DumpError),
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timeout => f.write_str("no answer (timeout)"),
            Self::Nak(code) => write!(f, "Nak {}", nak_name(*code)),
            Self::Dump(e) => write!(f, "{e}"),
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
    SaveEmpty,
    /// Over [`crate::save::MAX_SAVE_LEN`].
    SaveTooLarge {
        len: usize,
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
            Self::SaveEmpty => f.write_str("save is empty"),
            Self::SaveTooLarge { len } => write!(
                f,
                "save is {len} bytes, over the {} this tool accepts",
                crate::save::MAX_SAVE_LEN
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
    pub fn name(&self) -> &'static str {
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

/// A board -> PC frame, owned. PC -> board types (other than the save
/// frames, which go both ways) are `Other`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Reply(Reply),
    Nak(NakCode),
    SaveBegin {
        len: u32,
        hash: u64,
    },
    SaveChunk {
        offset: u32,
        data: Vec<u8>,
    },
    Stats {
        frames: u32,
        cycles_mean: u32,
        cycles_max: u32,
        /// `Stats.extra` as text (the player's `key=value` fields,
        /// docs/reference/player.md; lossy UTF-8, empty from a v1 board).
        extra: String,
    },
    Other,
}

impl Incoming {
    fn from_message(m: &Message<'_>) -> Self {
        match *m {
            Message::Nak { code } => Self::Nak(code),
            Message::Ack => Self::Reply(Reply::Ack),
            Message::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            } => Self::Reply(Reply::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            }),
            Message::AlreadyLoaded => Self::Reply(Reply::AlreadyLoaded),
            Message::RomDone { hash } => Self::Reply(Reply::RomDone { hash }),
            Message::SaveBegin { len, hash } => Self::SaveBegin { len, hash },
            Message::SaveChunk { offset, data } => Self::SaveChunk {
                offset,
                data: data.to_vec(),
            },
            Message::Stats {
                frames,
                cycles_mean,
                cycles_max,
                extra,
            } => Self::Stats {
                frames,
                cycles_mean,
                cycles_max,
                extra: String::from_utf8_lossy(extra).into_owned(),
            },
            _ => Self::Other,
        }
    }
}

/// What [`Client::recv`] got.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Received {
    Frame {
        seq: u8,
        msg: Incoming,
    },
    /// A frame that failed COBS, CRC or length checks (counted in
    /// [`Stats::corrupt`]).
    Corrupt,
    /// Nothing within the timeout.
    Timeout,
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
    /// The last `HelloReply`, once [`Client::hello`] has run.
    pub info: Option<BoardInfo>,
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
            info: None,
        }
    }

    pub fn config(&self) -> Config {
        self.cfg
    }

    /// Sends `msg` once with the next `seq`, waiting for nothing (for
    /// `Input`, which the board does not answer, and for requests whose
    /// answer the caller collects with [`Client::recv`]). Returns the seq.
    pub fn send(&mut self, msg: &Message<'_>) -> Result<u8, Error> {
        let seq = self.next_seq;
        self.next_seq = seq.wrapping_add(1);
        let n = self.encode(msg, seq)?;
        self.t.write_all(&self.frame[..n])?;
        Ok(seq)
    }

    fn encode(&mut self, msg: &Message<'_>, seq: u8) -> Result<usize, Error> {
        gbc_link::encode(msg, seq, &mut self.frame).map_err(|e| {
            Error::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot encode {}: {e:?}", msg_name(msg)),
            ))
        })
    }

    /// The next frame from the board, waiting up to `timeout` for bytes
    /// (bytes already received are decoded first, even with a zero
    /// timeout).
    pub fn recv(&mut self, timeout: Duration) -> Result<Received, Error> {
        let deadline = Instant::now() + timeout;
        loop {
            while self.rx_pos < self.rx_len {
                let (used, frame) = self.dec.feed_slice(&self.rx[self.rx_pos..self.rx_len]);
                self.rx_pos += used;
                match frame {
                    None => {}
                    Some(Ok((seq, m))) => {
                        let msg = Incoming::from_message(&m);
                        return Ok(Received::Frame { seq, msg });
                    }
                    Some(Err(_)) => {
                        self.stats.corrupt += 1;
                        return Ok(Received::Corrupt);
                    }
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(Received::Timeout);
            }
            let n = self.t.read_timeout(&mut self.rx, left)?;
            if n == 0 {
                return Ok(Received::Timeout);
            }
            self.rx_pos = 0;
            self.rx_len = n;
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
        let n = self.encode(msg, seq)?;
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
            let left = deadline.saturating_duration_since(Instant::now());
            match self.recv(left)? {
                Received::Timeout => return Ok(Wait::Timeout),
                Received::Corrupt => {}
                Received::Frame { seq: s, msg } => match msg {
                    // Any seq: see the module docs.
                    Incoming::Nak(code) => return Ok(Wait::Nak(code)),
                    Incoming::Reply(r) if s == seq => return Ok(Wait::Reply(r)),
                    Incoming::Reply(_) => self.stats.stale += 1,
                    _ => self.stats.other += 1,
                },
            }
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
                let info = BoardInfo {
                    protocol,
                    max_chunk,
                    flash_len,
                };
                self.info = Some(info);
                Ok(info)
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

pub(crate) fn unexpected(request: &'static str, r: Reply) -> Error {
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
