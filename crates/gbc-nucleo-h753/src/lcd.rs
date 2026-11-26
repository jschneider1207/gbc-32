//! The X-NUCLEO-GFX02Z1 shield's TFT on an 8080-I 8-bit bus driven by FMC
//! bank 1 (NOR/PSRAM subbank 1, NE1) as an asynchronous 8-bit SRAM (plan
//! Stage 2 M3). Bring-up level: the FMC, the pins, the MPU window, the
//! panel reset and init, commands, register reads and CPU pixel writes and
//! reads. No DMA, no TE synchronisation yet. Facts, arithmetic and
//! measurements: `docs/reference/lcd-panel.md`.
//!
//! The controller: the shield comes in two variants (UM2905 section 6.2):
//! XNGFX02Z1$AZ1 with an ILI9341V and XNGFX02Z1$AZ2 with an ST7789V. This
//! board's answers RDDID (0x04) = 85 85 52, the ST7789V's default ID
//! (ST7789V datasheet V1.3 sections 9.1.46-48), and its TE runs at 60 Hz,
//! the ST7789V's default frame rate (FRCTRL2 0x0F, section 9.2.16; the
//! ILI9341V's default is 70 Hz). Everything this driver sends is common to
//! both: the MIPI DCS commands (SWRESET, SLPOUT, DISPON, CASET, PASET,
//! RAMWR, RAMRD, MADCTL, COLMOD, TEON and the status reads) with the same
//! bit layouts, and the 8080-I timing tables are the same numbers
//! (ILI9341V section 18.3.1; ST7789V section 7.4.1 Table 4, which adds
//! TAHT and TCSH, 10 ns each). Section numbers below are the ILI9341V's
//! unless marked ST7789V.
//!
//! Sources: RM0433 Rev 9 chapter 22 (FMC) and section 8.7 (RCC); DS12117
//! Rev 10 Table 9 (alternate functions) and Tables 55/57 (FMC asynchronous
//! timings); ES0392 Rev 16 section 2.6 (FMC errata); UM2905 Rev 2 Table 2
//! (the shield's LCD wiring); ILI9341V datasheet V1.00 (section 7.6.3
//! 8-bit data formats, 8 commands, 15.4 reset, 18.3.1 8080-I timing);
//! ST7789V datasheet V1.3 (Sitronix, 2014; not in the hardware repo's
//! datasheet folder: `docs/reference/lcd-panel.md` says where from).
//!
//! Pins (UM2905 Table 2; AF12 is the FMC on every one, DS12117 Table 9):
//!
//! | LCD    | signal  | GPIO | function           |
//! |--------|---------|------|--------------------|
//! | DB0-3  | FMC_D0-3| PD14 PD15 PD0 PD1 | AF12  |
//! | DB4-7  | FMC_D4-7| PE7 PE8 PE9 PE10  | AF12  |
//! | RD     | FMC_NOE | PD4  | AF12               |
//! | WR     | FMC_NWE | PD5  | AF12               |
//! | CS     | FMC_NE1 | PD7  | AF12               |
//! | RS D/CX| FMC_A20 | PE4  | AF12               |
//! | RESET  | GPIO out| PE11 | (AF12 would be FMC_D8) |
//! | FMARK TE| GPIO in| PE13 | (AF12 would be FMC_D10)|
//!
//! Addresses: bank 1 subbank 1 is 0x6000_0000 to 0x63FF_FFFF in the
//! default mapping (BCR1.BMAP = 00, RM0433 Table 156). For an 8-bit bank
//! the external address is the AXI byte address ADDR[25:0] itself (Table
//! 158), so FMC_A20 is address bit 20: D/CX low (command) at
//! [`CMD_ADDR`] = 0x6000_0000, high (data) at [`DATA_ADDR`] =
//! 0x6010_0000. Only A20 is routed to a pin; the other address lines stay
//! GPIO.
//!
//! The window is MPU region [`crate::mpu::REGION_FMC`] (Device; see
//! `mpu::fmc_window`), opened only while bank 1 is configured.
//!
//! Errata (ES0392 2.6): only 2.6.4 (unaligned FMC reads unsupported)
//! applies to an asynchronous SRAM bank; every read here is a byte read
//! and Device memory faults on unaligned accesses anyway. 2.6.1, 2.6.3
//! and 2.6.5 concern synchronous memories and the continuous clock (not
//! used: CCLKEN = 0), 2.6.2, 2.6.6 and 2.6.7 NAND and SDRAM.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::gpio::{pin_af, pin_input, pin_output};
use crate::{CPU_HZ, cycles, mpu, pac, wait_since};

