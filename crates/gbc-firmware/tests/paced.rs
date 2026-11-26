//! The audio-paced loop (`gbc_firmware::paced`): its compared lines equal
//! an independent straight `run_frame` loop's, whatever the consumer does;
//! the pacing rule never overruns the ring; idle is called exactly when the
//! ring is full (and while draining).
//!
//! A tiny synthetic ROM (always available), and cgb_sound from
//! `test-roms/` (skipped when absent, unless `GBC_REQUIRE_TEST_DATA=1`;
//! `GBC_TEST_ROMS` overrides the directory).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;

use gbc_core::{
    Buttons, Config, Emulator, Events, Frame, Memory, StereoSample, WHITE, cart_ram_size,
};
use gbc_firmware::CycleClock;
use gbc_firmware::audio::{AudioRing, FeedRing, FeedTap, dac_feed_hash};
use gbc_firmware::buttons::{FRAME_SAMPLES, LiveInput, Raw, RawInputs, Shared};
use gbc_firmware::fnv;
use gbc_firmware::headless::{Buffers, FrameSink, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{Press, Script};
use gbc_firmware::paced::{self, Error, Feed, Hooks, Idle, Report, Service, Serviced};
use gbc_firmware::save;

/// Counts its own reads (deterministic stats; they aren't compared).
struct CountingClock(Cell<u32>);

impl CycleClock for CountingClock {
    fn now(&self) -> u32 {
        let t = self.0.get();
        self.0.set(t.wrapping_add(1000));
        t
    }
    fn hz(&self) -> u32 {
        1_000_000
    }
}

/// 32 KiB ROM-only: starts a square wave on channel 1, then forever
/// copies the action buttons (P1) to BGP, so input changes the frame and
/// the audio is not silence (same program as tests/headless.rs).
fn synthetic_rom() -> Vec<u8> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xc3, 0x50, 0x01]);
    let code = [
        0x3e, 0x80, 0xe0, 0x26, // NR52 = $80
        0x3e, 0x77, 0xe0, 0x24, // NR50 = $77
        0x3e, 0xff, 0xe0, 0x25, // NR51 = $ff
        0x3e, 0x80, 0xe0, 0x11, // NR11 = $80
        0x3e, 0xf0, 0xe0, 0x12, // NR12 = $f0
        0x3e, 0x00, 0xe0, 0x13, // NR13 = 0
        0x3e, 0x87, 0xe0, 0x14, // NR14 = $87: trigger
        0x3e, 0x10, 0xe0, 0x00, // loop: P1 = $10
        0xf0, 0x00, 0xf0, 0x00, // read P1 twice
        0xe0, 0x47, // BGP = P1
        0x18, 0xf4, // jr loop
    ];
    rom[0x150..0x150 + code.len()].copy_from_slice(&code);
    rom
}

const PRESSES: [Press; 2] = [
    Press {
        frame: 10,
        hold: 7,
        buttons: Buttons::A,
    },
    Press {
        frame: 40,
        hold: 30,
        buttons: Buttons::B,
    },
];

fn compared(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("paced "))
        .map(String::from)
        .collect()
}

