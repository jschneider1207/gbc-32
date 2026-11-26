//! QUADSPI in indirect mode: the X-NUCLEO-GFX02Z1 shield's 64-Mbit NOR
//! flash (plan Stage 2 M2). Read commands only, so far: nothing here
//! writes the NOR (no write enable, no register write, no program or
//! erase). The part, its commands and limits: `docs/reference/qspi-nor.md`.
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
//! Clock: the kernel clock is rcc_hclk3 (RCC_D1CCIPR QSPISEL = 00, the
//! reset value, set again explicitly), [`HCLK_HZ`] = 236 MHz, divided by
//! PRESCALER + 1. ES0392 2.7.6: with HCLK as the kernel clock, a
//! prescaler of 0 does not give a 50% duty cycle, and RM0433 23.5.1 says
//! no odd division factor does; so only even factors are accepted
//! ([`Qspi::init`]).

use crate::clock::{HCLK_HZ, wait_for};
use crate::pac;

/// The NOR's size: 64 Mbit (UM2905 section 5.3.1).
pub const NOR_BYTES: u32 = 8 << 20;
/// DCR FSIZE: 2^(FSIZE + 1) bytes (RM0433 section 23.3.9).
const FSIZE: u32 = NOR_BYTES.trailing_zeros() - 1;

/// The prescaler of the first-contact clock: 236 / 6 = 39.3 MHz, an even
/// factor, under the 50 MHz every common serial NOR allows for its slowest
/// read command (0x03).
pub const PRESCALER_SAFE: u8 = 5;

/// Bounded waits: polls of a status bit. One poll is a few AHB reads, so a
/// million is milliseconds, far more than any read command here takes at
/// the slowest clock (a 256-byte read at 1 MHz is 2 ms).
const POLLS: u32 = 4_000_000;

// QUADSPI_CR (RM0433 section 23.5.1).
const CR_EN: u32 = 1 << 0;
const CR_ABORT: u32 = 1 << 1;
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
const CCR_FMODE_INDIRECT_READ: u32 = 0b01 << 26;
const CCR_DMODE_SHIFT: u32 = 24;
const CCR_DCYC_SHIFT: u32 = 18;
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
            dummy_cycles: 0,
            data_lines: Lines::Single,
        }
    }

    /// The same command with a 24-bit address on one line.
    pub const fn at(self, address: u32) -> Self {
        Read {
            address: Some((address, Lines::Single)),
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

    /// QUADSPI_CCR for this command in indirect-read mode. Never 0: IMODE
    /// is never 00 (ES0392 2.7.3, CCR = 0 hangs the peripheral), and an
    /// instruction phase also keeps clear of ES0392 2.7.2 (a data-only
    /// indirect read hangs).
    fn ccr(&self) -> u32 {
        let (admode, adsize) = match self.address {
            Some((_, l)) => (l as u32, CCR_ADSIZE_24),
            None => (0, 0),
        };
        CCR_FMODE_INDIRECT_READ
            | (self.data_lines as u32) << CCR_DMODE_SHIFT
            | u32::from(self.dummy_cycles & 0x1F) << CCR_DCYC_SHIFT
            | adsize
            | admode << CCR_ADMODE_SHIFT
            | (self.instruction_lines as u32) << CCR_IMODE_SHIFT
            | u32::from(self.instruction)
    }
}

/// What went wrong in a command. After any error the command has been
/// aborted (CR.ABORT) and the peripheral is idle again, if the abort
/// completed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Error {
    /// The peripheral stayed busy before the command started.
    Busy,
    /// TEF: the address and length exceed FSIZE (RM0433 section 23.3.14).
    Transfer,
    /// No data within the bounded wait.
    Timeout,
    /// The dummy-cycle count does not fit DCYC or the address is not
    /// 24-bit, or the buffer is empty.
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

/// Configures one pin of a port as alternate function `af`, very high
/// speed, push-pull, with a pull-up if `pull_up`. The order is that of
/// `uart::init`: AF and output settings first, MODER last, so the pin
/// never drives the AF signal with stale settings.
fn pin_af(gpio: &pac::gpioa::RegisterBlock, pin: u32, af: u32, pull_up: bool) {
    // `field(r, shift, mask, v)`: `r` with one pin's field set to `v`.
    let field = |r: u32, shift: u32, mask: u32, v: u32| (r & !(mask << shift)) | (v << shift);
    let two = pin * 2;
    let pupd = u32::from(pull_up);
    // Each write below is a read-modify-write of one pin's field with a
    // value valid for it (RM0433 section 11.4: AF 0..15; OTYPER 0
    // push-pull; OSPEEDR 0b11 very high; PUPDR 0b00 none or 0b01 pull-up;
    // MODER 0b10 alternate function); the other pins' fields are kept.
    if pin < 8 {
        // SAFETY: see above.
        gpio.afrl
            .modify(|r, w| unsafe { w.bits(field(r.bits(), pin * 4, 0xF, af)) });
    } else {
        // SAFETY: see above.
        gpio.afrh
            .modify(|r, w| unsafe { w.bits(field(r.bits(), (pin - 8) * 4, 0xF, af)) });
    }
    // SAFETY: see above.
    gpio.otyper
        .modify(|r, w| unsafe { w.bits(field(r.bits(), pin, 1, 0)) });
    // SAFETY: see above.
    gpio.ospeedr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0b11)) });
    // SAFETY: see above.
    gpio.pupdr
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, pupd)) });
    // SAFETY: see above.
    gpio.moder
        .modify(|r, w| unsafe { w.bits(field(r.bits(), two, 0b11, 0b10)) });
}

