//! The board side of the M5 serial link (plan Stage 2 M5; protocol in
//! `gbc-link`, the board's duties in `docs/reference/link-protocol.md`):
//! live input, save load and save dump, polled between emulated frames.
//!
//! Board-independent, as [`crate::loader`]: the game is behind the small
//! [`Game`] trait and the UART is the caller's. The caller feeds every
//! received byte to [`Session::feed`] and, whenever its transmitter is
//! idle, lets [`Session::fill_tx`] write the next frames to send into its
//! transmit buffer. Only `core`, `gbc_link` and `gbc_firmware` paths are
//! used, so `tools/gbc-pc/tests/link_session.rs` compiles this file on
//! the host (`#[path]`) and runs the PC client against it.
//!
//! Behavior per message:
//! - Stop-and-wait replies as the loader: a frame byte-identical to the
//!   last answered request is a resend and gets the stored reply (the
//!   save verdict included) without being redone. `Hello` is handled
//!   afresh and forgets the last frame. A frame that fails COBS, CRC or
//!   its length check gets `Nak` with [`gbc_link::DecodeError::nak_code`]
//!   (its `seq` when the CRC vouches for it, else 0).
//! - `Hello`: `HelloReply` (this board's protocol version, [`MAX_CHUNK`],
//!   the `flash_len` given to [`Session::new`]); a save load in progress
//!   and a dump still being sent are dropped (a new session never wants
//!   the old one's), and the audio stream stops.
//! - `RomBegin { len, hash }`: `AlreadyLoaded` when the running game's ROM
//!   ([`Game::rom`]) has this length and hash, so `gbc-pc load <rom>
//!   --sav <f>` goes straight on to the save; otherwise `Nak
//!   UnexpectedType` (this session takes no ROM; the `loader` image does).
//!   `RomChunk` and `RomEnd`: `Nak UnexpectedType`.
//! - `Input { buttons }`: [`Shared::set_pc`], never answered. With no
//!   `Input` for [`PC_TIMEOUT_FRAMES`] frames ([`Session::end_frame`]) the
//!   PC buttons are released (the PC repeats its state every 500 ms).
//! - `SaveBegin { len, hash }`: `Nak UnexpectedType` if the game has no
//!   save ([`Game::layout`] is `None`), `Nak OutOfRange` if `len` is not
//!   one [`save::check`] accepts for the layout (RAM alone, or RAM plus
//!   the clock footer: 48 or 44 bytes for MBC3, 17 for HuC3) or does not
//!   fit the save buffer; else `Ack`. A new `SaveBegin` discards a load in
//!   progress.
//! - `SaveChunk { offset, data }`: `Nak UnexpectedType` with no load in
//!   progress, `Nak OutOfRange` for an empty chunk, an offset that does
//!   not continue the save or one that ends past `len`; else the bytes go
//!   into the save buffer and `Ack`. The chunk that completes `len` gets
//!   the **verdict** instead: the whole buffer is hashed (FNV-1a 64, `Nak
//!   HashMismatch`), checked with [`save::check`] (`Nak OutOfRange`, e.g.
//!   `BadFooter`), and only then applied ([`Game::apply`]): `Ack`. A
//!   refused save changes nothing in the game.
//! - `SaveDumpRequest`: the save is snapshot at once ([`Game::take`] into
//!   the save buffer) and streamed by [`Session::fill_tx`] as `SaveBegin
//!   { len, hash }` then `SaveChunk`s of up to [`gbc_link::MAX_CHUNK`]
//!   bytes, all with the request's seq; `Nak UnexpectedType` if there is
//!   no save. Each request restarts the dump (a new snapshot). Never
//!   stored as the last request, so it is never answered from the store.
//!   [`Session::board_dump`] starts the same stream unrequested (the
//!   joystick chord), with seq 0.
//! - `AudioStart` / `AudioStop` (ADR 0041): `Ack`, and the audio stream
//!   is on / off ([`Session::audio_on`]); `Nak UnexpectedType` unless the
//!   caller enabled audio ([`Session::set_audio_capable`]). The stream
//!   also stops on `Hello` and after [`PC_TIMEOUT_FRAMES`] frames without
//!   any decoded frame from the PC (it never outlives the PC). The caller
//!   writes the `Audio` messages through [`Session::fill_tx_with`].
//!
//! **One save buffer** serves the load and the dump (a 128 KiB-RAM save
//! twice would not fit AXI SRAM beside the cartridge RAM): a
//! `SaveDumpRequest` drops a save load in progress (its next chunk gets
//! `Nak UnexpectedType`), and a `SaveBegin` stops a dump being streamed
//! (the PC sees a gap and retries). The PC never does both at once.
//!
//! Transmit order ([`Session::fill_tx_with`]): queued replies first, then
//! the caller's frames (the player's `Stats` and audio), then as many
//! whole dump frames as fit. A reply is never split, so text the
//! caller sends between `fill_tx` calls (when [`Session::busy`] is false)
//! never lands inside a frame.

