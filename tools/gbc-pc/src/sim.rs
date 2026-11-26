//! A simulated board for tests: [`SimBoard`] follows the board side of
//! the protocol (the loader over a fake flash, plus the M5 save load,
//! save dump and `Input`, `docs/reference/link-protocol.md`), and
//! [`Faulty`] sits in front of it and
//! injects link faults. Both are [`Transport`]s whose reads never block:
//! a read with nothing queued returns 0 at once, which the client treats
//! as the timeout passing.

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use gbc_link::{FrameDecoder, MAX_FRAME, Message, NakCode, PROTOCOL_VERSION};

use crate::client::IMAGE_HEADER_LEN;
use crate::fnv;
use crate::transport::Transport;

/// Work the board did, to check that duplicates are not redone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub hellos: u32,
    pub begins: u32,
    pub chunks: u32,
    pub ends: u32,
    /// Requests answered again from the stored reply.
    pub duplicates: u32,
    /// Frames that failed to decode (answered `Nak`).
    pub bad_frames: u32,
    pub save_begins: u32,
    pub save_chunks: u32,
    /// `SaveDumpRequest`s answered (with a dump or a `Nak`).
    pub dumps: u32,
}

/// A fault in a dump the board sends (see [`SimBoard::dump_faults`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpFault {
    /// Only the first `n` chunks are sent.
    Truncate(usize),
    /// One byte of chunk frame `i` is changed (fails its CRC).
    CorruptChunk(usize),
    /// `SaveBegin.hash` is off by one.
    BadHash,
    /// A `Stats` frame goes out before every chunk (not a fault: the PC
    /// must skip it).
    StatsBetween,
}

/// [`SimBoard::dump_faults`] index for "every dump".
pub const EVERY_DUMP: u32 = u32::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Loading { len: u32, hash: u64, next: u32 },
}

pub struct SimBoard {
    /// The whole NOR: the image header at 0, the ROM from
    /// [`IMAGE_HEADER_LEN`] (ADR 0037).
    pub flash: Vec<u8>,
    pub protocol: u16,
    pub max_chunk: u16,
    /// The `(len, hash)` of a verified image (the header, in effect).
    pub stored: Option<(u32, u64)>,
    /// Flips bit 0 of this ROM offset as it is programmed (bad flash).
    pub corrupt_flash_at: Option<u32>,
    pub work: Work,
    /// The save the running game has (what a dump sends); a verified
    /// save load replaces it.
    pub save: Option<Vec<u8>>,
    /// The cartridge's save size: a `SaveBegin` of another length is
    /// refused `Nak OutOfRange` (as `gbc_firmware::save::check` would).
    /// `None` takes any non-zero length.
    pub save_len: Option<u32>,
    /// Flips bit 0 of this save offset as it arrives (the board's hash
    /// check then refuses the save).
    pub corrupt_save_at: Option<u32>,
    /// Chunk data size of dumps.
    pub dump_chunk: usize,
    /// Faults by dump number (0 = the first `SaveDumpRequest`, or
    /// [`EVERY_DUMP`]).
    pub dump_faults: Vec<(u32, DumpFault)>,
    /// Every `Input` bitmap received, in order.
    pub inputs: Vec<u8>,
    /// The save load in progress: length, hash, bytes so far.
    save_load: Option<(u32, u64, Vec<u8>)>,
    state: State,
    raw: Vec<u8>,
    /// The last request frame answered and its reply, both as on the wire.
    last: Option<(Vec<u8>, Vec<u8>)>,
    out: VecDeque<u8>,
}

impl SimBoard {
    /// A board with an erased `flash_len`-byte flash.
    pub fn new(flash_len: usize) -> Self {
        Self {
            flash: vec![0xFF; flash_len],
            protocol: PROTOCOL_VERSION,
            max_chunk: gbc_link::MAX_CHUNK as u16,
            stored: None,
            corrupt_flash_at: None,
            work: Work::default(),
            save: None,
            save_len: None,
            corrupt_save_at: None,
            dump_chunk: gbc_link::MAX_CHUNK,
            dump_faults: Vec::new(),
            inputs: Vec::new(),
            save_load: None,
            state: State::Idle,
            raw: Vec::new(),
            last: None,
            out: VecDeque::new(),
        }
    }

    /// The programmed ROM bytes (`len` of them, after the header).
    pub fn rom(&self, len: usize) -> &[u8] {
        let start = IMAGE_HEADER_LEN as usize;
        &self.flash[start..start + len]
    }

