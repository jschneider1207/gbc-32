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
//!   ISR is `audio_dac::on_dma(|_, out| TAP.pop_into(&RING, out))`.
//! - [`Idle`] is `WFI`, with DBGMCU_CR DBGSLEEP_D1 set first, so the CPU
//!   clock and with it the cycle counter keep running in Sleep (the image
//!   measures that: "Sleep and the cycle counter" below). The speed
//!   figures are against that counter.
//! - The display sink ([`display::Display`]): on each completed frame, if
//!   the previous transfer has finished, `scale_frame` and `present` (the
//!   transfer starts on TE, ADR 0039); otherwise the frame is skipped and
//!   counted. It never waits for the panel; a transfer still unfinished
//!   [`display::STUCK`] cycles after it was queued is abandoned and counted
//!   as an error. No readback.
//! - The DAC starts before `paced::run`, which begins four refills before
//!   the end of the DAC's 250 ms start ramp ([`RAMP_LEFT`]; `ramp_wait`
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
//! stats display paced presented=<n> skipped=<n> transfers=<n> transfer_errors=<n> stuck=<n> aborts=<n> te_edges=<n> scale_mean=<c> scale_max=<c> present_mean=<c> present_max=<c> [first_error=frame<n>:<e>]
//! stats dac refills=<n> late=<n> spurious=<n> te=<n> dme=<n> fe=<n> udr1=<n> udr2=<n> restarts=<n> isr_min=<c> isr_max=<c> stop=<ok|timeout>
//! stats paced board run_cycles=<c> run_refills=<n> dac_rate_millihz=<r> ramp_wait=<c> stack_used=<b>/<b> dbgmcu_cr_boot=<hex> sleep_gate_te=<c|untested> sleep_keep_te=<c|untested>
//! ```
//!
//! `presented` frames went to the panel, `skipped` found the previous
//! transfer still pending or running; `transfers` finished without error
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

use core::cell::{Cell, UnsafeCell};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::CycleClock;
use gbc_firmware::audio::{FeedRing, FeedTap};
use gbc_firmware::headless::{Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};
use gbc_firmware::paced::{self, Feed, Hooks, Idle};
use gbc_nucleo_h753::audio_dac::{self, AudioDac};
use gbc_nucleo_h753::pac::{self, interrupt};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, image, lcd_dma, led, stack, wait_since};

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
        TAP.pop_into(&RING, out);
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
    }

    struct Slot<T>(UnsafeCell<T>);

    // SAFETY: the slots are private to this module and only reachable
    // through `take`, which hands out one reference to each for the life
    // of the program (guarded by `TAKEN`), so no two contexts ever access
    // one.
    unsafe impl<T: Send> Sync for Slot<T> {}

    /// `.data` (`Memory::new()` is not all zeros): in DTCM in this
    /// placement, copied from flash by the startup code.
    static MEM: Slot<Memory> = Slot(UnsafeCell::new(Memory::new()));
    // SAFETY (the four `link_section`s): `.axisram` is the NOLOAD output
    // section of AXI SRAM that memory.x (memory_x::firmware_memory_x)
    // defines for such buffers. It is not zeroed at boot, which is sound
    // for these types (integers and structs of integers: every bit pattern
    // is a value); `paced::run` (through `headless::start`) fills the frame
    // and cartridge RAM before it reads them, the audio buffer is written
    // before it is read, and the capture is read only up to what was
    // written.
    #[unsafe(link_section = ".axisram.frame")]
    static FRAME: Slot<Frame> = Slot(UnsafeCell::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]));
    #[unsafe(link_section = ".axisram.audio")]
    static AUDIO: Slot<[StereoSample; MIN_AUDIO_SLOTS]> = Slot(UnsafeCell::new(
        [StereoSample { left: 0, right: 0 }; MIN_AUDIO_SLOTS],
    ));
    #[unsafe(link_section = ".axisram.cart_ram")]
    static CART_RAM: Slot<[u8; CART_RAM_BYTES]> = Slot(UnsafeCell::new([0; CART_RAM_BYTES]));
    #[unsafe(link_section = ".axisram.capture")]
    static CAPTURE: Slot<[u8; CAPTURE_BYTES]> = Slot(UnsafeCell::new([0; CAPTURE_BYTES]));

    static TAKEN: AtomicBool = AtomicBool::new(false);

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        assert!(!TAKEN.swap(true, Ordering::AcqRel), "buffers taken twice");
        // SAFETY: the swap above lets exactly one caller through, ever, so
        // these are the only references to the five statics.
        unsafe {
            Bufs {
                mem: &mut *MEM.0.get(),
                frame: &mut *FRAME.0.get(),
                audio: &mut *AUDIO.0.get(),
                cart_ram: &mut *CART_RAM.0.get(),
                capture: &mut *CAPTURE.0.get(),
            }
        }
    }
}

