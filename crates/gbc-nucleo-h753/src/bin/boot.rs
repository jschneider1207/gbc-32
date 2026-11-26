//! The `boot` image: the lib's bring-up and nothing else (plan Stage 2
//! M1). It measures the clock against the LSE, then prints, once a second,
//! the cycle count of a constant delay loop, and every 8 lines the banner
//! and a TCM/AXI probe line (repeated, so a port opened late sees them). Every line starts with `gbc-nucleo-h753`.
#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::{AtomicU32, Ordering};

use gbc_nucleo_h753::{self as board, CPU_HZ, cycles, led, stack, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "boot";
/// Argument of the reference `cortex_m::asm::delay`.
const DELAY: u32 = 8_000_000;
/// The pattern the probe writes.
const PROBE: u32 = 0x5A17_C0DE;

/// A word in `.bss`, which this image's memory.x puts in DTCM.
static DTCM_WORD: AtomicU32 = AtomicU32::new(0);
/// A word in `.axisram` (AXI SRAM, write-back cached through the MPU).
// SAFETY: `.axisram` is a NOLOAD output section of AXI SRAM that memory.x
// defines for such statics. It is not zeroed at boot, which is sound for
// an atomic integer (every bit pattern is a value).
#[unsafe(link_section = ".axisram.probe")]
static AXI_WORD: AtomicU32 = AtomicU32::new(0);

/// Writes a pattern to the top of ITCM, to [`DTCM_WORD`] and to
/// [`AXI_WORD`] and reads each back; all three should be [`PROBE`]. The
/// ITCM word only when nothing was placed in ITCM (this image has no ITCM
/// set); otherwise it reports 0.
fn probe(board: &board::Board) -> (u32, u32, u32) {
    const ITCM_TOP: *mut u32 = 0x0000_FFF0 as *mut u32;
    let itcm = if board.placed().0 == 0 {
        // SAFETY: the address is inside the 64 KB ITCM (RM0433 Table 7),
        // mapped read/write by the MPU; with nothing placed in ITCM no
        // code or data lives there, so the write disturbs nothing.
        unsafe {
            ITCM_TOP.write_volatile(PROBE);
            ITCM_TOP.read_volatile()
        }
    } else {
        0
    };
    DTCM_WORD.store(!PROBE, Ordering::Relaxed);
    AXI_WORD.store(PROBE.rotate_left(8), Ordering::Relaxed);
    (
        itcm,
        !DTCM_WORD.load(Ordering::Relaxed),
        AXI_WORD.load(Ordering::Relaxed).rotate_right(8),
    )
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let _ = writeln!(
        board.uart,
        "\r\n{PREFIX} {IMAGE} measuring the clock against the LSE\r"
    );
    board.measure_clock();
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(8) {
            board.banner(IMAGE);
            let (i, d, a) = probe(&board);
            let _ = writeln!(
                board.uart,
                "{PREFIX} tcmprobe want={PROBE:#x} itcm={i:#x} dtcm={d:#x} axi={a:#x} \
                 dtcm_at={:#010x} axi_at={:#010x}\r",
                DTCM_WORD.as_ptr() as usize,
                AXI_WORD.as_ptr() as usize
            );
        }
        // A known loop, timed by the cycle counter. The count must be the
        // same on every line (no interrupt is enabled) and of the order of
        // DELAY: `asm::delay` runs DELAY/2 iterations of a two-instruction
        // loop, which the M7 executes in 1 to 3 cycles each.
        let t0 = cycles();
        cortex_m::asm::delay(DELAY);
        let dt = cycles().wrapping_sub(t0);
        let _ = writeln!(
            board.uart,
            "{PREFIX} selftest n={n} delay={DELAY} cycles={dt} cyccnt={} stack_used={}\r",
            cycles(),
            stack::usage().0
        );
        // One line and one LD1 toggle per CPU_HZ cycles.
        wait_since(t0, CPU_HZ);
        led::toggle();
        n += 1;
    }
}