    /// Queues a `Stats` frame, as the running game sends them.
    pub fn push_stats(&mut self, frames: u32, cycles_mean: u32, cycles_max: u32) {
        // Text fields in `extra`, as the player image sends them.
        let extra = format!("image=sim frames={frames}");
        let m = Message::Stats {
            frames,
            cycles_mean,
            cycles_max,
            extra: extra.as_bytes(),
        };
        self.out.extend(encode(&m, 0));
    }

    /// Everything the board has sent and the client has not read.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.out.drain(..).collect()
    }

    /// Feeds received bytes; answers each complete frame.
    pub fn receive(&mut self, data: &[u8]) {
        for &b in data {
            if b != 0 {
                self.raw.push(b);
            } else if !self.raw.is_empty() {
                let mut frame = std::mem::take(&mut self.raw);
                frame.push(0);
                self.handle(&frame);
            }
        }
    }

    fn handle(&mut self, frame: &[u8]) {
        let mut dec = FrameDecoder::new();
        let mut decoded = None;
        for &b in frame {
            if let Some(d) = dec.feed(b) {
                decoded = Some(d.map(|(s, m)| (s, owned(&m))));
            }
        }
        let (seq, msg) = match decoded {
            Some(Ok(x)) => x,
            Some(Err(e)) => {
                self.work.bad_frames += 1;
                let reply = encode(&Message::Nak { code: e.nak_code() }, e.seq().unwrap_or(0));
                self.out.extend(reply);
                return;
            }
            None => return,
        };
        // `Input` is not answered (and not a request to repeat); a dump
        // is a stream, every request a fresh snapshot.
        match msg {
            Req::Input { buttons } => {
                self.inputs.push(buttons);
                return;
            }
            Req::SaveDumpRequest => {
                self.dump(seq);
                return;
            }
            _ => {}
        }
        // A repeat of the last answered frame is a resend: answer again,
        // don't redo the work. `Hello` opens a session, so it is always
        // handled afresh and forgets the last frame (a new session's
        // seqs restart and may equal the old session's last one).
        if matches!(msg, Req::Hello) {
            self.last = None;
        } else if let Some((req, reply)) = &self.last
            && req == frame
        {
            self.work.duplicates += 1;
            self.out.extend(reply.iter().copied());
            return;
        }
        let reply = self.answer(msg);
        let wire = encode(&reply.as_message(), seq);
        self.out.extend(wire.iter().copied());
        self.last = Some((frame.to_vec(), wire));
    }

    fn answer(&mut self, msg: Req) -> Ans {
        let nak = |code| Ans::Nak(code);
        let cap = (self.flash.len() as u64).saturating_sub(u64::from(IMAGE_HEADER_LEN));
        match msg {
            Req::Hello => {
                self.work.hellos += 1;
                self.state = State::Idle;
                Ans::HelloReply {
                    protocol: self.protocol,
                    max_chunk: self.max_chunk,
                    flash_len: self.flash.len() as u32,
                }
            }
            Req::RomBegin { len, hash } => {
                if len == 0 || u64::from(len) > cap {
                    return nak(NakCode::OutOfRange);
                }
                if self.stored == Some((len, hash)) {
                    self.state = State::Idle;
                    return Ans::AlreadyLoaded;
                }
                self.work.begins += 1;
                // Erase: the header first, so an interrupted load reads
                // as "no image" (ADR 0037).
                self.stored = None;
                self.flash.fill(0xFF);
                self.state = State::Loading { len, hash, next: 0 };
                Ans::Ack
            }
            Req::RomChunk { offset, data } => {
                let State::Loading { len, hash, next } = self.state else {
                    return nak(NakCode::UnexpectedType);
                };
                let end = u64::from(offset) + data.len() as u64;
                if offset != next
                    || end > u64::from(len)
                    || data.len() > usize::from(self.max_chunk)
                {
                    return nak(NakCode::OutOfRange);
                }
                self.work.chunks += 1;
                let base = IMAGE_HEADER_LEN as usize + offset as usize;
                self.flash[base..base + data.len()].copy_from_slice(&data);
                if let Some(bad) = self.corrupt_flash_at
                    && (offset..end as u32).contains(&bad)
                {
                    self.flash[IMAGE_HEADER_LEN as usize + bad as usize] ^= 1;
                }
                self.state = State::Loading {
                    len,
                    hash,
                    next: end as u32,
                };
                Ans::Ack
            }
            Req::RomEnd => {
                let State::Loading { len, hash, next } = self.state else {
                    return nak(NakCode::UnexpectedType);
                };
                if next != len {
                    return nak(NakCode::OutOfRange);
                }
                self.work.ends += 1;
                let read_back = fnv::hash(self.rom(len as usize));
                if read_back == hash {
                    self.stored = Some((len, hash));
                }
                self.state = State::Idle;
                Ans::RomDone { hash: read_back }
            }
            Req::SaveBegin { len, hash } => {
                if len == 0 || self.save_len.is_some_and(|n| n != len) {
                    return nak(NakCode::OutOfRange);
                }
                self.work.save_begins += 1;
                self.save_load = Some((len, hash, Vec::with_capacity(len as usize)));
                Ans::Ack
            }
            Req::SaveChunk { offset, data } => {
                let Some((len, hash, buf)) = &mut self.save_load else {
                    return nak(NakCode::UnexpectedType);
                };
                let end = u64::from(offset) + data.len() as u64;
                if offset as usize != buf.len()
                    || data.is_empty()
                    || end > u64::from(*len)
                    || data.len() > usize::from(self.max_chunk)
                {
                    return nak(NakCode::OutOfRange);
                }
                self.work.save_chunks += 1;
                buf.extend_from_slice(&data);
                if let Some(bad) = self.corrupt_save_at
                    && (offset..end as u32).contains(&bad)
                {
                    buf[bad as usize] ^= 1;
                }
                if buf.len() < *len as usize {
                    return Ans::Ack;
                }
                // The whole save is here: verify, then apply or refuse
                // (never half-applied).
                let hash = *hash;
                let (_, _, buf) = self.save_load.take().expect("checked above");
                if fnv::hash(&buf) != hash {
                    return nak(NakCode::HashMismatch);
                }
                self.save = Some(buf);
                Ans::Ack
            }
            Req::Input { .. } | Req::SaveDumpRequest | Req::Other => nak(NakCode::UnexpectedType),
        }
    }

    /// Answers `SaveDumpRequest` `seq`: `SaveBegin` then the chunks, all
    /// with `seq`; `Nak UnexpectedType` when there is no save.
    fn dump(&mut self, seq: u8) {
        let n = self.work.dumps;
        self.work.dumps += 1;
        let faults: Vec<DumpFault> = self
            .dump_faults
            .iter()
            .filter(|&&(i, _)| i == n || i == EVERY_DUMP)
            .map(|&(_, f)| f)
            .collect();
        let Some(save) = self.save.clone() else {
            let nak = Message::Nak {
                code: NakCode::UnexpectedType,
            };
            self.out.extend(encode(&nak, seq));
            return;
        };
        let mut hash = fnv::hash(&save);
        if faults.contains(&DumpFault::BadHash) {
            hash ^= 1;
        }
        let begin = Message::SaveBegin {
            len: save.len() as u32,
            hash,
        };
        self.out.extend(encode(&begin, seq));
        for (i, data) in save.chunks(self.dump_chunk).enumerate() {
            if faults.contains(&DumpFault::Truncate(i)) {
                break;
            }
            if faults.contains(&DumpFault::StatsBetween) {
                self.push_stats(i as u32, 1, 2);
            }
            let msg = Message::SaveChunk {
                offset: (i * self.dump_chunk) as u32,
                data,
            };
            let mut frame = encode(&msg, seq);
            if faults.contains(&DumpFault::CorruptChunk(i)) {
                flip(&mut frame);
            }
            self.out.extend(frame);
        }
    }
}

