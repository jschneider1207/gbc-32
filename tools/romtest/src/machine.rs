//! The run loop and the boundary between the runner and the emulator
//! (ADR 0007). The runner drives a [`Machine`]; the real one is
//! `gbc_core::Emulator`, and tests use a scriptable fake so the runner's
//! logic can be tested without a working CPU.
//!
//! Every call into the emulator goes through [`core_call`], the panic
//! boundary: a panic inside the core becomes [`CoreFailure::Panic`] (status
//! `crash`), while a panic in the runner's own code propagates (exit 2).

use gbc_core::{
    AUDIO_SAMPLE_CLOCKS, Buttons, Config, DmgPalettes, Emulator, Events, FRAME_CLOCKS, Frame,
    Memory, Registers, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE, cart_ram_size,
    rgb888_to_555,
};

use crate::manifest::{Exit, InputEntry};

/// What the runner needs from an emulator. Mirrors the parts of the
/// `gbc_core::Emulator` API that `romtest` uses.
pub trait Machine {
    fn set_stop_events(&mut self, events: Events);
    fn set_buttons(&mut self, buttons: Buttons);
    /// Schedules a button change at `clock` (applied now if that has
    /// passed); `false` if the machine's queue is full (ADR 0012).
    fn set_buttons_at(&mut self, clock: u64, buttons: Buttons) -> bool;
    /// Runs until `clock() >= target` or a stop event; returns the events
    /// and how many audio samples were written to the start of `audio`
    /// (an empty `audio` discards sound; a full one stops the call with
    /// `AUDIO_BUFFER_FULL`).
    fn run_until(
        &mut self,
        target: u64,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> (Events, usize);
    fn clock(&self) -> u64;
    fn registers(&self) -> Registers;
    fn serial_byte(&self) -> u8;
    /// Whether the screen shows white: the LCD is off, or was turned off
    /// and no frame has completed since.
    fn screen_blank(&self) -> bool;
}

impl Machine for Emulator<'_> {
    fn set_stop_events(&mut self, events: Events) {
        Emulator::set_stop_events(self, events);
    }
    fn set_buttons(&mut self, buttons: Buttons) {
        Emulator::set_buttons(self, buttons);
    }
    fn set_buttons_at(&mut self, clock: u64, buttons: Buttons) -> bool {
        Emulator::set_buttons_at(self, clock, buttons)
    }
    fn run_until(
        &mut self,
        target: u64,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> (Events, usize) {
        let r = Emulator::run_until(self, target, frame, audio);
        (r.events, r.audio_samples)
    }
    fn clock(&self) -> u64 {
        Emulator::clock(self)
    }
    fn registers(&self) -> Registers {
        Emulator::registers(self)
    }
    fn serial_byte(&self) -> u8 {
        Emulator::serial_byte(self)
    }
    fn screen_blank(&self) -> bool {
        Emulator::screen_blank(self)
    }
}

/// Why the emulator didn't run to an exit. Both count as `crash`: the ROM
/// files are hash-verified before anything runs, so a load error is the
/// emulator's fault too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreFailure {
    /// The core rejected the ROM.
    Load(String),
    /// The core panicked (message and location).
    Panic(String),
}

/// Calls into the emulator core: the only place where panics are caught.
pub fn core_call<R>(f: impl FnOnce() -> R) -> Result<R, CoreFailure> {
    crate::panics::catch_panic(f).map_err(CoreFailure::Panic)
}

/// The "common palette" of the reference screenshots
/// (acceptance/README.md, "Colors used for screenshot comparison"). The
/// README lists darkest first; the core indexes shade 0 (lightest) to 3
/// (darkest), so the order is reversed here. OBJ0 = OBJ1.
pub fn common_palette() -> DmgPalettes {
    const fn c(rgb: u32) -> u16 {
        rgb888_to_555((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }
    let obj = [c(0xFFFFFF), c(0xFF8484), c(0x943939), c(0x000000)];
    DmgPalettes {
        bg: [c(0xFFFFFF), c(0x7BFF31), c(0x0063C6), c(0x000000)],
        obj0: obj,
        obj1: obj,
    }
}

/// Everything about how one group's ROMs are run.
#[derive(Clone, Debug)]
pub struct RunPlan {
    pub exit: Exit,
    /// Button state changes: from clock `.0` on, exactly `.1` is held.
    /// Sorted by clock.
    pub input: Vec<(u64, Buttons)>,
    /// A CPU lockup on an invalid opcode doesn't end the run: the machine
    /// keeps running (LCD, timers) to the exit and the checks decide on
    /// the final frame. Only for a group with `lockup = "run_on"` (valid
    /// only on `gambatte = true` groups with a `run_frames` exit;
    /// acceptance/README.md; ADR 0018, amendments 2026-11-22). Otherwise a
    /// lockup is [`End::Locked`], a crash.
    pub lockup_runs_on: bool,
    /// Capture the audio of the last frame of the run ([`AudioCapture`]),
    /// for the gambatte check's `_outaudio0/1` rule (ADR 0018, audio
    /// amendment; ADR 0021 section 10). Before that frame every run call
    /// gets an empty audio buffer.
    pub audio_last_frame: bool,
}

impl RunPlan {
    pub fn new(exit: Exit, entries: &[InputEntry]) -> Self {
        Self {
            exit,
            input: input_timeline(entries),
            lockup_runs_on: false,
            audio_last_frame: false,
        }
    }
}

/// Audio slots for the last frame: the most samples one frame of clocks
/// can complete (`(FRAME_CLOCKS / AUDIO_SAMPLE_CLOCKS) + 2`, the core's
/// bound for one call: 550 at 32,768 Hz). Filling them is a runner error
/// ([`AudioCapture::full`]), so the core never stops early on
/// `AUDIO_BUFFER_FULL` within the window.
pub const AUDIO_SLOTS: usize = (FRAME_CLOCKS / AUDIO_SAMPLE_CLOCKS) as usize + 2;

/// The audio of the last frame of a run ([`RunPlan::audio_last_frame`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioCapture {
    /// Where the window should end: the exit's clock limit. It starts one
    /// frame (`FRAME_CLOCKS`) before.
    pub until: u64,
    /// The clock where the first run call with room started (the first
    /// call to end at or past `until - FRAME_CLOCKS`).
    pub start: u64,
    /// The clock when the run ended. `end < until`: the run stopped before
    /// the window was complete.
    pub end: u64,
    /// Every sample the core returned from `start` on, in order.
    pub samples: Vec<StereoSample>,
    /// All [`AUDIO_SLOTS`] filled: more samples than a frame can hold, so
    /// some may have been dropped (a runner or core bug).
    pub full: bool,
}

/// Where the CPU locked up, when the run went on past it
/// ([`RunPlan::lockup_runs_on`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lockup {
    /// The clock when the run call that reported the lockup returned.
    pub clock: u64,
    /// PC then (just past the invalid opcode).
    pub pc: u16,
}

