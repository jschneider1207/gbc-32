//! The headless loop on QEMU `mps2-an500` (plan Stage 2 M1, gate H6): the
//! same `gbc_firmware::headless::run` call as gbc-nucleo-h753's `headless`
//! image, built for the same target and placement (build.rs), with no
//! board. Its `headless ` lines must equal the host tool's
//! (`tools/headless/expected/<rom>.txt`); `run.sh` compares them.
//!
//! QEMU's board is not the NUCLEO: no STM32 power, clock, UART or MPU, and
//! no cycle counter (QEMU does not model DWT_CYCCNT). So this image does
//! only what the emulation needs: the TCM copy, then the run, its lines
//! written live through semihosting (QEMU prints them on its stdout), then
//! SYS_EXIT. The cycle clock reads 0 with `hz` 0, as gbc-firmware's host
//! tests do, so the `stats` lines carry no timing and no `realtime` line.
//!
//! Exit status: 0 once the run returned, whatever it printed (a
//! `headless error` line is a difference, for run.sh to report); 1 on a
//! panic or a bad input program (a tool error).
#![no_std]
#![no_main]

use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::sync::atomic::{AtomicBool, Ordering};

use cortex_m_semihosting::{debug, hio};
use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};
use gbc_firmware::CycleClock;
use gbc_firmware::headless::{self, Buffers, MIN_AUDIO_SLOTS};
use gbc_firmware::input::{self, Press, Script};

/// The ROM, embedded by build.rs from `GBC_HEADLESS_ROM`.
static ROM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/headless.gb"));
/// The ROM's file stem, for the stderr lines.
const ROM_NAME: &str = env!("GBC_HEADLESS_ROM_NAME");
/// The input program from `GBC_HEADLESS_INPUT` (empty: no input).
const INPUT: &str = include_str!(concat!(env!("OUT_DIR"), "/headless-input.txt"));
/// `GBC_HEADLESS_FRAMES`, or empty.
const FRAMES: &str = env!("GBC_HEADLESS_FRAMES");
/// As gbc-nucleo-h753's `headless` image.
const DEFAULT_FRAMES: u32 = 600;
const MAX_PRESSES: usize = 256;
const CART_RAM_BYTES: usize = 128 * 1024;

/// The statics lent to gbc-firmware, placed as on the board: `Memory` in
/// `.data` (the `RAM` region), the rest in `.axisram`.
mod buffers {
    use super::*;

    pub struct Bufs {
        pub mem: &'static mut Memory,
        pub frame: &'static mut Frame,
        pub audio: &'static mut [StereoSample; MIN_AUDIO_SLOTS],
        pub cart_ram: &'static mut [u8; CART_RAM_BYTES],
    }

    struct Slot<T>(UnsafeCell<T>);

    // SAFETY: the slots are private to this module and only reachable
    // through `take`, which hands out one reference to each for the life
    // of the program (guarded by `TAKEN`).
    unsafe impl<T: Send> Sync for Slot<T> {}

    static MEM: Slot<Memory> = Slot(UnsafeCell::new(Memory::new()));
    // SAFETY (the three `link_section`s): `.axisram` is the NOLOAD output
    // section that memory.x (memory_x::memory_x) defines for such buffers.
    // It is not zeroed at boot, which is sound for these types (integers
    // and structs of integers: every bit pattern is a value), and
    // `headless::run` fills the frame and cartridge RAM before it reads
    // them and writes the audio buffer before reading it.
    #[unsafe(link_section = ".axisram.frame")]
    static FRAME: Slot<Frame> = Slot(UnsafeCell::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]));
    #[unsafe(link_section = ".axisram.audio")]
    static AUDIO: Slot<[StereoSample; MIN_AUDIO_SLOTS]> = Slot(UnsafeCell::new(
        [StereoSample { left: 0, right: 0 }; MIN_AUDIO_SLOTS],
    ));
    #[unsafe(link_section = ".axisram.cart_ram")]
    static CART_RAM: Slot<[u8; CART_RAM_BYTES]> = Slot(UnsafeCell::new([0; CART_RAM_BYTES]));

    static TAKEN: AtomicBool = AtomicBool::new(false);

    /// The buffers. Panics on a second call.
    pub fn take() -> Bufs {
        assert!(!TAKEN.swap(true, Ordering::AcqRel), "buffers taken twice");
        // SAFETY: the swap above lets exactly one caller through, ever, so
        // these are the only references to the four statics.
        unsafe {
            Bufs {
                mem: &mut *MEM.0.get(),
                frame: &mut *FRAME.0.get(),
                audio: &mut *AUDIO.0.get(),
                cart_ram: &mut *CART_RAM.0.get(),
            }
        }
    }
}