/// The DWT cycle counter as a [`CycleClock`], extended to 64 bits on every
/// reading ([`Dwt::total`]; readings must be less than a wrap, 9.1 s,
/// apart: `paced::run` reads it at least once per call and per idle).
struct Dwt {
    last: Cell<u32>,
    total: Cell<u64>,
}

impl Dwt {
    fn new() -> Self {
        Dwt {
            last: Cell::new(cycles()),
            total: Cell::new(0),
        }
    }

    /// Cycles since [`Dwt::new`], as of this call.
    fn total(&self) -> u64 {
        self.now();
        self.total.get()
    }
}

impl CycleClock for Dwt {
    fn now(&self) -> u32 {
        let c = cycles();
        let d = c.wrapping_sub(self.last.replace(c));
        self.total.set(self.total.get() + u64::from(d));
        c
    }

    fn hz(&self) -> u32 {
        CPU_HZ
    }
}

/// `WFI`: the DAC refill interrupt (128 per second) or TE (60 per second)
/// wakes it. Then the DAC's stall check (M4 review P-2): after an
/// underrun or a stream error the refills stop, and the loop would wait
/// for ring space forever; TE still wakes it, and the watchdog restarts
/// the DAC path (counted: `restarts=` in `stats dac`).
struct Wfi<'d> {
    dac: &'d mut AudioDac,
}

impl Idle for Wfi<'_> {
    fn idle(&mut self) {
        cortex_m::asm::wfi();
        self.dac.watchdog(audio_dac::STALL_CYCLES);
    }
}

/// The run's lines, kept for the reports, and also sent to the UART as
/// they are written (`live`). Writing past the end fails (and so ends the
/// run with a write error) and sets `overflow`.
struct Capture<'u> {
    buf: &'static mut [u8; CAPTURE_BYTES],
    len: usize,
    overflow: bool,
    live: Option<&'u mut board::Uart>,
}

impl Write for Capture<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(u) = self.live.as_deref_mut() {
            // `\n` line ends become `\r\n` on the wire, as every line of
            // the board; the capture keeps `\n`.
            for (i, part) in s.split('\n').enumerate() {
                if i > 0 {
                    let _ = u.write_str("\r\n");
                }
                let _ = u.write_str(part);
            }
        }
        let Some(dst) = self.buf.get_mut(self.len..self.len + s.len()) else {
            self.overflow = true;
            return Err(fmt::Error);
        };
        dst.copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}

fn report(board: &mut board::Board, n: u32, frames: u32, cap: &[u8], overflow: bool) {
    board.banner(IMAGE);
    let (image_bytes, image_cksum) = board.image();
    let out = &mut board.uart;
    // The UART writer never fails.
    let _ = writeln!(
        out,
        "{PREFIX} paced report n={n} rom={ROM_NAME} frames={frames} \
         image_bytes={image_bytes} image_cksum={image_cksum}\r"
    );
    for line in cap.split_inclusive(|&b| b == b'\n') {
        let (body, nl) = match line.split_last() {
            Some((b'\n', body)) => (body, true),
            _ => (line, false),
        };
        // The capture holds only what `fmt::Write` wrote: UTF-8.
        let _ = out.write_str(core::str::from_utf8(body).unwrap_or("<not utf-8>"));
        if nl {
            let _ = out.write_str("\r\n");
        }
    }
    let _ = writeln!(
        out,
        "{PREFIX} paced report-end n={n} rom={ROM_NAME} bytes={} cksum={} overflow={}\r",
        cap.len(),
        image::cksum(cap),
        u8::from(overflow)
    );
}

/// The refill that completes the start ramp: `AudioDac::start` fills both
/// halves with its first [`audio_dac::FRAMES`] frames and each refill adds
/// [`audio_dac::HALF`], so the ramp's [`audio_dac::RAMP_FRAMES`] are done
/// at refill 30, and refill 31 is the first that pops the ring.
const RAMP_REFILLS: u32 =
    (audio_dac::RAMP_FRAMES - audio_dac::FRAMES as u32) / audio_dac::HALF as u32;
