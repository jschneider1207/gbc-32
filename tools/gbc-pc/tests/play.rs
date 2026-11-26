//! The `play` loop (M5) against the simulated board with a scripted
//! keyboard and a stepped clock: `Input` frames on change and on the
//! refresh, F5 save dumps, `Stats`, and Naks that answer nothing.

use std::cell::Cell;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use gbc_link::buttons as b;
use gbc_pc::input::{InputSchedule, Key, KeySet};
use gbc_pc::play::{Exit, Keyboard, PlayEvent, PlayOptions, play};
use gbc_pc::save::part_path;
use gbc_pc::sim::{DumpFault, EVERY_DUMP, SimBoard};
use gbc_pc::{Client, Config};

const STEP: Duration = Duration::from_millis(10);

/// One `KeySet` per poll; after the script, Esc.
struct Script(Vec<KeySet>, usize);

impl Keyboard for Script {
    fn held(&mut self) -> io::Result<KeySet> {
        let k = self
            .0
            .get(self.1)
            .copied()
            .unwrap_or(KeySet::of(&[Key::Esc]));
        self.1 += 1;
        Ok(k)
    }
}

fn keys(k: &[Key]) -> KeySet {
    KeySet::of(k)
}

fn cfg() -> Config {
    Config {
        retries: 2,
        timeout: Duration::from_millis(100),
    }
}

/// Runs `play` with a clock that advances `STEP` per reading.
fn run(
    board: SimBoard,
    script: Vec<KeySet>,
    opts: PlayOptions,
) -> (Client<SimBoard>, gbc_pc::play::PlayReport, Vec<PlayEvent>) {
    let mut c = Client::new(board, cfg());
    let t0 = Instant::now();
    let n = Cell::new(0u32);
    let mut now = || {
        n.set(n.get() + 1);
        t0 + STEP * n.get()
    };
    let mut events = Vec::new();
    let r = play(&mut c, &mut Script(script, 0), &opts, &mut now, &mut |e| {
        events.push(e)
    })
    .unwrap();
    (c, r, events)
}

#[test]
fn schedule_sends_on_change_and_refreshes() {
    let t0 = Instant::now();
    let ms = |m| t0 + Duration::from_millis(m);
    let mut s = InputSchedule::new(Duration::from_millis(500));
    assert!(s.due(0, ms(0)), "first poll");
    assert!(!s.due(0, ms(10)));
    assert!(s.due(b::A, ms(20)), "change");
    assert!(!s.due(b::A, ms(30)));
    assert!(!s.due(b::A, ms(519)));
    assert!(s.due(b::A, ms(520)), "refresh 500 ms after the last send");
    assert!(!s.due(b::A, ms(530)));
    assert!(s.due(0, ms(540)), "release");
    assert!(!s.due(0, ms(1039)));
    assert!(s.due(0, ms(1040)));
}

#[test]
fn input_frames_on_change_then_release_at_exit() {
    let script = vec![
        keys(&[]),
        keys(&[Key::X]),
        keys(&[Key::X]),
        keys(&[Key::X, Key::Up]),
        keys(&[Key::Up]),
        keys(&[Key::Up]),
        keys(&[Key::Enter, Key::Backspace]),
    ];
    let (c, r, _) = run(SimBoard::new(1 << 20), script, PlayOptions::default());
    assert_eq!(r.exit, Exit::Esc);
    assert_eq!(
        c.transport().inputs,
        vec![0, b::A, b::A | b::UP, b::UP, b::START | b::SELECT, 0]
    );
    assert_eq!(r.inputs, 6);
}

#[test]
fn unchanged_keys_are_refreshed_every_period() {
    // 120 polls of B held, ~20 ms of clock each (two readings per loop).
    let script = vec![keys(&[Key::Z]); 120];
    let opts = PlayOptions {
        refresh: Duration::from_millis(500),
        ..PlayOptions::default()
    };
    let (c, _, _) = run(SimBoard::new(1 << 20), script, opts);
    let sent = &c.transport().inputs;
    // First send, then one per 500 ms over 2.4 s, then the release.
    assert_eq!(sent.len(), 1 + 4 + 1, "{sent:?}");
    assert!(sent[..5].iter().all(|&x| x == b::B));
    assert_eq!(sent[5], 0);
}

#[test]
fn seconds_bound_ends_play_without_esc() {
    let script = vec![keys(&[]); 1000];
    let opts = PlayOptions {
        duration: Some(Duration::from_millis(200)),
        ..PlayOptions::default()
    };
    let (_, r, _) = run(SimBoard::new(1 << 20), script, opts);
    assert_eq!(r.exit, Exit::Time);
}

fn sav_out(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("play-{name}"));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.join("game.sav")
}

#[test]
fn f5_dumps_the_save_once_per_press() {
    let save: Vec<u8> = (0..9000u32).map(|i| (i % 253) as u8).collect();
    let mut board = SimBoard::new(1 << 20);
    board.save = Some(save.clone());
    board.push_stats(60, 1000, 2000);
    let out = sav_out("f5");
    let mut script = vec![keys(&[])];
    script.extend(vec![keys(&[Key::F5]); 30]); // held past the dump: still one
    script.extend(vec![keys(&[]); 20]);
    let opts = PlayOptions {
        sav_out: Some(out.clone()),
        ..PlayOptions::default()
    };
    let (c, r, events) = run(board, script, opts);
    assert_eq!(std::fs::read(&out).unwrap(), save);
    assert!(!part_path(&out).exists());
    assert_eq!(c.transport().work.dumps, 1);
    assert_eq!(r.saves, 1);
    assert!(events.contains(&PlayEvent::Stats {
        frames: 60,
        cycles_mean: 1000,
        cycles_max: 2000
    }));
    assert!(events.contains(&PlayEvent::Saved {
        path: out,
        len: save.len()
    }));
}

#[test]
fn broken_dump_in_play_writes_no_file_and_play_goes_on() {
    let mut board = SimBoard::new(1 << 20);
    board.save = Some(vec![7; 5000]);
    board.dump_faults = vec![(EVERY_DUMP, DumpFault::Truncate(2))];
    let out = sav_out("broken");
    let mut script = vec![keys(&[Key::F5])];
    script.extend(vec![keys(&[]); 100]);
    script.push(keys(&[Key::X]));
    let opts = PlayOptions {
        sav_out: Some(out.clone()),
        ..PlayOptions::default()
    };
    let (c, r, events) = run(board, script, opts);
    assert!(!out.exists());
    assert!(!part_path(&out).exists());
    assert_eq!(r.dumps_failed, 1);
    assert_eq!(c.transport().work.dumps, 3, "first try + 2 retries");
    assert!(
        matches!(events.last(), Some(PlayEvent::DumpFailed(_))),
        "{events:?}"
    );
    // Input still flowed after the failure.
    assert_eq!(c.transport().inputs[c.transport().inputs.len() - 2], b::A);
}
