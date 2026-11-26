//! The `headless` image (plan Stage 2 M1): one ROM from internal flash, run
//! by `gbc_firmware::headless` for a fixed number of frames with an
//! optional input script, in the M6-target placement (the ITCM set and the
//! data in DTCM; build.rs). Its `headless ` lines must equal the host
//! tool's (`tools/headless`) byte for byte; `run-headless.sh` compares
//! them.
//!
//! The run happens once after reset, its lines going into a capture buffer
//! in AXI SRAM rather than straight to the UART: nothing reads the serial
//! port during a flash (the ST-LINK refuses to program with it open), so
//! the lines of a run printed live would be lost. After the run the image
//! idles, re-sending a report, with a pause of [`REPORT_GAP`] cycles
//! after each:
//!
//! ```text
//! <banner>                                        (Board::banner)
//! gbc-nucleo-h753 headless report n=<k> rom=<name> frames=<N> run_cycles=<c> run_ms=<ms> stack_used=<b>/<b> image_bytes=<n> image_cksum=<crc>
//! <the captured lines, exactly as gbc_firmware::headless::run wrote them>
//! gbc-nucleo-h753 headless end n=<k> rom=<name> bytes=<len> cksum=<crc> overflow=<0|1>
//! ```
//!
//! `bytes` and `cksum` are the length and the POSIX `cksum` CRC of the
//! captured bytes (`\n` line ends; the UART sends `\r\n`), so the host can
//! tell a report garbled or cut by the serial link (a tool error) from
//! lines that differ. `image_bytes` and `image_cksum` identify the image
//! the board runs (`board::image`): run-headless.sh requires them to be
//! the `.bin` it flashed. `run_cycles` is the DWT count of the whole
//! `headless::run` call (emulation, hashing and formatting, no UART).
//!
//! Memory (ADR 0036: the board declares the statics, gbc-firmware borrows
//! them): the emulator's `Memory` in `.data` (`Memory::new()` is not all
//! zeros, so its 51,016 B initializer is also in flash), which this
//! placement puts in DTCM with the stack; the frame buffer, audio buffer,
//! cartridge RAM and
//! capture buffer in `.axisram` (AXI SRAM, write-back cached).
#![no_std]
#![no_main]

use core::cell::{Cell, UnsafeCell};
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::CycleClock;
use gbc_firmware::headless::{self, Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, image, led, stack, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "headless";

/// The ROM, embedded by build.rs from `GBC_HEADLESS_ROM` (in `.rodata`,
/// internal flash).
static ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/headless.gb"));
/// The ROM's file stem, for the report lines.
const ROM_NAME: &str = env!("GBC_HEADLESS_ROM_NAME");
/// The input program from `GBC_HEADLESS_INPUT` (empty: no input).
const INPUT: &str = include_str!(concat!(env!("OUT_DIR"), "/headless-input.txt"));
/// `GBC_HEADLESS_FRAMES`, or empty.
const FRAMES: &str = env!("GBC_HEADLESS_FRAMES");
/// Frames when neither `GBC_HEADLESS_FRAMES` nor an `end` line gives them
/// (the M1 acceptance runs).
const DEFAULT_FRAMES: u32 = 600;
/// The most presses an input program may have (12 B each, on the stack).
const MAX_PRESSES: usize = 256;

/// Cartridge RAM: the MBC5 maximum, 128 KiB (Pan Docs "MBC5").
const CART_RAM_BYTES: usize = 128 * 1024;
/// The capture buffer: about 64 B per frame line, so room for about 2,000
/// frames.
const CAPTURE_BYTES: usize = 128 * 1024;
/// The pause after each report (1 s at [`CPU_HZ`]); a report of 600
/// frames takes about 3.5 s on the wire.
const REPORT_GAP: u32 = CPU_HZ;

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
    // is a value); `headless::run` fills the frame and cartridge RAM before
    // it reads them, the audio buffer is written before it is read, and the
    // capture is read only up to what was written.
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

/// The DWT cycle counter as a [`CycleClock`]. It also extends the counter
/// to 64 bits on every reading, so [`Dwt::total`] spans a run longer than
/// one 32-bit wrap (9.1 s) as long as readings are less than a wrap apart
/// (`headless::run` reads it twice per frame).
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

/// The run's lines, kept for the reports. Writing past the end fails (and
/// so ends the run with `headless::Error::Write`) and sets `overflow`.
struct Capture {
    buf: &'static mut [u8; CAPTURE_BYTES],
    len: usize,
    overflow: bool,
}

impl Write for Capture {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let Some(dst) = self.buf.get_mut(self.len..self.len + s.len()) else {
            self.overflow = true;
            return Err(fmt::Error);
        };
        dst.copy_from_slice(s.as_bytes());
        self.len += s.len();
        Ok(())
    }
}

impl Capture {
    fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

/// What the reports say about the run.
struct Run {
    frames: u32,
    cycles: u64,
    stack: (usize, usize),
}

fn report(board: &mut board::Board, n: u32, run: &Run, cap: &Capture) {
    board.banner(IMAGE);
    let (image_bytes, image_cksum) = board.image();
    let ms = run.cycles * 1000 / u64::from(CPU_HZ);
    let out = &mut board.uart;
    // The UART writer never fails.
    let _ = writeln!(
        out,
        "{PREFIX} headless report n={n} rom={ROM_NAME} frames={} run_cycles={} run_ms={ms} \
         stack_used={}/{} image_bytes={image_bytes} image_cksum={image_cksum}\r",
        run.frames, run.cycles, run.stack.0, run.stack.1
    );
    // The captured lines end in `\n` alone; the UART sends `\r\n`, as
    // every other line of the board does.
    for line in cap.bytes().split_inclusive(|&b| b == b'\n') {
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
        "{PREFIX} headless end n={n} rom={ROM_NAME} bytes={} cksum={} overflow={}\r",
        cap.len,
        image::cksum(cap.bytes()),
        u8::from(cap.overflow)
    );
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
                "{PREFIX} headless input error line={} kind={:?}\r",
                e.line, e.kind
            );
            let t0 = cycles();
            wait_since(t0, CPU_HZ);
            led::toggle();
        },
    };
    let frames = FRAMES.parse().ok().or(end).unwrap_or(DEFAULT_FRAMES);
    let _ = writeln!(
        board.uart,
        "{PREFIX} headless running rom={ROM_NAME} rom_bytes={} presses={count} frames={frames}\r",
        ROM.len()
    );

    let b = buffers::take();
    let mut cap = Capture {
        buf: b.capture,
        len: 0,
        overflow: false,
    };
    let bufs = Buffers {
        mem: b.mem,
        save_ram: &mut b.cart_ram[..],
        frame: b.frame,
        audio: &mut b.audio[..],
    };
    let clock = Dwt::new();
    // The result is in the captured lines (`headless error ...`, or the
    // lines stopping short); the host compares those.
    let _ = headless::run(
        ROM,
        Script::new(&presses[..count]),
        frames,
        bufs,
        &clock,
        &mut cap,
    );
    let run = Run {
        frames,
        cycles: clock.total(),
        stack: stack::usage(),
    };

    let mut n = 0u32;
    loop {
        report(&mut board, n, &run, &cap);
        led::toggle();
        n = n.wrapping_add(1);
        wait_since(cycles(), REPORT_GAP);
    }
}
