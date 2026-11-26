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
//!
//! With the `rom-qspi` feature (plan Stage 2 M2; `out/headless-qspi.bin`)
//! the ROM is not embedded: at boot the image maps the QSPI NOR
//! (`qspi::Qspi::memory_mapped`, 0xEB at [`qspi::PRESCALER_MMAP`]) and
//! checks the ADR 0037 image there with `rom_image::validate`; the run's
//! ROM slice is the image's data in the 0x9000_0000 window (cached), which
//! stays mapped for the life of the program. On any QSPI or image error it
//! prints `<prefix> headless qspi error stage=<map|header|validate>
//! error=<e>` once a second and never runs a frame: there is no embedded
//! ROM to fall back to. Its banner is followed by
//!
//! ```text
//! gbc-nucleo-h753 headless qspi image rom_len=<n> rom_hash=<16 hex> title=<t> validate_cycles=<c> qspi_hz=<hz> exit_fallbacks=<k>
//! ```
//!
//! (`rom_hash` the header's FNV-1a 64 of the data, which `validate`
//! checked; `title` the header's, non-graphic bytes as `_`), and its report
//! line ends with `rom_len=<n> rom_hash=<16 hex> exit_fallbacks=<k>`:
//! `run-headless.sh` with `RUN_HEADLESS_ROM` requires them to be the ROM
//! file's, so a stale NOR image cannot pass.
//!
//! With the `display` feature (plan Stage 2 M3; build.sh
//! `headless-display-<rom>`) the run also shows every completed frame on
//! the shield's panel and reads each one back ([`display`]); the
//! `headless` lines stay those of the run without it, and the display's
//! figures follow the run's `stats` lines as `stats display ...` lines.
//! `GBC_DISPLAY_START` (build.rs; default `te-wait`) picks what starts a
//! transfer: `te-wait`, `te`, `now`, or `off` (no display, the same code).
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

#[cfg(feature = "rom-qspi")]
use gbc_firmware::rom_image::{self, ImageHeader};
#[cfg(feature = "rom-qspi")]
use gbc_nucleo_h753::qspi::{self, Qspi};

const PREFIX: &str = "gbc-nucleo-h753";
#[cfg(not(any(feature = "rom-qspi", feature = "display")))]
const IMAGE: &str = "headless";
#[cfg(feature = "display")]
const IMAGE: &str = "headless-display";
#[cfg(feature = "rom-qspi")]
const IMAGE: &str = "headless-qspi";

/// The ROM, embedded by build.rs from `GBC_HEADLESS_ROM` (in `.rodata`,
/// internal flash).
#[cfg(not(feature = "rom-qspi"))]
static ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/headless.gb"));
/// The ROM's file stem, for the report lines.
#[cfg(not(feature = "rom-qspi"))]
const ROM_NAME: &str = env!("GBC_HEADLESS_ROM_NAME");
/// The ROM is whatever the NOR holds; its header identifies it.
#[cfg(feature = "rom-qspi")]
const ROM_NAME: &str = "qspi";
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
#[cfg(not(feature = "display"))]
const CAPTURE_BYTES: usize = 128 * 1024;
/// With the display, half (about 1,000 frames): the panel's 128 KiB
/// buffer is aligned to its size (`lcd_dma::DmaBuffer`), so it goes at
/// the next 128 KiB boundary after these buffers, and with a 128 KiB
/// capture it would end at the top of AXI SRAM, leaving no room for the
/// fault stack. The frame and audio buffers keep their addresses (the
/// cartridge RAM moves; the acceptance ROMs use none).
#[cfg(feature = "display")]
const CAPTURE_BYTES: usize = 64 * 1024;
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
    /// Where the ROM came from.
    #[cfg(feature = "rom-qspi")]
    qspi: QspiRom,
}