const _: () = assert!(RAMP_REFILLS == 30);
/// The run starts right after the refill this many before the ramp's
/// last: five halves (39 ms) before the first pop. The loop needs about
/// 22 ms to fill the ring (the `paced v1` line, 9 ms on the UART, then
/// four calls of at most 7.1 ms, `synth:worst-case`); with two refills
/// (23.4 ms) a 1,200-frame run's `min_fill` was 793, against 1,243 in
/// steady state.
const RAMP_LEFT: u32 = 4;

/// DBGMCU_CR (RM0433 Rev 9 61.5.8), bit 0 DBGSLEEP_D1: 1 keeps the CPU
/// clock running in Sleep (CSleep).
const DBGSLEEP_D1: u32 = 1 << 0;

fn dbgmcu() -> &'static pac::dbgmcu::RegisterBlock {
    // SAFETY: DBGMCU_CR is written only from `main` (`set_sleep_clock`),
    // nothing else in the image touches it; the banner only reads IDC.
    unsafe { &*pac::DBGMCU::ptr() }
}

/// DBGSLEEP_D1 set (`keep`) or clear.
fn set_sleep_clock(keep: bool) {
    let cr = dbgmcu().cr.read().bits();
    let cr = if keep {
        cr | DBGSLEEP_D1
    } else {
        cr & !DBGSLEEP_D1
    };
    // SAFETY: only bit 0 changes; the other bits are written back as read.
    dbgmcu().cr.write(|w| unsafe { w.bits(cr) });
    let _ = dbgmcu().cr.read();
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
    let mut display = display::init();
    let mut dac = AudioDac::new();

    let dbgmcu_cr_boot = dbgmcu().cr.read().bits();
    let gate = sleep_te_cycles(false);
    let keep = sleep_te_cycles(true);
    // The run sleeps with the clock kept (module docs).
    set_sleep_clock(true);
    let _ = writeln!(
        board.uart,
        "{PREFIX} paced running rom={ROM_NAME} rom_bytes={} presses={count} frames={frames}\r",
        ROM.len()
    );

    let mut cap = Capture {
        buf: b.capture,
        len: 0,
        overflow: false,
        live: Some(&mut board.uart),
    };
    let clock = Dwt::new();
    // The start ramp (250 ms) first, up to [`RAMP_LEFT`] refills before
    // its end: started right before the run, the loop would fill the ring
    // and then idle through the ramp, which is 250 ms of real time with
    // no samples consumed in the speed figures (-417 ppm over 10 minutes,
    // -1.2% over 20 s: measured -8,630 ppm on a 1,200-frame run).
    let ramp0 = audio_dac::stats().refills;
    let t0 = cycles();
    dac.start();
    while audio_dac::stats().refills.wrapping_sub(ramp0) < RAMP_REFILLS - RAMP_LEFT
        && cycles().wrapping_sub(t0) < CPU_HZ
    {}
    let ramp_wait = cycles().wrapping_sub(t0);
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
            sink: &mut display,
            idle: &mut Wfi { dac: &mut dac },
        },
        &mut cap,
    );
    let run_refills = audio_dac::stats().refills.wrapping_sub(r0);
    let run_cycles = clock.total() - c0;
    let stop = dac.stop();
    cap.live = None;
    let _ = display.report(&mut cap);
    let s = audio_dac::stats();
    let _ = writeln!(
        cap,
        "stats dac refills={} late={} spurious={} te={} dme={} fe={} udr1={} udr2={} \
         restarts={} isr_min={} isr_max={} stop={}",
        s.refills,
        s.late,
        s.spurious,
        s.te,
        s.dme,
        s.fe,
        s.udr1,
        s.udr2,
        s.restarts,
        s.isr_min,
        s.isr_max,
        if stop.is_ok() { "ok" } else { "timeout" }
    );
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
        report(&mut board, n, frames, captured, overflow);
        led::toggle();
        n = n.wrapping_add(1);
        wait_since(cycles(), REPORT_GAP);
    }
}

/// The panel as the paced loop's [`FrameSink`]: shows a completed frame
/// when the previous transfer has finished, skips it otherwise; never
/// waits (module docs).
///
/// [`FrameSink`]: gbc_firmware::headless::FrameSink
mod display {
    use core::fmt::{self, Write};

