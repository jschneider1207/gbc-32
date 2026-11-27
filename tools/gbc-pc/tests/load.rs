//! The ROM-load flow against the simulated board, with and without link
//! faults (`gbc_pc::sim`). Sim reads never block, so timeouts cost no
//! wall time.

use std::time::Duration;

use gbc_link::{MAX_CHUNK, NakCode};
use gbc_pc::sim::{Fault, Faulty, SimBoard};
use gbc_pc::{Client, Config, Error, Failure, LoadOutcome, fnv};

const FLASH: usize = 8 << 20;

fn cfg() -> Config {
    Config {
        retries: 5,
        timeout: Duration::from_millis(50),
    }
}

/// A deterministic, non-repeating test ROM with a partial last chunk.
fn rom(len: usize) -> Vec<u8> {
    let mut x = 0x1234_5678u32;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

/// Request index of chunk `i` (0 Hello, 1 RomBegin, then the chunks).
fn chunk_req(i: usize) -> usize {
    2 + i
}

fn load(t: &mut Faulty, rom: &[u8]) -> Result<LoadOutcome, Error> {
    let mut c = Client::new(t, cfg());
    c.load(rom, &mut |_, _| {})
}

#[test]
fn full_round_trip() {
    let rom = rom(64 * 1024 + 300);
    let mut c = Client::new(SimBoard::new(FLASH), cfg());
    let mut seen = Vec::new();
    let out = c
        .load(&rom, &mut |sent, total| seen.push((sent, total)))
        .unwrap();
    assert_eq!(
        out,
        LoadOutcome::Loaded {
            hash: fnv::hash(&rom)
        }
    );
    let board = c.transport();
    assert_eq!(board.rom(rom.len()), &rom[..]);
    // Nothing written past the ROM.
    assert!(board.flash[256 + rom.len()..].iter().all(|&b| b == 0xFF));
    assert_eq!(board.stored, Some((rom.len() as u32, fnv::hash(&rom))));
    assert_eq!(board.work.chunks as usize, rom.len().div_ceil(MAX_CHUNK));
    assert_eq!(seen.last(), Some(&(rom.len(), rom.len())));
    assert_eq!(c.stats, Default::default());
}

#[test]
fn smaller_board_chunk_size_is_used() {
    let rom = rom(5000);
    let mut board = SimBoard::new(FLASH);
    board.max_chunk = 256;
    let mut c = Client::new(board, cfg());
    c.load(&rom, &mut |_, _| {}).unwrap();
    assert_eq!(c.transport().work.chunks, 20);
    assert_eq!(c.transport().rom(rom.len()), &rom[..]);
}

#[test]
fn already_loaded_is_skipped() {
    let rom = rom(10_000);
    let mut t = Faulty::new(SimBoard::new(FLASH), &[]);
    assert!(matches!(load(&mut t, &rom), Ok(LoadOutcome::Loaded { .. })));
    let chunks = t.board.work.chunks;
    assert_eq!(load(&mut t, &rom).unwrap(), LoadOutcome::AlreadyLoaded);
    assert_eq!(t.board.work.chunks, chunks, "no chunks resent");
    assert_eq!(t.board.work.begins, 1);
    // A different ROM is loaded again.
    let mut other = rom.clone();
    other[123] ^= 1;
    assert!(matches!(
        load(&mut t, &other),
        Ok(LoadOutcome::Loaded { .. })
    ));
    assert_eq!(t.board.rom(other.len()), &other[..]);
}

/// Each fault on its own, at a chunk, still loads the ROM exactly.
fn survives(fault: Fault) -> (Faulty, gbc_pc::Stats) {
    let rom = rom(20 * 1024 + 77);
    let mut t = Faulty::new(SimBoard::new(FLASH), &[(chunk_req(3), fault)]);
    let mut c = Client::new(&mut t, cfg());
    let out = c.load(&rom, &mut |_, _| {});
    let stats = c.stats;
    assert_eq!(
        out.unwrap(),
        LoadOutcome::Loaded {
            hash: fnv::hash(&rom)
        },
        "{fault:?}"
    );
    assert_eq!(t.board.rom(rom.len()), &rom[..], "{fault:?}");
    assert_eq!(
        t.board.work.chunks, 21,
        "{fault:?}: each chunk programmed once"
    );
    (t, stats)
}

#[test]
fn dropped_reply_is_resent_and_deduplicated() {
    let (t, s) = survives(Fault::DropReply);
    assert_eq!((s.timeouts, s.resends), (1, 1));
    assert_eq!(t.board.work.duplicates, 1);
}

#[test]
fn corrupt_reply_is_resent() {
    let (t, s) = survives(Fault::CorruptReply);
    assert_eq!((s.corrupt, s.timeouts, s.resends), (1, 1, 1));
    assert_eq!(t.board.work.duplicates, 1);
}

#[test]
fn corrupt_request_is_naked_and_resent() {
    let (t, s) = survives(Fault::CorruptRequest);
    assert_eq!((s.naks, s.resends), (1, 1));
    assert_eq!(t.board.work.bad_frames, 1);
    assert_eq!(t.board.work.duplicates, 0);
}

#[test]
fn delayed_reply_then_stale_duplicate_is_ignored() {
    let (t, s) = survives(Fault::DelayReply);
    assert_eq!((s.timeouts, s.resends), (1, 1));
    // The board answered the resend too; that Ack (old seq) turned up
    // while the next chunk was outstanding.
    assert_eq!(t.board.work.duplicates, 1);
    assert_eq!(s.stale, 1);
}

#[test]
fn busy_nak_is_resent_after_a_backoff() {
    let t0 = std::time::Instant::now();
    let (_, s) = survives(Fault::NakRequest);
    assert_eq!((s.naks, s.resends), (1, 1));
    assert!(
        t0.elapsed() >= gbc_pc::client::BUSY_BACKOFF,
        "{:?}",
        t0.elapsed()
    );
}

#[test]
fn faults_on_every_stage() {
    let rom = rom(3000);
    let faults = [
        (0, Fault::DropReply),
        (1, Fault::DelayReply),
        (2, Fault::CorruptReply),
        (3, Fault::NakRequest),
        (4, Fault::DelayReply),
        (5, Fault::CorruptRequest),
        (6, Fault::DropReply),
    ];
    let mut t = Faulty::new(SimBoard::new(FLASH), &faults);
    assert!(matches!(load(&mut t, &rom), Ok(LoadOutcome::Loaded { .. })));
    assert_eq!(t.board.rom(rom.len()), &rom[..]);
    assert_eq!(t.board.work.begins, 1);
    assert_eq!(t.board.work.chunks, 3);
    assert_eq!(t.board.work.ends, 1);
}

#[test]
fn refusal_nak_fails_at_once_without_resend() {
    let rom = rom(4000);
    let mut t = Faulty::new(
        SimBoard::new(FLASH),
        &[(chunk_req(1), Fault::NakForever(NakCode::FlashError))],
    );
    let e = load(&mut t, &rom).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                request: "RomChunk",
                attempts: 1,
                last: Failure::Nak(NakCode::FlashError)
            }
        ),
        "{e:?}"
    );
    assert!(e.to_string().contains("Nak FlashError"), "{e}");
    assert_eq!(t.requests, chunk_req(1) + 1, "one attempt, no resend");
}

