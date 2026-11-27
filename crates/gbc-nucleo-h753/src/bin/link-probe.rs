//! The `link-probe` image (plan Stage 2 M5): the board side of the serial
//! link with a real `gbc_core::Emulator` running, before the `player`
//! image puts display and audio around it. Built with `--features link`
//! (build.sh: out/link-probe.bin), which embeds `GBC_HEADLESS_ROM`
//! (build.sh's default: a generated MBC3+TIMER+RAM+BATTERY ROM,
//! `out/link-probe.gb`, see build.sh).
//!
//! At reset: the banner and a `boot` line at 115200, then the UART
//! switches to [`LINK_BAUD`] with DMA in both directions
//! ([`gbc_nucleo_h753::link_uart`]) and the frame loop starts:
//!
//! 1. sample the joystick and B1 ([`Joystick`]), debounce
//!    (`Debouncer<FRAME_SAMPLES>`), OR with the PC's buttons, and apply
//!    the result at the frame's start clock (`set_buttons_at`); the save
//!    chord (CENTER + B1) starts a board dump;
//! 2. run one frame (`run_frame`, no audio buffer, no display);
//! 3. until the frame's 16.74 ms (70,224 clocks at 4 MiHz) are over,
//!    serve the link: received bytes to the session
//!    ([`gbc_nucleo_h753::link_session`]), its replies and dump frames to
//!    the transmit DMA.
//!
//! Text and frames share the UART. Every text line ends with 0x00, so the
//! PC's frame decoder sees it as one bad frame and resyncs before the
//! next one (as the loader's reports), and text is only sent while the
//! session has nothing to send ([`Session::busy`] false, transmit idle),
//! so it never lands inside a reply or a dump. Text sent:
//! - `buttons merged=0x.. joy=0x.. pc=0x..` when any of them changes
//!   (the button echo for the human's check: `joy` is the debounced raw
//!   lines, bit 0 LEFT, 1 RIGHT, 2 UP, 3 DOWN, 4 CENTER, 5 B1; `merged`
//!   and `pc` the Game Boy bitmap, bit 0 A .. 7 Down);
//! - a `status` line every 10 s once the line has been quiet for 3 s.
//!
//! The rate is a build-time constant, `GBC_LINK_BAUD` (default 2,000,000,
//! the loader's rate and gbc-pc's default, so one `--baud` serves every
//! image).
#![no_std]
#![no_main]

use core::fmt::{self, Write};

use gbc_core::{
    CLOCK_HZ, Config, Emulator, FRAME_CLOCKS, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH,
};
use gbc_firmware::buttons::{
    ChordEdge, Debouncer, FRAME_SAMPLES, RawInputs, SAVE_CHORD, Shared, merge, to_buttons,
};
use gbc_firmware::fmt_buf::FmtBuf;
use gbc_firmware::fnv;
use gbc_firmware::save::{self, Image, Layout};
use gbc_link::{DEFAULT_BAUD, parse_baud};
use gbc_nucleo_h753::joystick::Joystick;
use gbc_nucleo_h753::link_session::{Game, Progress, Session};
use gbc_nucleo_h753::link_uart::LinkUart;
use gbc_nucleo_h753::qspi::NOR_BYTES;
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, led};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "link-probe";

static ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/headless.gb"));
const ROM_NAME: &str = env!("GBC_HEADLESS_ROM_NAME");

const LINK_BAUD: u32 = parse_baud(option_env!("GBC_LINK_BAUD"), DEFAULT_BAUD);
/// One emulated frame in CPU cycles: 70,224 / 4,194,304 s.
const FRAME_CYCLES: u32 = (CPU_HZ as u64 * FRAME_CLOCKS as u64 / CLOCK_HZ as u64) as u32;
/// Cartridge RAM: the MBC5 maximum, 128 KiB (Pan Docs "MBC5").
const CART_RAM_BYTES: usize = 128 * 1024;
/// The save buffer: the largest RAM plus the MBC3 footer, rounded up.
const SAVE_BYTES: usize = CART_RAM_BYTES + 64;

mod buffers {
    use super::*;