    use gbc_core::Frame;
    use gbc_firmware::headless::FrameSink;
    use gbc_firmware::video::scale_frame;
    use gbc_nucleo_h753::lcd::{self, FrameRate, Lcd};
    use gbc_nucleo_h753::lcd_dma::{self, LcdDma};
    use gbc_nucleo_h753::{CPU_HZ, cycles};

    /// A transfer still pending or running this long after `present` is
    /// abandoned (100 ms: TE comes every 16.7 ms and a transfer takes
    /// 8.1 ms, so a healthy one is done within 25 ms).
    pub const STUCK: u32 = CPU_HZ / 10;

    /// Count, max and sum of cycle counts.
    #[derive(Clone, Copy, Default)]
    struct Range {
        n: u32,
        max: u32,
        sum: u64,
    }

    impl Range {
        fn add(&mut self, v: u32) {
            self.n += 1;
            self.max = self.max.max(v);
            self.sum += u64::from(v);
        }
        fn mean(&self) -> u64 {
            self.sum / u64::from(self.n.max(1))
        }
    }

    pub struct Display {
        dma: LcdDma,
        /// When the transfer now pending or running was queued.
        queued_at: Option<u32>,
        presented: u32,
        skipped: u32,
        transfers: u32,
        errors: u32,
        stuck: u32,
        first_error: Option<(u32, lcd_dma::Error)>,
        scale: Range,
        present: Range,
    }

    /// Sets the panel up (FMC at `SPEC`, the Game Boy frame rate, all
    /// black) and the transfer (ADR 0039).
    pub fn init() -> Display {
        let mut l = Lcd::init(lcd::SPEC);
        l.set_frame_rate(FrameRate::GAME_BOY);
        l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
        l.begin_pixels();
        l.fill(0, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
        Display {
            dma: LcdDma::new(l),
            queued_at: None,
            presented: 0,
            skipped: 0,
            transfers: 0,
            errors: 0,
            stuck: 0,
            first_error: None,
            scale: Range::default(),
            present: Range::default(),
        }
    }

    impl Display {
        /// Notes how a finished transfer ended (`n`: the frame that found
        /// it finished).
        fn outcome(&mut self, n: u32, r: Result<Option<lcd_dma::Transfer>, lcd_dma::Error>) {
            match r {
                Ok(Some(_)) => self.transfers += 1,
                Ok(None) => {}
                Err(e) => {
                    self.errors += 1;
                    self.first_error.get_or_insert((n, e));
                }
            }
        }

        /// After the run: the last transfer waited for (at most
        /// [`STUCK`]), then the `stats display paced` line.
        pub fn report(&mut self, out: &mut dyn Write) -> fmt::Result {
            if self.queued_at.take().is_some() {
                let r = self.dma.wait(STUCK);
                if matches!(r, Err(lcd_dma::Error::Timeout { .. })) {
                    self.stuck += 1;
                }
                self.outcome(u32::MAX, r);
            }
            let te = lcd_dma::te_stats();
            write!(
                out,
                "stats display paced presented={} skipped={} transfers={} transfer_errors={} \
                 stuck={} aborts={} te_edges={} scale_mean={} scale_max={} present_mean={} \
                 present_max={}",
                self.presented,
                self.skipped,
                self.transfers,
                self.errors,
                self.stuck,
                self.dma.aborts(),
                te.edges,
                self.scale.mean(),
                self.scale.max,
                self.present.mean(),
                self.present.max,
            )?;
            if let Some((n, e)) = self.first_error {
                write!(out, " first_error=frame{n}:{e:?}")?;
            }
            writeln!(out)
        }
    }

    impl FrameSink for Display {
        fn frame(&mut self, n: u32, frame: &Frame) {
            if let Some(at) = self.queued_at {
                match self.dma.try_wait() {
                    Some(r) => self.outcome(n, r),
                    None if cycles().wrapping_sub(at) > STUCK => {
                        // Abandoned (`wait` with no time left aborts the
                        // channel and reports the timeout).
                        let r = self.dma.wait(0);
                        self.stuck += 1;
                        self.outcome(n, r);
                    }
                    None => {
                        self.skipped += 1;
                        return;
                    }
                }
                self.queued_at = None;
            }
            let t0 = cycles();
            // No transfer is pending or running: `buffer` does not wait.
            scale_frame(frame, self.dma.buffer());
            let t1 = cycles();
            self.dma.present();
            let t2 = cycles();
            self.scale.add(t1.wrapping_sub(t0));
            self.present.add(t2.wrapping_sub(t1));
            self.presented += 1;
            self.queued_at = Some(t2);
        }
    }
}
