//! The board side of the ROM loader (plan Stage 2 M2 "Loader"; protocol
//! in `gbc-link`; image format ADR 0037; this design ADR 0038). It is
//! board-independent: the NOR is behind the small [`Nor`] trait and the
//! UART is the caller's (it feeds received bytes to [`Loader::feed`] and
//! sends the [`Reply`] bytes it gets back). `tools/gbc-pc/tests/board.rs`
//! runs the PC client against it on the host over a fake NOR.
//!
//! Behavior (`tools/gbc-pc/src/sim.rs` runs this server for the PC
//! tests):
//! - Stop-and-wait ([`crate::responder`]). A frame byte-identical to the
//!   last answered request (same `seq`, same content: a resend) is
//!   answered with the stored reply and not redone. `Hello` is always
//!   handled afresh and forgets the last frame (each PC session restarts
//!   `seq` at 0).
//! - A frame that fails COBS, CRC or its length check is answered `Nak`
//!   with [`gbc_link::DecodeError::nak_code`], with its `seq` when the CRC
//!   vouches for it, else 0. It is not remembered, and its `Nak` never
//!   replaces the stored reply (a resend corrupted on the way, after the
//!   first answer was lost, must not turn the next good resend's replay
//!   into that `Nak`).
//! - `Hello`: `HelloReply` with this board's protocol version (even if
//!   the PC's differs), [`MAX_CHUNK`] and the NOR size; any load in
//!   progress is dropped.
//! - `RomBegin { len, hash }`: `Nak OutOfRange` if `len` is 0 or does not
//!   fit after the header. If the stored image validates
//!   (`rom_image::validate` over the window) and has this length and hash,
//!   `AlreadyLoaded`. Otherwise the 64 KiB block at offset 0 is erased at
//!   once (it holds the header, so the old image is gone from that moment:
//!   an interrupted load reads as `Erased`, "no image"), then `Ack`.
//! - `RomChunk { offset, data }`: only while loading (else `Nak
//!   UnexpectedType`); `offset` must equal the bytes received so far and
//!   the chunk must end inside `len` (else `Nak OutOfRange`). The flash
//!   under the chunk is erased lazily, just before it is programmed: the
//!   next 64 KiB block while a whole block fits below the image's end
//!   (rounded up to 4 KiB), 4 KiB sectors for the tail. Every reply thus
//!   costs at most one erase (64 KiB: typ 0.31-0.34 s measured, max 1 s)
//!   plus four page programs, well inside the PC's 2 s timeout. Never a
//!   chip erase; the status and configuration registers are never
//!   written (the driver refuses those opcodes).
//! - `RomEnd`: `Nak OutOfRange` before all `len` bytes arrived, `Nak
//!   UnexpectedType` when not loading. The data is hashed as read back
//!   through the memory-mapped window. Only if that equals the
//!   `RomBegin` hash is the header programmed (last, ADR 0037), and the
//!   image is then checked again with `rom_image::validate` over the
//!   window (`Nak FlashError` if that fails). `RomDone` carries the
//!   read-back hash, matching or not.
//! - Any NOR error: `Nak FlashError`, and the load is dropped (a resend of
//!   the same frame gets the same `Nak` from the stored reply).
//! - Anything else (save, input, board-to-PC types): `Nak UnexpectedType`.

use gbc_firmware::rom_image::{self, HEADER_LEN, ImageError, ImageHeader};
use gbc_link::{Message, NakCode, PROTOCOL_VERSION};

/// The largest chunk this board takes (`HelloReply.max_chunk`): the
/// protocol's maximum, four NOR pages. The reply buffer sizes, shared
/// with the link session.
pub use crate::responder::{MAX_CHUNK, REPLY_BODY_MAX, REPLY_FRAME_MAX, REPLY_MAX};
use crate::responder::{Received, Responder};

/// Erase units of the NOR (MX25L6433F SE 0x20 and BE 0xD8); the board's
/// QSPI driver re-exports these, so they are defined once (Q11).
pub const SECTOR_BYTES: u32 = 4096;
pub const BLOCK_BYTES: u32 = 65536;

const HEADER: u32 = HEADER_LEN as u32;

/// The NOR as the loader needs it. Addresses are NOR offsets.
pub trait Nor {
    type Error: Copy + core::fmt::Debug;
    /// The NOR's size in bytes (`HelloReply.flash_len`), a multiple of
    /// [`BLOCK_BYTES`].
    fn size(&self) -> u32;
    /// Erases the 4 KiB sector at `addr` (a multiple of [`SECTOR_BYTES`]).
    fn erase_4k(&mut self, addr: u32) -> Result<(), Self::Error>;
    /// Erases the 64 KiB block at `addr` (a multiple of [`BLOCK_BYTES`]).
    fn erase_64k(&mut self, addr: u32) -> Result<(), Self::Error>;
    /// Programs erased bytes at `addr` (any alignment and length).
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), Self::Error>;
    /// Calls `f` with the whole NOR as read through the memory-mapped
    /// window ([`Nor::size`] bytes), and leaves memory-mapped mode after.
    fn with_window<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Result<R, Self::Error>;
}

