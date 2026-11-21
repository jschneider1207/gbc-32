//! Cartridge: ROM, external RAM, mappers and RTC. No MBC and MBC1 since
//! M1; the other mappers arrive in M4.
//!
//! The ROM and cartridge RAM are borrowed from the caller for the
//! emulator's lifetime `'a` (ADR 0006) and handed back by
//! [`Emulator::eject`](crate::Emulator::eject), by a later
//! [`Emulator::load`](crate::Emulator::load), or in a [`LoadError`].

/// The smallest ROM [`Emulator::load`](crate::Emulator::load) accepts:
/// enough to hold the cartridge header (0100-014F).
///
/// Real cartridges are at least 32 KiB (two 16 KiB banks; Pan Docs: 0148
/// ROM size), but homebrew and test ROM files can be shorter. Like SameBoy
/// (`Core/gb.c`, `GB_load_rom`, which rounds the size up and pads with
/// 0xFF), a short ROM is accepted and behaves as if padded with 0xFF:
/// every read past the end of the ROM returns 0xFF.
pub const MIN_ROM_SIZE: usize = 0x150;

const HEADER_CART_TYPE: usize = 0x147;
const HEADER_RAM_SIZE: usize = 0x149;

/// The cartridge buffers the caller lent to the emulator: the ROM and the
/// whole cartridge RAM buffer exactly as passed to
/// [`Emulator::load`](crate::Emulator::load) (not trimmed to the header
/// size).
pub type CartBuffers<'a> = (&'a [u8], &'a mut [u8]);

/// Why a cartridge could not be loaded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum LoadErrorKind {
    /// The ROM is too short to hold the cartridge header (smaller than
    /// [`MIN_ROM_SIZE`]). Shorter ROMs that do hold it are accepted; see
    /// [`MIN_ROM_SIZE`].
    RomTooSmall { len: usize },
    /// The caller's cartridge RAM buffer is smaller than the header needs
    /// (see [`cart_ram_size`]).
    CartRamTooSmall { needed: usize, len: usize },
}

impl core::fmt::Display for LoadErrorKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Self::RomTooSmall { len } => {
                write!(f, "ROM is {len} bytes; at least {MIN_ROM_SIZE} needed")
            }
            Self::CartRamTooSmall { needed, len } => {
                write!(f, "cartridge RAM buffer is {len} bytes; {needed} needed")
            }
        }
    }
}

impl core::error::Error for LoadErrorKind {}

/// A failed [`Emulator::load`](crate::Emulator::load). Hands the buffers
/// back so the caller never loses them (on the MCU they are typically the
/// only `&'static mut` to a static buffer).
pub struct LoadError<'a> {
    pub kind: LoadErrorKind,
    /// The ROM passed to `load`.
    pub rom: &'a [u8],
    /// The cartridge RAM buffer passed to `load`, untouched.
    pub cart_ram: &'a mut [u8],
}

impl<'a> LoadError<'a> {
    /// The buffers, for reuse.
    pub fn into_buffers(self) -> CartBuffers<'a> {
        (self.rom, self.cart_ram)
    }
}

// Manual so that debug output doesn't dump the whole ROM.
impl core::fmt::Debug for LoadError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("LoadError")
            .field("kind", &self.kind)
            .field("rom_len", &self.rom.len())
            .field("cart_ram_len", &self.cart_ram.len())
            .finish()
    }
}

impl core::fmt::Display for LoadError<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        self.kind.fmt(f)
    }
}

impl core::error::Error for LoadError<'_> {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        Some(&self.kind)
    }
}

