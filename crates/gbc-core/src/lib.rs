//! Portable Game Boy Color emulator core: `no_std`, no `alloc`, no
//! dependencies (ADR 0001). The caller provides every buffer and drives the
//! emulator; the core never calls out. The API is described in ADR 0006.
//!
//! The emulated model is fixed: CPU CGB revision C (ADR 0003).
//!
//! # Usage (std caller)
//!
//! [`Emulator`] is about 49 KiB, so build it directly in its final location
//! with [`Emulator::init`] rather than on the stack:
//!
//! ```
//! use gbc_core::{Config, Emulator, Events, Frame, WHITE, cart_ram_size};
//!
//! let rom = vec![0u8; 0x8000]; // normally read from a file
//! let mut cart_ram = vec![0u8; cart_ram_size(&rom).unwrap()];
//!
//! let mut slot = Box::<Emulator>::new_uninit();
//! let emu = Emulator::init(&mut slot); // initialized in place on the heap
//! emu.load(&rom, &mut cart_ram, Config::default()).unwrap();
//!
//! let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
//! let result = emu.run_frame(&mut frame, &mut []);
//! assert!(result.events.contains(Events::FRAME_READY));
//! ```
//!
//! ## Owning a `Box<Emulator>`
//!
//! `init` returns a reference that borrows the `Box<MaybeUninit<_>>`. To
//! store the emulator as a plain `Box<Emulator>` (e.g. in a struct), convert
//! the box after `init`. The core has no `alloc`, so it cannot do this
//! itself; this is the one `unsafe` line a std front end needs:
//!
//! ```
//! use gbc_core::Emulator;
//!
//! fn boxed_emulator() -> Box<Emulator<'static>> {
//!     let mut slot = Box::<Emulator<'static>>::new_uninit();
//!     Emulator::init(&mut slot);
//!     // SAFETY: `Emulator::init` fully initialized the value in `slot`.
//!     unsafe { slot.assume_init() }
//! }
//! # let _ = boxed_emulator();
//! ```
//!
//! A stored emulator still borrows its ROM and cartridge RAM for `'a`.
//! A front end that switches games at run time typically uses
//! `Emulator<'static>` with buffers from `Box::leak` (one per buffer, for
//! the life of the program) and moves them between games with
//! [`Emulator::eject`] and [`Emulator::load`], which always hand the
//! buffers back.
//!
//! # Usage (MCU)
//!
//! The emulator lives in a `static mut MaybeUninit<Emulator<'static>>`
//! (ideally in a no-init RAM section, which keeps its image out of flash
//! and `.data`) and is initialized once at startup. A plain `static` can't
//! hand out `&mut`; in edition 2024 take a raw pointer with `&raw mut`
//! instead of a reference to the `static mut`:
//!
//! ```
//! use core::mem::MaybeUninit;
//! use gbc_core::{Config, Emulator};
//!
//! static mut EMU: MaybeUninit<Emulator<'static>> = MaybeUninit::uninit();
//! static mut SAVE_RAM: [u8; 0x2_0000] = [0; 0x2_0000];
//!
//! fn main() {
//!     // SAFETY: runs once, at startup, before anything else can reach
//!     // these statics, so these are the only references to them ever
//!     // created.
//!     let (emu, save_ram) = unsafe { (&mut *(&raw mut EMU), &mut *(&raw mut SAVE_RAM)) };
//!     let emu = Emulator::init(emu);
//!     let rom: &'static [u8] = &[0; 0x8000]; // memory-mapped flash on the MCU
//!     emu.load(rom, save_ram, Config::default()).unwrap();
//! }
//! ```
//!
//! Alternatively, a "static cell" (a `static` holding an `UnsafeCell` and
//! an atomic taken-flag, as in the `static_cell` crate, in the firmware not
//! the core) gives out the `&'static mut` once without `static mut`. Either
//! way, each buffer is borrowed exactly once for the program's life; to
//! switch games, get the buffers back with [`Emulator::eject`] (or from the
//! result of `load`) and pass them to the next `load`.

#![no_std]

mod apu;
mod bitset;
mod bus;
mod cart;
mod config;
mod cpu;
mod events;
mod joypad;
mod memory;
mod ppu;
mod serial;

use core::mem::MaybeUninit;
use core::ptr::addr_of_mut;

