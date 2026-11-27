//! The `player` image (plan Stage 2 M5): the dev-board game player. The
//! ROM comes from the QSPI NOR (an ADR 0037 image the `loader` put there,
//! as the `headless-qspi` image); the audio-paced loop
//! ([`gbc_firmware::paced::run_with`]) runs it with the display (the
//! `paced` image's newest-frame-at-TE policy, [`display`]), the DAC (with
//! its stall watchdog) and live input; the serial link session
//! ([`link_session`]) is the loop's per-frame [`Service`] hook. How to
//! play: `docs/reference/player.md` (`scripts/play-on-board.sh`).
//!
//! # Live mode (`--features player`, build.sh: out/player.bin)
//!
//! - Input: [`LiveInput`]: the joystick and B1 debounced, OR-ed with the
//!   PC's `Input` buttons, sampled once per frame and applied at the
//!   frame's start clock. CENTER + B1 dumps the save to the PC.
//! - Link (UART at `GBC_LINK_BAUD`, default 2,000,000, DMA both ways):
//!   `Hello`; `RomBegin` answers `AlreadyLoaded` when the NOR holds that
//!   ROM (the image header's length and hash), else `Nak UnexpectedType`
//!   (load it with the `loader`); save loads go through `save::reload`;
//!   dumps on request (`gbc-pc dump`, F5 in `gbc-pc play`) and on the chord.
//! - Audio to the PC (ADR 0041): after `AudioStart` the DAC ISR copies
//!   each refilled half's real frames into [`TEE`] and the service hook
//!   sends them as `Audio` messages (after the replies and the `Stats`,
//!   before dump frames), as many as the idle transmit buffer holds; a
//!   full [`TEE`] drops halves (`audio_dropped`), never DAC samples.
//! - DMG-mode games get [`COMPAT_PALETTE`] (romtest's common palette),
//!   set before the first frame and on every save reload.
//! - Statistics every [`STATS_FRAMES`] frames (5.0 s): a `Stats` frame
//!   while a PC session is active (a link frame came within
//!   [`ACTIVE_FRAMES`]), else a text line; also right when a session
//!   starts. Fields: `docs/reference/player.md`; the window and the
//!   over-budget rule: [`gbc_firmware::player`]. Text lines end with 0x00
//!   and are sent only while the session has nothing to send (the
//!   link-probe rules), so they never land inside a frame.
//! - No valid image in the NOR: the panel goes dark red, a `player no-rom`
//!   text line repeats every 2 s, and the link answers `Hello` (and `Nak`s
//!   `RomBegin`), so `gbc-pc` sees the player and the wrapper flashes the
//!   loader. Nothing runs until a reset.
//!
//! # Scripted mode (`--features player-script`, out/player-script.bin)
//!
//! The input is the build's `GBC_HEADLESS_INPUT` script for
//! `GBC_HEADLESS_FRAMES` frames (else the script's `end`, else 35,837),
//! joystick and PC are ignored, the UART stays at 115200 and the link is
//! off, and the core keeps its default DMG palette, so the run's `paced `
//! lines must equal the host's `headless --paced` (same ROM, script and
//! frames). The script is either the frame format (`gbc_firmware::input`)
//! or a `clocks v1` table (`gbc_firmware::replay`: an `acceptance/*.toml`
//! `input` converted by `tools/replay-input table`, its changes scheduled
//! at their own clocks as romtest does; a `stats input` line follows the
//! run's lines). The run's lines and the `stats display paced`, `stats dac`
//! and `stats player` lines are re-sent in the `paced` image's framed report,
//! so `run-paced.sh out/player-script.bin <rom>` checks it (after the
//! loader put `<rom>` in the NOR).
#![no_std]
#![no_main]

use core::cell::{Cell, RefCell};
use core::fmt::{self, Write};

