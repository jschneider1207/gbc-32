//! Save load and dump (M5) against the simulated board (`gbc_pc::sim`):
//! the bytes arrive exactly, the dump file equals what the board holds,
//! and every broken dump leaves no output file.

use std::path::PathBuf;
use std::time::Duration;

use gbc_link::NakCode;
use gbc_pc::save::{DumpError, dump_to_file, part_path};
use gbc_pc::sim::{DumpFault, EVERY_DUMP, Fault, Faulty, SimBoard};
use gbc_pc::{Client, Config, Error, Failure, fnv};

fn cfg() -> Config {
    Config {
        retries: 3,
        timeout: Duration::from_millis(50),
    }
}

/// A save-sized pattern: 32 KiB RAM + a 48-byte RTC footer, not a whole
/// number of chunks.
fn sav(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
}

fn board_with_save(save: &[u8]) -> SimBoard {
    let mut b = SimBoard::new(1 << 20);
    b.save = Some(save.to_vec());
    b
}

/// A fresh, empty directory under the target dir for one test.
fn out_dir(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("save-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn assert_no_output(path: &std::path::Path) {
    assert!(!path.exists(), "{} written", path.display());
    assert!(!part_path(path).exists(), "temp file left behind");
}

#[test]
fn load_rom_then_save_board_gets_exact_bytes() {
    let rom = sav(40_000);
    let save = sav(32 * 1024 + 48);
    let mut board = SimBoard::new(1 << 20);
    board.save_len = Some(save.len() as u32);
    let mut c = Client::new(board, cfg());
    c.load(&rom, &mut |_, _| {}).unwrap();
    let mut last = (0, 0);
    c.send_save(&save, &mut |a, b| last = (a, b)).unwrap();
    let b = c.transport();
    assert_eq!(b.save.as_deref(), Some(&save[..]));
    assert_eq!(b.work.save_begins, 1);
    assert_eq!(b.work.save_chunks as usize, save.len().div_ceil(1024));
    assert_eq!(last, (save.len(), save.len()));
    assert_eq!(c.stats, Default::default());
}

#[test]
fn save_chunks_follow_the_boards_chunk_size() {
    let save = sav(8 * 1024);
    let mut board = SimBoard::new(1 << 20);
    board.max_chunk = 256;
    let mut c = Client::new(board, cfg());
    c.send_save(&save, &mut |_, _| {}).unwrap();
    assert_eq!(c.transport().work.save_chunks, 32);
    assert_eq!(c.transport().save.as_deref(), Some(&save[..]));
}

#[test]
fn corrupted_save_chunk_on_the_link_is_resent() {
    let save = sav(5000);
    // Requests: 0 Hello, 1 SaveBegin, 2.. chunks.
    let link = Faulty::new(
        SimBoard::new(1 << 20),
        &[(3, Fault::CorruptRequest), (4, Fault::DropReply)],
    );
    let mut c = Client::new(link, cfg());
    c.send_save(&save, &mut |_, _| {}).unwrap();
    assert_eq!(c.transport().board.save.as_deref(), Some(&save[..]));
    assert_eq!(c.stats.resends, 2);
}

#[test]
fn wrong_size_save_is_refused_and_nothing_applied() {
    let save = sav(1000);
    let mut board = board_with_save(&[1, 2, 3]);
    board.save_len = Some(8192);
    let mut c = Client::new(board, cfg());
    let e = c.send_save(&save, &mut |_, _| {}).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                request: "SaveBegin",
                last: Failure::Nak(NakCode::OutOfRange),
                ..
            }
        ),
        "{e}"
    );
    assert_eq!(c.transport().save.as_deref(), Some(&[1u8, 2, 3][..]));
}