/// FMC bank 1, NOR/PSRAM subbank 1 (NE1), default mapping.
pub const WINDOW: usize = 0x6000_0000;
/// The MPU window: 2 MiB, enough for A20 (and a power of two).
pub const WINDOW_BYTES: u32 = 2 << 20;
/// D/CX = 0: the command register.
pub const CMD_ADDR: usize = WINDOW;
/// D/CX = 1 (FMC_A20 high): parameters and pixel data.
pub const DATA_ADDR: usize = WINDOW | 1 << 20;
const _: () = assert!(DATA_ADDR - WINDOW < WINDOW_BYTES as usize);

/// The panel, native orientation (both controllers: 240 RGB columns x 320
/// lines).
pub const WIDTH: u16 = 240;
/// See [`WIDTH`].
pub const HEIGHT: u16 = 320;

/// MADCTL (0x36, §8.2.29; ST7789V 9.1.28, the same bits): MY = MX = MV = 0, the native portrait
/// scan (240-pixel rows written in the order the panel scans them, which
/// the M3 tearing plan relies on), ML = MH = 0 (refresh top to bottom,
/// left to right), BGR = 1 (plan M3: the core's R-low RGB555 order goes
/// out without a channel swap). Which way up the image appears, and which
/// colour the top 5-bit field shows, is a human check
/// (`docs/reference/lcd-panel.md`).
pub const MADCTL: u8 = 0x08;
/// COLMOD (0x3A, §8.2.33; ST7789V 9.1.32): RGB-interface and MCU-interface
/// formats 101, 16 bits per pixel (RGB565, two bytes per pixel on the
/// 8-bit bus, high byte first, §7.6.3; ST7789V 8.8.3).
pub const COLMOD: u8 = 0x55;

// FMC_BCR1 (RM0433 22.7.6), mode A/D asynchronous SRAM (Table 174).
const BCR_FMCEN: u32 = 1 << 31;
const BCR_BMAP_MASK: u32 = 0b11 << 24;
const BCR_WFDIS: u32 = 1 << 21;
const BCR_CCLKEN: u32 = 1 << 20;
const BCR_CBURSTRW: u32 = 1 << 19;
const BCR_CPSIZE_MASK: u32 = 0b111 << 16;
const BCR_ASYNCWAIT: u32 = 1 << 15;
const BCR_EXTMOD: u32 = 1 << 14;
const BCR_WAITEN: u32 = 1 << 13;
const BCR_WREN: u32 = 1 << 12;
const BCR_WAITCFG: u32 = 1 << 11;
const BCR_WAITPOL: u32 = 1 << 9;
const BCR_BURSTEN: u32 = 1 << 8;
/// Reserved, "0x1" in Table 174 (and set in the reset value 0x30DB).
const BCR_RES7: u32 = 1 << 7;
const BCR_FACCEN: u32 = 1 << 6;
const BCR_MWID_MASK: u32 = 0b11 << 4;
const BCR_MTYP_MASK: u32 = 0b11 << 2;
const BCR_MUXEN: u32 = 1 << 1;
const BCR_MBKEN: u32 = 1 << 0;
/// Every BCR1 field this driver sets (the reserved bits are kept).
const BCR_FIELDS: u32 = BCR_FMCEN
    | BCR_BMAP_MASK
    | BCR_WFDIS
    | BCR_CCLKEN
    | BCR_CBURSTRW
    | BCR_CPSIZE_MASK
    | BCR_ASYNCWAIT
    | BCR_EXTMOD
    | BCR_WAITEN
    | BCR_WREN
    | BCR_WAITCFG
    | BCR_WAITPOL
    | BCR_BURSTEN
    | BCR_RES7
    | BCR_FACCEN
    | BCR_MWID_MASK
    | BCR_MTYP_MASK
    | BCR_MUXEN
    | BCR_MBKEN;
/// BMAP 00 (default mapping), write FIFO on, no clock, no wait, EXTMOD (read
/// and write timings apart), WREN, MWID 00 (8 bits), MTYP 00 (SRAM), not
/// multiplexed, bank enabled. FMCEN separately.
const BCR_VALUE: u32 = BCR_EXTMOD | BCR_WREN | BCR_RES7 | BCR_MBKEN;
// FMC_BTR1 / FMC_BWTR1 fields (22.7.6).
const TR_ACCMOD_SHIFT: u32 = 28;
const TR_BUSTURN_SHIFT: u32 = 16;
const TR_DATAST_SHIFT: u32 = 8;
const TR_ADDHLD_SHIFT: u32 = 4;
/// ACCMOD, BUSTURN, DATAST, ADDHLD, ADDSET (BTR also has DATLAT and
/// CLKDIV, don't care in asynchronous mode and kept at their reset value;
/// BWTR's bits 27:20 are reserved, kept).
const TR_FIELDS: u32 = 0b11 << TR_ACCMOD_SHIFT | 0xF << TR_BUSTURN_SHIFT | 0xFFFF;
const ACCMOD_A: u32 = 0b00;
const ACCMOD_D: u32 = 0b11;
// FMC_SR (22.8.7): the write FIFO is empty.
const SR_FEMPT: u32 = 1 << 6;