#[test]
fn silence_gives_up_after_retries() {
    let rom = rom(4000);
    let mut t = Faulty::new(SimBoard::new(FLASH), &[(1, Fault::SilenceForever)]);
    let e = load(&mut t, &rom).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                request: "RomBegin",
                attempts: 6,
                last: Failure::Timeout
            }
        ),
        "{e:?}"
    );
    assert_eq!(t.requests, 1 + 6);
}

#[test]
fn zero_retries_means_one_attempt() {
    let mut t = Faulty::new(SimBoard::new(FLASH), &[(0, Fault::SilenceForever)]);
    let mut c = Client::new(
        &mut t,
        Config {
            retries: 0,
            ..cfg()
        },
    );
    let e = c.hello().unwrap_err();
    assert!(matches!(e, Error::GaveUp { attempts: 1, .. }), "{e:?}");
}

#[test]
fn rom_too_large_is_refused_before_any_chunk() {
    let flash = 64 * 1024;
    let mut t = Faulty::new(SimBoard::new(flash), &[]);
    // Exactly flash_len - 256 fits.
    let fits = rom(flash - 256);
    assert!(matches!(
        load(&mut t, &fits),
        Ok(LoadOutcome::Loaded { .. })
    ));
    let mut t = Faulty::new(SimBoard::new(flash), &[]);
    let e = load(&mut t, &rom(flash - 255)).unwrap_err();
    assert!(
        matches!(e, Error::RomTooLarge { len, max } if len == flash - 255 && max == flash as u64 - 256),
        "{e:?}"
    );
    assert_eq!(t.requests, 1, "only Hello was sent");
    assert_eq!(t.board.work.chunks, 0);
}

#[test]
fn empty_rom_is_refused() {
    let mut t = Faulty::new(SimBoard::new(FLASH), &[]);
    assert!(matches!(load(&mut t, &[]), Err(Error::RomEmpty)));
    assert_eq!(t.requests, 1);
}

#[test]
fn protocol_version_mismatch_is_an_error() {
    let mut board = SimBoard::new(FLASH);
    const V: u16 = gbc_link::PROTOCOL_VERSION;
    board.protocol = V + 1;
    let mut t = Faulty::new(board, &[]);
    let e = load(&mut t, &rom(100)).unwrap_err();
    assert!(
        matches!(e, Error::Version { board, ours } if board == V + 1 && ours == V),
        "{e:?}"
    );
    assert_eq!(t.requests, 1, "nothing after Hello");
}

#[test]
fn read_back_hash_mismatch_is_an_error() {
    let rom = rom(9000);
    let mut board = SimBoard::new(FLASH);
    board.corrupt_flash_at = Some(4321);
    let mut t = Faulty::new(board, &[]);
    let e = load(&mut t, &rom).unwrap_err();
    let mut bad = rom.clone();
    bad[4321] ^= 1;
    assert!(
        matches!(e, Error::HashMismatch { sent, board }
            if sent == fnv::hash(&rom) && board == fnv::hash(&bad)),
        "{e:?}"
    );
}

#[test]
fn hello_flushes_with_a_leading_zero() {
    // A half-received frame at the board is ended by the leading 0x00,
    // so the board still decodes Hello.
    let mut board = SimBoard::new(FLASH);
    board.receive(&[0x05, 0x11, 0x22]);
    let mut c = Client::new(board, cfg());
    let info = c.hello().unwrap();
    assert_eq!(info.flash_len, FLASH as u32);
    assert_eq!(
        c.stats.naks, 1,
        "the flushed garbage is Nak'd, then ignored"
    );
}