use gbc_firmware::buttons::{Buttons, Shared};
use gbc_firmware::fnv;
use gbc_firmware::save::{
    self, ClockKind, HUC3_FOOTER_LEN, Image, Layout, MBC3_FOOTER_LEN, VBA_FOOTER_LEN,
};
use gbc_link::{FrameDecoder, MAX_ENCODED, MAX_FRAME, Message, NakCode, PROTOCOL_VERSION};

/// The largest chunk this board takes (`HelloReply.max_chunk`), and the
/// size of the chunks it dumps: the protocol's maximum.
pub const MAX_CHUNK: u16 = gbc_link::MAX_CHUNK as u16;
const _: () = assert!(MAX_CHUNK as usize == gbc_link::MAX_CHUNK);

/// Frames without an `Input` after which the PC's buttons are released:
/// 1.5 s at 60 Hz, three times the PC's 500 ms repeat
/// (`docs/reference/link-protocol.md` "Input").
pub const PC_TIMEOUT_FRAMES: u32 = 90;

/// The game as the session needs it.
pub trait Game {
    /// The running cartridge's save layout; `None` with no game or a
    /// cartridge without a save.
    fn layout(&self) -> Option<Layout>;
    /// Length and FNV-1a 64 of the running game's ROM, if any.
    fn rom(&self) -> Option<(u32, u64)>;
    /// Puts a save in. `bytes` passed [`save::check`] for [`Game::layout`]
    /// as `image`. False if it was refused anyway (nothing changed).
    fn apply(&mut self, bytes: &[u8], image: &Image<'_>) -> bool;
    /// Writes the current save into `out`; its length, or `None` if there
    /// is no save or `out` is too small.
    fn take(&mut self, out: &mut [u8]) -> Option<usize>;
}

/// What the session has done since reset, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames that decoded (duplicates included).
    pub frames: u32,
    /// Frames that did not decode (answered `Nak`).
    pub bad_frames: u32,
    /// Resends answered from the stored reply.
    pub duplicates: u32,
    pub hellos: u32,
    pub inputs: u32,
    /// Times the PC buttons were released for want of `Input`.
    pub pc_timeouts: u32,
    pub save_begins: u32,
    pub save_chunks: u32,
    /// Verdicts `Ack` (the save went in).
    pub saves_applied: u32,
    /// Verdicts `Nak` (hash, format or the game refused).
    pub saves_refused: u32,
    /// Dumps started (requested or by the chord) and fully queued.
    pub dumps: u32,
    pub dumps_done: u32,
    /// `Nak`s sent for decoded frames (not counting replays).
    pub naks: u32,
    /// Replies dropped because the reply queue was full (never expected:
    /// the PC waits for each answer) or because a reply did not fit
    /// [`REPLY_MAX`] (never expected either: the const assertion there).
    pub replies_dropped: u32,
    /// `AudioStart`s and `AudioStop`s handled (not counting replays).
    pub audio_starts: u32,
    pub audio_stops: u32,
    /// Times the audio stream stopped for want of any frame from the PC.
    pub audio_timeouts: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Load {
    len: u32,
    hash: u64,
    /// Bytes received so far (the next chunk's offset).
    next: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dump {
    seq: u8,
    len: u32,
    hash: u64,
    /// `SaveBegin` queued.
    begun: bool,
    /// Bytes queued so far.
    next: u32,
}

/// The largest reply body: `HelloReply` (protocol u16, max_chunk u16,
/// flash_len u32), 8 bytes; `Nak` has 1, `Ack` and `AlreadyLoaded` none
/// (`tests/link_session.rs` encodes each).
pub const REPLY_BODY_MAX: usize = 8;

/// One reply frame's buffer: at least [`REPLY_FRAME_MAX`], with room to spare.
pub const REPLY_MAX: usize = 32;
/// The largest reply frame on the wire: type, seq, body and CRC, COBS-encoded,
/// plus the delimiter.
pub const REPLY_FRAME_MAX: usize = gbc_link::cobs::max_encoded_len(2 + REPLY_BODY_MAX + 4) + 1;
const _: () = assert!(REPLY_FRAME_MAX <= REPLY_MAX);
/// Replies waiting for the transmitter.
const QUEUE_MAX: usize = 8 * REPLY_MAX;

/// Everything but the decoder and the frame being received, so a decoded
/// message (which borrows the decoder) can be handled with `&mut` to it.
struct State<'b> {
    buf: &'b mut [u8],
    flash_len: u32,
    counters: Counters,
    load: Option<Load>,
    dump: Option<Dump>,
    last_req: [u8; MAX_ENCODED],
    last_req_len: usize,
    last_reply: [u8; REPLY_MAX],
    last_reply_len: usize,
    queue: [u8; QUEUE_MAX],
    queue_len: usize,
    frames_since_input: u32,
    /// Frames since any frame decoded (saturating).
    frames_since_rx: u32,
    audio_capable: bool,
    audio: bool,
}

pub struct Session<'b> {
    dec: FrameDecoder,
    /// The frame being received, as on the wire (no delimiter).
    raw: [u8; MAX_ENCODED],
    raw_len: usize,
    st: State<'b>,
}