use gbc_core::{Config, Emulator, Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::audio::{AudioTee, FeedRing, FeedTap};
use gbc_link::audio::{U12, U12_BYTES, encode_u12};

/// One DMA refill (the tee's half).
const DMA_HALF_FRAMES: usize = audio_dac::HALF;
use gbc_firmware::buttons::{FRAME_SAMPLES, LiveInput, Shared};
use gbc_firmware::fmt_buf::FmtBuf;
use gbc_firmware::headless::{Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, InputSource, Press, Script};
use gbc_firmware::paced::{self, Feed, FrameCycles, Hooks, Idle, Service, Serviced};
use gbc_firmware::player::{COMPAT_PALETTE, Window, frame_budget};
use gbc_firmware::replay::{self, Change, ClockScript};
use gbc_firmware::save::{self, Image, Layout};
use gbc_link::{DEFAULT_BAUD, MAX_FRAME, Message, parse_baud};
use gbc_nucleo_h753::audio_dac::{self, AudioDac};
use gbc_nucleo_h753::display::{self, Display};
use gbc_nucleo_h753::joystick::Joystick;
use gbc_nucleo_h753::link_session::{Game, Session};
use gbc_nucleo_h753::link_uart::LinkUart;
use gbc_nucleo_h753::pac::interrupt;
use gbc_nucleo_h753::paced_board::{
    Capture, Dwt, RAMP_LEFT, dac_line, report, set_sleep_clock, start_dac,
};
use gbc_nucleo_h753::qspi::{self, NOR_BYTES};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, led, stack, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
#[cfg(not(feature = "player-script"))]
const IMAGE: &str = "player";
#[cfg(feature = "player-script")]
const IMAGE: &str = "player-script";
/// Only the scripted image's `paced` lines are compared, so only it hashes
/// the ROM for its header, its frames and its DAC feed ([`Service::HASHES`];
/// the frame hash is 2.3% of a frame, the ROM hash about 30 ms per MiB).
const HASHES: bool = cfg!(feature = "player-script");

const ROM_NAME: &str = "qspi";

const LINK_BAUD: u32 = parse_baud(option_env!("GBC_LINK_BAUD"), DEFAULT_BAUD);
/// Statistics period: 300 frames, 5.02 s at 59.7275 Hz.
const STATS_FRAMES: u32 = 300;
/// A PC session is active while a link frame came within this many
/// frames (3 s; `gbc-pc play` repeats its `Input` every 500 ms).
const ACTIVE_FRAMES: u32 = 180;
/// Cartridge RAM: the MBC5 maximum, 128 KiB (Pan Docs "MBC5").
const CART_RAM_BYTES: usize = 128 * 1024;
/// The save buffer: 64 KiB of cartridge RAM plus the MBC3 footer, rounded
/// up. AXI SRAM has no room for a 128 KiB one beside the 128 KiB cartridge
/// RAM, the panel buffer (128 KiB-aligned) and the stash: a game with more
/// than 64 KiB of RAM runs, but its saves are refused (`Nak OutOfRange`)
/// and cannot be dumped (docs/reference/player.md, "Limits").
const SAVE_BYTES: usize = 64 * 1024 + 64;

/// The scripted run's capture (about 1.5 KiB of lines), the first part of
/// the save buffer (no link in scripted mode; AXI SRAM has no room for
/// both).
const CAPTURE_BYTES: usize = 8 * 1024;

/// The input program from `GBC_HEADLESS_INPUT` (scripted mode).
const INPUT: &str = include_str!(concat!(env!("OUT_DIR"), "/headless-input.txt"));
/// `GBC_HEADLESS_FRAMES`, or empty.
const FRAMES: &str = env!("GBC_HEADLESS_FRAMES");
const DEFAULT_FRAMES: u32 = 35_837;
const MAX_PRESSES: usize = 256;
/// Clock-table entries (16 bytes each, on the stack).
const MAX_CHANGES: usize = 256;
/// Live mode starts the run this many DAC refills before the end of the
/// start ramp (187 ms; `paced_board::start_dac`). Scripted mode uses
/// [`RAMP_LEFT`] (4) as the `paced` image, so its speed figure stays
/// comparable: a large ROM may then underrun at the start.
///
/// 24 was sized for the live run hashing a 2 MiB ROM before its first
/// frame (about 60 ms). Since K10 the live run no longer hashes the ROM
/// (`HASHES` off), so the ramp only has to cover `headless::start` and the
/// first frames filling the ring, whatever the ROM size. 24 is kept as
/// margin until a board run shows a smaller value has 0 underruns in the
/// first Stats window.
const LIVE_RAMP_LEFT: u32 = 24;

static RING: FeedRing = FeedRing::new();
static TAP: FeedTap = FeedTap::new();
/// The copy of the DAC feed for the PC (live mode; ADR 0041), 4 KiB of
/// AXI SRAM (DTCM has no room: it would come out of the stack region, and
/// AXI SRAM had 6 KiB left).
// SAFETY: `.axisram` is the NOLOAD AXI SRAM section memory.x defines for
// such buffers; not zeroed at boot, which is sound for the tee's integer
// atomics, and `main` calls `TEE.reset()` before the DAC starts. Only the
// CPU touches it (no DMA), so caching needs no maintenance.
#[unsafe(link_section = ".axisram.audio_tee")]
static TEE: AudioTee<DMA_HALF_FRAMES> = AudioTee::new();

/// The largest `Audio` frame: one DMA half of real frames, as 12-bit codes
/// (ADR 0044: [`U12`], 3 bytes a frame, keeps the UART's headroom).
const AUDIO_WIRE_MAX: usize =
    gbc_link::cobs::max_encoded_len(2 + 7 + DMA_HALF_FRAMES * U12_BYTES + 4) + 1;
const _: () = assert!(DMA_HALF_FRAMES <= gbc_link::audio::max_frames(U12));

/// The most bytes one transmit carries while audio streams: 15.4 ms at
/// 2 Mbit/s, so the transfer ends within a video frame and every
/// service call finds the transmitter idle (the tee holds 2 frames).
const TX_AUDIO_CAP: usize = 3072;

#[interrupt]
fn DMA1_STR0() {
    audio_dac::on_dma(|_, out| {
        let tee = |_p: &gbc_firmware::audio::Popped<'_>| {
            #[cfg(not(feature = "player-script"))]
            TEE.offer_popped(_p);
        };
        if HASHES {
            TAP.pop_to(&RING, out, tee)
        } else {
            TAP.pop_unhashed_to(&RING, out, tee)
        };
    });
}

