//! QUADSPI in indirect and memory-mapped mode: the X-NUCLEO-GFX02Z1
//! shield's 64-Mbit NOR flash (plan Stage 2 M2). Reads, and in indirect
//! mode only, write enable, page program and 4 KiB / 64 KiB erase (the
//! write side, at the end of this file; [`NorError`]). Never chip erase,
//! never a status or configuration register write. The part, its
//! commands and limits: `docs/reference/qspi-nor.md`.
//!
//! Memory-mapped mode ([`Qspi::memory_mapped`], [`Mapped`]): the NOR
//! appears read-only at [`WINDOW`], [`NOR_BYTES`] long, through MPU region
//! [`crate::mpu::REGION_QSPI`] (Normal, write-back cached, execute-never).
//! The order, both ways, keeps every CPU access (speculative ones too) away
//! from the window while the QUADSPI is not in memory-mapped mode, which
//! RM0433 23.3.7 forbids ("It is forbidden to access the quad-SPI flash
//! bank area before having properly configured and enabled the QUADSPI")
//! and answers with a bus error (23.3.14):
//! - enter: QUADSPI idle, CCR with FMODE = 11 (the controller is now in
//!   memory-mapped mode, nothing is sent until the first access), then the
//!   MPU region on (DSB, ISB);
//! - exit: the MPU region off (DSB, ISB; no access can start any more),
//!   ABORT and BUSY = 0 (23.3.15; the prefetch keeps BUSY high until
//!   then), the ES0392 2.7.4 sequence (required "upon switching from
//!   memory-mapped to any other mode"), CR restored, then a D-cache clean
//!   and invalidate, so no line of the old NOR contents survives a program
//!   or erase in between.
//!
//! Sources: RM0433 Rev 9 chapter 23 (QUADSPI) and section 8.7 (RCC);
//! DS12117 Rev 10 Table 9 (alternate functions); ES0392 Rev 16 section 2.7
//! (QUADSPI errata); UM2905 Rev 2 Table 3 (the shield's NOR wiring for the
//! NUCLEO-H753ZI).
//!
//! Pins (UM2905 Table 3, footnote 2) and their alternate functions
//! (DS12117 Table 9):
//!
//! | NOR pin     | signal         | GPIO | AF   |
//! |-------------|----------------|------|------|
//! | 1 CS#       | QUADSPI_BK1_NCS| PG6  | AF10 |
//! | 6 SCLK      | QUADSPI_CLK    | PB2  | AF9  |
//! | 5 SIO0      | QUADSPI_BK1_IO0| PD11 | AF9  |
//! | 2 SIO1      | QUADSPI_BK1_IO1| PD12 | AF9  |
//! | 3 SIO2      | QUADSPI_BK1_IO2| PE2  | AF9  |
//! | 7 SIO3      | QUADSPI_BK1_IO3| PD13 | AF9  |
//!
//! PE2 is also Zio CN9's SAI_A_MCLK (UM2407): nothing in this firmware
//! uses that. In single-line phases the QUADSPI drives IO2 low and IO3
//! high (RM0433 section 23.3.4), so WP# and HOLD#/RESET# are defined even
//! before quad mode is used.
//!
//! Clock (M6 tuning, `docs/reference/qspi-nor.md` "Clock"): the kernel
//! clock is pll2_r_ck (RCC_D1CCIPR QSPISEL = 10), [`KER_HZ`] = 200 MHz,
//! divided by PRESCALER + 1. rcc_hclk3 (236 MHz) gives only 118 MHz (over
//! 0xEB's 104 MHz at CR.DC = 0) or 59 MHz with an even factor; PLL2 gives
//! 100 MHz. RM0433 23.5.1 (and DS12117 Table 170) say an odd division
//! factor does not give a 50% duty cycle, which the NOR needs (tCH, tCL
//! >= 45%); so only even factors are accepted ([`Qspi::init`]).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::clock::wait_for;
use crate::gpio::pin_af;
use crate::pac;

/// The NOR's size: 64 Mbit (UM2905 section 5.3.1).
pub const NOR_BYTES: u32 = 8 << 20;
/// DCR FSIZE: 2^(FSIZE + 1) bytes (RM0433 section 23.3.9), set to twice
/// the NOR: ES0392 2.7.5, "Memory-mapped read of last memory byte fails"
/// (the last byte of the FSIZE region reads 0x00 and a second read stalls
/// the AXI bus); its first workaround is "taking margin in FSIZE". So the
/// last byte of the FSIZE region is 0x90FF_FFFF, outside the MPU window
/// (8 MiB, [`crate::mpu`]), and the NOR's own last byte reads normally.
/// The upper 8 MiB would alias the NOR (it ignores A23), but nothing
/// reaches it: the MPU makes it no-access, and [`Qspi::read`] refuses
/// addresses past [`NOR_BYTES`] itself, which FSIZE no longer does.
const FSIZE: u32 = (2 * NOR_BYTES).trailing_zeros() - 1;

/// Where the NOR appears in memory-mapped mode (RM0433 Table 7, the
/// QUADSPI bank, 0x9000_0000 to 0x9FFF_FFFF).
pub const WINDOW: usize = 0x9000_0000;

/// The QUADSPI kernel clock: pll2_r_ck, started by [`Qspi::init`]: HSI
/// 64 MHz / DIVM2 4 = 16 MHz (PLL input range 8 to 16 MHz), x DIVN2 50 =
/// 800 MHz VCO (wide range, 192 to 836 MHz, DS12117 Table 39), / DIVR2 4.
/// The HSI is within +-0.5% at 30 C plus +-1% over -20 to 105 C (DS12117
/// Table 36), so the 100 MHz memory-mapped clock stays under 104 MHz.
pub const KER_HZ: u32 = 200_000_000;
/// PLL2 dividers for [`KER_HZ`] (RM0433 8.7.10 PLLCKSELR DIVM2, 8.7.14
/// PLL2DIVR: fields are N - 1 and R - 1).
const PLL2_DIVM: u8 = 4;
const PLL2_DIVN: u16 = 50;
const PLL2_DIVR: u8 = 4;

/// The prescaler of the first-contact clock: 200 / 6 = 33.3 MHz, an even
/// factor, under the 50 MHz every common serial NOR allows for its slowest
/// read command (0x03).
pub const PRESCALER_SAFE: u8 = 5;

/// The memory-mapped clock: 200 / 2 = 100 MHz, an even factor, under
/// 0xEB's 104 MHz at CR.DC = 0 and VCC >= 3 V (MX25L6433F Table 7; the
/// shield's VCC is 3.3 V) and DS12117 Table 170's 133 MHz, but over 0x03's
/// 50 MHz: 0x03 reads go at [`PRESCALER_SAFE`]. Measured: M6, docs/perf.md
/// "QSPI tuning (M6)".
pub const PRESCALER_MMAP: u8 = 1;

/// The 0xEB mode byte ("performance enhance" bits P7..P0, MX25L6433F
/// sections 10-12 and 10-13): the part enters enhance mode (the next
/// command has no instruction byte) only when P7..P4 toggle against
/// P3..P0 (e.g. 0xA5, 0x5A). 0xFF does not toggle, so every command keeps
/// its instruction byte; the alternate-byte phase drives the IOs during
/// the two mode cycles instead of leaving them floating (a quad dummy phase
/// is high-Z, RM0433 23.3.4). `docs/reference/qspi-nor.md` "0xEB mode
/// bits".
pub const MODE_BYTE: u8 = 0xFF;