/// The compared lines, computed without `gbc_firmware::paced`, the ring,
/// the tap or `Script`: a straight `run_frame` loop that keeps every
/// sample, then `dac_feed_hash` over all of them.
/// `reload_at`: eject and load again (cart RAM zeroed) before that frame's
/// input, as `save::reload` of a zero image does.
fn reference(rom: &[u8], presses: &[Press], frames: u32, reload_at: Option<u32>) -> Vec<String> {
    let mut mem = Box::new(Memory::new());
    let mut save = vec![0u8; cart_ram_size(rom).unwrap()];
    let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
    let mut audio = vec![StereoSample::default(); MIN_AUDIO_SLOTS];
    let mut emu = Emulator::new(&mut mem);
    assert!(emu.load(rom, &mut save, Config::default()).is_ok());
    let mut held = Buttons::EMPTY;
    let mut all = Vec::new();
    let mut fb_bytes = Vec::new();
    let mut ready = 0;
    for n in 0..frames {
        if Some(n) == reload_at {
            let c = emu.eject().unwrap();
            c.cart_ram.fill(0);
            assert!(emu.load(c.rom, c.cart_ram, Config::default()).is_ok());
            held = Buttons::EMPTY;
        }
        let b = presses
            .iter()
            .filter(|p| n >= p.frame && n - p.frame < p.hold)
            .fold(Buttons::EMPTY, |b, p| b | p.buttons);
        if b != held {
            emu.set_buttons(b);
            held = b;
        }
        let r = emu.run_frame(&mut frame, &mut audio);
        all.extend_from_slice(&audio[..r.audio_samples]);
        if r.events.contains(Events::FRAME_READY) {
            let px: Vec<u8> = frame.iter().flat_map(|p| p.to_le_bytes()).collect();
            fb_bytes.extend_from_slice(&fnv::hash(&px).to_le_bytes());
            ready += 1;
        }
    }
    let mut table = Vec::new();
    for p in presses {
        table.extend_from_slice(&p.frame.to_le_bytes());
        table.extend_from_slice(&p.hold.to_le_bytes());
        table.push(p.buttons.bits());
    }
    let feed = dac_feed_hash(&all);
    vec![
        format!(
            "paced v1 rom_bytes={} rom_fnv={:016x} input_fnv={:016x} frames={frames}",
            rom.len(),
            fnv::hash(rom),
            fnv::hash(&table)
        ),
        format!(
            "paced end frames={frames} clock={} ready={ready} fbs={:016x} feed_frames={} feed={feed:016x}",
            emu.clock(),
            fnv::hash(&fb_bytes),
            all.len()
        ),
        format!("paced drain feed_frames={} feed={feed:016x}", all.len()),
    ]
}

/// A simulated consumer. `idle_pop` frames are popped per idle call (the
/// ISR during `WFI`); `sink_pop(n)` frames on each completed frame (the
/// ISR firing while the loop runs). Every idle call records the ring's
/// free space and the frames pushed so far.
struct Consumer<'a, const N: usize> {
    ring: &'a AudioRing<N>,
    tap: &'a FeedTap,
    idle_pop: usize,
    sink_pop: fn(u32) -> usize,
    idles: RefCell<Vec<(usize, u64)>>,
}

impl<const N: usize> Consumer<'_, N> {
    fn pop(&self, k: usize) {
        // An empty pop would still set the ring's min_fill watermark.
        if k == 0 {
            return;
        }
        let mut out = vec![0u32; k];
        self.tap.pop_into(self.ring, &mut out);
    }
}

struct IdleSide<'c, 'a, const N: usize>(&'c Consumer<'a, N>);
struct SinkSide<'c, 'a, const N: usize>(&'c Consumer<'a, N>);

impl<const N: usize> Idle for IdleSide<'_, '_, N> {
    fn idle(&mut self) {
        let c = self.0;
        let pushed = c.ring.fill() as u64 + u64::from(c.tap.frames());
        c.idles.borrow_mut().push((c.ring.free(), pushed));
        c.pop(c.idle_pop);
    }
}

impl<const N: usize> FrameSink for SinkSide<'_, '_, N> {
    fn frame(&mut self, n: u32, _frame: &Frame) {
        let c = self.0;
        c.pop((c.sink_pop)(n));
    }
}

struct Run {
    out: String,
    report: Report,
    idles: Vec<(usize, u64)>,
}

