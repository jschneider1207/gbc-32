//! STM32H7 Nucleo measurement spike (NUCLEO-H753ZI or NUCLEO-H755ZI-Q):
//! Cortex-M7 cycles per emulated frame
//! for the bench workloads, printed on the ST-LINK virtual COM port. See
//! README.md for the board facts and their sources.
//!
//! Three images from this one crate:
//! - default: the measurement (`bench`);
//! - `--features selftest`: board bring-up only, no emulator;
//! - `--features qemu`: the same frame loop on QEMU's mps2-an500, to check
//!   the hashes of a thumbv7em build without hardware.
#![no_std]
#![no_main]

// The selftest image has no emulator; this keeps one dependency list.
#[cfg(feature = "selftest")]
use gbc_core as _;
// The QEMU image has no board code, but cortex-m-rt (with the `device`
// feature the PAC turns on) still wants the PAC's interrupt vector table.
#[cfg(feature = "qemu")]
use stm32h7 as _;

#[cfg(not(feature = "qemu"))]
mod board;
mod stack;
mod tcm;
#[cfg(not(feature = "selftest"))]
mod workload;

#[cfg(not(feature = "selftest"))]
mod bench {
    use gbc_core::{Frame, Memory, StereoSample};

    use crate::workload::{self, AUDIO_SLOTS, CART_RAM_SLOTS, Error, Stats, WORKLOADS};

    /// The ROM images, in flash (`.rodata`), in [`WORKLOADS`] order.
    /// Written to `OUT_DIR` by build.rs.
    static ROMS: [&[u8]; WORKLOADS.len()] = [
        include_bytes!(concat!(env!("OUT_DIR"), "/worst-case.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/cgb-raster.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/dmg-raster.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/event-storm.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/cpu_instrs.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/dma-storm.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/halt-irq.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/apu-writes.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/oam-lines.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-control.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-apu.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-spsw.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-div.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-gdma.gb")),
        include_bytes!(concat!(env!("OUT_DIR"), "/storm-ppu.gb")),
    ];

    /// The emulator's buffers. `mem` is a `.bss` static (DTCM or AXI SRAM,
    /// by placement); the others are in `.axisram`.
    pub struct Buffers {
        pub mem: &'static mut Memory,
        pub frame: &'static mut Frame,
        pub audio: &'static mut [StereoSample; AUDIO_SLOTS],
        pub cart_ram: &'static mut [u8; CART_RAM_SLOTS],
    }

    #[cfg(not(feature = "qemu"))]
    pub type Results = [Option<Result<Stats, Error>>; WORKLOADS.len()];

    /// Runs workload `i`. `now` and `between` as in [`workload::run`].
    pub fn run(
        i: usize,
        b: &mut Buffers,
        now: impl FnMut() -> u32,
        between: impl FnMut(u32),
    ) -> Result<Stats, Error> {
        workload::run(
            ROMS[i],
            b.cart_ram,
            b.mem,
            b.frame,
            b.audio,
            WORKLOADS[i].calls,
            now,
            between,
        )
    }
}

/// The emulator's large buffers, as statics handed out once.
#[cfg(not(feature = "selftest"))]
mod buffers {
    use core::cell::UnsafeCell;
    use core::sync::atomic::{AtomicBool, Ordering};

    use gbc_core::{Frame, Memory, SCREEN_HEIGHT, SCREEN_WIDTH, StereoSample};

    use crate::bench::Buffers;
    use crate::workload::{AUDIO_SLOTS, CART_RAM_SLOTS};

    struct Cell<T>(UnsafeCell<T>);

    // SAFETY: the cells are private to this module and only reachable
    // through `take`, which gives out one reference to each for the life
    // of the program (guarded by the atomic `TAKEN`), so no two contexts
    // can ever access one.
    unsafe impl<T: Send> Sync for Cell<T> {}

    /// `.bss`: DTCM or AXI SRAM, by placement.
    static MEM: Cell<Memory> = Cell(UnsafeCell::new(Memory::new()));
    // SAFETY (the three `link_section`s): `.axisram` is a NOLOAD output
    // section of RAM that build.rs's memory.x defines for exactly these
    // statics. It is not zeroed at boot, which is sound for these types
    // (plain integers: every bit pattern is a value), and `workload::run`
    // fills all three before it reads them.
    #[unsafe(link_section = ".axisram.frame")]
    static FRAME: Cell<Frame> = Cell(UnsafeCell::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]));
    #[unsafe(link_section = ".axisram.audio")]
    static AUDIO: Cell<[StereoSample; AUDIO_SLOTS]> = Cell(UnsafeCell::new(
        [StereoSample { left: 0, right: 0 }; AUDIO_SLOTS],
    ));
    #[unsafe(link_section = ".axisram.cart_ram")]
    static CART_RAM: Cell<[u8; CART_RAM_SLOTS]> = Cell(UnsafeCell::new([0; CART_RAM_SLOTS]));

    static TAKEN: AtomicBool = AtomicBool::new(false);

    /// The buffers. Panics on a second call.
    pub fn take() -> Buffers {
        assert!(!TAKEN.swap(true, Ordering::AcqRel), "buffers taken twice");
        // SAFETY: the swap above lets exactly one caller through, ever, so
        // these are the only references to the four statics.
        unsafe {
            Buffers {
                mem: &mut *MEM.0.get(),
                frame: &mut *FRAME.0.get(),
                audio: &mut *AUDIO.0.get(),
                cart_ram: &mut *CART_RAM.0.get(),
            }
        }
    }
}

