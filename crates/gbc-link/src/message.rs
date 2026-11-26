//! The v1 message set and its body codec. See the crate docs for the
//! wire table.

use crate::{DecodeError, EncodeError, MAX_CHUNK, MAX_STATS_EXTRA};

/// Message type codes. 0x01..=0x7F are PC -> board, 0x80..=0xFF board ->
/// PC; within each half, groups of 16 by purpose leave room for later
/// messages. 0x00 is never a valid type. `SAVE_CHUNK` goes both ways
/// (board -> PC as a save dump) and sits in the PC -> board half.
pub mod ty {
    pub const HELLO: u8 = 0x01;
    pub const ROM_BEGIN: u8 = 0x10;
    pub const ROM_CHUNK: u8 = 0x11;
    pub const ROM_END: u8 = 0x12;
    pub const SAVE_BEGIN: u8 = 0x20;
    pub const SAVE_CHUNK: u8 = 0x21;
    pub const SAVE_DUMP_REQUEST: u8 = 0x22;
    pub const INPUT: u8 = 0x30;
    pub const ACK: u8 = 0x80;
    pub const NAK: u8 = 0x81;
    pub const HELLO_REPLY: u8 = 0x82;
    pub const ALREADY_LOADED: u8 = 0x90;
    pub const ROM_DONE: u8 = 0x91;
    pub const STATS: u8 = 0xA0;
}

/// Why the board refused a request. Codes 1..=8 are assigned; anything
/// else (a newer peer's code) decodes to `Other` rather than failing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NakCode {
    /// The frame's CRC-32 did not match.
    BadCrc,
    /// Not valid COBS, too short, too long, or a body of the wrong length
    /// for its type.
    BadFrame,
    /// An unknown type, or a known one that is not valid now (e.g. a
    /// `RomChunk` before `RomBegin`).
    UnexpectedType,
    /// A length or offset outside what the board can store or expected.
    OutOfRange,
    /// Erase, program or read-back of the flash failed.
    FlashError,
    /// The programmed image's hash did not match `RomBegin`'s.
    HashMismatch,
    /// The board cannot take the request now; retry later.
    Busy,
    /// `Hello` asked for a protocol version the board does not speak.
    VersionMismatch,
    /// An unassigned code (from a newer peer). Never 1..=8.
    Other(u8),
}

impl NakCode {
    pub const fn to_u8(self) -> u8 {
        match self {
            Self::BadCrc => 1,
            Self::BadFrame => 2,
            Self::UnexpectedType => 3,
            Self::OutOfRange => 4,
            Self::FlashError => 5,
            Self::HashMismatch => 6,
            Self::Busy => 7,
            Self::VersionMismatch => 8,
            Self::Other(c) => c,
        }
    }

    pub const fn from_u8(c: u8) -> Self {
        match c {
            1 => Self::BadCrc,
            2 => Self::BadFrame,
            3 => Self::UnexpectedType,
            4 => Self::OutOfRange,
            5 => Self::FlashError,
            6 => Self::HashMismatch,
            7 => Self::Busy,
            8 => Self::VersionMismatch,
            c => Self::Other(c),
        }
    }
}