/// 4READ (0xEB, 1-4-4): instruction on 1 line, 24-bit address on 4, the
/// mode byte on 4 (2 cycles), 4 dummy cycles (6 in all at CR.DC = 0,
/// MX25L6433F Table 7; SFDP BFPT dword 3: 4 wait states + 2 mode clocks),
/// data on 4. Use [`Read::at`] for the address.
pub const QUAD_IO_READ: Read = Read {
    instruction: 0xEB,
    instruction_lines: Lines::Single,
    address: Some((0, Lines::Quad)),
    alternate: Some((MODE_BYTE, Lines::Quad)),
    dummy_cycles: 4,
    data_lines: Lines::Quad,
};

/// The mode byte that keeps the NOR in 0xEB "performance enhance" mode
/// (P7..P4 = 1010 toggles against P3..P0 = 0101, MX25L6433F sections 10-12
/// and 10-13): the next command starts with the address, no instruction.
pub const ENHANCE_MODE_BYTE: u8 = 0xA5;

/// [`QUAD_IO_READ`] with [`ENHANCE_MODE_BYTE`], for memory-mapped mode
/// only: [`Qspi::memory_mapped`] then sets SIOO (RM0433 23.3.13, send the
/// instruction only once), so every command after the first is address,
/// mode and dummy cycles only (8 QUADSPI clocks fewer per random miss).
/// Never use it in indirect mode: the NOR would take the next instruction
/// byte as an address. [`Mapped::exit`] and [`Qspi::init`] take the NOR
/// out of enhance mode ([`Qspi::release_enhance`]).
pub const QUAD_IO_READ_XIP: Read = Read {
    alternate: Some((ENHANCE_MODE_BYTE, Lines::Quad)),
    ..QUAD_IO_READ
};

/// Bounded waits: polls of a status bit. One poll is a few AHB reads, so a
/// million is milliseconds, far more than any read command here takes at
/// the slowest clock (a 256-byte read at 1 MHz is 2 ms).
const POLLS: u32 = 4_000_000;

// QUADSPI_CR (RM0433 section 23.5.1).
const CR_EN: u32 = 1 << 0;
const CR_ABORT: u32 = 1 << 1;
const CR_TCEN: u32 = 1 << 3;
const CR_SSHIFT: u32 = 1 << 4;
const CR_PRESCALER_SHIFT: u32 = 24;
// QUADSPI_DCR (23.5.2).
const DCR_FSIZE_SHIFT: u32 = 16;
const DCR_CSHT_SHIFT: u32 = 8;
// QUADSPI_SR (23.5.3).
const SR_TEF: u32 = 1 << 0;
const SR_TCF: u32 = 1 << 1;
const SR_BUSY: u32 = 1 << 5;
const SR_FLEVEL_SHIFT: u32 = 8;
const SR_FLEVEL_MASK: u32 = 0x3F;
// QUADSPI_FCR (23.5.4).
const FCR_CTEF: u32 = 1 << 0;
const FCR_CTCF: u32 = 1 << 1;
// QUADSPI_CCR (23.5.6).
const CCR_FRCM: u32 = 1 << 29;
const CCR_SIOO: u32 = 1 << 28;
const CCR_FMODE_INDIRECT_READ: u32 = 0b01 << 26;
const CCR_FMODE_MEMORY_MAPPED: u32 = 0b11 << 26;
const CCR_DMODE_SHIFT: u32 = 24;
const CCR_DCYC_SHIFT: u32 = 18;
/// ABSIZE 00: one alternate byte.
const CCR_ABSIZE_8: u32 = 0b00 << 16;
const CCR_ABMODE_SHIFT: u32 = 14;
const CCR_ADSIZE_24: u32 = 0b10 << 12;
const CCR_ADMODE_SHIFT: u32 = 10;
const CCR_IMODE_SHIFT: u32 = 8;

/// How many lines a phase uses (the IMODE/ADMODE/DMODE encodings of
/// QUADSPI_CCR, RM0433 section 23.5.6).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lines {
    Single = 0b01,
    Dual = 0b10,
    Quad = 0b11,
}

/// One read command in indirect mode: an instruction, an optional 24-bit
/// address, dummy cycles, then the data phase. SDR only.
#[derive(Clone, Copy, Debug)]
pub struct Read {
    pub instruction: u8,
    pub instruction_lines: Lines,
    /// The 24-bit address and its lines; `None`: no address phase.
    pub address: Option<(u32, Lines)>,
    /// One alternate byte (QUADSPI_ABR, RM0433 23.3.3) and its lines, sent
    /// after the address; `None`: no alternate-byte phase.
    pub alternate: Option<(u8, Lines)>,
    /// Dummy cycles, 0 to 31.
    pub dummy_cycles: u8,
    pub data_lines: Lines,
}

impl Read {
    /// `instruction`, every phase on one line, no address, no dummy
    /// cycles (e.g. 0x9F, 0x05).
    pub const fn single(instruction: u8) -> Self {
        Read {
            instruction,
            instruction_lines: Lines::Single,
            address: None,
            alternate: None,
            dummy_cycles: 0,
            data_lines: Lines::Single,
        }
    }

    /// The same command with a 24-bit address: on the lines of its address
    /// phase if it has one ([`QUAD_IO_READ`]), else on one line.
    pub const fn at(self, address: u32) -> Self {
        let lines = match self.address {
            Some((_, l)) => l,
            None => Lines::Single,
        };
        Read {
            address: Some((address, lines)),
            ..self
        }
    }

    /// The same command with `n` dummy cycles.
    pub const fn dummy(self, n: u8) -> Self {
        Read {
            dummy_cycles: n,
            ..self
        }
    }

    /// QUADSPI_CCR for this command in functional mode `fmode`
    /// (`CCR_FMODE_*`). Never 0: IMODE is never 00 (ES0392 2.7.3, CCR = 0
    /// hangs the peripheral), and an instruction phase also keeps clear of
    /// ES0392 2.7.2 (a data-only indirect read hangs). SIOO, DDRM, DHHC and
    /// FRCM stay 0: the instruction is sent with every command, SDR.
    fn ccr(&self, fmode: u32) -> u32 {
        let (admode, adsize) = match self.address {
            Some((_, l)) => (l as u32, CCR_ADSIZE_24),
            None => (0, 0),
        };
        let abmode = match self.alternate {
            Some((_, l)) => l as u32,
            None => 0,
        };
        fmode
            | (self.data_lines as u32) << CCR_DMODE_SHIFT
            | u32::from(self.dummy_cycles & 0x1F) << CCR_DCYC_SHIFT
            | CCR_ABSIZE_8
            | abmode << CCR_ABMODE_SHIFT
            | adsize
            | admode << CCR_ADMODE_SHIFT
            | (self.instruction_lines as u32) << CCR_IMODE_SHIFT
            | u32::from(self.instruction)
    }

    /// Writes ABR if the command has an alternate byte. RM0433 23.5.8: ABR
    /// only while BUSY = 0, before the CCR/AR write that starts the
    /// command.
    fn write_abr(&self) {
        if let Some((b, _)) = self.alternate {
            // SAFETY: ALTERNATE is any 32-bit value; with ABSIZE 00 only
            // its low byte is sent.
            regs().abr.write(|w| unsafe { w.bits(u32::from(b)) });
        }
    }
}

