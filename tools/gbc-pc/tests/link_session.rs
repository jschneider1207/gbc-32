//! The PC client against the board's real link session
//! (`crates/gbc-nucleo-h753/src/link_session.rs`, compiled here on the
//! host) over a fake game: `load --sav` (ROM `AlreadyLoaded`, then the
//! save), dumps, refusals that apply nothing, live input and its timeout.
//! `tests/save.rs` runs the same client against the simulated board.

#[path = "../../../crates/gbc-nucleo-h753/src/link_session.rs"]
#[allow(dead_code)]
mod link_session;

use std::collections::VecDeque;
use std::io;
use std::time::Duration;

use gbc_firmware::buttons::Shared;
use gbc_firmware::save::{ClockKind, Image, Layout};
use gbc_link::{FrameDecoder, Message, NakCode, buttons};
use gbc_pc::sim::encode;
use gbc_pc::{Client, Config, Error, Failure, LoadOutcome, Transport, fnv};
use link_session::{Game, PC_TIMEOUT_FRAMES, Session};

const RAM: usize = 32 * 1024;
const SAVE_LEN: usize = RAM + 48;

struct FakeGame {
    rom: Vec<u8>,
    layout: Option<Layout>,
    save: Vec<u8>,
    applied: u32,
}

impl Game for FakeGame {
    fn layout(&self) -> Option<Layout> {
        self.layout
    }
    fn rom(&self) -> Option<(u32, u64)> {
        Some((self.rom.len() as u32, fnv::hash(&self.rom)))
    }
    fn apply(&mut self, bytes: &[u8], image: &Image<'_>) -> bool {
        assert_eq!(image.ram(), &bytes[..RAM]);
        self.save = bytes.to_vec();
        self.applied += 1;
        true
    }
    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        self.layout?;
        out.get_mut(..self.save.len())?.copy_from_slice(&self.save);
        Some(self.save.len())
    }
}

/// The board: the session, polled once per write (as between frames),
/// and its transmit side drained into `out` in 4 KiB fills.
struct Board {
    session: Session<'static>,
    game: FakeGame,
    shared: &'static Shared,
    out: VecDeque<u8>,
}

impl Board {
    fn new() -> Self {
        let buf: &'static mut [u8] = Box::leak(vec![0u8; 128 * 1024 + 64].into_boxed_slice());
        Board {
            session: Session::new(buf, 8 << 20),
            game: FakeGame {
                rom: (0..32768u32).map(|i| (i * 13 + 1) as u8).collect(),
                layout: Some(Layout {
                    ram_size: RAM,
                    clock: Some(ClockKind::Mbc3),
                }),
                save: vec![0; SAVE_LEN],
                applied: 0,
            },
            shared: Box::leak(Box::new(Shared::new())),
            out: VecDeque::new(),
        }
    }
    fn poll_tx(&mut self) {
        let mut tx = [0u8; 4096];
        loop {
            let n = self.session.fill_tx(&mut tx);
            if n == 0 {
                break;
            }
            self.out.extend(&tx[..n]);
        }
    }
}

