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
//! the shield's panel and reads each one back (`display::measure`,
//! src/display/measure.rs, which owns that code); the
//! `headless` lines stay those of the run without it, and the display's
//! figures follow the run's `stats` lines as `stats display ...` lines.
//! `GBC_DISPLAY_START` (build.rs; default `te-wait`) picks what starts a
//! transfer: `te-wait`, `te`, `now`, or `off` (no display, the same code).
#![no_std]
#![no_main]

use core::fmt::Write;

use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::headless::{self, Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};
use gbc_nucleo_h753::paced_board::{self, Capture, Dwt};
use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, image, led, stack, wait_since};

#[cfg(feature = "rom-qspi")]
use gbc_firmware::rom_image::ImageHeader;
#[cfg(feature = "display")]
use gbc_nucleo_h753::display::measure;
#[cfg(feature = "display")]
use gbc_nucleo_h753::pac::interrupt;
#[cfg(feature = "rom-qspi")]
use gbc_nucleo_h753::qspi;

const PREFIX: &str = "gbc-nucleo-h753";
// `display` and `rom-qspi` each name the image; build.sh never combines them
// (review Q10), and both together would define IMAGE twice.
#[cfg(all(feature = "rom-qspi", feature = "display"))]
compile_error!("features `rom-qspi` and `display` are mutually exclusive (see build.sh)");
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

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        const TWICE: &str = "buffers taken twice";
        Bufs {
            // `.data` (`Memory::new()` is not all zeros): in DTCM in this
            // placement, copied from flash by the startup code.
            mem: gbc_nucleo_h753::take_once!(Memory = Memory::new()).expect(TWICE),
            // SAFETY (the four `link_section`s): `.axisram` is the NOLOAD
            // output section of AXI SRAM that memory.x
            // (memory_x::firmware_memory_x) defines for such buffers. It
            // is not zeroed at boot, which is sound for these types
            // (integers and structs of integers: every bit pattern is a
            // value); `headless::run` fills the frame and cartridge RAM
            // before it reads them, the audio buffer is written before it
            // is read, and the capture is read only up to what was
            // written.
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
        }
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

fn report(board: &mut board::Board, n: u32, run: &Run, cap: &Capture<CAPTURE_BYTES>) {
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
    paced_board::send_lines(out, cap.bytes());
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
fn qspi_error(board: &mut board::Board, stage: &str, error: &dyn core::fmt::Debug) -> ! {
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

/// Maps the NOR and validates the image in it ([`qspi::map_rom_image`]).
/// The returned [`qspi::RomImage`] must outlive every use of the ROM slice,
/// which `main` guarantees by never returning.
#[cfg(feature = "rom-qspi")]
fn map_qspi(board: &mut board::Board) -> (qspi::RomImage, QspiRom) {
    match qspi::map_rom_image() {
        Ok(image) => {
            let info = QspiRom {
                header: image.header,
                validate_cycles: image.validate_cycles,
                qspi_hz: image.mapped.clock_hz(),
            };
            info.print(board);
            (image, info)
        }
        Err(e) => qspi_error(board, e.stage(), e.error()),
    }
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
    let (nor_image, qspi_rom) = map_qspi(&mut board);
    #[cfg(feature = "rom-qspi")]
    let rom = nor_image.rom();

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
        live: None,
    };
    let bufs = Buffers {
        mem: b.mem,
        save_ram: &mut b.cart_ram[..],
        frame: b.frame,
        audio: &mut b.audio[..],
    };
    #[cfg(feature = "display")]
    let mut display = measure::init();
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

/// The TE interrupt for the measurement sink (`display` feature; src/lib.rs
/// table): the image defines it, the sink is `display::measure`.
#[cfg(feature = "display")]
#[interrupt]
fn EXTI15_10() {
    gbc_nucleo_h753::lcd_dma::on_te();
}