#[test]
fn save_failing_the_boards_hash_check_is_an_error_and_not_applied() {
    let save = sav(3000);
    let mut board = board_with_save(&[9; 16]);
    board.corrupt_save_at = Some(2500);
    let mut c = Client::new(board, cfg());
    let e = c.send_save(&save, &mut |_, _| {}).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                request: "SaveChunk",
                last: Failure::Nak(NakCode::HashMismatch),
                ..
            }
        ),
        "{e}"
    );
    assert_eq!(c.transport().save.as_deref(), Some(&[9u8; 16][..]));
}

#[test]
fn empty_save_is_refused_locally() {
    let mut c = Client::new(SimBoard::new(1 << 20), cfg());
    assert!(matches!(
        c.send_save(&[], &mut |_, _| {}),
        Err(Error::SaveEmpty)
    ));
    assert_eq!(c.transport().work.save_begins, 0);
}

#[test]
fn dump_file_equals_the_boards_save() {
    let save = sav(32 * 1024 + 48);
    let dir = out_dir("ok");
    let out = dir.join("game.sav");
    std::fs::write(&out, b"old contents").unwrap();
    let mut board = board_with_save(&save);
    board.dump_faults = vec![(EVERY_DUMP, DumpFault::StatsBetween)];
    let mut c = Client::new(board, cfg());
    assert_eq!(
        dump_to_file(&mut c, &out, None).unwrap(),
        (save.len(), false)
    );
    assert_eq!(std::fs::read(&out).unwrap(), save);
    assert!(!part_path(&out).exists());
    assert_eq!(c.transport().work.dumps, 1);
    assert_eq!(c.stats.other as usize, save.len().div_ceil(1024));
}

#[test]
fn load_then_dump_round_trips_byte_identically() {
    let save = sav(8 * 1024 + 48);
    let mut c = Client::new(SimBoard::new(1 << 20), cfg());
    c.send_save(&save, &mut |_, _| {}).unwrap();
    assert_eq!(c.dump_save().unwrap(), save);
}

#[test]
fn a_dump_broken_once_is_retried_with_a_fresh_request() {
    let save = sav(10_000);
    for fault in [
        DumpFault::Truncate(3),
        DumpFault::CorruptChunk(2),
        DumpFault::BadHash,
    ] {
        let mut board = board_with_save(&save);
        board.dump_faults = vec![(0, fault)];
        let mut c = Client::new(board, cfg());
        assert_eq!(c.dump_save().unwrap(), save, "{fault:?}");
        assert_eq!(c.transport().work.dumps, 2, "{fault:?}");
        assert_eq!(c.stats.resends, 1, "{fault:?}");
    }
}

#[test]
fn broken_dumps_are_errors_and_write_no_file() {
    let save = sav(10_000);
    let cases = [
        (DumpFault::Truncate(4), Failure::Timeout),
        (
            DumpFault::CorruptChunk(1),
            Failure::Dump(DumpError::Corrupt),
        ),
        (
            DumpFault::BadHash,
            Failure::Dump(DumpError::HashMismatch {
                expected: fnv::hash(&save) ^ 1,
                got: fnv::hash(&save),
            }),
        ),
    ];
    for (i, (fault, want)) in cases.into_iter().enumerate() {
        let dir = out_dir(&format!("broken-{i}"));
        let out = dir.join("game.sav");
        let mut board = board_with_save(&save);
        board.dump_faults = vec![(EVERY_DUMP, fault)];
        let mut c = Client::new(board, cfg());
        let e = dump_to_file(&mut c, &out, None).unwrap_err();
        assert!(
            matches!(e, Error::GaveUp { request: "SaveDumpRequest", attempts: 4, last } if last == want),
            "{fault:?}: {e}"
        );
        assert_no_output(&out);
    }
}

#[test]
fn nak_to_a_dump_request_is_an_error_and_writes_no_file() {
    let dir = out_dir("nak");
    let out = dir.join("game.sav");
    let mut c = Client::new(SimBoard::new(1 << 20), cfg());
    let e = dump_to_file(&mut c, &out, None).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                last: Failure::Nak(NakCode::UnexpectedType),
                ..
            }
        ),
        "{e}"
    );
    assert_no_output(&out);
}