/// Bytes of cartridge RAM the cartridge uses, from the header. The buffer
/// passed to [`Emulator::load`](crate::Emulator::load) must be at least
/// this long; only this many bytes are used (and are the save file, see
/// [`Emulator::cart_ram`](crate::Emulator::cart_ram)). MBC2's built-in
/// 512x4-bit RAM counts as 512 bytes. The caller owns this buffer and
/// persists it (battery saves).
pub fn cart_ram_size(rom: &[u8]) -> Result<usize, LoadErrorKind> {
    if rom.len() < MIN_ROM_SIZE {
        return Err(LoadErrorKind::RomTooSmall { len: rom.len() });
    }
    // Pan Docs: The Cartridge Header, 0147 and 0149.
    if matches!(rom[HEADER_CART_TYPE], 0x05 | 0x06) {
        return Ok(512);
    }
    // Unknown codes mean no RAM rather than a load failure, as in SameBoy
    // (Core/mbc.c, GB_configure_cart: a 256-entry `ram_sizes` table where
    // only 0x00-0x05 are non-zero). Bad headers are common in homebrew and
    // test ROMs, which may still run fine without cartridge RAM.
    Ok(match rom[HEADER_RAM_SIZE] {
        // "Unused" in Pan Docs; some homebrew uses it for 2 KiB (SameBoy
        // agrees).
        0x01 => 0x800,
        0x02 => 0x2000,
        0x03 => 0x8000,
        0x04 => 0x2_0000,
        0x05 => 0x1_0000,
        _ => 0,
    })
}

/// Whether the cartridge type has an MBC3 real-time clock.
fn has_rtc(rom: &[u8]) -> bool {
    matches!(rom[HEADER_CART_TYPE], 0x0F | 0x10)
}

/// MBC3 clock registers (Pan Docs: MBC3, "The Clock Counter Registers").
///
/// The fields are the raw register bits, not a validated time. Each
/// register holds only its hardware width ([`Emulator::set_rtc_state`]
/// masks to it): values a real clock never reaches, such as `seconds` =
/// 60-63 (a game can write them), are legal; such a register counts up to
/// the top of its width and wraps to 0 without carrying into the next
/// register.
///
/// [`Emulator::set_rtc_state`]: crate::Emulator::set_rtc_state
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RtcTime {
    /// RTC S, 6 bits (0-63).
    pub seconds: u8,
    /// RTC M, 6 bits (0-63).
    pub minutes: u8,
    /// RTC H, 5 bits (0-31).
    pub hours: u8,
    /// Day counter, 9 bits (0-511): RTC DL is bits 0-7, RTC DH bit 0 is
    /// bit 8.
    pub days: u16,
    /// RTC DH bit 6: the clock is stopped.
    pub halted: bool,
    /// RTC DH bit 7: the day counter overflowed past 511. Stays set until
    /// the game clears it. (The other DH bits don't exist and read as 0.)
    pub day_carry: bool,
}

impl RtcTime {
    /// The registers cut to their hardware widths.
    #[must_use]
    pub const fn masked(self) -> Self {
        Self {
            seconds: self.seconds & 0x3F,
            minutes: self.minutes & 0x3F,
            hours: self.hours & 0x1F,
            days: self.days & 0x1FF,
            halted: self.halted,
            day_carry: self.day_carry,
        }
    }
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
    /// Base clocks elapsed in the current second, `0..CLOCK_HZ`
    /// ([`CLOCK_HZ`](crate::CLOCK_HZ)).
    pub subsecond_clocks: u32,
}

impl RtcState {
    /// [`RtcTime::masked`] for both copies, and `subsecond_clocks` clamped
    /// below one second (`CLOCK_HZ - 1` at most). What
    /// [`Emulator::set_rtc_state`](crate::Emulator::set_rtc_state) stores.
    #[must_use]
    pub const fn masked(self) -> Self {
        let max = crate::CLOCK_HZ - 1;
        Self {
            live: self.live.masked(),
            latched: self.latched.masked(),
            subsecond_clocks: if self.subsecond_clocks > max {
                max
            } else {
                self.subsecond_clocks
            },
        }
    }

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

/// The memory bank controller, from the header's cartridge type (0147).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mapper {
    /// No MBC: 32 KiB of ROM, optional RAM that is always enabled (Pan
    /// Docs: No MBC).
    None,
    /// MBC1 (types 01-03). TODO(M4): MBC1M multicarts.
    Mbc1,
}

impl Mapper {
    fn from_header(cart_type: u8) -> Self {
        match cart_type {
            0x01..=0x03 => Self::Mbc1,
            // TODO(M4): MBC2, MBC3, MBC5 and the rest; until then they
            // run as if they had no MBC.
            _ => Self::None,
        }
    }
}