fn run_paced<const N: usize>(
    rom: &[u8],
    presses: &[Press],
    frames: u32,
    idle_pop: usize,
    sink_pop: fn(u32) -> usize,
) -> Run {
    let ring = Box::new(AudioRing::<N>::new());
    let tap = FeedTap::new();
    let consumer = Consumer {
        ring: &ring,
        tap: &tap,
        idle_pop,
        sink_pop,
        idles: RefCell::new(Vec::new()),
    };
    let mut mem = Box::new(Memory::new());
    let mut save = vec![0u8; cart_ram_size(rom).unwrap()];
    let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
    let mut audio = vec![StereoSample { left: 9, right: 9 }; 1000];
    let bufs = Buffers {
        mem: &mut mem,
        save_ram: &mut save,
        frame: &mut frame,
        audio: &mut audio,
    };
    let clock = CountingClock(Cell::new(0));
    let mut out = String::new();
    let report = paced::run(
        rom,
        Script::new(presses),
        frames,
        bufs,
        Feed {
            ring: &ring,
            tap: &tap,
        },
        Hooks {
            clock: &clock,
            sink: &mut SinkSide(&consumer),
            idle: &mut IdleSide(&consumer),
        },
        &mut out,
    )
    .unwrap();
    Run {
        out,
        report,
        idles: consumer.idles.into_inner(),
    }
}

fn none(_: u32) -> usize {
    0
}

/// Consumer patterns: (name, ring size 2048 or 1024, idle pop, sink pop).
type Pattern = (&'static str, bool, usize, fn(u32) -> usize);
const PATTERNS: [Pattern; 7] = [
    ("dma halves in idle (the host tool)", true, 256, none),
    ("starved producer: 1 frame per idle", true, 1, none),
    ("whole ring per idle", true, 2048, none),
    ("small ring", false, 256, none),
    ("eager consumer: underruns", true, 256, |_| 1200),
    ("mixed", true, 7, |n| (n as usize * 37) % 700),
    ("bursty", false, 3, |n| if n % 9 == 0 { 4000 } else { 0 }),
];

fn run_pattern(rom: &[u8], frames: u32, p: &Pattern) -> Run {
    let (_, big, idle_pop, sink_pop) = *p;
    if big {
        run_paced::<{ gbc_firmware::audio::RING_FRAMES }>(rom, &PRESSES, frames, idle_pop, sink_pop)
    } else {
        run_paced::<1024>(rom, &PRESSES, frames, idle_pop, sink_pop)
    }
}

/// The compared lines equal the straight reference loop, for every
/// consumer pattern (so pacing never changes them).
#[test]
fn compared_lines_match_a_straight_loop_at_any_consumer_rate() {
    let rom = synthetic_rom();
    let frames = 120;
    let want = reference(&rom, &PRESSES, frames, None);
    let mut saw_underrun = false;
    let mut saw_starving = false;
    for p in &PATTERNS {
        let r = run_pattern(&rom, frames, p);
        assert_eq!(compared(&r.out), want, "{}:\n{}", p.0, r.out);
        saw_underrun |= r.report.ring.underrun_events > 0;
        saw_starving |= r.report.idle_calls > 10_000;
    }
    // The patterns do exercise both extremes.
    assert!(saw_underrun && saw_starving);
}

/// The pacing rule never overruns the ring, and idle is called only when
/// the ring has no room for a call's samples, except while draining.
#[test]
fn never_overruns_and_idles_only_when_full() {
    let rom = synthetic_rom();
    for p in &PATTERNS {
        let r = run_pattern(&rom, 120, p);
        let s = r.report.ring;
        assert_eq!((s.overrun_events, s.overrun_frames), (0, 0), "{}", p.0);
        assert_eq!(r.report.drained_frames as u64, r.report.feed_frames);
        let total = r.report.feed_frames;
        let mut full = 0;
        for &(free, pushed) in &r.idles {
            if pushed < total {
                assert!(free < MIN_AUDIO_SLOTS, "{}: idle with {free} free", p.0);
                full += 1;
            }
        }
        assert_eq!(full, r.report.idle_calls, "{}", p.0);
        assert_eq!(
            r.idles.len() as u64,
            r.report.idle_calls + r.report.drain_idle_calls
        );
        // Unless the sink pops a lot, the ring fills and the loop idles.
        if (0..120).all(|n| (p.3)(n) == 0) {
            assert!(r.report.idle_calls > 0, "{}", p.0);
        }
    }
}

#[test]
fn stats_lines_are_report_only_and_well_formed() {
    let rom = synthetic_rom();
    let r = run_pattern(&rom, 30, &PATTERNS[0]);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 3 + 6, "{}", r.out);
    assert!(lines[3..].iter().all(|l| l.starts_with("stats paced ")));
    // Counting clock: 1000 per read, two reads per run_frame.
    assert_eq!(
        lines[3],
        "stats paced core min=1000 mean=1000 max=1000 max_frame=0 hz=1000000"
    );
    assert!(
        lines[8].starts_with("stats paced speed elapsed="),
        "{}",
        lines[8]
    );
    assert_eq!(
        lines[6],
        "stats paced ring min_fill=1243 max_fill=2047 underrun_events=0 underrun_frames=0 overrun_events=0 overrun_frames=0",
        "{}",
        r.out
    );
}