/// The FMC kernel clock (RCC_D1CCIPR FMCSEL, RM0433 8.7.19).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kernel {
    /// rcc_hclk3 (FMCSEL 00, the reset value): 236 MHz nominal.
    Hclk3,
    /// per_ck with CKPERSEL 00, hsi_ker_ck: 64 MHz nominal.
    PerHsi,
}

impl Kernel {
    /// The nominal frequency.
    pub const fn hz(self) -> u32 {
        match self {
            Kernel::Hclk3 => crate::clock::HCLK_HZ,
            Kernel::PerHsi => crate::clock::HSI_HZ,
        }
    }

    /// The highest frequency the part allows: both come from the HSI, at
    /// most +1.5% (64.96 MHz; `clock` module docs, DS12117 Table 133).
    /// Every minimum time is met at this clock.
    pub const fn max_hz(self) -> u32 {
        (self.hz() as u64 * 1015 / 1000) as u32
    }
}

/// The 8080-I minimum times this driver meets, in ns: ILI9341V §18.3.1,
/// the same values in ST7789V Table 4.
mod ili {
    /// Write cycle.
    pub const TWC: u32 = 66;
    /// WRX low / high.
    pub const TWRL: u32 = 15;
    pub const TWRH: u32 = 15;
    /// CSX setup before WRX (write).
    pub const TCS: u32 = 15;
    /// Write data setup before WRX rising.
    pub const TDST: u32 = 10;
    /// Frame-memory read: RDX low, RDX high, cycle; read access time
    /// (a maximum: data valid after RDX falling).
    pub const TRDLFM: u32 = 355;
    pub const TRDHFM: u32 = 90;
    pub const TRCFM: u32 = 450;
    pub const TRATFM: u32 = 340;
    /// Read output disable time (a maximum: the panel may drive the bus
    /// this long after RDX rising).
    pub const TROD: u32 = 80;
}

/// DS12117 Table 55: data setup before NOE high the FMC needs (11 ns);
/// Table 57: data valid at most Tker + 2.5 ns after NE low.
const FMC_TSU_DATA_NOE: u32 = 11;
const FMC_TV_DATA_NE_PS: u32 = 2_500;

/// ceil(ns x hz / 1e9): cycles of `hz` covering `ns`.
const fn cyc(ns: u32, hz: u32) -> u32 {
    (ns as u64 * hz as u64).div_ceil(1_000_000_000) as u32
}

/// `ns` stretched by `pct` percent.
const fn stretch(ns: u32, pct: u32) -> u32 {
    ns * (100 + pct) / 100
}

const fn max(a: u32, b: u32) -> u32 {
    if a > b { a } else { b }
}

/// FMC bank 1 timings (fmc_ker_ck cycles) for one kernel clock: writes in
/// mode A (RM0433 Figure 102: WR low DATAST, then high for 1 + the next
/// ADDSET; a cycle is ADDSET + DATAST + 1), reads in mode D (Figure 108:
/// RD high for ADDSET + ADDHLD, low for DATAST; BUSTURN after each read,
/// which mode D inserts, 22.7.6 BUSTURN). Mode D for reads because the
/// frame-memory read needs RD high for 90 ns, more than mode A's ADDSET
/// (at most 15 cycles = 64 ns at 236 MHz) gives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Timing {
    pub name: &'static str,
    pub kernel: Kernel,
    /// Every panel minimum is stretched by this many percent.
    pub slack_pct: u32,
    pub w_addset: u32,
    pub w_datast: u32,
    pub r_addset: u32,
    pub r_addhld: u32,
    pub r_datast: u32,
    pub r_busturn: u32,
}

