//! `--headless --frames N` (or `--headless --replay-input FILE`): run
//! without a window or audio device.

use gbc_core::{
    Buttons, Emulator, Events, Frame, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample, WHITE,
};

use std::path::Path;
use std::time::Instant;

use crate::frame_log::{self, Emulated, FrameLog};

use crate::input_log::{Driver, Inputs};
use crate::png;
use crate::session::{self, Autosave, Options};

/// Samples per frame at 32,768 Hz is 546.1: 550 always fits one frame.
const AUDIO_SLOTS: usize = 550;

/// Runs `frames` complete frames (after one unnamed run to the first frame
/// boundary), discarding video and audio; `None` (replays only) runs to
/// the end of the recording, which also ends a longer `frames`. Returns
/// whether the CPU locked up (the run still completes: a locked CPU is a
/// ROM behavior, not a front-end failure; a warning is printed once).
pub fn run(
    opts: &Options<'_>,
    frames: Option<u64>,
    screenshot: Option<&Path>,
    inputs: &Inputs<'_>,
) -> Result<bool, String> {
    session::run(opts, |emu, autosave, start| {
        let mut driver = Driver::open(inputs, start)?;
        // The core starts mid-frame (post-boot), so the first call ends early at
        // the first FRAME_READY. It is not counted: `frames` complete frames
        // follow it, each `FRAME_CLOCKS` long.
        let calls = match (frames.map(|f| f + 1), driver.replay_frames()) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return Err("--headless needs --frames or --replay-input".into()),
        };
        let mut log = frame_log::from_env(Instant::now(), &start.rom_title).transpose()?;
        let out = play(emu, autosave, &mut driver, &mut log, calls, |_| {
            Buttons::default()
        });
        driver.finish()?;
        if let Some(n) = log.map(FrameLog::finish).transpose()? {
            eprintln!("frame log: {n} rows written");
        }
        // `shown` holds the last complete frame, unless the screen is blank
        // (LCD off), when it is unspecified: `visible` gives white.
        if let Some(p) = screenshot {
            png::write_frame(p, crate::video::visible(&out.shown, emu.screen_blank()))?;
        }
        Ok(out.locked)
    })
    .and_then(|r| r)
}

pub struct Played {
    pub locked: bool,
    /// The last complete frame.
    pub shown: Box<Frame>,
}