impl Transport for SimBoard {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        self.receive(data);
        Ok(())
    }

    fn read_timeout(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
        let n = buf.len().min(self.out.len());
        for (dst, src) in buf.iter_mut().zip(self.out.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }
}

/// A request, owned (out of the decoder's buffer).
enum Req {
    Hello,
    RomBegin { len: u32, hash: u64 },
    RomChunk { offset: u32, data: Vec<u8> },
    RomEnd,
    SaveBegin { len: u32, hash: u64 },
    SaveChunk { offset: u32, data: Vec<u8> },
    SaveDumpRequest,
    Input { buttons: u8 },
    Other,
}

fn owned(m: &Message<'_>) -> Req {
    match *m {
        Message::Hello { .. } => Req::Hello,
        Message::RomBegin { len, hash } => Req::RomBegin { len, hash },
        Message::RomChunk { offset, data } => Req::RomChunk {
            offset,
            data: data.to_vec(),
        },
        Message::RomEnd => Req::RomEnd,
        Message::SaveBegin { len, hash } => Req::SaveBegin { len, hash },
        Message::SaveChunk { offset, data } => Req::SaveChunk {
            offset,
            data: data.to_vec(),
        },
        Message::SaveDumpRequest => Req::SaveDumpRequest,
        Message::Input { buttons } => Req::Input { buttons },
        _ => Req::Other,
    }
}

