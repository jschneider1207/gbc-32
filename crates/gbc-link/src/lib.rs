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
//! | 0x20 | `SaveBegin`       | both | len u32, hash u64 (FNV-1a 64; load PC→B, dump B→PC; M5) | 12 |
//! | 0x21 | `SaveChunk`       | both | offset u32, data (load PC→B, dump B→PC; M5) | 4 + 0..=1024 |
//! | 0x22 | `SaveDumpRequest` | PC→B | — (M5)                                      | 0 |
//! | 0x30 | `Input`           | PC→B | buttons u8 (`gbc_core::Buttons` bits; M5; not answered) | 1 |
//! | 0x40 | `AudioStart`      | PC→B | — (ADR 0041)                                | 0 |
//! | 0x41 | `AudioStop`       | PC→B | — (ADR 0041)                                | 0 |
//! | 0x80 | `Ack`             | B→PC | —                                           | 0 |
//! | 0x81 | `Nak`             | B→PC | code u8 ([`NakCode`])                       | 1 |
//! | 0x82 | `HelloReply`      | B→PC | protocol u16, max_chunk u16, flash_len u32  | 8 |
//! | 0x90 | `AlreadyLoaded`   | B→PC | —                                           | 0 |
//! | 0x91 | `RomDone`         | B→PC | hash u64 (of the image read back)           | 8 |
//! | 0xA0 | `Stats`           | B→PC | frames u32, cycles_mean u32, cycles_max u32, extra | 12 + 0..=1016 |
//! | 0xB0 | `Audio`           | B→PC | first u32, held u16, data (3 bytes per frame; ADR 0041) | 6 + 0..=1020, data a multiple of 3 |
//!
//! Code ranges: 0x00 invalid; 0x01..=0x0F session, 0x10..=0x1F ROM,
//! 0x20..=0x2F save, 0x30..=0x3F input, 0x40..=0x4F audio (PC→B);
//! 0x80..=0x8F replies, 0x90..=0x9F ROM results, 0xA0..=0xAF stats,
//! 0xB0..=0xBF audio (B→PC). The rest is free.
//! An unknown code is [`DecodeError::UnknownType`]; a known code with a
//! body of the wrong length is [`DecodeError::BadLength`] (fixed-size
//! bodies must match exactly). `Stats.extra` is room for later fields; a
//! v1 reader ignores it.
//!
//! # Saves and input (M5)
//!
//! Full description for the board side: `docs/reference/link-protocol.md`.
//!
//! - **Save load** (after the ROM): `SaveBegin { len, hash }` → `Ack`
//!   (`Nak OutOfRange` when `len` is not a valid `.sav` size for the
//!   loaded cartridge, `Nak UnexpectedType` when no game can take one),
//!   then `SaveChunk`s in order from offset 0, each → `Ack`. There is no
//!   end message: the answer to the chunk that completes `len` is the
//!   verdict, after the board has checked the hash and the format: `Ack`
//!   = applied, `Nak HashMismatch` / `Nak OutOfRange` = refused, nothing
//!   applied. A resend of that chunk gets the same verdict (duplicate).
//! - **Save dump**: `SaveDumpRequest` (seq s) → `SaveBegin { len, hash }`
//!   then `SaveChunk`s covering `0..len` in order, all with seq s and
//!   sent without waiting (no per-chunk `Ack`); or `Nak UnexpectedType`
//!   when there is no save. The board snapshots the save at the request.
//!   The PC checks the order and the hash; on a gap, a corrupt frame, a
//!   bad hash or silence it sends a **new** `SaveDumpRequest` (new seq),
//!   and ignores frames still carrying an older seq. A board-started dump
//!   (button combo) is the same `SaveBegin` + chunks with any seq.
//! - **Input**: `Input { buttons }`, a new seq per frame, never answered
//!   (no `Ack`; the stop-and-wait rule does not apply). The PC sends it on
//!   every change and every 500 ms without one, so the latest frame is the
//!   state and a lost one is healed by the next.
//!
//! # Audio to the PC (ADR 0041)
//!
//! Opt-in: the board sends no audio until the PC asks.
//!
//! - `AudioStart` → `Ack`: the board streams every frame its DAC is fed
//!   as `Audio` messages (any seq, never answered), one per DMA refill
//!   (256 frames, 7.8 ms, on the player). `AudioStop` → `Ack` ends it; so
//!   do `Hello` and 90 frames (1.5 s) without any frame from the PC
//!   (`gbc-pc play` repeats `Input` every 500 ms). A board without audio
//!   answers `Nak UnexpectedType`.
//! - `Audio { first, held, format, data }`: `data` holds the **real**
//!   frames the output was fed (not underrun filler) in the sample
//!   `format` ([`audio`]: 1 = 12-bit codes, 3 bytes a frame, what the H753
//!   sends; 2 = `s16le`, 4 bytes; ADR 0044); `first` counts real frames since the board started (wrapping
//!   u32), so the next message's `first` is this one's plus its frame
//!   count unless frames were dropped; `held` is the number of filler
//!   frames (the last frame repeated) the DAC played after them, 0 unless
//!   the board underran. A board that cannot send fast enough drops whole
//!   messages and counts them (the PC sees a jump in `first`); it never
//!   stalls the DAC.
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
/// `Message::Audio` sample formats (ADR 0041, versioned by ADR 0044).
/// Every `Audio` carries its format byte; a frame is one stereo sample
/// pair of signed 16-bit PCM (`gbc_firmware::audio::PcmFrame`: 0 is
/// silence) in one of these encodings:
///
/// - [`U12`] (1): the two 12-bit codes a 12-bit DAC is fed, in 3 bytes,
///   little-endian bit order: bits 0..=11 the left code, 12..=23 the
///   right. `code = clamp(2048 + n/16)`, rounded to nearest with ties up
///   ([`code`]); the receiver gets `(code - 2048) << 4` back ([`sample`]).
///   What the H753 sends: 3 bytes a frame leave the 2 Mbit/s UART room
///   for the rest (ADR 0044, "Bandwidth").
/// - [`S16`] (2): both samples as they are, left then right, each `i16`
///   little-endian (4 bytes, `s16le`), for a board with a 16-bit output
///   and the link to carry it. A DMA half of 256 frames does not fit one
///   message (at most [`max_frames`] = 255); such a board splits it.
///
/// Any other value is a format this version cannot play: [`frame_bytes`]
/// is `None`, the message still decodes (any data length), and the
/// receiver refuses it ([`decode`]).
pub mod audio {
    /// 12-bit codes, 3 bytes a frame (module docs).
    pub const U12: u8 = 1;
    /// `s16le` pairs, 4 bytes a frame.
    pub const S16: u8 = 2;
    /// Bytes per [`U12`] frame.
    pub const U12_BYTES: usize = 3;
    /// Bytes per [`S16`] frame.
    pub const S16_BYTES: usize = 4;