/// Runs `calls` frames, each with the buttons `driver` picks (`live(n)`
/// for frame `n` unless replaying), as the window does: buttons set once
/// before each `run_frame` call. With `log` (`GBC_FRAME_LOG`), each frame
/// gets a row without the window's wake-up and present columns.
pub fn play(
    emu: &mut Emulator<'_>,
    autosave: &mut Autosave,
    driver: &mut Driver,
    log: &mut Option<FrameLog>,
    calls: u64,
    mut live: impl FnMut(u64) -> Buttons,
) -> Played {
    // Double-buffered like the window (`window::App`), so the picture
    // hashed and saved is the last complete frame even when a call ends
    // mid-frame.
    let mut draw: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
    let mut shown: Box<Frame> = Box::new([WHITE; SCREEN_WIDTH * SCREEN_HEIGHT]);
    let mut audio = [StereoSample::default(); AUDIO_SLOTS];
    let mut locked = false;
    let mut was_blank = false;
    for n in 0..calls {
        if crate::shutdown::requested() {
            break;
        }
        let (buttons, mark) = driver.begin_frame(live(n));
        emu.set_buttons(buttons);
        let t_start = Instant::now();
        let r = emu.run_frame(&mut draw, &mut audio);
        let t_end = Instant::now();
        let ready = r.events.contains(Events::FRAME_READY);
        let blank = emu.screen_blank();
        if let Some(l) = log {
            let same = ready && draw == shown;
            let (changed, motion) =
                frame_log::picture_change(&shown, &draw, ready, same, was_blank, blank);
            l.frame(&Emulated {
                frame: n,
                wake: None,
                wake_late: None,
                start: t_start,
                end: t_end,
                ready,
                blank,
                changed,
                motion,
                audio_fill_ms: None,
            });
            if mark {
                l.mark();
            }
        }
        was_blank = blank;
        if ready {
            std::mem::swap(&mut draw, &mut shown);
        }
        if r.events.contains(Events::CPU_LOCKED) && !locked {
            locked = true;
            eprintln!("warning: the CPU locked up in frame {n}; continuing");
        }
        driver.end_frame(ready, blank, crate::video::visible(&shown, blank));
        autosave.after_frame(emu);
    }
    Played { locked, shown }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat_palette::Mode;
    use crate::input_log::Recording;

    /// MBC1+RAM+BATTERY. Enables RAM, then loops: BGP = (A000) xor P1
    /// (action buttons), also stored to A001. So the picture depends on
    /// both the held buttons and the save the run started from.
    fn input_rom() -> Vec<u8> {
        let mut rom = vec![0u8; 0x8000];
        rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]);
        rom[0x147] = 0x03;
        rom[0x149] = 0x02;
        #[rustfmt::skip]
        let code = [
            0x3E, 0x0A, 0xEA, 0x00, 0x00, // ld a,0A; ld (0000),a
            0x3E, 0x10, 0xE0, 0x00,       // loop: ld a,10; ldh (00),a
            0xF0, 0x00, 0xF0, 0x00,       // ldh a,(00) x2
            0x47,                         // ld b,a
            0xFA, 0x00, 0xA0,             // ld a,(A000)
            0xA8,                         // xor b
            0xE0, 0x47,                   // ldh (47),a
            0xEA, 0x01, 0xA0,             // ld (A001),a
            0x18, 0xEC,                   // jr loop
        ];
        rom[0x150..0x150 + code.len()].copy_from_slice(&code);
        rom[0x14D] = rom[0x134..=0x14C]
            .iter()
            .fold(0u8, |x, &b| x.wrapping_sub(b).wrapping_sub(1));
        rom
    }

    fn script(n: u64) -> Buttons {
        match n {
            20..40 => Buttons::A,
            40..60 => Buttons::B,
            60..80 => Buttons::A | Buttons::B,
            80..100 => Buttons::START | Buttons::UP,
            _ => Buttons::default(),
        }
    }

    #[test]
    fn a_recording_replays_bit_exactly_from_its_own_save() {
        let dir = std::env::temp_dir().join(format!("gbc-replay-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rom_path = dir.join("in.gb");
        std::fs::write(&rom_path, input_rom()).unwrap();
        let sav = dir.join("in.sav");
        let mut ram = vec![0u8; 0x2000];
        ram[0] = 0x01;
        std::fs::write(&sav, &ram).unwrap();
        let rec = dir.join("rec.txt");
        let (h_rec, h_rep, h_live) = (dir.join("a.txt"), dir.join("b.txt"), dir.join("c.txt"));
        let opts = Options {
            rom_path: &rom_path,
            now: || 1,
            sync_rtc_on_load: false,
            dmg_palette: Mode::Gray,
            replay: None,
        };

        // Record 120 frames of scripted input (as the window would), with
        // a mark after frame 50.
        session::run(&opts, |emu, autosave, start| {
            let inputs = Inputs {
                record: Some(&rec),
                replay: None,
                hashes: Some(&h_rec),
            };
            let mut d = Driver::open(&inputs, start).unwrap();
            play(emu, autosave, &mut d, &mut None, 51, script);
            assert_eq!(d.mark(), 50);
            let mut offset = |n| script(n + 51);
            play(emu, autosave, &mut d, &mut None, 69, &mut offset);
            d.finish().unwrap();
        })
        .unwrap();
        let recording = Recording::load(&rec).unwrap();
        assert_eq!(recording.frames, 120);
        assert!(
            recording
                .events
                .contains(&crate::input_log::Event::Mark(50))
        );
        assert_eq!(recording.header.cart_ram[0], 0x01, "the starting save");
        assert_eq!(
            recording.header.sav,
            Some((0x2000, crate::input_log::fnv1a64(&ram)))
        );

        // The save on disk changes (the exit save, then by hand); the
        // replay must start from the recorded one and write none.
        let mut other = std::fs::read(&sav).unwrap();
        other[0] = 0x02;
        std::fs::write(&sav, &other).unwrap();
        let replay_opts = Options {
            replay: Some(&recording.header),
            dmg_palette: Mode::Auto, // the recording's palette wins
            ..opts
        };
        let inputs = Inputs {
            record: None,
            replay: Some(&recording),
            hashes: Some(&h_rep),
        };
        run(&replay_opts, None, None, &inputs).unwrap();
        assert_eq!(std::fs::read(&sav).unwrap(), other, "replay wrote no save");

        let a = std::fs::read_to_string(&h_rec).unwrap();
        let b = std::fs::read_to_string(&h_rep).unwrap();
        assert_eq!(a.lines().count(), 120);
        assert_eq!(a, b, "replay is bit-exact");
        // The hashes see the input: several distinct pictures.
        let mut distinct: Vec<_> = a.lines().map(|l| l.rsplit(' ').next().unwrap()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        assert!(distinct.len() >= 3, "{distinct:?}");

        // And the save: the same input from the changed save differs.
        let live_opts = Options {
            replay: None,
            ..opts
        };
        session::run(&live_opts, |emu, autosave, start| {
            let inputs = Inputs {
                hashes: Some(&h_live),
                ..Inputs::default()
            };
            let mut d = Driver::open(&inputs, start).unwrap();
            play(emu, autosave, &mut d, &mut None, 120, script);
            d.finish().unwrap();
        })
        .unwrap();
        let c = std::fs::read_to_string(&h_live).unwrap();
        assert_ne!(a, c, "the starting save shows in the picture");

        // The wrong ROM is refused.
        let mut wrong = input_rom();
        wrong[0x7000] = 1;
        std::fs::write(&rom_path, wrong).unwrap();
        let e = run(&replay_opts, None, None, &inputs).unwrap_err();
        assert!(e.contains("different ROM"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