/// Turns input entries into a sorted list of button-state changes.
/// Overlapping entries combine: a button is held while any entry holds it.
pub fn input_timeline(entries: &[InputEntry]) -> Vec<(u64, Buttons)> {
    let mut points: Vec<u64> = entries
        .iter()
        .flat_map(|e| [e.press_at, e.release_at])
        .collect();
    points.sort_unstable();
    points.dedup();
    let mut out: Vec<(u64, Buttons)> = Vec::with_capacity(points.len());
    for t in points {
        let held = entries
            .iter()
            .filter(|e| e.press_at <= t && t < e.release_at)
            .fold(Buttons::EMPTY, |acc, e| acc | e.buttons);
        let prev = out.last().map_or(Buttons::EMPTY, |p| p.1);
        if held != prev {
            out.push((t, held));
        }
    }
    out
}

/// Why a run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// `run_s` / `run_frames` reached.
    Completed,
    /// `LD B,B` executed (ld_b_b exit).
    LdBB,
    /// The ld_b_b timeout passed.
    Timeout,
    /// The CPU locked up on an invalid opcode.
    Locked,
    /// Run calls stopped advancing time: one returned with no event and no
    /// time passed, or [`MAX_STALLED_CALLS`] in a row returned events
    /// without time passing.
    NoProgress,
    /// The input change at clock `at` could only be handed to the machine
    /// at `now`, after its M-cycle (its queue was full until then), so it
    /// can't land on time. The run stops there.
    InputLate { at: u64, now: u64 },
}

impl End {
    pub fn name(self) -> &'static str {
        match self {
            End::Completed => "completed",
            End::LdBB => "ld_b_b",
            End::Timeout => "timeout",
            End::Locked => "cpu_locked",
            End::NoProgress => "no_progress",
            End::InputLate { .. } => "input_late",
        }
    }
}

/// How many run calls in a row may return without advancing the clock
/// before the run counts as a crash ([`End::NoProgress`]). Every run call
/// executes at least one instruction (one M-cycle or more), so even one is
/// a core bug; the bound only keeps a broken core that keeps reporting
/// events (e.g. `SERIAL_TX`) from looping until the timeout, or forever.
pub const MAX_STALLED_CALLS: u32 = 1000;

/// What a run produced. Lives outside the panic boundary so partial results
/// (e.g. serial output before a panic) survive a crash.
#[derive(Default)]
pub struct RunState {
    pub end: Option<End>,
    pub serial: Vec<u8>,
    /// The last completed frame, if any.
    pub last_frame: Option<Box<Frame>>,
    pub frames: u64,
    pub clock: u64,
    pub registers: Option<Registers>,
    /// [`Machine::screen_blank`] at exit.
    pub screen_blank_at_exit: bool,
    /// The CPU locked up and the run went on ([`RunPlan::lockup_runs_on`]).
    pub lockup: Option<Lockup>,
    /// With [`RunPlan::audio_last_frame`]: the last frame's audio, if the
    /// run reached it.
    pub audio: Option<AudioCapture>,
}

pub fn new_frame() -> Box<Frame> {
    vec![WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]
        .into_boxed_slice()
        .try_into()
        .expect("frame size")
}

impl RunState {
    /// The final frame (acceptance/README.md, "Checks"): the last fully
    /// drawn frame, or solid white if no frame was completed or the screen
    /// is blank at exit. The screen is blank while the LCD is off and also
    /// after it is turned back on, through the first completed frame,
    /// which the hardware doesn't display (`gbc_core::Emulator::run_until`
    /// and `screen_blank`). So a frame drawn before an off/on cycle, or the
    /// first one after it, is never the final frame.
    pub fn final_frame(&self) -> Box<Frame> {
        match &self.last_frame {
            Some(f) if !self.screen_blank_at_exit => f.clone(),
            _ => new_frame(),
        }
    }
}

