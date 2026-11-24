//! The interactive front end: a winit window drawn with softbuffer.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gbc_core::{Emulator, Events, Frame, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE};
use gilrs::{Axis, EventType, GamepadId, Gilrs};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::dwm;
use crate::frame_log::{self, Emulated, FrameLog};
use crate::frame_stats::{self, FrameStats};
use crate::input::{HeldInput, key_to_button, pad_button_to_button};
use crate::input_log::Driver;
use crate::pacing::Pacing;
use crate::png;
use crate::present::Presenter;
use crate::session::Autosave;
use crate::video;
use crate::wait;

/// Samples per frame at 32,768 Hz is 546.1: 550 always fits one frame.
const AUDIO_SLOTS: usize = 550;

struct Gfx {
    window: Arc<Window>,
    surface: Surface<winit::event_loop::OwnedDisplayHandle, Arc<Window>>,
}

struct App<'e, 'a> {
    emu: &'e mut Emulator<'a>,
    autosave: &'e mut Autosave,
    title: String,
    scale: u32,
    gfx: Option<Gfx>,
    /// The frame being drawn into, and the last complete one.
    draw: Box<Frame>,
    shown: Box<Frame>,
    audio: [StereoSample; AUDIO_SLOTS],
    held: HeldInput<GamepadId>,
    /// `None` when gilrs could not start: keyboard only.
    pads: Option<Gilrs>,
    pacing: Pacing,
    /// The core says the screen is blank (LCD off, or the first frame
    /// after it turned on): show white instead of `shown`.
    blank: bool,
    /// Frames still to run before the window closes itself (`--frames`).
    frames_left: Option<u64>,
    /// `GBC_FRAME_STATS=1`: present/pacing statistics.
    stats: Option<FrameStats>,
    /// Buttons per frame (live, or `--replay-input`), `--record-input`
    /// and `--frame-hashes` (ADR 0030).
    driver: Driver,
    /// `GBC_FRAME_LOG`: one CSV row per frame.
    log: Option<FrameLog>,
    /// The frame about to run (`Driver` frame numbering).
    frame_no: u64,
    /// The audio fill estimate at the current wake-up, for `log`.
    wake_fill: Option<f64>,
    /// When the current event-loop iteration began, and how late that
    /// was for a timed wake-up, for `log`.
    wake: Option<(Instant, Option<Duration>)>,
    /// `--no-input`: ignore the keyboard and pads (unattended runs).
    no_input: bool,
    /// Presents at a safe phase of the display refresh (ADR 0024
    /// amendment); `None` with `GBC_PRESENT_PHASE=0`. Without DWM timing
    /// (off Windows) every picture is presented at once, as before.
    presenter: Option<Presenter>,
    /// Precise waits for frame starts and timed presents (`wait.rs`);
    /// off with `GBC_PRECISE_WAIT=0`.
    precise: bool,
    /// The deadline the current wait should end at precisely.
    deadline: Option<Instant>,
    /// The time the current wait aims for (the deadline, or a plain
    /// wake-up time), for wake lateness.
    aimed: Option<Instant>,
    /// This iteration began with a timed wake-up (`new_events`).
    timed: bool,
    counters: Counters,
    error: Option<String>,
}

impl App<'_, '_> {
    /// Runs one frame and returns how many audio samples it wrote.
    fn run_one_frame(&mut self) -> usize {
        let live = if self.no_input {
            gbc_core::Buttons::default()
        } else {
            self.held.held()
        };
        let (buttons, replayed_mark) = self.driver.begin_frame(live);
        self.emu.set_buttons(buttons);
        let t_start = Instant::now();
        let r = self.emu.run_frame(&mut self.draw, &mut self.audio);
        let t_end = Instant::now();
        let ready = r.events.contains(Events::FRAME_READY);
        let blank = self.emu.screen_blank();
        let was_blank = self.blank;
        let blank_changed = blank != was_blank;
        self.blank = blank;
        // The picture equals the last one (only computed when measured).
        let same = (self.stats.is_some() || self.log.is_some()) && ready && self.draw == self.shown;
        if let Some(s) = &mut self.stats
            && let Some(h) = s.emulated(ready, same, t_end)
        {
            eprintln!("{h}");
        }
        if let Some(l) = &mut self.log {
            let (changed, motion) =
                frame_log::picture_change(&self.shown, &self.draw, ready, same, was_blank, blank);
            l.frame(&Emulated {
                frame: self.frame_no,
                wake: self.wake.map(|w| w.0),
                wake_late: self.wake.and_then(|w| w.1),
                start: t_start,
                end: t_end,
                ready,
                blank,
                changed,
                motion,
                audio_fill_ms: self.wake_fill,
            });
            if replayed_mark {
                l.mark();
            }
        }
        self.frame_no += 1;
        if ready {
            std::mem::swap(&mut self.draw, &mut self.shown);
        }
        self.driver
            .end_frame(ready, blank, video::visible(&self.shown, blank));
        if ready || blank_changed {
            self.picture_ready(t_end);
        }
        self.autosave.after_frame(self.emu);
        r.audio_samples.min(AUDIO_SLOTS)
    }

