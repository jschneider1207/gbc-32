//! Saves over the link (Stage 2 M5, decision D6): sending a `.sav` to the
//! board after its ROM, and receiving the board's save dump. The message
//! order is in `docs/reference/link-protocol.md` and the `gbc-link` crate
//! docs. The `.sav` itself (cartridge RAM plus an optional clock footer,
//! ADR 0023) is opaque here: the board checks its size and footer
//! (`gbc_firmware::save::check`) against the loaded cartridge.
//!
//! The hash is the ROM path's: FNV-1a 64 over the whole file
//! ([`crate::fnv`]).

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use gbc_link::{MAX_CHUNK, Message};

use crate::client::{Client, Error, Failure, Incoming, Received, Reply, unexpected};
use crate::fnv;
use crate::transport::Transport;

/// The largest save this tool sends or accepts: far above any cartridge
/// (MBC5 128 KiB RAM + a 48-byte footer), so a garbled `SaveBegin.len`
/// cannot make it allocate gigabytes.
pub const MAX_SAVE_LEN: usize = 1 << 20;

/// Why a save dump was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpError {
    /// A frame failed its checks mid-dump (a chunk may be lost).
    Corrupt,
    /// `SaveBegin.len` is 0 or over [`MAX_SAVE_LEN`].
    BadLength(u32),
    /// A chunk that does not continue the dump (lost or reordered), is
    /// empty, or runs past `SaveBegin.len`.
    Gap {
        expected: u32,
        offset: u32,
        len: u32,
    },
    /// The received bytes do not hash to `SaveBegin.hash`.
    HashMismatch { expected: u64, got: u64 },
}

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Corrupt => f.write_str("a corrupt frame mid-dump"),
            Self::BadLength(len) => write!(f, "dump length {len} not in 1..={MAX_SAVE_LEN}"),
            Self::Gap {
                expected,
                offset,
                len,
            } => write!(
                f,
                "dump chunk at {offset} (+{len}) where {expected} was expected"
            ),
            Self::HashMismatch { expected, got } => write!(
                f,
                "dump hash mismatch: SaveBegin {expected:#018x}, data {got:#018x}"
            ),
        }
    }
}

/// One step of [`DumpRx`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DumpStep {
    /// Not (yet) a complete dump; this frame was used or ignored.
    Pending,
    /// The whole save, its hash checked.
    Done(Vec<u8>),
    Failed(Failure),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RxState {
    AwaitBegin,
    Receiving { len: u32, hash: u64, data: Vec<u8> },
}

/// Receives one save dump: `SaveBegin { len, hash }` then `SaveChunk`s
/// in order (offset 0, then each the previous end), all with the
/// request's `seq`. Frames with another `seq` are stale (an earlier
/// attempt's) and ignored, as are `Stats` and replies. A `Nak` fails the
/// dump; callers that also send unanswered `Input` frames pass only the
/// `Nak`s that carry the dump's `seq` (see [`crate::play`]).
#[derive(Clone, Debug)]
pub struct DumpRx {
    seq: Option<u8>,
    state: RxState,
}

impl DumpRx {
    /// Expects a dump answering request `seq`; `None` takes a dump the
    /// board starts on its own (its button combo), with any `seq`.
    pub fn new(seq: Option<u8>) -> Self {
        Self {
            seq,
            state: RxState::AwaitBegin,
        }
    }

    /// The dump's `seq`, once known.
    pub fn seq(&self) -> Option<u8> {
        self.seq
    }

    /// True once `SaveBegin` has arrived.
    pub fn started(&self) -> bool {
        matches!(self.state, RxState::Receiving { .. })
    }

    /// Bytes received so far.
    pub fn received(&self) -> usize {
        match &self.state {
            RxState::AwaitBegin => 0,
            RxState::Receiving { data, .. } => data.len(),
        }
    }

    /// Feeds one [`Client::recv`] result (not `Timeout`: the caller owns
    /// the deadline).
    pub fn on_received(&mut self, r: &Received) -> DumpStep {
        match r {
            Received::Frame { seq, msg } => self.on_frame(*seq, msg),
            Received::Corrupt if self.started() => {
                DumpStep::Failed(Failure::Dump(DumpError::Corrupt))
            }
            Received::Corrupt | Received::Timeout => DumpStep::Pending,
        }
    }