#[test]
fn silent_board_dump_times_out_and_writes_no_file() {
    let dir = out_dir("silent");
    let out = dir.join("game.sav");
    let link = Faulty::new(board_with_save(&sav(100)), &[(0, Fault::SilenceForever)]);
    let mut c = Client::new(link, cfg());
    let e = dump_to_file(&mut c, &out, None).unwrap_err();
    assert!(
        matches!(
            e,
            Error::GaveUp {
                last: Failure::Timeout,
                ..
            }
        ),
        "{e}"
    );
    assert_no_output(&out);
}

#[test]
fn dump_receiver_checks_order_and_seq() {
    use gbc_pc::Incoming;
    use gbc_pc::save::{DumpRx, DumpStep};
    let data = sav(3000);
    let begin = Incoming::SaveBegin {
        len: 3000,
        hash: fnv::hash(&data),
    };
    let chunk = |o: usize, n: usize| Incoming::SaveChunk {
        offset: o as u32,
        data: data[o..o + n].to_vec(),
    };
    // In order, with a stale frame (older seq) in between: done.
    let mut rx = DumpRx::new(Some(9));
    assert_eq!(rx.on_frame(9, &begin), DumpStep::Pending);
    assert_eq!(rx.on_frame(9, &chunk(0, 1024)), DumpStep::Pending);
    assert_eq!(rx.on_frame(8, &chunk(2048, 952)), DumpStep::Pending);
    assert_eq!(rx.on_frame(9, &chunk(1024, 1024)), DumpStep::Pending);
    assert_eq!(
        rx.on_frame(9, &chunk(2048, 952)),
        DumpStep::Done(data.clone())
    );
    // A lost chunk: the next one is a gap.
    let mut rx = DumpRx::new(Some(9));
    rx.on_frame(9, &begin);
    rx.on_frame(9, &chunk(0, 1024));
    assert_eq!(
        rx.on_frame(9, &chunk(2048, 952)),
        DumpStep::Failed(Failure::Dump(DumpError::Gap {
            expected: 1024,
            offset: 2048,
            len: 952
        }))
    );
    // Past the end.
    let mut rx = DumpRx::new(Some(1));
    rx.on_frame(1, &Incoming::SaveBegin { len: 10, hash: 0 });
    assert!(matches!(
        rx.on_frame(1, &chunk(0, 11)),
        DumpStep::Failed(Failure::Dump(DumpError::Gap { .. }))
    ));
    // Board-started (any seq): adopts the SaveBegin's seq.
    let mut rx = DumpRx::new(None);
    rx.on_frame(200, &begin);
    assert_eq!(rx.seq(), Some(200));
    assert_eq!(rx.on_frame(201, &chunk(0, 3000)), DumpStep::Pending);
    assert_eq!(rx.on_frame(200, &chunk(0, 3000)), DumpStep::Done(data));
    // Absurd lengths are refused before allocating.
    let mut rx = DumpRx::new(Some(1));
    assert!(matches!(
        rx.on_frame(
            1,
            &Incoming::SaveBegin {
                len: u32::MAX,
                hash: 0
            }
        ),
        DumpStep::Failed(Failure::Dump(DumpError::BadLength(u32::MAX)))
    ));
}

#[test]
fn write_atomic_failure_leaves_no_temp_file() {
    use gbc_pc::save::write_atomic;
    let dir = out_dir("atomic");
    // The target is a non-empty directory: the rename fails.
    let target = dir.join("game.sav");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("keep"), b"x").unwrap();
    assert!(write_atomic(&target, b"data").is_err());
    assert!(!part_path(&target).exists());
    assert!(target.join("keep").exists());
    // And a normal write replaces the old file.
    let f = dir.join("ok.sav");
    std::fs::write(&f, b"old").unwrap();
    write_atomic(&f, b"new").unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), b"new");
}