pub use apu::StereoSample;
pub use cart::{
    CartBuffers, LoadError, LoadErrorKind, MIN_ROM_SIZE, RtcState, RtcTime, cart_ram_size,
};
pub use config::{Config, DmgPalettes, rgb555, rgb555_to_888, rgb888_to_555};
pub use cpu::Registers;
pub use events::Events;
pub use joypad::Buttons;
pub use ppu::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH, WHITE};

use cart::Cart;
use cpu::Cpu;
use memory::Memory;
use ppu::Ppu;
use serial::Serial;

/// Base clock rate: 4,194,304 Hz. All times in this API ("clocks") are
/// counted in base clocks, independent of CGB double-speed mode. One
/// M-cycle is 4 base clocks at normal speed and 2 at double speed.
pub const CLOCK_HZ: u32 = 4_194_304;

/// Base clocks per LCD frame (154 lines x 456 dots).
pub const FRAME_CLOCKS: u32 = 70_224;

/// What a run call did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[must_use]
pub struct RunResult {
    /// Every event that occurred during the call.
    pub events: Events,
    /// Base clocks emulated by this call.
    pub clocks: u64,
    /// Number of samples written to the start of the audio buffer passed
    /// to this call (`audio[..audio_samples]`).
    pub audio_samples: usize,
}

/// A Game Boy Color.
///
/// `'a` is the lifetime of the cartridge ROM and RAM borrowed by
/// [`Emulator::load`]. The frame buffer and audio buffer are passed to each
/// run call instead, so the caller can swap them between calls (double
/// buffering) and read them while the emulator is idle.
pub struct Emulator<'a> {
    // Exactly two fields; `Emulator::init` writes each one. See there
    // before adding a field.
    mem: Memory,
    hw: Hw<'a>,
}

/// Everything except the large RAM arrays. `Emulator::init` builds this on
/// the stack, so it must stay small: under 1 KiB, checked by a test. The
/// limit protects the MCU, whose stack is a few KiB and is shared with
/// interrupt handlers. It is not tight: per-scanline PPU buffers, APU state
/// and mapper registers fit with room to spare. Anything large and made
/// only of integers belongs in `Memory` instead, which is zero-filled in
/// place.
struct Hw<'a> {
    cpu: Cpu,
    ppu: Ppu,
    serial: Serial,
    cart: Cart<'a>,
    config: Config,
    buttons: Buttons,
    /// Optional events that end a run call, besides `Events::ALWAYS_STOP`.
    stop_on: Events,
    /// Base clocks since the last load or reset.
    clock: u64,
}

impl Hw<'_> {
    const fn new() -> Self {
        Self {
            cpu: Cpu::new(),
            ppu: Ppu::new(),
            serial: Serial::new(),
            cart: Cart::empty(),
            config: Config {
                dmg_palettes: DmgPalettes::GRAYSCALE,
            },
            buttons: Buttons::EMPTY,
            stop_on: Events::EMPTY,
            clock: 0,
        }
    }
}

impl<'a> Emulator<'a> {
    /// A powered-off emulator with no cartridge, for `const` and `static`
    /// initializers only (evaluated at compile time).
    ///
    /// Do not call this at run time: it builds the whole ~49 KiB value on
    /// the stack, and `Box::new(Emulator::new())` does too before copying.
    /// Use [`Emulator::init`] instead. (There is deliberately no `Default`
    /// impl for the same reason.) A `static` initialized with `new()` is
    /// also stored in flash and copied to RAM at startup, which a no-init
    /// `MaybeUninit` static avoids.
    #[allow(clippy::new_without_default)] // Deliberate: see above.
    pub const fn new() -> Self {
        Self {
            mem: Memory::new(),
            hw: Hw::new(),
        }
    }