/// Bytes per switchable ROM bank and RAM bank.
const ROM_BANK: usize = 0x4000;
const RAM_BANK: usize = 0x2000;

pub(crate) struct Cart<'a> {
    pub(crate) rom: &'a [u8],
    /// The caller's whole RAM buffer, kept so that eject returns all of it.
    /// Emulation must use only `ram()`, so behavior and the save file
    /// depend on the header, not on the buffer length.
    ram_buf: &'a mut [u8],
    /// [`cart_ram_size`] of the ROM (0 or a power of two).
    ram_len: usize,
    pub(crate) has_rtc: bool,
    pub(crate) rtc: RtcState,
    mapper: Mapper,
    /// The ROM size rounded up to a whole bank, then to a power of two (at
    /// least 32 KiB), minus one. Bank offsets wrap with this mask, so bank
    /// numbers past the end of the ROM mirror like on hardware (SameBoy
    /// `GB_load_rom` rounds the same way). Recorded at load.
    rom_mask: usize,
    /// The ROM visible at 0000-3FFF and 4000-7FFF: at most 0x4000 bytes,
    /// recomputed on bank switches only. Shorter (or empty) where the ROM
    /// file ends; reads past the end of a window return 0xFF, as if the
    /// ROM were padded with 0xFF (the M0 short-ROM rule).
    rom_lo: &'a [u8],
    rom_hi: &'a [u8],
    /// Offset of the RAM bank visible at A000-BFFF.
    ram_off: usize,
    ram_enabled: bool,
    /// MBC1 registers: BANK1 (5 bits, 2000-3FFF), BANK2 (2 bits,
    /// 4000-5FFF), MODE (1 bit, 6000-7FFF) (Pan Docs: MBC1; GBCTR: MBC1).
    bank1: u8,
    bank2: u8,
    mode: u8,
}

impl<'a> Cart<'a> {
    /// No cartridge: reads return open-bus 0xFF.
    pub(crate) const fn empty() -> Self {
        Self {
            rom: &[],
            ram_buf: &mut [],
            ram_len: 0,
            has_rtc: false,
            rtc: RtcState::zero(),
            mapper: Mapper::None,
            rom_mask: 0,
            rom_lo: &[],
            rom_hi: &[],
            ram_off: 0,
            ram_enabled: false,
            bank1: 1,
            bank2: 0,
            mode: 0,
        }
    }