/// What went wrong in a command. After any error the command has been
/// aborted (CR.ABORT) and the peripheral is idle again, if the abort
/// completed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The peripheral stayed busy before the command started.
    Busy,
    /// TEF: the address and length exceed FSIZE (RM0433 section 23.3.14;
    /// [`Qspi::read`] checks against the smaller [`NOR_BYTES`] first).
    Transfer,
    /// No data within the bounded wait.
    Timeout,
    /// The dummy-cycle count does not fit DCYC, the address and length
    /// run past the NOR ([`NOR_BYTES`]), or the buffer is empty.
    Argument,
}

/// The QUADSPI, set up by [`Qspi::init`]. Owns the peripheral: only one
/// exists (the constructor is `init`, which the lib calls nowhere else).
pub struct Qspi {
    prescaler: u8,
}

/// The register block. QUADSPI's registers are at 0x5200_5000 (RM0433
/// Table 8), inside the MPU's Device region for the peripherals.
fn regs() -> &'static pac::quadspi::RegisterBlock {
    // SAFETY: a shared reference to the memory-mapped register block, which
    // is valid for the whole program; every access is a volatile register
    // access through the PAC, and only the [`Qspi`] owner touches it (no
    // interrupt, no second thread in this firmware).
    unsafe { &*pac::QUADSPI::ptr() }
}

/// Writes QUADSPI_CR.
fn write_cr(v: u32) {
    // SAFETY: every field of CR takes any value it is given here (the
    // reserved bits 2, 5, 13..15, 21 are kept 0 by the callers).
    regs().cr.write(|w| unsafe { w.bits(v) });
}

/// Writes QUADSPI_CCR. Never with 0 (ES0392 2.7.3), and never with a chip
/// erase or WRSR instruction ([`FORBIDDEN`]), in any mode: an indirect
/// read with one of them would send it too.
fn write_ccr(v: u32) {
    assert!(v != 0, "QUADSPI_CCR = 0 hangs the QUADSPI (ES0392 2.7.3)");
    assert!(
        !FORBIDDEN.contains(&(v as u8)),
        "chip erase and WRSR are never sent"
    );
    // SAFETY: any non-zero CCR value selects a valid frame format.
    regs().ccr.write(|w| unsafe { w.bits(v) });
}

fn sr() -> u32 {
    regs().sr.read().bits()
}

fn not_busy() -> bool {
    wait_for(POLLS, || sr() & SR_BUSY == 0)
}

/// CR as this driver runs it, for a given prescaler: SSHIFT, EN, TCEN 0,
/// FTHRES 0, no interrupts ([`Qspi::init`] says why).
fn cr_value(prescaler: u8) -> u32 {
    u32::from(prescaler) << CR_PRESCALER_SHIFT | CR_SSHIFT | CR_EN
}

/// DCR (RM0433 23.3.9, 23.3.11, 23.5.2): [`FSIZE`] (16 MiB, the ES0392
/// 2.7.5 margin); CSHT for at least 50 ns of NCS high between commands at
/// the clock `prescaler` gives (the MX25L6433F's tSHSL after a write-type
/// command, longer than the 15 ns after a read); CKMODE 0 (mode 0; serial
/// NORs take mode 0 and 3). Only while BUSY = 0.
fn write_dcr(prescaler: u8) {
    let clk = KER_HZ / (u32::from(prescaler) + 1);
    let csht = clk.div_ceil(20_000_000).clamp(1, 8) - 1;
    // SAFETY: FSIZE 23 and CSHT 0..7 are valid field values; the reserved
    // bits stay 0.
    regs()
        .dcr
        .write(|w| unsafe { w.bits(FSIZE << DCR_FSIZE_SHIFT | csht << DCR_CSHT_SHIFT) });
}

impl Qspi {
    /// Turns the QUADSPI on and sets it up for the shield's NOR, at
    /// [`KER_HZ`] / (`prescaler` + 1). Call once, after [`crate::init`].
    ///
    /// # Panics
    /// If `prescaler` is even (an odd division factor, ES0392 2.7.6 and
    /// RM0433 23.5.1), or if the QUADSPI stays busy through the ES0392
    /// 2.7.4 sequence.
    pub fn init(prescaler: u8) -> Qspi {
        assert!(
            prescaler % 2 == 1,
            "QUADSPI prescaler must divide by an even factor"
        );
        // SAFETY: shared access to the RCC and the four GPIO ports; no
        // interrupt and no second thread exists, so register accesses
        // never overlap. The pins set here belong to nobody else in this
        // lib (LEDs PB0, PB14, PE1; UART PD8).
        let (rcc, gpiob, gpiod, gpioe, gpiog) = unsafe {
            (
                &*pac::RCC::ptr(),
                &*pac::GPIOB::ptr(),
                &*pac::GPIOD::ptr(),
                &*pac::GPIOE::ptr(),
                &*pac::GPIOG::ptr(),
            )
        };
        // GPIO clocks (RCC_AHB4ENR); B, D and E are on since `init`.
        rcc.ahb4enr.modify(|_, w| {
            w.gpioben()
                .set_bit()
                .gpioden()
                .set_bit()
                .gpioeen()
                .set_bit()
                .gpiogen()
                .set_bit()
        });
        let _ = rcc.ahb4enr.read();

        // The kernel clock: pll2_r_ck (RM0433 section 8.7.19, RCC_D1CCIPR
        // QSPISEL = 10, [`KER_HZ`]), set while the peripheral clock is
        // off, once PLL2 runs (`pll2_on`). Then the
        // bus clock (RCC_AHB3ENR QSPIEN, 8.7.40), read back so it is on
        // before the reset, and a reset pulse (RCC_AHB3RSTR QSPIRST,
        // 8.7.28) for a known state whatever ran before.
        pll2_on(rcc);
        rcc.d1ccipr.modify(|_, w| w.qspisel().pll2_r());
        rcc.ahb3enr.modify(|_, w| w.qspien().set_bit());
        let _ = rcc.ahb3enr.read();
        rcc.ahb3rstr.modify(|_, w| w.qspirst().set_bit());
        rcc.ahb3rstr.modify(|_, w| w.qspirst().clear_bit());
        let _ = rcc.ahb3rstr.read();

        // NCS with a pull-up, so the NOR stays deselected if the pin is
        // ever not driven (ST's BSPs do the same); the IOs without pulls.
        pin_af(gpiog, 6, 10, true); // PG6 QUADSPI_BK1_NCS, AF10
        pin_af(gpiob, 2, 9, false); // PB2 QUADSPI_CLK, AF9
        pin_af(gpiod, 11, 9, false); // PD11 QUADSPI_BK1_IO0, AF9
        pin_af(gpiod, 12, 9, false); // PD12 QUADSPI_BK1_IO1, AF9
        pin_af(gpioe, 2, 9, false); // PE2 QUADSPI_BK1_IO2, AF9
        pin_af(gpiod, 13, 9, false); // PD13 QUADSPI_BK1_IO3, AF9

        errata_2_7_4();
        write_dcr(prescaler);
        // CR (23.3.11): the prescaler; SSHIFT, sampling half a CLK cycle
        // later than the default, for the delay of the path through the
        // shield's connectors (23.3.10; SDR only, DDR needs it clear);
        // FTHRES 0 (FTF with one byte in the FIFO); TCEN 0 (see
        // [`Qspi::memory_mapped`]); no interrupts; EN.
        write_cr(cr_value(prescaler));
        let mut q = Qspi { prescaler };
        // The NOR may still be in enhance mode from an image that ran
        // before this reset (it has no reset pin and the MCU reset does
        // not reach it). A failure shows in the first real command.
        let _ = q.release_enhance();
        q
    }