    /// Initializes `slot` in place to the same state as [`Emulator::new`]
    /// and returns it, without building a large temporary on the stack.
    ///
    /// std callers either keep the `Box<MaybeUninit<Emulator>>` from
    /// `Box::new_uninit()` and use the returned reference (no `unsafe`;
    /// `Emulator` has no destructor, so dropping the box as `MaybeUninit`
    /// is fine), or convert it to a `Box<Emulator>` with `assume_init`
    /// after this call (sound, because this call initializes every byte
    /// that matters; see the crate docs). On the MCU, pass a `static mut`
    /// `MaybeUninit<Emulator<'static>>` in a no-init RAM section through
    /// `&raw mut` (crate docs, "Usage (MCU)").
    pub fn init(slot: &mut MaybeUninit<Self>) -> &mut Self {
        // Compile-time guard: this exhaustive pattern stops compiling if a
        // field is added to `Emulator`, because the writes below must cover
        // every field.
        let _: fn(&Self) = |e| {
            let Self { mem: _, hw: _ } = e;
        };

        let p = slot.as_mut_ptr();
        // SAFETY: `p` comes from a `&mut MaybeUninit<Self>`, so it is
        // non-null, aligned and valid for writes of one `Self`. `addr_of_mut!`
        // projects to the fields without creating references to
        // uninitialized data. `Memory` contains only `u8` arrays (its
        // documented invariant), so the all-zero bit pattern is a valid
        // `Memory` equal to `Memory::new()`; `write_bytes` initializes it
        // without a stack temporary. `hw` is small and written normally.
        // `mem` and `hw` are all of `Emulator`'s fields (checked above), so
        // the value is fully initialized (padding aside, which needs no
        // initialization) and `assume_init_mut` is sound.
        unsafe {
            addr_of_mut!((*p).mem).write_bytes(0, 1);
            addr_of_mut!((*p).hw).write(Hw::new());
            slot.assume_init_mut()
        }
    }

    /// Inserts a cartridge and powers on (see [`Emulator::reset`]).
    ///
    /// `cart_ram` must hold at least [`cart_ram_size`]`(rom)` bytes; the
    /// caller owns it, loads it from the save file beforehand and persists
    /// it (see [`Emulator::cart_ram`]). Only the first `cart_ram_size(rom)`
    /// bytes are used, so a larger buffer (e.g. one static buffer sized
    /// for the largest game) behaves exactly like one of the header's size.
    ///
    /// Returns the previously inserted cartridge's buffers, if any, so a
    /// game switch never loses them (same as calling [`Emulator::eject`]
    /// first). On error the emulator is unchanged (the previous cartridge
    /// stays inserted) and the error hands back `rom` and `cart_ram`.
    pub fn load(
        &mut self,
        rom: &'a [u8],
        cart_ram: &'a mut [u8],
        config: Config,
    ) -> Result<Option<CartBuffers<'a>>, LoadError<'a>> {
        let cart = Cart::new(rom, cart_ram)?;
        let previous = core::mem::replace(&mut self.hw.cart, cart).into_buffers();
        self.hw.config = config;
        self.reset();
        Ok(previous)
    }