impl Timing {
    /// The shortest timing that meets the panel minimums (plus `slack_pct`)
    /// at `kernel`'s highest frequency. Panics (at compile time for the
    /// constants) if a field would not fit.
    pub const fn new(name: &'static str, kernel: Kernel, slack_pct: u32) -> Timing {
        let hz = kernel.max_hz();
        // Write: DATAST >= twrl; ADDSET + 1 >= twrh; ADDSET >= tcs (CS
        // falls with the transaction, WR after ADDSET); the cycle
        // ADDSET + DATAST + 1 >= twc; data, valid Tker + 2.5 ns after NE
        // low, set up tdst before WR rises at (ADDSET + DATAST) Tker. The
        // spare cycles go half to each phase.
        let datast_min = cyc(stretch(ili::TWRL, slack_pct), hz);
        let addset_min = max(
            cyc(stretch(ili::TCS, slack_pct), hz),
            cyc(stretch(ili::TWRH, slack_pct), hz).saturating_sub(1),
        );
        let total = max(
            cyc(stretch(ili::TWC, slack_pct), hz),
            addset_min + datast_min + 1,
        );
        let rest = total - 1;
        let w_datast = max(datast_min, rest / 2);
        let w_addset = max(addset_min, rest - w_datast);
        // tdst: (ADDSET + DATAST) x T >= T + 2.5 ns + tdst.
        let t_ps = 1_000_000_000_000 / hz as u64;
        assert!(
            (w_addset + w_datast) as u64 * t_ps
                >= t_ps + FMC_TV_DATA_NE_PS as u64 + stretch(ili::TDST, slack_pct) as u64 * 1000
        );
        assert!(w_addset <= 15 && w_datast >= 1 && w_datast <= 255);
        // Read: DATAST >= trdlfm and >= tratfm + the FMC's data setup;
        // ADDSET + ADDHLD >= trdhfm (ADDSET >= 1 and ADDHLD >= 1 in mode
        // D); the cycle >= trcfm; BUSTURN covers trod, so the panel has
        // let go of the bus before the FMC drives it again (at most 15).
        let r_datast = max(
            cyc(stretch(ili::TRDLFM, slack_pct), hz),
            cyc(ili::TRATFM + FMC_TSU_DATA_NOE, hz),
        );
        let high = max(
            max(cyc(stretch(ili::TRDHFM, slack_pct), hz), 2),
            cyc(stretch(ili::TRCFM, slack_pct), hz).saturating_sub(r_datast),
        );
        let r_addset = high.div_ceil(2);
        let r_addhld = high - r_addset;
        let r_busturn = if cyc(ili::TROD, hz) > 15 {
            15
        } else {
            cyc(ili::TROD, hz)
        };
        assert!(r_addset >= 1 && r_addset <= 15 && r_addhld >= 1 && r_addhld <= 15);
        assert!(r_datast >= 1 && r_datast <= 255);
        Timing {
            name,
            kernel,
            slack_pct,
            w_addset,
            w_datast,
            r_addset,
            r_addhld,
            r_datast,
            r_busturn,
        }
    }

    /// fmc_ker_ck cycles per byte written (mode A: ADDSET + DATAST + 1).
    pub const fn write_cycles(&self) -> u32 {
        self.w_addset + self.w_datast + 1
    }

    /// Bytes per second the bus can write at the nominal kernel clock (an
    /// upper bound: the FMC inserts no gap between consecutive writes to
    /// one bank, RM0433 22.7.6 BUSTURN).
    pub const fn write_bytes_per_s(&self) -> u32 {
        self.kernel.hz() / self.write_cycles()
    }

    /// FMC_BTR1 (reads, mode D), the managed fields.
    const fn btr(&self) -> u32 {
        ACCMOD_D << TR_ACCMOD_SHIFT
            | self.r_busturn << TR_BUSTURN_SHIFT
            | self.r_datast << TR_DATAST_SHIFT
            | self.r_addhld << TR_ADDHLD_SHIFT
            | self.r_addset
    }

    /// FMC_BWTR1 (writes, mode A), the managed fields. ADDHLD is don't
    /// care in mode A (Table 176); 1, since 0 is reserved. BUSTURN 0: no
    /// gap is needed after a write (tchw 0 ns; a following read's
    /// RD stays high for its own ADDSET + ADDHLD).
    const fn bwtr(&self) -> u32 {
        ACCMOD_A << TR_ACCMOD_SHIFT
            | self.w_datast << TR_DATAST_SHIFT
            | 1 << TR_ADDHLD_SHIFT
            | self.w_addset
    }
}

/// The fast timing: rcc_hclk3 at 236 MHz, every panel minimum met at
/// 239.6 MHz except the hold times after WR rising, which the FMC cannot
/// stretch (mode A ends one kernel cycle after NWE rises, RM0433 Figure
/// 102): data hold tdht 10 ns against the FMC's guaranteed Tker + 0.5 =
/// 4.7 ns (DS12117 Table 57 th(Data_NWE)), D/CX hold (ST7789V TAHT 10 ns)
/// against Tker - 0.5 = 3.7 ns (th(A_NWE)), CS hold and wait (TCSH, tcsf
/// 10 ns) against Tker = 4.2 ns (th(NE_NWE)). Out of spec, though no
/// error was seen (`docs/reference/lcd-panel.md`); the fastest.
pub const FAST: Timing = Timing::new("hclk3", Kernel::Hclk3, 0);
/// Every panel minimum met, the holds included: per_ck (the HSI, 64 MHz),
/// whose 15.4 ns cycle (at 64.96 MHz) covers tdht, TAHT, TCSH and tcsf.
pub const SPEC: Timing = Timing::new("perck", Kernel::PerHsi, 0);
/// The conservative fallback: as [`SPEC`] with every minimum doubled.
pub const SAFE: Timing = Timing::new("perck-x2", Kernel::PerHsi, 100);