/// What the loader has done since reset, for the status line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counters {
    /// Frames that decoded (duplicates included).
    pub frames: u32,
    /// Frames that did not decode (answered `Nak`).
    pub bad_frames: u32,
    /// Resends answered from the stored reply.
    pub duplicates: u32,
    pub hellos: u32,
    /// `RomBegin`s that started a load (erased block 0).
    pub begins: u32,
    pub already_loaded: u32,
    pub chunks: u32,
    pub ends: u32,
    /// `RomEnd`s whose read-back hash matched and whose image validated.
    pub loads_ok: u32,
    /// `RomEnd`s whose read-back hash did not match, or whose image did
    /// not validate after the header was programmed.
    pub verify_failures: u32,
    /// `Nak`s sent for decoded frames (not counting replays).
    pub naks: u32,
    /// Replies that did not fit the reply buffer, so nothing was sent
    /// (never expected: [`REPLY_MAX`] holds every reply).
    pub replies_dropped: u32,
    pub erases_64k: u32,
    pub erases_4k: u32,
    pub flash_errors: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Loading {
        len: u32,
        hash: u64,
        /// ROM bytes received so far (the next chunk's offset).
        next: u32,
        /// NOR offsets `0..erased` are erased in this load (or programmed
        /// since): always a multiple of [`SECTOR_BYTES`].
        erased: u32,
    },
}

/// One answered frame, from [`Loader::feed`].
#[derive(Clone, Copy, Debug)]
pub struct Reply<'a> {
    /// The reply as on the wire, delimiter included: send all of it.
    pub wire: &'a [u8],
    /// The request's type code; `None` for a frame that did not decode.
    pub request: Option<u8>,
    /// A resend, answered from the stored reply.
    pub duplicate: bool,
}

/// Where a load stands, for the status line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Progress {
    Idle,
    Loading { received: u32, len: u32 },
}

pub struct Loader<N: Nor> {
    pub nor: N,
    pub counters: Counters,
    /// The last NOR error, for the status line.
    pub last_flash_error: Option<N::Error>,
    state: State,
    link: Responder,
}

impl<N: Nor> Loader<N> {
    pub fn new(nor: N) -> Self {
        Self {
            nor,
            counters: Counters::default(),
            last_flash_error: None,
            state: State::Idle,
            link: Responder::new(),
        }
    }

    pub fn progress(&self) -> Progress {
        match self.state {
            State::Idle => Progress::Idle,
            State::Loading { len, next, .. } => Progress::Loading {
                received: next,
                len,
            },
        }
    }

    /// The stored image's header (no data hash): cheap, for status lines.
    pub fn stored_header(&mut self) -> Result<Result<ImageHeader, ImageError>, N::Error> {
        self.nor.with_window(rom_image::read_header)
    }

    /// The stored image checked in full (header and data hash).
    pub fn validate_stored(&mut self) -> Result<Result<ImageHeader, ImageError>, N::Error> {
        self.nor.with_window(validated_header)
    }

    /// Feeds one received byte. When it ends a frame, the frame is
    /// answered and the reply returned (send all of `wire`).
    pub fn feed(&mut self, b: u8) -> Option<Reply<'_>> {
        let req = match self.link.feed(b)? {
            Received::BadFrame { nak } => {
                self.counters.bad_frames += 1;
                return Some(Reply {
                    wire: sent(nak, &mut self.counters),
                    request: None,
                    duplicate: false,
                });
            }
            Received::Request(req) => req,
        };
        self.counters.frames += 1;
        let ty = req.msg.type_code();
        let req = match req.resend() {
            Ok(wire) => {
                self.counters.duplicates += 1;
                return Some(Reply {
                    wire,
                    request: Some(ty),
                    duplicate: true,
                });
            }
            Err(req) => req,
        };
        let mut s = Session {
            nor: &mut self.nor,
            state: &mut self.state,
            counters: &mut self.counters,
            last_flash_error: &mut self.last_flash_error,
        };
        let answer = s.handle(req.msg);
        if let Message::Nak { .. } = answer {
            self.counters.naks += 1;
        }
        let wire = req.answer(&answer);
        Some(Reply {
            wire: sent(wire, &mut self.counters),
            request: Some(ty),
            duplicate: false,
        })
    }
}

/// A reply's wire, or nothing (counted in [`Counters::replies_dropped`])
/// if it did not fit the reply buffer.
fn sent<'a>(wire: Option<&'a [u8]>, counters: &mut Counters) -> &'a [u8] {
    wire.unwrap_or_else(|| {
        counters.replies_dropped += 1;
        &[]
    })
}

/// `validate` and the header it checked.
fn validated_header(window: &[u8]) -> Result<ImageHeader, ImageError> {
    rom_image::validate(window)?;
    rom_image::read_header(window)
}

/// The request handlers, over the loader's fields (the decoder still
/// borrows the request's data).
struct Session<'a, N: Nor> {
    nor: &'a mut N,
    state: &'a mut State,
    counters: &'a mut Counters,
    last_flash_error: &'a mut Option<N::Error>,
}