    /// Removes the cartridge and returns its buffers: the ROM and the whole
    /// cartridge RAM buffer as passed to [`Emulator::load`] (not trimmed to
    /// the header size). Save [`Emulator::cart_ram`] (and
    /// [`Emulator::rtc_state`]) first if needed.
    ///
    /// Afterwards the emulator is in the state [`Emulator::init`] leaves it
    /// in: powered off, no cartridge, default settings (stop events, held
    /// buttons and palettes are cleared). Returns `None`, changing nothing,
    /// if no cartridge is inserted.
    pub fn eject(&mut self) -> Option<CartBuffers<'a>> {
        if !self.hw.cart.is_loaded() {
            return None;
        }
        self.mem.clear();
        core::mem::replace(&mut self.hw, Hw::new())
            .cart
            .into_buffers()
    }

    /// Changes the DMG compatibility palettes without a reset. Takes effect
    /// from the next pixel drawn.
    #[inline]
    pub fn set_dmg_palettes(&mut self, palettes: DmgPalettes) {
        self.hw.config.dmg_palettes = palettes;
    }

    /// Power cycle: the post-boot state (no boot ROM is run; registers are
    /// set as the CGB boot ROM leaves them) and the clock back to 0.
    /// Cartridge RAM and the RTC keep their contents; the held buttons and
    /// stop events are kept.
    pub fn reset(&mut self) {
        self.mem.clear();
        self.hw.cpu.reset();
        self.hw.ppu.reset();
        self.hw.serial = Serial::new();
        self.hw.cart.reset();
        self.hw.clock = 0;
    }

    /// Sets the buttons currently held.
    #[inline]
    pub fn set_buttons(&mut self, buttons: Buttons) {
        self.hw.buttons = buttons;
    }

    /// Sets which optional events (e.g. [`Events::LD_B_B`],
    /// [`Events::SERIAL_TX`]) end a run call. [`Events::ALWAYS_STOP`]
    /// events always do. Default: none.
    #[inline]
    pub fn set_stop_events(&mut self, events: Events) {
        self.hw.stop_on = events;
    }

    /// Runs until a frame is ready, or for [`FRAME_CLOCKS`] base clocks if
    /// none completes (LCD off), or until an earlier stop event. Same as
    /// `run_cycles(FRAME_CLOCKS, ..)`. See [`Emulator::run_until`] for the
    /// frame buffer rules.
    pub fn run_frame(&mut self, frame: &mut Frame, audio: &mut [StereoSample]) -> RunResult {
        self.run_cycles(u64::from(FRAME_CLOCKS), frame, audio)
    }

    /// Runs for at least `clocks` base clocks, or until a stop event. Same
    /// as `run_until(self.clock() + clocks, ..)`. See [`Emulator::run_until`]
    /// for the frame buffer rules.
    pub fn run_cycles(
        &mut self,
        clocks: u64,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> RunResult {
        let target = self.hw.clock.saturating_add(clocks);
        self.run_until(target, frame, audio)
    }

    /// Runs until [`Emulator::clock`] `>= target`, or until a stop event.
    ///
    /// Runs end on instruction boundaries, so the clock may overshoot
    /// `target` by part of an instruction (at most a few M-cycles);
    /// [`RunResult::clocks`] and [`Emulator::clock`] give the exact time.
    /// Callers that need a fixed total should loop on absolute targets so
    /// overshoot never accumulates. Returns immediately if `target` has
    /// already passed.
    ///
    /// # Frame buffer
    ///
    /// `frame` receives pixels as they are drawn (line by line); it holds a
    /// complete frame when the result contains [`Events::FRAME_READY`],
    /// which ends the call. Pass the same buffer to every call until then:
    /// swap buffers (double buffering) **only right after a `FRAME_READY`
    /// result**. Swapping at any other point leaves the top of the frame in
    /// one buffer and the rest in the other, so neither holds a whole frame.
    ///
    /// If the game turns the LCD off mid-frame, the partly drawn frame is
    /// abandoned: no `FRAME_READY` is reported for it and the buffer's
    /// contents are unspecified. While the LCD is off
    /// ([`Emulator::lcd_enabled`] is false) no frames complete and the
    /// screen should be shown as white ([`WHITE`]). The first frame after
    /// the LCD is turned back on is not displayed by the hardware either:
    /// its `FRAME_READY` is reported (the timing is real), but the screen
    /// stays white until the `FRAME_READY` after it. The buffer may be
    /// swapped while the LCD is off, as nothing is being drawn.
    /// [`Emulator::screen_blank`] says whether that white screen is
    /// showing (check it after every `FRAME_READY`); unlike polling
    /// `lcd_enabled` after each call, it also catches the LCD going off
    /// and back on within one call.
    ///
    /// # Audio
    ///
    /// Audio samples are written from `audio[0]` on; see
    /// [`RunResult::audio_samples`].
    pub fn run_until(
        &mut self,
        target: u64,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> RunResult {
        let start = self.hw.clock;
        let stop = Events::ALWAYS_STOP | self.hw.stop_on;
        let mut events = Events::EMPTY;
        while self.hw.clock < target {
            events |= self.step_inner(frame, audio);
            if events.intersects(stop) {
                break;
            }
        }
        RunResult {
            events,
            clocks: self.hw.clock - start,
            audio_samples: 0,
        }
    }

    /// Executes one instruction (or interrupt dispatch, or one idle M-cycle
    /// while halted, stopped or locked up). The frame buffer rules of
    /// [`Emulator::run_until`] apply.
    pub fn step(&mut self, frame: &mut Frame, audio: &mut [StereoSample]) -> RunResult {
        let start = self.hw.clock;
        let events = self.step_inner(frame, audio);
        RunResult {
            events,
            clocks: self.hw.clock - start,
            audio_samples: 0,
        }
    }

    /// Skeleton: no CPU yet, so every step is one idle M-cycle.
    #[inline]
    fn step_inner(&mut self, frame: &mut Frame, _audio: &mut [StereoSample]) -> Events {
        let clocks = self.hw.cpu.mcycle_clocks();
        self.hw.clock += u64::from(clocks);
        self.hw.ppu.advance(clocks, frame)
    }

    // --- Debug access: plain field reads, nothing on the hot path. ---

    /// Base clocks since the last load or reset.
    #[inline]
    pub fn clock(&self) -> u64 {
        self.hw.clock
    }

    /// The CPU registers.
    #[inline]
    pub fn registers(&self) -> Registers {
        self.hw.cpu.regs
    }

    /// The last byte sent on the serial port (see [`Events::SERIAL_TX`]).
    #[inline]
    pub fn serial_byte(&self) -> u8 {
        self.hw.serial.last_byte
    }

    /// Whether the LCD is on (LCDC bit 7). While it is off, no frames
    /// complete and the screen shows white.
    #[inline]
    pub fn lcd_enabled(&self) -> bool {
        self.hw.ppu.lcd_enabled()
    }

    /// Whether the screen is blank (white, [`WHITE`]): true while the LCD
    /// is off, and after it is turned back on through the first
    /// [`Events::FRAME_READY`], which the hardware does not display (Pan
    /// Docs: LCDC.7; SameBoy Core/display.c, `frame_skip`). It is cleared
    /// by the second `FRAME_READY` after the LCD is turned on. When false,
    /// the screen shows the frame of the last `FRAME_READY` (if there was
    /// one since the last load or reset). The flag is set when the LCD is
    /// turned off and cleared only by a displayed frame, so it is exact
    /// even if the LCD went off and on again within a single run call (see
    /// [`Emulator::run_until`]).
    #[inline]
    pub fn screen_blank(&self) -> bool {
        self.hw.ppu.screen_blank()
    }

    /// Whether the CPU has locked up on an invalid opcode.
    #[inline]
    pub fn cpu_locked(&self) -> bool {
        self.hw.cpu.locked
    }

    // --- Cartridge state for the caller to persist. ---

    /// The cartridge RAM, for saving while the emulator holds the borrow:
    /// exactly [`cart_ram_size`]`(rom)` bytes (the header size), whatever
    /// the length of the buffer passed to [`Emulator::load`]. Empty if no
    /// cartridge is inserted.
    #[inline]
    pub fn cart_ram(&self) -> &[u8] {
        self.hw.cart.ram()
    }

    /// The MBC3 RTC state, or `None` if the cartridge has no RTC.
    pub fn rtc_state(&self) -> Option<RtcState> {
        self.hw.cart.has_rtc.then_some(self.hw.cart.rtc)
    }

    /// Restores the MBC3 RTC state (e.g. from a save, advanced to the
    /// current wall-clock time). Ignored if the cartridge has no RTC.
    ///
    /// The registers are masked to their hardware widths and
    /// `subsecond_clocks` is clamped below one second (see [`RtcTime`] and
    /// [`RtcState`]), so `rtc_state()` afterwards may differ from `state`.
    pub fn set_rtc_state(&mut self, state: RtcState) {
        if self.hw.cart.has_rtc {
            self.hw.cart.rtc = state.masked();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    use std::vec;

    /// A heap-allocated emulator with a blank 32 KiB ROM loaded. Leaks
    /// everything, which keeps the lifetimes trivial in tests.
    fn emulator() -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x100] = 0xAB;
        let ram = vec![0u8; cart_ram_size(rom).unwrap()].leak();
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        emu.load(rom, ram, Config::default()).unwrap();
        emu
    }

    fn frame() -> Box<Frame> {
        vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT]
            .into_boxed_slice()
            .try_into()
            .unwrap()
    }

    #[test]
    fn hw_is_small_enough_to_build_on_the_stack() {
        // `Emulator::init` builds `Hw` on the stack; only `Memory` is big
        // (see the `Hw` docs for the limit).
        assert!(size_of::<Hw<'_>>() < 1024, "{}", size_of::<Hw<'_>>());
        assert!(size_of::<Memory>() > 48 * 1024);
    }

    #[test]
    fn init_matches_new() {
        // Evaluated at compile time, so nothing large is built on the stack.
        static A: Emulator<'static> = Emulator::new();
        let a = &A;
        let b = Emulator::init(Box::leak(Box::new_uninit()));
        assert!(a.mem == b.mem);
        assert_eq!(a.registers(), b.registers());
        assert_eq!(a.clock(), b.clock());
        assert_eq!(a.lcd_enabled(), b.lcd_enabled());
        assert!(b.mem.wram.iter().all(|&x| x == 0));
        assert!(b.mem.vram.iter().all(|&x| x == 0));
        for addr in 0..=0xFFFF {
            assert_eq!(a.peek(addr), b.peek(addr), "{addr:#06x}");
        }
    }

    #[test]
    fn load_sets_post_boot_state() {
        let emu = emulator();
        assert_eq!(emu.clock(), 0);
        assert_eq!(emu.registers().pc, 0x0100);
        assert_eq!(emu.registers().a, 0x11);
        assert!(emu.lcd_enabled());
        assert_eq!(emu.peek(0x0100), 0xAB);
        assert_eq!(emu.peek(0xFF40), 0x91);
        assert_eq!(emu.rtc_state(), None);
    }

    #[test]
    fn memory_zero_init_equals_cleared_state() {
        // `init` zero-fills `Memory` in place; that must equal both
        // `Memory::new()` and what `reset` (via `Memory::clear`) produces
        // after the RAM has been dirtied.
        let fresh = Emulator::init(Box::leak(Box::new_uninit()));
        let emu = emulator();
        emu.mem.wram.fill(0x5A);
        emu.mem.vram.fill(0xA5);
        emu.mem.oam.fill(1);
        emu.mem.hram.fill(2);
        emu.reset();
        assert!(emu.mem == fresh.mem);
        static NEW: Memory = Memory::new();
        assert!(fresh.mem == NEW);
    }

    #[test]
    fn load_error_returns_buffers_and_leaves_emulator_unchanged() {
        let emu = emulator();
        let tiny = vec![0u8; 16].leak();
        let ram = vec![0u8; 3].leak();
        let err = emu.load(tiny, ram, Config::default()).unwrap_err();
        assert_eq!(err.kind, LoadErrorKind::RomTooSmall { len: 16 });
        let (rom, ram) = err.into_buffers();
        assert_eq!((rom.len(), ram.len()), (16, 3));
        assert_eq!(emu.peek(0x0100), 0xAB);

        // RAM buffer too small for the header.
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x149] = 0x03;
        let err = emu.load(rom, ram, Config::default()).unwrap_err();
        assert_eq!(
            err.kind,
            LoadErrorKind::CartRamTooSmall {
                needed: 0x8000,
                len: 3
            }
        );
        assert_eq!(err.cart_ram.len(), 3);
        assert_eq!(emu.peek(0x0100), 0xAB);
    }

    #[test]
    fn eject_returns_buffers_and_allows_reload() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x100] = 0xAB;
        rom[0x149] = 0x02; // 8 KiB
        let ram = vec![0u8; 0x8000].leak(); // larger than needed
        ram[0] = 0x5A;
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        assert!(emu.eject().is_none());
        assert!(emu.load(rom, ram, Config::default()).unwrap().is_none());
        let mut fb = frame();
        let _ = emu.run_cycles(1000, &mut fb, &mut []);

        let (rom, ram) = emu.eject().unwrap();
        // The whole buffer comes back, not the trimmed one.
        assert_eq!((rom.len(), ram.len()), (MIN_ROM_SIZE, 0x8000));
        assert_eq!(ram[0], 0x5A);
        // Back to the freshly initialized state.
        assert_eq!(emu.clock(), 0);
        assert!(!emu.lcd_enabled());
        assert_eq!(emu.peek(0x0100), 0xFF);
        assert!(emu.cart_ram().is_empty());
        assert!(emu.eject().is_none());

        // The same buffers load again; a second load hands back the first.
        assert!(emu.load(rom, ram, Config::default()).unwrap().is_none());
        assert_eq!(emu.peek(0x0100), 0xAB);
        let rom2 = vec![0u8; MIN_ROM_SIZE].leak();
        let (old_rom, old_ram) = emu.load(rom2, &mut [], Config::default()).unwrap().unwrap();
        assert_eq!(old_rom[0x100], 0xAB);
        assert_eq!(old_ram.len(), 0x8000);
        assert_eq!(emu.peek(0x0100), 0x00);
    }

    #[test]
    fn cart_ram_is_trimmed_to_header_size() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x149] = 0x02; // 8 KiB
        let ram = vec![0u8; 0x2_0000].leak();
        ram[0x2000] = 0x77;
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        emu.load(rom, ram, Config::default()).unwrap();
        assert_eq!(emu.cart_ram().len(), 0x2000);
        // Nothing past the header size is visible to the game.
        assert_eq!(emu.peek(0xA000), 0x00);
    }

    #[test]
    fn unknown_ram_size_code_loads_without_ram() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x147] = 0x03; // MBC1+RAM+BATTERY
        rom[0x149] = 0x42;
        let ram = vec![0u8; 0x2000].leak();
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        emu.load(rom, ram, Config::default()).unwrap();
        assert!(emu.cart_ram().is_empty());
        assert_eq!(emu.peek(0xA000), 0xFF);
    }

    #[test]
    fn set_dmg_palettes_does_not_reset() {
        let emu = emulator();
        let mut fb = frame();
        let _ = emu.run_cycles(1000, &mut fb, &mut []);
        let clock = emu.clock();
        let p = DmgPalettes {
            bg: [1, 2, 3, 4],
            ..DmgPalettes::GRAYSCALE
        };
        emu.set_dmg_palettes(p);
        assert_eq!(emu.hw.config.dmg_palettes, p);
        assert_eq!(emu.clock(), clock);
    }

    /// Clocks from LY=0 dot 0 to the start of VBlank (LY=144).
    const VBLANK: u64 = 144 * 456;

    #[test]
    fn run_frame_produces_white_frame_at_vblank() {
        let emu = emulator();
        let mut fb = frame();
        let r = emu.run_frame(&mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        // FRAME_READY at the start of VBlank, not at the end of the frame.
        assert_eq!(r.clocks, VBLANK);
        assert_eq!(r.audio_samples, 0);
        assert!(fb.iter().all(|&p| p == WHITE));
        // Then one frame every FRAME_CLOCKS.
        for n in 1..4 {
            let r = emu.run_frame(&mut fb, &mut []);
            assert_eq!(r.events, Events::FRAME_READY);
            assert_eq!(emu.clock(), VBLANK + n * u64::from(FRAME_CLOCKS));
        }
    }

    #[test]
    fn run_cycles_stops_at_mcycle_boundary_at_or_after_target() {
        let emu = emulator();
        let mut fb = frame();
        let r = emu.run_cycles(10, &mut fb, &mut []);
        assert_eq!(r.clocks, 12);
        assert_eq!(emu.clock(), 12);
        assert!(r.events.is_empty());
        // A target in the past is a no-op.
        let r = emu.run_until(5, &mut fb, &mut []);
        assert_eq!(r.clocks, 0);
    }

    #[test]
    fn run_stops_early_on_frame_ready() {
        let emu = emulator();
        let mut fb = frame();
        let r = emu.run_cycles(100_000, &mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(emu.clock(), VBLANK);
    }

    #[test]
    fn screen_stays_blank_from_lcd_off_until_the_next_frame() {
        let emu = emulator();
        let mut fb = frame();
        assert!(!emu.screen_blank());
        let _ = emu.run_cycles(1000, &mut fb, &mut []);
        emu.hw.ppu.write_lcdc(0x11); // LCD off
        assert!(emu.screen_blank());
        let r = emu.run_cycles(u64::from(FRAME_CLOCKS) * 2, &mut fb, &mut []);
        assert!(r.events.is_empty());
        emu.hw.ppu.write_lcdc(0x91); // back on: still blank
        assert!(emu.lcd_enabled());
        assert!(emu.screen_blank());
        // The PPU restarts at line 0, so the first frame is ready at the
        // next VBlank. The hardware doesn't display it: still blank.
        let start = emu.clock();
        let r = emu.run_cycles(100_000, &mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(emu.clock() - start, VBLANK);
        assert!(emu.screen_blank());
        // The second frame is displayed.
        let r = emu.run_cycles(100_000, &mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(emu.clock() - start, VBLANK + u64::from(FRAME_CLOCKS));
        assert!(!emu.screen_blank());
        // Writes that keep the LCD on don't blank it.
        emu.hw.ppu.write_lcdc(0x93);
        assert!(!emu.screen_blank());
        // Off and on again with no frame in between: blank, through the
        // first frame after.
        emu.hw.ppu.write_lcdc(0x13);
        emu.hw.ppu.write_lcdc(0x93);
        assert!(emu.screen_blank());
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(emu.screen_blank());
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(!emu.screen_blank());
    }

    #[test]
    fn lcd_on_from_power_off_skips_the_first_frame_too() {
        // The LCD was never on since `init`: turning it on still hides the
        // first frame.
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        let mut fb = frame();
        assert!(emu.screen_blank());
        emu.hw.ppu.write_lcdc(0x91);
        assert!(emu.screen_blank());
        let r = emu.run_frame(&mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert!(emu.screen_blank());
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(!emu.screen_blank());
    }

    #[test]
    fn short_rom_loads_and_reads_ff_past_its_end() {
        let rom = vec![0x11u8; MIN_ROM_SIZE].leak();
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        rom[0x147] = 0;
        rom[0x149] = 0;
        emu.load(rom, &mut [], Config::default()).unwrap();
        assert_eq!(emu.peek(0x0100), 0x11);
        assert_eq!(emu.peek(0x014F), 0x11);
        assert_eq!(emu.peek(0x0150), 0xFF);
        assert_eq!(emu.peek(0x7FFF), 0xFF);
        let short = vec![0u8; MIN_ROM_SIZE - 1].leak();
        let err = emu.load(short, &mut [], Config::default()).unwrap_err();
        assert_eq!(
            err.kind,
            LoadErrorKind::RomTooSmall {
                len: MIN_ROM_SIZE - 1
            }
        );
    }

    #[test]
    fn set_rtc_state_masks_registers() {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x147] = 0x10; // MBC3+TIMER+RAM+BATTERY
        rom[0x149] = 0x03;
        let ram = vec![0u8; 0x8000].leak();
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        emu.load(rom, ram, Config::default()).unwrap();
        assert_eq!(emu.rtc_state(), Some(RtcState::default()));
        let t = RtcTime {
            seconds: 0xFF,
            minutes: 0x7F,
            hours: 0x3F,
            days: 0x3FF,
            halted: true,
            day_carry: false,
        };
        emu.set_rtc_state(RtcState {
            live: t,
            latched: RtcTime {
                seconds: 61,
                ..RtcTime::default()
            },
            subsecond_clocks: CLOCK_HZ,
        });
        let s = emu.rtc_state().unwrap();
        assert_eq!(
            (s.live.seconds, s.live.minutes, s.live.hours, s.live.days),
            (0x3F, 0x3F, 0x1F, 0x1FF)
        );
        assert!(s.live.halted && !s.live.day_carry);
        // 61 fits the 6-bit register: kept, not carried into minutes.
        assert_eq!((s.latched.seconds, s.latched.minutes), (61, 0));
        assert_eq!(s.subsecond_clocks, CLOCK_HZ - 1);
    }

    #[test]
    fn one_emulated_second_by_absolute_target() {
        let emu = emulator();
        let mut fb = frame();
        let target = u64::from(CLOCK_HZ);
        let mut frames = 0;
        while emu.clock() < target {
            let r = emu.run_until(target, &mut fb, &mut []);
            if r.events.contains(Events::FRAME_READY) {
                frames += 1;
            }
        }
        assert_eq!(emu.clock(), target);
        // Frames are ready at VBLANK + k * FRAME_CLOCKS.
        assert_eq!(frames, (target - VBLANK) / u64::from(FRAME_CLOCKS) + 1);
    }

    #[test]
    fn step_is_one_mcycle_when_idle() {
        let emu = emulator();
        let mut fb = frame();
        emu.set_stop_events(Events::LD_B_B | Events::SERIAL_TX);
        let r = emu.step(&mut fb, &mut []);
        assert_eq!(r.clocks, 4);
        assert!(r.events.is_empty());
    }

    #[test]
    fn reset_rewinds_clock_and_keeps_cart_ram() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x149] = 0x02;
        let ram = vec![0u8; 0x2000].leak();
        ram[0] = 0x5A;
        let emu = Emulator::init(Box::leak(Box::new_uninit()));
        emu.load(rom, ram, Config::default()).unwrap();
        let mut fb = frame();
        let _ = emu.run_cycles(1000, &mut fb, &mut []);
        emu.reset();
        assert_eq!(emu.clock(), 0);
        assert_eq!(emu.cart_ram()[0], 0x5A);
        assert_eq!(emu.peek(0xA000), 0x5A);
    }
}