    /// A new picture (or a blank screen) is ready at `at`: present it at
    /// its phase, or at once (`request_redraw`) when that doesn't apply.
    fn picture_ready(&mut self, at: Instant) {
        let Some(g) = &self.gfx else { return };
        let timed = self
            .presenter
            .as_mut()
            .is_some_and(|p| p.picture(at, dwm::grid()));
        if !timed {
            g.window.request_redraw();
            self.counters.fallbacks += 1;
        }
    }

    /// When to wake next, the deadline to meet precisely (if any) and
    /// the time the wake-up aims for.
    fn plan_wake(&self, now: Instant) -> (Instant, Option<Instant>, Instant) {
        let present = self.presenter.as_ref().and_then(Presenter::next);
        if !self.precise {
            let wake = self.pacing.next_wake(now);
            let wake = present.map_or(wake, |p| wake.min(p));
            return (wake, None, wake);
        }
        let due = self.pacing.next_due(now);
        let deadline = [due.precise.then_some(due.at), present]
            .into_iter()
            .flatten()
            .min();
        match deadline {
            Some(d) if due.precise || wait::wake_for(d, now) <= due.at => {
                (wait::wake_for(d, now), Some(d), d)
            }
            _ => (due.at, None, due.at),
        }
    }

    /// No frames left to run: `--frames` reached, or the replay ended.
    fn finished(&self) -> bool {
        self.frames_left == Some(0) || self.driver.replay_done()
    }

    /// Drains gamepad events (hotplug included) into `held`.
    fn poll_pads(&mut self) {
        let Some(g) = &mut self.pads else { return };
        while let Some(ev) = g.next_event() {
            let id = ev.id;
            match ev.event {
                EventType::ButtonPressed(b, _) | EventType::ButtonReleased(b, _) => {
                    let down = matches!(ev.event, EventType::ButtonPressed(..));
                    if let Some(b) = pad_button_to_button(b) {
                        self.held.set_pad_button(id, b, down);
                    }
                }
                EventType::AxisChanged(Axis::LeftStickX, v, _) => self.held.set_pad_stick_x(id, v),
                EventType::AxisChanged(Axis::LeftStickY, v, _) => self.held.set_pad_stick_y(id, v),
                EventType::Disconnected => self.held.release_pad(id),
                _ => {}
            }
        }
    }

    fn redraw(&mut self) {
        let Some(g) = &mut self.gfx else { return };
        let size = g.window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if g.surface.resize(w, h).is_err() {
            return;
        }
        let t = Instant::now();
        if let Ok(mut buf) = g.surface.buffer_mut() {
            video::blit(
                video::visible(&self.shown, self.blank),
                &mut buf,
                w.get() as usize,
                h.get() as usize,
            );
            if buf.present().is_ok() {
                let now = Instant::now();
                if let Some(p) = &mut self.presenter {
                    p.presented(t, now, dwm::grid());
                }
                if let Some(s) = &mut self.stats {
                    s.presented(now);
                    s.present_took(now - t);
                }
                if let Some(l) = &mut self.log {
                    l.presented(t, now, dwm::now());
                }
            }
        }
    }
}