    /// Takes the NOR out of 0xEB enhance mode ([`QUAD_IO_READ_XIP`]), if it
    /// is in it: one 0xEB read without the instruction phase (IMODE 00),
    /// address 0x7F_FFFF and mode byte 0xFF on 4 lines, 4 dummy cycles, one
    /// data byte. In enhance mode the NOR takes it as a 4READ and leaves
    /// the mode (0xFF does not toggle, MX25L6433F sections 10-12, 10-13).
    /// Otherwise IO0 carries the bit 0 of each address nibble and of the
    /// mode byte, all 1: instruction 0xFF, not a command of the part
    /// (Table 4), and NCS rises after 14 clocks, so it does nothing.
    fn release_enhance(&mut self) -> Result<(), Error> {
        let q = regs();
        if !not_busy() {
            return Err(Error::Busy);
        }
        // SAFETY: write-1-to-clear flags.
        q.fcr.write(|w| unsafe { w.bits(FCR_CTEF | FCR_CTCF) });
        // SAFETY: DL is any 32-bit value (one byte: 0).
        q.dlr.write(|w| unsafe { w.bits(0) });
        // SAFETY: ALTERNATE is any 32-bit value; with ABSIZE 00 only its
        // low byte is sent.
        q.abr.write(|w| unsafe { w.bits(0xFF) });
        let quad = Lines::Quad as u32;
        // IMODE 00 but an address phase: not ES0392 2.7.2's data-only read,
        // and CCR != 0 (2.7.3).
        write_ccr(
            CCR_FMODE_INDIRECT_READ
                | quad << CCR_DMODE_SHIFT
                | 4 << CCR_DCYC_SHIFT
                | CCR_ABSIZE_8
                | quad << CCR_ABMODE_SHIFT
                | CCR_ADSIZE_24
                | quad << CCR_ADMODE_SHIFT,
        );
        // SAFETY: ADDRESS is any 32-bit value; 24 bits are sent.
        q.ar.write(|w| unsafe { w.bits(NOR_BYTES - 1) });
        let r = self.drain(&mut [0u8]);
        if r.is_err() {
            abort();
        }
        r
    }

    /// The PRESCALER field `init` was given.
    pub fn prescaler(&self) -> u8 {
        self.prescaler
    }

    /// The QUADSPI clock, nominal: [`KER_HZ`] / (PRESCALER + 1).
    pub fn clock_hz(&self) -> u32 {
        KER_HZ / (u32::from(self.prescaler) + 1)
    }

    /// Changes the clock divider (RM0433 23.5.1: PRESCALER only while
    /// BUSY = 0), and CSHT with it (23.5.2, also only while BUSY = 0), so
    /// NCS stays high long enough at the new clock. Same rule as
    /// [`Qspi::init`]: an even division factor.
    pub fn set_prescaler(&mut self, prescaler: u8) -> Result<(), Error> {
        assert!(
            prescaler % 2 == 1,
            "QUADSPI prescaler must divide by an even factor"
        );
        if !not_busy() {
            return Err(Error::Busy);
        }
        write_dcr(prescaler);
        let cr = regs().cr.read().bits() & !(0xFF << CR_PRESCALER_SHIFT);
        write_cr(cr | u32::from(prescaler) << CR_PRESCALER_SHIFT);
        self.prescaler = prescaler;
        Ok(())
    }

    /// Runs `cmd` in indirect-read mode and fills `buf` with the data
    /// phase's bytes. RM0433 section 23.3.12 "Indirect mode": DLR, ABR,
    /// CCR, then AR, which starts the command when there is an address
    /// (otherwise the CCR write does, 23.3.5), then the FIFO through DR.
    /// An address phase must stay inside the NOR ([`NOR_BYTES`]; FSIZE has
    /// a margin, see [`FSIZE`]).
    pub fn read(&mut self, cmd: &Read, buf: &mut [u8]) -> Result<(), Error> {
        if buf.is_empty()
            || cmd.dummy_cycles > 31
            || cmd
                .address
                .is_some_and(|(a, _)| a as usize + buf.len() > NOR_BYTES as usize)
        {
            return Err(Error::Argument);
        }
        let q = regs();
        if !not_busy() {
            return Err(Error::Busy);
        }
        // SAFETY: CTEF and CTCF are write-1-to-clear bits; the rest stay 0.
        q.fcr.write(|w| unsafe { w.bits(FCR_CTEF | FCR_CTCF) });
        // SAFETY: DL is any 32-bit value (number of bytes - 1).
        q.dlr.write(|w| unsafe { w.bits(buf.len() as u32 - 1) });
        cmd.write_abr();
        write_ccr(cmd.ccr(CCR_FMODE_INDIRECT_READ));
        if let Some((addr, _)) = cmd.address {
            // SAFETY: ADDRESS is any 32-bit value; 24 bits are sent.
            q.ar.write(|w| unsafe { w.bits(addr) });
        }
        let r = self.drain(buf);
        if r.is_err() {
            abort();
        }
        r
    }

    /// Reads `buf.len()` bytes from the FIFO, then waits for TCF and for
    /// BUSY to fall (23.3.15: in indirect mode BUSY falls once the command
    /// is done and the FIFO is empty).
    fn drain(&mut self, buf: &mut [u8]) -> Result<(), Error> {
        // Byte reads of DR: only its least significant byte may be
        // accessed so (RM0433 23.3.5), and each pops one byte.
        let dr = regs().dr.as_ptr() as *const u8;
        for b in buf.iter_mut() {
            let mut level = 0;
            let mut err = false;
            let ok = wait_for(POLLS, || {
                let s = sr();
                err = s & SR_TEF != 0;
                level = (s >> SR_FLEVEL_SHIFT) & SR_FLEVEL_MASK;
                err || level > 0
            });
            if err {
                return Err(Error::Transfer);
            }
            if !ok {
                return Err(Error::Timeout);
            }
            // SAFETY: an aligned volatile byte read of QUADSPI_DR's least
            // significant byte, a valid MMIO address (RM0433 23.5.9);
            // FLEVEL > 0, so it pops a received byte.
            *b = unsafe { dr.read_volatile() };
        }
        if !wait_for(POLLS, || sr() & (SR_TCF | SR_TEF) != 0) {
            return Err(Error::Timeout);
        }
        if sr() & SR_TEF != 0 {
            return Err(Error::Transfer);
        }
        // SAFETY: as above, write-1-to-clear.
        regs().fcr.write(|w| unsafe { w.bits(FCR_CTCF) });
        if !not_busy() {
            return Err(Error::Timeout);
        }
        Ok(())
    }
}

