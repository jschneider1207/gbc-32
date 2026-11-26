//! The serial protocol between the PC tool (`tools/gbc-pc`) and the board
//! firmware (Stage 2 M2; plan `docs/plan/stage-2.md`, ADR 0036). It is
//! transport-agnostic: callers move bytes over a UART, USB serial or a
//! test vector, and own timeouts. `no_std`, no `alloc`, no `unsafe`, no
//! dependencies (D9), and no panics on any input.
//!
//! # Frame
//!
//! ```text
//! on wire:  COBS(payload) 0x00
//! payload:  type u8 | seq u8 | body (0..=MAX_BODY bytes) | crc u32 LE
//! crc:      CRC-32 (IEEE 802.3: reflected, poly 0xEDB88320, init and
//!           xorout 0xFFFFFFFF) over type, seq and body
//! ```
//!
//! COBS removes every 0x00 from the payload, so 0x00 only ever ends a
//! frame and a receiver resyncs at the next one after garbage, a lost
//! byte or an oversize frame. Empty frames (back-to-back 0x00s) are
//! ignored, so a sender may lead with a 0x00 to flush a half-received
//! frame at the other end. All integers are little-endian.
//!
//! **Reliability: stop-and-wait.** The PC sends one request with a `seq`
//! (incremented per new request, wrapping) and waits. The board answers
//! with the same `seq`: `Ack`, `Nak`, or the request's specific reply
//! (`HelloReply`, `AlreadyLoaded`, `RomDone`). The PC resends the same
//! frame, same `seq`, on `Nak` or timeout; the board should treat a repeat
//! of the `seq` it last answered as a duplicate (re-send the answer,
//! don't redo the work). A frame that fails COBS or CRC has no
//! trustworthy `seq` ([`DecodeError::seq`] is `None`): the board answers
//! `Nak` with any `seq`, and a waiting PC resends its outstanding frame on
//! any `Nak`, whatever its `seq`.
//!
//! # Messages (v1)
//!
//! | code | message           | dir  | body (LE)                                   | bytes |
//! |------|-------------------|------|---------------------------------------------|-------|
//! | 0x01 | `Hello`           | PC→B | protocol u16                                | 2 |
//! | 0x10 | `RomBegin`        | PC→B | len u32, hash u64 (FNV-1a 64, ADR 0037)     | 12 |
//! | 0x11 | `RomChunk`        | PC→B | offset u32, data                            | 4 + 0..=1024 |
//! | 0x12 | `RomEnd`          | PC→B | —                                           | 0 |
//! | 0x20 | `SaveBegin`       | PC→B | len u32, hash u64 (M5)                      | 12 |
//! | 0x21 | `SaveChunk`       | both | offset u32, data (load PC→B, dump B→PC; M5) | 4 + 0..=1024 |
//! | 0x22 | `SaveDumpRequest` | PC→B | — (M5)                                      | 0 |
//! | 0x30 | `Input`           | PC→B | buttons u8 (`gbc_core::Buttons` bits; M5)   | 1 |
//! | 0x80 | `Ack`             | B→PC | —                                           | 0 |
//! | 0x81 | `Nak`             | B→PC | code u8 ([`NakCode`])                       | 1 |
//! | 0x82 | `HelloReply`      | B→PC | protocol u16, max_chunk u16, flash_len u32  | 8 |
//! | 0x90 | `AlreadyLoaded`   | B→PC | —                                           | 0 |
//! | 0x91 | `RomDone`         | B→PC | hash u64 (of the image read back)           | 8 |
//! | 0xA0 | `Stats`           | B→PC | frames u32, cycles_mean u32, cycles_max u32, extra | 12 + 0..=1016 |
//!
//! Code ranges: 0x00 invalid; 0x01..=0x0F session, 0x10..=0x1F ROM,
//! 0x20..=0x2F save, 0x30..=0x3F input (PC→B); 0x80..=0x8F replies,
//! 0x90..=0x9F ROM results, 0xA0..=0xAF stats (B→PC). The rest is free.
//! An unknown code is [`DecodeError::UnknownType`]; a known code with a
//! body of the wrong length is [`DecodeError::BadLength`] (fixed-size
//! bodies must match exactly). `Stats.extra` is room for later fields; a
//! v1 reader ignores it.
//!
//! `Nak` codes: 1 BadCrc, 2 BadFrame, 3 UnexpectedType, 4 OutOfRange,
//! 5 FlashError, 6 HashMismatch, 7 Busy, 8 VersionMismatch; others decode
//! as [`NakCode::Other`].
//!
//! Sizes: chunk data up to [`MAX_CHUNK`] = 1,024 bytes (4 NOR pages), so
//! the largest body is [`MAX_BODY`] = 1,028 (offset + data), the largest
//! payload 1,034 and the largest frame on the wire [`MAX_FRAME`] = 1,040
//! bytes including the delimiter.