    /// Bytes per frame of `format`, `None` for an unknown one.
    pub const fn frame_bytes(format: u8) -> Option<usize> {
        match format {
            U12 => Some(U12_BYTES),
            S16 => Some(S16_BYTES),
            _ => None,
        }
    }

    /// The most frames one `Audio` of `format` holds (0 if unknown):
    /// [`crate::MAX_AUDIO_BYTES`] / [`frame_bytes`], 340 for [`U12`], 255
    /// for [`S16`].
    pub const fn max_frames(format: u8) -> usize {
        match frame_bytes(format) {
            Some(b) => crate::MAX_AUDIO_BYTES / b,
            None => 0,
        }
    }

    /// The 12-bit code of a sample: `2048 + n/16`, ties up, at most 4095
    /// (`(n + 32,768 + 8) >> 4`; -32,768 gives 0, 32,744 and up 4095).
    /// The same code as `gbc_firmware::audio::u12_lanes` (checked in
    /// `tools/gbc-pc/tests/audio.rs`).
    pub const fn code(n: i16) -> u16 {
        let c = (n as i32 + 32_768 + 8) >> 4;
        if c > 4095 { 4095 } else { c as u16 }
    }

    /// The sample a 12-bit code stands for, `(code - 2048) << 4` (bits
    /// above 11 are ignored); `code(sample(c)) == c`.
    pub const fn sample(code: u16) -> i16 {
        (((code & 0xFFF) as i32 - 2048) << 4) as i16
    }

