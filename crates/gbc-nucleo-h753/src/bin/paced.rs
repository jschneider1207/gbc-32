//! The `paced` image (plan Stage 2 M4 acceptance): one ROM from internal
//! flash run by the audio-paced main loop ([`gbc_firmware::paced`]) with
//! the display and the DAC on, in the M6-target placement (the ITCM set
//! and the data in DTCM, as the `headless` images; build.rs). Its
//! `paced ` lines must equal the host's (`tools/headless --paced`) byte
//! for byte; `run-paced.sh` compares them and checks the report-only
//! figures.
//!
//! # Wiring (the `paced` module docs, "Board use")
//!
//! - `RING: FeedRing` and `TAP: FeedTap` are statics; the DMA1 stream 0
//!   ISR is `audio_dac::on_dma(|_, out| TAP.pop_to(&RING, out, |_| {}))`.
//! - [`Idle`] is `WFI`, with DBGMCU_CR DBGSLEEP_D1 set first, so the CPU
//!   clock and with it the cycle counter keep running in Sleep (the image
//!   measures that: "Sleep and the cycle counter" below). The speed
//!   figures are against that counter. The loop's wait condition is
//!   re-checked with interrupts masked right before `WFI` (`cpsid i`,
//!   check, `wfi`, `cpsie i`; M4 review P-4), so a refill between the
//!   loop's check and the `WFI` cannot leave it asleep for another half.
//! - The display sink ([`display::Display`], src/display.rs in the lib) shows the newest completed
//!   frame at each TE (M4 review P-7; ADR 0039 amendment), never waiting
//!   for the panel. On each frame: no transfer queued, or the last one
//!   finished: `scale_frame` and `present` (the transfer starts on TE).
//!   Still pending (TE has not come): taken back (`retract`), and this
//!   frame converted and queued instead (`replaced`, counted as skipped).
//!   Running (MDMA reading the buffer): the frame is copied aside
//!   (`copy_*`) and shown by the idle hook as soon as the transfer is done
//!   (`deferred`), unless a newer frame comes first (the older then
//!   counts as skipped). A transfer still unfinished [`display::STUCK`]
//!   cycles after it was queued is abandoned and counted as an error. No
//!   readback.
//! - The DAC starts before `paced::run`, which begins four refills before
//!   the end of the DAC's 250 ms start ramp (`paced_board::RAMP_LEFT`; `ramp_wait`
//!   below is that wait), so the speed figures do not count the ramp as
//!   real time without consumption. The DAC stops after the run returns
//!   (the run drains the ring with the DAC playing).
//!
//! # Output
//!
//! The run's lines go to the UART as they are written (the `paced v1`
//! line before the first frame, `paced end` after the last, then `drain`
//! and the `stats paced` lines) and into a capture buffer, followed by
//! the board's own report-only lines:
//!
//! ```text
//! stats display paced presented=<n> skipped=<n> transfers=<n> transfer_errors=<n> stuck=<n> aborts=<n> te_edges=<n> scale_mean=<c> scale_max=<c> present_mean=<c> present_max=<c> replaced=<n> deferred=<n> copy_mean=<c> copy_max=<c> [first_error=frame<n>:<e>]
//! stats dac refills=<n> late=<n> spurious=<n> te=<n> dme=<n> fe=<n> udr1=<n> udr2=<n> restarts=<n> isr_min=<c> isr_max=<c> stop=<ok|timeout>
//! stats paced board run_cycles=<c> run_refills=<n> dac_rate_millihz=<r> ramp_wait=<c> stack_used=<b>/<b> dbgmcu_cr_boot=<hex> sleep_gate_te=<c|untested> sleep_keep_te=<c|untested>
//! ```
//!
//! `presented` frames went to the panel, `skipped` were superseded by a
//! newer frame before TE took them (`replaced` of them were queued and
//! taken back; the rest waited aside while a transfer ran), `deferred`
//! were shown from the copy; `transfers` finished without error
//! (counted when the next frame finds them done; the last one is waited
//! for after the run). `run_refills` and `dac_rate_millihz` are the DAC
//! interrupts from right before `paced::run` until it returned, and the
//! sample rate they imply against the cycle counter over the same span
//! (256 frames per refill): 32,768,675 if the counter ran throughout,
//! far above if it stopped in `WFI`.
//!
//! After the run the image re-sends the whole report every
//! [`REPORT_GAP`] cycles, framed as the `headless` image's:
//!
//! ```text
//! <banner>
//! gbc-nucleo-h753 paced report n=<k> rom=<name> frames=<N> image_bytes=<n> image_cksum=<crc>
//! <the captured lines>
//! gbc-nucleo-h753 paced report-end n=<k> rom=<name> bytes=<len> cksum=<crc> overflow=<0|1>
//! ```
//!
//! # Sleep and the cycle counter
//!
//! Before the run, with the panel's TE interrupt (60 Hz) as the only
//! wake-up, the image sleeps in `WFI` for 30 TE periods twice: with
//! DBGSLEEP_D1 clear (RM0433 DBGMCU_CR: the CPU clock is stopped in Sleep)
//! and set (kept running). `sleep_gate_te` and `sleep_keep_te` are the
//! mean TE period in cycle-counter counts each time: if the counter stops
//! with the CPU clock, the first is far below the second (about 7.9 M).
//! `untested` if TE did not come within 100 ms.
#![no_std]
#![no_main]