#![no_std]
#![forbid(unsafe_code)]
// Untrusted input must never panic: no indexing, unwrap or panics outside
// tests (everything goes through `get` / `Option`).
#![cfg_attr(
    not(test),
    deny(
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic
    )
)]

pub mod cobs;

/// The `Message::Input { buttons }` bitmap (M5): one bit per Game Boy
/// button, 1 = pressed, the layout of `gbc_core::Buttons` (the P1 register
/// halves: action buttons low, D-pad high). This crate has no dependencies
/// (D9), so the match is checked where both are visible:
/// `tools/gbc-pc/tests/buttons.rs`, against `gbc_firmware::buttons::from_pc`.
pub mod buttons {
    pub const A: u8 = 1 << 0;
    pub const B: u8 = 1 << 1;
    pub const SELECT: u8 = 1 << 2;
    pub const START: u8 = 1 << 3;
    pub const RIGHT: u8 = 1 << 4;
    pub const LEFT: u8 = 1 << 5;
    pub const UP: u8 = 1 << 6;
    pub const DOWN: u8 = 1 << 7;
}
mod crc;
mod message;

pub use crc::{Crc32, crc32};
pub use message::{Message, NakCode, ty};

/// The protocol version this crate speaks (`Hello`/`HelloReply`).
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest data length of a `RomChunk` or `SaveChunk`.
pub const MAX_CHUNK: usize = 1024;

/// Largest message body: a chunk's offset plus [`MAX_CHUNK`] data bytes.
pub const MAX_BODY: usize = 4 + MAX_CHUNK;

/// Largest `Stats.extra`: the rest of [`MAX_BODY`] after its 12 bytes.
pub const MAX_STATS_EXTRA: usize = MAX_BODY - 12;

/// Largest decoded payload: type, seq, body, CRC.
pub const MAX_PAYLOAD: usize = 2 + MAX_BODY + 4;

/// Largest COBS-encoded payload (no delimiter): the buffer a [`Decoder`]
/// needs to accept every valid frame.
pub const MAX_ENCODED: usize = cobs::max_encoded_len(MAX_PAYLOAD);

/// Largest frame on the wire, delimiter included: the `out` buffer
/// [`encode`] needs for any message.
pub const MAX_FRAME: usize = MAX_ENCODED + 1;

/// One decoded frame: `(seq, message)`, or why it was dropped.
pub type Decoded<'a> = Result<(u8, Message<'a>), DecodeError>;

/// A decoder sized for every valid v1 frame.
pub type FrameDecoder = Decoder<MAX_ENCODED>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncodeError {
    /// `out` is too small for this frame ([`MAX_FRAME`] always fits).
    BufferTooSmall,
    /// Chunk data over [`MAX_CHUNK`] or `Stats.extra` over
    /// [`MAX_STATS_EXTRA`].
    BodyTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// More bytes than the decoder's buffer before a 0x00; dropped.
    Oversize,
    /// Not valid COBS.
    BadCobs,
    /// Shorter than type + seq + CRC.
    TooShort,
    /// The CRC-32 does not match.
    BadCrc,
    /// CRC good, but the type code is not a v1 message.
    UnknownType { ty: u8, seq: u8 },
    /// CRC good, but the body length is wrong for the type.
    BadLength { ty: u8, seq: u8 },
}

impl DecodeError {
    /// The `Nak` code a board answers this error with.
    pub const fn nak_code(&self) -> NakCode {
        match self {
            Self::BadCrc => NakCode::BadCrc,
            Self::UnknownType { .. } => NakCode::UnexpectedType,
            Self::Oversize | Self::BadCobs | Self::TooShort | Self::BadLength { .. } => {
                NakCode::BadFrame
            }
        }
    }

    /// The frame's `seq`, when the CRC vouches for it.
    pub const fn seq(&self) -> Option<u8> {
        match self {
            Self::UnknownType { seq, .. } | Self::BadLength { seq, .. } => Some(*seq),
            _ => None,
        }
    }
}

/// COBS writer that also feeds the CRC.
struct FrameWriter<'o> {
    cobs: cobs::Writer<'o>,
    crc: Crc32,
}

