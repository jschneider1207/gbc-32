//! The PC client against the board's real link session
//! (`gbc_link_board::link_session`, the code the `player` and
//! `link-probe` images run) over a fake game: `load --sav` (ROM
//! `AlreadyLoaded`, then the save), dumps, refusals that apply nothing,
//! live input and its timeout.
//! `tests/save.rs` runs the same client against the simulated board.

use std::collections::VecDeque;
use std::io;
use std::time::{Duration, Instant};

use gbc_firmware::buttons::Shared;
use gbc_firmware::save::{ClockKind, Image, Layout};
use gbc_link::{FrameDecoder, Message, NakCode, buttons};
use gbc_link_board::link_session::{self, Game, PC_TIMEOUT_FRAMES, Session};
use gbc_pc::input::{Key, KeySet};
use gbc_pc::play::{Keyboard, PlayOptions, play};
use gbc_pc::sim::encode;
use gbc_pc::{Client, Config, Error, Failure, LoadOutcome, Transport, fnv};

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

/// Every frame in `bytes`, as debug strings (seq, message).
fn frames(bytes: &[u8]) -> Vec<String> {
    let mut d = FrameDecoder::new();
    bytes
        .iter()
        .filter_map(|&b| {
            d.feed(b)
                .map(|r| format!("{:?}", r.map(|(s, m)| (s, format!("{m:?}")))))
        })
        .collect()
}

fn ack(seq: u8) -> String {
    format!("{:?}", Ok::<_, ()>((seq, "Ack".to_string())))
}

#[test]
fn audio_start_is_refused_unless_the_board_streams_audio() {
    let mut b = Board::new();
    b.write_all(&encode(&Message::AudioStart, 3)).unwrap();
    let out: Vec<u8> = b.out.drain(..).collect();
    let nak = Message::Nak {
        code: NakCode::UnexpectedType,
    };
    assert_eq!(
        frames(&out),
        [format!("{:?}", Ok::<_, ()>((3u8, format!("{nak:?}"))))]
    );
    assert!(!b.session.audio_on());
}

#[test]
fn audio_start_stop_hello_and_timeout() {
    let mut b = Board::new();
    b.session.set_audio_capable(true);
    b.write_all(&encode(&Message::AudioStart, 1)).unwrap();
    assert_eq!(frames(&b.out.drain(..).collect::<Vec<_>>()), [ack(1)]);
    assert!(b.session.audio_on());
    b.write_all(&encode(&Message::AudioStop, 2)).unwrap();
    assert_eq!(frames(&b.out.drain(..).collect::<Vec<_>>()), [ack(2)]);
    assert!(!b.session.audio_on());
    // Hello ends it.
    b.write_all(&encode(&Message::AudioStart, 3)).unwrap();
    b.write_all(&encode(&Message::Hello { protocol: 1 }, 4))
        .unwrap();
    assert!(!b.session.audio_on());
    // Any frame from the PC keeps it alive; PC_TIMEOUT_FRAMES of silence end it.
    b.write_all(&encode(&Message::AudioStart, 5)).unwrap();
    for _ in 0..PC_TIMEOUT_FRAMES - 1 {
        b.session.end_frame(b.shared);
    }
    b.write_all(&encode(&Message::Input { buttons: 0 }, 6))
        .unwrap();
    for _ in 0..PC_TIMEOUT_FRAMES - 1 {
        b.session.end_frame(b.shared);
    }
    assert!(b.session.audio_on());
    b.session.end_frame(b.shared);
    assert!(!b.session.audio_on());
    let c = b.session.counters();
    assert_eq!((c.audio_starts, c.audio_stops, c.audio_timeouts), (3, 1, 1));
    // Turning the capability off stops a stream.
    b.write_all(&encode(&Message::AudioStart, 7)).unwrap();
    b.session.set_audio_capable(false);
    assert!(!b.session.audio_on());
}

#[test]
fn hello_drops_a_dump_being_sent() {
    let mut b = Board::new();
    // Fed straight to the session: nothing is sent between the frames,
    // so the dump is still queued when Hello arrives.
    for &x in &encode(&Message::SaveDumpRequest, 4) {
        b.session.feed(x, &mut b.game, b.shared);
    }
    assert!(matches!(
        b.session.progress(),
        link_session::Progress::Dumping { .. }
    ));
    for &x in &encode(&Message::Hello { protocol: 1 }, 0) {
        b.session.feed(x, &mut b.game, b.shared);
    }
    assert_eq!(b.session.progress(), link_session::Progress::Idle);
    b.poll_tx();
    let f = frames(&b.out.drain(..).collect::<Vec<_>>());
    assert_eq!(f.len(), 1, "{f:?}");
    assert!(f[0].contains("HelloReply"), "{}", f[0]);
    let c = b.session.counters();
    assert_eq!((c.dumps, c.dumps_done), (1, 0));
}