/// CR.ABORT, then waits for it to clear (RM0433 23.3.15: BUSY and ABORT
/// clear once the abort is done, and the FIFO is flushed).
fn abort() {
    let cr = regs().cr.read().bits();
    write_cr(cr | CR_ABORT);
    let _ = wait_for(POLLS, || regs().cr.read().bits() & CR_ABORT == 0);
    // SAFETY: write-1-to-clear flags.
    regs().fcr.write(|w| unsafe { w.bits(FCR_CTEF | FCR_CTCF) });
}

/// Starts PLL2 for [`KER_HZ`] (RM0433 8.5.5 "PLL initialization phase":
/// DIVM2, the input range and VCO, the dividers, then PLL2ON and a wait
/// for PLL2RDY), integer mode, only the R output. The source is the one
/// PLLCKSELR.PLLSRC already selects for PLL1 (HSI, [`crate::clock`]).
/// If PLL2 already runs (nothing in the lib starts it, but a debugger
/// might), the QUADSPI is moved to rcc_hclk3 and PLL2 restarted.
///
/// # Panics
/// If PLL2 does not lock.
fn pll2_on(rcc: &pac::rcc::RegisterBlock) {
    if rcc.cr.read().pll2on().bit_is_set() {
        rcc.d1ccipr.modify(|_, w| w.qspisel().rcc_hclk3());
        rcc.cr.modify(|_, w| w.pll2on().clear_bit());
        assert!(
            wait_for(POLLS, || rcc.cr.read().pll2rdy().bit_is_clear()),
            "PLL2 did not stop"
        );
    }
    rcc.pllckselr.modify(|_, w| w.divm2().bits(PLL2_DIVM));
    rcc.pllcfgr.modify(|_, w| {
        w.pll2rge()
            .range8()
            .pll2vcosel()
            .wide_vco()
            .pll2fracen()
            .reset()
            .divp2en()
            .disabled()
            .divq2en()
            .disabled()
            .divr2en()
            .enabled()
    });
    // SAFETY: DIVN2 field 49 (x50, valid 4..512), DIVR2 field 3 (/4);
    // DIVP2 and DIVQ2 0 for disabled outputs (RM0433 8.5.5, as clock.rs
    // does for PLL1).
    rcc.pll2divr.write(|w| unsafe {
        w.divn2()
            .bits(PLL2_DIVN - 1)
            .divp2()
            .bits(0)
            .divq2()
            .bits(0)
            .divr2()
            .bits(PLL2_DIVR - 1)
    });
    rcc.cr.modify(|_, w| w.pll2on().set_bit());
    assert!(
        wait_for(POLLS, || rcc.cr.read().pll2rdy().bit_is_set()),
        "PLL2 did not lock"
    );
}

/// ES0392 section 2.7.4, "QUADSPI internal timing criticality": a
/// sequence to run after reset (and when leaving memory-mapped mode).
/// The CR = 0 that follows the first CCR write must complete within 127
/// kernel clocks (300 CPU cycles at 472 MHz and [`KER_HZ`]): the three writes are back to
/// back, with nothing between them but the store instructions.
fn errata_2_7_4() {
    write_cr(0);
    assert!(not_busy(), "QUADSPI busy before the ES0392 2.7.4 sequence");
    // Maximum prescaler, enabled.
    write_cr(0xFF << CR_PRESCALER_SHIFT | CR_EN);
    // Free-running clock mode, written twice "to prevent a back-to-back
    // disable" (ES0392's words); then disable.
    write_ccr(CCR_FRCM);
    write_ccr(CCR_FRCM);
    write_cr(0);
    assert!(not_busy(), "QUADSPI busy after the ES0392 2.7.4 sequence");
    // ES0392 says to restore CCR. Its reset value, 0, cannot be written
    // (2.7.3), and FRCM left set would start the free-running clock as
    // soon as EN is set. So CCR is parked on a command that needs an
    // address and so cannot start before AR is written (RM0433 23.3.5):
    // a 1-line read (0x03) of one byte.
    write_ccr(Read::single(0x03).at(0).ccr(CCR_FMODE_INDIRECT_READ));
}

impl Qspi {
    /// Switches to memory-mapped mode with `cmd` (normally
    /// [`QUAD_IO_READ`]; its address is ignored, the bus access gives it)
    /// at the current clock, then opens the MPU window (module docs for
    /// the order). The caller picks a clock the command allows
    /// ([`PRESCALER_MMAP`] for 0xEB).
    ///
    /// TCEN stays 0 (RM0433 23.3.7, 23.5.1): after an access the QUADSPI
    /// keeps prefetching into its FIFO and holds NCS low with the clock
    /// stopped once the FIFO is full, so the next sequential access is
    /// served without a new command. TCEN = 1 would raise NCS after
    /// LPTR.TIMEOUT idle cycles, saving the NOR's selected-but-idle
    /// current at the cost of a new instruction, address, mode and dummy
    /// phase (8 + 6 + 6 = 20 QUADSPI clocks, 0.2 us at 100 MHz) on the next
    /// access, even a sequential one. The core reads the ROM mostly
    /// sequentially and the NUCLEO is USB-powered, so the latency wins;
    /// a battery-powered Stage 3 board can revisit this.
    ///
    /// # Errors
    /// [`Error::Busy`] if the QUADSPI stays busy; [`Error::Argument`] if
    /// `cmd` has no address phase or too many dummy cycles. The QUADSPI is
    /// handed back unchanged then.
    pub fn memory_mapped(self, cmd: &Read) -> Result<Mapped, (Qspi, Error)> {
        if cmd.address.is_none() || cmd.dummy_cycles > 31 {
            return Err((self, Error::Argument));
        }
        if !not_busy() {
            return Err((self, Error::Busy));
        }
        let q = regs();
        // TCEN only while BUSY = 0 (23.5.1); cleared explicitly (see above).
        write_cr(q.cr.read().bits() & !CR_TCEN);
        // SAFETY: write-1-to-clear flags.
        q.fcr.write(|w| unsafe { w.bits(FCR_CTEF | FCR_CTCF) });
        cmd.write_abr();
        // FMODE = 11 (23.3.12 "Memory-mapped mode"): the frame is sent
        // when a master reads the window; DLR and AR are not used.
        // SIOO with the enhance-mode byte only ([`QUAD_IO_READ_XIP`]).
        let enhance = cmd.alternate.is_some_and(|(b, _)| b == ENHANCE_MODE_BYTE);
        let sioo = if enhance { CCR_SIOO } else { 0 };
        write_ccr(cmd.ccr(CCR_FMODE_MEMORY_MAPPED) | sioo);
        // The CCR write reaches the peripheral before the MPU opens the
        // window (the region change itself ends with a DSB and an ISB).
        cortex_m::asm::dsb();
        crate::mpu::qspi_window(crate::mpu::Window::Cached);
        Ok(Mapped { q: self, enhance })
    }
}

/// The QUADSPI in memory-mapped mode, from [`Qspi::memory_mapped`]: the
/// NOR is readable at [`WINDOW`] ([`Mapped::bytes`]). Indirect commands
/// need [`Mapped::exit`], which hands the [`Qspi`] back.
pub struct Mapped {
    q: Qspi,
    /// Mapped with [`QUAD_IO_READ_XIP`]: the NOR is in enhance mode.
    enhance: bool,
}