/// The register block. FMC is at 0x5200_4000 (RM0433 Table 8), inside the
/// MPU's Device region for the peripherals.
fn fmc() -> &'static pac::fmc::RegisterBlock {
    // SAFETY: a shared reference to the memory-mapped register block, valid
    // for the whole program; every access is a volatile register access
    // through the PAC, and only the [`Lcd`] owner touches it.
    unsafe { &*pac::FMC::ptr() }
}

fn rcc() -> &'static pac::rcc::RegisterBlock {
    // SAFETY: as `fmc`; the RCC fields written here (AHB3ENR FMCEN,
    // AHB3RSTR FMCRST, D1CCIPR FMCSEL and CKPERSEL) belong to this driver
    // (`qspi` writes QSPISEL/QSPIEN with read-modify-writes; one thread).
    unsafe { &*pac::RCC::ptr() }
}

fn gpiod() -> &'static pac::gpioa::RegisterBlock {
    // SAFETY: as `fmc`; the pins this driver sets (PD0 PD1 PD4 PD5 PD7
    // PD14 PD15) belong to nobody else (UART PD8 PD9, QSPI PD11-13).
    unsafe { &*pac::GPIOD::ptr() }
}

fn gpioe() -> &'static pac::gpioa::RegisterBlock {
    // SAFETY: as `gpiod`: PE4 PE7-11 PE13 (LED PE1, QSPI PE2).
    unsafe { &*pac::GPIOE::ptr() }
}

/// Busy-waits `us` microseconds on the cycle counter.
pub(crate) fn delay_us(us: u32) {
    wait_since(cycles(), us * (CPU_HZ / 1_000_000));
}

/// The panel driver. Only one exists ([`Lcd::init`] panics on a second
/// call).
pub struct Lcd {
    timing: Timing,
}

impl Lcd {
    /// Sets up the FMC, the pins and the MPU window for `timing`, then
    /// resets and initialises the panel ([`Lcd::init_panel`]). Call once,
    /// after [`crate::init`].
    pub fn init(timing: Timing) -> Lcd {
        static DONE: AtomicBool = AtomicBool::new(false);
        assert!(!DONE.swap(true, Ordering::AcqRel), "Lcd::init called twice");
        // GPIO clocks D and E are on since `crate::init` (RCC_AHB4ENR).
        // RESET high (inactive) before it becomes an output; TE an input.
        pin_output(gpioe(), 11, true); // PE11 DISP_NRESET
        pin_input(gpioe(), 13); // PE13 DISP_TE
        // CS with a pull-up, so the panel stays deselected if the pin is
        // ever undriven; the rest without pulls.
        pin_af(gpiod(), 7, 12, true); // PD7 FMC_NE1
        pin_af(gpiod(), 4, 12, false); // PD4 FMC_NOE
        pin_af(gpiod(), 5, 12, false); // PD5 FMC_NWE
        pin_af(gpioe(), 4, 12, false); // PE4 FMC_A20 (D/CX)
        for pin in [14, 15, 0, 1] {
            pin_af(gpiod(), pin, 12, false); // PD14 PD15 PD0 PD1 FMC_D0-3
        }
        for pin in [7, 8, 9, 10] {
            pin_af(gpioe(), pin, 12, false); // PE7-10 FMC_D4-7
        }
        let mut lcd = Lcd { timing };
        lcd.configure(timing, true);
        lcd.init_panel();
        lcd
    }

    /// The timing in use.
    pub fn timing(&self) -> Timing {
        self.timing
    }

    /// Switches to another timing (and kernel clock): the window closed,
    /// the FMC reset and set up again, the window reopened. The panel keeps
    /// its state.
    pub fn set_timing(&mut self, timing: Timing) {
        self.configure(timing, false);
        self.timing = timing;
    }

    /// (BCR1, BTR1, BWTR1, D1CCIPR) as programmed, for the report.
    pub fn registers(&self) -> (u32, u32, u32, u32) {
        let f = fmc();
        (
            f.bcr1.read().bits(),
            f.btr1.read().bits(),
            f.bwtr1.read().bits(),
            rcc().d1ccipr.read().bits(),
        )
    }