/// Runs `m` (already loaded) according to `plan`, recording into `st`.
/// Only the `Machine` calls are inside the panic boundary.
pub fn run<M: Machine + ?Sized>(
    m: &mut M,
    plan: &RunPlan,
    st: &mut RunState,
) -> Result<(), CoreFailure> {
    let ld_b_b = plan.exit.is_ld_b_b();
    let limit = plan.exit.limit_clocks();
    let mut stop = Events::SERIAL_TX;
    if ld_b_b {
        stop |= Events::LD_B_B;
    }
    core_call(|| {
        m.set_stop_events(stop);
        m.set_buttons(Buttons::EMPTY);
    })?;

    let mut fb = new_frame();
    // The last frame's audio: run calls before `audio_from` get no buffer;
    // one call ends there, and from then on every call gets the free rest
    // of `audio`.
    let audio_from = plan
        .audio_last_frame
        .then(|| limit.saturating_sub(u64::from(FRAME_CLOCKS)));
    let mut audio =
        vec![StereoSample::default(); if audio_from.is_some() { AUDIO_SLOTS } else { 0 }];
    let mut audio_len = 0;
    let mut audio_start = None;
    // Input changes before `next_input` have been handed to the machine.
    let mut next_input = 0;
    let mut stalled_calls = 0u32;
    let end = 'run: loop {
        let now = core_call(|| m.clock())?;
        // Hand the machine every change its queue takes. It applies each
        // on its exact M-cycle, wherever run calls end (ADR 0012).
        while let Some(&(at, buttons)) = plan.input.get(next_input) {
            if at < now {
                // Its M-cycle has passed (more changes fell inside one run
                // call's overshoot than the queue holds): the machine
                // would apply it late, so the run can't count. One after
                // the exit limit is never needed.
                if at < limit {
                    break 'run End::InputLate { at, now };
                }
                break;
            }
            if !core_call(|| m.set_buttons_at(at, buttons))? {
                break;
            }
            next_input += 1;
        }
        if now >= limit {
            break if ld_b_b { End::Timeout } else { End::Completed };
        }
        // With the queue full, stop at its earliest change so it drains
        // and can be refilled.
        let target = match plan.input.get(next_input) {
            None => limit,
            Some(&(waiting, _)) => plan.input[..next_input]
                .iter()
                .map(|&(at, _)| at)
                .find(|&at| at > now)
                .unwrap_or(waiting)
                .min(limit),
        };
        let (target, room) = match audio_from {
            Some(from) if now < from => (target.min(from), &mut audio[..0]),
            Some(_) => {
                audio_start.get_or_insert(now);
                (target, &mut audio[audio_len..])
            }
            None => (target, &mut audio[..0]),
        };
        let (events, written) = core_call(|| m.run_until(target, &mut fb, room))?;
        audio_len += written;
        if events.contains(Events::SERIAL_TX) {
            st.serial.push(core_call(|| m.serial_byte())?);
        }
        if events.contains(Events::FRAME_READY) {
            match &mut st.last_frame {
                Some(f) => f.copy_from_slice(&fb[..]),
                None => st.last_frame = Some(fb.clone()),
            }
            st.frames += 1;
        }
        if events.contains(Events::CPU_LOCKED) {
            if !plan.lockup_runs_on {
                break End::Locked;
            }
            // As on hardware, the CPU stays stuck (no interrupt wakes it)
            // while the LCD keeps running; the core keeps advancing time.
            if st.lockup.is_none() {
                let (clock, pc) = core_call(|| (m.clock(), m.registers().pc))?;
                st.lockup = Some(Lockup { clock, pc });
            }
        }
        if ld_b_b && events.contains(Events::LD_B_B) {
            break End::LdBB;
        }
        if core_call(|| m.clock())? == now {
            stalled_calls += 1;
            if events.is_empty() || stalled_calls >= MAX_STALLED_CALLS {
                break End::NoProgress;
            }
        } else {
            stalled_calls = 0;
        }
    };
    st.end = Some(end);
    (st.clock, st.registers, st.screen_blank_at_exit) =
        core_call(|| (m.clock(), Some(m.registers()), m.screen_blank()))?;
    st.audio = audio_start.map(|start| AudioCapture {
        until: limit,
        start,
        end: st.clock,
        samples: audio[..audio_len].to_vec(),
        full: audio_len == AUDIO_SLOTS,
    });
    Ok(())
}

/// Runs a ROM on the real core.
pub fn run_core(rom: &[u8], plan: &RunPlan, st: &mut RunState) -> Result<(), CoreFailure> {
    let load = |e: &dyn std::fmt::Display| CoreFailure::Load(e.to_string());
    let ram_size = core_call(|| cart_ram_size(rom))?.map_err(|e| load(&e))?;
    // Memory, ROM and cart RAM outlive the emulator, which borrows them.
    let mut cart_ram = vec![0u8; ram_size];
    let mut mem = Box::new(Memory::new());
    let mem_ref = &mut *mem;
    let mut emu = core_call(move || Emulator::new(mem_ref))?;
    let config = Config {
        dmg_palettes: common_palette(),
    };
    let ram = &mut cart_ram[..];
    let emu_ref = &mut emu;
    core_call(move || {
        emu_ref
            .load(rom, ram, config)
            .map(drop)
            .map_err(|e| e.to_string())
    })?
    .map_err(|e| load(&e))?;
    run(&mut emu, plan, st)
}

#[cfg(test)]
pub mod fake {
    //! A scriptable stand-in for the emulator.