impl ApplicationHandler for App<'_, '_> {
    fn resumed(&mut self, el: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        let k = self.scale;
        let attrs = Window::default_attributes()
            .with_title(self.title.clone())
            .with_inner_size(PhysicalSize::new(
                SCREEN_WIDTH as u32 * k,
                SCREEN_HEIGHT as u32 * k,
            ))
            .with_min_inner_size(PhysicalSize::new(SCREEN_WIDTH as u32, SCREEN_HEIGHT as u32));
        let made = el
            .create_window(attrs)
            .map_err(|e| format!("can't create a window: {e}"))
            .and_then(|w| {
                let w = Arc::new(w);
                let ctx = Context::new(el.owned_display_handle())
                    .map_err(|e| format!("can't open the display: {e}"))?;
                let surface = Surface::new(&ctx, w.clone())
                    .map_err(|e| format!("can't create a drawing surface: {e}"))?;
                Ok(Gfx { window: w, surface })
            });
        match made {
            Ok(g) => {
                self.pacing.restart(Instant::now());
                g.window.request_redraw();
                self.gfx = Some(g);
            }
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        self.timed = match cause {
            StartCause::ResumeTimeReached { .. } => true,
            // Woken by an event after the deadline had already passed: as
            // late as a timed wake-up would have been.
            StartCause::WaitCancelled {
                requested_resume: Some(r),
                ..
            } => Instant::now() > r,
            _ => false,
        };
        if let Some(s) = &mut self.stats {
            // The spin finishing a precise wait counts as busy.
            s.iteration_began(Instant::now());
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            // Release everything, pads too: simplest, and nothing sticks.
            WindowEvent::Focused(false) => self.held.clear(),
            WindowEvent::Resized(_) => self.redraw(),
            WindowEvent::RedrawRequested => {
                self.counters.redraw_events += 1;
                self.redraw();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let down = event.state == ElementState::Pressed;
                if code == KeyCode::Escape && down {
                    el.exit();
                } else if code == KeyCode::KeyM {
                    // Mark the last frame (a stutter the human saw).
                    if down && !event.repeat && self.driver.marks_enabled(self.log.is_some()) {
                        self.driver.mark();
                        if let Some(l) = &mut self.log {
                            l.mark();
                        }
                    }
                } else if let Some(b) = key_to_button(code) {
                    self.held.set_key(b, down);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        if crate::shutdown::requested() {
            el.exit();
            return;
        }
        // Finish a precise wait: the event loop woke us a margin early.
        if let Some(d) = wait::spin_target(Instant::now(), self.deadline) {
            self.counters.add(wait::spin_until(d));
        }
        self.deadline = None;
        let t = Instant::now();
        if self.stats.is_some() || self.log.is_some() {
            let late = self
                .aimed
                .filter(|_| self.timed)
                .map(|a| t.saturating_duration_since(a));
            self.wake = Some((t, late));
            if let Some(s) = &mut self.stats
                && let (Some(a), true) = (self.aimed, self.timed)
            {
                s.woke(a, t);
            }
        }
        self.timed = false;
        self.poll_pads();
        let t_pads = Instant::now();
        self.pacing.begin(t_pads);
        self.wake_fill = self.pacing.fill_ms();
        if let (Some(s), Some(fill)) = (&mut self.stats, self.wake_fill) {
            s.fill(fill);
        }
        let mut ran = 0;
        while self.pacing.should_run(ran) {
            if self.finished() {
                break;
            }
            let n = self.run_one_frame();
            self.pacing.push_audio(&self.audio[..n]);
            ran += 1;
            if let Some(left) = &mut self.frames_left {
                *left -= 1;
            }
        }
        // A picture whose present time has come (or is ready inside its
        // window right now).
        if self
            .presenter
            .as_ref()
            .is_some_and(|p| p.due(Instant::now()))
        {
            self.counters.timed_presents += 1;
            self.redraw();
        }
        if let Some(s) = &mut self.stats {
            s.pads_took(t_pads - t);
            s.step_took(t_pads.elapsed());
            s.ran(ran);
            let w = self.autosave.take_write_stats();
            s.saved(w.count, w.max);
            if let Some(line) = s.report(Instant::now()) {
                eprintln!("{line}");
            }
            s.iteration_ended(Instant::now());
        }
        if self.finished() {
            el.exit();
            return;
        }
        let (wake, deadline, aimed) = self.plan_wake(Instant::now());
        self.deadline = deadline;
        self.aimed = Some(aimed);
        el.set_control_flow(ControlFlow::WaitUntil(wake));
    }
}

/// Opens the window and plays until it is closed (or `frames` or the
/// replay ran out). `Err` is a message for stderr; it returns normally so
/// the caller still writes the save.
#[allow(clippy::too_many_arguments)]
pub fn run(
    emu: &mut Emulator<'_>,
    autosave: &mut Autosave,
    title: String,
    scale: u32,
    frames: Option<u64>,
    screenshot: Option<&Path>,
    driver: Driver,
    log: Option<FrameLog>,
    no_input: bool,
) -> Result<(), String> {
    let started = (Instant::now(), dwm::process_cpu());
    let el = EventLoop::new().map_err(|e| format!("can't start the event loop: {e}"))?;
    let mut app = App {
        emu,
        autosave,
        title,
        scale,
        gfx: None,
        draw: Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]),
        shown: Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]),
        audio: [StereoSample::default(); AUDIO_SLOTS],
        held: HeldInput::new(),
        pads: pads_init(),
        pacing: Pacing::open(Instant::now()),
        blank: false,
        // +1: the first call ends early at the first frame boundary (see
        // `headless::run`), so `frames` complete frames follow it.
        frames_left: frames.map(|f| f + 1),
        stats: frame_stats::from_env(Instant::now()),
        driver,
        log,
        frame_no: 0,
        wake_fill: None,
        wake: None,
        no_input,
        presenter: (!env_off("GBC_PRESENT_PHASE")).then(Presenter::new),
        precise: !env_off("GBC_PRECISE_WAIT"),
        deadline: None,
        aimed: None,
        timed: false,
        counters: Counters::default(),
        error: None,
    };
    let ran = el
        .run_app(&mut app)
        .map_err(|e| format!("event loop failed: {e}"));
    let App {
        driver,
        log,
        shown,
        blank,
        error,
        counters,
        stats,
        presenter,
        ..
    } = app;
    if stats.is_some() || log.is_some() {
        eprintln!("{}", counters.summary(started));
        if let Some(p) = presenter {
            eprintln!(
                "present phase: on; present estimate {:.3} ms",
                p.present_estimate().as_secs_f64() * 1e3
            );
        }
    }
    // The recording and logs are finished even if the loop failed.
    let inputs = driver.finish();
    let logged = log.map(FrameLog::finish).transpose();
    ran?;
    if let Some(n) = logged? {
        eprintln!("frame log: {n} rows written");
    }
    inputs?;
    if let Some(p) = screenshot {
        png::write_frame(p, video::visible(&shown, blank))?;
    }
    error.map_or(Ok(()), Err)
}

