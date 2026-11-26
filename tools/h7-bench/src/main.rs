//! NUCLEO-H753ZI measurement spike: Cortex-M7 cycles per emulated frame
//! for the bench workloads, printed on the ST-LINK virtual COM port. See
//! README.md for the board facts and their sources.
//!
//! Four images from this one crate:
//! - default: the measurement (`bench`);
//! - `--features selftest`: board bring-up only, no emulator;
//! - `--features clockcheck`: the selftest's bring-up, then the CPU clock
//!   measured against the LSE crystal (no emulator);
//! - `--no-default-features --features qemu`: the same frame loop on
//!   QEMU's mps2-an500, to check the hashes of a thumbv7em build without
//!   hardware.
//!
//! The board images' bring-up, banner registers and fault handlers are the
//! gbc-nucleo-h753 lib's (ADR 0036), so they cannot drift from the
//! firmware's; this crate keeps the workloads, its own lines and the TCM
//! probe.
#![no_std]
#![no_main]

// The selftest image has no emulator; this keeps one dependency list.
#[cfg(feature = "selftest")]
use gbc_core as _;
#[cfg(feature = "board")]
use gbc_nucleo_h753 as board;

#[cfg(feature = "qemu")]
mod qemu;
#[cfg(feature = "qemu")]
use qemu::run;
#[cfg(not(feature = "selftest"))]
mod workload;

/// The start of every line, the lib's banner and fault report included
/// (`h7bench v1 FAULT ...`).
#[cfg(feature = "board")]
const PREFIX: &str = "h7bench v1";

/// The placement's name, for the banner (a macro, for `concat!`).
#[cfg(all(feature = "board", feature = "itcm"))]
macro_rules! placement {
    () => {
        "itcm-dtcm"
    };
}
#[cfg(all(feature = "board", feature = "dtcm", not(feature = "itcm")))]
macro_rules! placement {
    () => {
        "flash-dtcm"
    };
}
#[cfg(all(feature = "board", not(feature = "dtcm")))]
macro_rules! placement {
    () => {
        "flash-axi"
    };
}

/// The lib's banner, with `banner image=<image> placement=<placement>` in
/// the place of its image name, so the first line still carries the keys
/// it had before the lib (`h7bench v1 banner image=bench
/// placement=itcm-dtcm board=... clock=pll472 cpu_hz=... baud=...`).
#[cfg(feature = "board")]
macro_rules! banner {
    ($board:expr, $image:literal) => {
        $board.banner(concat!(
            "banner image=",
            $image,
            " placement=",
            placement!()
        ))
    };
}

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

    #[cfg(feature = "board")]
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
#[cfg(all(not(feature = "selftest"), feature = "board"))]
fn run(mut b: bench::Buffers) -> ! {
    use core::fmt::Write;

    let mut board = board::init(PREFIX);
    // The clock this run actually has, for every banner (plan D11); about
    // 2 s, before the first workload.
    board.measure_clock();
    banner!(board, "bench");
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
                board::led::toggle();
            }
        });
        workload::report_line(&mut board.uart, i, &r);
        results[i] = Some(r);
    }
    // Deepest stack use over the whole run (the report loop below is
    // shallower than the workloads).
    let (used, size) = board::stack::usage();
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
            board::led::toggle();
        }
        pass += 1;
        banner!(board, "bench");
        for (i, r) in results.iter().enumerate() {
            if let Some(r) = r {
                workload::report_line(&mut board.uart, i, r);
            }
        }
        let _ = writeln!(board.uart, "h7bench v1 stack used={used} of={size}\r");
    }
}