/// Where a save load or dump stands, for the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Loading { received: u32, len: u32 },
    Dumping { queued: u32, len: u32 },
}

const fn nak(code: NakCode) -> Message<'static> {
    Message::Nak { code }
}

/// Whether [`save::check`] can accept a save of `len` bytes for `layout`
/// (its exact lengths; the content is checked at the verdict).
pub fn accepted_len(layout: Layout, len: usize) -> bool {
    let ram = layout.ram_size;
    len != 0
        && (len == ram
            || match layout.clock {
                Some(ClockKind::Mbc3) => {
                    len == ram + MBC3_FOOTER_LEN || len == ram + VBA_FOOTER_LEN
                }
                Some(ClockKind::Huc3) => len == ram + HUC3_FOOTER_LEN,
                None => false,
            })
}

impl<'b> Session<'b> {
    /// `buf` is the save buffer (load and dump; at least the largest save
    /// the games take: 128 KiB + 48); `flash_len` goes into `HelloReply`.
    pub fn new(buf: &'b mut [u8], flash_len: u32) -> Self {
        Self {
            dec: FrameDecoder::new(),
            raw: [0; MAX_ENCODED],
            raw_len: 0,
            st: State {
                buf,
                flash_len,
                counters: Counters::default(),
                load: None,
                dump: None,
                last_req: [0; MAX_ENCODED],
                last_req_len: 0,
                last_reply: [0; REPLY_MAX],
                last_reply_len: 0,
                queue: [0; QUEUE_MAX],
                queue_len: 0,
                frames_since_input: 0,
                frames_since_rx: 0,
                audio_capable: false,
                audio: false,
            },
        }
    }

    pub fn counters(&self) -> Counters {
        self.st.counters
    }

    pub fn progress(&self) -> Progress {
        match (self.st.load, self.st.dump) {
            (_, Some(d)) => Progress::Dumping {
                queued: d.next,
                len: d.len,
            },
            (Some(l), None) => Progress::Loading {
                received: l.next,
                len: l.len,
            },
            (None, None) => Progress::Idle,
        }
    }

    /// Whether `AudioStart` is accepted (off by default: link-probe and
    /// the player's no-ROM loop have no audio). Turning it off stops a
    /// stream.
    pub fn set_audio_capable(&mut self, on: bool) {
        self.st.audio_capable = on;
        self.st.audio &= on;
    }

    /// The PC asked for audio and is still there.
    pub fn audio_on(&self) -> bool {
        self.st.audio
    }

    /// Replies or dump frames are waiting for [`Session::fill_tx`]: the
    /// caller must not send text now.
    pub fn busy(&self) -> bool {
        self.st.queue_len != 0 || self.st.dump.is_some()
    }