impl Mapped {
    /// The whole NOR, [`NOR_BYTES`] long, read through the window: the
    /// slice the core will take as its ROM. Reads of any width and
    /// alignment work (Normal memory, Armv7-M A3.2.1).
    pub fn bytes(&self) -> &[u8] {
        // SAFETY: [`WINDOW`]..+[`NOR_BYTES`] is readable for as long as
        // `self` exists: the QUADSPI is in memory-mapped mode and the MPU
        // maps exactly that range as readable Normal memory (opened by
        // `memory_mapped`, closed only by `exit`, which consumes `self`).
        // The contents do not change meanwhile: the only way to send a
        // program or erase command is the [`Qspi`] this owns, and nothing
        // else in the lib drives the QUADSPI. The range ends before the
        // FSIZE region's last byte (ES0392 2.7.5, [`FSIZE`]).
        unsafe { core::slice::from_raw_parts(WINDOW as *const u8, NOR_BYTES as usize) }
    }

    /// The QUADSPI clock, as [`Qspi::clock_hz`].
    pub fn clock_hz(&self) -> u32 {
        self.q.clock_hz()
    }

    /// Makes the window cacheable (the default) or not, through its MPU
    /// region's attributes, for measurements: the uncached figure is the
    /// cost of one bus access. The D-cache keeps any lines it already holds
    /// for the window; invalidate them to measure misses.
    pub fn set_cacheable(&mut self, cacheable: bool) {
        crate::mpu::qspi_window(if cacheable {
            crate::mpu::Window::Cached
        } else {
            crate::mpu::Window::Uncached
        });
    }

    /// Leaves memory-mapped mode (module docs for the order): closes the
    /// MPU window, stops the prefetch, runs the ES0392 2.7.4 sequence,
    /// restores CR and DCR, and cleans and invalidates the D-cache.
    ///
    /// Stopping the prefetch: CR.ABORT first (RM0433 23.3.15). Measured on
    /// this board (2026-11-25): when the abort lands while a prefetch is
    /// still filling the FIFO (SR = 0x620: BUSY, FLEVEL 6), the abort
    /// completes (ABORT clears, TCF sets) and the QUADSPI, still in FMODE
    /// 11, starts prefetching again: BUSY stays high with FLEVEL climbing to
    /// 30 (SR = 0x1E22), and no CPU access can have asked for it (the MPU
    /// window was closed and a DSB had completed every access). RM0433
    /// 23.3.7 names the other way BUSY falls: "the peripheral is disabled".
    /// So if BUSY is still high after the abort, EN is cleared, which the
    /// ES0392 2.7.4 sequence does next anyway (its first step is CR = 0
    /// and a wait for BUSY). Only reads are ever interrupted so (the mode
    /// is read-only), which the NOR tolerates; RM0433's caution is about
    /// aborting a status-register write. [`exit_fallbacks`] counts how
    /// often the abort alone did not do it.
    ///
    /// # Panics
    /// If the QUADSPI stays busy even disabled (the message has SR, CR and
    /// CCR as found).
    pub fn exit(self) -> Qspi {
        crate::mpu::qspi_window(crate::mpu::Window::Off);
        let sr0 = sr();
        abort();
        if !wait_for(ABORT_POLLS, || sr() & SR_BUSY == 0) {
            EXIT_FALLBACKS.fetch_add(1, Ordering::Relaxed);
            let (sr1, cr1) = (sr(), regs().cr.read().bits());
            write_cr(cr1 & !CR_EN);
            if !not_busy() {
                panic!(
                    "QUADSPI busy after the memory-mapped abort and EN = 0: \
                     sr_before={sr0:#x} sr_after_abort={sr1:#x} sr={:#x} cr={cr1:#x} ccr={:#x}",
                    sr(),
                    regs().ccr.read().bits()
                );
            }
        }
        errata_2_7_4();
        write_dcr(self.q.prescaler);
        write_cr(cr_value(self.q.prescaler));
        let mut q = self.q;
        if self.enhance {
            // Panics rather than hand back a NOR that would take the next
            // instruction as an address.
            if let Err(e) = q.release_enhance() {
                panic!("QUADSPI: leaving 0xEB enhance mode failed: {e:?}");
            }
        }
        // No line of the window may outlive the mapping: a later program
        // or erase would leave it stale. Clean as well as invalidate, since
        // the other cacheable regions (AXI SRAM) hold dirty lines.
        // SAFETY: SCB and CPUID are core registers; this firmware has one
        // thread and no interrupt, so nothing else uses them meanwhile, and
        // a clean-and-invalidate by set/way loses no data.
        let mut cp = unsafe { cortex_m::Peripherals::steal() };
        cp.SCB.clean_invalidate_dcache(&mut cp.CPUID);
        q
    }
}

/// How long [`Mapped::exit`] waits for BUSY to fall after the abort
/// before it disables the QUADSPI: polls of SR, far longer than an abort
/// takes (a few QUADSPI clocks), far shorter than [`POLLS`].
const ABORT_POLLS: u32 = 10_000;

/// See [`exit_fallbacks`].
static EXIT_FALLBACKS: AtomicU32 = AtomicU32::new(0);

/// How many [`Mapped::exit`] calls so far needed EN = 0 because BUSY
/// stayed high after the abort (a diagnostic for the test images).
pub fn exit_fallbacks() -> u32 {
    EXIT_FALLBACKS.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------
// The write side: write enable, program and erase (indirect mode only).
//
// Only a [`Qspi`] can send these, and a [`Qspi`] exists only while the
// QUADSPI is not in memory-mapped mode: [`Qspi::memory_mapped`] consumes
// it and [`Mapped::exit`] hands it back (memory-mapped mode is read-only,
// RM0433 23.3.12). [`Qspi::require_indirect`] checks FMODE as well.
//
// Never sent: chip erase (0x60, 0xC7) and WRSR (0x01). QE is already 1 on
// this board (`docs/reference/qspi-nor.md` "The quad-enable bit") and
// status-register writes are non-volatile, so this driver never writes
// the status or configuration register ([`write_ccr`] refuses all three
// opcodes).
//
// Source for the commands, bits and times: MX25L6433F datasheet PM2130
// Rev. 1.4, Table 4 (command set), section 10-4 (status register: WIP bit
// 0, WEL bit 1; WEL clears when a program or erase completes; program and
// erase are ignored without WEL), 10-19 (PP: the CS# rise starts tPP; data
// past the page end wraps to the page start, so the driver never sends
// it), 10-26 and Table 10 (security register: P_FAIL bit 5, E_FAIL bit 6,
// cleared by the next successful operation), and the AC table (p. 66: tPP,
// tSE, tBE; tSHSL >= 50 ns after write, erase and program, which DCSHT
// gives, [`write_dcr`]).
// ---------------------------------------------------------------------

/// The NOR's page: a program never crosses one (MX25L6433F 10-19).
pub const PAGE_BYTES: u32 = 256;
/// The 4 KiB sector of [`Qspi::erase_4k`] (SE, 0x20).
pub const SECTOR_BYTES: u32 = 4096;
/// The 64 KiB block of [`Qspi::erase_64k`] (BE, 0xD8).
pub const BLOCK_BYTES: u32 = 65536;

const CMD_PP: u8 = 0x02;
const CMD_RDSR: u8 = 0x05;
const CMD_WREN: u8 = 0x06;
const CMD_SE: u8 = 0x20;
const CMD_RDSCUR: u8 = 0x2B;
const CMD_BE: u8 = 0xD8;
/// Opcodes [`write_ccr`] refuses in every mode: chip erase (0x60, 0xC7)
/// and WRSR (0x01).
const FORBIDDEN: [u8; 3] = [0x60, 0xC7, 0x01];

/// Status register (RDSR): write in progress, write enable latch.
const STATUS_WIP: u8 = 1 << 0;
const STATUS_WEL: u8 = 1 << 1;
/// Security register (RDSCUR, Table 10): the last program or erase failed.
const SCUR_P_FAIL: u8 = 1 << 5;
const SCUR_E_FAIL: u8 = 1 << 6;

// QUADSPI_CR (23.5.1), QUADSPI_SR (23.5.3), QUADSPI_FCR (23.5.4),
// QUADSPI_CCR (23.5.6) fields of the write side.
const CR_APMS: u32 = 1 << 22;
const CR_PMM: u32 = 1 << 23;
const SR_SMF: u32 = 1 << 3;
const FCR_CSMF: u32 = 1 << 3;
const CCR_FMODE_INDIRECT_WRITE: u32 = 0b00 << 26;
const CCR_FMODE_AUTO_POLL: u32 = 0b10 << 26;
const CCR_FMODE_MASK: u32 = 0b11 << 26;
/// The FIFO's depth in bytes (FLEVEL counts up to 32, RM0433 23.5.3).
const FIFO_BYTES: u32 = 32;
/// PIR (23.5.12): QUADSPI clocks between two status reads of
/// [`Qspi::wait_idle`]; 64 clocks is 1.6 us at 39 MHz, far below the
/// shortest wait (a page program, 0.33 ms typ).
const POLL_INTERVAL: u32 = 64;

/// The operations [`Qspi::wait_idle`] waits for, with the MX25L6433F's
/// typical and maximum times (AC table, p. 66).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    /// PP (0x02), one page: tPP 0.33 ms typ, 1.2 ms max.
    PageProgram,
    /// SE (0x20), 4 KiB: tSE 25 ms typ, 200 ms max.
    SectorErase,
    /// BE (0xD8), 64 KiB: tBE 0.25 s typ, 1 s max.
    BlockErase,
}

