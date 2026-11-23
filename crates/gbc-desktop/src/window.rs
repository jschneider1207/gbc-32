//! The interactive front end: a winit window drawn with softbuffer.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

use gbc_core::{
    Buttons, Emulator, Events, Frame, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE,
};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::input::key_to_button;
use crate::pacing::Pacer;
use crate::session::Autosave;
use crate::video;

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
    held: Buttons,
    pacer: Pacer,
    error: Option<String>,
}

impl App<'_, '_> {
    fn run_one_frame(&mut self) {
        self.emu.set_buttons(self.held);
        let r = self.emu.run_frame(&mut self.draw, &mut self.audio);
        if r.events.contains(Events::FRAME_READY) {
            std::mem::swap(&mut self.draw, &mut self.shown);
            if let Some(g) = &self.gfx {
                g.window.request_redraw();
            }
        }
        self.autosave.after_frame(self.emu);
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
        if let Ok(mut buf) = g.surface.buffer_mut() {
            video::blit(&self.shown, &mut buf, w.get() as usize, h.get() as usize);
            let _ = buf.present();
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
                self.pacer = Pacer::new(Instant::now());
                g.window.request_redraw();
                self.gfx = Some(g);
            }
            Err(e) => {
                self.error = Some(e);
                el.exit();
            }
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::Focused(false) => self.held = Buttons::default(),
            WindowEvent::Resized(_) => self.redraw(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else {
                    return;
                };
                let down = event.state == ElementState::Pressed;
                if code == KeyCode::Escape && down {
                    el.exit();
                } else if let Some(b) = key_to_button(code) {
                    self.held.set(b, down);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        for _ in 0..self.pacer.due(Instant::now()) {
            self.run_one_frame();
        }
        el.set_control_flow(ControlFlow::WaitUntil(self.pacer.next_deadline()));
    }
}

/// Opens the window and plays until it is closed. `Err` is a message for
/// stderr; it returns normally so the caller still writes the save.
pub fn run(
    emu: &mut Emulator<'_>,
    autosave: &mut Autosave,
    title: String,
    scale: u32,
) -> Result<(), String> {
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
        held: Buttons::default(),
        pacer: Pacer::new(Instant::now()),
        error: None,
    };
    el.run_app(&mut app)
        .map_err(|e| format!("event loop failed: {e}"))?;
    app.error.map_or(Ok(()), Err)
}
