//! A simulated board for tests. [`SimBoard`] answers with the board's
//! real link servers (`gbc_link_board`, ADR 0043) over in-memory
//! hardware: the `loader` image's [`Loader`] over a [`MemNor`], and the
//! `player` image's [`Session`] over a [`SimGame`]. On the board those are
//! two images (`bin/loader`, `bin/player`), so a sim runs one at a time:
//! [`SimBoard::loader`] or [`SimBoard::player`], and
//! [`SimBoard::boot_player`] is the reboot from the first into the
//! second (the player then runs the ROM the loader stored).
//!
//! Nothing here answers a request itself. What this file adds is outside
//! the servers' protocol logic:
//! - the player image's glue: its own `Stats` and `Audio` frames
//!   ([`SimBoard::push_stats`], [`SimBoard::audio_script`]) and the
//!   recorded `Input`s ([`SimBoard::inputs`]);
//! - faults on the wire: [`Faulty`] in front of the board (lost,
//!   corrupted, delayed requests and replies), [`DumpFault`]s on the dump
//!   frames the session sends, [`SimBoard::corrupt_save_at`] on the save
//!   chunks it receives, and [`MemNor::corrupt_rom_at`] in the flash;
//! - two labelled overrides of the real `HelloReply`
//!   ([`SimBoard::hello_protocol`], [`SimBoard::hello_max_chunk`]): a
//!   board of another protocol version or chunk size, which no image of
//!   this tree is.
//!
//! Both are [`Transport`]s whose reads never block: a read with nothing
//! queued returns 0 at once, which the client treats as the timeout
//! passing.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::io;
use std::time::Duration;

use gbc_firmware::buttons::Shared;
use gbc_firmware::rom_image;
use gbc_firmware::save::{Image, Layout};
use gbc_link::{FrameDecoder, MAX_FRAME, Message, NakCode};
use gbc_link_board::link_session::{Counters as SessionCounters, Game, Session};
use gbc_link_board::loader::{BLOCK_BYTES, Loader, Nor, SECTOR_BYTES};

use crate::client::IMAGE_HEADER_LEN;
use crate::transport::Transport;

/// Work the board did, to check that duplicates are not redone: the real
/// servers' counters (loader and session summed).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Work {
    pub hellos: u32,
    /// `RomBegin`s that started a load (erased the header's block).
    pub begins: u32,
    pub chunks: u32,
    pub ends: u32,
    /// Requests answered again from the stored reply.
    pub duplicates: u32,
    /// Frames that failed to decode (answered `Nak`).
    pub bad_frames: u32,
    pub save_begins: u32,
    pub save_chunks: u32,
    /// Dumps started (requested or [`SimBoard::board_dump`]).
    pub dumps: u32,
}

/// A fault in a dump the board sends (see [`SimBoard::dump_faults`]),
/// applied to the session's dump frames on their way out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DumpFault {
    /// Only the first `n` chunks arrive.
    Truncate(usize),
    /// One byte of chunk frame `i` is changed (fails its CRC).
    CorruptChunk(usize),
    /// The `SaveBegin` frame fails its CRC (the chunks are fine).
    CorruptBegin,
    /// `SaveBegin.hash` is off by one (re-encoded, a valid frame).
    BadHash,
    /// A `Stats` frame goes out before every chunk (not a fault: the PC
    /// must skip it; the player image interleaves its `Stats`).
    StatsBetween,
}

/// [`SimBoard::dump_faults`] index for "every dump".
pub const EVERY_DUMP: u32 = u32::MAX;

/// The QSPI NOR in memory, as the loader needs it ([`Nor`]): erase sets
/// 0xFF, program only clears bits (as NOR does), the window is the bytes.
/// A misaligned erase panics, so a loader bug shows here.
pub struct MemNor {
    pub bytes: Vec<u8>,
    /// Flips bit 0 of this ROM offset (NOR address `IMAGE_HEADER_LEN +`
    /// it) as it is programmed: bad flash, which the loader's read-back
    /// hash then catches.
    pub corrupt_rom_at: Option<u32>,
}

impl MemNor {
    fn erase(&mut self, addr: u32, unit: u32) -> Result<(), Infallible> {
        assert!(addr.is_multiple_of(unit), "erase at {addr:#x}");
        self.bytes[addr as usize..(addr + unit) as usize].fill(0xFF);
        Ok(())
    }
}

impl Nor for MemNor {
    type Error = Infallible;

    fn size(&self) -> u32 {
        self.bytes.len() as u32
    }

    fn erase_4k(&mut self, addr: u32) -> Result<(), Infallible> {
        self.erase(addr, SECTOR_BYTES)
    }

