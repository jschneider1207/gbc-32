//! Cartridge: ROM, external RAM, mappers and RTC. No MBC, MBC1 (and
//! MBC1M), MBC2, MBC3/MBC30, MBC5, HuC1 and HuC3 are emulated; a header that
//! names
//! any other mapper ([`MapperKind`]) is a load error (ADR 0031).
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

/// A cartridge taken out of the emulator by
/// [`Emulator::eject`](crate::Emulator::eject) or by loading another one
/// over it ([`Emulator::load`](crate::Emulator::load)): the buffers the
/// caller lent, plus the save state that lived inside the emulator, so
/// nothing is lost on a game switch.
#[must_use = "dropping it loses the RTC state, the dirty flag and the RAM buffer"]
#[non_exhaustive]
pub struct EjectedCart<'a> {
    /// The ROM passed to `load`.
    pub rom: &'a [u8],
    /// The whole cartridge RAM buffer passed to `load` (not trimmed to the
    /// header size; [`EjectedCart::save_ram`] is the save file part).
    pub cart_ram: &'a mut [u8],
    /// The MBC3 RTC state at the moment the cartridge was removed, or
    /// `None` if it has no RTC. Persist it with the save; it is not kept
    /// anywhere else. The RTC keeps counting without setting
    /// [`EjectedCart::save_dirty`], so persist it at exit regardless of that
    /// flag.
    pub rtc: Option<RtcState>,
    /// The HuC3 clock state at the moment the cartridge was removed, or
    /// `None` if it is not a HuC3 cartridge. Persist it like `rtc`.
    pub huc3_rtc: Option<Huc3RtcState>,
    /// [`Emulator::save_dirty`](crate::Emulator::save_dirty) at removal:
    /// the game changed its save since the caller last cleared the flag.
    pub save_dirty: bool,
}

impl<'a> EjectedCart<'a> {
    /// The save file part of `cart_ram`: its first
    /// [`cart_ram_size`]`(rom)` bytes, the same bytes
    /// [`Emulator::cart_ram`](crate::Emulator::cart_ram) returned.
    pub fn save_ram(&self) -> &[u8] {
        let n = cart_ram_size(self.rom).unwrap_or(0);
        self.cart_ram.get(..n).unwrap_or(&[])
    }

    /// The buffers, for reuse (the RTC state and dirty flag are dropped).
    pub fn into_buffers(self) -> CartBuffers<'a> {
        (self.rom, self.cart_ram)
    }
}

// Manual so that debug output doesn't dump the ROM and RAM.
impl core::fmt::Debug for EjectedCart<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EjectedCart")
            .field("rom_len", &self.rom.len())
            .field("cart_ram_len", &self.cart_ram.len())
            .field("rtc", &self.rtc)
            .field("huc3_rtc", &self.huc3_rtc)
            .field("save_dirty", &self.save_dirty)
            .finish()
    }
}

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
    /// The header's cartridge type (0147, `cart_type`) names a mapper the
    /// core does not emulate, or no known mapper at all
    /// ([`MapperKind::Unknown`]). Such a cartridge is refused rather than
    /// run without banking: the game would execute the wrong bank and
    /// could scribble over its save (ADR 0031).
    UnsupportedMapper { mapper: MapperKind, cart_type: u8 },
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
            Self::UnsupportedMapper { mapper, cart_type } => write!(
                f,
                "unsupported cartridge: {mapper} (header cartridge type {cart_type:02X})"
            ),
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

/// The mapper family the header's cartridge type (0147) names (Pan Docs:
/// The Cartridge Header, "0147 - Cartridge type"), including the ones the
/// core does not emulate ([`MapperKind::is_supported`]). Only the header
/// byte decides it: an MBC1 multicart is [`MapperKind::Mbc1`], and the
/// RAM, battery, RTC and rumble variants of a mapper share its kind (see
/// [`CartInfo`] for those).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum MapperKind {
    /// No mapper: 00 (ROM only), 08 (ROM+RAM), 09 (ROM+RAM+BATTERY).
    RomOnly,
    /// MBC1: 01-03.
    Mbc1,
    /// MBC2: 05-06.
    Mbc2,
    /// MBC3 and MBC30: 0F-13.
    Mbc3,
    /// MBC5: 19-1E.
    Mbc5,
    /// MMM01: 0B-0D. Not emulated.
    Mmm01,
    /// MBC6: 20. Not emulated.
    Mbc6,
    /// MBC7 (accelerometer, EEPROM): 22. Not emulated.
    Mbc7,
    /// Pocket Camera: FC. Not emulated.
    PocketCamera,
    /// Bandai TAMA5: FD. Not emulated.
    Tama5,
    /// Hudson HuC3 (clock; its infrared port and speaker are not
    /// connected to anything): FE.
    HuC3,
    /// Hudson HuC1 (infrared port, not connected to anything): FF.
    HuC1,
    /// A code Pan Docs does not assign (04, 07, 0A, 0E, 14-18, 1F, 21,
    /// 23-FB).
    Unknown,
}

impl MapperKind {
    /// The kind a cartridge type byte (header 0147) names.
    pub const fn from_cart_type(cart_type: u8) -> Self {
        match cart_type {
            0x00 | 0x08 | 0x09 => Self::RomOnly,
            0x01..=0x03 => Self::Mbc1,
            0x05 | 0x06 => Self::Mbc2,
            0x0B..=0x0D => Self::Mmm01,
            0x0F..=0x13 => Self::Mbc3,
            0x19..=0x1E => Self::Mbc5,
            0x20 => Self::Mbc6,
            0x22 => Self::Mbc7,
            0xFC => Self::PocketCamera,
            0xFD => Self::Tama5,
            0xFE => Self::HuC3,
            0xFF => Self::HuC1,
            _ => Self::Unknown,
        }
    }

    /// Whether the core emulates this mapper.
    /// [`Emulator::load`](crate::Emulator::load), [`cart_ram_size`] and
    /// [`cart_info`] fail with [`LoadErrorKind::UnsupportedMapper`] for
    /// the others.
    pub const fn is_supported(self) -> bool {
        // Keep in step with `Mapper::from_header` (the test
        // `supported_kinds_are_the_ones_that_load` checks every code).
        matches!(
            self,
            Self::RomOnly
                | Self::Mbc1
                | Self::Mbc2
                | Self::Mbc3
                | Self::Mbc5
                | Self::HuC1
                | Self::HuC3
        )
    }

    /// The mapper's usual name, for messages.
    pub const fn name(self) -> &'static str {
        match self {
            Self::RomOnly => "no mapper",
            Self::Mbc1 => "MBC1",
            Self::Mbc2 => "MBC2",
            Self::Mbc3 => "MBC3",
            Self::Mbc5 => "MBC5",
            Self::Mmm01 => "MMM01",
            Self::Mbc6 => "MBC6",
            Self::Mbc7 => "MBC7",
            Self::PocketCamera => "Pocket Camera",
            Self::Tama5 => "TAMA5",
            Self::HuC3 => "HuC3",
            Self::HuC1 => "HuC1",
            Self::Unknown => "unknown mapper",
        }
    }
}

impl core::fmt::Display for MapperKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.name())
    }
}

/// Header byte `at` of `rom` (0 past the end: callers check the length
/// first).
///
/// Out of line (code size): header parsing runs once per load, and a call
/// in a loop keeps LLVM from unrolling it (`is_mbc1m`).
#[inline(never)]
fn header(rom: &[u8], at: usize) -> u8 {
    Rom::new(rom).get(at).unwrap_or(0)
}

/// Byte `i` of `rom`, or FF past its end. Out of line for the same reason
/// as `header`, for the cold ROM reads of VRAM DMA (`Cart::rom_tail`).
#[inline(never)]
fn rom_byte(rom: Rom<'_>, i: usize) -> u8 {
    rom.get(i).unwrap_or(0xFF)
}

