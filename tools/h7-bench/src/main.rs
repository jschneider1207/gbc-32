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
//! The measurement images run every workload on its ROM in internal flash
//! (the `.rodata` copy) and then, when the QSPI NOR holds a valid bench
//! pack (`pack.rs`, loaded with the gbc-nucleo-h753 loader), once more on
//! the pack's copy read through the memory-mapped window: the
//! `<placement>-qspi` figures (plan Stage 2 M2), from the same binary and
//! boot as the internal-flash ones. Without a pack only the first pass runs.
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
#[cfg(all(feature = "board", not(feature = "selftest")))]
mod pack;
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

    /// Workload `i`'s ROM in internal flash.
    #[cfg(feature = "board")]
    pub fn rom(i: usize) -> &'static [u8] {
        ROMS[i]
    }

    /// Runs workload `i` on its ROM in internal flash. `now` and `between`
    /// as in [`workload::run`].
    #[cfg(feature = "qemu")]
    pub fn run(
        i: usize,
        b: &mut Buffers,
        now: impl FnMut() -> u32,
        between: impl FnMut(u32),
    ) -> Result<Stats, Error> {
        run_on(ROMS[i], i, b, now, between)
    }

    /// Runs workload `i` on `rom` (its bytes, from wherever they are).
    /// The board calls this with the same `now` and `between` types for
    /// every ROM source, so both sources run one instance of
    /// [`workload::run`]: the same code at the same addresses.
    pub fn run_on<N: FnMut() -> u32, B: FnMut(u32)>(
        rom: &[u8],
        i: usize,
        b: &mut Buffers,
        now: N,
        between: B,
    ) -> Result<Stats, Error> {
        workload::run(
            rom,
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

/// The measurement: every workload once on its ROM in internal flash, then
/// once more on the bench pack's copy in the QSPI NOR if there is a valid
/// one, then the report forever.
#[cfg(all(not(feature = "selftest"), feature = "board"))]
fn run(mut b: bench::Buffers) -> ! {
    use core::fmt::Write;

    use workload::WORKLOADS;

    let mut board = board::init(PREFIX);
    // The clock this run actually has, for every banner (plan D11); about
    // 2 s, before the first workload.
    board.measure_clock();
    banner!(board, "bench");
    // Mapped for good when it is a pack: `run` never returns, so the ROM
    // slices into the window stay valid.
    let qspi = qspi_rom::open();
    qspi_rom::print(&mut board.uart, &qspi);
    let qspi_roms = qspi.as_ref().ok().map(qspi_rom::Pack::roms);
    // One pass per ROM source: internal flash, then the QSPI window. Both
    // call `bench::run_on` with the same `now` and `between` (fn items), so
    // they run the same instance of `workload::run`.
    let mut results: [bench::Results; 2] = [[None; WORKLOADS.len()]; 2];
    for (s, source) in ["", "qspi "].into_iter().enumerate() {
        for (i, w) in WORKLOADS.iter().enumerate() {
            let rom = match (s, &qspi_roms) {
                (0, _) => bench::rom(i),
                (_, Some(roms)) => roms[i],
                (_, None) => break,
            };
            let _ = writeln!(
                board.uart,
                "h7bench v1 start {source}{} calls={}\r",
                w.name, w.calls
            );
            let r = bench::run_on(rom, i, &mut b, board::cycles, blink);
            workload::report_line_from(&mut board.uart, source, i, &r);
            results[s][i] = Some(r);
        }
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
        qspi_rom::print(&mut board.uart, &qspi);
        for (source, rs) in ["", "qspi "].into_iter().zip(&results) {
            for (i, r) in rs.iter().enumerate() {
                if let Some(r) = r {
                    workload::report_line_from(&mut board.uart, source, i, r);
                }
            }
        }
        let _ = writeln!(board.uart, "h7bench v1 stack used={used} of={size}\r");
    }
}

/// `between` for [`workload::run`]: LD1 toggles every 8 calls, outside the
/// measured window.
#[cfg(all(not(feature = "selftest"), feature = "board"))]
fn blink(n: u32) {
    if n.is_multiple_of(8) {
        board::led::toggle();
    }
}

/// The bench pack in the QSPI NOR (`pack.rs`): mapped and checked once at
/// boot, before the first workload.
#[cfg(all(not(feature = "selftest"), feature = "board"))]
mod qspi_rom {
    use core::fmt::Write;

    use gbc_firmware::rom_image::{self, HEADER_LEN, ImageError, ImageHeader};
    use gbc_nucleo_h753::qspi::{self, Qspi};
    use gbc_nucleo_h753::{Uart, cycles};

    use crate::pack::{self, PackError};
    use crate::workload::WORKLOADS;

    /// Why there is no QSPI pass.
    #[derive(Debug)]
    pub enum Fail {
        /// The QUADSPI would not enter memory-mapped mode.
        Map(qspi::Error),
        /// No valid ADR 0037 image in the NOR (`stage` header or validate).
        Image(&'static str, ImageError),
        /// A valid image that is not a bench pack.
        Pack(PackError),
    }

    /// A valid pack, mapped for good.
    pub struct Pack {
        mapped: qspi::Mapped,
        header: ImageHeader,
        /// Cycles of `rom_image::validate` (its header checks and the hash
        /// of the whole pack through the cold window) and `pack::parse`.
        validate_cycles: u32,
        /// Every ROM in the pack is byte for byte the one in internal flash.
        same_as_flash: bool,
    }

    impl Pack {
        /// The pack's ROMs, in [`WORKLOADS`] order, in the window.
        pub fn roms(&self) -> [&[u8]; WORKLOADS.len()] {
            let data = &self.mapped.bytes()[HEADER_LEN..HEADER_LEN + self.header.len as usize];
            // `open` parsed these same bytes; the NOR cannot change while
            // `mapped` exists (qspi::Mapped::bytes).
            pack::parse(data).expect("the pack parsed at boot")
        }
    }

    /// Maps the NOR (0xEB at [`qspi::PRESCALER_MMAP`], cached, as the
    /// `headless-qspi` image) and checks the image there and its pack.
    /// On an error after mapping the window stays mapped and unused.
    pub fn open() -> Result<Pack, Fail> {
        let mut q = Qspi::init(qspi::PRESCALER_SAFE);
        q.set_prescaler(qspi::PRESCALER_MMAP).map_err(Fail::Map)?;
        let mapped = q
            .memory_mapped(&qspi::QUAD_IO_READ_XIP)
            .map_err(|(_, e)| Fail::Map(e))?;
        let window = mapped.bytes();
        let t0 = cycles();
        let header = rom_image::read_header(window).map_err(|e| Fail::Image("header", e))?;
        let data = rom_image::validate(window).map_err(|e| Fail::Image("validate", e))?;
        let roms = pack::parse(data).map_err(Fail::Pack)?;
        let validate_cycles = cycles().wrapping_sub(t0);
        let same_as_flash = roms
            .iter()
            .enumerate()
            .all(|(i, r)| *r == crate::bench::rom(i));
        Ok(Pack {
            header,
            validate_cycles,
            same_as_flash,
            mapped,
        })
    }

    /// The `qspi pack` line: `ok` with what was found, or `none` with why.
    pub fn print(out: &mut Uart, p: &Result<Pack, Fail>) {
        // The UART writer never fails.
        let _ = match p {
            Ok(p) => writeln!(
                out,
                "h7bench v1 qspi pack ok len={} hash={:016x} roms={} same_as_flash={} \
                 validate_cycles={} qspi_hz={} window=cached exit_fallbacks={}\r",
                p.header.len,
                p.header.hash,
                WORKLOADS.len(),
                u8::from(p.same_as_flash),
                p.validate_cycles,
                p.mapped.clock_hz(),
                qspi::exit_fallbacks()
            ),
            Err(Fail::Map(e)) => writeln!(out, "h7bench v1 qspi pack none stage=map error={e:?}\r"),
            Err(Fail::Image(stage, e)) => {
                writeln!(out, "h7bench v1 qspi pack none stage={stage} error={e:?}\r")
            }
            Err(Fail::Pack(e)) => {
                writeln!(out, "h7bench v1 qspi pack none stage=pack error={e:?}\r")
            }
        };
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
    let delay = ItcmDelay::place();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(8) {
            banner!(board, "selftest");
        }
        // The TCM probe after every banner, so a port opened late sees it.
        if n % 8 == 1 {
            let (i, d) = tcm_probe(&board);
            let _ = writeln!(
                board.uart,
                "h7bench v1 tcmprobe want={PROBE:#x} itcm={i:#x} dtcm={d:#x}\r"
            );
        }
        // A known loop, timed by the cycle counter. The count must be the
        // same on every line (no interrupt is enabled) and of the order of
        // DELAY: `asm::delay`'s loop of DELAY + 1 iterations of `subs` and
        // `bne`, which the M7 runs in one cycle each. From ITCM, so the
        // count cannot depend on the I-cache (see [`ItcmDelay`]).
        let t0 = board::cycles();
        let dt = delay.run(DELAY);
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

/// The top word of ITCM, which [`tcm_probe`] writes.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
const ITCM_TOP: *mut u32 = 0x0000_FFF0 as *mut u32;

/// The DWT cycle counter, DWT_CYCCNT (Armv7-M ARM C1.8.7; the lib's
/// `cycles` reads it).
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
const CYCCNT: *const u32 = 0xE000_1004 as *const u32;

// The `selftest` line's timed loop (see [`ItcmDelay`]):
// `h7bench_itcm_delay(iterations: r0, cyccnt: r1) -> cycles: r0` waits for
// every earlier memory access to complete and refetches (`dsb`, `isb`),
// reads the cycle counter, runs `cortex_m::asm::delay`'s loop (`subs` and
// `bne` on an 8-byte boundary, `iterations` times), reads the counter
// again and returns the difference. Position independent (no literal pool, no
// absolute address), so a copy runs anywhere; the labels are plain
// (not `.thumb_func`), so their addresses are the code's bytes, without
// the Thumb bit.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
core::arch::global_asm!(
    ".section .text.h7bench_itcm_delay,\"ax\",%progbits",
    ".p2align 3",
    ".global h7bench_itcm_delay_start",
    ".global h7bench_itcm_delay_end",
    "h7bench_itcm_delay_start:",
    "    dsb",
    "    isb",
    "    ldr r2, [r1]",
    "    .p2align 3",
    "2:",
    "    subs r0, #1",
    "    bne 2b",
    "    ldr r3, [r1]",
    "    subs r0, r3, r2",
    "    bx lr",
    "h7bench_itcm_delay_end:",
);

/// The timed loop of the `selftest` line, copied from flash to ITCM.
///
/// From flash, the count depended on the I-cache (gate B1, 2026-11-26):
/// the loop, both counter reads and the instructions between them were one
/// 32-byte line, 0x0800_0420 in both builds, and the I-cache (16 KiB, two
/// ways of 256 sets of 32 B, CCSIDR 0xf01fe009 read on the board: index
/// bits 12:5, a way every 8 KiB) gives that line one set with 0x0800_2420
/// and 0x0800_4420. The fault handler grown in d8c8588 moved
/// `<u32 as Display>::fmt`, run for every line, onto 0x0800_4420, beside
/// `Board::banner` on 0x0800_2420: three lines for two ways, so after each
/// banner the timed line was refetched from flash for one to three lines,
/// and those counts were 1 to 3 cycles high (8,000,020-22 for 8,000,019).
/// With the I-cache invalidated before every count they were 8,000,010 and
/// 8,000,024: a line fill's timing varies too. ITCM is not cached and has
/// no wait states (DUI 0646 "Cache"), so the count no longer depends on
/// what else ran or where the linker put it.
///
/// From ITCM without the `dsb`/`isb`, every even `n` but the banner's
/// (`n % 8 == 0`) read 8,000,008 instead of 8,000,019: the LD1 toggle's
/// GPIOB_BSRR write, just before the count, was still in flight when the
/// first counter read ran (without the toggle: 8,000,019 on every line).
/// The barrier completes it before the first read, so the window holds
/// the loop alone.
#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
struct ItcmDelay(unsafe extern "C" fn(u32, *const u32) -> u32);

#[cfg(all(feature = "selftest", not(feature = "clockcheck")))]
impl ItcmDelay {
    /// Copies the loop to the first free 8-byte boundary of ITCM, behind
    /// whatever the lib placed there (nothing, in this image).
    fn place() -> ItcmDelay {
        unsafe extern "C" {
            static h7bench_itcm_delay_start: u16;
            static h7bench_itcm_delay_end: u16;
            /// The lib's ITCM copy ends here (memory.x; ORIGIN(ITCM) when
            /// nothing is placed).
            static __eitcm: u16;
        }
        let src = &raw const h7bench_itcm_delay_start;
        let len = (&raw const h7bench_itcm_delay_end) as usize - src as usize;
        let dst = ((&raw const __eitcm) as usize).next_multiple_of(8) as *mut u16;
        // The loop is a few halfwords; it must stay clear of the word
        // `tcm_probe` writes.
        assert!(len > 0 && len <= 64 && len.is_multiple_of(2));
        assert!(dst as usize >= 0x100 && dst as usize + len <= ITCM_TOP as usize);
        for i in 0..len / 2 {
            // SAFETY: `src` .. `src + len` is the assembled loop in flash
            // (its two labels); `dst` .. `dst + len` is inside the ITCM
            // (checked above), behind the lib's ITCM code and below the
            // probe word, so it holds nothing else, and the MPU maps ITCM
            // read/write and executable (the lib's region 1).
            unsafe { dst.add(i).write_volatile(src.add(i).read_volatile()) };
        }
        // Complete the stores before the instruction side fetches them
        // (Armv7-M ARM A3.7.3), as the lib's `tcm::place` does.
        cortex_m::asm::dsb();
        cortex_m::asm::isb();
        // SAFETY: `dst` now holds the routine above, a Thumb function with
        // the AAPCS signature `(u32, *const u32) -> u32`; bit 0 selects
        // Thumb state for the branch to it.
        ItcmDelay(unsafe {
            core::mem::transmute::<usize, unsafe extern "C" fn(u32, *const u32) -> u32>(
                dst as usize | 1,
            )
        })
    }

    /// The cycles of `cortex_m::asm::delay(delay)`'s loop: `delay + 1`
    /// iterations, as `asm::delay` runs (cortex-m 0.7.10), timed from
    /// inside ITCM.
    fn run(&self, delay: u32) -> u32 {
        // SAFETY: the routine only reads the word its second argument
        // points to, here the cycle counter, and clobbers only r0-r3 and
        // the flags, which the AAPCS lets a callee clobber.
        unsafe { (self.0)(delay.saturating_add(1), CYCCNT) }
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
        // placed in ITCM no code or data lives there but `ItcmDelay`'s
        // loop at its bottom, which ends below this word (`place`
        // asserts it), so the write disturbs nothing.
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