#[interrupt]
fn EXTI15_10() {
    gbc_nucleo_h753::lcd_dma::on_te();
}

mod buffers {
    use super::*;

    pub struct Bufs {
        pub mem: &'static mut Memory,
        pub frame: &'static mut Frame,
        pub audio: &'static mut [StereoSample; MIN_AUDIO_SLOTS],
        pub cart_ram: &'static mut [u8; CART_RAM_BYTES],
        pub save: &'static mut [u8; SAVE_BYTES],
        pub stash: &'static mut Frame,
    }

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        const TWICE: &str = "buffers taken twice";
        Bufs {
            // `.data` (`Memory::new()` is not all zeros): DTCM in this
            // placement.
            mem: gbc_nucleo_h753::take_once!(Memory = Memory::new()).expect(TWICE),
            // `.bss`, DTCM (the M6-target placement for the frame, ADR
            // 0026; here because AXI SRAM is full): only the CPU touches
            // it (the scaler reads it into the panel buffer).
            frame: gbc_nucleo_h753::take_once!(Frame = [0; SCREEN_WIDTH * SCREEN_HEIGHT])
                .expect(TWICE),
            // SAFETY (the four `link_section`s): `.axisram` is the NOLOAD
            // AXI SRAM section memory.x defines for such buffers; not
            // zeroed at boot, which is sound for these integer types.
            // `headless::start` fills the cartridge RAM before it is read;
            // the audio buffer, the stash and the save buffer (in scripted
            // mode partly the capture) are written before they are read.
            audio: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.audio")]
                [StereoSample; MIN_AUDIO_SLOTS] =
                    [StereoSample { left: 0, right: 0 }; MIN_AUDIO_SLOTS]
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
            stash: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.stash")]
                Frame = [0; SCREEN_WIDTH * SCREEN_HEIGHT]
            )
            .expect(TWICE),
        }
    }
}

/// `WFI` as in the `paced` image (interrupts masked around the wait
/// check, M4 review P-4; then the held frame and the DAC watchdog), and
/// the cycles spent asleep added to `asleep` (the statistics' `busy`).
struct Wfi<'d> {
    dac: &'d mut AudioDac,
    display: &'d RefCell<Display>,
    asleep: &'d Cell<u32>,
}