fn report(board: &mut board::Board, n: u32, run: &Run, cap: &Capture) {
    board.banner(IMAGE);
    #[cfg(feature = "rom-qspi")]
    run.qspi.print(board);
    let (image_bytes, image_cksum) = board.image();
    let ms = run.cycles * 1000 / u64::from(CPU_HZ);
    let out = &mut board.uart;
    // The UART writer never fails.
    #[cfg(not(feature = "rom-qspi"))]
    let _ = writeln!(
        out,
        "{PREFIX} headless report n={n} rom={ROM_NAME} frames={} run_cycles={} run_ms={ms} \
         stack_used={}/{} image_bytes={image_bytes} image_cksum={image_cksum}\r",
        run.frames, run.cycles, run.stack.0, run.stack.1
    );
    #[cfg(feature = "rom-qspi")]
    let _ = writeln!(
        out,
        "{PREFIX} headless report n={n} rom={ROM_NAME} frames={} run_cycles={} run_ms={ms} \
         stack_used={}/{} image_bytes={image_bytes} image_cksum={image_cksum} \
         rom_len={} rom_hash={:016x} exit_fallbacks={}\r",
        run.frames,
        run.cycles,
        run.stack.0,
        run.stack.1,
        run.qspi.header.len,
        run.qspi.header.hash,
        qspi::exit_fallbacks()
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

/// The QSPI image the run's ROM came from.
#[cfg(feature = "rom-qspi")]
struct QspiRom {
    header: ImageHeader,
    /// Cycles of `rom_image::validate` (header checks and the data hash,
    /// through the cold window).
    validate_cycles: u32,
    qspi_hz: u32,
}

#[cfg(feature = "rom-qspi")]
impl QspiRom {
    /// The `headless qspi image` line.
    fn print(&self, board: &mut board::Board) {
        let h = &self.header;
        let out = &mut board.uart;
        let _ = write!(
            out,
            "{PREFIX} headless qspi image rom_len={} rom_hash={:016x} title=",
            h.len, h.hash
        );
        // Up to the first NUL; anything but a graphic ASCII byte as `_`, so
        // the field has no spaces and the line stays ASCII.
        for &b in h.title.iter().take_while(|&&b| b != 0) {
            let c = if b.is_ascii_graphic() { b as char } else { '_' };
            let _ = out.write_char(c);
        }
        let _ = writeln!(
            out,
            " validate_cycles={} qspi_hz={} exit_fallbacks={}\r",
            self.validate_cycles,
            self.qspi_hz,
            qspi::exit_fallbacks()
        );
    }
}

/// Prints the QSPI error once a second, forever: no frame runs without a
/// valid image (and there is no embedded ROM to fall back to).
#[cfg(feature = "rom-qspi")]
fn qspi_error(board: &mut board::Board, stage: &str, error: &dyn fmt::Debug) -> ! {
    loop {
        let _ = writeln!(
            board.uart,
            "{PREFIX} headless qspi error stage={stage} error={error:?}\r"
        );
        let t0 = cycles();
        wait_since(t0, CPU_HZ);
        led::toggle();
    }
}

/// Maps the NOR and validates the image in it. The returned [`qspi::Mapped`]
/// must outlive every use of the ROM slice (`rom_image::validate` over its
/// window), which `main` guarantees by never returning.
#[cfg(feature = "rom-qspi")]
fn map_qspi(board: &mut board::Board) -> qspi::Mapped {
    let mut q = Qspi::init(qspi::PRESCALER_SAFE);
    if let Err(e) = q.set_prescaler(qspi::PRESCALER_MMAP) {
        qspi_error(board, "map", &e);
    }
    match q.memory_mapped(&qspi::QUAD_IO_READ) {
        Ok(m) => m,
        Err((_, e)) => qspi_error(board, "map", &e),
    }
}

#[cfg(feature = "rom-qspi")]
fn validate_qspi<'a>(board: &mut board::Board, m: &'a qspi::Mapped) -> (&'a [u8], QspiRom) {
    let window = m.bytes();
    let t0 = cycles();
    let header = match rom_image::read_header(window) {
        Ok(h) => h,
        Err(e) => qspi_error(board, "header", &e),
    };
    let rom = match rom_image::validate(window) {
        Ok(r) => r,
        Err(e) => qspi_error(board, "validate", &e),
    };
    let info = QspiRom {
        header,
        validate_cycles: cycles().wrapping_sub(t0),
        qspi_hz: m.clock_hz(),
    };
    info.print(board);
    (rom, info)
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
    #[cfg(not(feature = "rom-qspi"))]
    let rom = ROM;
    // Mapped for good: `main` never returns, so the slice stays valid.
    #[cfg(feature = "rom-qspi")]
    let mapped = map_qspi(&mut board);
    #[cfg(feature = "rom-qspi")]
    let (rom, qspi_rom) = validate_qspi(&mut board, &mapped);

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
        rom.len()
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
    #[cfg(feature = "display")]
    let mut display = display::init();
    let clock = Dwt::new();
    // The result is in the captured lines (`headless error ...`, or the
    // lines stopping short); the host compares those.
    #[cfg(not(feature = "display"))]
    let _ = headless::run(
        rom,
        Script::new(&presses[..count]),
        frames,
        bufs,
        &clock,
        &mut cap,
    );
    #[cfg(feature = "display")]
    {
        let r = headless::run_with_sink(
            rom,
            Script::new(&presses[..count]),
            frames,
            bufs,
            &clock,
            &mut cap,
            &mut display,
        );
        let core = match r {
            Ok(s) => (s.cycles.sum / u64::from(s.frames.max(1)), s.cycles.max),
            Err(_) => (0, 0),
        };
        // After the run's lines, as `stats ` lines (not compared). A full
        // capture sets `overflow` and the host refuses the report.
        let _ = display.report(&mut cap, core);
    }
    let run = Run {
        frames,
        cycles: clock.total(),
        stack: stack::usage(),
        #[cfg(feature = "rom-qspi")]
        qspi: qspi_rom,
    };

    let mut n = 0u32;
    loop {
        report(&mut board, n, &run, &cap);
        led::toggle();
        n = n.wrapping_add(1);
        wait_since(cycles(), REPORT_GAP);
    }
}

/// The panel in the loop (plan Stage 2 M3, the `display` feature): a
/// [`FrameSink`] that shows every completed frame and checks the previous
/// one by reading it back. The `headless` lines are those of a run without
/// it (the sink writes no line, `gbc_firmware::headless` "Board hook"); its
/// figures go on `stats display` lines after the run (not compared).
///
/// Per completed frame `n`, outside the core's cycle count:
/// 1. wait for frame `n - 1`'s transfer to finish, then read the window
///    back and compare it with the buffer, which still holds `n - 1`
///    ([`lcd_dma::readback`], ~70 ms);
/// 2. `scale_frame` frame `n` into the buffer (timed);
/// 3. the sentinel (M3 review T-2): the bitwise inverse of frame `n`
///    written into the window by the CPU ([`lcd_dma::write_inverse`],
///    ~12 ms, timed separately: self-test overhead, not part of the 5%
///    budget), so every pixel the transfer of `n` fails to write reads
///    back as a mismatch, even when frame `n` equals frame `n - 1`;
/// 4. queue frame `n` (`present`, timed: window commands and the MDMA
///    registers);
/// 5. depending on [`Start`]: wait until TE has started it, start it at
///    once, or return with it queued.
///
/// So with `te-wait` and `now` the transfer of frame `n` (8.1 ms) runs
/// while the core emulates frame `n + 1`, for every frame (the contention
/// measurement); with `te` the overlap depends on where TE falls. After
/// the run the last transfer is read back too, then a control (one buffer
/// word inverted must give exactly 2 mismatching pixels).
#[cfg(feature = "display")]
mod display {
    use core::fmt::{self, Write};
    use core::hint::black_box;

    use gbc_core::{FRAME_CLOCKS, Frame};
    use gbc_firmware::headless::FrameSink;
    use gbc_firmware::video::scale_frame;
    use gbc_nucleo_h753::lcd::{self, FrameRate, Lcd};
    use gbc_nucleo_h753::lcd_dma::{self, LcdDma};
    use gbc_nucleo_h753::pac::interrupt;
    use gbc_nucleo_h753::{CPU_HZ, cycles};

    #[interrupt]
    fn EXTI15_10() {
        lcd_dma::on_te();
    }

    /// What starts a queued transfer (`GBC_DISPLAY_START`, build.rs).
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum Start {
        /// No display: the sink returns at once (the panel is set up and
        /// black). The same code as the other modes, for a same-build
        /// baseline of the core's cycles.
        Off,
        /// TE, and the sink waits until it has started (ADR 0039's start,
        /// with the overlap made certain).
        TeWait,
        /// TE; the sink returns with the transfer queued.
        Te,
        /// At once, from the sink (`LcdDma::start_now`; may tear).
        Now,
    }

    impl Start {
        const NAMES: [(&str, Start); 4] = [
            ("off", Start::Off),
            ("te-wait", Start::TeWait),
            ("te", Start::Te),
            ("now", Start::Now),
        ];

        pub fn name(self) -> &'static str {
            match self {
                Start::Off => "off",
                Start::TeWait => "te-wait",
                Start::Te => "te",
                Start::Now => "now",
            }
        }
    }

    const fn same(a: &str, b: &str) -> bool {
        let (a, b) = (a.as_bytes(), b.as_bytes());
        if a.len() != b.len() {
            return false;
        }
        let mut i = 0;
        while i < a.len() {
            if a[i] != b[i] {
                return false;
            }
            i += 1;
        }
        true
    }

    const fn code(s: &str) -> u8 {
        let mut i = 0;
        while i < Start::NAMES.len() {
            if same(s, Start::NAMES[i].0) {
                return i as u8;
            }
            i += 1;
        }
        panic!("GBC_DISPLAY_START: not off, te-wait, te or now");
    }

    /// The mode's index in [`Start::NAMES`]. Read through `black_box`, so
    /// the compiler cannot specialise the code on it: images that differ
    /// only in the mode have the same code (one constant differs).
    const START_CODE: u8 = code(env!("GBC_DISPLAY_START"));

    fn start() -> Start {
        match black_box(START_CODE) {
            0 => Start::Off,
            1 => Start::TeWait,
            2 => Start::Te,
            _ => Start::Now,
        }
    }

    /// Any wait here is shorter: 50 ms (a TE period plus a transfer is
    /// 25 ms).
    const TIMEOUT: u32 = CPU_HZ / 20;

    /// Min / max / sum of a series of cycle counts.
    #[derive(Clone, Copy)]
    struct Range {
        min: u32,
        max: u32,
        sum: u64,
        n: u32,
    }

    impl Range {
        const fn new() -> Range {
            Range {
                min: 0,
                max: 0,
                sum: 0,
                n: 0,
            }
        }
        fn add(&mut self, v: u32) {
            self.min = if self.n == 0 { v } else { self.min.min(v) };
            self.max = self.max.max(v);
            self.sum += u64::from(v);
            self.n += 1;
        }
        fn mean(&self) -> u64 {
            self.sum / u64::from(self.n.max(1))
        }
    }

    fn range(out: &mut dyn Write, name: &str, r: &Range) -> fmt::Result {
        write!(
            out,
            " {name}_min={} {name}_mean={} {name}_max={}",
            r.min,
            r.mean(),
            r.max
        )
    }

    fn us(c: u64) -> u64 {
        c * 1_000_000 / u64::from(CPU_HZ)
    }

    /// (x, y, got, want), as `lcd_dma::Readback::first`.
    type BadPixel = (u16, u16, [u8; 3], [u8; 3]);

    pub struct Display {
        dma: LcdDma,
        start: Start,
        /// The frame whose transfer is queued or running and not yet read
        /// back.
        queued: Option<u32>,
        /// A wait timed out: the display stops (the channel's state is
        /// unknown, so nothing may touch the FMC again).
        dead: bool,
        frames: u32,
        transfers: u32,
        readbacks: u32,
        mismatches: u32,
        /// The first mismatching pixel.
        first_bad: Option<(u32, BadPixel)>,
        errors: u32,
        first_error: Option<(u32, lcd_dma::Error)>,
        start_timeouts: u32,
        /// Transfers already finished when the sink first looked (their
        /// `start_to_done` is then only an upper bound, so not in `mdma`).
        late: u32,
        scale: Range,
        setup: Range,
        mdma: Range,
        te_to_start: Range,
        /// The wait for the previous transfer (TE included).
        wait: Range,
        /// `te-wait`: from the end of `present` until TE started it.
        to_start: Range,
        readback: Range,
        /// The sentinel's CPU writes ([`lcd_dma::write_inverse`]).
        sentinel: Range,
    }

    /// Sets the panel up (FMC at `SPEC`, the Game Boy frame rate, all
    /// black) and the transfer (ADR 0039).
    pub fn init() -> Display {
        let mut l = Lcd::init(lcd::SPEC);
        l.set_frame_rate(FrameRate::GAME_BOY);
        l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
        l.begin_pixels();
        l.fill(0, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
        let dma = LcdDma::new(l);
        Display {
            dma,
            start: start(),
            queued: None,
            dead: false,
            frames: 0,
            transfers: 0,
            readbacks: 0,
            mismatches: 0,
            first_bad: None,
            errors: 0,
            first_error: None,
            start_timeouts: 0,
            late: 0,
            scale: Range::new(),
            setup: Range::new(),
            mdma: Range::new(),
            te_to_start: Range::new(),
            wait: Range::new(),
            to_start: Range::new(),
            readback: Range::new(),
            sentinel: Range::new(),
        }
    }

    impl Display {
        /// Waits for the queued transfer and reads it back.
        fn finish(&mut self) {
            let Some(n) = self.queued.take() else {
                return;
            };
            let t0 = cycles();
            let r = match self.dma.try_wait() {
                Some(r) => {
                    if matches!(r, Ok(Some(_))) {
                        self.late += 1;
                    }
                    r
                }
                None => match self.dma.wait(TIMEOUT) {
                    Ok(Some(t)) => {
                        self.mdma.add(t.start_to_done);
                        Ok(Some(t))
                    }
                    r => r,
                },
            };
            self.wait.add(cycles().wrapping_sub(t0));
            match r {
                Ok(Some(t)) => {
                    self.transfers += 1;
                    self.te_to_start.add(t.te_to_start);
                }
                Ok(None) => {}
                Err(e) => {
                    self.errors += 1;
                    self.first_error.get_or_insert((n, e));
                    if matches!(e, lcd_dma::Error::Timeout { .. }) {
                        self.dead = true;
                        return;
                    }
                }
            }
            let t0 = cycles();
            let (l, buf) = self.dma.parts();
            let rb = lcd_dma::readback(l, buf);
            self.readback.add(cycles().wrapping_sub(t0));
            self.readbacks += 1;
            self.mismatches += rb.mismatches;
            if let (None, Some(p)) = (self.first_bad, rb.first) {
                self.first_bad = Some((n, p));
            }
        }

        /// After the run: the last transfer read back, the control, and
        /// the `stats display` lines. `run_frame`: the core's (mean, max)
        /// cycles from the run's summary.
        pub fn report(&mut self, out: &mut dyn Write, run_frame: (u64, u32)) -> fmt::Result {
            let mut control = None;
            if self.start != Start::Off && !self.dead {
                self.finish();
                if !self.dead && self.readbacks > 0 {
                    // The GRAM holds the last frame: one buffer word
                    // inverted (both its pixels, every bit) must give
                    // exactly 2 mismatching pixels.
                    let (l, buf) = self.dma.parts();
                    buf[100][60] ^= 0xFFFF_FFFF;
                    let rb = lcd_dma::readback(l, buf);
                    buf[100][60] ^= 0xFFFF_FFFF;
                    control = Some(rb.mismatches);
                }
            }
            let te = lcd_dma::te_stats();
            write!(
                out,
                "stats display start={} frames={} transfers={} readbacks={} mismatches={} \
                 control_mismatches=",
                self.start.name(),
                self.frames,
                self.transfers,
                self.readbacks,
                self.mismatches,
            )?;
            match control {
                Some(c) => write!(out, "{c}")?,
                None => write!(out, "none")?,
            }
            write!(
                out,
                " transfer_errors={} start_timeouts={} dead={} late={} te_edges={} \
                 te_min={} te_max={}",
                self.errors,
                self.start_timeouts,
                u8::from(self.dead),
                self.late,
                te.edges,
                te.min,
                te.max,
            )?;
            if let Some((n, (x, y, got, want))) = self.first_bad {
                write!(
                    out,
                    " first_bad=frame{n}:({x},{y}) got={:02x}{:02x}{:02x} want={:02x}{:02x}{:02x}",
                    got[0], got[1], got[2], want[0], want[1], want[2]
                )?;
            }
            if let Some((n, e)) = self.first_error {
                write!(out, " first_error=frame{n}:{e:?}")?;
            }
            writeln!(out)?;

            // The CPU's share: conversion plus queueing, against 5% of a
            // real-time frame at CPU_HZ (plan M3 acceptance).
            let frame = u64::from(CPU_HZ) * u64::from(FRAME_CLOCKS) / u64::from(gbc_core::CLOCK_HZ);
            let budget = frame / 20;
            let worst = u64::from(self.scale.max) + u64::from(self.setup.max);
            write!(out, "stats display cpu")?;
            range(out, "scale", &self.scale)?;
            range(out, "setup", &self.setup)?;
            writeln!(
                out,
                " worst_sum={worst} budget={budget} ok={}",
                u8::from(self.scale.n > 0 && worst <= budget)
            )?;

            write!(out, "stats display dma")?;
            range(out, "mdma", &self.mdma)?;
            write!(out, " mdma_max_us={}", us(u64::from(self.mdma.max)))?;
            range(out, "te_to_start", &self.te_to_start)?;
            writeln!(out)?;

            write!(out, "stats display waits")?;
            range(out, "wait", &self.wait)?;
            range(out, "to_start", &self.to_start)?;
            range(out, "readback", &self.readback)?;
            range(out, "sentinel", &self.sentinel)?;
            writeln!(out)?;

            writeln!(
                out,
                "stats display core start={} run_frame_mean={} run_frame_max={}",
                self.start.name(),
                run_frame.0,
                run_frame.1
            )
        }
    }

    impl FrameSink for Display {
        fn frame(&mut self, n: u32, frame: &Frame) {
            if self.start == Start::Off || self.dead {
                return;
            }
            self.frames += 1;
            self.finish();
            if self.dead {
                return;
            }
            let t0 = cycles();
            scale_frame(frame, self.dma.buffer());
            let t1 = cycles();
            // The sentinel (T-2): `finish` left no transfer running.
            let (l, buf) = self.dma.parts();
            lcd_dma::write_inverse(l, buf);
            let ts = cycles();
            self.dma.present();
            if self.start == Start::Now {
                self.dma.start_now();
            }
            let t2 = cycles();
            self.scale.add(t1.wrapping_sub(t0));
            self.sentinel.add(ts.wrapping_sub(t1));
            self.setup.add(t2.wrapping_sub(ts));
            self.queued = Some(n);
            if self.start == Start::TeWait {
                if !self.dma.wait_started(TIMEOUT) {
                    self.start_timeouts += 1;
                }
                self.to_start.add(cycles().wrapping_sub(t2));
            }
        }
    }
}
