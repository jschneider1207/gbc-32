//! The Macronix MX25L6433F command set and the flash operations built on
//! it: write enable, page program and 4 KiB / 64 KiB erase, in indirect
//! mode only (the NOR is read-only in memory-mapped mode, RM0433 23.3.12).
//! Never chip erase, never a status or configuration register write. The
//! part, its commands and limits: `docs/reference/qspi-nor.md`.
//!
//! The module knows nothing about the controller: it is written against
//! [`SpiFlashBus`], the four primitives it needs (a write command, a
//! register read, an auto-poll of the status register, a guard against
//! memory-mapped mode). [`crate::qspi::Qspi`] (STM32H753 QUADSPI) implements
//! it; Stage 3's H735 has an OCTOSPI and would implement it too (review Q8).
//! The operations are the provided methods of [`NorFlash`], which every
//! [`SpiFlashBus`] gets. The memory-mapped read path does not come through
//! here: it is plain memory reads of [`crate::qspi::WINDOW`].
//!
//! Never sent: chip erase (0x60, 0xC7) and WRSR (0x01). QE is already 1 on
//! this board (`docs/reference/qspi-nor.md` "The quad-enable bit") and
//! status-register writes are non-volatile, so this driver never writes
//! the status or configuration register ([`FORBIDDEN`]; the controller
//! refuses all three opcodes in every mode).
//!
//! Source for the commands, bits and times: MX25L6433F datasheet PM2130
//! Rev. 1.4, Table 4 (command set), section 10-4 (status register: WIP bit
//! 0, WEL bit 1; WEL clears when a program or erase completes; program and
//! erase are ignored without WEL), 10-19 (PP: the CS# rise starts tPP; data
//! past the page end wraps to the page start, so the driver never sends
//! it), 10-26 and Table 10 (security register: P_FAIL bit 5, E_FAIL bit 6,
//! cleared by the next successful operation), and the AC table (p. 66: tPP,
//! tSE, tBE; tSHSL >= 50 ns after write, erase and program, which the
//! controller must give: for the QUADSPI, DCSHT, `qspi::write_dcr`).

/// The NOR's size: 64 Mbit (UM2905 section 5.3.1).
pub const NOR_BYTES: u32 = 8 << 20;
/// The NOR's page: a program never crosses one (MX25L6433F 10-19).
pub const PAGE_BYTES: u32 = 256;
/// The 4 KiB sector of [`NorFlash::erase_4k`] (SE, 0x20) and the 64 KiB
/// block of [`NorFlash::erase_64k`] (BE, 0xD8): defined once, with the
/// loader that erases in these units (`gbc_link_board::loader`, Q11).
pub use gbc_link_board::loader::{BLOCK_BYTES, SECTOR_BYTES};

const CMD_PP: u8 = 0x02;
/// Read status register (also the instruction of the auto-poll).
pub const CMD_RDSR: u8 = 0x05;
const CMD_WREN: u8 = 0x06;
const CMD_SE: u8 = 0x20;
const CMD_RDSCUR: u8 = 0x2B;
const CMD_BE: u8 = 0xD8;
/// Opcodes the controller refuses in every mode: chip erase (0x60, 0xC7)
/// and WRSR (0x01). An indirect read with one of them would send it too.
pub const FORBIDDEN: [u8; 3] = [0x60, 0xC7, 0x01];

/// Status register (RDSR): write in progress, write enable latch.
pub const STATUS_WIP: u8 = 1 << 0;
const STATUS_WEL: u8 = 1 << 1;
/// Security register (RDSCUR, Table 10): the last program or erase failed.
const SCUR_P_FAIL: u8 = 1 << 5;
const SCUR_E_FAIL: u8 = 1 << 6;

/// What went wrong on the bus, in controller-neutral terms. The
/// controller maps its own error to one of these.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BusError {
    /// The controller stayed busy before the command started.
    Busy,
    /// The controller flagged a transfer error.
    Transfer,
    /// No completion within the bounded wait.
    Timeout,
    /// An argument the controller cannot send.
    Argument,
    /// Anything else (e.g. a clock problem).
    Other,
}

/// What went wrong in a flash operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NorError {
    /// A command failed in the controller ([`BusError`]).
    Bus(BusError),
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
    /// The controller is in memory-mapped mode (QUADSPI: CCR.FMODE = 11).
    MemoryMapped,
}