impl Idle for Wfi<'_> {
    fn idle(&mut self, waiting: &dyn Fn() -> bool) {
        // Scripted mode: one step of a display readback sample while the
        // loop is ahead (src/display.rs, "Readback samples"); awake time.
        #[cfg(feature = "player-script")]
        if waiting() {
            self.display.borrow_mut().sample_step();
        }
        cortex_m::interrupt::disable();
        if waiting() {
            let t0 = cycles();
            cortex_m::asm::wfi();
            // The waking interrupt runs at `enable` below, after this
            // reading: its handler counts as awake.
            let slept = cycles().wrapping_sub(t0);
            self.asleep.set(self.asleep.get().wrapping_add(slept));
        }
        // SAFETY: re-enables what `disable` above masked; the loop calls
        // the idle hook with interrupts on, so no critical section is open.
        unsafe { cortex_m::interrupt::enable() };
        self.display.borrow_mut().service();
        self.dac.watchdog(audio_dac::STALL_CYCLES);
    }
}

/// The running emulator behind the session's [`Game`], for one service
/// call.
struct PlayerGame<'e, 'm> {
    emu: &'e mut Emulator<'m>,
    rom: (u32, u64),
    /// The footer time of the last save applied, stamped back on dumps
    /// (as link-probe: load then dump is byte-identical).
    unix_time: &'e mut u64,
    reloaded: bool,
}

impl Game for PlayerGame<'_, '_> {
    fn layout(&self) -> Option<Layout> {
        Layout::of_emulator(self.emu).ok()
    }
    fn rom(&self) -> Option<(u32, u64)> {
        Some(self.rom)
    }
    fn apply(&mut self, _bytes: &[u8], image: &Image<'_>) -> bool {
        let config = Config {
            dmg_palettes: COMPAT_PALETTE,
        };
        let ok = save::reload(self.emu, image, config).is_ok();
        if ok {
            *self.unix_time = image.unix_time();
            self.reloaded = true;
        }
        ok
    }
    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        save::take(self.emu, *self.unix_time, out).ok()
    }
}

/// No game (no ROM image): `Hello` is answered, the rest refused.
struct NoGame;

impl Game for NoGame {
    fn layout(&self) -> Option<Layout> {
        None
    }
    fn rom(&self) -> Option<(u32, u64)> {
        None
    }
    fn apply(&mut self, _: &[u8], _: &Image<'_>) -> bool {
        false
    }
    fn take(&mut self, _: &mut [u8]) -> Option<usize> {
        None
    }
}

/// A text line or a `Stats` extra built in memory, at most 1,023 bytes:
/// the last byte is kept for a text line's 0x00 ([`FmtBuf::reserving_nul`]).
type Text = FmtBuf<1024>;

/// An error's `Debug` text, short (truncated at 95 bytes).
type Why = FmtBuf<95>;

/// Feeds what the DMA received to the session and hands its output to the
/// transmit DMA (when idle), `extra`'s frames after the replies
/// ([`Session::fill_tx_with`]). True if anything was received.
fn serve(
    link: &mut LinkUart,
    session: &mut Session<'_>,
    game: &mut impl Game,
    shared: &Shared,
    extra: impl FnOnce(&mut [u8]) -> usize,
) -> bool {
    let mut got = false;
    link.read(|b| {
        got = true;
        session.feed(b, game, shared);
    });
    if let Some(buf) = link.tx_buf() {
        let cap = if session.audio_on() {
            TX_AUDIO_CAP
        } else {
            buf.len()
        };
        let n = session.fill_tx_with(&mut buf[..cap], extra);
        link.start_tx(n);
    }
    got
}

/// What goes out next once the transmitter is free.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pending {
    None,
    Text,
    Frame(usize),
}

/// The loop's service hook: link, saves, statistics.
struct Player<'a> {
    link: Option<LinkUart>,
    session: Session<'a>,
    shared: &'a Shared,
    rom: (u32, u64),
    unix_time: u64,
    live: bool,
    display: &'a RefCell<Display>,
    asleep: &'a Cell<u32>,
    budget: u32,
    /// The service call's start, for the next one's `busy`.
    last: Option<u32>,
    /// The last call's core cycles (`frame_done`).
    core: u32,
    window: Window,
    total: Window,
    frames_seen: u32,
    /// Frames since a link frame came (saturating).
    quiet: u32,
    since_stats: u32,
    stats_sent: u32,
    stats_dropped: u32,
    sessions: u32,
    reloads: u32,
    text: Text,
    wire: [u8; MAX_FRAME],
    pending: Pending,
    audio: AudioOut,
}