    /// One frame as [`U12`].
    pub const fn encode_u12(left: i16, right: i16) -> [u8; U12_BYTES] {
        let v = code(left) as u32 | (code(right) as u32) << 12;
        [v as u8, (v >> 8) as u8, (v >> 16) as u8]
    }

    /// The left and right codes of a [`U12`] frame.
    pub const fn codes_u12(b: [u8; U12_BYTES]) -> (u16, u16) {
        let v = b[0] as u32 | (b[1] as u32) << 8 | (b[2] as u32) << 16;
        ((v & 0xFFF) as u16, (v >> 12) as u16)
    }

    /// One frame as [`S16`].
    pub const fn encode_s16(left: i16, right: i16) -> [u8; S16_BYTES] {
        let (l, r) = (left.to_le_bytes(), right.to_le_bytes());
        [l[0], l[1], r[0], r[1]]
    }

    /// An `Audio` format this version does not know.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct UnknownFormat(pub u8);

    /// Calls `each(left, right)` for every whole frame of `data` in
    /// `format`, as 16-bit samples ([`sample`] for [`U12`]); returns the
    /// frame count, or the format if it is unknown (then `each` is never
    /// called). A partial trailing frame (never sent) is ignored.
    pub fn decode(
        format: u8,
        data: &[u8],
        mut each: impl FnMut(i16, i16),
    ) -> Result<usize, UnknownFormat> {
        match format {
            U12 => {
                let (frames, _) = data.as_chunks::<U12_BYTES>();
                for &f in frames {
                    let (l, r) = codes_u12(f);
                    each(sample(l), sample(r));
                }
                Ok(frames.len())
            }
            S16 => {
                let (frames, _) = data.as_chunks::<S16_BYTES>();
                for &[a, b, c, d] in frames {
                    each(i16::from_le_bytes([a, b]), i16::from_le_bytes([c, d]));
                }
                Ok(frames.len())
            }
            other => Err(UnknownFormat(other)),
        }
    }
}
mod crc;
mod message;

pub use crc::{Crc32, crc32};
pub use message::{Message, NakCode, ty};

/// The link's default rate in bit/s: the board images' (loader, player,
/// link-probe; each overridable at build time, [`parse_baud`]) and
/// `gbc-pc`'s `--baud` default. USARTDIV 59 at the 118 MHz USART clock,
/// exact (`docs/reference/loader.md` "Baud rate"). The shell scripts that
/// pass it (`scripts/play-on-board.sh`, `crates/gbc-nucleo-h753/run-*.sh`)
/// carry the literal; `tests/default_baud.rs` checks they agree.
pub const DEFAULT_BAUD: u32 = 2_000_000;

/// A baud rate from a build-time variable (`option_env!("GBC_LINK_BAUD")`
/// and the like): `default` when unset; a panic (a compile error in a
/// `const`) when empty or not a decimal number.
pub const fn parse_baud(s: Option<&str>, default: u32) -> u32 {
    let Some(s) = s else { return default };
    let mut b = s.as_bytes();
    assert!(!b.is_empty(), "the baud variable is empty");
    let mut v: u32 = 0;
    while let [d, rest @ ..] = b {
        assert!(d.is_ascii_digit(), "the baud variable is not a number");
        v = v * 10 + (*d - b'0') as u32;
        b = rest;
    }
    v
}

/// The protocol version this crate speaks (`Hello`/`HelloReply`).
/// 2: `Audio` carries a sample format (ADR 0044).
pub const PROTOCOL_VERSION: u16 = 2;

