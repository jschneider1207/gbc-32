//! Scripted input for the board's replays (Stage 2 M5, "Scripted input
//! mode"): converts a manifest group's `input` into the firmware's clock
//! table, and checks that the firmware's paced loop with that table
//! emulates what romtest's own run loop emulates.
//!
//! ```text
//! replay-input table --manifest <toml> --group <id>
//! replay-input check --manifest <toml> --group <id> --rom <path>
//! ```
//!
//! Both read the manifest with romtest's parser (`romtest::manifest`,
//! which rounds each time down to an M-cycle) and build the change list
//! with romtest's `machine::input_timeline`, so the conversion is
//! romtest's code, not a copy.
//!
//! `table` prints the list in the `clocks v1` text format
//! (`gbc_firmware::replay`), for `headless --paced --input` and the
//! `player-script` image (`GBC_HEADLESS_INPUT`), with `end <N>`,
//! N = ceil(limit / FRAME_CLOCKS) + 1 run calls for the group's exit limit
//! (each call is at most FRAME_CLOCKS, so N calls cover romtest's run). It
//! refuses (exit 1) a list in which more than
//! `gbc_core::SCHEDULED_INPUTS` changes fall within two frames: the loop
//! hands changes over between frames, so such a change could be late
//! (`gbc_firmware::replay` docs, "Lateness").
//!
//! `check` runs the ROM twice from the same start (`Config::default`, cart
//! RAM zeroed: the firmware's `headless::start`):
//! 1. romtest's `machine::run` with the group's `RunPlan` (its exit and
//!    input), on an `Emulator` wrapped so every `FRAME_READY` frame's hash
//!    is recorded (the wrapper only observes; the calls are romtest's);
//! 2. `gbc_firmware::paced::run_with` for the table's N calls with a
//!    `ClockScript` of the parsed table text, recording every frame hash.
//!
//! and prints the romtest run's frame count and hash chain, the paced
//! run's chain over the same frames, its `stats input` line, and
//! `equal=yes` (exit 0) when the paced run's first F frames are romtest's
//! F frames, romtest completed its run, and no change was late; else
//! `equal=no` (exit 1). Exit 2: usage, file or manifest error.
//!
//! Frame hashes are the `headless` `fb=` value (FNV-1a of the frame
//! buffer); the chain folds them as `paced`'s `fbs=` does.

use std::fmt::Write as _;
use std::process::ExitCode;

use gbc_core::{
    Buttons, Config, Emulator, Events, FRAME_CLOCKS, Frame, Memory, Registers, SCHEDULED_INPUTS,
    StereoSample, WHITE, cart_ram_size,
};
use gbc_firmware::CycleClock;
use gbc_firmware::audio::{DMA_HALF_FRAMES, FeedRing, FeedTap};
use gbc_firmware::fnv::Fnv1a;
use gbc_firmware::headless::{Buffers, FrameSink, MIN_AUDIO_SLOTS};
use gbc_firmware::paced::{self, Feed, Hooks, Idle, NoService};
use gbc_firmware::replay::{self, Change, ClockScript};
use romtest::machine::{self, End, Machine, RunPlan, RunState};
use romtest::manifest::{self, Group};

const USAGE: &str = "usage: replay-input table --manifest <toml> --group <id>\n       \
                     replay-input check --manifest <toml> --group <id> --rom <path>";