/// The audio stream's main-loop side (ADR 0041). Its buffer is a local of
/// [`AudioOut::fill`], so the scripted mode's stack (which never sends)
/// does not grow.
struct AudioOut {
    /// `Audio` messages written and their real frames.
    sent: u32,
    frames: u32,
    /// Cycles spent packing and encoding them.
    cycles: u64,
}

impl AudioOut {
    const fn new() -> Self {
        AudioOut {
            sent: 0,
            frames: 0,
            cycles: 0,
        }
    }

    /// Writes queued halves into `room` as whole `Audio` frames while one
    /// more surely fits; the bytes written.
    fn fill(&mut self, room: &mut [u8]) -> usize {
        let mut n = 0;
        let mut packed = [0u8; DMA_HALF_FRAMES * U12_BYTES];
        while room.len() - n >= AUDIO_WIRE_MAX {
            let t0 = cycles();
            let mut dst = packed.as_chunks_mut::<U12_BYTES>().0.iter_mut();
            let Some(h) = TEE.pop_with(|f| {
                if let Some(d) = dst.next() {
                    *d = encode_u12(f.left, f.right);
                }
            }) else {
                break;
            };
            let real = usize::from(h.real).min(DMA_HALF_FRAMES);
            let msg = Message::Audio {
                first: h.first,
                held: h.held,
                format: U12,
                data: &packed[..real * U12_BYTES],
            };
            if let Ok(k) = gbc_link::encode(&msg, 0, &mut room[n..]) {
                n += k;
                self.sent = self.sent.wrapping_add(1);
                self.frames = self.frames.wrapping_add(real as u32);
            }
            self.cycles += u64::from(cycles().wrapping_sub(t0));
        }
        n
    }
}