impl Op {
    /// The datasheet's typical time, in microseconds.
    pub const fn typ_us(self) -> u32 {
        match self {
            Op::PageProgram => 330,
            Op::SectorErase => 25_000,
            Op::BlockErase => 250_000,
        }
    }

    /// The datasheet's maximum time, in microseconds.
    pub const fn max_us(self) -> u32 {
        match self {
            Op::PageProgram => 1_200,
            Op::SectorErase => 200_000,
            Op::BlockErase => 1_000_000,
        }
    }

    /// [`Qspi::wait_idle`]'s timeout: twice the datasheet maximum, in CPU
    /// cycles (2 s at most, well inside the DWT counter's 9.1 s wrap).
    pub const fn timeout_cycles(self) -> u32 {
        2 * self.max_us() * (crate::CPU_HZ / 1_000_000)
    }
}

/// What went wrong in a write-side operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NorError {
    /// A command failed in the QUADSPI ([`Error`]).
    Qspi(Error),
    /// An erase address not on its sector or block boundary, or a page
    /// program longer than a page or crossing a page boundary.
    Alignment,
    /// The address, or address plus length, runs past [`NOR_BYTES`].
    OutOfRange,
    /// WIP stayed 1 past [`Op::timeout_cycles`]; the polling was aborted.
    Timeout,
    /// WEL read 0 after WREN; the status register value.
    WelNotSet(u8),
    /// RDSCUR flagged P_FAIL or E_FAIL after the operation; its value.
    Failed(u8),
    /// The QUADSPI is in memory-mapped mode (CCR.FMODE = 11).
    MemoryMapped,
}

impl From<Error> for NorError {
    fn from(e: Error) -> Self {
        NorError::Qspi(e)
    }
}

impl Qspi {
    /// The runtime half of the indirect-only rule: FMODE must not be 11.
    /// (The type is the other half: no [`Qspi`] exists in memory-mapped
    /// mode.)
    fn require_indirect(&self) -> Result<(), NorError> {
        if regs().ccr.read().bits() & CCR_FMODE_MASK == CCR_FMODE_MEMORY_MAPPED {
            return Err(NorError::MemoryMapped);
        }
        Ok(())
    }

    /// One 1-line command in indirect-write mode (RM0433 23.3.12): the
    /// instruction, an optional 24-bit address, and `data` (none if
    /// empty). Never dummy cycles: ES0392 2.7.1, the first data nibble is
    /// lost after a dummy phase in indirect write mode. The command starts
    /// at the CCR write (no address, no data), the AR write (address, no
    /// data) or the first DR write (data), 23.3.12.
    fn write_command(
        &mut self,
        instruction: u8,
        addr: Option<u32>,
        data: &[u8],
    ) -> Result<(), Error> {
        let q = regs();
        if !not_busy() {
            return Err(Error::Busy);
        }
        // SAFETY: write-1-to-clear flags.
        q.fcr.write(|w| unsafe { w.bits(FCR_CTEF | FCR_CTCF) });
        if !data.is_empty() {
            // SAFETY: DL is any 32-bit value (number of bytes - 1).
            q.dlr.write(|w| unsafe { w.bits(data.len() as u32 - 1) });
        }
        let (admode, adsize) = match addr {
            Some(_) => (Lines::Single as u32, CCR_ADSIZE_24),
            None => (0, 0),
        };
        let dmode = if data.is_empty() {
            0
        } else {
            Lines::Single as u32
        };
        // DCYC 0, ABMODE 00 (no alternate bytes), SDR.
        write_ccr(
            CCR_FMODE_INDIRECT_WRITE
                | dmode << CCR_DMODE_SHIFT
                | adsize
                | admode << CCR_ADMODE_SHIFT
                | (Lines::Single as u32) << CCR_IMODE_SHIFT
                | u32::from(instruction),
        );
        if let Some(a) = addr {
            // SAFETY: ADDRESS is any 32-bit value; 24 bits are sent.
            q.ar.write(|w| unsafe { w.bits(a) });
        }
        // Byte writes of DR: each pushes one byte into the FIFO (RM0433
        // 23.3.5, 23.5.9).
        let dr = q.dr.as_ptr() as *mut u8;
        let mut r = Ok(());
        for &b in data {
            let mut err = false;
            let ok = wait_for(POLLS, || {
                let s = sr();
                err = s & SR_TEF != 0;
                err || (s >> SR_FLEVEL_SHIFT) & SR_FLEVEL_MASK < FIFO_BYTES
            });
            if err || !ok {
                r = Err(if err { Error::Transfer } else { Error::Timeout });
                break;
            }
            // SAFETY: an aligned volatile byte write of QUADSPI_DR's least
            // significant byte, a valid MMIO address (RM0433 23.5.9); the
            // FIFO has room (FLEVEL < 32).
            unsafe { dr.write_volatile(b) };
        }
        if r.is_ok() {
            r = if !wait_for(POLLS, || sr() & (SR_TCF | SR_TEF) != 0) {
                Err(Error::Timeout)
            } else if sr() & SR_TEF != 0 {
                Err(Error::Transfer)
            } else if !not_busy() {
                Err(Error::Timeout)
            } else {
                Ok(())
            };
        }
        if r.is_err() {
            abort();
        }
        // SAFETY: write-1-to-clear flags.
        q.fcr.write(|w| unsafe { w.bits(FCR_CTCF) });
        r
    }