    /// On error, the buffers come back in the [`LoadError`]. The mapper is
    /// in its power-on state.
    pub(crate) fn new(rom: &'a [u8], ram: &'a mut [u8]) -> Result<Self, LoadError<'a>> {
        let kind = match cart_ram_size(rom) {
            Ok(needed) if ram.len() >= needed => {
                let padded = rom.len().next_multiple_of(ROM_BANK).max(2 * ROM_BANK);
                let mut cart = Self {
                    rom,
                    ram_buf: ram,
                    ram_len: needed,
                    has_rtc: has_rtc(rom),
                    mapper: Mapper::from_header(rom[HEADER_CART_TYPE]),
                    rom_mask: padded.next_power_of_two() - 1,
                    ..Self::empty()
                };
                cart.reset();
                return Ok(cart);
            }
            Ok(needed) => LoadErrorKind::CartRamTooSmall {
                needed,
                len: ram.len(),
            },
            Err(kind) => kind,
        };
        Err(LoadError {
            kind,
            rom,
            cart_ram: ram,
        })
    }

    /// Whether a cartridge is inserted (a loaded ROM is never empty).
    pub(crate) fn is_loaded(&self) -> bool {
        !self.rom.is_empty()
    }

    /// The cartridge RAM the header declares (`cart_ram_size` bytes).
    #[inline]
    pub(crate) fn ram(&self) -> &[u8] {
        &self.ram_buf[..self.ram_len]
    }

    /// Mutable [`Cart::ram`], for tests.
    #[cfg(test)]
    pub(crate) fn ram_mut(&mut self) -> &mut [u8] {
        &mut self.ram_buf[..self.ram_len]
    }

    /// The caller's buffers (the RAM buffer untrimmed), or `None` for an
    /// empty slot.
    pub(crate) fn into_buffers(self) -> Option<CartBuffers<'a>> {
        self.is_loaded().then_some((self.rom, self.ram_buf))
    }

    /// Mapper state back to power-on. Cartridge RAM and the RTC keep their
    /// contents (battery backed).
    pub(crate) fn reset(&mut self) {
        self.bank1 = 1;
        self.bank2 = 0;
        self.mode = 0;
        // Without an MBC there is no enable register.
        self.ram_enabled = self.mapper == Mapper::None;
        self.update_banks();
    }

    /// A read of 0000-7FFF.
    #[inline(always)]
    pub(crate) fn read_rom(&self, addr: u16) -> u8 {
        let window = if addr < 0x4000 {
            self.rom_lo
        } else {
            self.rom_hi
        };
        // One compare against the window length (ADR 0008).
        match window.get(usize::from(addr) & (ROM_BANK - 1)) {
            Some(&v) => v,
            None => 0xFF,
        }
    }

    /// A read of A000-BFFF.
    #[inline]
    pub(crate) fn read_ram(&self, addr: u16) -> u8 {
        if !self.ram_enabled || self.ram_len == 0 {
            return 0xFF;
        }
        self.ram_buf[self.ram_index(addr)]
    }

    /// A write to A000-BFFF.
    #[inline]
    pub(crate) fn write_ram(&mut self, addr: u16, value: u8) {
        if self.ram_enabled && self.ram_len != 0 {
            let i = self.ram_index(addr);
            self.ram_buf[i] = value;
        }
    }

    /// RAM smaller than the window (2 KiB) or than the selected bank
    /// mirrors: `ram_len` is a power of two.
    #[inline]
    fn ram_index(&self, addr: u16) -> usize {
        (self.ram_off | (usize::from(addr) & (RAM_BANK - 1))) & (self.ram_len - 1)
    }

    /// A write to 0000-7FFF (the mapper's registers).
    pub(crate) fn write_rom(&mut self, addr: u16, value: u8) {
        match self.mapper {
            Mapper::None => {}
            Mapper::Mbc1 => {
                match addr >> 13 {
                    // RAM enable: 0A in the low nibble.
                    0 => self.ram_enabled = value & 0x0F == 0x0A,
                    // BANK1: 0 selects 1 (the 5-bit value, before masking
                    // to the ROM size, is what is checked).
                    1 => self.bank1 = (value & 0x1F).max(1),
                    2 => self.bank2 = value & 0x03,
                    _ => self.mode = value & 0x01,
                }
                self.update_banks();
            }
        }
    }

    /// Recomputes the ROM windows and RAM offset from the mapper registers.
    fn update_banks(&mut self) {
        let (lo, hi, ram) = match self.mapper {
            Mapper::None => (0, 1, 0),
            Mapper::Mbc1 => {
                // Mode 1 also applies BANK2 to 0000-3FFF and to RAM (Pan
                // Docs: MBC1, 6000-7FFF).
                let upper = usize::from(self.bank2) << 5;
                let hi = upper | usize::from(self.bank1);
                if self.mode == 0 {
                    (0, hi, 0)
                } else {
                    (upper, hi, usize::from(self.bank2))
                }
            }
        };
        self.rom_lo = self.rom_window(lo);
        self.rom_hi = self.rom_window(hi);
        self.ram_off = ram * RAM_BANK;
    }

    fn rom_window(&self, bank: usize) -> &'a [u8] {
        let rom: &'a [u8] = self.rom;
        let start = (bank * ROM_BANK) & self.rom_mask;
        rom.get(start..rom.len().min(start + ROM_BANK))
            .unwrap_or(&[])
    }
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
        assert_eq!(cart_ram_size(&rom_with(0x03, 0x01)), Ok(0x800));
        assert_eq!(cart_ram_size(&rom_with(0x03, 0x02)), Ok(0x2000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x03)), Ok(0x8000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x04)), Ok(0x2_0000));
        assert_eq!(cart_ram_size(&rom_with(0x1B, 0x05)), Ok(0x1_0000));
        assert_eq!(cart_ram_size(&rom_with(0x06, 0x00)), Ok(512));
        assert_eq!(
            cart_ram_size(&[0; 0x100]),
            Err(LoadErrorKind::RomTooSmall { len: 0x100 })
        );
    }

    #[test]
    fn rom_must_hold_the_header() {
        // Anything that holds the header (0100-014F) loads, as in SameBoy.
        assert_eq!(MIN_ROM_SIZE, 0x150);
        assert_eq!(
            cart_ram_size(&[0; 0x14F]),
            Err(LoadErrorKind::RomTooSmall { len: 0x14F })
        );
        assert!(Cart::new(&[0; 0x14F], &mut []).is_err());
        let mut rom = [0u8; 0x150];
        rom[HEADER_RAM_SIZE] = 0x02;
        assert_eq!(cart_ram_size(&rom), Ok(0x2000));
        assert!(Cart::new(&rom, &mut [0; 0x2000]).is_ok());
    }

    #[test]
    fn rtc_state_masks_to_register_widths() {
        let wide = RtcTime {
            seconds: 0xFF,
            minutes: 0xFF,
            hours: 0xFF,
            days: 0xFFFF,
            halted: true,
            day_carry: true,
        };
        let m = wide.masked();
        assert_eq!(
            (m.seconds, m.minutes, m.hours, m.days),
            (0x3F, 0x3F, 0x1F, 0x1FF)
        );
        assert!(m.halted && m.day_carry);
        // Out-of-range values that fit the register are kept as they are.
        let odd = RtcTime {
            seconds: 60,
            minutes: 63,
            hours: 24,
            days: 511,
            ..RtcTime::default()
        };
        assert_eq!(odd.masked(), odd);
        let s = RtcState {
            live: wide,
            latched: odd,
            subsecond_clocks: u32::MAX,
        }
        .masked();
        assert_eq!((s.live, s.latched), (m, odd));
        assert_eq!(s.subsecond_clocks, crate::CLOCK_HZ - 1);
        let s = RtcState {
            subsecond_clocks: 1234,
            ..RtcState::default()
        };
        assert_eq!(s.masked(), s);
    }

    #[test]
    fn unknown_ram_size_code_means_no_ram() {
        for code in [0x06, 0x09, 0x80, 0xFF] {
            let rom = rom_with(0x03, code);
            assert_eq!(cart_ram_size(&rom), Ok(0), "{code:#04x}");
            let cart = Cart::new(&rom, &mut []).unwrap();
            assert!(cart.ram().is_empty());
        }
    }

    #[test]
    fn ram_buffer_must_be_big_enough() {
        let rom = rom_with(0x03, 0x02);
        let mut small = [0u8; 0x1000];
        let err = Cart::new(&rom, &mut small).err().unwrap();
        assert_eq!(
            err.kind,
            LoadErrorKind::CartRamTooSmall {
                needed: 0x2000,
                len: 0x1000
            }
        );
        // The buffers come back.
        let (r, ram) = err.into_buffers();
        assert_eq!(r.len(), MIN_ROM_SIZE);
        assert_eq!(ram.len(), 0x1000);
        let mut ok = [0u8; 0x2000];
        assert!(Cart::new(&rom, &mut ok).is_ok());
    }

    #[test]
    fn ram_is_trimmed_to_header_size_but_returned_whole() {
        let rom = rom_with(0x03, 0x02);
        let mut buf = [0u8; 0x8000];
        buf[0x2000] = 0x77;
        let mut cart = Cart::new(&rom, &mut buf).unwrap();
        assert_eq!(cart.ram().len(), 0x2000);
        assert_eq!(cart.ram_mut().len(), 0x2000);
        let (_, whole) = cart.into_buffers().unwrap();
        assert_eq!(whole.len(), 0x8000);
        assert_eq!(whole[0x2000], 0x77);
    }

    #[test]
    fn empty_cart_has_no_buffers() {
        assert!(Cart::empty().into_buffers().is_none());
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
