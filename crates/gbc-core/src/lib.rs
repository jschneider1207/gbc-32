//! Portable Game Boy Color emulator core: `no_std`, no `alloc`, no
//! dependencies (ADR 0001). The caller provides every buffer and drives the
//! emulator; the core never calls out. The API is described in ADR 0006.
//!
//! The emulated model is fixed: CPU CGB revision C (ADR 0003).
//!
//! # Usage (std caller)
//!
//! [`Emulator`] is large, so build it directly in its final location with
//! [`Emulator::init`] rather than on the stack:
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
//! On the MCU, the emulator lives in a `static` of type
//! `MaybeUninit<Emulator<'static>>` (placed in a no-init RAM section) and
//! is initialized with [`Emulator::init`] at startup.

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
pub use cart::{LoadError, MIN_ROM_SIZE, RtcState, RtcTime, cart_ram_size};
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

/// Everything except the large RAM arrays. Small enough (well under 1 KiB)
/// to build on the stack.
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

impl Default for Emulator<'_> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'a> Emulator<'a> {
    /// A powered-off emulator with no cartridge. `const`, so it can
    /// initialize a `static`, but note that this builds the whole (large)
    /// value: prefer [`Emulator::init`] for heap or no-init statics.
    pub const fn new() -> Self {
        Self {
            mem: Memory::new(),
            hw: Hw::new(),
        }
    }

    /// Initializes `slot` in place to the same state as [`Emulator::new`]
    /// and returns it, without building a large temporary on the stack.
    ///
    /// std callers keep a `Box<MaybeUninit<Emulator>>` from
    /// `Box::new_uninit()` and use the returned reference (see the crate
    /// example). No `unsafe` is needed on their side: `Emulator` has no
    /// destructor, so dropping the box as `MaybeUninit` is fine. On the MCU,
    /// pass a `static` `MaybeUninit<Emulator<'static>>` in a no-init RAM
    /// section, which keeps the emulator's image out of flash and `.data`.
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
        // the value is fully initialized and `assume_init_mut` is sound.
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
    /// it (see [`Emulator::cart_ram`]). On error the emulator is unchanged.
    pub fn load(
        &mut self,
        rom: &'a [u8],
        cart_ram: &'a mut [u8],
        config: Config,
    ) -> Result<(), LoadError> {
        self.hw.cart = Cart::new(rom, cart_ram)?;
        self.hw.config = config;
        self.reset();
        Ok(())
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
    /// `run_cycles(FRAME_CLOCKS, ..)`.
    pub fn run_frame(&mut self, frame: &mut Frame, audio: &mut [StereoSample]) -> RunResult {
        self.run_cycles(u64::from(FRAME_CLOCKS), frame, audio)
    }

    /// Runs for at least `clocks` base clocks, or until a stop event. Same
    /// as `run_until(self.clock() + clocks, ..)`.
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
    /// `frame` receives pixels as they are drawn; it holds a complete frame
    /// when the result contains [`Events::FRAME_READY`]. Audio samples are
    /// written from `audio[0]` on; see [`RunResult::audio_samples`].
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
    /// while halted, stopped or locked up).
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

    /// Whether the CPU has locked up on an invalid opcode.
    #[inline]
    pub fn cpu_locked(&self) -> bool {
        self.hw.cpu.locked
    }

    // --- Cartridge state for the caller to persist. ---

    /// The cartridge RAM, for saving while the emulator holds the borrow.
    #[inline]
    pub fn cart_ram(&self) -> &[u8] {
        self.hw.cart.ram
    }

    /// The MBC3 RTC state, or `None` if the cartridge has no RTC.
    pub fn rtc_state(&self) -> Option<RtcState> {
        self.hw.cart.has_rtc.then_some(self.hw.cart.rtc)
    }

    /// Restores the MBC3 RTC state (e.g. from a save, advanced to the
    /// current wall-clock time). Ignored if the cartridge has no RTC.
    pub fn set_rtc_state(&mut self, state: RtcState) {
        if self.hw.cart.has_rtc {
            self.hw.cart.rtc = state;
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
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
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
        // `Emulator::init` builds `Hw` on the stack; only `Memory` is big.
        assert!(size_of::<Hw<'_>>() < 512, "{}", size_of::<Hw<'_>>());
        assert!(size_of::<Memory>() > 48 * 1024);
    }

    #[test]
    fn init_matches_new() {
        let a = Emulator::new();
        let b = Emulator::init(Box::leak(Box::new_uninit()));
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
    fn load_error_leaves_emulator_unchanged() {
        let emu = emulator();
        let tiny = vec![0u8; 16].leak();
        let err = emu.load(tiny, &mut [], Config::default());
        assert_eq!(err, Err(LoadError::RomTooSmall { len: 16 }));
        assert_eq!(emu.peek(0x0100), 0xAB);
    }

    #[test]
    fn run_frame_produces_white_frame_after_frame_clocks() {
        let emu = emulator();
        let mut fb = frame();
        let r = emu.run_frame(&mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(r.clocks, u64::from(FRAME_CLOCKS));
        assert_eq!(r.audio_samples, 0);
        assert!(fb.iter().all(|&p| p == WHITE));
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
        assert_eq!(emu.clock(), u64::from(FRAME_CLOCKS));
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
        assert_eq!(frames, CLOCK_HZ / FRAME_CLOCKS);
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