/// Bring-up only: the banner, then one line a second with the cycle count
/// of a known delay loop.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
#[cortex_m_rt::entry]
fn main() -> ! {
    use core::fmt::Write;

    /// Argument of the reference `cortex_m::asm::delay`.
    const DELAY: u32 = 8_000_000;

    let mut board = board::init(PREFIX);
    board.measure_clock();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(8) {
            banner!(board, "selftest");
        }
        // A known loop, timed by the cycle counter. The count must be the
        // same on every line (no interrupt is enabled) and of the order of
        // DELAY: `asm::delay` runs DELAY/2 iterations of a two-instruction
        // loop, which the M7 executes in 1 to 3 cycles each.
        // The TCM probe after every banner, so a port opened late sees it.
        if n % 8 == 1 {
            let (i, d) = tcm_probe(&board);
            let _ = writeln!(
                board.uart,
                "h7bench v1 tcmprobe want={PROBE:#x} itcm={i:#x} dtcm={d:#x}\r"
            );
        }
        let t0 = board::cycles();
        cortex_m::asm::delay(DELAY);
        let dt = board::cycles().wrapping_sub(t0);
        let _ = writeln!(
            board.uart,
            "h7bench v1 selftest n={n} delay={DELAY} cycles={dt} cyccnt={} stack_used={}\r",
            board::cycles(),
            board::stack::usage().0
        );
        // One line and one LD1 toggle per CPU_HZ cycles: against a watch
        // this checks the counter's rate (and so the clock).
        board::wait_since(t0, board::CPU_HZ);
        board::led::toggle();
        n += 1;
    }
}

/// The pattern [`tcm_probe`] writes.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
const PROBE: u32 = 0x5A17_C0DE;

/// Selftest only: writes a pattern to a word of each TCM that nothing else
/// uses and reads it back. Returns (ITCM word, DTCM word); both should be
/// [`PROBE`], and are 0 where no free word was probed.
///
/// The words: the top of ITCM when the lib placed no code there (the
/// selftest image has no ITCM set); the top of DTCM when the image's RAM
/// (stack and statics) is AXI SRAM, otherwise a `.bss` static, which is
/// then in DTCM. The top of DTCM is the stack in a DTCM placement, so a
/// fixed address there would overwrite a live stack frame. Both TCMs are
/// enabled when this runs (the lib's `tcm::place` sets ITCMCR.EN; DTCM is
/// enabled out of reset, as the banner's `tcm` line shows) and mapped
/// read/write by the lib's MPU.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
fn tcm_probe(board: &board::Board) -> (u32, u32) {
    use core::sync::atomic::{AtomicU32, Ordering};

    const ITCM_TOP: *mut u32 = 0x0000_FFF0 as *mut u32;
    const DTCM_TOP: *mut u32 = 0x2001_FFF0 as *mut u32;
    /// DTCM: RM0433 Table 7.
    const DTCM: core::ops::Range<usize> = 0x2000_0000..0x2002_0000;
    /// In `.bss`, so in DTCM exactly when the RAM is.
    static DTCM_WORD: AtomicU32 = AtomicU32::new(0);
    unsafe extern "C" {
        /// cortex-m-rt: the top of the stack, the end of the RAM region.
        static _stack_start: u32;
    }
    let ram_in_dtcm = DTCM.contains(&((&raw const _stack_start) as usize - 4));

    let itcm = if board.placed().0 == 0 {
        // SAFETY: the address is inside the 64 KB ITCM; with nothing
        // placed in ITCM no code or data lives there, so the write
        // disturbs nothing.
        unsafe {
            ITCM_TOP.write_volatile(PROBE);
            ITCM_TOP.read_volatile()
        }
    } else {
        0
    };
    let dtcm = if ram_in_dtcm {
        DTCM_WORD.store(!PROBE, Ordering::Relaxed);
        !DTCM_WORD.load(Ordering::Relaxed)
    } else {
        // SAFETY: the address is inside the 128 KB DTCM, and the RAM
        // region (stack, `.data`, `.bss`) is not in DTCM (checked above),
        // so no code or data lives there.
        unsafe {
            DTCM_TOP.write_volatile(!PROBE);
            !DTCM_TOP.read_volatile()
        }
    };
    (itcm, dtcm)
}

