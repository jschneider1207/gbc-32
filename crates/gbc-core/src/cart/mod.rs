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
pub fn cart_ram_size(rom: &[u8]) -> Result<usize, LoadErrorKind> {
    if rom.len() < MIN_ROM_SIZE {
        return Err(LoadErrorKind::RomTooSmall { len: rom.len() });
    }
    // Pan Docs: The Cartridge Header, 0147 and 0149.
    // ROM ONLY (00) has no RAM whatever 0149 says. SameBoy (Core/mbc.c,
    // GB_configure_cart) zeroes the RAM size for a no-MBC type whose
    // table entry has no RAM, but for MBC types without RAM and a nonzero
    // 0149 it assumes the RAM exists ("Assuming cartridge has RAM"), which
    // is what the table below does anyway (ADR 0016).
    if header(rom, HEADER_CART_TYPE) == 0x00 {
        return Ok(0);
    }
    if matches!(header(rom, HEADER_CART_TYPE), 0x05 | 0x06) {
        return Ok(512);
    }
    // Unknown codes mean no RAM rather than a load failure, as in SameBoy
    // (Core/mbc.c, GB_configure_cart: a 256-entry `ram_sizes` table where
    // only 0x00-0x05 are non-zero). Bad headers are common in homebrew and
    // test ROMs, which may still run fine without cartridge RAM.
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
    /// [`cart_ram_size`]: bytes of cartridge RAM (the save file length).
    pub ram_size: usize,
    /// The cartridge type (0147) is battery-backed: its RAM (and RTC)
    /// keep their contents with the power off, so they are a save.
    pub battery: bool,
    /// The cartridge has an MBC3 real-time clock
    /// ([`Emulator::rtc_state`](crate::Emulator::rtc_state) is `Some`).
    pub rtc: bool,
}

impl CartInfo {
    /// Whether the cartridge has anything to persist: a battery, and RAM
    /// or an RTC behind it. Without a battery the cartridge RAM is lost at
    /// power off on hardware, so a front end should not write a save file
    /// (nor load one: a game may rely on RAM starting fresh).
    pub const fn has_save(&self) -> bool {
        self.battery && (self.ram_size != 0 || self.rtc)
    }
}

/// [`CartInfo`] from the header, usable before
/// [`Emulator::load`](crate::Emulator::load) (e.g. to decide whether to
/// read a save file). Fails like [`cart_ram_size`].
pub fn cart_info(rom: &[u8]) -> Result<CartInfo, LoadErrorKind> {
    let ram_size = cart_ram_size(rom)?;
    Ok(CartInfo {
        ram_size,
        battery: has_battery(rom),
        rtc: has_rtc(rom),
    })
}

mod rtc;

use crate::rom::Rom;

/// Whether an MBC1 ROM is a multicart (MBC1M), by its contents (ADR 0016).
/// Each game of a multicart has its own header, so the Nintendo logo
/// (0104-0133, 0x30 bytes) is repeated at the start of the game in bank
/// 0x10 (offset 0x40104). Same test as SameBoy (`Core/mbc.c`,
/// GB_configure_cart: ROM of at least 0x44000 bytes and the two logo
/// copies equal).
fn is_mbc1m(rom: &[u8]) -> bool {
    if rom.len() < 0x4_4000 {
        return false;
    }
    // Byte by byte through `Rom::get` (`header`), not a slice compare (a
    // `memcmp` may read words; ADR 0027). Both copies are inside the ROM.
    (0x104..0x134).all(|i| header(rom, i) == header(rom, 0x4_0000 + i))
}

/// Whether the cartridge type has an MBC3 real-time clock.
fn has_rtc(rom: &[u8]) -> bool {
    matches!(header(rom, HEADER_CART_TYPE), 0x0F | 0x10)
}

/// Whether the cartridge type is battery-backed. Pan Docs ("The Cartridge
/// Header", 0147): every type named "+BATTERY" (03 MBC1, 06 MBC2, 09 ROM,
/// 0D MMM01, 0F/10/13 MBC3, 1B/1E MBC5, 22 MBC7, FF HuC1). SameBoy
/// (`Core/mbc.c`, `GB_cart_defs`) also marks the Pocket Camera (FC),
/// TAMA5 (FD) and HuC3 (FE) as battery-backed. (Mappers not emulated yet
/// run as if they had no MBC, but their saves are still saves.)
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
    /// No MBC: 32 KiB of ROM, optional RAM that is always enabled (Pan
    /// Docs: No MBC).
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
}