impl message::Sink for FrameWriter<'_> {
    // Out of line: inlined at every field it made `encode` ~4 KiB of
    // Thumb code; a call per byte costs nothing next to UART time.
    #[inline(never)]
    fn put(&mut self, b: u8) -> Result<(), EncodeError> {
        self.crc.update(b);
        self.cobs.push(b).ok_or(EncodeError::BufferTooSmall)
    }
}

/// Encodes `msg` as frame `seq` into `out`: the full on-wire bytes,
/// trailing 0x00 included. Returns the length written. Nothing past it is
/// meaningful on error.
pub fn encode(msg: &Message<'_>, seq: u8, out: &mut [u8]) -> Result<usize, EncodeError> {
    use message::Sink;
    msg.check()?;
    let mut w = FrameWriter {
        cobs: cobs::Writer::new(&mut *out).ok_or(EncodeError::BufferTooSmall)?,
        crc: Crc32::new(),
    };
    w.put(msg.type_code())?;
    w.put(seq)?;
    msg.write_body(&mut w)?;
    for b in w.crc.finish().to_le_bytes() {
        w.cobs.push(b).ok_or(EncodeError::BufferTooSmall)?;
    }
    let n = w.cobs.finish().ok_or(EncodeError::BufferTooSmall)?;
    *out.get_mut(n).ok_or(EncodeError::BufferTooSmall)? = 0;
    Ok(n + 1)
}

/// Streaming frame decoder with a fixed `N`-byte buffer for the encoded
/// frame (use [`FrameDecoder`], `N` = [`MAX_ENCODED`], unless a smaller
/// limit is wanted). Feed it received bytes; at each 0x00 it yields the
/// frame's `(seq, message)` or an error, then starts over. A frame longer
/// than `N` is dropped as [`DecodeError::Oversize`] at its 0x00. The
/// message borrows the buffer until the next feed.
pub struct Decoder<const N: usize> {
    buf: [u8; N],
    len: usize,
    overflow: bool,
}

impl<const N: usize> Default for Decoder<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Decoder<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
            overflow: false,
        }
    }

    /// Discards a partly received frame.
    pub fn reset(&mut self) {
        self.len = 0;
        self.overflow = false;
    }

    /// Buffers `b`; true when it ends a non-empty frame.
    fn push(&mut self, b: u8) -> bool {
        if b != 0 {
            match self.buf.get_mut(self.len) {
                Some(slot) => {
                    *slot = b;
                    self.len += 1;
                }
                None => self.overflow = true,
            }
            false
        } else {
            self.len != 0 || self.overflow
        }
    }

    /// Decodes the buffered frame and resets for the next one.
    fn take_frame(&mut self) -> Decoded<'_> {
        let len = core::mem::take(&mut self.len);
        if core::mem::take(&mut self.overflow) {
            return Err(DecodeError::Oversize);
        }
        let enc = self.buf.get_mut(..len).ok_or(DecodeError::Oversize)?;
        let n = cobs::decode_in_place(enc).ok_or(DecodeError::BadCobs)?;
        let payload = enc.get(..n).ok_or(DecodeError::BadCobs)?;
        let body_end = n.checked_sub(4).ok_or(DecodeError::TooShort)?;
        let (head, crc) = payload
            .split_at_checked(body_end)
            .ok_or(DecodeError::TooShort)?;
        let [t, seq, body @ ..] = head else {
            return Err(DecodeError::TooShort);
        };
        let crc: [u8; 4] = crc.try_into().map_err(|_| DecodeError::TooShort)?;
        if crc32(head) != u32::from_le_bytes(crc) {
            return Err(DecodeError::BadCrc);
        }
        message::parse(*t, *seq, body).map(|m| (*seq, m))
    }

    /// Feeds one byte; `Some` when it completed a frame.
    pub fn feed(&mut self, b: u8) -> Option<Decoded<'_>> {
        if self.push(b) {
            Some(self.take_frame())
        } else {
            None
        }
    }

    /// Feeds bytes until one completes a frame. Returns how many bytes were
    /// consumed and the frame, if any; feed the rest of `data` afterwards.
    pub fn feed_slice(&mut self, data: &[u8]) -> (usize, Option<Decoded<'_>>) {
        for (i, &b) in data.iter().enumerate() {
            if self.push(b) {
                return (i + 1, Some(self.take_frame()));
            }
        }
        (data.len(), None)
    }
}
