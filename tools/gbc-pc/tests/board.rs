//! The PC client against the board's real loader session
//! (`crates/gbc-nucleo-h753/src/loader.rs`, compiled here on the host)
//! over a fake NOR that behaves like one: erase sets 0xFF, program can
//! only clear bits, and programming a byte that is not erased is counted
//! as an error. `tests/load.rs` runs the same client against the
//! simulated board (`gbc_pc::sim`); this checks that the firmware's own
//! logic agrees with it, and the NOR-side rules (lazy erase, header last).

#[path = "../../../crates/gbc-nucleo-h753/src/loader.rs"]
#[allow(dead_code)]
mod loader;

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use gbc_firmware::rom_image::{self, ImageError};
use gbc_link::{FrameDecoder, Message, NakCode};
use gbc_pc::sim::encode;
use gbc_pc::{Client, Config, Error, LoadOutcome, Transport, fnv};
use loader::{BLOCK_BYTES, Loader, Nor, SECTOR_BYTES};

const FLASH: usize = 8 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Erase4k(u32),
    Erase64k(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FakeError;

struct FakeNor {
    flash: Vec<u8>,
    ops: Vec<Op>,
    /// Bytes programmed that were not 0xFF before (a missed erase).
    unerased_writes: u32,
    /// Flips bit 0 of this NOR offset when it is programmed.
    corrupt_at: Option<u32>,
    /// Erases of this address fail.
    fail_erase_at: Option<u32>,
}

impl FakeNor {
    fn new() -> Self {
        Self {
            flash: vec![0xFF; FLASH],
            ops: Vec::new(),
            unerased_writes: 0,
            corrupt_at: None,
            fail_erase_at: None,
        }
    }

    fn erase(&mut self, addr: u32, size: u32) -> Result<(), FakeError> {
        assert_eq!(addr % size, 0, "misaligned erase");
        if self.fail_erase_at == Some(addr) {
            return Err(FakeError);
        }
        self.flash[addr as usize..(addr + size) as usize].fill(0xFF);
        Ok(())
    }
}

impl Nor for FakeNor {
    type Error = FakeError;
    fn size(&self) -> u32 {
        self.flash.len() as u32
    }
    fn erase_4k(&mut self, addr: u32) -> Result<(), FakeError> {
        self.ops.push(Op::Erase4k(addr));
        self.erase(addr, SECTOR_BYTES)
    }
    fn erase_64k(&mut self, addr: u32) -> Result<(), FakeError> {
        self.ops.push(Op::Erase64k(addr));
        self.erase(addr, BLOCK_BYTES)
    }
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), FakeError> {
        for (i, &d) in data.iter().enumerate() {
            let a = addr as usize + i;
            if self.flash[a] != 0xFF {
                self.unerased_writes += 1;
            }
            let mut d = d;
            if self.corrupt_at == Some(a as u32) {
                d ^= 1;
            }
            self.flash[a] &= d;
        }
        Ok(())
    }
    fn with_window<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> Result<R, FakeError> {
        Ok(f(&self.flash))
    }
}

/// The loader behind a byte link, with optional faults on request `drop_reply_at`
/// (its reply is lost) and `corrupt_request_at` (one byte changed).
struct Board {
    loader: Loader<FakeNor>,
    out: VecDeque<u8>,
    requests: usize,
    raw: Vec<u8>,
    drop_reply_at: Option<usize>,
    corrupt_request_at: Option<usize>,
}

impl Board {
    fn new() -> Self {
        Self::with(FakeNor::new())
    }
    fn with(nor: FakeNor) -> Self {
        Self {
            loader: Loader::new(nor),
            out: VecDeque::new(),
            requests: 0,
            raw: Vec::new(),
            drop_reply_at: None,
            corrupt_request_at: None,
        }
    }
    fn nor(&self) -> &FakeNor {
        &self.loader.nor
    }
    fn feed_frame(&mut self, frame: &[u8]) {
        let idx = self.requests;
        self.requests += 1;
        let mut frame = frame.to_vec();
        if self.corrupt_request_at == Some(idx) {
            let i = frame.len() / 2;
            frame[i] = if frame[i] == 0x80 {
                0x40
            } else {
                frame[i] ^ 0x80
            };
        }
        let mut reply = Vec::new();
        for &b in &frame {
            if let Some(r) = self.loader.feed(b) {
                reply.extend_from_slice(r.wire);
            }
        }
        if self.drop_reply_at != Some(idx) {
            self.out.extend(reply);
        }
    }
}