/// Copies a load image from flash to RAM, word by word; returns its size
/// in bytes.
///
/// # Safety
/// `[dst, end)` must be a RAM range the linker script reserved for this
/// image, 4-byte aligned, that nothing reads or executes yet, and `src`
/// its load address.
unsafe fn copy_image(mut dst: *mut u32, end: *const u32, mut src: *const u32) -> usize {
    let mut n = 0;
    while dst.cast_const() < end {
        // SAFETY: by the caller's contract both pointers are inside their
        // ranges. Volatile, so the loop is not turned into a `memcpy` call
        // (which might itself be in the range being filled).
        unsafe {
            dst.write_volatile(src.read_volatile());
            dst = dst.add(1);
            src = src.add(1);
        }
        n += 4;
    }
    n
}

/// Fills `.itcm` (the stand-in ITCM) and `.dtcm_rodata` from their load
/// images, as gbc-nucleo-h753's `tcm::place` does on the board (QEMU's
/// "ITCM" is plain RAM: no ITCMCR to set). Returns (code, table) bytes.
fn place() -> (usize, usize) {
    unsafe extern "C" {
        static mut __sitcm: u32;
        static __eitcm: u32;
        static __siitcm: u32;
        static mut __sdtcm_ro: u32;
        static __edtcm_ro: u32;
        static __sidtcm_ro: u32;
    }
    // SAFETY: the symbols delimit `.dtcm_rodata` and `.itcm` and their
    // load images (memory_x's memory.x: 4-byte aligned, in RAM). They hold
    // only gbc-core code and tables, and no gbc-core code has run yet.
    unsafe {
        let tables = copy_image(
            &raw mut __sdtcm_ro,
            &raw const __edtcm_ro,
            &raw const __sidtcm_ro,
        );
        let code = copy_image(&raw mut __sitcm, &raw const __eitcm, &raw const __siitcm);
        // The copied code must be visible to instruction fetch.
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        (code, tables)
    }
}

/// QEMU has no cycle counter: every reading is 0, `hz` 0.
struct NoClock;

impl CycleClock for NoClock {
    fn now(&self) -> u32 {
        0
    }

    fn hz(&self) -> u32 {
        0
    }
}

/// Semihosting stdout, one SYS_WRITE per line rather than per `write_str`
/// (each call is a trap into QEMU).
struct LineOut {
    out: hio::HostStream,
    buf: [u8; 256],
    len: usize,
}

impl LineOut {
    fn flush(&mut self) -> fmt::Result {
        let r = self.out.write_all(&self.buf[..self.len]);
        self.len = 0;
        r.map_err(|()| fmt::Error)
    }
}

impl Write for LineOut {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &b in s.as_bytes() {
            if self.len == self.buf.len() {
                self.flush()?;
            }
            self.buf[self.len] = b;
            self.len += 1;
            if b == b'\n' {
                self.flush()?;
            }
        }
        Ok(())
    }
}

/// Writes one line to semihosting stderr (not compared).
fn note(args: fmt::Arguments<'_>) {
    if let Ok(mut err) = hio::hstderr() {
        let _ = err.write_fmt(args);
    }
}

fn fail() -> ! {
    debug::exit(debug::EXIT_FAILURE);
    loop {
        cortex_m::asm::nop();
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let (code, tables) = place();
    note(format_args!(
        "headless-qemu rom={ROM_NAME} rom_bytes={} itcm={code} dtcm_ro={tables}\n",
        ROM.len()
    ));

    let mut presses = [Press::NONE; MAX_PRESSES];
    let (count, end) = match input::parse(INPUT, &mut presses) {
        Ok(p) => (p.presses, p.end),
        Err(e) => {
            note(format_args!(
                "headless-qemu input error line={} kind={:?}\n",
                e.line, e.kind
            ));
            fail();
        }
    };
    let frames = FRAMES.parse().ok().or(end).unwrap_or(DEFAULT_FRAMES);

    let Ok(out) = hio::hstdout() else { fail() };
    let mut out = LineOut {
        out,
        buf: [0; 256],
        len: 0,
    };
    let b = buffers::take();
    let bufs = Buffers {
        mem: b.mem,
        save_ram: &mut b.cart_ram[..],
        frame: b.frame,
        audio: &mut b.audio[..],
    };
    // The result is in the lines (`headless error ...`, or the lines
    // stopping short); run.sh compares those. A write error is a tool
    // error.
    let r = headless::run(
        ROM,
        Script::new(&presses[..count]),
        frames,
        bufs,
        &NoClock,
        &mut out,
    );
    if out.flush().is_err() || matches!(r, Err(headless::Error::Write)) {
        fail();
    }
    debug::exit(debug::EXIT_SUCCESS);
    loop {
        cortex_m::asm::nop();
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo<'_>) -> ! {
    note(format_args!("headless-qemu panic: {info}\n"));
    fail();
}