    fn erase_64k(&mut self, addr: u32) -> Result<(), Infallible> {
        self.erase(addr, BLOCK_BYTES)
    }

    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), Infallible> {
        let bad = self.corrupt_rom_at.map(|o| IMAGE_HEADER_LEN + o);
        let dst = &mut self.bytes[addr as usize..addr as usize + data.len()];
        for (i, (d, &s)) in dst.iter_mut().zip(data).enumerate() {
            *d &= s;
            if bad == Some(addr + i as u32) {
                *d ^= 1;
            }
        }
        Ok(())
    }

    fn with_window<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Result<R, Infallible> {
        Ok(f(&self.bytes))
    }
}

/// The game the player image runs, as its link session sees it
/// ([`Game`]).
#[derive(Clone, Debug, Default)]
pub struct SimGame {
    /// The cartridge's save layout: `None` is a cartridge without a save
    /// (`SaveBegin` gets `Nak UnexpectedType`); [`ram_only`] makes one
    /// that takes a save of exactly that size.
    pub layout: Option<Layout>,
    /// The running game's save (what a dump sends); an applied save load
    /// replaces it.
    pub save: Option<Vec<u8>>,
    /// Length and hash of the running ROM ([`SimBoard::boot_player`]
    /// takes it from the NOR's image).
    pub rom: Option<(u32, u64)>,
}

impl Game for SimGame {
    fn layout(&self) -> Option<Layout> {
        self.layout
    }

    fn rom(&self) -> Option<(u32, u64)> {
        self.rom
    }

    fn apply(&mut self, bytes: &[u8], _: &Image<'_>) -> bool {
        self.save = Some(bytes.to_vec());
        true
    }

    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        let save = self.save.as_ref()?;
        out.get_mut(..save.len())?.copy_from_slice(save);
        Some(save.len())
    }
}

/// A save layout of `len` bytes of cartridge RAM and no clock: the
/// session takes a save of exactly `len` bytes.
pub fn ram_only(len: usize) -> Layout {
    Layout {
        ram_size: len,
        clock: None,
    }
}

/// The player image's save buffer size (`bin/player.rs` `SAVE_BYTES`).
const SAVE_BYTES: usize = 64 * 1024 + 64;

/// The player image: its link session and the buttons it feeds.
struct Player {
    session: Session<'static>,
    shared: Shared,
}

pub struct SimBoard {
    /// The `loader` image's server; its NOR is also what the player image
    /// boots from.
    pub loader: Loader<MemNor>,
    /// The player image once booted; it answers instead of the loader.
    player: Option<Player>,
    /// The player's game.
    pub game: SimGame,
    /// Every `Input` bitmap the session took, in order (the PC buttons
    /// after each).
    pub inputs: Vec<u8>,
    /// `Audio` messages `(first, held, code words: left 12-bit code in
    /// bits 11:0, right in 27:16)` the player sends once the session's
    /// stream is on, right after the answer that turned it on.
    pub audio_script: Vec<(u32, u16, Vec<u32>)>,
    /// The `Audio.format` byte the player sends ([`gbc_link::audio::U12`];
    /// the data is always U12, so a test can send an unknown format).
    pub audio_format: u8,
    /// Faults by dump number (0 = the first dump the session sends, or
    /// [`EVERY_DUMP`]).
    pub dump_faults: Vec<(u32, DumpFault)>,
    /// Flips bit 0 of this save offset in the `SaveChunk` that carries it,
    /// re-encoded past its CRC (as if the board's RAM took it), so the
    /// session's hash check refuses the save.
    pub corrupt_save_at: Option<u32>,
    /// Wire override: the `HelloReply` says this protocol version instead
    /// of the real server's.
    pub hello_protocol: Option<u16>,
    /// Wire override: the `HelloReply` says this chunk size instead of the
    /// real server's (the servers take any chunk up to theirs).
    pub hello_max_chunk: Option<u16>,
    /// Dumps seen going out.
    dumps_out: u32,
    /// The faults of the dump going out, and its chunks so far.
    dump: (Vec<DumpFault>, usize),
    /// A request frame being received.
    rx: Vec<u8>,
    out: VecDeque<u8>,
}

impl SimBoard {
    /// The `loader` image, with an erased `flash_len`-byte NOR (a
    /// multiple of 64 KiB).
    pub fn loader(flash_len: usize) -> Self {
        assert!(flash_len.is_multiple_of(BLOCK_BYTES as usize));
        Self {
            loader: Loader::new(MemNor {
                bytes: vec![0xFF; flash_len],
                corrupt_rom_at: None,
            }),
            player: None,
            game: SimGame::default(),
            inputs: Vec::new(),
            audio_script: Vec::new(),
            audio_format: gbc_link::audio::U12,
            dump_faults: Vec::new(),
            corrupt_save_at: None,
            hello_protocol: None,
            hello_max_chunk: None,
            dumps_out: 0,
            dump: (Vec::new(), 0),
            rx: Vec::new(),
            out: VecDeque::new(),
        }
    }