fn main() -> ExitCode {
    match real_main() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("replay-input: {e}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<bool, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or(USAGE)?;
    let (mut manifest_path, mut group_id, mut rom_path) = (None, None, None);
    while let Some(a) = it.next() {
        let slot = match a.as_str() {
            "--manifest" => &mut manifest_path,
            "--group" => &mut group_id,
            "--rom" => &mut rom_path,
            _ => return Err(format!("unexpected argument {a:?}\n{USAGE}")),
        };
        *slot = Some(it.next().ok_or_else(|| format!("{a} needs a value"))?);
    }
    let manifest_path = manifest_path.ok_or(USAGE)?;
    let group_id = group_id.ok_or(USAGE)?;
    let text =
        std::fs::read_to_string(&manifest_path).map_err(|e| format!("{manifest_path}: {e}"))?;
    let m = manifest::parse(&text).map_err(|e| format!("{manifest_path}: {e}"))?;
    let group = m
        .groups
        .iter()
        .find(|g| g.id == group_id)
        .ok_or_else(|| format!("{manifest_path}: no group {group_id:?}"))?;
    let plan = RunPlan::new(group.exit, &group.input);
    let calls = calls_for(plan.exit.limit_clocks());
    let table = table_text(&manifest_path, group, &plan.input, calls);
    match cmd.as_str() {
        "table" => {
            if let Err(e) = density(&plan.input) {
                eprintln!("replay-input: {e}");
                return Ok(false);
            }
            print!("{table}");
            Ok(true)
        }
        "check" => {
            let rom_path = rom_path.ok_or(USAGE)?;
            let rom = std::fs::read(&rom_path).map_err(|e| format!("{rom_path}: {e}"))?;
            check(&rom, group, &plan, &table, calls)
        }
        _ => Err(USAGE.into()),
    }
}

/// N run calls for an exit at `limit` clocks (module docs).
fn calls_for(limit: u64) -> u32 {
    let n = limit.div_ceil(u64::from(FRAME_CLOCKS)) + 1;
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Refuses a list where a change could come up after its clock.
fn density(changes: &[(u64, Buttons)]) -> Result<(), String> {
    let window = 2 * u64::from(FRAME_CLOCKS);
    for (k, w) in changes.windows(SCHEDULED_INPUTS + 1).enumerate() {
        let (first, last) = (w[0].0, w[SCHEDULED_INPUTS].0);
        if last - first < window {
            return Err(format!(
                "changes {k}..={} (clocks {first}..{last}) are {} changes within two frames; \
                 the frame loop's hand-over could apply one late",
                k + SCHEDULED_INPUTS,
                SCHEDULED_INPUTS + 1
            ));
        }
    }
    Ok(())
}

fn button_names(b: Buttons) -> String {
    const NAMES: [(Buttons, &str); 8] = [
        (Buttons::A, "a"),
        (Buttons::B, "b"),
        (Buttons::SELECT, "select"),
        (Buttons::START, "start"),
        (Buttons::RIGHT, "right"),
        (Buttons::LEFT, "left"),
        (Buttons::UP, "up"),
        (Buttons::DOWN, "down"),
    ];
    let names: Vec<&str> = NAMES
        .iter()
        .filter(|(f, _)| b.contains(*f))
        .map(|&(_, n)| n)
        .collect();
    if names.is_empty() {
        "none".into()
    } else {
        names.join("+")
    }
}

fn table_text(manifest: &str, group: &Group, changes: &[(u64, Buttons)], calls: u32) -> String {
    let mut s = String::new();
    let limit = group.exit.limit_clocks();
    let _ = writeln!(
        s,
        "# replay-input: {manifest} group {}: romtest's input_timeline (base clocks)",
        group.id
    );
    let _ = writeln!(
        s,
        "# exit {} = {limit} clocks; end = ceil({limit} / {FRAME_CLOCKS}) + 1 run calls",
        group.exit
    );
    s.push_str(replay::HEADER);
    s.push('\n');
    for &(clock, b) in changes {
        let _ = writeln!(s, "{clock} {}", button_names(b));
    }
    let _ = writeln!(s, "end {calls}");
    s
}

/// romtest's machine, observed: every frame a run call completes is
/// hashed. Every call goes straight to the emulator.
struct FrameLog<'a, 'e> {
    emu: &'a mut Emulator<'e>,
    hashes: Vec<u64>,
}

fn frame_hash(f: &Frame) -> u64 {
    let mut h = Fnv1a::new();
    h.frame(f);
    h.get()
}

impl Machine for FrameLog<'_, '_> {
    fn set_stop_events(&mut self, events: Events) {
        self.emu.set_stop_events(events);
    }
    fn set_buttons(&mut self, buttons: Buttons) {
        self.emu.set_buttons(buttons);
    }
    fn set_buttons_at(&mut self, clock: u64, buttons: Buttons) -> bool {
        self.emu.set_buttons_at(clock, buttons)
    }
    fn run_until(
        &mut self,
        target: u64,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> (Events, usize) {
        let r = self.emu.run_until(target, frame, audio);
        if r.events.contains(Events::FRAME_READY) {
            self.hashes.push(frame_hash(frame));
        }
        (r.events, r.audio_samples)
    }
    fn clock(&self) -> u64 {
        self.emu.clock()
    }
    fn registers(&self) -> Registers {
        self.emu.registers()
    }
    fn serial_byte(&self) -> u8 {
        self.emu.serial_byte()
    }
    fn screen_blank(&self) -> bool {
        self.emu.screen_blank()
    }
}

fn chain(hashes: &[u64]) -> u64 {
    let mut h = Fnv1a::new();
    for &x in hashes {
        h.u64(x);
    }
    h.get()
}

struct Log(Vec<u64>);

impl FrameSink for Log {
    fn frame(&mut self, _n: u32, frame: &Frame) {
        self.0.push(frame_hash(frame));
    }
}

struct NoClock;

impl CycleClock for NoClock {
    fn now(&self) -> u32 {
        0
    }
    fn hz(&self) -> u32 {
        0
    }
}

/// The DMA ISR's stand-in, as in `tools/headless`.
struct Consumer<'a> {
    ring: &'a FeedRing,
    tap: &'a FeedTap,
    half: [u32; DMA_HALF_FRAMES],
}

impl Idle for Consumer<'_> {
    fn idle(&mut self, _waiting: &dyn Fn() -> bool) {
        self.tap.pop_into(self.ring, &mut self.half);
    }
}

fn check(
    rom: &[u8],
    group: &Group,
    plan: &RunPlan,
    table: &str,
    calls: u32,
) -> Result<bool, String> {
    let ram_size = cart_ram_size(rom).map_err(|e| format!("ROM header: {e:?}"))?;

    // 1. romtest's run loop.
    let mut mem = Box::new(Memory::new());
    let mut ram = vec![0u8; ram_size];
    let mut emu = Emulator::new(&mut mem);
    emu.load(rom, &mut ram, Config::default())
        .map_err(|e| format!("load: {e}"))?;
    let mut log = FrameLog {
        emu: &mut emu,
        hashes: Vec::new(),
    };
    let mut st = RunState::default();
    machine::run(&mut log, plan, &mut st).map_err(|e| format!("romtest run: {e:?}"))?;
    let r = log.hashes;
    let end = st.end.map_or("none", End::name);
    println!(
        "romtest group={} end={end} frames={} clock={} fbs={:016x}",
        group.id,
        r.len(),
        st.clock,
        chain(&r)
    );

    // 2. The firmware's paced loop with the parsed table.
    let mut changes = vec![Change::NONE; table.lines().count()];
    let p = replay::parse(table, &mut changes)
        .map_err(|e| format!("table line {}: {:?}", e.line, e.kind))?;
    changes.truncate(p.changes);
    let mut script = ClockScript::new(&changes);
    let mut mem = Box::new(Memory::new());
    let mut save_ram = vec![0u8; ram_size];
    let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
    let mut audio = vec![StereoSample::default(); MIN_AUDIO_SLOTS];
    let bufs = Buffers {
        mem: &mut mem,
        save_ram: &mut save_ram,
        frame: &mut frame,
        audio: &mut audio,
    };
    let ring = Box::new(FeedRing::new());
    let tap = FeedTap::new();
    let mut consumer = Consumer {
        ring: &ring,
        tap: &tap,
        half: [0; DMA_HALF_FRAMES],
    };
    let mut sink = Log(Vec::new());
    let mut lines = String::new();
    paced::run_with(
        rom,
        &mut script,
        calls,
        bufs,
        Feed {
            ring: &ring,
            tap: &tap,
        },
        Hooks {
            clock: &NoClock,
            sink: &mut sink,
            idle: &mut consumer,
        },
        &mut NoService,
        &mut lines,
    )
    .map_err(|e| format!("paced run: {e:?}"))?;
    let pl = sink.0;
    let f = r.len().min(pl.len());
    println!(
        "paced calls={calls} ready={} fbs_first={f}={:016x}",
        pl.len(),
        chain(&pl[..f])
    );
    let mut stats = String::new();
    let _ = script.report(&mut stats);
    print!("{stats}");
    let first_diff = r.iter().zip(&pl).position(|(a, b)| a != b);
    let equal = matches!(st.end, Some(End::Completed))
        && pl.len() >= r.len()
        && first_diff.is_none()
        && script.late() == 0;
    match first_diff {
        Some(i) => println!("equal=no first_diff_frame={i}"),
        None if !equal => println!("equal=no"),
        None => println!("equal=yes"),
    }
    Ok(equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_refuses_five_changes_within_two_frames() {
        let two = 2 * u64::from(FRAME_CLOCKS);
        let at = |cs: &[u64]| cs.iter().map(|&c| (c, Buttons::A)).collect::<Vec<_>>();
        assert!(density(&at(&[0, 1, 2, 3])).is_ok());
        assert!(density(&at(&[0, 1, 2, 3, two])).is_ok());
        assert!(density(&at(&[0, 1, 2, 3, two - 1])).is_err());
        assert!(
            density(&at(&[
                100,
                0x10_0000,
                0x10_0001,
                0x10_0002,
                0x10_0003,
                two + 0x10_0000 - 1
            ]))
            .is_err()
        );
    }

    #[test]
    fn button_names_round_trip_through_the_firmware_parser() {
        let all = Buttons::A | Buttons::DOWN | Buttons::START;
        assert_eq!(button_names(all), "a+start+down");
        assert_eq!(button_names(Buttons::EMPTY), "none");
        let text = format!("clocks v1\n4 {}\n8 none\n", button_names(all));
        let mut out = [Change::NONE; 2];
        replay::parse(&text, &mut out).unwrap();
        assert_eq!(out[0].buttons, all);
        assert_eq!(out[1].buttons, Buttons::EMPTY);
    }
}
