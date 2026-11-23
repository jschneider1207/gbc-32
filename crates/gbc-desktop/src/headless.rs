//! `--headless --frames N`: run without a window or audio device.

use gbc_core::{Events, Frame, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE};

use std::path::Path;

use crate::png;
use crate::session::{self, Options};

/// Samples per frame at 32,768 Hz is 546.1: 550 always fits one frame.
const AUDIO_SLOTS: usize = 550;

/// Runs `frames` frames, discarding video and audio. Returns whether the
/// CPU locked up (the run still completes: a locked CPU is a ROM
/// behavior, not a front-end failure; a warning is printed once).
pub fn run(opts: &Options<'_>, frames: u64, screenshot: Option<&Path>) -> Result<bool, String> {
    session::run(opts, |emu, autosave| {
        let mut frame: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
        let mut audio = [StereoSample::default(); AUDIO_SLOTS];
        let mut locked = false;
        for n in 0..frames {
            let r = emu.run_frame(&mut frame, &mut audio);
            if r.events.contains(Events::CPU_LOCKED) && !locked {
                locked = true;
                eprintln!("warning: the CPU locked up in frame {n}; continuing");
            }
            autosave.after_frame(emu);
        }
        // The frame buffer holds the last complete frame: run_frame always
        // finishes one.
        if let Some(p) = screenshot {
            png::write_frame(p, &frame)?;
        }
        Ok(locked)
    })
    .and_then(|r| r)
}