/// `name=0` in the environment.
fn env_off(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| v == "0")
}

/// Precise-wait spins (their CPU cost) and how pictures were presented,
/// for the summary at exit.
#[derive(Debug, Default)]
struct Counters {
    count: u64,
    total: Duration,
    max: Duration,
    /// Presents by path: timed by the presenter, pictures handed to
    /// `request_redraw` instead, and `RedrawRequested` events.
    timed_presents: u64,
    fallbacks: u64,
    redraw_events: u64,
}

impl Counters {
    fn add(&mut self, d: Duration) {
        self.count += 1;
        self.total += d;
        self.max = self.max.max(d);
    }

    /// One line for stderr: the spins against the run's wall-clock time,
    /// and the process's CPU time (Windows) since `started`.
    fn summary(&self, started: (Instant, Option<Duration>)) -> String {
        let wall = started.0.elapsed().as_secs_f64();
        let ms = |d: Duration| d.as_secs_f64() * 1e3;
        let cpu = match (started.1, dwm::process_cpu()) {
            (Some(a), Some(b)) => format!(
                "; process CPU {:.2} s in {wall:.1} s ({:.1}% of one core)",
                b.saturating_sub(a).as_secs_f64(),
                b.saturating_sub(a).as_secs_f64() / wall * 100.0
            ),
            _ => String::new(),
        };
        format!(
            "precise wait: {} spins, {:.0} ms in {wall:.1} s ({:.2}% of one core), mean {:.3} max {:.3} ms{cpu}; \
             presents: {} timed, {} pictures at once, {} redraw events",
            self.count,
            ms(self.total),
            self.total.as_secs_f64() / wall.max(1e-9) * 100.0,
            ms(self.total) / self.count.max(1) as f64,
            ms(self.max),
            self.timed_presents,
            self.fallbacks,
            self.redraw_events,
        )
    }
}

/// Gamepads are optional: a failure (e.g. no udev) is one warning.
fn pads_init() -> Option<Gilrs> {
    match Gilrs::new() {
        Ok(g) => Some(g),
        Err(e) => {
            eprintln!("warning: gamepads unavailable ({e}); keyboard only");
            None
        }
    }
}