/// Largest data length of a `RomChunk` or `SaveChunk`.
pub const MAX_CHUNK: usize = 1024;

/// Largest `Audio.data`: 1,020 bytes, what fits [`MAX_BODY`] after its 7
/// bytes, cut to a multiple of 12 so it is whole frames in every format
/// (340 [`audio::U12`] or 255 [`audio::S16`]; [`audio::max_frames`]).
pub const MAX_AUDIO_BYTES: usize = (MAX_BODY - 7) / 12 * 12;
const _: () = assert!(MAX_AUDIO_BYTES == 1020);

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

    // A field's bytes in one go: the CRC four bytes a step, COBS a zero
    // run at a time (review P3; the player streams ~550 audio frames,
    // 1.6 kB, per video frame through here). Out of line for the same
    // code-size reason as `put`.
    #[inline(never)]
    fn put_all(&mut self, data: &[u8]) -> Result<(), EncodeError> {
        self.crc.update_slice(data);
        self.cobs
            .push_slice(data)
            .ok_or(EncodeError::BufferTooSmall)
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
    w.cobs
        .push_slice(&w.crc.finish().to_le_bytes())
        .ok_or(EncodeError::BufferTooSmall)?;
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

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    /// The frame encoder before review P3, rebuilt from its parts: every
    /// body byte through `put` alone (the trait's per-byte `put_all`), the
    /// nibble-table CRC, and a per-byte COBS `push` loop.
    fn reference_encode(msg: &Message<'_>, seq: u8) -> Vec<u8> {
        struct Bytes(Vec<u8>);
        impl message::Sink for Bytes {
            fn put(&mut self, b: u8) -> Result<(), EncodeError> {
                self.0.push(b);
                Ok(())
            }
        }
        let mut p = Bytes(std::vec![msg.type_code(), seq]);
        msg.write_body(&mut p).unwrap();
        let crc = crc::reference_crc32(&p.0);
        p.0.extend_from_slice(&crc.to_le_bytes());
        let mut out = std::vec![0u8; MAX_FRAME];
        let mut w = cobs::Writer::new(&mut out).unwrap();
        for &b in &p.0 {
            w.push(b).unwrap();
        }
        let n = w.finish().unwrap();
        out.truncate(n);
        out.push(0);
        out
    }

    /// `encode` is byte-identical to the pre-P3 encoder for `Audio`,
    /// `SaveChunk` and `Stats` frames of every data length the protocol
    /// allows, at several zero densities, with random header fields (so
    /// zeros in `first`/`held`/`offset` move the block edges too).
    #[test]
    fn encode_matches_reference_encoder() {
        let mut rng = crc::TestRng(0x0123_4567_89AB_CDEF);
        let mut data = [0u8; MAX_CHUNK];
        let mut out = [0u8; MAX_FRAME];
        for zero in [0, 1, 64, 256] {
            for len in 0..=MAX_CHUNK {
                rng.bytes(len, zero, &mut data);
                let d = &data[..len];
                let r = rng.next();
                let seq = (r >> 56) as u8;
                let mut msgs = std::vec![
                    Message::SaveChunk {
                        offset: (r as u32) & 0x00FF_FF00,
                        data: d
                    },
                    Message::Audio {
                        first: (r >> 8) as u32,
                        held: (r >> 40) as u16 & 0xFF0F,
                        format: audio::U12,
                        data: d.get(..len.min(MAX_AUDIO_BYTES) / 3 * 3).unwrap(),
                    },
                ];
                if len <= MAX_STATS_EXTRA {
                    msgs.push(Message::Stats {
                        frames: r as u32,
                        cycles_mean: (r >> 16) as u32,
                        cycles_max: 0,
                        extra: d,
                    });
                }
                for m in &msgs {
                    let want = reference_encode(m, seq);
                    let n = encode(m, seq, &mut out).unwrap();
                    assert_eq!(&out[..n], &want[..], "len {len} zero {zero} {m:?}");
                }
            }
        }
    }
}