impl Transport for Board {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for &b in data {
            if b != 0 {
                self.raw.push(b);
            } else if self.raw.is_empty() {
                // The client's leading flush byte.
                assert!(self.loader.feed(0).is_none());
            } else {
                let mut frame = std::mem::take(&mut self.raw);
                frame.push(0);
                self.feed_frame(&frame);
            }
        }
        Ok(())
    }
    fn read_timeout(&mut self, buf: &mut [u8], _: Duration) -> io::Result<usize> {
        let n = buf.len().min(self.out.len());
        for (d, s) in buf.iter_mut().zip(self.out.drain(..n)) {
            *d = s;
        }
        Ok(n)
    }
}

fn cfg() -> Config {
    Config {
        retries: 5,
        timeout: Duration::from_millis(50),
    }
}

fn rom(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

fn load(board: &mut Board, rom: &[u8]) -> Result<LoadOutcome, Error> {
    Client::new(board, cfg()).load(rom, &mut |_, _| {})
}

/// The one reply to `frame` fed straight to the loader.
fn answer(board: &mut Board, frame: &[u8]) -> (u8, Message<'static>) {
    let mut dec = FrameDecoder::new();
    let mut got = None;
    for &b in frame {
        if let Some(r) = board.loader.feed(b) {
            for &w in r.wire {
                if let Some(d) = dec.feed(w) {
                    let (seq, m) = d.expect("the loader's reply decodes");
                    got = Some((seq, static_msg(&m)));
                }
            }
        }
    }
    got.expect("a reply")
}

fn static_msg(m: &Message<'_>) -> Message<'static> {
    match *m {
        Message::Ack => Message::Ack,
        Message::Nak { code } => Message::Nak { code },
        Message::HelloReply {
            protocol,
            max_chunk,
            flash_len,
        } => Message::HelloReply {
            protocol,
            max_chunk,
            flash_len,
        },
        Message::AlreadyLoaded => Message::AlreadyLoaded,
        Message::RomDone { hash } => Message::RomDone { hash },
        ref other => panic!("not a reply: {other:?}"),
    }
}

fn hello(board: &mut Board) {
    let r = answer(board, &encode(&Message::Hello { protocol: 1 }, 0));
    assert!(matches!(r.1, Message::HelloReply { .. }), "{r:?}");
}

#[test]
fn load_then_already_loaded() {
    // Two whole 64 KiB blocks after block 0, then a partial tail.
    let rom = rom(3 * 65536 + 5000, 0x1234_5678);
    let mut b = Board::new();
    let out = load(&mut b, &rom).unwrap();
    assert_eq!(
        out,
        LoadOutcome::Loaded {
            hash: fnv::hash(&rom)
        }
    );
    let nor = b.nor();
    assert_eq!(nor.unerased_writes, 0);
    assert_eq!(&nor.flash[256..256 + rom.len()], &rom[..]);
    assert_eq!(rom_image::validate(&nor.flash).unwrap(), &rom[..]);
    // Lazy erase: block 0 at RomBegin, then whole blocks while one fits
    // below the image's end (rounded up to 4 KiB), then 4 KiB sectors.
    let end = (256 + rom.len() as u32).next_multiple_of(4096);
    assert_eq!(end, 0x3_2000);
    assert_eq!(
        nor.ops,
        vec![
            Op::Erase64k(0),
            Op::Erase64k(0x1_0000),
            Op::Erase64k(0x2_0000),
            Op::Erase4k(0x3_0000),
            Op::Erase4k(0x3_1000),
        ]
    );
    let c = b.loader.counters;
    assert_eq!(c.chunks as usize, rom.len().div_ceil(1024));
    assert_eq!((c.loads_ok, c.verify_failures, c.naks), (1, 0, 0));

    // The same ROM again: nothing erased or written.
    let before = b.nor().flash.clone();
    assert_eq!(load(&mut b, &rom).unwrap(), LoadOutcome::AlreadyLoaded);
    assert_eq!(b.nor().ops.len(), 5);
    assert!(b.nor().flash == before);
    assert_eq!(b.loader.counters.already_loaded, 1);
}

#[test]
fn image_ending_on_a_block_boundary_needs_no_sector_erase() {
    let rom = rom(2 * 65536 - 256, 7);
    let mut b = Board::new();
    load(&mut b, &rom).unwrap();
    assert_eq!(b.nor().ops, vec![Op::Erase64k(0), Op::Erase64k(0x1_0000)]);
    // Nothing past the image was erased or written.
    assert_eq!(rom_image::validate(&b.nor().flash).unwrap(), &rom[..]);
}

#[test]
fn erase_happens_when_the_first_chunk_reaches_a_block() {
    let rom = rom(2 * 65536, 9);
    let mut b = Board::new();
    hello(&mut b);
    let hash = fnv::hash(&rom);
    let len = rom.len() as u32;
    let r = answer(&mut b, &encode(&Message::RomBegin { len, hash }, 1));
    assert_eq!(r.1, Message::Ack);
    assert_eq!(b.nor().ops, vec![Op::Erase64k(0)]);
    // Chunks up to ROM offset 65280 stay inside block 0 (header 256 B).
    let mut seq = 2u8;
    for off in (0..65280).step_by(1024) {
        let n = 1024.min(65280 - off);
        let msg = Message::RomChunk {
            offset: off as u32,
            data: &rom[off..off + n],
        };
        assert_eq!(answer(&mut b, &encode(&msg, seq)).1, Message::Ack);
        seq = seq.wrapping_add(1);
    }
    assert_eq!(b.nor().ops, vec![Op::Erase64k(0)]);
    let msg = Message::RomChunk {
        offset: 65280,
        data: &rom[65280..65281],
    };
    assert_eq!(answer(&mut b, &encode(&msg, seq)).1, Message::Ack);
    assert_eq!(b.nor().ops, vec![Op::Erase64k(0), Op::Erase64k(0x1_0000)]);
}

#[test]
fn a_new_load_erases_the_old_header_at_once() {
    let old = rom(10_000, 1);
    let new = rom(200_000, 2);
    let mut b = Board::new();
    load(&mut b, &old).unwrap();
    assert!(rom_image::validate(&b.nor().flash).is_ok());
    // Begin a different ROM and send only part of it: the PC went away.
    hello(&mut b);
    let msg = Message::RomBegin {
        len: new.len() as u32,
        hash: fnv::hash(&new),
    };
    assert_eq!(answer(&mut b, &encode(&msg, 1)).1, Message::Ack);
    assert_eq!(rom_image::validate(&b.nor().flash), Err(ImageError::Erased));
    let msg = Message::RomChunk {
        offset: 0,
        data: &new[..1024],
    };
    assert_eq!(answer(&mut b, &encode(&msg, 2)).1, Message::Ack);
    assert_eq!(
        rom_image::read_header(&b.nor().flash),
        Err(ImageError::Erased)
    );
    // A fresh session loads it in full.
    assert!(matches!(
        load(&mut b, &new).unwrap(),
        LoadOutcome::Loaded { .. }
    ));
    assert_eq!(rom_image::validate(&b.nor().flash).unwrap(), &new[..]);
    assert_eq!(b.nor().unerased_writes, 0);
}

#[test]
fn resend_is_answered_from_the_stored_reply() {
    let rom = rom(5000, 3);
    let mut b = Board::new();
    hello(&mut b);
    let len = rom.len() as u32;
    let begin = encode(
        &Message::RomBegin {
            len,
            hash: fnv::hash(&rom),
        },
        1,
    );
    assert_eq!(answer(&mut b, &begin).1, Message::Ack);
    let chunk = encode(
        &Message::RomChunk {
            offset: 0,
            data: &rom[..1024],
        },
        2,
    );
    assert_eq!(answer(&mut b, &chunk), (2, Message::Ack));
    // The same frame again: the same Ack, no second program (which the
    // fake NOR would count as writing unerased bytes).
    assert_eq!(answer(&mut b, &chunk), (2, Message::Ack));
    assert_eq!(b.loader.counters.chunks, 1);
    assert_eq!(b.loader.counters.duplicates, 1);
    assert_eq!(b.nor().unerased_writes, 0);
    // A resend of RomBegin after a chunk is not the last frame: it is a
    // new RomBegin and restarts the load.
    assert_eq!(answer(&mut b, &begin).1, Message::Ack);
    assert_eq!(b.loader.counters.begins, 2);
}

#[test]
fn hello_is_always_fresh() {
    let mut b = Board::new();
    let h = encode(&Message::Hello { protocol: 1 }, 0);
    for _ in 0..3 {
        assert!(matches!(answer(&mut b, &h).1, Message::HelloReply { .. }));
    }
    assert_eq!(b.loader.counters.hellos, 3);
    assert_eq!(b.loader.counters.duplicates, 0);
    // After a Hello, a frame equal to the one before it is new work.
    let rom = rom(3000, 4);
    let begin = encode(
        &Message::RomBegin {
            len: rom.len() as u32,
            hash: fnv::hash(&rom),
        },
        1,
    );
    assert_eq!(answer(&mut b, &begin).1, Message::Ack);
    answer(&mut b, &h);
    assert_eq!(answer(&mut b, &begin).1, Message::Ack);
    assert_eq!(b.loader.counters.begins, 2);
}

#[test]
fn hello_reply_carries_the_boards_version_even_on_mismatch() {
    let mut b = Board::new();
    let r = answer(&mut b, &encode(&Message::Hello { protocol: 99 }, 5));
    assert_eq!(
        r,
        (
            5,
            Message::HelloReply {
                protocol: gbc_link::PROTOCOL_VERSION,
                max_chunk: 1024,
                flash_len: FLASH as u32,
            }
        )
    );
}

#[test]
fn protocol_errors_are_naked() {
    let nak = |code| Message::Nak { code };
    let mut b = Board::new();
    hello(&mut b);
    let data = [0u8; 16];
    // Chunk and RomEnd without RomBegin.
    let chunk0 = Message::RomChunk {
        offset: 0,
        data: &data,
    };
    assert_eq!(
        answer(&mut b, &encode(&chunk0, 1)).1,
        nak(NakCode::UnexpectedType)
    );
    assert_eq!(
        answer(&mut b, &encode(&Message::RomEnd, 2)).1,
        nak(NakCode::UnexpectedType)
    );
    // RomBegin out of range.
    for len in [0, (FLASH - 256 + 1) as u32] {
        let m = Message::RomBegin { len, hash: 0 };
        assert_eq!(answer(&mut b, &encode(&m, 3)).1, nak(NakCode::OutOfRange));
    }
    let m = Message::RomBegin { len: 100, hash: 0 };
    assert_eq!(answer(&mut b, &encode(&m, 4)).1, Message::Ack);
    // A gap, a repeat of offset 0 after it was taken, and past the end.
    let gap = Message::RomChunk {
        offset: 16,
        data: &data,
    };
    assert_eq!(answer(&mut b, &encode(&gap, 5)).1, nak(NakCode::OutOfRange));
    assert_eq!(answer(&mut b, &encode(&chunk0, 6)).1, Message::Ack);
    assert_eq!(
        answer(&mut b, &encode(&chunk0, 7)).1,
        nak(NakCode::OutOfRange)
    );
    let past = Message::RomChunk {
        offset: 16,
        data: &[0u8; 85],
    };
    assert_eq!(
        answer(&mut b, &encode(&past, 8)).1,
        nak(NakCode::OutOfRange)
    );
    // RomEnd before all bytes arrived.
    assert_eq!(
        answer(&mut b, &encode(&Message::RomEnd, 9)).1,
        nak(NakCode::OutOfRange)
    );
    // Messages the loader does not take.
    assert_eq!(
        answer(&mut b, &encode(&Message::Input { buttons: 1 }, 10)).1,
        nak(NakCode::UnexpectedType)
    );
    // A bad CRC: Nak BadCrc, seq 0.
    let mut bad = encode(&Message::RomEnd, 11);
    bad[1] ^= 0x01;
    assert_eq!(answer(&mut b, &bad), (0, nak(NakCode::BadCrc)));
    assert_eq!(b.loader.counters.bad_frames, 1);
}

#[test]
fn read_back_mismatch_reports_the_read_hash_and_writes_no_header() {
    let rom = rom(20_000, 5);
    let mut nor = FakeNor::new();
    nor.corrupt_at = Some(256 + 12_345);
    let mut b = Board::with(nor);
    match load(&mut b, &rom) {
        Err(Error::HashMismatch { sent, board }) => {
            assert_eq!(sent, fnv::hash(&rom));
            let mut bad = rom.clone();
            bad[12_345] ^= 1;
            assert_eq!(board, fnv::hash(&bad));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rom_image::read_header(&b.nor().flash),
        Err(ImageError::Erased)
    );
    assert_eq!(b.loader.counters.verify_failures, 1);
}

#[test]
fn flash_error_is_naked_and_drops_the_load() {
    let rom = rom(100_000, 6);
    let mut nor = FakeNor::new();
    nor.fail_erase_at = Some(0x1_0000);
    let mut b = Board::with(nor);
    match load(&mut b, &rom) {
        Err(Error::GaveUp { request, last, .. }) => {
            assert_eq!(request, "RomChunk");
            assert_eq!(last, gbc_pc::Failure::Nak(NakCode::FlashError));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(b.loader.counters.flash_errors, 1);
    assert_eq!(b.loader.last_flash_error, Some(FakeError));
}

#[test]
fn link_faults_are_survived_without_redoing_work() {
    let rom = rom(10_000, 8);
    for (drop, corrupt) in [(Some(3), None), (None, Some(4)), (Some(1), Some(12))] {
        let mut b = Board::new();
        b.drop_reply_at = drop;
        b.corrupt_request_at = corrupt;
        let mut c = Client::new(&mut b, cfg());
        let out = c.load(&rom, &mut |_, _| {}).unwrap();
        assert!(matches!(out, LoadOutcome::Loaded { .. }));
        assert_eq!(
            c.stats.resends,
            u32::from(drop.is_some()) + u32::from(corrupt.is_some())
        );
        assert_eq!(b.loader.counters.chunks as usize, rom.len().div_ceil(1024));
        assert_eq!(
            b.loader.counters.duplicates,
            u32::from(drop.is_some_and(|d| d >= 1))
        );
        assert_eq!(b.nor().unerased_writes, 0);
        assert_eq!(rom_image::validate(&b.nor().flash).unwrap(), &rom[..]);
    }
}

#[test]
fn reply_type_matches_the_client() {
    // The board's HelloReply goes through the client's own parser.
    let mut b = Board::new();
    let info = Client::new(&mut b, cfg()).hello().unwrap();
    assert_eq!((info.max_chunk, info.flash_len), (1024, FLASH as u32));
}

// The AlreadyLoaded guard in `Loader::begin` has three parts (the image
// validates, the length matches, the hash matches). Each test below makes
// exactly one part fail while the other two hold.

/// A fresh load of `new` over a stored `old` must erase and rewrite.
fn assert_full_reload(b: &mut Board, new: &[u8]) {
    let begins = b.loader.counters.begins;
    let already = b.loader.counters.already_loaded;
    assert_eq!(
        load(b, new).unwrap(),
        LoadOutcome::Loaded {
            hash: fnv::hash(new)
        }
    );
    assert_eq!(b.loader.counters.begins, begins + 1);
    assert_eq!(b.loader.counters.already_loaded, already);
    assert_eq!(rom_image::validate(&b.nor().flash).unwrap(), new);
}

#[test]
fn same_length_different_hash_is_a_full_load() {
    let old = rom(10_000, 21);
    let new = rom(10_000, 22);
    assert_ne!(fnv::hash(&old), fnv::hash(&new));
    let mut b = Board::new();
    load(&mut b, &old).unwrap();
    assert_full_reload(&mut b, &new);
}

#[test]
fn same_hash_different_length_is_a_full_load() {
    let old = rom(10_000, 23);
    let mut b = Board::new();
    load(&mut b, &old).unwrap();
    // The client derives the hash from the file, so send RomBegin by hand:
    // the stored image's hash with another length.
    hello(&mut b);
    let begin = Message::RomBegin {
        len: old.len() as u32 + 1,
        hash: fnv::hash(&old),
    };
    assert_eq!(answer(&mut b, &encode(&begin, 1)).1, Message::Ack);
    assert_eq!(b.loader.counters.already_loaded, 0);
    assert_eq!(b.loader.counters.begins, 2);
    assert_eq!(rom_image::validate(&b.nor().flash), Err(ImageError::Erased));
}

#[test]
fn stored_image_with_a_corrupt_data_byte_is_reloaded() {
    let rom = rom(10_000, 24);
    let mut b = Board::new();
    load(&mut b, &rom).unwrap();
    // The header (len and hash) still matches the request; the data does not.
    b.loader.nor.flash[256 + 5000] ^= 0x10;
    assert!(rom_image::validate(&b.nor().flash).is_err());
    assert_full_reload(&mut b, &rom);
}

#[test]
fn header_programmed_wrong_is_naked_and_leaves_no_image() {
    let rom = rom(900, 25);
    let mut nor = FakeNor::new();
    // Bit 0 of the header's first byte flips when the header is programmed
    // (data is at 256.., so only the final header program reaches it).
    nor.corrupt_at = Some(0);
    let mut b = Board::with(nor);
    hello(&mut b);
    let begin = Message::RomBegin {
        len: rom.len() as u32,
        hash: fnv::hash(&rom),
    };
    assert_eq!(answer(&mut b, &encode(&begin, 1)).1, Message::Ack);
    let chunk = Message::RomChunk {
        offset: 0,
        data: &rom,
    };
    assert_eq!(answer(&mut b, &encode(&chunk, 2)).1, Message::Ack);
    assert_eq!(
        answer(&mut b, &encode(&Message::RomEnd, 3)).1,
        Message::Nak {
            code: NakCode::FlashError
        }
    );
    assert_eq!(b.loader.counters.verify_failures, 1);
    assert_eq!(b.loader.counters.loads_ok, 0);
    assert!(rom_image::validate(&b.nor().flash).is_err());
}