const fn nak(code: NakCode) -> Message<'static> {
    Message::Nak { code }
}

impl<N: Nor> Session<'_, N> {
    fn handle(&mut self, msg: Message<'_>) -> Message<'static> {
        match msg {
            Message::Hello { .. } => {
                self.counters.hellos += 1;
                *self.state = State::Idle;
                Message::HelloReply {
                    protocol: PROTOCOL_VERSION,
                    max_chunk: MAX_CHUNK,
                    flash_len: self.nor.size(),
                }
            }
            Message::RomBegin { len, hash } => self.begin(len, hash),
            Message::RomChunk { offset, data } => self.chunk(offset, data),
            Message::RomEnd => self.end(),
            _ => nak(NakCode::UnexpectedType),
        }
    }

    /// Records a NOR error and drops the load.
    fn flash_error(&mut self, e: N::Error) -> Message<'static> {
        self.counters.flash_errors += 1;
        *self.last_flash_error = Some(e);
        *self.state = State::Idle;
        nak(NakCode::FlashError)
    }

    fn begin(&mut self, len: u32, hash: u64) -> Message<'static> {
        *self.state = State::Idle;
        if len == 0 || len > self.nor.size().saturating_sub(HEADER) {
            return nak(NakCode::OutOfRange);
        }
        match self.nor.with_window(validated_header) {
            Err(e) => return self.flash_error(e),
            Ok(Ok(h)) if h.len == len && h.hash == hash => {
                self.counters.already_loaded += 1;
                return Message::AlreadyLoaded;
            }
            Ok(_) => {}
        }
        // The header's block first: from here the NOR holds no image.
        if let Err(e) = self.nor.erase_64k(0) {
            return self.flash_error(e);
        }
        self.counters.erases_64k += 1;
        self.counters.begins += 1;
        *self.state = State::Loading {
            len,
            hash,
            next: 0,
            erased: BLOCK_BYTES,
        };
        Message::Ack
    }

    fn chunk(&mut self, offset: u32, data: &[u8]) -> Message<'static> {
        let State::Loading {
            len,
            hash,
            next,
            mut erased,
        } = *self.state
        else {
            return nak(NakCode::UnexpectedType);
        };
        let end = u64::from(offset) + data.len() as u64;
        if offset != next || end > u64::from(len) || data.len() > usize::from(MAX_CHUNK) {
            return nak(NakCode::OutOfRange);
        }
        // `end <= len <= size - HEADER`, so these fit.
        let start = HEADER + offset;
        let stop = HEADER + end as u32;
        let limit = (HEADER + len).next_multiple_of(SECTOR_BYTES);
        while erased < stop {
            let r = if erased.is_multiple_of(BLOCK_BYTES) && erased + BLOCK_BYTES <= limit {
                self.counters.erases_64k += 1;
                self.nor.erase_64k(erased).map(|()| BLOCK_BYTES)
            } else {
                self.counters.erases_4k += 1;
                self.nor.erase_4k(erased).map(|()| SECTOR_BYTES)
            };
            match r {
                Ok(n) => erased += n,
                Err(e) => return self.flash_error(e),
            }
        }
        if let Err(e) = self.nor.program(start, data) {
            return self.flash_error(e);
        }
        self.counters.chunks += 1;
        *self.state = State::Loading {
            len,
            hash,
            next: end as u32,
            erased,
        };
        Message::Ack
    }

    fn end(&mut self) -> Message<'static> {
        let State::Loading {
            len, hash, next, ..
        } = *self.state
        else {
            return nak(NakCode::UnexpectedType);
        };
        if next != len {
            return nak(NakCode::OutOfRange);
        }
        *self.state = State::Idle;
        self.counters.ends += 1;
        let size = self.nor.size() as usize;
        let data = HEADER_LEN..HEADER_LEN + len as usize;
        // The header for the data as read back: its hash is the read-back
        // hash, and it is what gets programmed if that matches.
        let header = match self.nor.with_window(|w| {
            w.get(data)
                .map_or(Err(ImageError::LengthOutOfRange), rom_image::encode_header)
        }) {
            Err(e) => return self.flash_error(e),
            Ok(Err(_)) => return nak(NakCode::OutOfRange),
            Ok(Ok(h)) => h,
        };
        let Ok(parsed) = rom_image::parse_header(&header, size) else {
            return nak(NakCode::OutOfRange);
        };
        let read_back = parsed.hash;
        if read_back != hash {
            self.counters.verify_failures += 1;
            return Message::RomDone { hash: read_back };
        }
        if let Err(e) = self.nor.program(0, &header) {
            return self.flash_error(e);
        }
        match self.nor.with_window(validated_header) {
            Err(e) => self.flash_error(e),
            Ok(Ok(h)) if h.len == len && h.hash == hash => {
                self.counters.loads_ok += 1;
                Message::RomDone { hash: read_back }
            }
            Ok(_) => {
                self.counters.verify_failures += 1;
                nak(NakCode::FlashError)
            }
        }
    }
}