/// One protocol message. Chunk data borrows from the decoder's buffer (or
/// the sender's ROM), so nothing is copied or allocated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Message<'a> {
    // --- PC -> board ---
    /// Opens a session; `protocol` is the PC's [`crate::PROTOCOL_VERSION`].
    Hello { protocol: u16 },
    /// Starts a ROM load: total length and FNV-1a 64 of the ROM
    /// (ADR 0037). Answered `AlreadyLoaded` if the stored image matches.
    RomBegin { len: u32, hash: u64 },
    /// ROM bytes at `offset`; `data.len() <= MAX_CHUNK`.
    RomChunk { offset: u32, data: &'a [u8] },
    /// All chunks sent; the board verifies and answers `RomDone`.
    RomEnd,
    /// Starts a save (cartridge RAM) load (M5).
    SaveBegin { len: u32, hash: u64 },
    /// Save bytes at `offset`: PC -> board for a load, board -> PC for a
    /// dump (M5).
    SaveChunk { offset: u32, data: &'a [u8] },
    /// Asks the board to dump its save as `SaveChunk`s (M5).
    SaveDumpRequest,
    /// Held buttons ([`crate::buttons`]), bit layout as `gbc_core::Buttons` (M5): bit 0 A,
    /// 1 B, 2 Select, 3 Start, 4 Right, 5 Left, 6 Up, 7 Down; 1 = pressed.
    Input { buttons: u8 },
    // --- board -> PC ---
    /// The request with this frame's `seq` was accepted.
    Ack,
    /// The request with this frame's `seq` was refused.
    Nak { code: NakCode },
    /// Answer to `Hello`: the board's protocol version, the largest chunk
    /// data it accepts, and the ROM capacity of its flash in bytes.
    HelloReply {
        protocol: u16,
        max_chunk: u16,
        flash_len: u32,
    },
    /// Answer to `RomBegin`: the stored image already has this length and
    /// hash; the PC skips the load.
    AlreadyLoaded,
    /// Answer to `RomEnd`: the programmed image, read back, has `hash`.
    RomDone { hash: u64 },
    /// Per-frame timing stats. `extra` is reserved for fields added later
    /// (empty in v1; `len <= MAX_STATS_EXTRA`); a v1 reader ignores it.
    Stats {
        frames: u32,
        cycles_mean: u32,
        cycles_max: u32,
        extra: &'a [u8],
    },
}

/// Where body bytes go when encoding (the COBS + CRC writer).
pub(crate) trait Sink {
    fn put(&mut self, b: u8) -> Result<(), EncodeError>;

    fn put_all(&mut self, data: &[u8]) -> Result<(), EncodeError> {
        for &b in data {
            self.put(b)?;
        }
        Ok(())
    }
}

impl Message<'_> {
    pub const fn type_code(&self) -> u8 {
        match self {
            Self::Hello { .. } => ty::HELLO,
            Self::RomBegin { .. } => ty::ROM_BEGIN,
            Self::RomChunk { .. } => ty::ROM_CHUNK,
            Self::RomEnd => ty::ROM_END,
            Self::SaveBegin { .. } => ty::SAVE_BEGIN,
            Self::SaveChunk { .. } => ty::SAVE_CHUNK,
            Self::SaveDumpRequest => ty::SAVE_DUMP_REQUEST,
            Self::Input { .. } => ty::INPUT,
            Self::Ack => ty::ACK,
            Self::Nak { .. } => ty::NAK,
            Self::HelloReply { .. } => ty::HELLO_REPLY,
            Self::AlreadyLoaded => ty::ALREADY_LOADED,
            Self::RomDone { .. } => ty::ROM_DONE,
            Self::Stats { .. } => ty::STATS,
        }
    }

    /// Checks the variable-length fields before anything is written.
    pub(crate) fn check(&self) -> Result<(), EncodeError> {
        match self {
            Self::RomChunk { data, .. } | Self::SaveChunk { data, .. }
                if data.len() > MAX_CHUNK =>
            {
                Err(EncodeError::BodyTooLarge)
            }
            Self::Stats { extra, .. } if extra.len() > MAX_STATS_EXTRA => {
                Err(EncodeError::BodyTooLarge)
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn write_body(&self, s: &mut impl Sink) -> Result<(), EncodeError> {
        match *self {
            Self::Hello { protocol } => s.put_all(&protocol.to_le_bytes()),
            Self::RomBegin { len, hash } | Self::SaveBegin { len, hash } => {
                s.put_all(&len.to_le_bytes())?;
                s.put_all(&hash.to_le_bytes())
            }
            Self::RomChunk { offset, data } | Self::SaveChunk { offset, data } => {
                s.put_all(&offset.to_le_bytes())?;
                s.put_all(data)
            }
            Self::RomEnd | Self::SaveDumpRequest | Self::Ack | Self::AlreadyLoaded => Ok(()),
            Self::Input { buttons } => s.put(buttons),
            Self::Nak { code } => s.put(code.to_u8()),
            Self::HelloReply {
                protocol,
                max_chunk,
                flash_len,
            } => {
                s.put_all(&protocol.to_le_bytes())?;
                s.put_all(&max_chunk.to_le_bytes())?;
                s.put_all(&flash_len.to_le_bytes())
            }
            Self::RomDone { hash } => s.put_all(&hash.to_le_bytes()),
            Self::Stats {
                frames,
                cycles_mean,
                cycles_max,
                extra,
            } => {
                s.put_all(&frames.to_le_bytes())?;
                s.put_all(&cycles_mean.to_le_bytes())?;
                s.put_all(&cycles_max.to_le_bytes())?;
                s.put_all(extra)
            }
        }
    }
}

/// Little-endian field reader over a body; every read is checked.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take<const K: usize>(&mut self) -> Option<[u8; K]> {
        let (head, rest) = self.0.split_first_chunk::<K>()?;
        self.0 = rest;
        Some(*head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take::<1>().map(|[b]| b)
    }
    fn u16(&mut self) -> Option<u16> {
        self.take().map(u16::from_le_bytes)
    }
    fn u32(&mut self) -> Option<u32> {
        self.take().map(u32::from_le_bytes)
    }
    fn u64(&mut self) -> Option<u64> {
        self.take().map(u64::from_le_bytes)
    }
    /// The remaining bytes, at most `max` of them.
    fn rest(&mut self, max: usize) -> Option<&'a [u8]> {
        let r = self.0;
        self.0 = &[];
        (r.len() <= max).then_some(r)
    }
    /// The body must be fully consumed.
    fn end(&self) -> Option<()> {
        self.0.is_empty().then_some(())
    }
}

/// Parses a body of type `t` (from frame `seq`, for the error).
pub(crate) fn parse(t: u8, seq: u8, body: &[u8]) -> Result<Message<'_>, DecodeError> {
    let mut r = Reader(body);
    let msg = match t {
        ty::HELLO => r.u16().map(|protocol| Message::Hello { protocol }),
        ty::ROM_BEGIN | ty::SAVE_BEGIN => r.u32().and_then(|len| {
            let hash = r.u64()?;
            Some(if t == ty::ROM_BEGIN {
                Message::RomBegin { len, hash }
            } else {
                Message::SaveBegin { len, hash }
            })
        }),
        ty::ROM_CHUNK | ty::SAVE_CHUNK => r.u32().and_then(|offset| {
            let data = r.rest(MAX_CHUNK)?;
            Some(if t == ty::ROM_CHUNK {
                Message::RomChunk { offset, data }
            } else {
                Message::SaveChunk { offset, data }
            })
        }),
        ty::ROM_END => Some(Message::RomEnd),
        ty::SAVE_DUMP_REQUEST => Some(Message::SaveDumpRequest),
        ty::INPUT => r.u8().map(|buttons| Message::Input { buttons }),
        ty::ACK => Some(Message::Ack),
        ty::NAK => r.u8().map(|c| Message::Nak {
            code: NakCode::from_u8(c),
        }),
        ty::HELLO_REPLY => r.u16().and_then(|protocol| {
            Some(Message::HelloReply {
                protocol,
                max_chunk: r.u16()?,
                flash_len: r.u32()?,
            })
        }),
        ty::ALREADY_LOADED => Some(Message::AlreadyLoaded),
        ty::ROM_DONE => r.u64().map(|hash| Message::RomDone { hash }),
        ty::STATS => r.u32().and_then(|frames| {
            Some(Message::Stats {
                frames,
                cycles_mean: r.u32()?,
                cycles_max: r.u32()?,
                extra: r.rest(MAX_STATS_EXTRA)?,
            })
        }),
        _ => return Err(DecodeError::UnknownType { ty: t, seq }),
    };
    match (msg, r.end()) {
        (Some(m), Some(())) => Ok(m),
        _ => Err(DecodeError::BadLength { ty: t, seq }),
    }
}