    /// One received byte.
    pub fn feed(&mut self, b: u8, game: &mut impl Game, shared: &Shared) {
        if b != 0 {
            // Past MAX_ENCODED the decoder reports Oversize; the raw copy
            // is then never compared.
            if let Some(slot) = self.raw.get_mut(self.raw_len) {
                *slot = b;
                self.raw_len += 1;
            }
        }
        let decoded = self.dec.feed(b);
        if b == 0 && decoded.is_none() {
            // An empty frame (a lone 0x00): ignored.
            self.raw_len = 0;
        }
        let Some(decoded) = decoded else { return };
        let raw_len = core::mem::take(&mut self.raw_len);
        let st = &mut self.st;
        let (seq, msg) = match decoded {
            Ok(x) => x,
            Err(e) => {
                st.counters.bad_frames += 1;
                st.queue_msg(&nak(e.nak_code()), e.seq().unwrap_or(0));
                return;
            }
        };
        st.counters.frames += 1;
        st.frames_since_rx = 0;
        let raw = self.raw.get(..raw_len).unwrap_or(&[]);
        match msg {
            Message::Input { buttons } => {
                st.counters.inputs += 1;
                st.frames_since_input = 0;
                shared.set_pc(buttons);
                return;
            }
            Message::SaveDumpRequest => {
                st.start_dump(seq, game);
                return;
            }
            Message::Hello { .. } => st.last_req_len = 0,
            _ if st.last_req_len != 0 && st.last_req.get(..st.last_req_len) == Some(raw) => {
                st.counters.duplicates += 1;
                let n = st.last_reply_len;
                let reply = st.last_reply;
                st.queue_wire(reply.get(..n).unwrap_or(&[]));
                return;
            }
            _ => {}
        }
        let answer = st.handle(msg, game);
        if let Message::Nak { .. } = answer {
            st.counters.naks += 1;
        }
        st.last_reply_len = encode_reply(&answer, seq, &mut st.last_reply, &mut st.counters);
        let reply = st.last_reply;
        st.queue_wire(reply.get(..st.last_reply_len).unwrap_or(&[]));
        if let Some(dst) = st.last_req.get_mut(..raw_len) {
            dst.copy_from_slice(raw);
            st.last_req_len = raw_len;
        }
    }

    /// Starts a dump nobody asked for (the joystick chord), with seq 0.
    pub fn board_dump(&mut self, game: &mut impl Game) {
        self.st.start_dump(0, game);
    }

    /// Call once per emulated frame: releases the PC buttons after
    /// [`PC_TIMEOUT_FRAMES`] frames without an `Input`.
    pub fn end_frame(&mut self, shared: &Shared) {
        let st = &mut self.st;
        st.frames_since_input = st.frames_since_input.saturating_add(1);
        if st.frames_since_input == PC_TIMEOUT_FRAMES && shared.pc() != Buttons::EMPTY {
            shared.set_pc(0);
            st.counters.pc_timeouts += 1;
        }
        st.frames_since_rx = st.frames_since_rx.saturating_add(1);
        if st.frames_since_rx >= PC_TIMEOUT_FRAMES && st.audio {
            st.audio = false;
            st.counters.audio_timeouts += 1;
        }
    }

    /// Writes what is waiting to be sent into `out` (the caller's idle
    /// transmit buffer): the queued replies, then as many whole dump
    /// frames as fit. Returns the bytes written; 0 when nothing waits.
    /// Give it at least [`MAX_FRAME`] + 256 bytes, or a dump never moves.
    pub fn fill_tx(&mut self, out: &mut [u8]) -> usize {
        self.fill_tx_with(out, |_| 0)
    }

    /// [`Session::fill_tx`] with the caller's frames between the replies
    /// and the dump: `extra` gets the room left after the replies and
    /// returns how many bytes it wrote there (whole frames only).
    pub fn fill_tx_with(
        &mut self,
        out: &mut [u8],
        extra: impl FnOnce(&mut [u8]) -> usize,
    ) -> usize {
        let st = &mut self.st;
        let mut n = 0;
        if let (Some(dst), Some(src)) = (out.get_mut(..st.queue_len), st.queue.get(..st.queue_len))
        {
            dst.copy_from_slice(src);
            n = st.queue_len;
            st.queue_len = 0;
        }
        if let Some(room) = out.get_mut(n..) {
            n += extra(room).min(room.len());
        }
        while let Some(mut d) = st.dump {
            let Some(room) = out.get_mut(n..) else { break };
            let start = d.next as usize;
            let end = (start + gbc_link::MAX_CHUNK).min(d.len as usize);
            let msg = if !d.begun {
                Message::SaveBegin {
                    len: d.len,
                    hash: d.hash,
                }
            } else {
                Message::SaveChunk {
                    offset: d.next,
                    data: st.buf.get(start..end).unwrap_or(&[]),
                }
            };
            if room.len() < MAX_FRAME {
                break;
            }
            let Ok(k) = gbc_link::encode(&msg, d.seq, room) else {
                break;
            };
            n += k;
            if d.begun {
                d.next = end as u32;
            }
            d.begun = true;
            if d.next == d.len {
                st.dump = None;
                st.counters.dumps_done += 1;
            } else {
                st.dump = Some(d);
            }
        }
        n
    }
}

/// Encodes a reply into `out`: its length, or 0 (nothing to send, counted
/// in [`Counters::replies_dropped`]) if it does not fit.
fn encode_reply(msg: &Message<'_>, seq: u8, out: &mut [u8], counters: &mut Counters) -> usize {
    gbc_link::encode(msg, seq, out).unwrap_or_else(|_| {
        counters.replies_dropped += 1;
        0
    })
}