use core::cell::RefCell;
use core::fmt::Write;

use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::audio::{FeedRing, FeedTap};
use gbc_firmware::headless::{Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};
use gbc_firmware::paced::{self, Feed, Hooks, Idle};
use gbc_nucleo_h753::audio_dac::{self, AudioDac};
use gbc_nucleo_h753::display;
use gbc_nucleo_h753::pac::interrupt;
use gbc_nucleo_h753::paced_board::{
    Capture, Dwt, RAMP_LEFT, dac_line, dbgmcu_cr, report, set_sleep_clock, start_dac,
};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, lcd_dma, led, stack, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "paced";

/// The ROM, embedded by build.rs from `GBC_HEADLESS_ROM` (in `.rodata`,
/// internal flash).
static ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/headless.gb"));
/// The ROM's file stem, for the report lines.
const ROM_NAME: &str = env!("GBC_HEADLESS_ROM_NAME");
/// The input program from `GBC_HEADLESS_INPUT` (empty: no input).
const INPUT: &str = include_str!(concat!(env!("OUT_DIR"), "/headless-input.txt"));
/// `GBC_HEADLESS_FRAMES`, or empty.
const FRAMES: &str = env!("GBC_HEADLESS_FRAMES");
/// Frames when neither `GBC_HEADLESS_FRAMES` nor an `end` line gives them:
/// 10 minutes at 59.7275 Hz (the M4 acceptance run).
const DEFAULT_FRAMES: u32 = 35_837;
/// The most presses an input program may have (12 B each, on the stack).
const MAX_PRESSES: usize = 256;
/// Cartridge RAM: the MBC5 maximum, 128 KiB (Pan Docs "MBC5").
const CART_RAM_BYTES: usize = 128 * 1024;
/// The capture: about 1.5 KiB of lines are expected.
const CAPTURE_BYTES: usize = 8 * 1024;
/// The pause after each report (1 s at [`CPU_HZ`]).
const REPORT_GAP: u32 = CPU_HZ;

/// The sample ring between the loop and the DAC's ISR, and the tap that
/// hashes what the ISR hands the DAC (`.bss`/`.data`, DTCM: CPU only).
static RING: FeedRing = FeedRing::new();
static TAP: FeedTap = FeedTap::new();

#[interrupt]
fn DMA1_STR0() {
    audio_dac::on_dma(|_, out| {
        TAP.pop_to(&RING, out, |_| {});
    });
}

#[interrupt]
fn EXTI15_10() {
    lcd_dma::on_te();
}

/// The statics lent to gbc-firmware.
mod buffers {
    use super::*;