/// Bytes of cartridge RAM the cartridge uses, from the header. The buffer
/// passed to [`Emulator::load`](crate::Emulator::load) must be at least
/// this long; only this many bytes are used (and are the save file, see
/// [`Emulator::cart_ram`](crate::Emulator::cart_ram)). MBC2's built-in
/// 512x4-bit RAM counts as 512 bytes. The caller owns this buffer and
/// persists it (battery saves).
///
/// Fails as [`Emulator::load`](crate::Emulator::load) would for a ROM too
/// short to hold the header or one whose mapper is not emulated
/// ([`LoadErrorKind::UnsupportedMapper`]).
pub fn cart_ram_size(rom: &[u8]) -> Result<usize, LoadErrorKind> {
    if rom.len() < MIN_ROM_SIZE {
        return Err(LoadErrorKind::RomTooSmall { len: rom.len() });
    }
    // Pan Docs: The Cartridge Header, 0147 and 0149.
    // A mapper the core does not emulate has no RAM to size: the
    // cartridge does not load (ADR 0031). Checked here too, not only in
    // `Mapper::from_header`, so a front end that sizes its buffer or asks
    // `cart_info` before `load` learns it before it touches a save file.
    let cart_type = header(rom, HEADER_CART_TYPE);
    let mapper = MapperKind::from_cart_type(cart_type);
    if !mapper.is_supported() {
        return Err(LoadErrorKind::UnsupportedMapper { mapper, cart_type });
    }
    // ROM ONLY (00) has no RAM whatever 0149 says. SameBoy (Core/mbc.c,
    // GB_configure_cart) zeroes the RAM size for a no-MBC type whose
    // table entry has no RAM, but for MBC types without RAM and a nonzero
    // 0149 it assumes the RAM exists ("Assuming cartridge has RAM"), which
    // is what the table below does anyway (ADR 0016).
    if cart_type == 0x00 {
        return Ok(0);
    }
    if matches!(mapper, MapperKind::Mbc2) {
        return Ok(512);
    }
    // Unknown RAM size codes mean no RAM rather than a load failure, as in
    // SameBoy (Core/mbc.c, GB_configure_cart: a 256-entry `ram_sizes`
    // table where only 0x00-0x05 are non-zero). Bad headers are common in
    // homebrew and test ROMs, which may still run fine without cartridge
    // RAM.
    Ok(match header(rom, HEADER_RAM_SIZE) {
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

/// What the cartridge header says about saving, for a front end deciding
/// what to persist (see [`cart_info`] and
/// [`Emulator::cart_info`](crate::Emulator::cart_info)).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct CartInfo {
    /// The mapper the cartridge type (0147) names; always a supported one
    /// ([`cart_info`] fails for the others).
    pub mapper: MapperKind,
    /// [`cart_ram_size`]: bytes of cartridge RAM (the save file length).
    pub ram_size: usize,
    /// The cartridge type (0147) is battery-backed: its RAM (and RTC)
    /// keep their contents with the power off, so they are a save.
    pub battery: bool,
    /// The cartridge has an MBC3 real-time clock
    /// ([`Emulator::rtc_state`](crate::Emulator::rtc_state) is `Some`).
    pub rtc: bool,
    /// The cartridge has a HuC3 clock
    /// ([`Emulator::huc3_rtc_state`](crate::Emulator::huc3_rtc_state) is
    /// `Some`): type FE.
    pub huc3_rtc: bool,
}

impl CartInfo {
    /// Whether the cartridge has anything to persist: a battery, and RAM
    /// or a clock (MBC3 or HuC3) behind it. Without a battery the cartridge RAM is lost at
    /// power off on hardware, so a front end should not write a save file
    /// (nor load one: a game may rely on RAM starting fresh).
    pub const fn has_save(&self) -> bool {
        self.battery && (self.ram_size != 0 || self.rtc || self.huc3_rtc)
    }
}

/// [`CartInfo`] from the header, usable before
/// [`Emulator::load`](crate::Emulator::load) (e.g. to decide whether to
/// read a save file). Fails like [`cart_ram_size`]: in particular for a
/// cartridge whose mapper is not emulated, which has no `CartInfo`.
pub fn cart_info(rom: &[u8]) -> Result<CartInfo, LoadErrorKind> {
    let ram_size = cart_ram_size(rom)?;
    Ok(CartInfo {
        mapper: MapperKind::from_cart_type(header(rom, HEADER_CART_TYPE)),
        ram_size,
        battery: has_battery(rom),
        rtc: has_rtc(rom),
        huc3_rtc: header(rom, HEADER_CART_TYPE) == 0xFE,
    })
}

mod huc3;
mod rtc;

pub use huc3::Huc3RtcState;

use crate::rom::Rom;

/// The Nintendo logo every licensed cartridge carries at 0104-0133 (Pan
/// Docs: The Cartridge Header, "0104-0133 - Nintendo logo"); the boot ROM
/// refuses to start a cartridge without it.
const NINTENDO_LOGO: [u8; 0x30] = [
    0xCE, 0xED, 0x66, 0x66, 0xCC, 0x0D, 0x00, 0x0B, 0x03, 0x73, 0x00, 0x83, 0x00, 0x0C, 0x00, 0x0D,
    0x00, 0x08, 0x11, 0x1F, 0x88, 0x89, 0x00, 0x0E, 0xDC, 0xCC, 0x6E, 0xE6, 0xDD, 0xDD, 0xD9, 0x99,
    0xBB, 0xBB, 0x67, 0x63, 0x6E, 0x0E, 0xEC, 0xCC, 0xDD, 0xDC, 0x99, 0x9F, 0xBB, 0xB9, 0x33, 0x3E,
];

/// Whether an MBC1 ROM is a multicart (MBC1M), by its contents (ADR 0016).
/// Each game of a multicart has its own header, so the Nintendo logo
/// (0104-0133, 0x30 bytes) is repeated at the start of the game in bank
/// 0x10 (offset 0x40104). SameBoy's test (`Core/mbc.c`,
/// GB_configure_cart) is a ROM of at least 0x44000 bytes whose two logo
/// areas are equal; here both must also be the logo itself (Pan Docs:
/// MBC1, "MBC1M": look for the logo), so that two equal areas of filler
/// (a logo-less homebrew ROM with an empty bank 0x10) are not taken for a
/// second game.
fn is_mbc1m(rom: &[u8]) -> bool {
    if rom.len() < 0x4_4000 {
        return false;
    }
    // Byte by byte through `Rom::get` (`header`), not a slice compare (a
    // `memcmp` may read words; ADR 0027). Both copies are inside the ROM.
    NINTENDO_LOGO
        .iter()
        .zip(0x104..)
        .all(|(&b, i)| header(rom, i) == b && header(rom, 0x4_0000 + i) == b)
}

/// Whether the cartridge type has an MBC3 real-time clock.
fn has_rtc(rom: &[u8]) -> bool {
    matches!(header(rom, HEADER_CART_TYPE), 0x0F | 0x10)
}

/// Whether the cartridge type is battery-backed. Pan Docs ("The Cartridge
/// Header", 0147): every type named "+BATTERY" (03 MBC1, 06 MBC2, 09 ROM,
/// 0D MMM01, 0F/10/13 MBC3, 1B/1E MBC5, 22 MBC7, FF HuC1). SameBoy
/// (`Core/mbc.c`, `GB_cart_defs`) also marks the Pocket Camera (FC),
/// TAMA5 (FD) and HuC3 (FE) as battery-backed. The table is the header's;
/// the types of mappers the core does not emulate never reach a caller,
/// since those cartridges do not load (ADR 0031).
fn has_battery(rom: &[u8]) -> bool {
    matches!(
        header(rom, HEADER_CART_TYPE),
        0x03 | 0x06 | 0x09 | 0x0D | 0x0F | 0x10 | 0x13 | 0x1B | 0x1E | 0x22 | 0xFC..=0xFF
    )
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
    /// No MBC (types 00, 08, 09): 32 KiB of ROM, optional RAM that is
    /// always enabled (Pan Docs: No MBC). Also the empty slot.
    None,
    /// MBC1 (types 01-03), standard wiring: BANK2 is ROM bank bits 5-6.
    Mbc1,
    /// MBC1 multicart (MBC1M, ADR 0016): types 01-03 whose ROM looks like
    /// a multicart (`is_mbc1m`). BANK1 reaches the ROM address with 4 bits
    /// and BANK2 sits above them (bits 4-5). A unit variant, like
    /// `Mbc3Rtc`, to keep `Mapper` one byte.
    Mbc1M,
    /// MBC2 (types 05-06): 16 ROM banks, 512x4 bits of built-in RAM.
    Mbc2,
    /// MBC3 (types 11-13), no clock. Also MBC30 (see `write_rom`).
    Mbc3,
    /// MBC3 with the RTC (types 0F-10). A variant rather than a `Cart`
    /// flag, and a unit one (two variants with data would make `Mapper` 2
    /// bytes), to keep `Cart` at 128 bytes (ADR 0015).
    Mbc3Rtc,
    /// MBC5 (types 19-1E). `rumble` is set for 1C-1E.
    Mbc5 { rumble: bool },
    /// Hudson HuC1 (type FF): ROM and RAM banking, and an infrared port in
    /// place of the RAM enable (Pan Docs: HuC1). See `write_rom`.
    HuC1,
    /// Hudson HuC3 (type FE): ROM and RAM banking and a mode register that
    /// puts RAM, the clock chip's mailbox or the infrared port at
    /// A000-BFFF (Pan Docs: HuC-3). See `set_huc3_mode`.
    HuC3,
}

impl Mapper {
    /// The mapper to emulate, resolved once at load: the header's
    /// [`MapperKind`] plus what the rest of the type byte and the ROM
    /// contents add (RTC, rumble, multicart wiring). Fails for a kind the
    /// core does not emulate (ADR 0031).
    ///
    /// To add a mapper: a variant here, its arm below, and its kind in
    /// [`MapperKind::is_supported`]; the match is exhaustive, and the test
    /// `supported_kinds_are_the_ones_that_load` holds the two together.
    fn from_header(rom: &[u8]) -> Result<Self, LoadErrorKind> {
        use MapperKind as K;
        let cart_type = header(rom, HEADER_CART_TYPE);
        let mapper = MapperKind::from_cart_type(cart_type);
        Ok(match mapper {
            K::RomOnly => Self::None,
            K::Mbc1 if is_mbc1m(rom) => Self::Mbc1M,
            K::Mbc1 => Self::Mbc1,
            K::Mbc2 => Self::Mbc2,
            K::Mbc3 if has_rtc(rom) => Self::Mbc3Rtc,
            K::Mbc3 => Self::Mbc3,
            // Pan Docs 0147: 1C-1E are the "+RUMBLE" types.
            K::Mbc5 => Self::Mbc5 {
                rumble: cart_type >= 0x1C,
            },
            K::HuC1 => Self::HuC1,
            K::HuC3 => Self::HuC3,
            K::Mmm01 | K::Mbc6 | K::Mbc7 | K::PocketCamera | K::Tama5 | K::Unknown => {
                return Err(LoadErrorKind::UnsupportedMapper { mapper, cart_type });
            }
        })
    }
}

/// `Cart::rtc_sel` values of the Hudson mappers: what is at A000-BFFF when
/// it is not plain read/write RAM (0). The MBC3 values are 08-0C and FF
/// (nothing mapped), so these start at 10: `10 | mode` for the HuC3 mode
/// register values that map something (Pan Docs: HuC-3, "0000-1FFF -
/// RAM/RTC/IR Select"). Everything from `SEL_RAM_RO` up is handled by
/// `Cart::read_huc` and `Cart::write_huc`.
///
/// HuC3 mode 0: cartridge RAM, read-only.
const SEL_RAM_RO: u8 = 0x10;
/// HuC3 mode B: clock command/argument (write).
const SEL_CLOCK_ARG: u8 = 0x1B;
/// HuC3 mode C: clock command/response (read).
const SEL_CLOCK_RESPONSE: u8 = 0x1C;
/// HuC3 mode D: clock semaphore.
const SEL_CLOCK_SEMAPHORE: u8 = 0x1D;
/// HuC3 mode E, and HuC1's IR mode: the infrared register.
const SEL_IR: u8 = 0x1E;

/// Bytes per switchable ROM bank and RAM bank.
const ROM_BANK: usize = 0x4000;
const RAM_BANK: usize = 0x2000;

pub(crate) struct Cart<'a> {
    pub(crate) rom: Rom<'a>,
    /// The caller's whole RAM buffer, kept so that eject returns all of it.
    /// Emulation must use only `ram()`, so behavior and the save file
    /// depend on the header, not on the buffer length.
    ram_buf: &'a mut [u8],
    /// [`cart_ram_size`] of the ROM (0 or a power of two).
    ram_len: usize,
    /// The RTC as of clock `rtc_synced` (lazy catch-up, see `rtc.rs`).
    /// Read it through [`Cart::rtc_at`].
    rtc: RtcState,
    /// The emulator clock `rtc` was last advanced to.
    rtc_synced: u64,
    /// Bit 0 of the last 6000-7FFF write (MBC3 latch: 0 then 1 latches).
    rtc_latch: bool,
    /// The HuC3 clock chip, as of clock `rtc_synced` too. Read its clock
    /// through [`Cart::huc3_at`].
    huc3: huc3::Huc3,
    /// The game changed the save since the caller last cleared this: a RAM
    /// write that changed a byte, or an RTC register write. (Clock ticks
    /// and latches don't count: the caller saves the RTC with the time
    /// anyway, and games latch every frame.)
    pub(crate) save_dirty: bool,
    mapper: Mapper,
    /// The ROM size rounded up to a whole bank, then to a power of two (at
    /// least 32 KiB), minus one. Bank offsets wrap with this mask, so bank
    /// numbers past the end of the ROM mirror like on hardware (SameBoy
    /// `GB_load_rom` rounds the same way). Recorded at load.
    rom_mask: usize,
    /// The ROM visible at 0000-3FFF and 4000-7FFF: at most 0x4000 bytes,
    /// recomputed on bank switches only. Shorter (or empty) where the ROM
    /// file ends; reads past the end of a window return 0xFF, as if the
    /// ROM were padded with 0xFF (the M0 short-ROM rule). Indexed by
    /// address bit 14, so a read selects its window with one shift.
    rom_win: [Rom<'a>; 2],
    /// Offset of the RAM bank visible at A000-BFFF.
    ram_off: usize,
    ram_enabled: bool,
    /// Bits forced to 1 in every stored RAM byte: 0xF0 for MBC2's 4-bit RAM
    /// (Pan Docs: MBC2, the upper nibble reads as 1s), else 0. The buffer
    /// holds the byte as the CPU reads it (SameBoy `Core/mbc.c` stores
    /// `value | 0xF0` too), so `read_ram` needs no mapper check; `new`
    /// normalizes a loaded save the same way. Writes OR it in.
    ram_or: u8,
    /// MBC1 registers: BANK1 (5 bits, 2000-3FFF), BANK2 (2 bits,
    /// 4000-5FFF), MODE (1 bit, 6000-7FFF) (Pan Docs: MBC1; GBCTR: MBC1).
    bank1: u8,
    bank2: u8,
    mode: u8,
    /// MBC5 ROM bank, 9 bits (2000-2FFF is bits 0-7, 3000-3FFF bit 8) and
    /// RAM bank (4 bits, 3 on rumble carts). Bank 0 is a real bank.
    ///
    /// MBC3 reuses both: `mbc5_rom` is its ROM bank (8 bits stored, 7 used
    /// below 4 MiB) and `mbc5_ram` the raw 4000-5FFF value. HuC1 and HuC3
    /// too: a 6-bit or 7-bit ROM bank and a 3-bit or 4-bit RAM bank.
    mbc5_rom: u16,
    mbc5_ram: u8,
    /// MBC3: the selected RTC register, 08-0C, or 0xFF when the 4000-5FFF
    /// value maps nothing at A000-BFFF (RTC select above 0C, or RAM
    /// bank 4-7 on an RTC cart below MBC30); 0 when RAM is mapped. Kept apart from `mbc5_ram`
    /// so the RAM read path tests one byte. HuC1 and HuC3: one of the
    /// `SEL_*` values when A000-BFFF is not read/write RAM.
    rtc_sel: u8,
}

impl<'a> Cart<'a> {
    /// No cartridge: reads return open-bus 0xFF.
    pub(crate) const fn empty() -> Self {
        Self {
            rom: Rom::EMPTY,
            ram_buf: &mut [],
            ram_len: 0,
            rtc: RtcState::zero(),
            rtc_synced: 0,
            rtc_latch: false,
            huc3: huc3::Huc3::new(),
            save_dirty: false,
            mapper: Mapper::None,
            rom_mask: 0,
            rom_win: [Rom::EMPTY; 2],
            ram_off: 0,
            ram_enabled: false,
            ram_or: 0,
            bank1: 1,
            bank2: 0,
            mode: 0,
            mbc5_rom: 1,
            mbc5_ram: 0,
            rtc_sel: 0,
        }
    }

    /// On error, the buffers come back in the [`LoadError`]. The mapper is
    /// in its power-on state.
    pub(crate) fn new(rom: &'a [u8], ram: &'a mut [u8]) -> Result<Self, LoadError<'a>> {
        let resolved = cart_ram_size(rom).and_then(|n| Ok((n, Mapper::from_header(rom)?)));
        let kind = match resolved {
            Ok((needed, mapper)) if ram.len() >= needed => {
                let padded = rom.len().next_multiple_of(ROM_BANK).max(2 * ROM_BANK);
                let mut cart = Self {
                    rom: Rom::new(rom),
                    ram_buf: ram,
                    ram_len: needed,
                    mapper,
                    ram_or: if mapper == Mapper::Mbc2 { 0xF0 } else { 0 },
                    rom_mask: padded.next_power_of_two() - 1,
                    ..Self::empty()
                };
                if cart.ram_or != 0 {
                    for b in cart.ram_buf[..needed].iter_mut() {
                        *b |= cart.ram_or;
                    }
                }
                cart.reset();
                return Ok(cart);
            }
            Ok((needed, _)) => LoadErrorKind::CartRamTooSmall {
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

    /// Whether the cartridge has an MBC3 real-time clock.
    pub(crate) fn has_rtc(&self) -> bool {
        self.mapper == Mapper::Mbc3Rtc
    }

    /// Whether the cartridge has a HuC3 clock.
    pub(crate) fn has_huc3_rtc(&self) -> bool {
        self.mapper == Mapper::HuC3
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

    /// The caller's buffers (the RAM buffer untrimmed) with the RTC state
    /// at clock `now` and the dirty flag, or `None` for an empty slot.
    #[inline(never)]
    pub(crate) fn into_ejected(self, now: u64) -> Option<EjectedCart<'a>> {
        if !self.is_loaded() {
            return None;
        }
        let rtc = self.has_rtc().then(|| self.rtc_at(now));
        let huc3_rtc = self.has_huc3_rtc().then(|| self.huc3_at(now));
        Some(EjectedCart {
            rom: self.rom.into_inner(), // check-rom-loads: allowed (hand-back)
            cart_ram: self.ram_buf,
            rtc,
            huc3_rtc,
            save_dirty: self.save_dirty,
        })
    }

    /// Mapper state back to power-on. Cartridge RAM and the RTC keep their
    /// contents (battery backed). The emulator clock restarts at 0, so the
    /// caller first catches the RTC up ([`Cart::rtc_sync`]) if time passed.
    pub(crate) fn reset(&mut self) {
        self.rtc_synced = 0;
        self.rtc_latch = false;
        self.huc3.reset_mailbox();
        self.bank1 = 1;
        self.bank2 = 0;
        self.mode = 0;
        self.mbc5_rom = 1;
        self.mbc5_ram = 0;
        self.rtc_sel = 0;
        // Without an MBC there is no enable register. Nor on HuC1, whose
        // 0000-1FFF register selects RAM or the IR port instead (Pan Docs:
        // HuC1, "Most MBCs can disable the cartridge RAM [...] HuC1
        // doesn't do this"); it powers on in RAM mode (`rtc_sel` 0).
        self.ram_enabled = matches!(self.mapper, Mapper::None | Mapper::HuC1);
        if self.mapper == Mapper::HuC3 {
            // Mode 0 at power-on (SameBoy `GB_configure_cart` zeroes the
            // mapper state): RAM, read-only.
            self.set_huc3_mode(0);
        }
        self.update_banks();
    }

    /// A read of 0000-7FFF.
    #[inline(always)]
    pub(crate) fn read_rom(&self, addr: u16) -> u8 {
        let window = self.rom_win[usize::from(addr >> 14) & 1];
        // One compare against the window length (ADR 0008).
        window
            .get(usize::from(addr) & (ROM_BANK - 1))
            .unwrap_or(0xFF)
    }

    /// A read of A000-BFFF.
    ///
    /// The index is below `ram_len <= ram_buf.len()`, but the compiler
    /// can't see that, so `get` replaces the bounds check: the fallback
    /// is never taken and there is no panic path in the CPU's hot code.
    #[inline]
    pub(crate) fn read_ram(&self, addr: u16) -> u8 {
        if !self.ram_enabled {
            return 0xFF;
        }
        if self.rtc_sel != 0 {
            return self.read_rtc(addr);
        }
        if self.ram_len == 0 {
            return 0xFF;
        }
        self.ram_buf
            .get(self.ram_index(addr))
            .copied()
            .unwrap_or(0xFF)
    }

    /// The 16 bytes [`Self::read_rom`] or [`Self::read_ram`] return for
    /// `addr..addr + 16` (`addr` 16-aligned, below C000), for VRAM DMA
    /// blocks: 16 byte reads from the ROM bank window (never a wide copy:
    /// the ROM may be in memory without unaligned reads, ADR 0027) or one
    /// copy from the RAM bank. The RTC
    /// and Hudson registers don't depend on the address. Every RAM size is 0 or a
    /// power of two of at least 512 (`cart_ram_size`), so the
    /// `ram_len - 1` mask keeps the low 4 bits and the 16 bytes are one
    /// run of `ram_buf`.
    pub(crate) fn dma_block(&self, addr: u16) -> [u8; 16] {
        let (mem, off): (&[u8], usize) = if addr < 0x8000 {
            let window = self.rom_win[usize::from(addr >> 14) & 1];
            let off = usize::from(addr) & (ROM_BANK - 1);
            return Self::rom_block(window, off);
        } else if !self.ram_enabled || (self.ram_mapped() && self.ram_len == 0) {
            return [0xFF; 16];
        } else if !self.ram_mapped() {
            return [self.read_rtc(addr); 16];
        } else {
            debug_assert!(self.ram_len.is_multiple_of(16));
            (self.ram_buf, self.ram_index(addr))
        };
        match mem.as_chunks::<16>().0.get(off >> 4) {
            Some(b) => *b,
            None => Self::block_tail(mem, off),
        }
    }

    /// 16 byte reads from `window` at `off`. When the whole block is in
    /// the window the bounds checks fold away (16 plain `ldrb`, not 16
    /// checked loads: those cost ~500 B of code, ADR 0027); otherwise
    /// [`Self::rom_tail`].
    #[inline(never)]
    fn rom_block(window: Rom<'_>, off: usize) -> [u8; 16] {
        let mut b = [0xFF; 16];
        match window.sub(off, off + 16) {
            Some(src) => {
                for (i, x) in b.iter_mut().enumerate() {
                    *x = src.get(i).unwrap_or(0xFF);
                }
            }
            None => Self::rom_tail(window, off, &mut b),
        }
        b
    }

    /// A block that runs past the end of the ROM bank window (a ROM whose
    /// size isn't a multiple of 16): those bytes read FF, as in
    /// `read_rom`. A loop with a call per byte and a trip count LLVM
    /// can't see, so it stays rolled (code size; it is never hot).
    #[cold]
    #[inline(never)]
    fn rom_tail(window: Rom<'_>, off: usize, b: &mut [u8; 16]) {
        let n = window.len().saturating_sub(off);
        for (i, x) in b.iter_mut().take(n).enumerate() {
            *x = rom_byte(window, off + i);
        }
    }

    /// A block from `off` in `mem` that ends inside it: the bytes past the
    /// end read FF, as in [`Self::read_ram`]. Never taken for the RAM bank
    /// (see [`Self::dma_block`]); the fallback replaces a panic path.
    #[cold]
    #[inline(never)]
    fn block_tail(mem: &[u8], off: usize) -> [u8; 16] {
        let mut b = [0xFF; 16];
        for (x, &y) in b.iter_mut().zip(mem.get(off..).unwrap_or(&[])) {
            *x = y;
        }
        b
    }

    /// A write to A000-BFFF (no panic path; see [`Cart::read_ram`]).
    #[inline]
    pub(crate) fn write_ram(&mut self, addr: u16, value: u8, now: u64) {
        if self.ram_enabled && self.rtc_sel != 0 {
            self.write_rtc(value, now);
        } else if self.ram_enabled && self.ram_len != 0 {
            let i = self.ram_index(addr);
            if let Some(b) = self.ram_buf.get_mut(i) {
                // Only a change dirties the save: games that keep work data
                // in cartridge RAM rewrite the same bytes constantly.
                let v = value | self.ram_or;
                self.save_dirty |= *b != v;
                *b = v;
            }
        }
    }

    /// The RTC state at clock `now` (caught up, without storing it).
    pub(crate) fn rtc_at(&self, now: u64) -> RtcState {
        let mut rtc = self.rtc;
        rtc.advance(now.saturating_sub(self.rtc_synced));
        rtc
    }

    /// Advances the stored clock (the MBC3 RTC, or the HuC3 clock on a
    /// HuC3 cartridge) to clock `now`.
    pub(crate) fn rtc_sync(&mut self, now: u64) {
        if self.mapper == Mapper::HuC3 {
            self.huc3.clock = self.huc3_at(now);
        } else {
            self.rtc = self.rtc_at(now);
        }
        self.rtc_synced = now;
    }

    /// The HuC3 clock state at clock `now` (caught up, without storing
    /// it).
    pub(crate) fn huc3_at(&self, now: u64) -> Huc3RtcState {
        let mut clock = self.huc3.clock;
        clock.advance(now.saturating_sub(self.rtc_synced));
        clock
    }

    /// Replaces the HuC3 clock state as of clock `now`. `state` is masked
    /// ([`Huc3RtcState::masked`]).
    pub(crate) fn set_huc3(&mut self, state: Huc3RtcState, now: u64) {
        self.huc3.clock = state;
        self.rtc_synced = now;
    }

    /// Replaces the RTC state as of clock `now`.
    pub(crate) fn set_rtc(&mut self, state: RtcState, now: u64) {
        self.rtc = state;
        self.rtc_synced = now;
    }

    /// Whether A000-BFFF shows cartridge RAM (read/write, or HuC3's
    /// read-only mode) rather than a register or nothing.
    fn ram_mapped(&self) -> bool {
        matches!(self.rtc_sel, 0 | SEL_RAM_RO)
    }

    /// A000-BFFF read with an RTC register (or nothing) selected: the
    /// latched copy, so no catch-up. Unused bits read 0 (rtc3test "Valid
    /// bits"); the stored registers are already cut to their widths.
    /// Also the way to the Hudson mappers' special modes (`read_huc`),
    /// the only ones that use `addr`.
    #[inline(never)]
    fn read_rtc(&self, addr: u16) -> u8 {
        let t = &self.rtc.latched;
        match self.rtc_sel {
            0x08 => t.seconds,
            0x09 => t.minutes,
            0x0A => t.hours,
            0x0B => t.days as u8,
            0x0C => (t.days >> 8) as u8 & 1 | u8::from(t.halted) << 6 | u8::from(t.day_carry) << 7,
            SEL_RAM_RO..=SEL_IR => self.read_huc(addr),
            _ => 0xFF,
        }
    }

    /// A000-BFFF read in one of the Hudson mappers' special modes. Cold:
    /// it keeps this code out of `read_rtc`, which sits with the hot
    /// code (ITCM), and games spend their time in the plain RAM mode.
    #[cold]
    #[inline(never)]
    fn read_huc(&self, addr: u16) -> u8 {
        match self.rtc_sel {
            // HuC3 mode 0: the RAM reads as usual (Pan Docs: HuC-3, "$0
            // Cart RAM (read-only)"; SameBoy `Core/memory.c`
            // `read_mbc_ram` also reads RAM in mode 0, with a "R/O RAM?
            // (or is it disabled?)" note).
            SEL_RAM_RO if self.ram_len != 0 => self
                .ram_buf
                .get(self.ram_index(addr))
                .copied()
                .unwrap_or(0xFF),
            // The IR register. HuC1: C0 = no light seen, C1 = light (Pan
            // Docs: HuC1, A000-BFFF; SameBoy `read_mbc_ram`: `0xC0 |
            // effective_ir_input`). No IR peer is emulated, so the
            // receiver never sees light. HuC3: Pan Docs says the register
            // is "similar to the equivalent register of the HuC1" with
            // the light in bit 0 and D7 not connected (open bus, usually
            // high), so the same C0 is returned; SameBoy returns only the
            // light bit (00) with a "what are the other bits?" note.
            // Bits 1-6 on a HuC3 are unverified on hardware.
            SEL_IR => 0xC0,
            SEL_CLOCK_RESPONSE => self.huc3.read_response(),
            SEL_CLOCK_SEMAPHORE => self.huc3.read_semaphore(),
            // A HuC3 mode that maps nothing readable (1-9, F), the
            // write-only mode B, or read-only RAM on a cartridge without
            // RAM: Pan Docs says open bus, "often $FF" (SameBoy returns
            // 01 for the unknown modes, with a TODO).
            _ => 0xFF,
        }
    }

    /// A000-BFFF write with an RTC register (or nothing) selected, at
    /// clock `now`. Writes go to the live counters at once, running or
    /// not. Writing the seconds also restarts the sub-second counter, so
    /// the next tick is a full second later; other writes (including
    /// halting and restarting) leave it alone (rtc3test "Sub-second
    /// writes"; SameBoy `Core/memory.c` `write_mbc_ram` clears
    /// `rtc_cycles` on a seconds write only).
    #[inline(never)]
    fn write_rtc(&mut self, v: u8, now: u64) {
        // Nothing mapped (FF) or a Hudson mode.
        if self.rtc_sel >= SEL_RAM_RO {
            return self.write_huc(v, now);
        }
        // Count the time before the write with the old halt state.
        self.rtc_sync(now);
        self.save_dirty = true;
        let t = &mut self.rtc.live;
        match self.rtc_sel {
            0x08 => {
                t.seconds = v & 0x3F;
                self.rtc.subsecond_clocks = 0;
            }
            0x09 => t.minutes = v & 0x3F,
            0x0A => t.hours = v & 0x1F,
            0x0B => t.days = (t.days & 0x100) | u16::from(v),
            0x0C => {
                t.days = (t.days & 0xFF) | u16::from(v & 1) << 8;
                t.halted = v & 0x40 != 0;
                t.day_carry = v & 0x80 != 0;
            }
            _ => {}
        }
    }

    /// A000-BFFF write in one of the Hudson mappers' special modes (or
    /// with nothing mapped). None of them reaches the RAM:
    /// - HuC3 mode 0 is read-only RAM (Pan Docs: HuC-3; SameBoy
    ///   `write_mbc_ram` requires `mbc_ram_enable`, which is `mode ==
    ///   0xA`);
    /// - the IR register only switches the transmitter LED (Pan Docs:
    ///   HuC1), which nothing can see;
    /// - the modes that map nothing ignore writes (Pan Docs: HuC-3).
    ///
    /// The HuC3 clock chip takes a command in mode B and runs it when
    /// the semaphore register (mode D) is written with bit 0 clear (Pan
    /// Docs: HuC-3, "$D - RTC Semaphore"; games write FE). See `huc3.rs`.
    #[inline(never)]
    fn write_huc(&mut self, v: u8, now: u64) {
        match self.rtc_sel {
            SEL_CLOCK_ARG => self.huc3.write_command(v),
            SEL_CLOCK_SEMAPHORE if v & 1 == 0 => {
                // The command may read or set the time: count up to now.
                self.rtc_sync(now);
                self.save_dirty |= self.huc3.execute();
            }
            _ => {}
        }
    }

    /// A write of `value` to the HuC3 mode register (0000-1FFF): what
    /// A000-BFFF shows. Only the low 4 bits count (Pan Docs: HuC-3,
    /// "0000-1FFF - RAM/RTC/IR Select"; SameBoy `Core/gb.h` `mode:4`):
    /// 0 = RAM read-only, A = RAM read/write, B = clock command/argument,
    /// C = clock command/response, D = clock semaphore, E = IR; any other
    /// value maps nothing. `ram_enabled` stays set throughout: the mode
    /// is all in `rtc_sel`, so that plain RAM access (mode A) costs what
    /// it costs on the other mappers.
    fn set_huc3_mode(&mut self, value: u8) {
        self.ram_enabled = true;
        self.rtc_sel = match value & 0x0F {
            0x0A => 0,
            0x00 => SEL_RAM_RO,
            m @ 0x0B..=0x0E => 0x10 | m,
            _ => 0xFF,
        };
    }

    /// RAM smaller than the window (2 KiB) or than the selected bank
    /// mirrors: `ram_len` is a power of two.
    #[inline]
    fn ram_index(&self, addr: u16) -> usize {
        (self.ram_off | (usize::from(addr) & (RAM_BANK - 1))) & (self.ram_len - 1)
    }

    /// MBC30 (a bigger MBC3, e.g. Pokemon Crystal JP: 2 MiB ROM, 64 KiB
    /// RAM). The header has no MBC30 type; SameBoy (`Core/mbc.c`,
    /// `GB_configure_cart`) infers it from `rom_size > 0x200000 ||
    /// mbc_ram_size > 0x8000`. It has an 8-bit ROM bank and 8 RAM banks.
    #[inline]
    fn is_mbc30(&self) -> bool {
        self.rom_mask > 0x1F_FFFF || self.ram_len > 0x8000
    }

    /// A write to 0000-7FFF (the mapper's registers) at clock `now`.
    pub(crate) fn write_rom(&mut self, addr: u16, value: u8, now: u64) {
        match self.mapper {
            Mapper::None => {}
            Mapper::Mbc1 | Mapper::Mbc1M => {
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
            Mapper::Mbc2 => {
                // Pan Docs: MBC2; SameBoy Core/mbc.c (MBC2). Only 0000-3FFF
                // has registers, and address bit 8 picks one: clear = RAM
                // enable, set = ROM bank. Both use the low nibble of the value.
                if addr < 0x4000 {
                    if addr & 0x100 == 0 {
                        self.ram_enabled = value & 0x0F == 0x0A;
                    } else {
                        // 0 selects 1 (checked on the 4-bit value).
                        self.bank1 = (value & 0x0F).max(1);
                    }
                    self.update_banks();
                }
            }
            Mapper::Mbc3 | Mapper::Mbc3Rtc => {
                let rtc = self.mapper == Mapper::Mbc3Rtc;
                // Pan Docs: MBC3; SameBoy Core/mbc.c (MBC3).
                match addr >> 13 {
                    // RAM and RTC enable: 0A in the low nibble.
                    0 => self.ram_enabled = value & 0x0F == 0x0A,
                    // ROM bank: 7 bits, 0 selects 1. MBC30 has an 8-bit
                    // register; the header has no MBC30 type, so it is
                    // inferred (see `is_mbc30`). The mask then wraps the
                    // bank to the ROM size.
                    1 => {
                        let bits = if self.is_mbc30() { 0xFF } else { 0x7F };
                        self.mbc5_rom = u16::from((value & bits).max(1));
                    }
                    // SameBoy (`Core/memory.c` `write_mbc`, MBC3): bit 3
                    // selects the RTC (`rtc_mapped = value & 8`) and the
                    // bank is `value & 7`, so 10-17 select RAM banks and
                    // 18-1F the RTC again. RTC registers are bank 0-4 (08-0C);
                    // 0D-0F read nothing. Without an RTC, bit 3 is ignored.
                    // Without MBC30, banks 4-7 mirror 0-3 (`ram_index`
                    // masks by RAM size), except on RTC carts where SameBoy
                    // reads and writes nothing there (`read_mbc_ram`).
                    2 => {
                        let bank = value & 7;
                        self.mbc5_ram = bank;
                        self.rtc_sel = if rtc && value & 8 != 0 {
                            if bank <= 4 { 8 + bank } else { 0xFF }
                        } else if rtc && bank > 3 && !self.is_mbc30() {
                            0xFF
                        } else {
                            0
                        };
                    }
                    // Latch: writing 00 then 01 copies the live clock to
                    // the registers the game reads (Pan Docs: MBC3). We
                    // follow Pan Docs (bit 0 going 0 -> 1 latches);
                    // SameBoy (`Core/memory.c`) latches on every write to
                    // 6000-7FFF. Unverified on hardware (plan task).
                    _ => {
                        let bit = value & 1 != 0;
                        if bit && !self.rtc_latch && rtc {
                            self.latch_rtc(now);
                        }
                        self.rtc_latch = bit;
                    }
                }
                self.update_banks();
            }
            Mapper::Mbc5 { rumble } => {
                // Pan Docs: MBC5. Registers decode on address bits 12-14.
                match addr >> 12 {
                    // SameBoy (`Core/memory.c`, MBC5): all 8 bits must be
                    // 0A (MBC1/MBC3 compare only the low nibble).
                    0 | 1 => self.ram_enabled = value == 0x0A,
                    2 => self.mbc5_rom = (self.mbc5_rom & 0x100) | u16::from(value),
                    3 => self.mbc5_rom = (self.mbc5_rom & 0xFF) | (u16::from(value & 1) << 8),
                    // Pan Docs: on rumble carts bit 3 drives the motor, so the
                    // RAM bank is 3 bits; SameBoy (Core/mbc.c, MBC5 RAM bank)
                    // masks with 7 when `has_rumble`. Rumble itself is ignored.
                    4 | 5 => self.mbc5_ram = value & if rumble { 0x07 } else { 0x0F },
                    _ => {}
                }
                self.update_banks();
            }
            Mapper::HuC1 => {
                // Pan Docs: HuC1; SameBoy `Core/memory.c` `write_mbc`
                // (GB_HUC1) and `Core/gb.h` (`huc1`: `bank_low:6`,
                // `bank_high:3`), which is where the register widths come
                // from (Pan Docs: "at least 6 bits", "at least 2 bits").
                match addr >> 13 {
                    // RAM/IR select, not a RAM enable: 0E maps the IR
                    // register at A000-BFFF, anything else the RAM (games
                    // still write 0A and 00 here; both mean RAM). SameBoy
                    // compares the low nibble (`(value & 0xF) == 0xE`);
                    // Pan Docs only says "write $0E". Unverified on
                    // hardware which of the two it is for 1E-FE.
                    0 => self.rtc_sel = if value & 0x0F == 0x0E { SEL_IR } else { 0 },
                    // ROM bank, 6 bits. No 0 -> 1 fix-up: SameBoy maps the
                    // value as written (`Core/mbc.c`
                    // `GB_update_mbc_mappings`, GB_HUC1), so 0 puts bank 0
                    // at 4000-7FFF. Pan Docs doesn't say; unverified on
                    // hardware. Power-on is bank 1 (`GB_reset_mbc`).
                    1 => self.mbc5_rom = u16::from(value & 0x3F),
                    // RAM bank; the RAM size masks it (`ram_index`).
                    2 => self.mbc5_ram = value & 0x07,
                    // 6000-7FFF: no effect (Pan Docs: "Nothing?").
                    _ => {}
                }
                self.update_banks();
            }
            Mapper::HuC3 => {
                // Pan Docs: HuC-3; SameBoy `Core/memory.c` `write_mbc`
                // (GB_HUC3) and `Core/gb.h` (`huc3`: `rom_bank:7`,
                // `ram_bank:4`).
                match addr >> 13 {
                    0 => self.set_huc3_mode(value),
                    // ROM bank, 7 bits. Bank 0 can be mapped at 4000-7FFF
                    // (Pan Docs: "Like the MBC5, bank $00 can also be
                    // mapped to this region").
                    1 => self.mbc5_rom = u16::from(value & 0x7F),
                    // RAM bank ("at least 2 bits" in Pan Docs, 4 in
                    // SameBoy); the RAM size masks it (`ram_index`).
                    2 => self.mbc5_ram = value & 0x0F,
                    // 6000-7FFF: games write 01 at startup, with no
                    // observable effect (Pan Docs: "Nothing?").
                    _ => {}
                }
                self.update_banks();
            }
        }
    }

    #[inline(never)]
    fn latch_rtc(&mut self, now: u64) {
        self.rtc_sync(now);
        self.rtc.latched = self.rtc.live;
    }

    /// Recomputes the ROM windows and RAM offset from the mapper registers.
    fn update_banks(&mut self) {
        let (lo, hi, ram) = match self.mapper {
            Mapper::None => (0, 1, 0),
            Mapper::Mbc2 => (0, usize::from(self.bank1), 0),
            Mapper::Mbc3 | Mapper::Mbc3Rtc => (
                0,
                usize::from(self.mbc5_rom),
                usize::from(self.mbc5_ram & 7),
            ),
            Mapper::Mbc5 { .. } | Mapper::HuC1 | Mapper::HuC3 => {
                (0, usize::from(self.mbc5_rom), usize::from(self.mbc5_ram))
            }
            Mapper::Mbc1 | Mapper::Mbc1M => {
                // Multicart: only the low 4 bits of BANK1 reach the ROM
                // address and BANK2 sits above them (Pan Docs: MBC1,
                // "MBC1M"; SameBoy Core/mbc.c, GB_MBC1M_WIRING). The
                // 0-selects-1 fix-up already ran on the 5-bit value.
                let (shift, mask) = if self.mapper == Mapper::Mbc1M {
                    (4, 0x0F)
                } else {
                    (5, 0x1F)
                };
                // Mode 1 also applies BANK2 to 0000-3FFF and to RAM (Pan
                // Docs: MBC1, 6000-7FFF).
                let upper = usize::from(self.bank2) << shift;
                let hi = upper | (usize::from(self.bank1) & mask);
                if self.mode == 0 {
                    (0, hi, 0)
                } else {
                    (upper, hi, usize::from(self.bank2))
                }
            }
        };
        self.rom_win = [self.rom_window(lo), self.rom_window(hi)];
        self.ram_off = ram * RAM_BANK;
    }

    fn rom_window(&self, bank: usize) -> Rom<'a> {
        let start = (bank * ROM_BANK) & self.rom_mask;
        self.rom
            .sub(start, self.rom.len().min(start + ROM_BANK))
            .unwrap_or(Rom::EMPTY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    extern crate std;

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
        rom[HEADER_CART_TYPE] = 0x08; // ROM+RAM
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
        let ej = cart.into_ejected(0).unwrap();
        assert_eq!(ej.save_ram().len(), 0x2000);
        assert!(ej.rtc.is_none());
        let (_, whole) = ej.into_buffers();
        assert_eq!(whole.len(), 0x8000);
        assert_eq!(whole[0x2000], 0x77);
    }

    fn mbc2_cart<'a>(rom: &'a mut [u8], ram: &'a mut [u8]) -> Cart<'a> {
        for (i, chunk) in rom.chunks_mut(ROM_BANK).enumerate() {
            chunk[0x100] = i as u8;
        }
        Cart::new(rom, ram).unwrap()
    }

    fn mbc2_rom() -> std::vec::Vec<u8> {
        let mut rom = std::vec![0u8; 16 * ROM_BANK];
        rom[HEADER_CART_TYPE] = 0x06;
        rom
    }

    #[test]
    fn mbc2_register_select_is_address_bit_8() {
        let mut rom = mbc2_rom();
        let mut ram = [0u8; 512];
        let mut c = mbc2_cart(&mut rom, &mut ram);
        // Bit 8 clear: RAM enable, never the bank, anywhere in 0000-3FFF.
        c.write_rom(0x0000, 0x0A, 0);
        assert_eq!(c.read_rom(0x4100), 1);
        c.write_rom(0x3EFF, 0x05, 0);
        assert_eq!(c.read_rom(0x4100), 1);
        assert!(!c.ram_enabled);
        // Bit 8 set: ROM bank, never RAM enable.
        c.write_rom(0x2100, 0x0A, 0);
        assert_eq!(c.read_rom(0x4100), 0x0A);
        c.write_rom(0x01FF, 0x03, 0);
        assert_eq!(c.read_rom(0x4100), 3);
        // 4000-7FFF has no registers.
        c.write_rom(0x4100, 0x05, 0);
        c.write_rom(0x6000, 0x0A, 0);
        assert_eq!(c.read_rom(0x4100), 3);
        assert!(!c.ram_enabled);
    }

    #[test]
    fn mbc2_bank_zero_maps_to_one_and_uses_4_bits() {
        let mut rom = mbc2_rom();
        let mut ram = [0u8; 512];
        let mut c = mbc2_cart(&mut rom, &mut ram);
        c.write_rom(0x2100, 0x00, 0);
        assert_eq!(c.read_rom(0x4100), 1);
        // Upper bits are ignored, and the zero check is on the 4-bit value.
        c.write_rom(0x2100, 0xF5, 0);
        assert_eq!(c.read_rom(0x4100), 5);
        c.write_rom(0x2100, 0x10, 0);
        assert_eq!(c.read_rom(0x4100), 1);
        assert_eq!(c.read_rom(0x0100), 0);
    }

    #[test]
    fn mbc2_ram_enable_needs_0a_in_low_nibble() {
        let mut rom = mbc2_rom();
        let mut ram = [0u8; 512];
        let mut c = mbc2_cart(&mut rom, &mut ram);
        assert_eq!(c.read_ram(0xA000), 0xFF);
        c.write_rom(0x0000, 0xFA, 0);
        assert!(c.ram_enabled);
        c.write_ram(0xA000, 0x05, 0);
        assert_eq!(c.read_ram(0xA000), 0xF5);
        c.write_rom(0x0000, 0x0B, 0);
        assert!(!c.ram_enabled);
        // Disabled RAM reads 0xFF and ignores writes.
        c.write_ram(0xA000, 0x01, 0);
        assert_eq!(c.read_ram(0xA000), 0xFF);
        c.write_rom(0x0000, 0x0A, 0);
        assert_eq!(c.read_ram(0xA000), 0xF5);
    }

    #[test]
    fn mbc2_ram_is_nibble_wide_and_echoed() {
        let mut rom = mbc2_rom();
        let mut ram = [0u8; 512];
        ram[7] = 0xA3; // junk upper bits in a loaded save
        let mut c = mbc2_cart(&mut rom, &mut ram);
        c.write_rom(0x0000, 0x0A, 0);
        assert_eq!(c.read_ram(0xA007), 0xF3);
        c.write_ram(0xA1FF, 0x5C, 0);
        assert_eq!(c.read_ram(0xA1FF), 0xFC);
        // Only address bits 0-8 matter: echoed through A000-BFFF.
        assert_eq!(c.read_ram(0xA3FF), 0xFC);
        assert_eq!(c.read_ram(0xBFFF), 0xFC);
        c.write_ram(0xB400, 0x02, 0);
        assert_eq!(c.read_ram(0xA000), 0xF2);
        // The save buffer holds the byte as read (upper nibble 1s).
        assert_eq!(c.ram()[0x1FF], 0xFC);
        assert_eq!(c.ram()[0], 0xF2);
        // A save loaded with 0 upper nibbles reads as 1s too.
        assert_eq!(c.ram()[100], 0xF0);
    }

    #[test]
    fn empty_cart_has_no_buffers() {
        assert!(Cart::empty().into_ejected(0).is_none());
    }

    #[test]
    fn battery_from_cart_type() {
        // Pan Docs 0147: the "+BATTERY" types, plus FC-FE (SameBoy).
        let battery = [
            0x03, 0x06, 0x09, 0x0D, 0x0F, 0x10, 0x13, 0x1B, 0x1E, 0x22, 0xFC, 0xFD, 0xFE, 0xFF,
        ];
        for t in 0..=0xFFu8 {
            // The table is the header's (`has_battery`), whatever the
            // mapper; only supported types have a `CartInfo`.
            assert_eq!(has_battery(&rom_with(t, 0x02)), battery.contains(&t));
            let Ok(info) = cart_info(&rom_with(t, 0x02)) else {
                continue;
            };
            assert_eq!(info.battery, battery.contains(&t), "type {t:02X}");
            assert_eq!(info.rtc, matches!(t, 0x0F | 0x10), "type {t:02X}");
        }
        let info = |t, ram| cart_info(&rom_with(t, ram)).unwrap();
        assert_eq!(
            info(0x03, 0x03),
            CartInfo {
                mapper: MapperKind::Mbc1,
                ram_size: 0x8000,
                battery: true,
                rtc: false,
                huc3_rtc: false,
            }
        );
        assert_eq!(info(0x06, 0x00).ram_size, 512);
        assert!(info(0x03, 0x02).has_save());
        assert!(!info(0x02, 0x02).has_save()); // MBC1+RAM, no battery
        assert!(!info(0x03, 0x00).has_save()); // battery, nothing behind it
        assert!(info(0x0F, 0x00).has_save()); // RTC only
        assert_eq!(
            cart_info(&[0; 0x100]),
            Err(LoadErrorKind::RomTooSmall { len: 0x100 })
        );
    }

    /// The codes that load: every type Pan Docs assigns to no mapper,
    /// MBC1, MBC2, MBC3, MBC5, HuC1 or HuC3.
    const SUPPORTED: [u8; 21] = [
        0x00, 0x01, 0x02, 0x03, 0x05, 0x06, 0x08, 0x09, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x19, 0x1A,
        0x1B, 0x1C, 0x1D, 0x1E, 0xFE, 0xFF,
    ];

    /// A4-1/D12: a mapper the core does not emulate is refused with a
    /// typed error that names it, and gets no RAM, battery flag or run.
    #[test]
    fn unsupported_mappers_are_refused_by_name() {
        use MapperKind as K;
        let named = [
            (0x0B, K::Mmm01),
            (0x0C, K::Mmm01),
            (0x0D, K::Mmm01),
            (0x20, K::Mbc6),
            (0x22, K::Mbc7),
            (0xFC, K::PocketCamera),
            (0xFD, K::Tama5),
            // Unassigned codes, the old "MBC4" range (15-17) among them.
            (0x04, K::Unknown),
            (0x07, K::Unknown),
            (0x0A, K::Unknown),
            (0x0E, K::Unknown),
            (0x15, K::Unknown),
            (0x16, K::Unknown),
            (0x17, K::Unknown),
            (0x1F, K::Unknown),
            (0x21, K::Unknown),
            (0x23, K::Unknown),
            (0x80, K::Unknown),
            (0xFB, K::Unknown),
        ];
        for (cart_type, mapper) in named {
            assert_eq!(MapperKind::from_cart_type(cart_type), mapper);
            assert!(!mapper.is_supported());
            // 0149 = 03: 32 KiB of RAM, were the type trusted.
            let rom = rom_with(cart_type, 0x03);
            let kind = LoadErrorKind::UnsupportedMapper { mapper, cart_type };
            assert_eq!(cart_ram_size(&rom), Err(kind), "type {cart_type:02X}");
            assert_eq!(cart_info(&rom), Err(kind), "type {cart_type:02X}");
            // Refused whatever the RAM buffer; the buffers come back.
            for len in [0usize, 0x8000] {
                let mut ram = std::vec![0x5Au8; len];
                let err = Cart::new(&rom, &mut ram).err().expect("refused");
                assert_eq!(err.kind, kind, "type {cart_type:02X}");
                let (r, ram) = err.into_buffers();
                assert_eq!((r.len(), ram.len()), (MIN_ROM_SIZE, len));
                assert!(ram.iter().all(|&b| b == 0x5A));
            }
        }
        // A too-short ROM is still reported as that.
        assert_eq!(
            cart_ram_size(&[0xFF; 0x100]),
            Err(LoadErrorKind::RomTooSmall { len: 0x100 })
        );
    }

    #[test]
    fn supported_kinds_are_the_ones_that_load() {
        let mut ram = std::vec![0u8; 0x8000];
        for t in 0..=0xFFu8 {
            let rom = rom_with(t, 0x03);
            let kind = MapperKind::from_cart_type(t);
            let listed = SUPPORTED.contains(&t);
            assert_eq!(kind.is_supported(), listed, "type {t:02X}");
            assert_eq!(Mapper::from_header(&rom).is_ok(), listed, "type {t:02X}");
            assert_eq!(cart_ram_size(&rom).is_ok(), listed, "type {t:02X}");
            assert_eq!(Cart::new(&rom, &mut ram).is_ok(), listed, "type {t:02X}");
            if listed {
                assert_eq!(cart_info(&rom).unwrap().mapper, kind);
            }
        }
    }

    #[test]
    fn unsupported_mapper_message_names_it() {
        use std::string::ToString;
        let e = cart_info(&rom_with(0xFD, 0x03)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "unsupported cartridge: TAMA5 (header cartridge type FD)"
        );
        let e = cart_info(&rom_with(0x16, 0x03)).unwrap_err();
        assert_eq!(
            e.to_string(),
            "unsupported cartridge: unknown mapper (header cartridge type 16)"
        );
    }

    #[test]
    fn ram_write_dirties_only_on_change() {
        let rom = rom_with(0x03, 0x02);
        let mut buf = [0u8; 0x2000];
        buf[5] = 0x42;
        let mut c = Cart::new(&rom, &mut buf).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        c.write_ram(0xA005, 0x42, 0); // same value
        assert!(!c.save_dirty);
        c.write_ram(0xA006, 0x01, 0);
        assert!(c.save_dirty);
        assert!(c.into_ejected(0).unwrap().save_dirty);
    }

    #[test]
    fn mbc2_ram_write_compares_stored_form() {
        // The buffer holds F0 | nibble, so rewriting the nibble is no change.
        let mut rom = mbc2_rom();
        let mut ram = [0u8; 512];
        let mut c = mbc2_cart(&mut rom, &mut ram);
        c.write_rom(0x0000, 0x0A, 0);
        c.write_ram(0xA000, 0x00, 0);
        c.write_ram(0xA001, 0xF0, 0);
        assert!(!c.save_dirty);
        c.write_ram(0xA001, 0x01, 0);
        assert!(c.save_dirty);
    }

    #[test]
    fn rtc_register_write_dirties_latch_does_not() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        rtc_read(&mut c, 0x08, 5 * SEC); // latch + read
        c.write_rom(0x4000, 0x0D, 0); // maps nothing
        c.write_ram(0xA000, 1, 0);
        assert!(!c.save_dirty);
        rtc_write(&mut c, 0x0C, 0x40, 6 * SEC);
        assert!(c.save_dirty);
    }

    #[test]
    fn ejected_cart_carries_the_rtc_at_removal() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        rtc_write(&mut c, 0x09, 7, SEC / 2);
        let ej = c.into_ejected(3 * SEC + SEC / 2).unwrap();
        let rtc = ej.rtc.unwrap();
        assert_eq!((rtc.live.minutes, rtc.live.seconds), (7, 3));
        assert!(ej.save_dirty);
    }

    /// A ROM of `banks` 16 KiB banks whose bank `n` starts with `n as u8`
    /// and has `n >> 8` at offset 1.
    fn mbc5_rom(banks: usize, cart_type: u8) -> std::vec::Vec<u8> {
        let mut rom = std::vec![0u8; banks * ROM_BANK];
        for b in 0..banks {
            rom[b * ROM_BANK] = b as u8;
            rom[b * ROM_BANK + 1] = (b >> 8) as u8;
        }
        rom[HEADER_CART_TYPE] = cart_type;
        rom[HEADER_RAM_SIZE] = 0x04; // 128 KiB: 16 banks
        rom
    }

    #[test]
    fn mbc5_rom_banking() {
        let rom = mbc5_rom(512, 0x19);
        let mut ram = [0u8; 0x2_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        let at = |c: &Cart, a| u16::from(c.read_rom(a)) | u16::from(c.read_rom(a + 1)) << 8;
        // Power-on: bank 1.
        assert_eq!(at(&c, 0x4000), 1);
        // Bank 0 is selectable (not remapped to 1).
        c.write_rom(0x2000, 0, 0);
        assert_eq!(at(&c, 0x4000), 0);
        assert_eq!(at(&c, 0x0000), 0);
        // 9th bit.
        c.write_rom(0x2000, 0x23, 0);
        c.write_rom(0x3000, 1, 0);
        assert_eq!(at(&c, 0x4000), 0x123);
        // Only bit 0 of the 3000 register counts.
        c.write_rom(0x3FFF, 0xFE, 0);
        assert_eq!(at(&c, 0x4000), 0x023);
        assert_eq!(c.mbc5_rom, 0x023);
        c.write_rom(0x2FFF, 0xFF, 0);
        c.write_rom(0x3000, 1, 0);
        assert_eq!(at(&c, 0x4000), 0x1FF);
        // 0000-3FFF stays on bank 0.
        assert_eq!(at(&c, 0x0000), 0);
    }

    #[test]
    fn mbc5_rom_bank_masks_to_rom_size() {
        let rom = mbc5_rom(4, 0x19);
        let mut ram = [0u8; 0x2_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x2000, 6, 0); // 6 mod 4 = 2
        assert_eq!(c.read_rom(0x4000), 2);
        c.write_rom(0x3000, 1, 0); // 0x106 mod 4 = 2
        assert_eq!(c.read_rom(0x4000), 2);
        c.write_rom(0x2000, 4, 0); // bank 0 mirror
        assert_eq!(c.read_rom(0x4000), 0);
    }

    #[test]
    fn mbc5_ram_banking_and_enable() {
        for (cart_type, mask) in [(0x1B, 0x0F), (0x1E, 0x07)] {
            let rom = mbc5_rom(2, cart_type);
            let mut ram = [0u8; 0x2_0000];
            let mut c = Cart::new(&rom, &mut ram).unwrap();
            // Disabled at power-on.
            assert_eq!(c.read_ram(0xA000), 0xFF);
            c.write_rom(0x0000, 0x0A, 0);
            for bank in 0..16u8 {
                c.write_rom(0x4000, bank, 0);
                c.write_ram(0xA000, bank + 1, 0);
            }
            // Rumble carts: bit 3 does not select a bank.
            for bank in 0..16u8 {
                c.write_rom(0x5FFF, bank, 0);
                let last = (0..16u8).rev().find(|b| b & mask == bank & mask).unwrap();
                assert_eq!(c.read_ram(0xA000), last + 1, "{cart_type:#x} bank {bank}");
            }
            // Only a low nibble of A enables (and the 1000 half decodes too).
            c.write_rom(0x0000, 0x00, 0);
            assert_eq!(c.read_ram(0xA000), 0xFF);
            c.write_rom(0x1000, 0x0A, 0);
            assert_ne!(c.read_ram(0xA000), 0xFF);
            c.write_rom(0x0000, 0x0B, 0);
            assert_eq!(c.read_ram(0xA000), 0xFF);
            // MBC5 compares all 8 bits (SameBoy `value == 0x0A`).
            for v in [0x1A, 0x8A] {
                c.write_rom(0x0000, v, 0);
                assert_eq!(c.read_ram(0xA000), 0xFF, "{v:#x}");
            }
            c.write_rom(0x0000, 0x0A, 0);
            assert_ne!(c.read_ram(0xA000), 0xFF);
        }
    }

    #[test]
    fn mbc3_rom_banking() {
        let rom = mbc5_rom(256, 0x13);
        let mut ram = [0u8; 0x2_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        assert_eq!(c.read_rom(0x4000), 1);
        c.write_rom(0x2000, 0, 0); // 0 selects 1
        assert_eq!(c.read_rom(0x4000), 1);
        c.write_rom(0x3FFF, 0x45, 0);
        assert_eq!(c.read_rom(0x4000), 0x45);
        assert_eq!(c.read_rom(0x0000), 0);
        // 7-bit register below 4 MiB: bit 7 ignored.
        let mut rom = mbc5_rom(128, 0x13);
        rom[HEADER_RAM_SIZE] = 0x03; // 32 KiB: not MBC30 by RAM size
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x2000, 0xC5, 0);
        assert_eq!(c.read_rom(0x4000), 0x45);
        c.write_rom(0x2000, 0x80, 0); // 0 -> 1, not bank 0x80
        assert_eq!(c.read_rom(0x4000), 1);
        // MBC30 (over 2 MiB): 8 bits.
        let rom = mbc5_rom(256, 0x13);
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x2000, 0xC5, 0);
        assert_eq!(c.read_rom(0x4000), 0xC5);
    }

    #[test]
    fn mbc3_rom_bank_masks_to_rom_size() {
        let rom = mbc5_rom(4, 0x13);
        let mut ram = [0u8; 0x2_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x2000, 6, 0); // 6 mod 4 = 2
        assert_eq!(c.read_rom(0x4000), 2);
        c.write_rom(0x2000, 4, 0); // bank 0 mirror; no 0 -> 1 after the mask
        assert_eq!(c.read_rom(0x4000), 0);
    }

    #[test]
    fn mbc3_ram_banking_and_enable() {
        let mut rom = mbc5_rom(2, 0x13);
        rom[HEADER_RAM_SIZE] = 0x03; // 32 KiB: 4 banks
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        assert_eq!(c.read_ram(0xA000), 0xFF); // disabled at power-on
        c.write_rom(0x1FFF, 0x0A, 0);
        for bank in 0..4u8 {
            c.write_rom(0x4000, bank, 0);
            c.write_ram(0xA000, bank + 1, 0);
        }
        for bank in 0..4u8 {
            c.write_rom(0x5FFF, bank, 0);
            assert_eq!(c.read_ram(0xA000), bank + 1);
        }
        // 04-07 mirror 0-3 on 32 KiB. Without an RTC, bit 3 is ignored and
        // the bank is `value & 7` (SameBoy), so 10-17 select RAM too.
        c.write_rom(0x4000, 5, 0);
        assert_eq!(c.read_ram(0xA000), 2);
        for (sel, want) in [
            (0x08, 1),
            (0x0C, 1),
            (0x0D, 2),
            (0x11, 2),
            (0x17, 4),
            (0xFF, 4),
        ] {
            c.write_rom(0x4000, sel, 0);
            assert_eq!(c.read_ram(0xA000), want, "{sel:#x}");
        }
        c.write_rom(0x4000, 1, 0);
        assert_eq!(c.read_ram(0xA000), 2);
        // Only a low nibble of A enables.
        c.write_rom(0x0000, 0x1B, 0);
        assert_eq!(c.read_ram(0xA000), 0xFF);
        c.write_rom(0x0000, 0x1A, 0);
        assert_eq!(c.read_ram(0xA000), 2);
    }

    #[test]
    fn mbc3_rtc_select_routes_a000() {
        let mut rom = mbc5_rom(2, 0x10);
        rom[HEADER_RAM_SIZE] = 0x03;
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        c.write_rom(0x4000, 0, 0);
        c.write_ram(0xA000, 0x77, 0);
        c.rtc.latched.minutes = 0x21;
        c.write_rom(0x4000, 0x09, 0);
        assert_eq!(c.read_ram(0xA000), 0x21); // RTC M, not RAM
        c.write_ram(0xA000, 0x13, 0); // goes to the live register
        assert_eq!(c.rtc.live.minutes, 0x13);
        c.write_rom(0x4000, 0x0D, 0); // RTC bit set, bank 5: nothing
        assert_eq!(c.read_ram(0xA000), 0xFF);
        // 10-17 select RAM banks (SameBoy `rtc_mapped = value & 8`), bank
        // 4-7 reads nothing on a 4-bank MBC3; 18-1F map the RTC again.
        c.write_rom(0x4000, 0x10, 0);
        assert_eq!(c.read_ram(0xA000), 0x77);
        c.write_rom(0x4000, 0x14, 0);
        assert_eq!(c.read_ram(0xA000), 0xFF);
        c.write_rom(0x4000, 0x19, 0);
        assert_eq!(c.read_ram(0xA000), 0x21); // RTC M (latched)
        c.write_rom(0x4000, 0, 0);
        assert_eq!(c.read_ram(0xA000), 0x77); // RAM untouched
        // RTC reads still need the enable.
        c.write_rom(0x4000, 0x09, 0);
        c.write_rom(0x0000, 0, 0);
        assert_eq!(c.read_ram(0xA000), 0xFF);
    }

    /// Writes `v` to RTC register `reg` (08-0C) at clock `now`.
    fn rtc_write(c: &mut Cart<'_>, reg: u8, v: u8, now: u64) {
        c.write_rom(0x4000, reg, now);
        c.write_ram(0xA000, v, now);
    }

    /// Latches at `now` and reads RTC register `reg`.
    fn rtc_read(c: &mut Cart<'_>, reg: u8, now: u64) -> u8 {
        c.write_rom(0x6000, 0, now);
        c.write_rom(0x6000, 1, now);
        c.write_rom(0x4000, reg, now);
        c.read_ram(0xA000)
    }

    const SEC: u64 = crate::CLOCK_HZ as u64;

    /// MBC3+TIMER, no RAM.
    fn rtc_rom() -> std::vec::Vec<u8> {
        let mut rom = mbc5_rom(2, 0x0F);
        rom[HEADER_RAM_SIZE] = 0;
        rom
    }

    #[test]
    fn mbc3_rtc_latch_copies_live_on_0_then_1() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        rtc_write(&mut c, 0x08, 10, 0);
        c.write_rom(0x6000, 0, 0);
        c.write_rom(0x6000, 1, 0);
        c.write_rom(0x4000, 0x08, 0);
        assert_eq!(c.read_ram(0xA000), 10);
        // The latched copy holds until the next 0 -> 1.
        rtc_write(&mut c, 0x08, 20, 3 * SEC);
        c.write_rom(0x6000, 1, 5 * SEC); // 1 -> 1: no latch
        assert_eq!(c.read_ram(0xA000), 10);
        c.write_rom(0x6000, 0, 5 * SEC);
        assert_eq!(c.read_ram(0xA000), 10);
        c.write_rom(0x6000, 1, 5 * SEC + SEC / 2);
        assert_eq!(c.read_ram(0xA000), 22); // 2.5 s after the write
    }

    #[test]
    fn mbc3_rtc_seconds_write_restarts_subsecond_others_do_not() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        // Like RTCS/900: written 1/8 s into a second, next tick 1 s later.
        rtc_write(&mut c, 0x08, 5, SEC + SEC / 8);
        assert_eq!(rtc_read(&mut c, 0x08, 2 * SEC + SEC / 8 - 1), 5);
        assert_eq!(rtc_read(&mut c, 0x08, 2 * SEC + SEC / 8), 6);
        // RTCM/600 (and H, DL, DH alike): the second keeps its phase.
        let t = 2 * SEC + SEC / 8 + 3 * SEC / 8; // next tick at +5/8 s
        for reg in [0x09, 0x0A, 0x0B, 0x0C] {
            rtc_write(&mut c, reg, 0, t);
        }
        assert_eq!(rtc_read(&mut c, 0x08, t + 5 * SEC / 8 - 1), 6);
        assert_eq!(rtc_read(&mut c, 0x08, t + 5 * SEC / 8), 7);
    }

    #[test]
    fn mbc3_rtc_halt_bit_stops_and_resumes() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        // Like RTC off/400: stopped 5/8 s into a second, the next tick comes
        // 3/8 s after restarting, however long it was off.
        rtc_write(&mut c, 0x0C, 0x40, 5 * SEC / 8);
        assert_eq!(rtc_read(&mut c, 0x0C, 50 * SEC), 0x40);
        assert_eq!(rtc_read(&mut c, 0x08, 50 * SEC), 0);
        rtc_write(&mut c, 0x0C, 0x00, 50 * SEC);
        assert_eq!(rtc_read(&mut c, 0x08, 50 * SEC + 3 * SEC / 8 - 1), 0);
        assert_eq!(rtc_read(&mut c, 0x08, 50 * SEC + 3 * SEC / 8), 1);
        // Registers stay writable while halted; unused bits read 0.
        rtc_write(&mut c, 0x0C, 0xFF, 60 * SEC);
        rtc_write(&mut c, 0x0A, 0xFF, 60 * SEC);
        assert_eq!(rtc_read(&mut c, 0x0C, 70 * SEC), 0xC1);
        assert_eq!(rtc_read(&mut c, 0x0A, 70 * SEC), 0x1F);
    }

    #[test]
    fn mbc3_rtc_catches_up_from_last_sync() {
        let rom = rtc_rom();
        let mut ram = [];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        // Hours of emulated time with no cart access: one catch-up.
        let now = (3 * 86_400 + 2 * 3600 + 61) * SEC + 5;
        let s = c.rtc_at(now);
        assert_eq!((s.live.days, s.live.hours), (3, 2));
        assert_eq!((s.live.minutes, s.live.seconds), (1, 1));
        assert_eq!(s.subsecond_clocks, 5);
        assert_eq!(c.rtc.live, RtcTime::default()); // `rtc_at` stores nothing
        c.rtc_sync(now);
        assert_eq!(c.rtc, s);
        assert_eq!(c.rtc_at(now), s); // no double counting
    }

    #[test]
    fn mbc30_ram_has_eight_banks() {
        let mut rom = mbc5_rom(2, 0x13);
        rom[HEADER_RAM_SIZE] = 0x05; // 64 KiB: 8 banks
        let mut ram = [0u8; 0x1_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        for bank in 0..8u8 {
            c.write_rom(0x4000, bank, 0);
            c.write_ram(0xA000, bank + 1, 0);
        }
        for bank in 0..8u8 {
            c.write_rom(0x4000, bank, 0);
            assert_eq!(c.read_ram(0xA000), bank + 1);
        }
    }

    #[test]
    fn mbc3_ram_enable_uses_low_nibble() {
        for v in [0x1A, 0x8A] {
            let mut rom = mbc5_rom(2, 0x13);
            rom[HEADER_RAM_SIZE] = 0x03;
            let mut ram = [0u8; 0x8000];
            let mut c = Cart::new(&rom, &mut ram).unwrap();
            c.write_rom(0x0000, v, 0);
            c.write_ram(0xA000, 5, 0);
            assert_eq!(c.read_ram(0xA000), 5, "{v:#x}");
        }
    }

    #[test]
    fn mbc30_by_ram_size_alone_has_eight_banks_and_8bit_rom_bank() {
        // Pokemon Crystal JP: 2 MiB ROM (128 banks), 64 KiB RAM. SameBoy
        // detects MBC30 from `mbc_ram_size > 0x8000` as well as ROM size.
        let mut rom = mbc5_rom(128, 0x10);
        rom[HEADER_RAM_SIZE] = 0x05;
        let mut ram = [0u8; 0x1_0000];
        let mut c = Cart::new(&rom, &mut ram).unwrap();
        c.write_rom(0x0000, 0x0A, 0);
        c.write_rom(0x2000, 0x80, 0); // 8 bits: bank 0x80 wraps to 0
        assert_eq!(c.read_rom(0x4000), 0);
        // RAM banks 4-7 are real, and 10-17 select them too.
        c.write_rom(0x4000, 6, 0);
        c.write_ram(0xA000, 0x66, 0);
        c.write_rom(0x4000, 0x16, 0);
        assert_eq!(c.read_ram(0xA000), 0x66);
        c.write_rom(0x4000, 0x0E, 0); // RTC bit, bank 6: nothing
        assert_eq!(c.read_ram(0xA000), 0xFF);
    }

    #[test]
    fn rtc_detected_from_type() {
        let rom = rom_with(0x10, 0x03);
        let mut ram = [0u8; 0x8000];
        assert!(Cart::new(&rom, &mut ram).unwrap().has_rtc());
        let rom = rom_with(0x13, 0x03);
        assert!(!Cart::new(&rom, &mut ram).unwrap().has_rtc());
    }
}

#[cfg(test)]
mod mbc1m_tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    const LOGO: [u8; 0x30] = NINTENDO_LOGO;

    /// 1 MiB MBC1 ROM, 64 banks; each bank's first byte is its number.
    /// `logo_at_10`: copy the logo to bank 0x10 (multicart layout).
    fn rom(cart_type: u8, logo_at_10: bool) -> Vec<u8> {
        let mut r = std::vec![0u8; 64 * ROM_BANK];
        for b in 0..64 {
            r[b * ROM_BANK] = b as u8;
        }
        r[HEADER_CART_TYPE] = cart_type;
        r[0x104..0x134].copy_from_slice(&LOGO);
        if logo_at_10 {
            r[0x4_0104..0x4_0134].copy_from_slice(&LOGO);
        }
        r
    }

    fn bank_at(c: &Cart<'_>, addr: u16) -> u8 {
        c.read_rom(addr)
    }

    #[test]
    fn detection() {
        assert!(is_mbc1m(&rom(0x01, true)));
        assert!(!is_mbc1m(&rom(0x01, false)));
        // One differing logo byte is not a copy.
        let mut r = rom(0x01, true);
        r[0x4_0110] ^= 1;
        assert!(!is_mbc1m(&r));
        // Too short to be 8 Mbit.
        assert!(!is_mbc1m(&r[..0x4_0000]));
        // Holds both logos but under SameBoy's 0x44000 minimum.
        assert!(is_mbc1m(&rom(0x01, true)[..0x4_4000]));
        assert!(!is_mbc1m(&rom(0x01, true)[..0x4_3FFF]));
        // Only MBC1 types are multicart candidates.
        assert_eq!(Mapper::from_header(&rom(0x01, true)), Ok(Mapper::Mbc1M));
        assert_eq!(Mapper::from_header(&rom(0x03, true)), Ok(Mapper::Mbc1M));
        assert_eq!(Mapper::from_header(&rom(0x01, false)), Ok(Mapper::Mbc1));
        assert_eq!(
            Mapper::from_header(&rom(0x19, true)),
            Ok(Mapper::Mbc5 { rumble: false })
        );
    }

    /// A4-8: two equal *blank* logo areas (a logo-less homebrew ROM with
    /// filler at the start of bank 0x10) are not a second game's header.
    #[test]
    fn blank_logo_areas_are_not_a_multicart() {
        for fill in [0x00u8, 0xFF] {
            let mut r = std::vec![fill; 64 * ROM_BANK];
            r[HEADER_CART_TYPE] = 0x01;
            r[HEADER_RAM_SIZE] = 0x00;
            // Standard wiring: BANK2 = 1, BANK1 = 1 is bank 0x21, where
            // the multicart wiring would map bank 0x11.
            for b in [0x11usize, 0x21] {
                r[b * ROM_BANK] = b as u8;
            }
            assert!(!is_mbc1m(&r), "fill {fill:#04x}");
            let mut ram = [];
            let mut c = Cart::new(&r, &mut ram).unwrap();
            assert_eq!(c.mapper, Mapper::Mbc1, "fill {fill:#04x}");
            c.write_rom(0x4000, 1, 0);
            c.write_rom(0x2000, 1, 0);
            assert_eq!(bank_at(&c, 0x4000), 0x21, "fill {fill:#04x}");
        }
        // Equal non-blank bytes that are not the logo aren't one either.
        let mut r = std::vec![0u8; 64 * ROM_BANK];
        r[HEADER_CART_TYPE] = 0x01;
        r[0x104..0x134].fill(0xCE);
        r[0x4_0104..0x4_0134].fill(0xCE);
        assert!(!is_mbc1m(&r));
    }
    #[test]
    fn multicart_wires_bank2_at_bit_4() {
        let r = rom(0x01, true);
        let mut ram = [];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        assert_eq!(c.mapper, Mapper::Mbc1M);
        // BANK2 = 2, BANK1 = 3: bank (2 << 4) | 3 = 0x23.
        c.write_rom(0x4000, 2, 0);
        c.write_rom(0x2000, 3, 0);
        assert_eq!(bank_at(&c, 0x4000), 0x23);
        // Only 4 bits of BANK1 count: 0x13 -> 3.
        c.write_rom(0x2000, 0x13, 0);
        assert_eq!(bank_at(&c, 0x4000), 0x23);
        // Mode 0: 0000-3FFF stays bank 0; mode 1 applies BANK2 << 4.
        assert_eq!(bank_at(&c, 0x0000), 0);
        c.write_rom(0x6000, 1, 0);
        assert_eq!(bank_at(&c, 0x0000), 0x20);
        // 0 selects 1 is checked on the 5 bits: 0x10 selects 0 (low 4
        // bits of 0x10), not 1.
        c.write_rom(0x2000, 0x10, 0);
        assert_eq!(bank_at(&c, 0x4000), 0x20);
        c.write_rom(0x2000, 0, 0);
        assert_eq!(bank_at(&c, 0x4000), 0x21);
    }

    #[test]
    fn standard_mbc1_wires_bank2_at_bit_5() {
        let r = rom(0x01, false);
        let mut ram = [];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        c.write_rom(0x4000, 1, 0);
        c.write_rom(0x2000, 3, 0);
        assert_eq!(bank_at(&c, 0x4000), 0x23);
        c.write_rom(0x6000, 1, 0);
        assert_eq!(bank_at(&c, 0x0000), 0x20);
    }

    #[test]
    fn rom_only_type_has_no_ram_but_mbc_types_trust_0149() {
        let mut h = [0u8; MIN_ROM_SIZE];
        h[HEADER_RAM_SIZE] = 0x02;
        h[HEADER_CART_TYPE] = 0x00;
        assert_eq!(cart_ram_size(&h), Ok(0));
        // 01 is "MBC1" (no RAM) but 0149 says 8 KiB: RAM is assumed.
        h[HEADER_CART_TYPE] = 0x01;
        assert_eq!(cart_ram_size(&h), Ok(0x2000));
        // 08 is ROM+RAM.
        h[HEADER_CART_TYPE] = 0x08;
        assert_eq!(cart_ram_size(&h), Ok(0x2000));
    }
}