    fn configure(&mut self, t: Timing, first: bool) {
        if !first {
            // No access may start once the window is closed; the write
            // FIFO drains before the bank is touched (FMC_SR FEMPT).
            mpu::fmc_window(false);
            let mut n = 0u32;
            while fmc().sr.read().bits() & SR_FEMPT == 0 && n < 1_000_000 {
                n += 1;
            }
        }
        let rcc = rcc();
        // The kernel clock is switched with the FMC's clock off (RM0433
        // 8.7.19 allows it on the fly, but nothing needs that). Then the
        // bus clock (AHB3ENR FMCEN, 8.7.40), read back, and a reset pulse
        // (AHB3RSTR FMCRST, 8.7.28) for a known state: BCR1 = 0x30DB with
        // FMCEN = 0 (22.7.6).
        rcc.ahb3enr.modify(|_, w| w.fmcen().clear_bit());
        let _ = rcc.ahb3enr.read();
        rcc.d1ccipr.modify(|_, w| {
            let w = w.ckpersel().hsi();
            match t.kernel {
                Kernel::Hclk3 => w.fmcsel().rcc_hclk3(),
                Kernel::PerHsi => w.fmcsel().per(),
            }
        });
        rcc.ahb3enr.modify(|_, w| w.fmcen().set_bit());
        let _ = rcc.ahb3enr.read();
        rcc.ahb3rstr.modify(|_, w| w.fmcrst().set_bit());
        rcc.ahb3rstr.modify(|_, w| w.fmcrst().clear_bit());
        let _ = rcc.ahb3rstr.read();

        let f = fmc();
        // Timings first, then BCR1 with the bank configured, then FMCEN
        // (Table 174 lists FMCEN in BCR1; the FMC is off until then).
        // SAFETY: the managed fields get values valid for them (checked in
        // `Timing::new`); the reserved and don't-care bits are kept.
        f.btr1
            .modify(|r, w| unsafe { w.bits(r.bits() & !TR_FIELDS | t.btr()) });
        // SAFETY: as above.
        f.bwtr1
            .modify(|r, w| unsafe { w.bits(r.bits() & !TR_FIELDS | t.bwtr()) });
        // SAFETY: as above; BCR_VALUE is the Table 174 configuration.
        f.bcr1
            .modify(|r, w| unsafe { w.bits(r.bits() & !BCR_FIELDS | BCR_VALUE) });
        // SAFETY: as above.
        f.bcr1
            .modify(|r, w| unsafe { w.bits(r.bits() | BCR_FMCEN) });
        let _ = f.bcr1.read();
        mpu::fmc_window(true);
    }

    /// Hardware reset and the init sequence: RESX low 1 ms (tRW >= 10 us,
    /// §15.4; ST7789V 7.4.5), high and 120 ms (tRT, the NV-memory load);
    /// SWRESET (0x01) and 120 ms (§8.2.2: 5 ms, 120 ms before SLPOUT if it
    /// was sent in sleep-out mode); COLMOD
    /// [`COLMOD`]; MADCTL [`MADCTL`]; SLPOUT (0x11) and 120 ms (§8.2.12:
    /// 5 ms before the next command, 120 ms before SLPIN); DISPON (0x29);
    /// TEON (0x35, mode 0: V-blank only). The power, VCOM and gamma
    /// registers keep their reset values (loaded from the module's NV
    /// memory at reset, §15.4 note 1).
    pub fn init_panel(&mut self) {
        let e = gpioe();
        // SAFETY: BSRR is write-only; bit 27 resets PE11, bit 11 sets it.
        e.bsrr.write(|w| unsafe { w.bits(1 << (11 + 16)) });
        delay_us(1_000);
        // SAFETY: as above.
        e.bsrr.write(|w| unsafe { w.bits(1 << 11) });
        delay_us(120_000);
        self.command(0x01, &[]);
        delay_us(120_000);
        self.command(0x3A, &[COLMOD]);
        self.command(0x36, &[MADCTL]);
        self.command(0x11, &[]);
        delay_us(120_000);
        self.command(0x29, &[]);
        self.command(0x35, &[0x00]);
    }

    /// One command byte (D/CX low).
    #[inline(always)]
    pub fn write_cmd(&mut self, c: u8) {
        // SAFETY: CMD_ADDR is inside the MPU window, which is open while
        // the bank is configured (always, outside `configure`); an aligned
        // byte store to Device memory.
        unsafe { core::ptr::write_volatile(CMD_ADDR as *mut u8, c) }
    }

    /// One parameter or pixel byte (D/CX high).
    #[inline(always)]
    pub fn write_data(&mut self, b: u8) {
        // SAFETY: as `write_cmd`, for DATA_ADDR.
        unsafe { core::ptr::write_volatile(DATA_ADDR as *mut u8, b) }
    }