    /// One register byte read with a 1-line instruction (RDSR, RDSCUR).
    fn read_reg(&mut self, instruction: u8) -> Result<u8, Error> {
        let mut b = [0u8];
        self.read(&Read::single(instruction), &mut b)?;
        Ok(b[0])
    }

    /// WREN (0x06), then RDSR to check that WEL is set (MX25L6433F 10-4:
    /// program and erase are ignored without it).
    pub fn write_enable(&mut self) -> Result<(), NorError> {
        self.require_indirect()?;
        self.write_command(CMD_WREN, None, &[])?;
        let status = self.read_reg(CMD_RDSR)?;
        if status & STATUS_WEL == 0 {
            return Err(NorError::WelNotSet(status));
        }
        Ok(())
    }

    /// Waits for WIP = 0 in automatic status-polling mode (RM0433 23.3.6:
    /// RDSR every [`POLL_INTERVAL`] clocks, mask WIP, match 0, AND mode,
    /// APMS: the QUADSPI stops by itself on the match). Gives up after
    /// `op`'s [`Op::timeout_cycles`] (DWT), aborting the polling. Returns
    /// the last status byte read.
    pub fn wait_idle(&mut self, op: Op) -> Result<u8, NorError> {
        self.require_indirect()?;
        let q = regs();
        if !not_busy() {
            return Err(Error::Busy.into());
        }
        // APMS and PMM, PIR, PSMKR and PSMAR only while BUSY = 0 (23.5.1,
        // 23.5.10 to 23.5.12).
        let cr = q.cr.read().bits();
        write_cr((cr | CR_APMS) & !CR_PMM);
        // SAFETY: INTERVAL is any 16-bit value, MASK and MATCH any 32-bit
        // values (only the first status byte is read, DLR 0).
        unsafe {
            q.pir.write(|w| w.bits(POLL_INTERVAL));
            q.psmkr.write(|w| w.bits(u32::from(STATUS_WIP)));
            q.psmar.write(|w| w.bits(0));
            q.fcr.write(|w| w.bits(FCR_CSMF | FCR_CTEF | FCR_CTCF));
            q.dlr.write(|w| w.bits(0));
        }
        // No address: the polling starts with this write (23.3.6).
        write_ccr(
            CCR_FMODE_AUTO_POLL
                | (Lines::Single as u32) << CCR_DMODE_SHIFT
                | (Lines::Single as u32) << CCR_IMODE_SHIFT
                | u32::from(CMD_RDSR),
        );
        let t0 = crate::cycles();
        let limit = op.timeout_cycles();
        let matched = loop {
            let s = sr();
            if s & SR_SMF != 0 {
                break true;
            }
            if crate::cycles().wrapping_sub(t0) > limit {
                break false;
            }
        };
        let r = if matched {
            // DR holds the last status bytes read (23.3.6); byte read.
            let dr = q.dr.as_ptr() as *const u8;
            // SAFETY: an aligned volatile byte read of QUADSPI_DR, a valid
            // MMIO address (RM0433 23.5.9).
            let status = unsafe { dr.read_volatile() };
            if not_busy() {
                Ok(status)
            } else {
                abort();
                Err(Error::Timeout.into())
            }
        } else {
            abort();
            Err(NorError::Timeout)
        };
        // SAFETY: write-1-to-clear flags.
        q.fcr.write(|w| unsafe { w.bits(FCR_CSMF | FCR_CTCF) });
        // BUSY is 0 here (stopped on the match, or aborted): CR back to
        // its indirect value.
        write_cr(cr);
        r
    }

    /// RDSCUR after a program or erase: `fail` (P_FAIL or E_FAIL) set
    /// means the operation failed (MX25L6433F Figure "program/erase flow",
    /// 10-26).
    fn check_failed(&mut self, fail: u8) -> Result<(), NorError> {
        let scur = self.read_reg(CMD_RDSCUR)?;
        if scur & fail != 0 {
            return Err(NorError::Failed(scur));
        }
        Ok(())
    }

    /// WIP = 0 (a previous operation, e.g. one cut by a reset, may still
    /// run: the NOR ignores WREN meanwhile), WREN, the command, the wait,
    /// RDSCUR.
    fn write_op(&mut self, cmd: u8, addr: u32, data: &[u8], op: Op) -> Result<(), NorError> {
        self.require_indirect()?;
        self.wait_idle(Op::BlockErase)?;
        self.write_enable()?;
        self.write_command(cmd, Some(addr), data)?;
        self.wait_idle(op)?;
        self.check_failed(if op == Op::PageProgram {
            SCUR_P_FAIL
        } else {
            SCUR_E_FAIL
        })
    }

    fn erase(&mut self, addr: u32, size: u32, cmd: u8, op: Op) -> Result<(), NorError> {
        if addr >= NOR_BYTES {
            return Err(NorError::OutOfRange);
        }
        if !addr.is_multiple_of(size) {
            return Err(NorError::Alignment);
        }
        self.write_op(cmd, addr, &[], op)
    }

    /// Erases the 4 KiB sector at `addr` (SE, 0x20) to 0xFF; `addr` must
    /// be a multiple of [`SECTOR_BYTES`].
    pub fn erase_4k(&mut self, addr: u32) -> Result<(), NorError> {
        self.erase(addr, SECTOR_BYTES, CMD_SE, Op::SectorErase)
    }

    /// Erases the 64 KiB block at `addr` (BE, 0xD8) to 0xFF; `addr` must
    /// be a multiple of [`BLOCK_BYTES`].
    pub fn erase_64k(&mut self, addr: u32) -> Result<(), NorError> {
        self.erase(addr, BLOCK_BYTES, CMD_BE, Op::BlockErase)
    }

    /// Programs `data` at `addr` (PP, 0x02, 1-1-1), inside one page: at
    /// most [`PAGE_BYTES`], and `addr % 256 + len <= 256` (the NOR would
    /// wrap the rest to the page start, MX25L6433F 10-19). Programming
    /// only clears bits: the bytes should be erased first. Empty: nothing
    /// is sent.
    pub fn program_page(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        if addr as usize + data.len() > NOR_BYTES as usize {
            return Err(NorError::OutOfRange);
        }
        if (addr % PAGE_BYTES) as usize + data.len() > PAGE_BYTES as usize {
            return Err(NorError::Alignment);
        }
        if data.is_empty() {
            return Ok(());
        }
        self.write_op(CMD_PP, addr, data, Op::PageProgram)
    }

    /// Programs `data` at `addr`, split on page boundaries into
    /// [`Qspi::program_page`] calls. The range is checked whole first:
    /// nothing is written if it runs past [`NOR_BYTES`].
    pub fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
        if addr as usize + data.len() > NOR_BYTES as usize {
            return Err(NorError::OutOfRange);
        }
        let mut a = addr;
        let mut rest = data;
        while !rest.is_empty() {
            let n = ((PAGE_BYTES - a % PAGE_BYTES) as usize).min(rest.len());
            let (page, tail) = rest.split_at(n);
            self.program_page(a, page)?;
            a += n as u32;
            rest = tail;
        }
        Ok(())
    }
}