    pub fn on_frame(&mut self, seq: u8, msg: &Incoming) -> DumpStep {
        if let Incoming::Nak(code) = msg {
            return DumpStep::Failed(Failure::Nak(*code));
        }
        if self.seq.is_some_and(|s| s != seq) {
            return DumpStep::Pending;
        }
        match (msg, &mut self.state) {
            // A second SaveBegin for the same seq restarts the dump (the
            // board answered a resend of the request again).
            (&Incoming::SaveBegin { len, hash }, _) => {
                if len == 0 || len as usize > MAX_SAVE_LEN {
                    return DumpStep::Failed(Failure::Dump(DumpError::BadLength(len)));
                }
                self.seq = Some(seq);
                self.state = RxState::Receiving {
                    len,
                    hash,
                    data: Vec::with_capacity(len as usize),
                };
                DumpStep::Pending
            }
            (
                Incoming::SaveChunk { offset, data },
                RxState::Receiving {
                    len,
                    hash,
                    data: buf,
                },
            ) => {
                let expected = buf.len() as u32;
                let end = u64::from(*offset) + data.len() as u64;
                if *offset != expected || data.is_empty() || end > u64::from(*len) {
                    return DumpStep::Failed(Failure::Dump(DumpError::Gap {
                        expected,
                        offset: *offset,
                        len: data.len() as u32,
                    }));
                }
                buf.extend_from_slice(data);
                if buf.len() < *len as usize {
                    return DumpStep::Pending;
                }
                let got = fnv::hash(buf);
                if got != *hash {
                    return DumpStep::Failed(Failure::Dump(DumpError::HashMismatch {
                        expected: *hash,
                        got,
                    }));
                }
                let data = std::mem::take(buf);
                self.state = RxState::AwaitBegin;
                DumpStep::Done(data)
            }
            _ => DumpStep::Pending,
        }
    }
}

impl<T: Transport> Client<T> {
    /// Sends `sav` to the board: `SaveBegin { len, hash }`, then the
    /// chunks in order, each answered `Ack`. The last chunk's answer is the
    /// board's verdict: `Ack` = hash verified and the save applied; a
    /// `Nak` (resent like any other, then [`Error::GaveUp`]) = refused,
    /// nothing applied. Runs `Hello` first if this client has not.
    /// `progress` gets the bytes acknowledged so far after each chunk.
    pub fn send_save(
        &mut self,
        sav: &[u8],
        progress: &mut dyn FnMut(usize, usize),
    ) -> Result<(), Error> {
        if sav.is_empty() {
            return Err(Error::SaveEmpty);
        }
        if sav.len() > MAX_SAVE_LEN {
            return Err(Error::SaveTooLarge { len: sav.len() });
        }
        let info = match self.info {
            Some(i) => i,
            None => self.hello()?,
        };
        let chunk = usize::from(info.max_chunk).min(MAX_CHUNK);
        if chunk == 0 {
            return Err(Error::NoChunkSize);
        }
        let begin = Message::SaveBegin {
            len: sav.len() as u32,
            hash: fnv::hash(sav),
        };
        match self.request(&begin)? {
            Reply::Ack => {}
            r => return Err(unexpected("SaveBegin", r)),
        }
        let mut offset = 0usize;
        for data in sav.chunks(chunk) {
            let msg = Message::SaveChunk {
                offset: offset as u32,
                data,
            };
            match self.request(&msg)? {
                Reply::Ack => {}
                r => return Err(unexpected("SaveChunk", r)),
            }
            offset += data.len();
            progress(offset, sav.len());
        }
        Ok(())
    }

    /// Asks for the board's save (`SaveDumpRequest`) and returns it once
    /// complete and its hash checked. Each attempt is a new request (new
    /// `seq`, so the board takes a fresh snapshot); an attempt fails on a
    /// `Nak`, a broken dump ([`DumpError`]) or no frame of the dump for
    /// [`crate::Config::timeout`], and is retried up to
    /// [`crate::Config::retries`] times. Non-dump frames (e.g. `Stats`)
    /// are counted in [`crate::Stats::other`] and ignored.
    pub fn dump_save(&mut self) -> Result<Vec<u8>, Error> {
        let cfg = self.config();
        let mut attempts = 0;
        loop {
            if attempts > 0 {
                self.stats.resends += 1;
            }
            attempts += 1;
            let seq = self.send(&Message::SaveDumpRequest)?;
            let mut rx = DumpRx::new(Some(seq));
            let last = loop {
                let r = self.recv(cfg.timeout)?;
                if r == Received::Timeout {
                    self.stats.timeouts += 1;
                    break Failure::Timeout;
                }
                if let Received::Frame { msg, .. } = &r
                    && matches!(msg, Incoming::Stats { .. } | Incoming::Other)
                {
                    self.stats.other += 1;
                }
                match rx.on_received(&r) {
                    DumpStep::Pending => {}
                    DumpStep::Done(data) => return Ok(data),
                    DumpStep::Failed(f) => {
                        if let Failure::Nak(_) = f {
                            self.stats.naks += 1;
                        }
                        break f;
                    }
                }
            };
            if attempts > cfg.retries {
                return Err(Error::GaveUp {
                    request: "SaveDumpRequest",
                    attempts,
                    last,
                });
            }
        }
    }
}

/// Writes `data` to `path` all or nothing: into `<path>.part` in the same
/// directory, flushed to disk, then renamed over `path` (replacing it).
/// On any error the temporary file is removed and `path` is untouched.
pub fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = part_path(path);
    let result = (|| {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// The temporary name [`write_atomic`] uses for `path`.
pub fn part_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".part");
    path.with_file_name(name)
}

/// `dump`: the board's save into `path`, only when complete and verified
/// (never a partial file). Returns its length.
pub fn dump_to_file<T: Transport>(client: &mut Client<T>, path: &Path) -> Result<usize, Error> {
    let data = client.dump_save()?;
    write_atomic(path, &data)?;
    Ok(data.len())
}