/// Unit tests for the Hudson mappers. No test ROM in the suite covers
/// them, so these tests and the citations in `mod.rs` are what their
/// behavior rests on.
#[cfg(test)]
mod huc_tests {
    use super::*;
    extern crate std;
    use std::vec::Vec;

    /// A ROM of `banks` banks of the given type whose byte 0 of every bank is
    /// the bank number, with 32 KiB of RAM declared (0149 = 03).
    fn rom(cart_type: u8, banks: usize) -> Vec<u8> {
        let mut rom = std::vec![0u8; banks * ROM_BANK];
        for (i, chunk) in rom.chunks_mut(ROM_BANK).enumerate() {
            chunk[0] = i as u8;
        }
        rom[HEADER_CART_TYPE] = cart_type;
        rom[HEADER_RAM_SIZE] = 0x03;
        rom
    }

    fn bank(c: &Cart<'_>) -> u8 {
        c.read_rom(0x4000)
    }

    #[test]
    fn huc1_header() {
        let r = rom(0xFF, 2);
        assert_eq!(
            cart_info(&r),
            Ok(CartInfo {
                mapper: MapperKind::HuC1,
                ram_size: 0x8000,
                battery: true,
                rtc: false,
                huc3_rtc: false,
            })
        );
        assert!(cart_info(&r).unwrap().has_save());
        let mut r = r;
        r[HEADER_RAM_SIZE] = 0x02;
        assert_eq!(cart_ram_size(&r), Ok(0x2000));
        assert_eq!(Mapper::from_header(&r), Ok(Mapper::HuC1), "type FF is HuC1");
    }