    /// The `player` image, booted over an erased `flash_len`-byte NOR (no
    /// ROM: set [`SimGame::rom`] for one).
    pub fn player(flash_len: usize) -> Self {
        let mut b = Self::loader(flash_len);
        b.boot_player();
        b
    }

    /// Reboots into the player image: a fresh session (nothing of the
    /// loader's link state carries over), running the NOR's image if it
    /// validates. Audio is on offer, as in the player.
    pub fn boot_player(&mut self) {
        // The session borrows its save buffer for its lifetime; a test
        // board leaks one each.
        let buf = Box::leak(vec![0u8; SAVE_BYTES].into_boxed_slice());
        let mut session = Session::new(buf, self.loader.nor.size());
        session.set_audio_capable(true);
        self.game.rom = self.stored();
        self.rx.clear();
        self.player = Some(Player {
            session,
            shared: Shared::new(),
        });
    }

    /// The whole NOR: the image header at 0, the ROM from
    /// [`IMAGE_HEADER_LEN`] (ADR 0037).
    pub fn flash(&self) -> &[u8] {
        &self.loader.nor.bytes
    }

    /// The programmed ROM bytes (`len` of them, after the header).
    pub fn rom(&self, len: usize) -> &[u8] {
        let start = IMAGE_HEADER_LEN as usize;
        &self.flash()[start..start + len]
    }

    /// The `(len, hash)` of the NOR's image, if it validates.
    pub fn stored(&self) -> Option<(u32, u64)> {
        let w = self.flash();
        rom_image::validate(w).ok()?;
        rom_image::read_header(w).ok().map(|h| (h.len, h.hash))
    }

    fn session_counters(&self) -> SessionCounters {
        self.player
            .as_ref()
            .map(|p| p.session.counters())
            .unwrap_or_default()
    }

    /// What the servers did, from their counters.
    pub fn work(&self) -> Work {
        let l = self.loader.counters;
        let s = self.session_counters();
        Work {
            hellos: l.hellos + s.hellos,
            begins: l.begins,
            chunks: l.chunks,
            ends: l.ends,
            duplicates: l.duplicates + s.duplicates,
            bad_frames: l.bad_frames + s.bad_frames,
            save_begins: s.save_begins,
            save_chunks: s.save_chunks,
            dumps: s.dumps,
        }
    }