    use super::*;
    use gbc_core::FRAME_CLOCKS;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Action {
        Serial(u8),
        LdBB,
        Lock,
        Panic,
        LcdOff,
        LcdOn,
        /// Stop advancing time (to test the no-progress guard).
        Stall,
        /// Stop advancing time but keep returning `SERIAL_TX` from every
        /// run call.
        StallWithEvents,
    }

    /// Clocks from the start of a frame (LY=0) to the start of VBlank
    /// (LY=144), where the core reports `FRAME_READY`.
    pub const VBLANK: u64 = 144 * 456;

    /// Advances in 4-clock steps (or `step` clocks); completes a frame
    /// (filled with the frame number) at the start of each VBlank
    /// (`VBLANK` into every `FRAME_CLOCKS` period) while the LCD is on;
    /// fires scripted actions at given clocks. Like the core, turning the
    /// LCD off abandons the frame in progress and blanks the screen, and
    /// after the LCD is turned on the screen stays blank through the first
    /// completed frame.
    pub struct FakeMachine {
        pub clock: u64,
        pub step: u64,
        pub script: Vec<(u64, Action)>,
        pub regs: Registers,
        pub lcd: bool,
        pub stop: Events,
        pub buttons: Buttons,
        /// Every button change with the clock it applied at.
        pub button_log: Vec<(u64, Buttons)>,
        /// Scheduled changes (sorted, one per clock) and the queue size;
        /// like the core, each applies at its exact clock.
        pub pending: Vec<(u64, Buttons)>,
        pub pending_cap: usize,
        pub serial: u8,
        frame_clock: u64,
        frames: u16,
        blanked: bool,
        skip_frame: bool,
        stalled: Option<Events>,
        locked: bool,
        /// If set, every completed frame shows this image instead of the
        /// frame number.
        pub image: Option<Box<Frame>>,
        /// Sound plays in `[.0, .1)`: a sample window (one per
        /// `AUDIO_SAMPLE_CLOCKS`, on the absolute clock grid) that ends in
        /// it alternates between two levels; every other window has the
        /// constant level `FAKE_DC`.
        pub sound: Option<(u64, u64)>,
        /// Every run call: its start clock and the audio room it got.
        pub audio_calls: Vec<(u64, usize)>,
    }

    /// The fake's level while silent: a constant, not 0, as the core's
    /// output keeps DC (ADR 0021 section 8).
    pub const FAKE_DC: StereoSample = StereoSample { left: 7, right: -3 };

    impl FakeMachine {
        pub fn new(script: Vec<(u64, Action)>) -> Self {
            Self {
                clock: 0,
                step: 4,
                script,
                regs: Registers::default(),
                lcd: true,
                stop: Events::EMPTY,
                buttons: Buttons::EMPTY,
                button_log: Vec::new(),
                pending: Vec::new(),
                pending_cap: gbc_core::SCHEDULED_INPUTS,
                serial: 0,
                frame_clock: 0,
                frames: 0,
                blanked: false,
                skip_frame: false,
                stalled: None,
                locked: false,
                image: None,
                sound: None,
                audio_calls: Vec::new(),
            }
        }

        /// The sample whose window ends at `clock` (a multiple of
        /// `AUDIO_SAMPLE_CLOCKS`).
        fn sample_at(&self, clock: u64) -> StereoSample {
            match self.sound {
                Some((from, to)) if (from..to).contains(&clock) => {
                    let level = if (clock / u64::from(AUDIO_SAMPLE_CLOCKS)).is_multiple_of(2) {
                        100
                    } else {
                        -100
                    };
                    StereoSample {
                        left: level,
                        right: level,
                    }
                }
                _ => FAKE_DC,
            }
        }

        fn step_once(&mut self, frame: &mut Frame) -> Events {
            let mut ev = Events::EMPTY;
            let before = self.clock;
            self.clock += self.step;
            while let Some(&(at, buttons)) = self.pending.first() {
                if at > self.clock {
                    break;
                }
                self.pending.remove(0);
                self.buttons = buttons;
                self.button_log.push((at, buttons));
            }
            for &(at, action) in &self.script {
                if at < before || at >= self.clock {
                    continue;
                }
                match action {
                    Action::Serial(b) => {
                        self.serial = b;
                        ev |= Events::SERIAL_TX;
                    }
                    Action::LdBB if !self.locked => ev |= Events::LD_B_B,
                    Action::LdBB => {}
                    Action::Lock => {
                        self.locked = true;
                        ev |= Events::CPU_LOCKED;
                    }
                    Action::Panic => panic!("fake emulator panic at {at}"),
                    Action::LcdOff => {
                        if self.lcd {
                            self.blanked = true;
                            self.skip_frame = false;
                            self.frame_clock = 0;
                        }
                        self.lcd = false;
                    }
                    Action::LcdOn => {
                        if !self.lcd {
                            self.blanked = true;
                            self.skip_frame = true;
                        }
                        self.lcd = true;
                    }
                    Action::Stall => self.stalled = Some(Events::EMPTY),
                    Action::StallWithEvents => self.stalled = Some(Events::SERIAL_TX),
                }
            }
            if self.lcd {
                let before = self.frame_clock;
                self.frame_clock = (before + self.step) % u64::from(FRAME_CLOCKS);
                if before < VBLANK && before + self.step >= VBLANK {
                    self.frames += 1;
                    if self.skip_frame {
                        self.skip_frame = false;
                    } else {
                        self.blanked = false;
                    }
                    match &self.image {
                        Some(img) => frame.copy_from_slice(&img[..]),
                        None => frame.fill(self.frames),
                    }
                    ev |= Events::FRAME_READY;
                }
            }
            ev
        }
    }