impl Mapper {
    fn from_header(rom: &[u8]) -> Self {
        match header(rom, HEADER_CART_TYPE) {
            0x01..=0x03 if is_mbc1m(rom) => Self::Mbc1M,
            0x01..=0x03 => Self::Mbc1,
            0x05 | 0x06 => Self::Mbc2,
            0x0F | 0x10 => Self::Mbc3Rtc,
            0x11..=0x13 => Self::Mbc3,
            0x19..=0x1B => Self::Mbc5 { rumble: false },
            0x1C..=0x1E => Self::Mbc5 { rumble: true },
            // TODO(M4): the rest; until then they
            // run as if they had no MBC.
            _ => Self::None,
        }
    }
}

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
    /// below 4 MiB) and `mbc5_ram` the raw 4000-5FFF value.
    mbc5_rom: u16,
    mbc5_ram: u8,
    /// MBC3: the selected RTC register, 08-0C, or 0xFF when the 4000-5FFF
    /// value maps nothing at A000-BFFF (RTC select above 0C, or RAM
    /// bank 4-7 on an RTC cart below MBC30); 0 when RAM is mapped. Kept apart from `mbc5_ram`
    /// so the RAM read path tests one byte.
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
        let kind = match cart_ram_size(rom) {
            Ok(needed) if ram.len() >= needed => {
                let mapper = Mapper::from_header(rom);
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

    /// Whether the cartridge has an MBC3 real-time clock.
    pub(crate) fn has_rtc(&self) -> bool {
        self.mapper == Mapper::Mbc3Rtc
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
        Some(EjectedCart {
            rom: self.rom.into_inner(), // check-rom-loads: allowed (hand-back)
            cart_ram: self.ram_buf,
            rtc,
            save_dirty: self.save_dirty,
        })
    }

    /// Mapper state back to power-on. Cartridge RAM and the RTC keep their
    /// contents (battery backed). The emulator clock restarts at 0, so the
    /// caller first catches the RTC up ([`Cart::rtc_sync`]) if time passed.
    pub(crate) fn reset(&mut self) {
        self.rtc_synced = 0;
        self.rtc_latch = false;
        self.bank1 = 1;
        self.bank2 = 0;
        self.mode = 0;
        self.mbc5_rom = 1;
        self.mbc5_ram = 0;
        self.rtc_sel = 0;
        // Without an MBC there is no enable register.
        self.ram_enabled = self.mapper == Mapper::None;
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
            return self.read_rtc();
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
    /// register doesn't depend on the address. Every RAM size is 0 or a
    /// power of two of at least 512 (`cart_ram_size`), so the
    /// `ram_len - 1` mask keeps the low 4 bits and the 16 bytes are one
    /// run of `ram_buf`.
    pub(crate) fn dma_block(&self, addr: u16) -> [u8; 16] {
        let (mem, off): (&[u8], usize) = if addr < 0x8000 {
            let window = self.rom_win[usize::from(addr >> 14) & 1];
            let off = usize::from(addr) & (ROM_BANK - 1);
            return Self::rom_block(window, off);
        } else if !self.ram_enabled || (self.rtc_sel == 0 && self.ram_len == 0) {
            return [0xFF; 16];
        } else if self.rtc_sel != 0 {
            return [self.read_rtc(); 16];
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

    /// Advances the stored RTC to clock `now`.
    pub(crate) fn rtc_sync(&mut self, now: u64) {
        self.rtc = self.rtc_at(now);
        self.rtc_synced = now;
    }

    /// Replaces the RTC state as of clock `now`.
    pub(crate) fn set_rtc(&mut self, state: RtcState, now: u64) {
        self.rtc = state;
        self.rtc_synced = now;
    }

    /// A000-BFFF read with an RTC register (or nothing) selected: the
    /// latched copy, so no catch-up. Unused bits read 0 (rtc3test "Valid
    /// bits"); the stored registers are already cut to their widths.
    #[inline(never)]
    fn read_rtc(&self) -> u8 {
        let t = &self.rtc.latched;
        match self.rtc_sel {
            0x08 => t.seconds,
            0x09 => t.minutes,
            0x0A => t.hours,
            0x0B => t.days as u8,
            0x0C => (t.days >> 8) as u8 & 1 | u8::from(t.halted) << 6 | u8::from(t.day_carry) << 7,
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
        if self.rtc_sel == 0xFF {
            return;
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
            Mapper::Mbc5 { .. } => (0, usize::from(self.mbc5_rom), usize::from(self.mbc5_ram)),
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
            let info = cart_info(&rom_with(t, 0x02)).unwrap();
            assert_eq!(info.battery, battery.contains(&t), "type {t:02X}");
            assert_eq!(info.rtc, matches!(t, 0x0F | 0x10), "type {t:02X}");
        }
        let info = |t, ram| cart_info(&rom_with(t, ram)).unwrap();
        assert_eq!(
            info(0x03, 0x03),
            CartInfo {
                ram_size: 0x8000,
                battery: true,
                rtc: false
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

    const LOGO: [u8; 0x30] = [0xCE; 0x30];

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
        assert_eq!(Mapper::from_header(&rom(0x01, true)), Mapper::Mbc1M);
        assert_eq!(Mapper::from_header(&rom(0x03, true)), Mapper::Mbc1M);
        assert_eq!(Mapper::from_header(&rom(0x01, false)), Mapper::Mbc1);
        assert_eq!(
            Mapper::from_header(&rom(0x19, true)),
            Mapper::Mbc5 { rumble: false }
        );
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