#[test]
fn refuses_a_used_ring() {
    let rom = synthetic_rom();
    let ring = FeedRing::new();
    let tap = FeedTap::new();
    ring.push_frames(&[1, 2, 3]);
    let mut mem = Box::new(Memory::new());
    let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
    let mut audio = vec![StereoSample::default(); MIN_AUDIO_SLOTS];
    let mut out = String::new();
    let r = paced::run(
        &rom,
        Script::EMPTY,
        5,
        Buffers {
            mem: &mut mem,
            save_ram: &mut [],
            frame: &mut frame,
            audio: &mut audio,
        },
        Feed {
            ring: &ring,
            tap: &tap,
        },
        Hooks {
            clock: &CountingClock(Cell::new(0)),
            sink: &mut gbc_firmware::headless::NoSink,
            idle: &mut NeverIdle,
        },
        &mut out,
    );
    assert_eq!(
        r,
        Err(Error::NotFresh {
            fill: 3,
            tap_frames: 0
        })
    );
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1], "paced error not_fresh fill=3 tap_frames=0");
}

struct NeverIdle;
impl Idle for NeverIdle {
    fn idle(&mut self) {
        panic!("idle");
    }
}

fn test_rom(rel: &str) -> Option<Vec<u8>> {
    let dir = std::env::var_os("GBC_TEST_ROMS").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-roms"),
        PathBuf::from,
    );
    let path = dir.join(rel);
    match std::fs::read(&path) {
        Ok(r) => Some(r),
        Err(e) => {
            assert!(
                std::env::var("GBC_REQUIRE_TEST_DATA").as_deref() != Ok("1"),
                "{}: {e} (GBC_REQUIRE_TEST_DATA=1)",
                path.display()
            );
            eprintln!("SKIPPED: {}: {e}", path.display());
            None
        }
    }
}

/// Real sound content: cgb_sound, 600 calls, against the reference, with
/// the host tool's consumer and a starved one.
#[test]
fn cgb_sound_matches_the_reference() {
    let Some(rom) = test_rom("blargg/cgb_sound/cgb_sound.gb") else {
        return;
    };
    let want = reference(&rom, &PRESSES, 600, None);
    for p in [&PATTERNS[0], &PATTERNS[5]] {
        let r = run_pattern(&rom, 600, p);
        assert_eq!(compared(&r.out), want, "{}", p.0);
    }
}

/// A battery-backed copy of the synthetic ROM (MBC5+RAM+BATTERY, 8 KiB),
/// so `save::reload` accepts it; the program is the same.
fn battery_rom() -> Vec<u8> {
    let mut rom = synthetic_rom();
    rom[0x147] = 0x1B;
    rom[0x149] = 0x02;
    rom
}

struct LogPins<'a>(&'a RefCell<Vec<(char, u32)>>);
impl RawInputs for LogPins<'_> {
    fn read(&mut self) -> Raw {
        let mut log = self.0.borrow_mut();
        let n = log.iter().filter(|e| e.0 == 'i').count() as u32;
        log.push(('i', n));
        Raw::NONE
    }
}