impl State<'_> {
    fn queue_wire(&mut self, wire: &[u8]) {
        match self
            .queue
            .get_mut(self.queue_len..self.queue_len + wire.len())
        {
            Some(dst) => {
                dst.copy_from_slice(wire);
                self.queue_len += wire.len();
            }
            None => self.counters.replies_dropped += 1,
        }
    }

    fn queue_msg(&mut self, msg: &Message<'_>, seq: u8) {
        let mut wire = [0u8; REPLY_MAX];
        let n = encode_reply(msg, seq, &mut wire, &mut self.counters);
        if n != 0 {
            self.queue_wire(wire.get(..n).unwrap_or(&[]));
        }
    }

    fn start_dump(&mut self, seq: u8, game: &mut impl Game) {
        // The shared buffer: a load in progress is gone (module docs).
        self.load = None;
        self.dump = None;
        match game.take(self.buf) {
            Some(len) if len != 0 => {
                let hash = fnv::hash(self.buf.get(..len).unwrap_or(&[]));
                self.counters.dumps += 1;
                self.dump = Some(Dump {
                    seq,
                    len: len as u32,
                    hash,
                    begun: false,
                    next: 0,
                });
            }
            _ => {
                self.counters.naks += 1;
                self.queue_msg(&nak(NakCode::UnexpectedType), seq);
            }
        }
    }

    fn handle(&mut self, msg: Message<'_>, game: &mut impl Game) -> Message<'static> {
        match msg {
            Message::Hello { .. } => {
                self.counters.hellos += 1;
                self.load = None;
                self.dump = None;
                self.audio = false;
                Message::HelloReply {
                    protocol: PROTOCOL_VERSION,
                    max_chunk: MAX_CHUNK,
                    flash_len: self.flash_len,
                }
            }
            Message::RomBegin { len, hash } if game.rom() == Some((len, hash)) => {
                Message::AlreadyLoaded
            }
            Message::SaveBegin { len, hash } => self.save_begin(len, hash, game),
            Message::SaveChunk { offset, data } => self.save_chunk(offset, data, game),
            Message::AudioStart | Message::AudioStop if self.audio_capable => {
                let on = msg == Message::AudioStart;
                self.audio = on;
                if on {
                    self.counters.audio_starts += 1;
                } else {
                    self.counters.audio_stops += 1;
                }
                Message::Ack
            }
            _ => nak(NakCode::UnexpectedType),
        }
    }

    fn save_begin(&mut self, len: u32, hash: u64, game: &impl Game) -> Message<'static> {
        self.load = None;
        let Some(layout) = game.layout() else {
            return nak(NakCode::UnexpectedType);
        };
        if !accepted_len(layout, len as usize) || len as usize > self.buf.len() {
            return nak(NakCode::OutOfRange);
        }
        // The shared buffer: a dump being streamed stops (module docs).
        self.dump = None;
        self.counters.save_begins += 1;
        self.load = Some(Load { len, hash, next: 0 });
        Message::Ack
    }

    fn save_chunk(&mut self, offset: u32, data: &[u8], game: &mut impl Game) -> Message<'static> {
        let Some(mut l) = self.load else {
            return nak(NakCode::UnexpectedType);
        };
        let end = u64::from(offset) + data.len() as u64;
        if data.is_empty() || offset != l.next || end > u64::from(l.len) {
            return nak(NakCode::OutOfRange);
        }
        let Some(dst) = self.buf.get_mut(offset as usize..end as usize) else {
            return nak(NakCode::OutOfRange);
        };
        dst.copy_from_slice(data);
        self.counters.save_chunks += 1;
        l.next = end as u32;
        if l.next < l.len {
            self.load = Some(l);
            return Message::Ack;
        }
        // The verdict: all of it hashed and checked before anything is
        // applied.
        self.load = None;
        let bytes = self.buf.get(..l.len as usize).unwrap_or(&[]);
        let verdict = if fnv::hash(bytes) != l.hash {
            nak(NakCode::HashMismatch)
        } else {
            match game.layout().map(|layout| save::check(layout, bytes)) {
                None => nak(NakCode::UnexpectedType),
                Some(Err(_)) => nak(NakCode::OutOfRange),
                Some(Ok(image)) if game.apply(bytes, &image) => Message::Ack,
                Some(Ok(_)) => nak(NakCode::OutOfRange),
            }
        };
        if verdict == Message::Ack {
            self.counters.saves_applied += 1;
        } else {
            self.counters.saves_refused += 1;
        }
        verdict
    }
}