    /// Four data bytes in one store: the FMC splits an AXI access wider
    /// than the memory into memory-width accesses (RM0433 22.7.2, Table
    /// 171), lowest address first, so the low byte of `w` goes out first
    /// (little-endian; the readback test checks the order).
    #[inline(always)]
    pub fn write_data_u32(&mut self, w: u32) {
        // SAFETY: as `write_data`; DATA_ADDR is word aligned.
        unsafe { core::ptr::write_volatile(DATA_ADDR as *mut u32, w) }
    }

    /// One data byte read (D/CX high, RD strobed). The caller sent the
    /// read command; the first byte after it is the dummy byte.
    #[inline(always)]
    pub fn read_data(&mut self) -> u8 {
        // SAFETY: as `write_data`; a byte load (ES0392 2.6.4).
        unsafe { core::ptr::read_volatile(DATA_ADDR as *const u8) }
    }

    /// A command and its parameters.
    pub fn command(&mut self, c: u8, params: &[u8]) {
        self.write_cmd(c);
        for &p in params {
            self.write_data(p);
        }
    }

    /// A read command: the dummy byte is discarded, then `out` is filled.
    pub fn read(&mut self, c: u8, out: &mut [u8]) {
        self.write_cmd(c);
        self.begin_read();
        let _ = self.read_data();
        for b in out.iter_mut() {
            *b = self.read_data();
        }
        self.end_read();
    }

    /// Before the first read after a write: the write has reached the FMC
    /// (DSB).
    pub fn begin_read(&mut self) {
        cortex_m::asm::dsb();
    }

    /// After the last read of a burst: the panel may drive the bus up to
    /// trod = 80 ns after RD rises (§18.3.1; ST7789V TODH), longer than BUSTURN
    /// covers at 236 MHz (15 cycles, 64 ns); 1 us before the next write.
    pub fn end_read(&mut self) {
        delay_us(1);
    }

    /// CASET (0x2A) and PASET (0x2B): the window, inclusive.
    pub fn set_window(&mut self, x0: u16, y0: u16, x1: u16, y1: u16) {
        let [a, b] = x0.to_be_bytes();
        let [c, d] = x1.to_be_bytes();
        self.command(0x2A, &[a, b, c, d]);
        let [a, b] = y0.to_be_bytes();
        let [c, d] = y1.to_be_bytes();
        self.command(0x2B, &[a, b, c, d]);
    }

    /// RAMWR (0x2C): pixel data follows.
    pub fn begin_pixels(&mut self) {
        self.write_cmd(0x2C);
    }

    /// RAMRD (0x2E) with the dummy byte read: pixel data follows, 3 bytes
    /// per pixel ([`expected_readback`]).
    pub fn begin_readback(&mut self) {
        self.write_cmd(0x2E);
        self.begin_read();
        let _ = self.read_data();
    }

    /// `n` pixels of one RGB565 colour after [`Lcd::begin_pixels`], by
    /// 32-bit stores (two pixels each, high byte first).
    pub fn fill(&mut self, rgb565: u16, n: u32) {
        let [hi, lo] = rgb565.to_be_bytes();
        let w = u32::from_le_bytes([hi, lo, hi, lo]);
        for _ in 0..n / 2 {
            self.write_data_u32(w);
        }
        if n % 2 == 1 {
            self.write_data(hi);
            self.write_data(lo);
        }
    }

    /// One RGB565 pixel, high byte first (§7.6.3; ST7789V 8.8.3).
    #[inline(always)]
    pub fn pixel(&mut self, rgb565: u16) {
        let [hi, lo] = rgb565.to_be_bytes();
        self.write_data(hi);
        self.write_data(lo);
    }

    /// The ST7789V normal-mode frame rate: PORCTRL (B2h; BPA, FPA, PSEN 0,
    /// and the idle and partial porches at their reset value 33h) and
    /// FRCTRL2 (C6h; NLA 000, dot inversion, the reset value 0Fh has it, and
    /// RTNA). ST7789V datasheet V1.3 sections 9.2.3 and 9.2.16. Not an
    /// ILI9341V command set (its B2h is FRMCTR2, C6h undefined).
    pub fn set_frame_rate(&mut self, f: FrameRate) {
        self.command(0xB2, &[f.bpa, f.fpa, 0x00, 0x33, 0x33]);
        self.command(0xC6, &[f.rtna]);
    }

    /// The TE (FMARK) pin's level.
    pub fn te(&self) -> bool {
        gpioe().idr.read().bits() & (1 << 13) != 0
    }
}