/// The player-loop hook under test: feeds the script's buttons as PC
/// input frames, logs its calls, and reloads a zero save at `reload_at`.
struct Svc<'a> {
    shared: &'a Shared,
    log: &'a RefCell<Vec<(char, u32)>>,
    reload_at: Option<u32>,
    zero: Vec<u8>,
}
impl Service for Svc<'_> {
    fn service(&mut self, n: u32, emu: &mut Emulator<'_>) -> Serviced {
        self.log.borrow_mut().push(('s', n));
        self.shared.set_pc(Script::new(&PRESSES).held(n).bits());
        if Some(n) == self.reload_at {
            let layout = save::Layout::of_emulator(emu).unwrap();
            let img = save::check(layout, &self.zero).unwrap();
            save::reload(emu, &img, Config::default()).unwrap();
            return Serviced::Reloaded;
        }
        Serviced::Continue
    }
}

fn run_live(
    rom: &[u8],
    frames: u32,
    reload_at: Option<u32>,
) -> (Vec<String>, Report, Vec<(char, u32)>) {
    let ring = Box::new(AudioRing::<{ gbc_firmware::audio::RING_FRAMES }>::new());
    let tap = FeedTap::new();
    let consumer = Consumer {
        ring: &ring,
        tap: &tap,
        idle_pop: 256,
        sink_pop: none,
        idles: RefCell::new(Vec::new()),
    };
    let mut mem = Box::new(Memory::new());
    let mut save_ram = vec![0u8; cart_ram_size(rom).unwrap()];
    let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
    let mut audio = vec![StereoSample::default(); 1000];
    let bufs = Buffers {
        mem: &mut mem,
        save_ram: &mut save_ram,
        frame: &mut frame,
        audio: &mut audio,
    };
    let log = RefCell::new(Vec::new());
    let shared = Shared::new();
    let mut input = LiveInput::<_, FRAME_SAMPLES>::new(LogPins(&log), &shared);
    let mut svc = Svc {
        shared: &shared,
        log: &log,
        reload_at,
        zero: vec![0; 0x2000],
    };
    let clock = CountingClock(Cell::new(0));
    let mut out = String::new();
    let report = paced::run_with(
        rom,
        &mut input,
        frames,
        bufs,
        Feed {
            ring: &ring,
            tap: &tap,
        },
        Hooks {
            clock: &clock,
            sink: &mut SinkSide(&consumer),
            idle: &mut IdleSide(&consumer),
        },
        &mut svc,
        &mut out,
    )
    .unwrap();
    (compared(&out), report, log.into_inner())
}

/// Live input (PC frames through the service hook, pins idle) replaying
/// the script gives the script run's state lines; the hook runs once per
/// frame, before that frame's input sample.
#[test]
fn run_with_live_input_and_service_matches_the_script() {
    let rom = battery_rom();
    let frames = 90;
    let (got, report, log) = run_live(&rom, frames, None);
    let want = reference(&rom, &PRESSES, frames, None);
    assert_eq!(&got[1..], &want[1..]);
    assert!(got[0].contains("input_fnv=0000000000000000"), "{}", got[0]);
    let expect: Vec<(char, u32)> = (0..frames).flat_map(|n| [('s', n), ('i', n)]).collect();
    assert_eq!(log, expect);
    assert_eq!(report.emulated, report.clock);
}

/// A reload from the hook in the middle of a held button (B, frames
/// 40..70) re-applies it, and the speed figures count the clocks before
/// the reload.
#[test]
fn run_with_reload_reapplies_buttons_and_counts_clocks() {
    let rom = battery_rom();
    let frames = 90;
    let (got, report, _) = run_live(&rom, frames, Some(50));
    let want = reference(&rom, &PRESSES, frames, Some(50));
    assert_eq!(&got[1..], &want[1..]);
    // The emulated total adds the clock at the reload (after 50 calls,
    // the same as a 50-frame run's end) to the clock since.
    let (_, first, _) = run_live(&rom, 50, None);
    assert!(report.clock < report.emulated);
    assert_eq!(report.emulated, first.clock + report.clock);
}