#[test]
fn caller_frames_go_after_replies_and_before_the_dump() {
    let mut b = Board::new();
    b.session.set_audio_capable(true);
    for &x in &encode(&Message::AudioStart, 9) {
        b.session.feed(x, &mut b.game, b.shared);
    }
    for &x in &encode(&Message::SaveDumpRequest, 10) {
        b.session.feed(x, &mut b.game, b.shared);
    }
    let marker = encode(
        &Message::Audio {
            first: 77,
            held: 0,
            format: gbc_link::audio::U12,
            data: &[1, 2, 3],
        },
        0,
    );
    let mut tx = [0u8; 4096];
    let mut room_seen = 0;
    let n = b.session.fill_tx_with(&mut tx, |room| {
        room_seen = room.len();
        room[..marker.len()].copy_from_slice(&marker);
        marker.len()
    });
    let f = frames(&tx[..n]);
    assert_eq!(f[0], ack(9));
    assert!(f[1].contains("Audio { first: 77"), "{}", f[1]);
    assert!(f[2].contains("SaveBegin"), "{}", f[2]);
    assert!(f[3..].iter().all(|s| s.contains("SaveChunk")));
    assert_eq!(room_seen, 4096 - encode(&Message::Ack, 9).len());
}

/// The board behind a link that loses every `AudioStop` frame.
struct LoseAudioStop<'a> {
    board: &'a mut Board,
    raw: Vec<u8>,
}

impl Transport for LoseAudioStop<'_> {
    fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
        for &b in data {
            self.raw.push(b);
            if b != 0 {
                continue;
            }
            let frame = std::mem::take(&mut self.raw);
            let mut dec = FrameDecoder::new();
            let mut stop = false;
            for &x in &frame {
                stop |= matches!(dec.feed(x), Some(Ok((_, Message::AudioStop))));
            }
            if !stop {
                self.board.write_all(&frame)?;
            }
        }
        Ok(())
    }
    fn read_timeout(&mut self, buf: &mut [u8], t: Duration) -> io::Result<usize> {
        self.board.read_timeout(buf, t)
    }
}

/// `play --audio` for `polls` loop turns (then Esc), no keys held.
fn play_audio<T: Transport>(t: T, polls: usize) -> gbc_pc::play::PlayReport {
    struct Polls(usize);
    impl Keyboard for Polls {
        fn held(&mut self) -> io::Result<KeySet> {
            self.0 = self.0.saturating_sub(1);
            Ok(if self.0 == 0 {
                KeySet::of(&[Key::Esc])
            } else {
                KeySet::default()
            })
        }
    }
    let mut c = Client::new(
        t,
        Config {
            retries: 2,
            timeout: Duration::from_millis(20),
        },
    );
    let t0 = Instant::now();
    let mut n = 0u32;
    let mut now = || {
        n += 1;
        t0 + Duration::from_millis(10) * n
    };
    let opts = PlayOptions {
        audio: true,
        poll: Duration::ZERO,
        ..PlayOptions::default()
    };
    play(&mut c, &mut Polls(polls), &opts, &mut now, &mut |_| {}).unwrap()
}

#[test]
fn play_audio_after_a_run_that_ended_without_audio_stop() {
    let mut b = Board::new();
    b.session.set_audio_capable(true);
    // Run 1: its AudioStop is lost, so the stream stays on until the
    // board's PC timeout ends it.
    play_audio(
        LoseAudioStop {
            board: &mut b,
            raw: Vec::new(),
        },
        5,
    );
    assert!(b.session.audio_on(), "AudioStop was lost");
    for _ in 0..PC_TIMEOUT_FRAMES {
        b.session.end_frame(b.shared);
    }
    assert!(!b.session.audio_on());
    // Run 2, a new process: its seqs restart. Its AudioStart must turn
    // the stream on again, not be taken for a resend of run 1's.
    let mut on_during_run = false;
    {
        struct Watch<'a>(&'a mut Board, &'a mut bool);
        impl Transport for Watch<'_> {
            fn write_all(&mut self, data: &[u8]) -> io::Result<()> {
                self.0.write_all(data)?;
                *self.1 |= self.0.session.audio_on();
                Ok(())
            }
            fn read_timeout(&mut self, buf: &mut [u8], t: Duration) -> io::Result<usize> {
                self.0.read_timeout(buf, t)
            }
        }
        play_audio(Watch(&mut b, &mut on_during_run), 5);
    }
    assert!(on_during_run, "run 2's AudioStart turned the stream on");
    assert!(!b.session.audio_on(), "run 2's AudioStop turned it off");
    let c = b.session.counters();
    assert_eq!((c.audio_starts, c.audio_stops), (2, 1));
    assert_eq!(c.duplicates, 0);
}

#[test]
fn every_reply_fits_the_reply_buffer() {
    // Every message the session answers with, at its largest; the const
    // assertion in link_session.rs relies on REPLY_BODY_MAX being right.
    let bound = link_session::REPLY_FRAME_MAX;
    for m in [
        Message::Ack,
        Message::Nak {
            code: NakCode::HashMismatch,
        },
        Message::HelloReply {
            protocol: u16::MAX,
            max_chunk: u16::MAX,
            flash_len: u32::MAX,
        },
        Message::AlreadyLoaded,
    ] {
        let mut out = [0u8; link_session::REPLY_MAX];
        let n = gbc_link::encode(&m, 0xFF, &mut out).unwrap();
        assert!(n <= bound, "{m:?}: {n} > {bound}");
    }
}