    pub struct Bufs {
        pub mem: &'static mut Memory,
        pub frame: &'static mut Frame,
        pub cart_ram: &'static mut [u8; CART_RAM_BYTES],
        pub save: &'static mut [u8; SAVE_BYTES],
    }

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        const TWICE: &str = "buffers taken twice";
        Bufs {
            mem: gbc_nucleo_h753::take_once!(Memory = Memory::new()).expect(TWICE),
            // SAFETY (the `link_section`s): `.axisram` is the NOLOAD AXI
            // SRAM section memory.x defines for such buffers; not zeroed
            // at boot, which is sound for these integer arrays. `main`
            // zeroes the cartridge RAM before `load`; the frame and the
            // save buffer are written before they are read.
            frame: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.frame")]
                Frame = [0; SCREEN_WIDTH * SCREEN_HEIGHT]
            )
            .expect(TWICE),
            cart_ram: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.cart_ram")]
                [u8; CART_RAM_BYTES] = [0; CART_RAM_BYTES]
            )
            .expect(TWICE),
            save: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.save")]
                [u8; SAVE_BYTES] = [0; SAVE_BYTES]
            )
            .expect(TWICE),
        }
    }
}

/// The running emulator behind the session's [`Game`].
struct EmuGame {
    emu: Emulator<'static>,
    rom_hash: u64,
    /// The footer time of the last save applied, stamped back on dumps,
    /// so load then dump is byte-identical (`save` module docs; the board
    /// has no wall clock).
    unix_time: u64,
}

impl Game for EmuGame {
    fn layout(&self) -> Option<Layout> {
        Layout::of_emulator(&self.emu).ok()
    }
    fn rom(&self) -> Option<(u32, u64)> {
        Some((ROM.len() as u32, self.rom_hash))
    }
    fn apply(&mut self, _bytes: &[u8], image: &Image<'_>) -> bool {
        let ok = save::reload(&mut self.emu, image, Config::default()).is_ok();
        if ok {
            self.unix_time = image.unix_time();
        }
        ok
    }
    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        save::take(&self.emu, self.unix_time, out).ok()
    }
}

/// A text line built in memory, sent with a trailing 0x00 (the last byte
/// is kept for it: [`FmtBuf::reserving_nul`]).
type Text = FmtBuf<1024>;