    impl Machine for FakeMachine {
        fn set_stop_events(&mut self, events: Events) {
            self.stop = events;
        }
        fn set_buttons(&mut self, buttons: Buttons) {
            self.buttons = buttons;
            self.button_log.push((self.clock, buttons));
        }
        fn set_buttons_at(&mut self, clock: u64, buttons: Buttons) -> bool {
            if clock <= self.clock {
                self.set_buttons(buttons);
                return true;
            }
            match self.pending.binary_search_by_key(&clock, |p| p.0) {
                Ok(i) => self.pending[i].1 = buttons,
                Err(_) if self.pending.len() == self.pending_cap => return false,
                Err(i) => self.pending.insert(i, (clock, buttons)),
            }
            true
        }
        fn run_until(
            &mut self,
            target: u64,
            frame: &mut Frame,
            audio: &mut [StereoSample],
        ) -> (Events, usize) {
            let stop = Events::ALWAYS_STOP | self.stop;
            let mut ev = Events::EMPTY;
            let mut written = 0;
            self.audio_calls.push((self.clock, audio.len()));
            if let Some(stalled) = self.stalled {
                return (stalled, 0);
            }
            let period = u64::from(AUDIO_SAMPLE_CLOCKS);
            while self.clock < target && self.stalled.is_none() {
                let before = self.clock;
                ev |= self.step_once(frame);
                // A window ends in this step: write its sample if there's
                // room (an empty buffer discards sound).
                let end = (before / period + 1) * period;
                if end <= self.clock && written < audio.len() {
                    audio[written] = self.sample_at(end);
                    written += 1;
                    if written == audio.len() {
                        ev |= Events::AUDIO_BUFFER_FULL;
                    }
                }
                if ev.intersects(stop) {
                    break;
                }
            }
            (ev, written)
        }
        fn clock(&self) -> u64 {
            self.clock
        }
        fn registers(&self) -> Registers {
            self.regs
        }
        fn serial_byte(&self) -> u8 {
            self.serial
        }
        fn screen_blank(&self) -> bool {
            self.blanked || !self.lcd
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::{Action, FAKE_DC, FakeMachine, VBLANK};
    use super::*;
    use gbc_core::{CLOCK_HZ, FRAME_CLOCKS, rgb555_to_888};

    const HZ: u64 = CLOCK_HZ as u64;
    const FRAME: u64 = FRAME_CLOCKS as u64;

    fn plan(exit: Exit) -> RunPlan {
        RunPlan::new(exit, &[])
    }

    fn run_fake(m: &mut FakeMachine, plan: &RunPlan) -> RunState {
        let mut st = RunState::default();
        run(m, plan, &mut st).unwrap();
        st
    }

    #[test]
    fn run_s_runs_exactly_n_seconds() {
        let mut m = FakeMachine::new(vec![]);
        let st = run_fake(&mut m, &plan(Exit::RunS { seconds: 2.0 }));
        assert_eq!(st.end, Some(End::Completed));
        assert_eq!(st.clock, 2 * HZ);
        // Frames are ready at the start of VBlank: VBLANK + k * FRAME.
        assert_eq!(st.frames, (2 * HZ - VBLANK) / FRAME + 1);
    }

    #[test]
    fn run_frames_and_overshoot() {
        let mut m = FakeMachine::new(vec![]);
        // 12-clock "instructions": the run ends at the first boundary at or
        // after the limit.
        m.step = 12;
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 3 }));
        assert_eq!(st.end, Some(End::Completed));
        let limit = 3 * FRAME;
        assert_eq!(st.clock, limit.div_ceil(12) * 12);
    }

    #[test]
    fn ld_b_b_stops_and_captures_registers() {
        let mut m = FakeMachine::new(vec![(1000, Action::LdBB)]);
        m.regs.b = 3;
        let st = run_fake(&mut m, &plan(Exit::LdBB { timeout_s: 1.0 }));
        assert_eq!(st.end, Some(End::LdBB));
        assert_eq!(st.clock, 1004);
        assert_eq!(st.registers.unwrap().b, 3);
        assert!(m.stop.contains(Events::LD_B_B | Events::SERIAL_TX));
    }

    #[test]
    fn ld_b_b_is_ignored_for_timed_exits() {
        let mut m = FakeMachine::new(vec![(1000, Action::LdBB)]);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 1 }));
        assert_eq!(st.end, Some(End::Completed));
        assert_eq!(st.clock, FRAME);
        assert!(!m.stop.contains(Events::LD_B_B));
    }

    #[test]
    fn ld_b_b_timeout() {
        let mut m = FakeMachine::new(vec![(2 * HZ, Action::LdBB)]);
        let st = run_fake(&mut m, &plan(Exit::LdBB { timeout_s: 1.0 }));
        assert_eq!(st.end, Some(End::Timeout));
        assert_eq!(st.clock, HZ);
    }

    #[test]
    fn lockup_stops_early() {
        let mut m = FakeMachine::new(vec![(5000, Action::Lock)]);
        let st = run_fake(&mut m, &plan(Exit::LdBB { timeout_s: 120.0 }));
        assert_eq!(st.end, Some(End::Locked));
        assert!(st.clock < 6000);
        assert_eq!(st.lockup, None);
    }

    #[test]
    fn lockup_runs_on_when_the_plan_says_so() {
        // Without the flag, the same run_frames plan stops at the lockup.
        let mut m = FakeMachine::new(vec![(5000, Action::Lock)]);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 15 }));
        assert_eq!(st.end, Some(End::Locked));
        assert!(st.clock < 6000);
        assert_eq!(st.lockup, None);

        let mut m = FakeMachine::new(vec![(5000, Action::Lock)]);
        m.regs.pc = 0x1234;
        let p = RunPlan {
            lockup_runs_on: true,
            ..plan(Exit::RunFrames { frames: 15 })
        };
        let st = run_fake(&mut m, &p);
        assert_eq!(st.end, Some(End::Completed));
        assert_eq!(st.clock, 15 * FRAME);
        assert_eq!(st.frames, 15);
        assert_eq!(
            st.lockup,
            Some(Lockup {
                clock: 5004,
                pc: 0x1234
            })
        );
    }

    #[test]
    fn no_progress_is_detected() {
        let mut m = FakeMachine::new(vec![(100, Action::Stall)]);
        let st = run_fake(&mut m, &plan(Exit::RunS { seconds: 1.0 }));
        assert_eq!(st.end, Some(End::NoProgress));
    }

    #[test]
    fn no_progress_is_detected_while_events_keep_coming() {
        // A broken core that stops advancing time but keeps reporting
        // events: caught after MAX_STALLED_CALLS calls, not at the timeout.
        let mut m = FakeMachine::new(vec![(100, Action::StallWithEvents)]);
        let st = run_fake(&mut m, &plan(Exit::LdBB { timeout_s: 120.0 }));
        assert_eq!(st.end, Some(End::NoProgress));
        assert_eq!(st.clock, 104);
        assert_eq!(st.serial.len(), MAX_STALLED_CALLS as usize);

        // Events with progress in between reset the count.
        let script = (0..2 * u64::from(MAX_STALLED_CALLS))
            .map(|i| (100 + 4 * i, Action::Serial(b'x')))
            .collect();
        let mut m = FakeMachine::new(script);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 1 }));
        assert_eq!(st.end, Some(End::Completed));
        assert_eq!(st.serial.len(), 2 * MAX_STALLED_CALLS as usize);
    }

    #[test]
    fn collects_every_serial_byte() {
        let script = b"Passed"
            .iter()
            .enumerate()
            .map(|(i, &b)| (100 + 4 * i as u64, Action::Serial(b)))
            .collect();
        let mut m = FakeMachine::new(script);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 1 }));
        assert_eq!(st.serial, b"Passed");
    }

    #[test]
    fn final_frame_is_the_last_completed_one() {
        let mut m = FakeMachine::new(vec![]);
        let st = run_fake(&mut m, &plan(Exit::RunS { seconds: 1.0 }));
        // 59 frames complete in one second; the frame buffer holds frame 59.
        assert_eq!(st.frames, 59);
        assert!(st.final_frame().iter().all(|&p| p == 59));
    }

    #[test]
    fn final_frame_is_white_with_lcd_off_or_no_frame() {
        let mut m = FakeMachine::new(vec![(FRAME * 3 / 2, Action::LcdOff)]);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 3 }));
        assert_eq!(st.frames, 1);
        assert!(st.screen_blank_at_exit);
        assert!(st.final_frame().iter().all(|&p| p == WHITE));

        let mut m = FakeMachine::new(vec![]);
        let st = run_fake(&mut m, &plan(Exit::LdBB { timeout_s: 0.001 }));
        assert_eq!(st.frames, 0);
        assert!(st.final_frame().iter().all(|&p| p == WHITE));
    }

    #[test]
    fn final_frame_after_lcd_off_and_on() {
        // Off and on again within one run call after the last FRAME_READY,
        // with no frame since: the LCD is on at exit, but the screen is
        // still white (gbc_core::Emulator::run_until), so the final frame
        // is white, not the frame drawn before the LCD went off.
        let script = vec![
            (VBLANK + FRAME + 100, Action::LcdOff),
            (VBLANK + FRAME + 1000, Action::LcdOn),
        ];
        let mut m = FakeMachine::new(script.clone());
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 2 }));
        assert_eq!(st.frames, 2);
        assert!(m.lcd);
        assert!(st.screen_blank_at_exit);
        assert!(st.final_frame().iter().all(|&p| p == WHITE));

        // The first frame after the LCD comes back on is not displayed
        // (gbc_core::Emulator::screen_blank): still white, although a frame
        // completed after LcdOn.
        let mut m = FakeMachine::new(script);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 3 }));
        assert_eq!(st.frames, 3);
        assert!(st.screen_blank_at_exit);
        assert!(st.final_frame().iter().all(|&p| p == WHITE));

        // The second frame after the LCD came back on is displayed. The
        // PPU restarts at line 0 on LcdOn, so frames complete at
        // FRAME + VBLANK (not shown) and 2 * FRAME + VBLANK.
        let script = vec![(FRAME / 2, Action::LcdOff), (FRAME, Action::LcdOn)];
        let mut m = FakeMachine::new(script.clone());
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 2 }));
        assert_eq!(st.frames, 1);
        assert!(st.screen_blank_at_exit);
        assert!(st.final_frame().iter().all(|&p| p == WHITE));
        let mut m = FakeMachine::new(script);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 3 }));
        assert_eq!(st.frames, 2);
        assert!(!st.screen_blank_at_exit);
        assert!(st.final_frame().iter().all(|&p| p == 2));
    }

    #[test]
    fn core_panics_are_core_failures() {
        let mut m = FakeMachine::new(vec![(4000, Action::Serial(b'x')), (5000, Action::Panic)]);
        let mut st = RunState::default();
        let r = run(&mut m, &plan(Exit::RunS { seconds: 1.0 }), &mut st);
        match r {
            Err(CoreFailure::Panic(msg)) => assert!(msg.contains("fake emulator panic at 5000")),
            other => panic!("{other:?}"),
        }
        // Output before the panic survives.
        assert_eq!(st.serial, b"x");
        assert_eq!(st.end, None);
    }

    #[test]
    fn input_with_ld_b_b_exit_and_serial_stops() {
        // Serial stops, input changes and the LD B,B exit interleave: every
        // byte is collected, inputs still apply at their times, and the run
        // ends at LD B,B (not at the timeout or the next input).
        let mut script: Vec<(u64, Action)> = b"ab"
            .iter()
            .enumerate()
            .map(|(i, &b)| (3000 + 2000 * i as u64, Action::Serial(b)))
            .collect();
        script.push((9000, Action::LdBB));
        let input = [
            entry(2000, 4000, Buttons::START),
            entry(8000, 20000, Buttons::A),
        ];
        let plan = RunPlan::new(Exit::LdBB { timeout_s: 1.0 }, &input);
        let mut m = FakeMachine::new(script);
        m.step = 12;
        let st = run_fake(&mut m, &plan);
        assert_eq!(st.end, Some(End::LdBB));
        assert_eq!(st.serial, b"ab");
        assert_eq!(st.clock, 9012);
        assert_eq!(
            m.button_log,
            [
                (0, Buttons::EMPTY),
                (2000, Buttons::START),
                (4000, Buttons::EMPTY),
                (8000, Buttons::A),
            ]
        );
        // A serial byte in the same step as LD B,B is still collected.
        let script = vec![(600, Action::Serial(b'z')), (604, Action::LdBB)];
        let mut m = FakeMachine::new(script);
        m.step = 8;
        let st = run_fake(&mut m, &RunPlan::new(Exit::LdBB { timeout_s: 1.0 }, &[]));
        assert_eq!((st.end, st.serial.as_slice()), (Some(End::LdBB), &b"z"[..]));
    }

    fn entry(at: u64, release: u64, b: Buttons) -> InputEntry {
        InputEntry {
            press_at: at,
            release_at: release,
            buttons: b,
        }
    }

    #[test]
    fn overlapping_inputs_combine() {
        let t = input_timeline(&[
            entry(100, 300, Buttons::A),
            entry(200, 400, Buttons::A | Buttons::DOWN),
            entry(500, 600, Buttons::B),
        ]);
        assert_eq!(
            t,
            [
                (100, Buttons::A),
                (200, Buttons::A | Buttons::DOWN),
                (400, Buttons::EMPTY),
                (500, Buttons::B),
                (600, Buttons::EMPTY),
            ]
        );
        // Back-to-back entries for the same button stay held.
        let t = input_timeline(&[entry(0, 100, Buttons::A), entry(100, 200, Buttons::A)]);
        assert_eq!(t, [(0, Buttons::A), (200, Buttons::EMPTY)]);
    }

    #[test]
    fn inputs_apply_at_their_emulated_time() {
        let input = [
            entry(4000, 8000, Buttons::A),
            entry(6000, 10000, Buttons::B),
        ];
        let plan = RunPlan::new(Exit::RunFrames { frames: 1 }, &input);
        let mut m = FakeMachine::new(vec![]);
        m.step = 12; // instruction boundaries every 12 clocks
        run_fake(&mut m, &plan);
        assert_eq!(
            m.button_log,
            [
                (0, Buttons::EMPTY),
                (4000, Buttons::A),
                (6000, Buttons::A | Buttons::B),
                (8000, Buttons::B),
                (10000, Buttons::EMPTY),
            ]
        );
    }

    #[test]
    fn inputs_beyond_the_queue_wait_for_it_to_drain() {
        // A queue of one: each change is handed over once the previous
        // one has applied, and still lands on its exact clock. Changes
        // closer together than one run call's overshoot can't all be
        // queued: the run stops, failing, rather than apply one late.
        let input = [
            entry(4000, 8000, Buttons::A),
            entry(6000, 10000, Buttons::B),
            entry(20000, 20002, Buttons::START),
        ];
        let plan = RunPlan::new(Exit::RunFrames { frames: 1 }, &input);
        let mut m = FakeMachine::new(vec![]);
        m.step = 12;
        m.pending_cap = 1;
        let st = run_fake(&mut m, &plan);
        assert_eq!(
            st.end,
            Some(End::InputLate {
                at: 20002,
                now: 20004
            })
        );
        assert_eq!(st.clock, 20004);
        assert_eq!(
            m.button_log,
            [
                (0, Buttons::EMPTY),
                (4000, Buttons::A),
                (6000, Buttons::A | Buttons::B),
                (8000, Buttons::B),
                (10000, Buttons::EMPTY),
                (20000, Buttons::START),
            ]
        );
        // A change after the exit limit is never needed: one that the
        // last run call overshoots doesn't fail the run. FRAME + 6 is
        // still waiting for the queue when the run ends at 71000.
        let input = [
            entry(FRAME - 500, FRAME + 5, Buttons::A),
            entry(FRAME + 6, FRAME + 100, Buttons::B),
        ];
        let plan = RunPlan::new(Exit::RunFrames { frames: 1 }, &input);
        let mut m = FakeMachine::new(vec![]);
        m.step = 1000;
        m.pending_cap = 1;
        let st = run_fake(&mut m, &plan);
        assert_eq!((st.end, st.clock), (Some(End::Completed), 71000));
    }

    #[test]
    fn common_palette_matches_the_contract() {
        let p = common_palette();
        let hex = |c: u16| {
            let [r, g, b] = rgb555_to_888(c);
            u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
        };
        // Shade 0 is the lightest (gbc_core::DmgPalettes).
        assert_eq!(p.bg.map(hex), [0xFFFFFF, 0x7BFF31, 0x0063C6, 0x000000]);
        assert_eq!(p.obj0.map(hex), [0xFFFFFF, 0xFF8484, 0x943939, 0x000000]);
        assert_eq!(p.obj0, p.obj1);
    }

    #[test]
    fn real_core_runs_a_blank_rom() {
        let rom = vec![0u8; 0x8000];
        let mut st = RunState::default();
        run_core(&rom, &plan(Exit::RunFrames { frames: 2 }), &mut st).unwrap();
        assert_eq!(st.end, Some(End::Completed));
        assert_eq!(st.clock, 2 * FRAME);
        // Ready at the start of VBlank, like the fake.
        assert_eq!(st.frames, 2);
        assert!(!st.screen_blank_at_exit);
        let mut st = RunState::default();
        run_core(&rom, &plan(Exit::RunS { seconds: 1.0 }), &mut st).unwrap();
        assert_eq!(st.frames, (HZ - VBLANK) / FRAME + 1);
        let mut st = RunState::default();
        assert!(matches!(
            run_core(&rom[..100], &plan(Exit::RunFrames { frames: 1 }), &mut st),
            Err(CoreFailure::Load(_))
        ));
    }

    /// A mapper the core does not emulate is a load error like any other
    /// (it used to run as "no MBC" and complete).
    #[test]
    fn real_core_reports_an_unsupported_mapper_as_a_load_error() {
        let mut rom = vec![0u8; 0x8000];
        rom[0x147] = 0xFF; // HuC1
        rom[0x149] = 0x03;
        let mut st = RunState::default();
        assert_eq!(
            run_core(&rom, &plan(Exit::RunFrames { frames: 1 }), &mut st),
            Err(CoreFailure::Load(
                "unsupported cartridge: HuC1 (header cartridge type FF)".into()
            ))
        );
        assert_eq!(st.end, None);
    }

    fn audio_plan(frames: u64) -> RunPlan {
        RunPlan {
            audio_last_frame: true,
            ..plan(Exit::RunFrames { frames })
        }
    }

    const F: u64 = FRAME_CLOCKS as u64;
    const P: u64 = AUDIO_SAMPLE_CLOCKS as u64;

    #[test]
    fn audio_is_captured_over_the_last_frame_only() {
        let mut m = FakeMachine::new(vec![]);
        let st = run_fake(&mut m, &audio_plan(3));
        let a = st.audio.as_ref().unwrap();
        assert_eq!((a.until, a.start, a.end), (3 * F, 2 * F, 3 * F));
        // One sample per window that ends in (2F, 3F].
        assert_eq!(a.samples.len() as u64, 3 * F / P - 2 * F / P);
        assert!(a.samples.iter().all(|&s| s == FAKE_DC));
        assert!(!a.full);
        // Calls before the last frame get no room; one call ends exactly
        // where it starts, and every call from there on gets the rest.
        assert!(
            m.audio_calls
                .iter()
                .all(|&(at, room)| (room > 0) == (at >= 2 * F))
        );
        assert!(m.audio_calls.contains(&(2 * F, AUDIO_SLOTS)));
        assert_eq!(AUDIO_SLOTS, 550);

        // Without the plan flag: no capture, never any room.
        let mut m = FakeMachine::new(vec![]);
        let st = run_fake(&mut m, &plan(Exit::RunFrames { frames: 3 }));
        assert_eq!(st.audio, None);
        assert!(m.audio_calls.iter().all(|&(_, room)| room == 0));
    }

    #[test]
    fn only_sound_in_the_last_frame_counts() {
        let silent = |sound: (u64, u64)| {
            let mut m = FakeMachine::new(vec![]);
            m.sound = Some(sound);
            let st = run_fake(&mut m, &audio_plan(3));
            let a = st.audio.unwrap();
            a.samples.iter().all(|&s| s == a.samples[0])
        };
        // Sound up to the start of the last frame is not heard.
        assert!(silent((0, 2 * F)));
        assert!(silent((F, 2 * F)));
        // Sound anywhere in the last frame is.
        assert!(!silent((2 * F, 2 * F + 2 * P)));
        assert!(!silent((2 * F + 5000, 2 * F + 6000)));
        assert!(!silent((3 * F - 2 * P, 3 * F)));
    }

    #[test]
    fn audio_capture_records_an_early_end() {
        // A lockup (a crash here) in the last frame: the window is
        // incomplete.
        let mut m = FakeMachine::new(vec![(2 * F + 1000, Action::Lock)]);
        let st = run_fake(&mut m, &audio_plan(3));
        assert_eq!(st.end, Some(End::Locked));
        let a = st.audio.unwrap();
        assert!(a.end < a.until, "{} {}", a.end, a.until);
        // One before it: no capture at all.
        let mut m = FakeMachine::new(vec![(F, Action::Lock)]);
        let st = run_fake(&mut m, &audio_plan(3));
        assert_eq!(st.audio, None);
    }
}