impl Transport for Board {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for &b in data {
            self.session.feed(b, &mut self.game, self.shared);
        }
        self.poll_tx();
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

fn client(board: &mut Board) -> Client<&mut Board> {
    Client::new(
        board,
        Config {
            retries: 2,
            timeout: Duration::from_millis(20),
        },
    )
}

/// A valid save: RAM pattern, RTC footer (halted), a unix time.
fn sav() -> Vec<u8> {
    let mut s: Vec<u8> = (0..RAM).map(|i| (i * 7 + i / 251) as u8).collect();
    for v in [12u32, 34, 5, 0x89, 0x41, 12, 34, 5, 0x89, 0x41] {
        s.extend_from_slice(&v.to_le_bytes());
    }
    s.extend_from_slice(&1_700_000_000u64.to_le_bytes());
    s
}

fn load_save(board: &mut Board, save: &[u8]) -> Result<(), Error> {
    let rom = board.game.rom.clone();
    let mut c = client(board);
    assert_eq!(c.load(&rom, &mut |_, _| {})?, LoadOutcome::AlreadyLoaded);
    c.send_save(save, &mut |_, _| {})
}

fn dump(board: &mut Board) -> Result<Vec<u8>, Error> {
    client(board).dump_save()
}

#[test]
fn load_then_dump_round_trips() {
    let mut b = Board::new();
    let before = dump(&mut b).unwrap();
    assert_eq!(before, vec![0; SAVE_LEN]);
    load_save(&mut b, &sav()).unwrap();
    assert_eq!(b.game.applied, 1);
    assert_eq!(b.game.save, sav());
    assert_eq!(dump(&mut b).unwrap(), sav());
    let c = b.session.counters();
    assert_eq!((c.saves_applied, c.dumps, c.dumps_done), (1, 2, 2));
    assert_eq!((c.bad_frames, c.naks, c.replies_dropped), (0, 0, 0));
}

#[test]
fn a_rom_that_is_not_the_running_one_is_refused() {
    let mut b = Board::new();
    let other = vec![1u8; 32768];
    match client(&mut b).load(&other, &mut |_, _| {}) {
        Err(Error::GaveUp {
            last: Failure::Nak(NakCode::UnexpectedType),
            ..
        }) => {}
        r => panic!("{r:?}"),
    }
}

fn assert_refused(r: Result<(), Error>, code: NakCode) {
    match r {
        Err(Error::GaveUp {
            last: Failure::Nak(c),
            ..
        }) if c == code => {}
        r => panic!("expected Nak {code:?}, got {r:?}"),
    }
}

#[test]
fn wrong_size_bad_footer_and_bad_hash_apply_nothing() {
    let mut b = Board::new();
    load_save(&mut b, &sav()).unwrap();
    // One byte too many, and RAM + 47: no accepted length.
    let mut long = sav();
    long.push(0);
    assert_refused(load_save(&mut b, &long), NakCode::OutOfRange);
    assert_refused(
        load_save(&mut b, &sav()[..SAVE_LEN - 1]),
        NakCode::OutOfRange,
    );
    // DH with bit 5 set: BadFooter, found only at the verdict.
    let mut bad = sav();
    bad[RAM + 16] |= 0x20;
    assert_refused(load_save(&mut b, &bad), NakCode::OutOfRange);
    assert_eq!(b.game.applied, 1);
    assert_eq!(dump(&mut b).unwrap(), sav());
    // A chunk whose bytes do not hash to SaveBegin's: HashMismatch.
    let mut c = client(&mut b);
    c.hello().unwrap();
    // A wrong length is refused at SaveBegin already, before any chunk.
    let long_begin = Message::SaveBegin {
        len: SAVE_LEN as u32 + 1,
        hash: 0,
    };
    assert_refused(c.request(&long_begin).map(drop), NakCode::OutOfRange);
    let s = sav();
    let wrong_hash = Message::SaveBegin {
        len: SAVE_LEN as u32,
        hash: fnv::hash(&s) ^ 1,
    };
    c.request(&wrong_hash).unwrap();
    let mut off = 0;
    let mut last = Ok(());
    while off < s.len() {
        let end = (off + 1024).min(s.len());
        let chunk = Message::SaveChunk {
            offset: off as u32,
            data: &s[off..end],
        };
        last = c.request(&chunk).map(drop);
        off = end;
    }
    assert_refused(last, NakCode::HashMismatch);
    assert_eq!(b.game.applied, 1);
}

#[test]
fn a_game_without_a_save_refuses_load_and_dump() {
    let mut b = Board::new();
    b.game.layout = None;
    assert_refused(load_save(&mut b, &sav()), NakCode::UnexpectedType);
    assert!(matches!(
        dump(&mut b),
        Err(Error::GaveUp {
            last: Failure::Nak(NakCode::UnexpectedType),
            ..
        })
    ));
}

#[test]
fn a_resent_last_chunk_gets_the_stored_verdict_without_reapplying() {
    let mut b = Board::new();
    load_save(&mut b, &sav()).unwrap();
    // The verdict frame again, byte for byte: the stored Ack.
    let s = sav();
    let last_off = (SAVE_LEN - 1) / 1024 * 1024;
    let seq_of_last = {
        // load: Hello 0, RomBegin 1; save: SaveBegin 2, chunks 3.. .
        (3 + SAVE_LEN.div_ceil(1024) - 1) as u8
    };
    let frame = encode(
        &Message::SaveChunk {
            offset: last_off as u32,
            data: &s[last_off..],
        },
        seq_of_last,
    );
    b.write_all(&frame).unwrap();
    let mut dec = FrameDecoder::new();
    let got: Vec<u8> = b.out.drain(..).collect();
    let (_, r) = dec.feed_slice(&got);
    assert!(
        matches!(r, Some(Ok((s, Message::Ack))) if s == seq_of_last),
        "{r:?}"
    );
    assert_eq!(b.game.applied, 1);
    assert_eq!(b.session.counters().duplicates, 1);
}

#[test]
fn input_sets_the_pc_buttons_and_times_out() {
    let mut b = Board::new();
    let bits = buttons::A | buttons::LEFT;
    b.write_all(&encode(&Message::Input { buttons: bits }, 7))
        .unwrap();
    assert!(b.out.is_empty(), "Input is never answered");
    assert_eq!(b.shared.pc().bits(), bits);
    for _ in 0..PC_TIMEOUT_FRAMES - 1 {
        b.session.end_frame(b.shared);
    }
    assert_eq!(b.shared.pc().bits(), bits);
    b.session.end_frame(b.shared);
    assert_eq!(b.shared.pc().bits(), 0);
    assert_eq!(b.session.counters().pc_timeouts, 1);
}

#[test]
fn a_board_dump_streams_unrequested_with_seq_0() {
    let mut b = Board::new();
    load_save(&mut b, &sav()).unwrap();
    b.session.board_dump(&mut b.game);
    b.poll_tx();
    let wire: Vec<u8> = b.out.drain(..).collect();
    let mut dec = FrameDecoder::new();
    let mut rest = &wire[..];
    let mut got = Vec::new();
    let mut first = true;
    while !rest.is_empty() {
        let (used, r) = dec.feed_slice(rest);
        rest = &rest[used..];
        let Some(r) = r else { break };
        match r.unwrap() {
            (0, Message::SaveBegin { len, hash }) if first => {
                assert_eq!((len as usize, hash), (SAVE_LEN, fnv::hash(&sav())));
                first = false;
            }
            (0, Message::SaveChunk { offset, data }) if !first => {
                assert_eq!(offset as usize, got.len());
                got.extend_from_slice(data);
            }
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(got, sav());
}