impl Player<'_> {
    /// The `Stats` extra / text line fields (`docs/reference/player.md`).
    fn fields(&self, out: &mut dyn Write, w: &Window) -> fmt::Result {
        write!(out, "image={IMAGE} ")?;
        w.write_fields(out, self.budget)?;
        let r = RING.stats();
        let d = self.display.borrow().counters();
        let a = audio_dac::stats();
        write!(
            out,
            " underruns={} underrun_frames={} overruns={} presented={} skipped={} \
             display_errors={} stuck={} dac_restarts={} total_frames={} total_over_budget={} \
             total_core_max={} total_busy_max={} reloads={} rom_len={} rom_hash={:016x}",
            r.underrun_events,
            r.underrun_frames,
            r.overrun_events,
            d.presented,
            d.skipped,
            d.errors,
            d.stuck,
            a.restarts,
            self.total.frames(),
            self.total.over_budget(),
            self.total.core_max(),
            self.total.busy_max(),
            self.reloads,
            self.rom.0,
            self.rom.1,
        )?;
        let c = self.session.counters();
        write!(
            out,
            " link_frames={} bad_frames={} hellos={} inputs={} saves_applied={} \
             saves_refused={} dumps={} dumps_done={} naks={} replies_dropped={} sessions={} \
             stats_sent={} stats_dropped={}",
            c.frames,
            c.bad_frames,
            c.hellos,
            c.inputs,
            c.saves_applied,
            c.saves_refused,
            c.dumps,
            c.dumps_done,
            c.naks,
            c.replies_dropped,
            self.sessions,
            self.stats_sent,
            self.stats_dropped,
        )?;
        if self.live {
            let t = TEE.stats();
            let a = &self.audio;
            write!(
                out,
                " audio_on={} audio_sent={} audio_frames={} audio_dropped={} audio_dropped_halves={} \
                 audio_timeouts={} audio_cyc_per_frame={} audio_isr_max={}",
                u8::from(TEE.is_on()),
                a.sent,
                a.frames,
                t.dropped_frames,
                t.dropped_halves,
                c.audio_timeouts,
                a.cycles / u64::from(a.frames.max(1)),
                audio_dac::stats().isr_max,
            )?;
        }
        // The deepest stack use since boot, the paint high-water mark (M5
        // review P-1: live mode's save and dump paths go deeper than the
        // scripted run).
        let (used, size) = stack::usage();
        write!(out, " stack_used={used}/{size}")?;
        if let Some(link) = &self.link {
            write!(
                out,
                " rx_high={} rx_overflow={} rx_dma_errors={} tx_errors={} baud={}",
                link.rx_high(),
                link.rx_overflow(),
                link.rx_dma_errors(),
                link.tx_errors(),
                link.setting().actual
            )?;
        }
        Ok(())
    }

    /// Builds this window's report (a `Stats` frame with an active PC
    /// session, else a text line) and starts a new window.
    fn queue_stats(&mut self) {
        if self.pending != Pending::None {
            self.stats_dropped += 1;
        }
        let mut t = Text::reserving_nul();
        let active = self.quiet < ACTIVE_FRAMES;
        if !active {
            let _ = write!(t, "{PREFIX} {IMAGE} stats ");
        }
        let _ = self.fields(&mut t, &self.window);
        self.pending = if active {
            let msg = Message::Stats {
                frames: self.window.frames(),
                cycles_mean: self.window.core_mean(),
                cycles_max: self.window.core_max(),
                extra: t.as_bytes(),
            };
            match gbc_link::encode(&msg, 0, &mut self.wire) {
                Ok(n) => Pending::Frame(n),
                Err(_) => Pending::None,
            }
        } else {
            let _ = t.write_str("\r\n");
            let _ = t.push_nul();
            self.text = t;
            Pending::Text
        };
        self.window.reset();
        self.since_stats = 0;
        if self.live {
            // `audio_isr_max` per window, so on and off can be compared.
            audio_dac::reset_isr_cost();
        }
    }

    fn link_service(&mut self, emu: &mut Emulator<'_>) -> Serviced {
        let Some(link) = self.link.as_mut() else {
            return Serviced::Continue;
        };
        let mut game = PlayerGame {
            emu,
            rom: self.rom,
            unix_time: &mut self.unix_time,
            reloaded: false,
        };
        if self.shared.take_dump_request() {
            self.session.board_dump(&mut game);
        }
        let (pending, wire, stats_sent, audio) = (
            &mut self.pending,
            &self.wire,
            &mut self.stats_sent,
            &mut self.audio,
        );
        let got = serve(link, &mut self.session, &mut game, self.shared, |room| {
            // The pending `Stats` frame first (else a stream that keeps
            // the transmitter busy would starve it), then audio.
            let mut n = 0;
            if let Pending::Frame(k) = *pending
                && room.len() >= k
            {
                room[..k].copy_from_slice(&wire[..k]);
                n = k;
                *pending = Pending::None;
                *stats_sent += 1;
            }
            n + audio.fill(&mut room[n..])
        });
        self.session.end_frame(self.shared);
        let on = self.session.audio_on();
        if on != TEE.is_on() {
            TEE.set_on(on);
        }
        if !on {
            TEE.clear();
        }
        let reloaded = game.reloaded;
        if got {
            if self.quiet >= ACTIVE_FRAMES {
                // A session starts: report right away.
                self.sessions += 1;
                self.quiet = 0;
                self.since_stats = STATS_FRAMES;
            }
            self.quiet = 0;
        } else {
            self.quiet = self.quiet.saturating_add(1);
        }
        if reloaded {
            self.reloads += 1;
            Serviced::Reloaded
        } else {
            Serviced::Continue
        }
    }

    /// Sends the pending report once the session and the transmitter are
    /// idle (never inside a reply or a dump).
    fn send_pending(&mut self) {
        let Some(link) = self.link.as_mut() else {
            return;
        };
        if self.pending == Pending::None || self.session.busy() || !link.tx_idle() {
            return;
        }
        let sent = match self.pending {
            Pending::Text => link.send(self.text.as_bytes()),
            Pending::Frame(n) => link.send(&self.wire[..n]),
            Pending::None => true,
        };
        if sent {
            self.pending = Pending::None;
            self.stats_sent += 1;
        }
    }
}

impl Service for Player<'_> {
    fn service(&mut self, n: u32, emu: &mut Emulator<'_>) -> Serviced {
        let now = cycles();
        let slept = self.asleep.replace(0);
        if let Some(last) = self.last.replace(now) {
            let busy = now.wrapping_sub(last).saturating_sub(slept);
            self.window.add(self.core, busy, self.budget);
            self.total.add(self.core, busy, self.budget);
            self.frames_seen = self.frames_seen.wrapping_add(1);
            self.since_stats = self.since_stats.saturating_add(1);
        }
        if n == 0 && self.live {
            emu.set_dmg_palettes(COMPAT_PALETTE);
        }
        let r = self.link_service(emu);
        if self.link.is_some() && self.since_stats >= STATS_FRAMES {
            self.queue_stats();
        }
        self.send_pending();
        if n.is_multiple_of(30) {
            led::toggle();
        }
        r
    }

    fn frame_done(&mut self, _: u32, c: FrameCycles) {
        self.core = c.core;
    }

    const HASHES: bool = HASHES;
}