    pub struct Bufs {
        pub mem: &'static mut Memory,
        pub frame: &'static mut Frame,
        pub audio: &'static mut [StereoSample; MIN_AUDIO_SLOTS],
        pub cart_ram: &'static mut [u8; CART_RAM_BYTES],
        pub capture: &'static mut [u8; CAPTURE_BYTES],
        pub stash: &'static mut Frame,
    }

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        const TWICE: &str = "buffers taken twice";
        Bufs {
            // `.data` (`Memory::new()` is not all zeros): in DTCM in this
            // placement, copied from flash by the startup code.
            mem: gbc_nucleo_h753::take_once!(Memory = Memory::new()).expect(TWICE),
            // SAFETY (the five `link_section`s): `.axisram` is the NOLOAD
            // output section of AXI SRAM that memory.x
            // (memory_x::firmware_memory_x) defines for such buffers. It
            // is not zeroed at boot, which is sound for these types
            // (integers and structs of integers: every bit pattern is a
            // value); `paced::run` (through `headless::start`) fills the
            // frame and cartridge RAM before it reads them, the audio
            // buffer is written before it is read, the capture is read
            // only up to what was written, and the display's copy of a
            // frame (the stash) is written whole before it is read.
            frame: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.frame")]
                Frame = [0; SCREEN_WIDTH * SCREEN_HEIGHT]
            )
            .expect(TWICE),
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
            capture: gbc_nucleo_h753::take_once!(
                #[unsafe(link_section = ".axisram.capture")]
                [u8; CAPTURE_BYTES] = [0; CAPTURE_BYTES]
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

/// `WFI`: the DAC refill interrupt (128 per second) or TE (60 per second)
/// wakes it. The wait condition is checked with interrupts masked right
/// before it (M4 review P-4): an interrupt that comes after the check is
/// pending at the `WFI`, which then does not sleep (Armv7-M B1.5.19: WFI
/// wakes on a pending interrupt even with PRIMASK set), and runs at
/// `cpsie i`. Then the display's held frame (`Display::service`), and the
/// DAC's stall check (M4 review P-2): after an underrun or a stream error
/// the refills stop, and the loop would wait for ring space forever; TE
/// still wakes it, and the watchdog restarts the DAC path (counted:
/// `restarts=` in `stats dac`).
struct Wfi<'d> {
    dac: &'d mut AudioDac,
    display: &'d RefCell<display::Display>,
}

impl Idle for Wfi<'_> {
    fn idle(&mut self, waiting: &dyn Fn() -> bool) {
        cortex_m::interrupt::disable();
        if waiting() {
            cortex_m::asm::wfi();
        }
        // SAFETY: this re-enables what `disable` above masked; no critical
        // section is open around the idle hook (the loop calls it with
        // interrupts on), so none is broken.
        unsafe { cortex_m::interrupt::enable() };
        self.display.borrow_mut().service();
        self.dac.watchdog(audio_dac::STALL_CYCLES);
    }
}