    #[test]
    fn huc1_rom_bank_is_6_bits_and_zero_is_bank_zero() {
        // 128 banks, more than the register reaches, so that its width shows
        // (with 64 the ROM-size mask would hide a wider register).
        let r = rom(0xFF, 128);
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        // Power-on: bank 0 at 0000, bank 1 at 4000.
        assert_eq!((c.read_rom(0x0000), bank(&c)), (0, 1));
        for b in [2u8, 0x1F, 0x20, 0x3F] {
            c.write_rom(0x2000, b, 0);
            assert_eq!(bank(&c), b);
        }
        // Anywhere in 2000-3FFF, and nowhere else.
        c.write_rom(0x3FFF, 0x05, 0);
        assert_eq!(bank(&c), 5);
        c.write_rom(0x1FFF, 0x06, 0);
        c.write_rom(0x4000, 0x07, 0);
        c.write_rom(0x6000, 0x08, 0);
        assert_eq!(bank(&c), 5);
        // Only 6 bits are kept.
        c.write_rom(0x2000, 0xC9, 0);
        assert_eq!(bank(&c), 9);
        // 0 is not turned into 1 (SameBoy; unverified on hardware).
        c.write_rom(0x2000, 0x00, 0);
        assert_eq!(bank(&c), 0);
        c.write_rom(0x2000, 0x40, 0);
        assert_eq!(bank(&c), 0);
        // 0000-3FFF is always bank 0.
        c.write_rom(0x2000, 0x3F, 0);
        assert_eq!(c.read_rom(0x0000), 0);
    }

