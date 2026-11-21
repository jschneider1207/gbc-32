//! SM83 CPU. Skeleton only: instruction execution arrives in M1.

/// A snapshot of the CPU registers ([`Emulator::registers`](crate::Emulator::registers)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Registers {
    pub a: u8,
    pub f: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub h: u8,
    pub l: u8,
    pub sp: u16,
    pub pc: u16,
}

impl Registers {
    const fn zero() -> Self {
        Self {
            a: 0,
            f: 0,
            b: 0,
            c: 0,
            d: 0,
            e: 0,
            h: 0,
            l: 0,
            sp: 0,
            pc: 0,
        }
    }

    /// CGB register state after the boot ROM hands over to a CGB-mode
    /// cartridge (Pan Docs: Power Up Sequence). DMG compatibility mode
    /// differs; M1 refines this.
    pub(crate) const fn post_boot_cgb() -> Self {
        Self {
            a: 0x11,
            f: 0x80,
            b: 0x00,
            c: 0x00,
            d: 0xFF,
            e: 0x56,
            h: 0x00,
            l: 0x0D,
            sp: 0xFFFE,
            pc: 0x0100,
        }
    }
}

pub(crate) struct Cpu {
    pub(crate) regs: Registers,
    /// Locked up after an invalid opcode; only a reset recovers.
    pub(crate) locked: bool,
    /// CGB double-speed mode (KEY1): one M-cycle is 2 base clocks instead
    /// of 4.
    pub(crate) double_speed: bool,
}

impl Cpu {
    pub(crate) const fn new() -> Self {
        Self {
            regs: Registers::zero(),
            locked: false,
            double_speed: false,
        }
    }

    pub(crate) fn reset(&mut self) {
        *self = Self::new();
        self.regs = Registers::post_boot_cgb();
    }

    /// Base clocks per M-cycle in the current speed mode.
    #[inline]
    pub(crate) const fn mcycle_clocks(&self) -> u32 {
        if self.double_speed { 2 } else { 4 }
    }
}