/// The mean TE period in cycle-counter counts over 30 periods slept in
/// `WFI` (module docs, "Sleep and the cycle counter"); `None` if TE does
/// not come within 100 ms (checked by spinning first, so `WFI` cannot wait
/// forever for it).
fn sleep_te_cycles(keep: bool) -> Option<u32> {
    set_sleep_clock(keep);
    lcd_dma::te_reset();
    let t0 = cycles();
    while lcd_dma::te_stats().edges < 2 {
        if cycles().wrapping_sub(t0) > CPU_HZ / 10 {
            return None;
        }
    }
    lcd_dma::te_reset();
    while lcd_dma::te_stats().edges < 31 {
        cortex_m::asm::wfi();
    }
    Some(lcd_dma::te_stats().mean)
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let _ = writeln!(
        board.uart,
        "\r\n{PREFIX} {IMAGE} rom={ROM_NAME} measuring the clock against the LSE\r"
    );
    board.measure_clock();
    board.banner(IMAGE);

    let mut presses = [Press::NONE; MAX_PRESSES];
    let (count, end) = match input::parse(INPUT, &mut presses) {
        Ok(p) => (p.presses, p.end),
        Err(e) => loop {
            // A bad program is a build input error: say so and stop.
            let _ = writeln!(
                board.uart,
                "{PREFIX} paced input error line={} kind={:?}\r",
                e.line, e.kind
            );
            let t0 = cycles();
            wait_since(t0, CPU_HZ);
            led::toggle();
        },
    };
    let frames = FRAMES.parse().ok().or(end).unwrap_or(DEFAULT_FRAMES);

    let b = buffers::take();
    let bufs = Buffers {
        mem: b.mem,
        save_ram: &mut b.cart_ram[..],
        frame: b.frame,
        audio: &mut b.audio[..],
    };
    let display = RefCell::new(display::init(b.stash));
    let mut dac = AudioDac::new();

    let dbgmcu_cr_boot = dbgmcu_cr();
    let gate = sleep_te_cycles(false);
    let keep = sleep_te_cycles(true);
    // The run sleeps with the clock kept (module docs).
    set_sleep_clock(true);
    let _ = writeln!(
        board.uart,
        "{PREFIX} paced running rom={ROM_NAME} rom_bytes={} presses={count} frames={frames}\r",
        ROM.len()
    );

    let mut cap: Capture<'_, CAPTURE_BYTES> = Capture {
        buf: b.capture,
        len: 0,
        overflow: false,
        live: Some(&mut board.uart),
    };
    let clock = Dwt::new();
    // The start ramp (250 ms) first, up to `RAMP_LEFT` refills before
    // its end: started right before the run, the loop would fill the ring
    // and then idle through the ramp, which is 250 ms of real time with
    // no samples consumed in the speed figures (-417 ppm over 10 minutes,
    // -1.2% over 20 s: measured -8,630 ppm on a 1,200-frame run).
    let ramp_wait = start_dac(&mut dac, RAMP_LEFT);
    let r0 = audio_dac::stats().refills;
    let c0 = clock.total();
    // The result is in the captured lines (`paced error ...`, or the lines
    // stopping short); the host compares those.
    let _ = paced::run(
        ROM,
        Script::new(&presses[..count]),
        frames,
        bufs,
        Feed {
            ring: &RING,
            tap: &TAP,
        },
        Hooks {
            clock: &clock,
            sink: &mut display::SinkRef(&display),
            idle: &mut Wfi {
                dac: &mut dac,
                display: &display,
            },
        },
        &mut cap,
    );
    let run_refills = audio_dac::stats().refills.wrapping_sub(r0);
    let run_cycles = clock.total() - c0;
    let stop = dac.stop();
    cap.live = None;
    let _ = display.borrow_mut().report(&mut cap);
    let _ = dac_line(&mut cap, stop.is_ok());
    // Frames per second times 1000, from the refills over the run.
    let rate = (u128::from(run_refills) * audio_dac::HALF as u128 * u128::from(CPU_HZ) * 1000)
        .checked_div(u128::from(run_cycles))
        .unwrap_or(0);
    let (used, size) = stack::usage();
    let _ = write!(
        cap,
        "stats paced board run_cycles={run_cycles} run_refills={run_refills} \
         dac_rate_millihz={rate} ramp_wait={ramp_wait} stack_used={used}/{size} dbgmcu_cr_boot={dbgmcu_cr_boot:#010x}"
    );
    for (name, v) in [("sleep_gate_te", gate), ("sleep_keep_te", keep)] {
        let _ = match v {
            Some(c) => write!(cap, " {name}={c}"),
            None => write!(cap, " {name}=untested"),
        };
    }
    let _ = writeln!(cap);

    // The capture outlives the UART borrow it held for the live lines.
    let (buf, len, overflow) = (cap.buf, cap.len, cap.overflow);
    let captured = &buf[..len];
    let mut n = 0u32;
    loop {
        report(
            &mut board, PREFIX, IMAGE, ROM_NAME, n, frames, captured, overflow,
        );
        led::toggle();
        n = n.wrapping_add(1);
        wait_since(cycles(), REPORT_GAP);
    }
}