    #[test]
    fn huc1_rom_bank_masks_to_rom_size() {
        // 8 banks: bank numbers wrap at the ROM size.
        let r = rom(0xFF, 8);
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        c.write_rom(0x2000, 0x0B, 0);
        assert_eq!(bank(&c), 3);
        c.write_rom(0x2000, 0x3F, 0);
        assert_eq!(bank(&c), 7);
        c.write_rom(0x2000, 0x08, 0);
        assert_eq!(bank(&c), 0);
    }

    #[test]
    fn huc1_ram_banking() {
        let r = rom(0xFF, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        for b in 0..4u8 {
            c.write_rom(0x4000, b, 0);
            c.write_ram(0xA000, 0x10 + b, 0);
            c.write_ram(0xBFFF, 0x20 + b, 0);
        }
        for b in 0..4u8 {
            c.write_rom(0x5FFF, b, 0);
            assert_eq!(c.read_ram(0xA000), 0x10 + b);
            assert_eq!(c.read_ram(0xBFFF), 0x20 + b);
            assert_eq!(c.ram()[usize::from(b) * RAM_BANK], 0x10 + b);
        }
        // Bank numbers past the RAM size mirror; other ranges don't select.
        c.write_rom(0x4000, 0x06, 0);
        assert_eq!(c.read_ram(0xA000), 0x12);
        c.write_rom(0x6000, 0x01, 0);
        c.write_rom(0x2000, 0x01, 0);
        assert_eq!(c.read_ram(0xA000), 0x12);
    }

    #[test]
    fn huc1_ram_is_never_disabled() {
        let r = rom(0xFF, 2);
        let mut ram = [0u8; 0x8000];
        ram[3] = 0x77; // a loaded save
        let mut c = Cart::new(&r, &mut ram).unwrap();
        // Readable and writable from power-on, with no 0A written.
        assert_eq!(c.read_ram(0xA003), 0x77);
        c.write_ram(0xA000, 0x42, 0);
        assert_eq!(c.read_ram(0xA000), 0x42);
        assert!(c.save_dirty);
        // 00 ("disable" on other mappers) and 0A both leave the RAM mapped.
        for v in [0x00u8, 0x0A, 0xFF, 0x0F] {
            c.write_rom(0x0000, v, 0);
            assert_eq!(c.read_ram(0xA000), 0x42, "select {v:02X}");
            c.write_ram(0xA001, v, 0);
            assert_eq!(c.read_ram(0xA001), v, "select {v:02X}");
        }
        // Battery RAM: a reset keeps it, and the buffer handed back holds it.
        c.reset();
        assert_eq!(c.read_ram(0xA000), 0x42);
        let ej = c.into_ejected(0).unwrap();
        assert_eq!((ej.save_ram()[0], ej.save_ram()[3]), (0x42, 0x77));
        assert_eq!(ej.rtc, None);
    }

    #[test]
    fn huc1_ir_mode_reads_no_light_and_ignores_writes() {
        let r = rom(0xFF, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = Cart::new(&r, &mut ram).unwrap();
        c.write_ram(0xA000, 0x55, 0);
        c.save_dirty = false;
        c.write_rom(0x0000, 0x0E, 0);
        // The IR register everywhere in A000-BFFF: C0 = no light.
        assert_eq!(c.read_ram(0xA000), 0xC0);
        assert_eq!(c.read_ram(0xBFFF), 0xC0);
        assert_eq!(c.dma_block(0xA000), [0xC0; 16]);
        // Writes drive the LED; they never reach the RAM or dirty the save.
        c.write_ram(0xA000, 0x01, 0);
        c.write_ram(0xA000, 0x00, 0);
        assert_eq!(c.read_ram(0xA000), 0xC0);
        assert!(!c.save_dirty);
        assert_eq!(c.ram()[0], 0x55);
        // Banking still works in IR mode.
        c.write_rom(0x2000, 0x01, 0);
        assert_eq!(bank(&c), 1);
        // Anything else is RAM mode again.
        c.write_rom(0x1FFF, 0x0A, 0);
        assert_eq!(c.read_ram(0xA000), 0x55);
        // The low nibble decides (SameBoy).
        c.write_rom(0x0000, 0x1E, 0);
        assert_eq!(c.read_ram(0xA000), 0xC0);
        c.write_rom(0x0000, 0xE0, 0);
        assert_eq!(c.read_ram(0xA000), 0x55);
        // A reset returns to RAM mode.
        c.write_rom(0x0000, 0x0E, 0);
        c.reset();
        assert_eq!(c.read_ram(0xA000), 0x55);
    }

    fn huc3<'a>(rom: &'a [u8], ram: &'a mut [u8]) -> Cart<'a> {
        Cart::new(rom, ram).unwrap()
    }

    #[test]
    fn huc3_header() {
        let mut r = rom(0xFE, 2);
        let info = cart_info(&r).unwrap();
        assert_eq!(info.mapper, MapperKind::HuC3);
        assert_eq!((info.ram_size, info.battery), (0x8000, true));
        assert!(info.huc3_rtc && !info.rtc);
        assert!(info.has_save());
        // The clock alone is a save, as for MBC3+TIMER+BATTERY.
        r[HEADER_RAM_SIZE] = 0x00;
        assert!(cart_info(&r).unwrap().has_save());
        r[HEADER_RAM_SIZE] = 0x03;
        assert_eq!(Mapper::from_header(&r), Ok(Mapper::HuC3));
        r[HEADER_RAM_SIZE] = 0x02;
        assert_eq!(cart_ram_size(&r), Ok(0x2000));
    }

    #[test]
    fn huc3_rom_bank_is_7_bits_and_zero_is_bank_zero() {
        // 256 banks, more than the register reaches (see the HuC1 test).
        let r = rom(0xFE, 256);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        assert_eq!((c.read_rom(0x0000), bank(&c)), (0, 1));
        for b in [2u8, 0x3F, 0x40, 0x7F] {
            c.write_rom(0x2000, b, 0);
            assert_eq!(bank(&c), b);
        }
        c.write_rom(0x3FFF, 0x05, 0);
        assert_eq!(bank(&c), 5);
        // The other ranges don't select a ROM bank.
        c.write_rom(0x1FFF, 0x06, 0);
        c.write_rom(0x4000, 0x07, 0);
        c.write_rom(0x6000, 0x01, 0);
        assert_eq!(bank(&c), 5);
        // Bit 7 is dropped.
        c.write_rom(0x2000, 0x89, 0);
        assert_eq!(bank(&c), 9);
        // Bank 0 can be mapped at 4000-7FFF (Pan Docs).
        c.write_rom(0x2000, 0x00, 0);
        assert_eq!(bank(&c), 0);
        c.write_rom(0x2000, 0x7F, 0);
        assert_eq!(c.read_rom(0x0000), 0);
    }

    #[test]
    fn huc3_rom_bank_masks_to_rom_size() {
        let r = rom(0xFE, 16);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        c.write_rom(0x2000, 0x13, 0);
        assert_eq!(bank(&c), 3);
        c.write_rom(0x2000, 0x7F, 0);
        assert_eq!(bank(&c), 15);
        c.write_rom(0x2000, 0x10, 0);
        assert_eq!(bank(&c), 0);
    }

    #[test]
    fn huc3_ram_banking() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        c.write_rom(0x0000, 0x0A, 0);
        for b in 0..4u8 {
            c.write_rom(0x4000, b, 0);
            c.write_ram(0xA000, 0x30 + b, 0);
            c.write_ram(0xBFFF, 0x40 + b, 0);
        }
        for b in 0..4u8 {
            c.write_rom(0x5FFF, b, 0);
            assert_eq!(c.read_ram(0xA000), 0x30 + b);
            assert_eq!(c.read_ram(0xBFFF), 0x40 + b);
            assert_eq!(c.ram()[usize::from(b) * RAM_BANK], 0x30 + b);
        }
        // Past the RAM size the bank mirrors; other ranges don't select.
        c.write_rom(0x4000, 0x0E, 0);
        assert_eq!(c.read_ram(0xA000), 0x32);
        c.write_rom(0x6000, 0x01, 0);
        c.write_rom(0x2000, 0x01, 0);
        assert_eq!(c.read_ram(0xA000), 0x32);
    }

    #[test]
    fn huc3_mode_0_is_read_only_ram_and_the_power_on_mode() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        ram[0] = 0x11;
        ram[0x2005] = 0x22;
        let mut c = huc3(&r, &mut ram);
        // Power-on: readable, not writable.
        assert_eq!(c.read_ram(0xA000), 0x11);
        c.write_ram(0xA000, 0x99, 0);
        assert_eq!(c.read_ram(0xA000), 0x11);
        assert!(!c.save_dirty);
        // Mode A writes; mode 0 again (any value with low nibble 0) reads
        // what was written and protects it.
        c.write_rom(0x0000, 0x0A, 0);
        c.write_ram(0xA000, 0x33, 0);
        assert!(c.save_dirty);
        for v in [0x00u8, 0xF0] {
            c.write_rom(0x1FFF, v, 0);
            assert_eq!(c.read_ram(0xA000), 0x33);
            c.write_ram(0xA000, 0x44, 0);
            assert_eq!(c.read_ram(0xA000), 0x33);
        }
        assert_eq!(c.ram()[0], 0x33);
        // Read-only RAM is banked and addressed like read/write RAM.
        c.write_rom(0x4000, 0x01, 0);
        assert_eq!(c.read_ram(0xA005), 0x22);
        assert_eq!(c.read_ram(0xA006), 0x00);
        let mut want = [0u8; 16];
        want[5] = 0x22;
        assert_eq!(c.dma_block(0xA000), want);
        // A reset returns to mode 0 from any mode.
        c.write_rom(0x0000, 0x0A, 0);
        c.reset();
        c.write_ram(0xA000, 0x55, 0);
        assert_eq!(c.read_ram(0xA000), 0x33);
    }