enum Ans {
    Ack,
    Nak(NakCode),
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

impl Ans {
    fn as_message(&self) -> Message<'static> {
        match *self {
            Self::Ack => Message::Ack,
            Self::Nak(code) => Message::Nak { code },
            Self::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            } => Message::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            },
            Self::AlreadyLoaded => Message::AlreadyLoaded,
            Self::RomDone { hash } => Message::RomDone { hash },
        }
    }
}

/// `msg` as frame `seq`, as on the wire.
pub fn encode(msg: &Message<'_>, seq: u8) -> Vec<u8> {
    let mut buf = [0u8; MAX_FRAME];
    let n = gbc_link::encode(msg, seq, &mut buf).expect("v1 messages fit MAX_FRAME");
    buf[..n].to_vec()
}

/// A link fault, applied to one request frame (counted from 0 in the
/// order the client sends them, the leading 0x00 not counted) or its reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The board answers, the reply is lost.
    DropReply,
    /// One byte of the reply is changed (fails the CRC at the client).
    CorruptReply,
    /// The reply arrives only after the client's read has timed out once.
    DelayReply,
    /// One byte of the request is changed (the board answers `Nak`).
    CorruptRequest,
    /// The request never reaches the board; a `Nak Busy` comes back.
    NakRequest,
    /// This and every later request is answered `Nak` with this code.
    NakForever(NakCode),
    /// This and every later request is lost.
    SilenceForever,
}

/// A fault-injecting link in front of a [`SimBoard`].
pub struct Faulty {
    pub board: SimBoard,
    faults: Vec<(usize, Fault)>,
    /// Request frames seen so far.
    pub requests: usize,
    raw: Vec<u8>,
    pending: VecDeque<u8>,
    delayed: Vec<u8>,
}

impl Faulty {
    pub fn new(board: SimBoard, faults: &[(usize, Fault)]) -> Self {
        Self {
            board,
            faults: faults.to_vec(),
            requests: 0,
            raw: Vec::new(),
            pending: VecDeque::new(),
            delayed: Vec::new(),
        }
    }

    fn fault_for(&self, idx: usize) -> Option<Fault> {
        self.faults.iter().find_map(|&(at, f)| match f {
            Fault::NakForever(_) | Fault::SilenceForever if idx >= at => Some(f),
            _ if idx == at => Some(f),
            _ => None,
        })
    }

    fn request(&mut self, mut frame: Vec<u8>) {
        let idx = self.requests;
        self.requests += 1;
        match self.fault_for(idx) {
            Some(Fault::NakRequest) => self.pending.extend(encode(
                &Message::Nak {
                    code: NakCode::Busy,
                },
                0x77,
            )),
            Some(Fault::NakForever(code)) => {
                self.pending.extend(encode(&Message::Nak { code }, 0x77));
            }
            Some(Fault::SilenceForever) => {}
            Some(Fault::CorruptRequest) => {
                flip(&mut frame);
                self.board.receive(&frame);
                self.pending.extend(self.board.take_output());
            }
            fault => {
                self.board.receive(&frame);
                let mut reply = self.board.take_output();
                match fault {
                    Some(Fault::DropReply) => {}
                    Some(Fault::CorruptReply) => {
                        flip(&mut reply);
                        self.pending.extend(reply);
                    }
                    Some(Fault::DelayReply) => self.delayed.extend(reply),
                    _ => self.pending.extend(reply),
                }
            }
        }
    }
}

/// Changes a byte in the middle of a frame, never to or from 0x00 (the
/// delimiter), so the frame stays one frame but fails its checks.
fn flip(frame: &mut [u8]) {
    let i = frame.len() / 2;
    frame[i] = if frame[i] == 0x80 {
        0x40
    } else {
        frame[i] ^ 0x80
    };
}

impl Transport for Faulty {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for &b in data {
            if b != 0 {
                self.raw.push(b);
            } else if self.raw.is_empty() {
                // The leading flush byte: pass it on, not a request.
                self.board.receive(&[0]);
            } else {
                let mut frame = std::mem::take(&mut self.raw);
                frame.push(0);
                self.request(frame);
            }
        }
        Ok(())
    }

    fn read_timeout(&mut self, buf: &mut [u8], _timeout: Duration) -> io::Result<usize> {
        if self.pending.is_empty() {
            // Nothing in time; a delayed reply turns up after this.
            self.pending.extend(self.delayed.drain(..));
            return Ok(0);
        }
        let n = buf.len().min(self.pending.len());
        for (dst, src) in buf.iter_mut().zip(self.pending.drain(..n)) {
            *dst = src;
        }
        Ok(n)
    }
}