#[cfg(not(feature = "selftest"))]
#[cortex_m_rt::entry]
fn main() -> ! {
    run(buffers::take())
}

/// The measurement: every workload once, then the report forever.
#[cfg(all(not(feature = "selftest"), not(feature = "qemu")))]
fn run(mut b: bench::Buffers) -> ! {
    use core::fmt::Write;

    let mut board = board::init();
    board.banner("bench");
    let mut results: bench::Results = [None; workload::WORKLOADS.len()];
    for (i, w) in workload::WORKLOADS.iter().enumerate() {
        let _ = writeln!(
            board.uart,
            "h7bench v1 start {} calls={}\r",
            w.name, w.calls
        );
        // LD1 toggles every 8 calls, outside the measured window.
        let r = bench::run(i, &mut b, board::cycles, |n| {
            if n.is_multiple_of(8) {
                board::led_toggle();
            }
        });
        workload::report_line(&mut board.uart, i, &r);
        results[i] = Some(r);
    }
    // Deepest stack use over the whole run (the report loop below is
    // shallower than the workloads).
    let (used, size) = stack::usage();
    // Repeat the whole report forever, so a port opened late still sees it.
    let mut pass = 1u32;
    loop {
        if pass == 1 {
            let _ = writeln!(board.uart, "h7bench v1 stack used={used} of={size}\r");
        }
        let _ = writeln!(board.uart, "h7bench v1 end pass={pass}\r");
        // About 3 s, LD1 blinking at 2 Hz.
        for _ in 0..12 {
            board::wait_since(board::cycles(), board::CPU_HZ / 4);
            board::led_toggle();
        }
        pass += 1;
        board.banner("bench");
        for (i, r) in results.iter().enumerate() {
            if let Some(r) = r {
                workload::report_line(&mut board.uart, i, r);
            }
        }
        let _ = writeln!(board.uart, "h7bench v1 stack used={used} of={size}\r");
    }
}

/// QEMU smoke run: the same calls, printed through semihosting. QEMU does
/// not model the cycle counter: the cycle fields are 0, as on the host, and
/// only calls/frames/clocks/hash are compared.
#[cfg(feature = "qemu")]
fn run(mut b: bench::Buffers) -> ! {
    use cortex_m_semihosting::{debug, hio};

    let _ = tcm::place();
    if let Ok(mut out) = hio::hstdout() {
        for i in 0..workload::WORKLOADS.len() {
            let r = bench::run(i, &mut b, || 0, |_| ());
            workload::report_line(&mut out, i, &r);
        }
    }
    // On stderr: stdout is compared with expected-hashes.txt.
    if let Ok(mut err) = hio::hstderr() {
        use core::fmt::Write;
        let (used, size) = stack::usage();
        let _ = writeln!(err, "h7bench v1 stack used={used} of={size}");
    }
    debug::exit(debug::EXIT_SUCCESS);
    loop {
        cortex_m::asm::nop();
    }
}

#[cfg(feature = "qemu")]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    cortex_m_semihosting::debug::exit(cortex_m_semihosting::debug::EXIT_FAILURE);
    loop {
        cortex_m::asm::nop();
    }
}

/// Bring-up only: the banner, then one line a second with the cycle count
/// of a known delay loop.
#[cfg(feature = "selftest")]
#[cortex_m_rt::entry]
fn main() -> ! {
    use core::fmt::Write;

    /// Argument of the reference `cortex_m::asm::delay`.
    const DELAY: u32 = 8_000_000;

    let mut board = board::init();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(8) {
            board.banner("selftest");
        }
        // A known loop, timed by the cycle counter. The count must be the
        // same on every line (no interrupt is enabled) and of the order of
        // DELAY: `asm::delay` runs DELAY/2 iterations of a two-instruction
        // loop, which the M7 executes in 1 to 3 cycles each.
        if n == 1 {
            let (i, d) = board::tcm_probe();
            let _ = writeln!(
                board.uart,
                "h7bench v1 tcmprobe want={:#x} itcm={i:#x} dtcm={d:#x}\r",
                board::PROBE
            );
        }
        let t0 = board::cycles();
        cortex_m::asm::delay(DELAY);
        let dt = board::cycles().wrapping_sub(t0);
        let _ = writeln!(
            board.uart,
            "h7bench v1 selftest n={n} delay={DELAY} cycles={dt} cyccnt={} stack_used={}\r",
            board::cycles(),
            stack::usage().0
        );
        // One line and one LD1 toggle per CPU_HZ cycles: against a watch
        // this checks the counter's rate (and so the clock).
        board::wait_since(t0, board::CPU_HZ);
        board::led_toggle();
        n += 1;
    }
}