/// Clock check: the CPU clock measured against the LSE crystal (see
/// the lib's `clockcheck`), one line per 2 s window, with the running average
/// over every window since reset.
#[cfg(feature = "clockcheck")]
#[cortex_m_rt::entry]
fn main() -> ! {
    use core::fmt::Write;

    use board::clockcheck::{self, LSE_HZ, PERIODS, REF, TIM_HZ};

    /// `x` parts per million of `nominal` away from it, in tenths.
    fn ppm10(x: u64, nominal: u32) -> i64 {
        (x as i64 - i64::from(nominal)) * 10_000_000 / i64::from(nominal)
    }
    /// Tenths as a signed decimal.
    struct Tenths(i64);
    impl core::fmt::Display for Tenths {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            let s = if self.0 < 0 { "-" } else { "+" };
            write!(
                f,
                "{s}{}.{}",
                self.0.unsigned_abs() / 10,
                self.0.unsigned_abs() % 10
            )
        }
    }

    let mut board = board::init(PREFIX);
    banner!(board, "clockcheck");
    let Some(waited) = clockcheck::start() else {
        // No reference: say so, never print a measurement.
        loop {
            let _ = writeln!(
                board.uart,
                "h7bench v1 clockcheck lse=FAIL (LSERDY not set within 5 s)\r"
            );
            board::wait_since(board::cycles(), board::CPU_HZ);
            board::led::toggle();
        }
    };
    // Repeated with every banner, so a port opened late still sees it.
    let lse_line = |uart: &mut board::Uart| {
        let _ = writeln!(
            uart,
            "h7bench v1 clockcheck lse=ready waited_cycles={waited} (0: already running) \
             window_periods={PERIODS} lse_hz={LSE_HZ} ref={} ref_hz_nominal={} tim_hz_nominal={TIM_HZ}\r",
            REF.0, REF.1
        );
    };
    lse_line(&mut board.uart);
    let (mut sum_cycles, mut sum_tim, mut sum_periods) = (0u64, 0u64, 0u64);
    let mut n = 0u32;
    loop {
        if n > 0 && n.is_multiple_of(16) {
            banner!(board, "clockcheck");
            lse_line(&mut board.uart);
        }
        let Some(w) = clockcheck::window() else {
            let _ = writeln!(board.uart, "h7bench v1 clockcheck n={n} lse=EDGE_TIMEOUT\r");
            n += 1;
            continue;
        };
        let periods = u64::from(PERIODS);
        let lse = u64::from(LSE_HZ);
        // Each window starts on a fresh edge; the sums are of whole
        // windows, so the average needs no continuity between them.
        sum_cycles += u64::from(w.cycles);
        sum_tim += u64::from(w.tim_clocks);
        sum_periods += periods;
        let cpu = (u64::from(w.cycles) * lse + periods / 2) / periods;
        let avg = (sum_cycles * lse + sum_periods / 2) / sum_periods;
        let tim = (sum_tim * lse + sum_periods / 2) / sum_periods;
        // The PLL multiplies its reference exactly: ref = cpu * REF / CPU_HZ.
        let ref_hz =
            (avg * u64::from(REF.1) + u64::from(board::CPU_HZ) / 2) / u64::from(board::CPU_HZ);
        let _ = writeln!(
            board.uart,
            "h7bench v1 clockcheck n={n} cycles={} cpu_hz_measured={cpu} avg_s={} \
             avg_cpu_hz_measured={avg} cpu_ppm={} {}_hz_implied={ref_hz} tim_hz_measured={tim} \
             tim_ppm={} overcapture={} stack_used={}\r",
            w.cycles,
            sum_periods / lse,
            Tenths(ppm10(avg, board::CPU_HZ)),
            REF.0,
            Tenths(ppm10(tim, TIM_HZ)),
            u8::from(w.overcapture),
            board::stack::usage().0
        );
        board::led::toggle();
        n += 1;
    }
}