    /// The session's audio stream is on (ADR 0041).
    pub fn audio_on(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.session.audio_on())
    }

    /// Whether the session takes `AudioStart` (else `Nak UnexpectedType`,
    /// as a player without audio).
    pub fn set_audio_capable(&mut self, on: bool) {
        self.running().session.set_audio_capable(on);
    }

    fn running(&mut self) -> &mut Player {
        self.player.as_mut().expect("the player image is running")
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

    /// Queues an `Audio` message of the DAC words `frames` (whatever
    /// [`SimBoard::audio_on`] says: the test decides).
    pub fn push_audio(&mut self, first: u32, held: u16, frames: &[u32]) {
        use gbc_link::audio::{encode_u12, sample};
        let data: Vec<u8> = frames
            .iter()
            .flat_map(|&w| encode_u12(sample(w as u16), sample((w >> 16) as u16)))
            .collect();
        let m = Message::Audio {
            first,
            held,
            format: self.audio_format,
            data: &data,
        };
        self.out.extend(encode(&m, 0));
    }

    /// Everything the board has sent and the client has not read.
    pub fn take_output(&mut self) -> Vec<u8> {
        self.out.drain(..).collect()
    }

    /// Feeds received bytes; the running image answers each frame.
    pub fn receive(&mut self, data: &[u8]) {
        for &b in data {
            self.rx.push(b);
            if b == 0 {
                let frame = std::mem::take(&mut self.rx);
                self.request(frame);
            }
        }
    }

    /// A dump nobody asked for (the joystick chord):
    /// `Session::board_dump`, seq 0.
    pub fn board_dump(&mut self) {
        let p = self.player.as_mut().expect("the player image is running");
        p.session.board_dump(&mut self.game);
        let wire = self.drain_player();
        self.send(&wire);
    }

    /// One frame (delimiter included) to the running image.
    fn request(&mut self, mut frame: Vec<u8>) {
        if let Some(bad) = self.corrupt_save_at
            && let Some((seq, Seen::SaveChunk { offset, mut data })) = seen(&frame)
            && (offset..offset + data.len() as u32).contains(&bad)
        {
            data[(bad - offset) as usize] ^= 1;
            frame = encode(
                &Message::SaveChunk {
                    offset,
                    data: &data,
                },
                seq,
            );
        }
        let Some(p) = &mut self.player else {
            let mut wire = Vec::new();
            for &b in &frame {
                if let Some(r) = self.loader.feed(b) {
                    wire.extend_from_slice(r.wire);
                }
            }
            self.send(&wire);
            return;
        };
        let inputs = p.session.counters().inputs;
        for &b in &frame {
            p.session.feed(b, &mut self.game, &p.shared);
        }
        if p.session.counters().inputs != inputs {
            self.inputs.push(p.shared.pc().bits());
        }
        let wire = self.drain_player();
        self.send(&wire);
        if self.audio_on() {
            for (first, held, frames) in std::mem::take(&mut self.audio_script) {
                self.push_audio(first, held, &frames);
            }
        }
    }

    /// Everything the session has to send (replies, then dump frames).
    fn drain_player(&mut self) -> Vec<u8> {
        let p = self.running();
        let mut wire = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            let n = p.session.fill_tx(&mut buf);
            if n == 0 {
                return wire;
            }
            wire.extend_from_slice(&buf[..n]);
        }
    }

    /// Sends the servers' frames, through the wire overrides and the dump
    /// faults.
    fn send(&mut self, wire: &[u8]) {
        for frame in wire.split_inclusive(|&b| b == 0) {
            let mut frame = frame.to_vec();
            match seen(&frame) {
                Some((
                    seq,
                    Seen::HelloReply {
                        protocol,
                        max_chunk,
                        flash_len,
                    },
                )) if self.hello_protocol.is_some() || self.hello_max_chunk.is_some() => {
                    let m = Message::HelloReply {
                        protocol: self.hello_protocol.unwrap_or(protocol),
                        max_chunk: self.hello_max_chunk.unwrap_or(max_chunk),
                        flash_len,
                    };
                    frame = encode(&m, seq);
                }
                Some((seq, Seen::SaveBegin { len, hash })) => {
                    let n = self.dumps_out;
                    self.dumps_out += 1;
                    let faults: Vec<DumpFault> = self
                        .dump_faults
                        .iter()
                        .filter(|&&(i, _)| i == n || i == EVERY_DUMP)
                        .map(|&(_, f)| f)
                        .collect();
                    if faults.contains(&DumpFault::BadHash) {
                        frame = encode(
                            &Message::SaveBegin {
                                len,
                                hash: hash ^ 1,
                            },
                            seq,
                        );
                    }
                    if faults.contains(&DumpFault::CorruptBegin) {
                        flip(&mut frame);
                    }
                    self.dump = (faults, 0);
                }
                Some((_, Seen::SaveChunk { .. })) => {
                    let i = self.dump.1;
                    self.dump.1 += 1;
                    let faults = &self.dump.0;
                    if faults
                        .iter()
                        .any(|&f| matches!(f, DumpFault::Truncate(n) if i >= n))
                    {
                        continue;
                    }
                    let corrupt = faults.contains(&DumpFault::CorruptChunk(i));
                    if faults.contains(&DumpFault::StatsBetween) {
                        self.push_stats(i as u32, 1, 2);
                    }
                    if corrupt {
                        flip(&mut frame);
                    }
                }
                _ => {}
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

/// The frames the wire wrappers look into, owned.
enum Seen {
    HelloReply {
        protocol: u16,
        max_chunk: u16,
        flash_len: u32,
    },
    SaveBegin {
        len: u32,
        hash: u64,
    },
    SaveChunk {
        offset: u32,
        data: Vec<u8>,
    },
    Other,
}

/// `frame` decoded (seq and what it is), if it decodes.
fn seen(frame: &[u8]) -> Option<(u8, Seen)> {
    let mut dec = FrameDecoder::new();
    let mut got = None;
    for &b in frame {
        if let Some(r) = dec.feed(b) {
            got = r.ok().map(|(seq, m)| {
                let s = match m {
                    Message::HelloReply {
                        protocol,
                        max_chunk,
                        flash_len,
                    } => Seen::HelloReply {
                        protocol,
                        max_chunk,
                        flash_len,
                    },
                    Message::SaveBegin { len, hash } => Seen::SaveBegin { len, hash },
                    Message::SaveChunk { offset, data } => Seen::SaveChunk {
                        offset,
                        data: data.to_vec(),
                    },
                    _ => Seen::Other,
                };
                (seq, s)
            });
        }
    }
    got
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
