//! Cartridge: ROM, external RAM, mappers and RTC. Skeleton only: mappers
//! arrive in M1 (MBC1) and M4 (the rest).
//!
//! The ROM and cartridge RAM are borrowed from the caller for the
//! emulator's lifetime `'a` (ADR 0006).

/// The smallest cartridge ROM: two 16 KiB banks (Pan Docs: 0148 ROM size).
pub const MIN_ROM_SIZE: usize = 0x8000;

const HEADER_CART_TYPE: usize = 0x147;
const HEADER_RAM_SIZE: usize = 0x149;

/// Why a cartridge could not be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadError {
    /// The ROM is smaller than [`MIN_ROM_SIZE`].
    RomTooSmall { len: usize },
    /// The header's RAM size byte (0149) is not a known value.
    UnknownRamSize { code: u8 },
    /// The caller's cartridge RAM buffer is smaller than the header needs
    /// (see [`cart_ram_size`]).
    CartRamTooSmall { needed: usize, len: usize },
}

impl core::fmt::Display for LoadError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::RomTooSmall { len } => {
                write!(f, "ROM is {len} bytes; at least {MIN_ROM_SIZE} needed")
            }
            Self::UnknownRamSize { code } => write!(f, "unknown RAM size code {code:#04x}"),
            Self::CartRamTooSmall { needed, len } => {
                write!(f, "cartridge RAM buffer is {len} bytes; {needed} needed")
            }
        }
    }
}

impl core::error::Error for LoadError {}

/// Bytes of cartridge RAM the caller must provide for `rom`, from the
/// header. MBC2's built-in 512x4-bit RAM counts as 512 bytes. The caller
/// owns this buffer and persists it (battery saves).
pub fn cart_ram_size(rom: &[u8]) -> Result<usize, LoadError> {
    if rom.len() < MIN_ROM_SIZE {
        return Err(LoadError::RomTooSmall { len: rom.len() });
    }
    // Pan Docs: The Cartridge Header, 0147 and 0149.
    if matches!(rom[HEADER_CART_TYPE], 0x05 | 0x06) {
        return Ok(512);
    }
    match rom[HEADER_RAM_SIZE] {
        0x00 => Ok(0),
        // "Unused" in Pan Docs; some homebrew uses it for 2 KiB.
        0x01 => Ok(0x800),
        0x02 => Ok(0x2000),
        0x03 => Ok(0x8000),
        0x04 => Ok(0x2_0000),
        0x05 => Ok(0x1_0000),
        code => Err(LoadError::UnknownRamSize { code }),
    }
}

/// Whether the cartridge type has an MBC3 real-time clock.
fn has_rtc(rom: &[u8]) -> bool {
    matches!(rom[HEADER_CART_TYPE], 0x0F | 0x10)
}

/// MBC3 clock registers (Pan Docs: MBC3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcTime {
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    /// Day counter, 0-511.
    pub days: u16,
    pub halted: bool,
    /// Day counter overflowed past 511.
    pub day_carry: bool,
}

/// Complete MBC3 RTC state, for the caller to save and restore. The RTC
/// advances only with emulated time; the caller may add wall-clock time on
/// load. Provisional shape: M4 may extend it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcState {
    /// The running counters.
    pub live: RtcTime,
    /// The copy the game reads after latching.
    pub latched: RtcTime,
    /// Base clocks elapsed in the current second, `0..CLOCK_HZ`.
    pub subsecond_clocks: u32,
}

impl RtcState {
    const fn zero() -> Self {
        const T: RtcTime = RtcTime {
            seconds: 0,
            minutes: 0,
            hours: 0,
            days: 0,
            halted: false,
            day_carry: false,
        };
        Self {
            live: T,
            latched: T,
            subsecond_clocks: 0,
        }
    }
}

pub(crate) struct Cart<'a> {
    pub(crate) rom: &'a [u8],
    pub(crate) ram: &'a mut [u8],
    pub(crate) has_rtc: bool,
    pub(crate) rtc: RtcState,
}

impl<'a> Cart<'a> {
    /// No cartridge: reads return open-bus 0xFF.
    pub(crate) const fn empty() -> Self {
        Self {
            rom: &[],
            ram: &mut [],
            has_rtc: false,
            rtc: RtcState::zero(),
        }
    }

    pub(crate) fn new(rom: &'a [u8], ram: &'a mut [u8]) -> Result<Self, LoadError> {
        let needed = cart_ram_size(rom)?;
        if ram.len() < needed {
            return Err(LoadError::CartRamTooSmall {
                needed,
                len: ram.len(),
            });
        }
        Ok(Self {
            rom,
            ram,
            has_rtc: has_rtc(rom),
            rtc: RtcState::zero(),
        })
    }

    /// Mapper state back to power-on. Cartridge RAM and the RTC keep their
    /// contents (battery backed).
    pub(crate) fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rom_with(cart_type: u8, ram_code: u8) -> [u8; MIN_ROM_SIZE] {
        let mut rom = [0u8; MIN_ROM_SIZE];
        rom[HEADER_CART_TYPE] = cart_type;
        rom[HEADER_RAM_SIZE] = ram_code;
        rom
    }

    #[test]
    fn ram_size_from_header() {
        assert_eq!(cart_ram_size(&rom_with(0x00, 0x00)), Ok(0));
        assert_eq!(cart_ram_size(&rom_with(0x03, 0x02)), Ok(0x2000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x03)), Ok(0x8000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x04)), Ok(0x2_0000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x05)), Ok(0x1_0000));
        assert_eq!(cart_ram_size(&rom_with(0x06, 0x00)), Ok(512));
        assert_eq!(
            cart_ram_size(&rom_with(0x00, 0x09)),
            Err(LoadError::UnknownRamSize { code: 0x09 })
        );
        assert_eq!(
            cart_ram_size(&[0; 0x100]),
            Err(LoadError::RomTooSmall { len: 0x100 })
        );
    }

    #[test]
    fn ram_buffer_must_be_big_enough() {
        let rom = rom_with(0x03, 0x02);
        let mut small = [0u8; 0x1000];
        assert_eq!(
            Cart::new(&rom, &mut small).err(),
            Some(LoadError::CartRamTooSmall {
                needed: 0x2000,
                len: 0x1000
            })
        );
        let mut ok = [0u8; 0x2000];
        assert!(Cart::new(&rom, &mut ok).is_ok());
    }

    #[test]
    fn rtc_detected_from_type() {
        let rom = rom_with(0x10, 0x03);
        let mut ram = [0u8; 0x8000];
        assert!(Cart::new(&rom, &mut ram).unwrap().has_rtc);
        let rom = rom_with(0x13, 0x03);
        assert!(!Cart::new(&rom, &mut ram).unwrap().has_rtc);
    }
}