impl From<BusError> for NorError {
    fn from(e: BusError) -> Self {
        NorError::Bus(e)
    }
}

/// The operations [`SpiFlashBus::poll_idle`] waits for, with the
/// MX25L6433F's typical and maximum times (AC table, p. 66).
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

    /// [`NorFlash::wait_idle`]'s timeout: twice the datasheet maximum, in
    /// CPU cycles (2 s at most, well inside the DWT counter's 9.1 s wrap).
    pub const fn timeout_cycles(self) -> u32 {
        2 * self.max_us() * (crate::CPU_HZ / 1_000_000)
    }
}

/// The primitives the NOR operations need from a SPI controller. All
/// commands are 1-line (1-1-1), SDR, no dummy cycles.
pub trait SpiFlashBus {
    /// The runtime half of the indirect-only rule: fails with
    /// [`NorError::MemoryMapped`] if the controller is in memory-mapped mode.
    fn require_indirect(&self) -> Result<(), NorError>;

    /// One write command: the instruction, an optional 24-bit address, and
    /// `data` (none if empty). Never dummy cycles (QUADSPI: ES0392 2.7.1).
    fn write_command(
        &mut self,
        instruction: u8,
        addr: Option<u32>,
        data: &[u8],
    ) -> Result<(), BusError>;

    /// One register byte read with a 1-line instruction (RDSR, RDSCUR).
    fn read_reg(&mut self, instruction: u8) -> Result<u8, BusError>;

    /// Polls the status register ([`CMD_RDSR`]) until `status & mask`
    /// is 0 (no read-modify-write races: the controller does the polling
    /// itself where it can), giving up after `timeout_cycles` CPU cycles
    /// ([`NorError::Timeout`]; the polling is aborted). Returns the last
    /// status byte read.
    fn poll_idle(&mut self, mask: u8, timeout_cycles: u32) -> Result<u8, NorError>;
}

/// The MX25L6433F operations, provided for every [`SpiFlashBus`].
pub trait NorFlash: SpiFlashBus {
    /// WREN (0x06), then RDSR to check that WEL is set (MX25L6433F 10-4:
    /// program and erase are ignored without it).
    fn write_enable(&mut self) -> Result<(), NorError> {
        self.require_indirect()?;
        self.write_command(CMD_WREN, None, &[])?;
        let status = self.read_reg(CMD_RDSR)?;
        if status & STATUS_WEL == 0 {
            return Err(NorError::WelNotSet(status));
        }
        Ok(())
    }

    /// Waits for WIP = 0 ([`SpiFlashBus::poll_idle`]). Gives up after
    /// `op`'s [`Op::timeout_cycles`]. Returns the last status byte read.
    fn wait_idle(&mut self, op: Op) -> Result<u8, NorError> {
        self.require_indirect()?;
        self.poll_idle(STATUS_WIP, op.timeout_cycles())
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

    /// One erase: range and alignment checks, then [`NorFlash::write_op`].
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
    fn erase_4k(&mut self, addr: u32) -> Result<(), NorError> {
        self.erase(addr, SECTOR_BYTES, CMD_SE, Op::SectorErase)
    }

    /// Erases the 64 KiB block at `addr` (BE, 0xD8) to 0xFF; `addr` must
    /// be a multiple of [`BLOCK_BYTES`].
    fn erase_64k(&mut self, addr: u32) -> Result<(), NorError> {
        self.erase(addr, BLOCK_BYTES, CMD_BE, Op::BlockErase)
    }

    /// Programs `data` at `addr` (PP, 0x02, 1-1-1), inside one page: at
    /// most [`PAGE_BYTES`], and `addr % 256 + len <= 256` (the NOR would
    /// wrap the rest to the page start, MX25L6433F 10-19). Programming
    /// only clears bits: the bytes should be erased first. Empty: nothing
    /// is sent.
    fn program_page(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
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
    /// [`NorFlash::program_page`] calls. The range is checked whole first:
    /// nothing is written if it runs past [`NOR_BYTES`].
    fn program(&mut self, addr: u32, data: &[u8]) -> Result<(), NorError> {
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

impl<T: SpiFlashBus> NorFlash for T {}