    #[test]
    fn huc3_mode_a_is_read_write_ram_by_low_nibble() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        for (i, v) in [0x0Au8, 0xFA, 0x1A].into_iter().enumerate() {
            c.write_rom(0x0000, 0x00, 0);
            c.write_rom(0x0000, v, 0);
            c.write_ram(0xA010, 0x60 + i as u8, 0);
            assert_eq!(c.read_ram(0xA010), 0x60 + i as u8, "mode {v:02X}");
        }
        let ej = c.into_ejected(0).unwrap();
        assert_eq!(ej.save_ram()[0x10], 0x62);
    }

    #[test]
    fn huc3_ir_mode_reads_no_light_and_ignores_writes() {
        let r = rom(0xFE, 2);
        let mut ram = [0x5Au8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        c.write_rom(0x0000, 0x0E, 0);
        assert_eq!(c.read_ram(0xA000), 0xC0);
        assert_eq!(c.read_ram(0xBFFF), 0xC0);
        assert_eq!(c.dma_block(0xB000), [0xC0; 16]);
        c.write_ram(0xA000, 0x01, 0);
        assert_eq!(c.read_ram(0xA000), 0xC0);
        assert!(!c.save_dirty);
        assert!(c.ram().iter().all(|&b| b == 0x5A));
    }

    #[test]
    fn huc3_other_modes_map_nothing() {
        let r = rom(0xFE, 2);
        let mut ram = [0x5Au8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        for m in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 0x0F, 0xF1, 0x2F] {
            c.write_rom(0x0000, m, 0);
            assert_eq!(c.read_ram(0xA000), 0xFF, "mode {m:02X}");
            assert_eq!(c.dma_block(0xA000), [0xFF; 16], "mode {m:02X}");
            c.write_ram(0xA000, 0x00, 0);
        }
        assert!(!c.save_dirty);
        assert!(c.ram().iter().all(|&b| b == 0x5A));
        // The RAM is back, untouched, in a RAM mode.
        c.write_rom(0x0000, 0x00, 0);
        assert_eq!(c.read_ram(0xA000), 0x5A);
    }

    #[test]
    fn huc3_without_ram_reads_ff_in_both_ram_modes() {
        let mut r = rom(0xFE, 2);
        r[HEADER_RAM_SIZE] = 0x00;
        let mut c = huc3(&r, &mut []);
        for m in [0x00u8, 0x0A] {
            c.write_rom(0x0000, m, 0);
            assert_eq!(c.read_ram(0xA000), 0xFF);
            assert_eq!(c.dma_block(0xA000), [0xFF; 16]);
            c.write_ram(0xA000, 0x12, 0);
        }
        assert!(!c.save_dirty);
    }

    const HZ: u64 = crate::CLOCK_HZ as u64;
    const MINUTE: u64 = 60 * HZ;

    /// Runs one clock command as games do (Pan Docs: HuC-3, "RTC
    /// Communication Protocol"): wait for the semaphore, write the
    /// command and argument in mode B, clear the semaphore in mode D,
    /// wait again, read the response in mode C. Returns the whole
    /// response byte.
    fn command(c: &mut Cart<'_>, value: u8, now: u64) -> u8 {
        c.write_rom(0x0000, 0x0D, now);
        assert_eq!(c.read_ram(0xA000) & 1, 1, "ready before");
        c.write_rom(0x0000, 0x0B, now);
        c.write_ram(0xA000, value, now);
        c.write_rom(0x0000, 0x0D, now);
        c.write_ram(0xA000, 0xFE, now);
        assert_eq!(c.read_ram(0xA000) & 1, 1, "ready after");
        c.write_rom(0x0000, 0x0C, now);
        c.read_ram(0xA000)
    }

    fn set_address(c: &mut Cart<'_>, address: u8, now: u64) {
        command(c, 0x40 | (address & 0x0F), now);
        command(c, 0x50 | (address >> 4), now);
    }

    /// `n` nibbles from `address` on, with the auto-incrementing read.
    fn read_nibbles(c: &mut Cart<'_>, address: u8, n: u32, now: u64) -> u32 {
        set_address(c, address, now);
        (0..n).fold(0, |x, i| {
            x | u32::from(command(c, 0x10, now) & 0x0F) << (4 * i)
        })
    }

    fn write_nibbles(c: &mut Cart<'_>, address: u8, n: u32, value: u32, now: u64) {
        set_address(c, address, now);
        for i in 0..n {
            command(c, 0x30 | (value >> (4 * i)) as u8 & 0x0F, now);
        }
    }

    /// The time as games read it: extended command 0 copies it to
    /// locations 00-06. Returns (days, minute of the day).
    fn read_time(c: &mut Cart<'_>, now: u64) -> (u16, u16) {
        command(c, 0x60, now);
        let t = read_nibbles(c, 0x00, 7, now);
        ((t >> 12) as u16, (t & 0xFFF) as u16)
    }

    /// The time as games set it: locations 00-06, then extended command 1.
    fn set_time(c: &mut Cart<'_>, days: u16, minutes: u16, now: u64) {
        write_nibbles(c, 0x00, 7, u32::from(minutes) | u32::from(days) << 12, now);
        command(c, 0x61, now);
    }

    #[test]
    fn huc3_clock_time_is_set_and_read_through_commands() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        assert_eq!(read_time(&mut c, 0), (0, 0));
        assert!(!c.save_dirty);
        // 12:34 on day 0x1234, each nibble distinct.
        set_time(&mut c, 0x1234, 12 * 60 + 34, 0);
        assert!(c.save_dirty);
        assert_eq!(read_time(&mut c, 0), (0x1234, 754));
        assert_eq!(c.huc3_at(0).minutes, 754);
        assert_eq!(c.huc3_at(0).days, 0x1234);
        // The clock commands never touch the RAM.
        assert!(c.ram().iter().all(|&b| b == 0));
    }

    #[test]
    fn huc3_clock_counts_emulated_minutes_and_days() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 100, 600, 0);
        // 59 seconds later it is still the same minute; at 60 it ticks.
        assert_eq!(read_time(&mut c, 59 * HZ), (100, 600));
        assert_eq!(read_time(&mut c, 60 * HZ - 1), (100, 600));
        assert_eq!(read_time(&mut c, 60 * HZ), (100, 601));
        // N seconds: 3 h 25 min 59 s after the set.
        assert_eq!(
            read_time(&mut c, (3 * 3600 + 25 * 60 + 59) * HZ),
            (100, 805)
        );
        // N days.
        let later = 5 * 1440 * MINUTE + 205 * MINUTE;
        assert_eq!(read_time(&mut c, later), (105, 805));
        // Counting never dirties the save.
        c.save_dirty = false;
        assert_eq!(read_time(&mut c, later + MINUTE), (105, 806));
        assert!(!c.save_dirty);
    }

    #[test]
    fn huc3_clock_day_rollover() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 41, 1439, 0);
        assert_eq!(read_time(&mut c, MINUTE - 1), (41, 1439));
        assert_eq!(read_time(&mut c, MINUTE), (42, 0));
        // The day counter is 16 bits and wraps to 0.
        set_time(&mut c, 0xFFFF, 1439, MINUTE);
        assert_eq!(read_time(&mut c, 2 * MINUTE), (0, 0));
    }

    #[test]
    fn huc3_setting_the_time_restarts_the_minute() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        // 40 s into a minute, the time is set: the next tick is a whole
        // minute later, not 20 s later.
        set_time(&mut c, 0, 10, 40 * HZ);
        assert_eq!(read_time(&mut c, 99 * HZ), (0, 10));
        assert_eq!(read_time(&mut c, 100 * HZ), (0, 11));
    }

    #[test]
    fn huc3_time_buffer_is_a_snapshot() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 3, 59, 0);
        command(&mut c, 0x60, 0);
        // Time passes; the buffer still holds the copy until the next
        // extended command 0.
        let now = 10 * MINUTE;
        let t = read_nibbles(&mut c, 0x00, 7, now);
        assert_eq!((t >> 12, t & 0xFFF), (3, 59));
        assert_eq!(read_time(&mut c, now), (3, 69));
        // Writing the buffer alone does not set the clock.
        write_nibbles(&mut c, 0x00, 7, 0, now);
        assert_eq!(c.huc3_at(now).minutes, 69);
    }

    #[test]
    fn huc3_command_runs_on_the_semaphore_write_only() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 0, 0x5A5, 0);
        command(&mut c, 0x60, 0);
        set_address(&mut c, 0x00, 0);
        // The command alone (mode B) does nothing yet; mode C shows it
        // with the old result.
        c.write_rom(0x0000, 0x0B, 0);
        c.write_ram(0xA000, 0x10, 0);
        c.write_rom(0x0000, 0x0C, 0);
        let before = c.read_ram(0xA000);
        assert_eq!(before & 0x70, 0x10);
        // A semaphore write with bit 0 set does not run it.
        c.write_rom(0x0000, 0x0D, 0);
        c.write_ram(0xA000, 0xFF, 0);
        c.write_ram(0xA000, 0x01, 0);
        c.write_rom(0x0000, 0x0C, 0);
        assert_eq!(c.read_ram(0xA000), before);
        // Bit 0 clear runs it, once per write: nibbles 5, A, 5.
        for want in [0x5, 0xA, 0x5] {
            c.write_rom(0x0000, 0x0D, 0);
            c.write_ram(0xA000, 0x00, 0);
            c.write_rom(0x0000, 0x0C, 0);
            assert_eq!(c.read_ram(0xA000) & 0x0F, want);
        }
        // Writes in mode C are ignored.
        c.write_ram(0xA000, 0x00, 0);
        assert_eq!(c.read_ram(0xA000) & 0x0F, 0x5);
    }

    #[test]
    fn huc3_clock_register_read_values() {
        let r = rom(0xFE, 2);
        let mut ram = [0x5Au8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 0, 0x3AC, 0);
        command(&mut c, 0x60, 0);
        set_address(&mut c, 0x00, 0);
        // Mode C: bit 7 high (not connected), the command in bits 4-6,
        // the result in bits 0-3; the same everywhere in A000-BFFF.
        assert_eq!(command(&mut c, 0x10, 0), 0x80 | 0x10 | 0x0C);
        assert_eq!(c.read_ram(0xBFFF), 0x9C);
        assert_eq!(c.dma_block(0xA000), [0x9C; 16]);
        // Bit 7 of the command written is not connected: 90 is command 1
        // (read), here of the next location.
        assert_eq!(command(&mut c, 0x90, 0), 0x9A);
        // Mode D: ready.
        c.write_rom(0x0000, 0x0D, 0);
        assert_eq!(c.read_ram(0xA000), 0x81);
        assert_eq!(c.read_ram(0xB123), 0x81);
        // Mode B is write-only.
        c.write_rom(0x0000, 0x0B, 0);
        assert_eq!(c.read_ram(0xA000), 0xFF);
        // None of it is RAM.
        assert!(c.ram().iter().all(|&b| b == 0x5A));
    }

    #[test]
    fn huc3_access_address_and_auto_increment() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        // Command 3 writes and increments: 1..7 into locations 00-06.
        write_nibbles(&mut c, 0x00, 7, 0x0765_4321, 0);
        // Commands 4 and 5 set the two address nibbles independently.
        command(&mut c, 0x43, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 4);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 5);
        // The address is now 05: the high nibble changes alone.
        command(&mut c, 0x55, 0); // 55: nothing kept there
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0);
        command(&mut c, 0x50, 0); // 06 (the read incremented it)
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 7);
        // Command 4 leaves the high nibble alone: 58 -> 5A.
        write_nibbles(&mut c, 0x58, 3, 0x321, 0);
        set_address(&mut c, 0x58, 0);
        command(&mut c, 0x4A, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 3);
        c.save_dirty = false;
        // Command 2 writes without incrementing (SameBoy).
        set_address(&mut c, 0x02, 0);
        command(&mut c, 0x2E, 0);
        command(&mut c, 0x2D, 0);
        assert_eq!(read_nibbles(&mut c, 0x00, 7, 0), 0x0765_4D21);
        // The address wraps from FF to 00.
        set_address(&mut c, 0xFF, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 1);
        set_address(&mut c, 0xFF, 0);
        command(&mut c, 0x30, 0);
        command(&mut c, 0x3B, 0);
        assert_eq!(read_nibbles(&mut c, 0x00, 7, 0), 0x0765_4D2B);
        // Commands 0 and 7 do nothing.
        set_address(&mut c, 0x00, 0);
        command(&mut c, 0x0F, 0);
        command(&mut c, 0x7F, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0xB);
        assert!(!c.save_dirty);
    }

    #[test]
    fn huc3_status_and_tone_commands_are_answered_benignly() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 77, 123, 0);
        c.save_dirty = false;
        // Status request: games need the result 1.
        assert_eq!(command(&mut c, 0x62, 0), 0x80 | 0x60 | 1);
        // A read replaces the result; the status request gives 1 again.
        set_address(&mut c, 0x10, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0xB);
        assert_eq!(command(&mut c, 0x62, 0) & 0x0F, 1);
        // The tone (extended command E, twice) and its settings at
        // locations 26-27, and unknown extended commands: accepted,
        // nothing changes.
        let before = c.huc3;
        write_nibbles(&mut c, 0x26, 2, 0x13, 0);
        for ext in [0x6E, 0x6E, 0x63, 0x6F] {
            assert_eq!(command(&mut c, ext, 0) & 0x0F, 1, "{ext:02X}");
        }
        assert_eq!(read_nibbles(&mut c, 0x26, 2, 0), 0);
        assert_eq!(c.huc3.clock, before.clock);
        assert_eq!(read_time(&mut c, 0), (77, 123));
        assert!(!c.save_dirty);
    }

    #[test]
    fn huc3_location_map() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 0xFEDC, 0x4A7, 0);
        c.save_dirty = false;
        // 10-12 and 13-16: the running counters, without a copy.
        assert_eq!(read_nibbles(&mut c, 0x10, 7, 0), 0x0FED_C4A7);
        assert_eq!(read_nibbles(&mut c, 0x10, 3, MINUTE), 0x4A8);
        // They can't be written directly.
        write_nibbles(&mut c, 0x10, 7, 0, MINUTE);
        assert_eq!(read_nibbles(&mut c, 0x10, 7, MINUTE), 0x0FED_C4A8);
        assert!(!c.save_dirty);
        // 58-5A, 5B-5E, 5F: the event (alarm) time, kept and saved.
        write_nibbles(&mut c, 0x58, 8, 0x1876_5321, MINUTE);
        assert!(c.save_dirty);
        assert_eq!(read_nibbles(&mut c, 0x58, 8, MINUTE), 0x1876_5321);
        let s = c.huc3_at(MINUTE);
        assert_eq!((s.alarm_minutes, s.alarm_days), (0x321, 0x8765));
        assert!(s.alarm_enabled);
        // 5F keeps bit 0 only.
        write_nibbles(&mut c, 0x5F, 1, 0xE, MINUTE);
        assert_eq!(read_nibbles(&mut c, 0x5F, 1, MINUTE), 0);
        assert!(!c.huc3_at(MINUTE).alarm_enabled);
        // Setting the time leaves the event time alone.
        set_time(&mut c, 1, 1, MINUTE);
        assert_eq!(read_nibbles(&mut c, 0x58, 7, MINUTE), 0x0876_5321);
        // Everything else reads 0 and is not kept.
        for a in [0x07u8, 0x0F, 0x17, 0x26, 0x57, 0x60, 0xFF] {
            write_nibbles(&mut c, a, 1, 0xF, MINUTE);
            assert_eq!(read_nibbles(&mut c, a, 1, MINUTE), 0, "location {a:02X}");
        }
        assert_eq!(read_time(&mut c, MINUTE), (1, 1));
    }

    #[test]
    fn huc3_out_of_range_minutes_through_the_protocol() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        // FFF minutes (what a fresh SameBoy save holds): counts to the
        // top of the 12 bits and wraps without a day carry.
        set_time(&mut c, 9, 0xFFF, 0);
        assert_eq!(read_time(&mut c, MINUTE - 1), (9, 0xFFF));
        assert_eq!(read_time(&mut c, MINUTE), (9, 0));
    }

    #[test]
    fn huc3_reset_keeps_the_clock_and_clears_the_mailbox() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        set_time(&mut c, 2, 30, 0);
        command(&mut c, 0x60, 0);
        set_address(&mut c, 0x01, 0);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0x1);
        // The emulator catches the clock up, then resets (clock back to 0).
        c.rtc_sync(5 * MINUTE);
        c.reset();
        assert_eq!(c.huc3_at(0).minutes, 35);
        assert_eq!(c.huc3_at(MINUTE).minutes, 36);
        // Mode 0 again, and the mailbox is empty.
        c.write_rom(0x0000, 0x0C, 0);
        assert_eq!(c.read_ram(0xA000), 0x80);
        assert_eq!(command(&mut c, 0x10, 0) & 0x0F, 0, "buffer and address");
        assert_eq!(read_time(&mut c, 0), (2, 35));
    }

    #[test]
    fn huc3_state_restore_and_eject() {
        let r = rom(0xFE, 2);
        let mut ram = [0u8; 0x8000];
        let mut c = huc3(&r, &mut ram);
        let s = Huc3RtcState {
            minutes: 1439,
            days: 6,
            subminute_clocks: (MINUTE - 10) as u32,
            ..Huc3RtcState::default()
        };
        // Restored at clock 1000: it counts from there.
        c.set_huc3(s, 1000);
        assert_eq!(c.huc3_at(1000), s);
        assert_eq!(read_time(&mut c, 1009), (6, 1439));
        assert_eq!(read_time(&mut c, 1010), (7, 0));
        assert!(!c.save_dirty);
        let ej = c.into_ejected(1010 + MINUTE + 3).unwrap();
        let got = ej.huc3_rtc.unwrap();
        assert_eq!((got.days, got.minutes, got.subminute_clocks), (7, 1, 3));
        assert_eq!(ej.rtc, None);
    }

    #[test]
    fn huc1_and_mbc3_have_no_huc3_clock() {
        for t in [0xFFu8, 0x10] {
            let r = rom(t, 2);
            let mut ram = [0u8; 0x8000];
            let c = Cart::new(&r, &mut ram).unwrap();
            assert!(!c.has_huc3_rtc());
            assert!(!cart_info(&r).unwrap().huc3_rtc);
            assert_eq!(c.into_ejected(0).unwrap().huc3_rtc, None);
        }
    }
}