/// Writes QUADSPI_CR.
fn write_cr(v: u32) {
    // SAFETY: every field of CR takes any value it is given here (the
    // reserved bits 2, 5, 13..15, 21 are kept 0 by the callers).
    regs().cr.write(|w| unsafe { w.bits(v) });
}

/// Writes QUADSPI_CCR. Never with 0 (ES0392 2.7.3).
fn write_ccr(v: u32) {
    assert!(v != 0, "QUADSPI_CCR = 0 hangs the QUADSPI (ES0392 2.7.3)");
    // SAFETY: any non-zero CCR value selects a valid frame format.
    regs().ccr.write(|w| unsafe { w.bits(v) });
}

fn sr() -> u32 {
    regs().sr.read().bits()
}

fn not_busy() -> bool {
    wait_for(POLLS, || sr() & SR_BUSY == 0)
}

impl Qspi {
    /// Turns the QUADSPI on and sets it up for the shield's NOR, at
    /// [`HCLK_HZ`] / (`prescaler` + 1). Call once, after [`crate::init`].
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

        // The kernel clock: rcc_hclk3 (RM0433 section 8.7.19, RCC_D1CCIPR
        // QSPISEL = 00), set while the peripheral clock is off. Then the
        // bus clock (RCC_AHB3ENR QSPIEN, 8.7.40), read back so it is on
        // before the reset, and a reset pulse (RCC_AHB3RSTR QSPIRST,
        // 8.7.28) for a known state whatever ran before.
        rcc.d1ccipr.modify(|_, w| w.qspisel().rcc_hclk3());
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

        // DCR (RM0433 23.3.9, 23.3.11): FSIZE for 8 MiB; CSHT for at least
        // 50 ns of NCS high between commands (the common serial NOR
        // minimum after a write-type command, longer than the read-type
        // one); CKMODE 0 (mode 0; serial NORs take mode 0 and 3).
        let clk = HCLK_HZ / (u32::from(prescaler) + 1);
        let csht = clk.div_ceil(20_000_000).clamp(1, 8) - 1;
        // SAFETY: FSIZE 22 and CSHT 0..7 are valid field values; the
        // reserved bits stay 0.
        regs()
            .dcr
            .write(|w| unsafe { w.bits(FSIZE << DCR_FSIZE_SHIFT | csht << DCR_CSHT_SHIFT) });
        // CR (23.3.11): the prescaler; SSHIFT, sampling half a CLK cycle
        // later than the default, for the delay of the path through the
        // shield's connectors (23.3.10; SDR only, DDR needs it clear);
        // FTHRES 0 (FTF with one byte in the FIFO); no interrupts; EN.
        write_cr(u32::from(prescaler) << CR_PRESCALER_SHIFT | CR_SSHIFT | CR_EN);
        Qspi { prescaler }
    }

    /// The PRESCALER field `init` was given.
    pub fn prescaler(&self) -> u8 {
        self.prescaler
    }

    /// The QUADSPI clock, nominal: [`HCLK_HZ`] / (PRESCALER + 1).
    pub fn clock_hz(&self) -> u32 {
        HCLK_HZ / (u32::from(self.prescaler) + 1)
    }

    /// Changes the clock divider (RM0433 23.5.1: PRESCALER only while
    /// BUSY = 0). Same rule as [`Qspi::init`]: an even division factor.
    pub fn set_prescaler(&mut self, prescaler: u8) -> Result<(), Error> {
        assert!(
            prescaler % 2 == 1,
            "QUADSPI prescaler must divide by an even factor"
        );
        if !not_busy() {
            return Err(Error::Busy);
        }
        let cr = regs().cr.read().bits() & !(0xFF << CR_PRESCALER_SHIFT);
        write_cr(cr | u32::from(prescaler) << CR_PRESCALER_SHIFT);
        self.prescaler = prescaler;
        Ok(())
    }

    /// Runs `cmd` in indirect-read mode and fills `buf` with the data
    /// phase's bytes. RM0433 section 23.3.12 "Indirect mode": DLR, CCR,
    /// then AR, which starts the command when there is an address
    /// (otherwise the CCR write does, 23.3.5), then the FIFO through DR.
    pub fn read(&mut self, cmd: &Read, buf: &mut [u8]) -> Result<(), Error> {
        if buf.is_empty()
            || cmd.dummy_cycles > 31
            || cmd.address.is_some_and(|(a, _)| a > 0xFF_FFFF)
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
        write_ccr(cmd.ccr());
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

/// ES0392 section 2.7.4, "QUADSPI internal timing criticality": a
/// sequence to run after reset (and when leaving memory-mapped mode).
/// The CR = 0 that follows the first CCR write must complete within 127
/// kernel clocks (254 CPU cycles here): the three writes are back to
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
    write_ccr(Read::single(0x03).at(0).ccr());
}