/// Discards the run's lines (live mode: the run never ends).
struct Discard;

impl Write for Discard {
    fn write_str(&mut self, _: &str) -> fmt::Result {
        Ok(())
    }
}

/// No valid ROM image: a dark red panel, the reason on the UART every
/// 2 s, and the link session with no game (module docs). Never returns.
fn no_rom(
    board: &mut board::Board,
    save: &'static mut [u8; SAVE_BYTES],
    stage: &str,
    why: &Why,
) -> ! {
    display::status_screen(0x8000);
    let why = core::str::from_utf8(why.as_bytes()).unwrap_or("?");
    let mut line: Text = Text::reserving_nul();
    let _ = write!(
        line,
        "{PREFIX} {IMAGE} no-rom stage={stage} error={why}: no valid ROM image in the QSPI NOR; \
         load one with the loader (scripts/play-on-board.sh <rom>)\r\n"
    );
    let _ = board
        .uart
        .write_str(core::str::from_utf8(line.as_bytes()).unwrap_or(""));
    let _ = line.push_nul();
    let mut link = match LinkUart::new(&mut board.uart, LINK_BAUD) {
        Ok(l) => l,
        Err(e) => panic!("link baud {LINK_BAUD} refused: {e:?}"),
    };
    let shared = Shared::new();
    let mut session = Session::new(&mut save[..], NOR_BYTES);
    let mut t_text = cycles().wrapping_sub(2 * CPU_HZ);
    let mut t_quiet = cycles();
    loop {
        if serve(&mut link, &mut session, &mut NoGame, &shared, |_| 0) {
            t_quiet = cycles();
        }
        let now = cycles();
        if now.wrapping_sub(t_text) >= 2 * CPU_HZ
            && now.wrapping_sub(t_quiet) >= CPU_HZ
            && !session.busy()
            && link.tx_idle()
        {
            link.send(line.as_bytes());
            led::toggle();
            t_text = now;
        }
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    board.banner(IMAGE);
    TEE.reset();
    let b = buffers::take();

    // Mapped for good: `main` never returns, so the ROM slice stays valid.
    let image = match qspi::map_rom_image() {
        Ok(i) => i,
        Err(e) => {
            let mut why = Why::new();
            let _ = write!(why, "{:?}", e.error());
            no_rom(&mut board, b.save, e.stage(), &why)
        }
    };
    let (rom, header) = (image.rom(), image.header);
    let title = &header.title[..header.title.iter().position(|&c| c == 0).unwrap_or(16)];
    let _ = writeln!(
        board.uart,
        "{PREFIX} {IMAGE} rom rom_len={} rom_hash={:016x} title={} qspi_hz={} link_baud={LINK_BAUD} \
         stats_frames={STATS_FRAMES} budget={}\r",
        header.len,
        header.hash,
        core::str::from_utf8(title).unwrap_or("?"),
        image.mapped.clock_hz(),
        frame_budget(CPU_HZ),
    );

    let bufs = Buffers {
        mem: b.mem,
        save_ram: &mut b.cart_ram[..],
        frame: b.frame,
        audio: &mut b.audio[..],
    };
    let display = RefCell::new(display::init(b.stash));
    let mut dac = AudioDac::new();
    set_sleep_clock(true);
    let asleep = Cell::new(0u32);
    let shared = Shared::new();
    let live = !cfg!(feature = "player-script");
    let link = if live {
        match LinkUart::new(&mut board.uart, LINK_BAUD) {
            Ok(l) => Some(l),
            Err(e) => panic!("link baud {LINK_BAUD} refused: {e:?}"),
        }
    } else {
        None
    };
    let (session_buf, capture): (&'static mut [u8], _) = if live {
        (&mut b.save[..], None)
    } else {
        let (c, rest) = b
            .save
            .split_first_chunk_mut::<CAPTURE_BYTES>()
            .expect("capture fits");
        (rest, Some(c))
    };
    let mut player = Player {
        link,
        session: Session::new(session_buf, NOR_BYTES),

        shared: &shared,
        rom: (header.len, header.hash),
        unix_time: 0,
        live,
        display: &display,
        asleep: &asleep,
        budget: frame_budget(CPU_HZ),
        last: None,
        core: 0,
        window: Window::new(),
        total: Window::new(),
        frames_seen: 0,
        quiet: ACTIVE_FRAMES,
        since_stats: 0,
        stats_sent: 0,
        stats_dropped: 0,
        sessions: 0,
        reloads: 0,
        text: Text::reserving_nul(),
        wire: [0; MAX_FRAME],
        pending: Pending::None,
        audio: AudioOut::new(),
    };
    player.session.set_audio_capable(live);
    let clock = Dwt::new();
    let feed = Feed {
        ring: &RING,
        tap: &TAP,
    };

    if live {
        let mut input = LiveInput::<_, FRAME_SAMPLES>::new(Joystick::new(), &shared);
        // The ring must be full before the ramp ends. The live run skips
        // the ROM hash (`HASHES` off; with it, a 2 MiB ROM cost about
        // 60 ms, measured as 5 underruns at the start with RAMP_LEFT, and
        // an 8 MiB one would cost about 240 ms, past the 187 ms ramp).
        let _ = start_dac(&mut dac, LIVE_RAMP_LEFT);
        let _ = paced::run_with(
            rom,
            &mut input,
            u32::MAX,
            bufs,
            feed,
            Hooks {
                clock: &clock,
                sink: &mut display::SinkRef(&display),
                idle: &mut Wfi {
                    dac: &mut dac,
                    display: &display,
                    asleep: &asleep,
                },
            },
            &mut player,
            &mut Discard,
        );
        // 2.3 years of frames, or the core refused the ROM.
        panic!("player run ended");
    }

    // Scripted mode (module docs).
    let mut presses = [Press::NONE; MAX_PRESSES];
    let mut changes = [Change::NONE; MAX_CHANGES];
    let clocked = replay::is_clock_table(INPUT);
    let (count, end) = if clocked {
        match replay::parse(INPUT, &mut changes) {
            Ok(p) => (p.changes, p.end),
            Err(e) => panic!("clock input error line={} kind={:?}", e.line, e.kind),
        }
    } else {
        match input::parse(INPUT, &mut presses) {
            Ok(p) => (p.presses, p.end),
            Err(e) => panic!("input error line={} kind={:?}", e.line, e.kind),
        }
    };
    let frames = FRAMES.parse().ok().or(end).unwrap_or(DEFAULT_FRAMES);
    let _ = writeln!(
        board.uart,
        "{PREFIX} {IMAGE} running rom_bytes={} {}={count} frames={frames}\r",
        rom.len(),
        if clocked { "clock_changes" } else { "presses" },
    );
    let mut frame_script = Script::new(&presses[..count.min(MAX_PRESSES)]);
    let mut clock_script = ClockScript::new(&changes[..count.min(MAX_CHANGES)]);
    let source: &mut dyn InputSource = if clocked {
        &mut clock_script
    } else {
        &mut frame_script
    };
    let mut cap: Capture<'_, CAPTURE_BYTES> = Capture {
        buf: capture.expect("scripted mode"),
        len: 0,
        overflow: false,
        live: Some(&mut board.uart),
    };
    let _ = start_dac(&mut dac, RAMP_LEFT);
    let _ = paced::run_with(
        rom,
        source,
        frames,
        bufs,
        feed,
        Hooks {
            clock: &clock,
            sink: &mut display::SinkRef(&display),
            idle: &mut Wfi {
                dac: &mut dac,
                display: &display,
                asleep: &asleep,
            },
        },
        &mut player,
        &mut cap,
    );
    if clocked {
        let _ = clock_script.report(&mut cap);
    }
    let stop = dac.stop();
    cap.live = None;
    let _ = display.borrow_mut().report(&mut cap);
    let _ = dac_line(&mut cap, stop.is_ok());
    let _ = write!(cap, "stats player ");
    let _ = player.fields(&mut cap, &player.total);
    let _ = writeln!(cap);
    let (buf, len, overflow) = (cap.buf, cap.len, cap.overflow);
    let captured = &buf[..len];
    let mut n = 0u32;
    loop {
        report(
            &mut board, PREFIX, IMAGE, ROM_NAME, n, frames, captured, overflow,
        );
        led::toggle();
        n = n.wrapping_add(1);
        wait_since(cycles(), CPU_HZ);
    }
}