/// Feeds what the DMA received to the session and hands its output to
/// the transmit DMA (when idle).
fn serve(
    link: &mut LinkUart,
    session: &mut Session<'_>,
    game: &mut EmuGame,
    shared: &Shared,
) -> bool {
    let mut got = false;
    link.read(|b| {
        got = true;
        session.feed(b, game, shared);
    });
    if let Some(buf) = link.tx_buf() {
        let n = session.fill_tx(&mut buf[..]);
        link.start_tx(n);
    }
    got
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    board.banner(IMAGE);
    let b = buffers::take();
    b.cart_ram.fill(0);
    let mut emu = Emulator::new(b.mem);
    if let Err(e) = emu.load(ROM, &mut b.cart_ram[..], Config::default()) {
        panic!("load {ROM_NAME}: {:?}", e.kind);
    }
    let rom_hash = fnv::hash(ROM);
    let mut game = EmuGame {
        emu,
        rom_hash,
        unix_time: 0,
    };
    let layout = game.layout();
    let _ = writeln!(
        board.uart,
        "{PREFIX} {IMAGE} boot rom={ROM_NAME} rom_len={} rom_hash={rom_hash:#018x} save_len={} \
         serving gbc-link v{} at baud={LINK_BAUD} frame_cycles={FRAME_CYCLES}\r",
        ROM.len(),
        layout.map_or(0, |l| l.len()),
        gbc_link::PROTOCOL_VERSION,
    );
    let mut link = match LinkUart::new(&mut board.uart, LINK_BAUD) {
        Ok(l) => l,
        Err(e) => panic!("link baud {LINK_BAUD} refused: {e:?}"),
    };
    let shared = Shared::new();
    let mut session = Session::new(&mut b.save[..], NOR_BYTES);
    let mut joy = Joystick::new();
    let mut deb = Debouncer::<FRAME_SAMPLES>::new();
    let mut chord = ChordEdge::default();
    link.send(&[0]);

    let (mut frames, mut core_max) = (0u32, 0u32);
    let mut core_last: u32;
    let mut echoed: Option<(u8, u8, u8)> = None;
    // Time in frames (the DWT counter wraps every 9.1 s).
    let (mut quiet, mut since_status) = (0u32, 0u32);
    let mut text = Text::reserving_nul();
    let mut t_frame = cycles();
    loop {
        let raw = deb.sample(joy.read());
        if chord.update(raw, SAVE_CHORD) {
            session.board_dump(&mut game);
        }
        let merged = merge(to_buttons(raw), shared.pc());
        let at = game.emu.clock();
        let _ = game.emu.set_buttons_at(at, merged);
        let t0 = cycles();
        let _ = game.emu.run_frame(b.frame, &mut []);
        core_last = cycles().wrapping_sub(t0);
        core_max = core_max.max(core_last);
        frames = frames.wrapping_add(1);
        session.end_frame(&shared);
        if frames % 30 == 0 {
            led::toggle();
        }

        let mut got = false;
        loop {
            got |= serve(&mut link, &mut session, &mut game, &shared);
            if cycles().wrapping_sub(t_frame) >= FRAME_CYCLES {
                break;
            }
        }
        t_frame = t_frame.wrapping_add(FRAME_CYCLES);
        if cycles().wrapping_sub(t_frame) >= FRAME_CYCLES {
            // Fell a whole frame behind (a long reload): restart pacing.
            t_frame = cycles();
        }
        quiet = if got { 0 } else { quiet.saturating_add(1) };
        since_status = since_status.saturating_add(1);

        // Text only while the session has nothing to send.
        if session.busy() || !link.tx_idle() {
            continue;
        }
        let now = (merged.bits(), raw.bits(), shared.pc().bits());
        text.clear();
        if echoed != Some(now) {
            echoed = Some(now);
            let _ = write!(
                text,
                "{PREFIX} {IMAGE} buttons merged={:#04x} joy={:#04x} pc={:#04x} frame={frames}\r\n",
                now.0, now.1, now.2
            );
        } else if quiet >= 180 && since_status >= 600 {
            since_status = 0;
            let _ = status(
                &mut text, &session, &mut link, frames, core_last, core_max, &game,
            );
        } else {
            continue;
        }
        let _ = text.push_nul();
        link.send(text.as_bytes());
    }
}

fn status(
    t: &mut Text,
    session: &Session<'_>,
    link: &mut LinkUart,
    frames: u32,
    core_last: u32,
    core_max: u32,
    game: &EmuGame,
) -> fmt::Result {
    let c = session.counters();
    let e = link.errors();
    write!(t, "{PREFIX} {IMAGE} status frames={frames} state=")?;
    match session.progress() {
        Progress::Idle => write!(t, "idle")?,
        Progress::Loading { received, len } => write!(t, "loading({received}/{len})")?,
        Progress::Dumping { queued, len } => write!(t, "dumping({queued}/{len})")?,
    }
    write!(
        t,
        " link_frames={} bad_frames={} duplicates={} hellos={} inputs={} pc_timeouts={} \
         save_begins={} save_chunks={} saves_applied={} saves_refused={} dumps={} dumps_done={} \
         naks={} replies_dropped={}",
        c.frames,
        c.bad_frames,
        c.duplicates,
        c.hellos,
        c.inputs,
        c.pc_timeouts,
        c.save_begins,
        c.save_chunks,
        c.saves_applied,
        c.saves_refused,
        c.dumps,
        c.dumps_done,
        c.naks,
        c.replies_dropped,
    )?;
    write!(
        t,
        " overruns={} framing={} noise={} rx_high={} rx_overflow={} rx_dma_errors={} tx_errors={} \
         core_cycles_last={core_last} core_cycles_max={core_max} unix_time={} baud={}\r\n",
        e.overruns,
        e.framing,
        e.noise,
        link.rx_high(),
        link.rx_overflow(),
        link.rx_dma_errors(),
        link.tx_errors(),
        game.unix_time,
        link.setting().actual,
    )?;
    Ok(())
}