/// What RAMRD returns for a pixel written as `rgb565` (§8.2.24 and §7.6.3
/// 18-bit format; ST7789V 9.1.23): three bytes, R, G, B, each a 6-bit value in
/// D7..D2 (D1, D0 undefined: compare under [`READBACK_MASK`]). The 5-bit
/// red and blue fields come back with green's LSB as their new LSB
/// (measured on this board, `docs/reference/lcd-panel.md`: neither
/// datasheet states the 16 -> 18 bit rule in text, and the ILI9341V's
/// RGBSET LUT has no effect here). Frame memory holds what was written; MADCTL.BGR
/// only changes the panel output (§8.2.29).
pub const fn expected_readback(rgb565: u16) -> [u8; 3] {
    let r = (rgb565 >> 11) as u8 & 0x1F;
    let g = (rgb565 >> 5) as u8 & 0x3F;
    let b = rgb565 as u8 & 0x1F;
    let l = g & 1;
    [(r << 1 | l) << 2, g << 2, (b << 1 | l) << 2]
}

/// The bits of a readback byte that carry data (D7..D2).
pub const READBACK_MASK: u8 = 0xFC;

/// The ST7789V's normal-mode frame timing (datasheet V1.3 9.2.16 note 1):
/// a frame is `320 + FPA + BPA` lines of `250 + 16 RTNA` clocks of the
/// panel's internal oscillator (nominally 10 MHz, so the reset values
/// FPA = BPA = 0Ch, RTNA = 0Fh give 10 MHz / (344 x 490) = 59.3 Hz; the
/// datasheet's table rounds that to 60). TE is high for the porch lines
/// (V-blank, section 8.15).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameRate {
    /// FRCTRL2 RTNA[4:0], 0..=31.
    pub rtna: u8,
    /// PORCTRL FPA[6:0], front porch lines, 1..=127.
    pub fpa: u8,
    /// PORCTRL BPA[6:0], back porch lines, 1..=127.
    pub bpa: u8,
}

impl FrameRate {
    /// The reset values (9.2.3, 9.2.16).
    pub const DEFAULT: FrameRate = FrameRate {
        rtna: 0x0F,
        fpa: 0x0C,
        bpa: 0x0C,
    };

    /// The setting closest to the Game Boy's 59.7275 Hz (16,742.7 µs) on
    /// this board's panel: RTNA 0Eh (474-clock lines), FPA = BPA = 11h
    /// (354 lines). Its oscillator measured 10.02 MHz (the reset setting
    /// gave 16,811 to 16,823 µs, 59.45 Hz), and this one 16,735 to 16,749
    /// µs (mean of 120 TE periods, four runs of `lcd-dma-probe`; ADR
    /// 0039): within 0.04% of the target, less than the oscillator's own
    /// run-to-run drift (0.08%). One porch line moves the frame 0.28%, one
    /// RTNA step 3.3%.
    pub const GAME_BOY: FrameRate = FrameRate {
        rtna: 0x0E,
        fpa: 0x11,
        bpa: 0x11,
    };

    /// Oscillator clocks per line.
    pub const fn line_clocks(&self) -> u32 {
        250 + 16 * self.rtna as u32
    }

    /// Lines per frame, the porches included.
    pub const fn lines(&self) -> u32 {
        HEIGHT as u32 + self.fpa as u32 + self.bpa as u32
    }

    /// Oscillator clocks per frame.
    pub const fn frame_clocks(&self) -> u32 {
        self.lines() * self.line_clocks()
    }

    /// The setting whose frame is closest to `target_ns` with a panel
    /// oscillator of `osc_hz`, among those whose line lasts at least
    /// `min_line_ns` and whose porches add up to at most `max_porch` lines
    /// (split evenly, the back porch taking the odd line). On a tie the
    /// RTNA nearest the reset value wins, then the shorter porch.
    pub fn closest(osc_hz: u32, target_ns: u32, min_line_ns: u32, max_porch: u32) -> Option<Self> {
        let ns = |clocks: u32| clocks as u64 * 1_000_000_000 / osc_hz.max(1) as u64;
        let mut best: Option<(u64, FrameRate)> = None;
        // RTNA in the order 15, 14, 16, 13, 17, ...: on a tie the one
        // nearest the reset value wins.
        for step in 0..64u32 {
            let off = step.div_ceil(2) as i32 * if step % 2 == 1 { -1 } else { 1 };
            let rtna = 0x0F + off;
            if !(0..=31).contains(&rtna) {
                continue;
            }
            for porch in 2..=max_porch.min(254) {
                let f = FrameRate {
                    rtna: rtna as u8,
                    fpa: (porch / 2) as u8,
                    bpa: porch.div_ceil(2) as u8,
                };
                if ns(f.line_clocks()) < min_line_ns as u64 {
                    continue;
                }
                let err = ns(f.frame_clocks()).abs_diff(target_ns as u64);
                if best.is_none_or(|(e, _)| err < e) {
                    best = Some((err, f));
                }
            }
        }
        best.map(|(_, f)| f)
    }
}
