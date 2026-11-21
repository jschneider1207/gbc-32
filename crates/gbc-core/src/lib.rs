//! Portable Game Boy Color emulator core: `no_std`, no `alloc`, no
//! dependencies (ADR 0001), and no `unsafe` code (ADR 0009). The caller
//! provides every buffer and drives the emulator; the core never calls
//! out. The API is described in ADR 0006 (construction: ADR 0009).
//!
//! The emulated model is fixed: CPU CGB revision C (ADR 0003).
//!
//! # Construction
//!
//! The large RAM arrays (~49 KiB) are a separate type, [`Memory`], which
//! the caller provides; [`Emulator`] itself is small (under 1 KiB plus a
//! pointer) and borrows it, along with the cartridge ROM and RAM:
//!
//! - [`Memory::new`] is a `const fn` that is all zero bytes;
//! - [`Emulator::new`]`(&'a mut Memory) -> Emulator<'a>` (powered off, no
//!   cartridge; it zero-fills the memory, so its previous contents don't
//!   matter);
//! - [`Emulator::load`] inserts a cartridge and [`Emulator::eject`] hands
//!   its buffers back.
//!
//! # Usage (std caller)
//!
//! Put the `Memory` on the heap; the `Emulator` can live anywhere:
//!
//! ```
//! use gbc_core::{Config, Emulator, Events, Frame, Memory, WHITE, cart_ram_size};
//!
//! let rom = vec![0u8; 0x8000]; // normally read from a file
//! let mut cart_ram = vec![0u8; cart_ram_size(&rom).unwrap()];
//!
//! let mut mem = Box::new(Memory::new());
//! let mut emu = Emulator::new(&mut mem);
//! emu.load(&rom, &mut cart_ram, Config::default()).unwrap();
//!
//! let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
//! let result = emu.run_frame(&mut frame, &mut []);
//! assert!(result.events.contains(Events::FRAME_READY));
//! ```
//!
//! An emulator stored in a long-lived struct borrows its memory, ROM and
//! cartridge RAM for `'a`. A front end that switches games at run time
//! typically uses `Emulator<'static>` with buffers from `Box::leak` (one
//! per buffer, for the life of the program) and moves the cartridge
//! buffers between games with [`Emulator::eject`] and
//! [`Emulator::load`], which always hand them back:
//!
//! ```
//! use gbc_core::{Emulator, Memory};
//!
//! fn boxed_emulator() -> Box<Emulator<'static>> {
//!     Box::new(Emulator::new(Box::leak(Box::new(Memory::new()))))
//! }
//! # let _ = boxed_emulator();
//! ```
//!
//! # Usage (MCU)
//!
//! The memory is a `static`: `Memory::new()` is all zero bytes, so it goes
//! to `.bss` (no image in flash, no copy at boot, nothing on the stack).
//! The small `Emulator` can live on the stack of the main loop or in a
//! static of its own. Getting the `&'static mut` borrows out of statics,
//! exactly once, is the firmware's business, not the core's: for example a
//! `static MEM: ConstStaticCell<Memory> = ConstStaticCell::new(Memory::new());`
//! from the `static_cell` crate (whose `take()` returns the `&'static mut`
//! once, and which is still all zeros, so still `.bss`), or a
//! `static mut MEM: Memory = Memory::new();` reached through `&raw mut`
//! with a small audited `unsafe` in the platform layer. The core's side is
//! all safe:
//!
//! ```
//! use gbc_core::{Buttons, Config, Emulator, Frame, Memory};
//!
//! /// Firmware main loop. `mem` and `save_ram` come from statics, handed
//! /// out once at startup; `rom` is memory-mapped flash.
//! fn run(
//!     mem: &'static mut Memory,
//!     save_ram: &'static mut [u8],
//!     rom: &'static [u8],
//!     frame: &mut Frame,
//! ) {
//!     let mut emu = Emulator::new(mem);
//!     emu.load(rom, save_ram, Config::default()).unwrap();
//!     // Once per frame (the firmware's main loop):
//!     emu.set_buttons(Buttons::EMPTY); // read from the GPIOs
//!     let _ = emu.run_frame(frame, &mut []);
//!     // To switch games, `emu.eject()` (or the result of `load`) hands
//!     // back the ROM and `save_ram` for the next `load`.
//! }
//! # let mem = Box::leak(Box::new(Memory::new()));
//! # let save = Box::leak(vec![0u8; 0x2_0000].into_boxed_slice());
//! # let rom = Box::leak(vec![0u8; 0x8000].into_boxed_slice());
//! # let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
//! # run(mem, save, rom, &mut frame);
//! ```

#![no_std]
#![forbid(unsafe_code)]

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
mod timer;

pub use apu::StereoSample;
pub use cart::{
    CartBuffers, LoadError, LoadErrorKind, MIN_ROM_SIZE, RtcState, RtcTime, cart_ram_size,
};
pub use config::{Config, DmgPalettes, rgb555, rgb555_to_888, rgb888_to_555};
pub use cpu::Registers;
pub use events::Events;
pub use joypad::Buttons;
pub use memory::Memory;
pub use ppu::{Frame, SCREEN_HEIGHT, SCREEN_WIDTH, WHITE};

/// Direct access to the SM83 CPU, for instruction-level tests that drive it
/// with their own [`sm83::Bus`] (`tools/sm83-tests`). Front ends don't need
/// this; [`Emulator`] wires the CPU to the real bus.
pub mod sm83 {
    pub use crate::cpu::{Bus, Cpu, Registers};
}

use bus::{Ctx, Sys};
use cart::Cart;
use cpu::Cpu;

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
/// `'a` is the lifetime of the [`Memory`] borrowed by [`Emulator::new`] and
/// of the cartridge ROM and RAM borrowed by [`Emulator::load`]. The frame
/// buffer and audio buffer are passed to each run call instead, so the
/// caller can swap them between calls (double buffering) and read them
/// while the emulator is idle.
///
/// The emulator itself is small (`Hw`, under 1 KiB, plus the borrows), so
/// it can live on the stack, even on the MCU.
pub struct Emulator<'a> {
    /// The large RAM arrays, provided by the caller (ADR 0009). Run calls
    /// copy this pointer into the per-call `Ctx`, so the hot path reaches
    /// memory through the context exactly as before the split.
    mem: &'a mut Memory,
    hw: Hw<'a>,
}

/// Everything except the large RAM arrays, which live in [`Memory`]. It
/// must stay small: under 1 KiB, checked by a test. The limit protects the
/// MCU, whose stack is a few KiB and is shared with interrupt handlers, and
/// where the `Emulator` (and so `Hw`) may live on the stack. It is not
/// tight: per-scanline PPU buffers, APU state and mapper registers fit with
/// room to spare. Anything large belongs in `Memory` instead, as a field
/// whose power-on value is all zeros (non-zero post-boot values are
/// written by `reset`).
///
/// The CPU is kept apart from [`Sys`] (what its bus reaches) so a run call
/// can borrow both mutably at once: `cpu.run(&mut Ctx { sys, mem, .. })`
/// (ADR 0008).
struct Hw<'a> {
    cpu: Cpu,
    sys: Sys<'a>,
    /// Optional events that end a run call, besides `Events::ALWAYS_STOP`.
    stop_on: Events,
}

impl Hw<'_> {
    const fn new() -> Self {
        Self {
            cpu: Cpu::powered_off(),
            sys: Sys::new(),
            stop_on: Events::EMPTY,
        }
    }
}

impl<'a> Emulator<'a> {
    /// A powered-off emulator with no cartridge, using `mem` for its RAM.
    ///
    /// `mem` is zero-filled in place, so its previous contents (e.g. from
    /// an earlier emulator) don't matter and the result is always the same
    /// state. That write is also why this is not a `const fn`: a `const`
    /// initializer couldn't write to another static's memory anyway. The
    /// `Memory` itself is the caller's: `Box::new(Memory::new())` on a PC, a
    /// `static` (in `.bss`) on the MCU; see the crate docs.
    pub fn new(mem: &'a mut Memory) -> Self {
        mem.clear();
        Self { mem, hw: Hw::new() }
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
        let previous = core::mem::replace(&mut self.hw.sys.cart, cart).into_buffers();
        self.hw.sys.config = config;
        self.reset();
        Ok(previous)
    }

    /// Removes the cartridge and returns its buffers: the ROM and the whole
    /// cartridge RAM buffer as passed to [`Emulator::load`] (not trimmed to
    /// the header size). Save [`Emulator::cart_ram`] (and
    /// [`Emulator::rtc_state`]) first if needed.
    ///
    /// Afterwards the emulator is in the state [`Emulator::new`] leaves it
    /// in: powered off, no cartridge, default settings (stop events, held
    /// buttons and palettes are cleared). Returns `None`, changing nothing,
    /// if no cartridge is inserted.
    pub fn eject(&mut self) -> Option<CartBuffers<'a>> {
        if !self.hw.sys.cart.is_loaded() {
            return None;
        }
        self.mem.clear();
        core::mem::replace(&mut self.hw, Hw::new())
            .sys
            .cart
            .into_buffers()
    }

    /// Changes the DMG compatibility palettes without a reset. Takes effect
    /// from the next pixel drawn.
    #[inline]
    pub fn set_dmg_palettes(&mut self, palettes: DmgPalettes) {
        self.hw.sys.config.dmg_palettes = palettes;
    }

    /// Power cycle: the post-boot state (no boot ROM is run; registers are
    /// set as the CGB boot ROM leaves them) and the clock back to 0.
    /// Cartridge RAM and the RTC keep their contents; the held buttons and
    /// stop events are kept.
    pub fn reset(&mut self) {
        self.mem.clear();
        let sys = &mut self.hw.sys;
        sys.reset(self.mem);
        self.hw.cpu.reset(sys.cart.rom, sys.cgb_mode);
    }

    /// Sets the buttons currently held. Pressing a button in a group the
    /// game has selected in P1 requests the joypad interrupt (Pan Docs:
    /// Interrupt Sources, "INT $60 - Joypad interrupt"), which also ends
    /// HALT if enabled in IE.
    #[inline]
    pub fn set_buttons(&mut self, buttons: Buttons) {
        let sys = &mut self.hw.sys;
        sys.set_joypad(buttons, sys.joyp_select);
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
        let target = self.hw.sys.hot.clock().saturating_add(clocks);
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
        self.run(Some(target), frame, audio)
    }

    /// Executes one instruction (or interrupt dispatch, or one idle M-cycle
    /// while halted, stopped or locked up). The frame buffer rules of
    /// [`Emulator::run_until`] apply.
    pub fn step(&mut self, frame: &mut Frame, audio: &mut [StereoSample]) -> RunResult {
        self.run(None, frame, audio)
    }

    /// The run call: one step if `target` is `None`, else steps until the
    /// clock reaches `target` or a stop event. Builds the per-call context
    /// (ADR 0008) once; the CPU gets a pointer to it and runs the
    /// instruction loop itself ([`Cpu::run`]) until the context reports
    /// that the call is done.
    #[inline]
    fn run(
        &mut self,
        target: Option<u64>,
        frame: &mut Frame,
        audio: &mut [StereoSample],
    ) -> RunResult {
        let Hw { cpu, sys, stop_on } = &mut self.hw;
        let start = sys.hot.clock();
        if target.is_some_and(|t| start >= t) {
            return RunResult {
                events: Events::EMPTY,
                clocks: 0,
                audio_samples: 0,
            };
        }
        let stop = Events::ALWAYS_STOP | *stop_on;
        let mut ctx = Ctx::new(
            self.mem,
            sys,
            frame,
            audio,
            stop,
            target.unwrap_or(u64::MAX),
        );
        // A single step: done before it starts, so `run` stops after one.
        ctx.done = target.is_none();
        cpu.run(&mut ctx);
        RunResult {
            events: ctx.events,
            clocks: ctx.clock() - start,
            audio_samples: ctx.audio_len,
        }
    }

    // --- Debug access: plain field reads, nothing on the hot path. ---

    /// Base clocks since the last load or reset.
    #[inline]
    pub fn clock(&self) -> u64 {
        self.hw.sys.hot.clock()
    }

    /// The CPU registers.
    #[inline]
    pub fn registers(&self) -> Registers {
        self.hw.cpu.regs
    }

    /// The last byte sent on the serial port (see [`Events::SERIAL_TX`]).
    #[inline]
    pub fn serial_byte(&self) -> u8 {
        self.hw.sys.serial.last_byte
    }

    /// Whether the LCD is on (LCDC bit 7). While it is off, no frames
    /// complete and the screen shows white.
    #[inline]
    pub fn lcd_enabled(&self) -> bool {
        self.hw.sys.ppu.lcd_enabled()
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
        self.hw.sys.ppu.screen_blank()
    }

    /// Whether the CPU has locked up on an invalid opcode.
    #[inline]
    pub fn cpu_locked(&self) -> bool {
        self.hw.cpu.locked()
    }

    // --- Cartridge state for the caller to persist. ---

    /// The cartridge RAM, for saving while the emulator holds the borrow:
    /// exactly [`cart_ram_size`]`(rom)` bytes (the header size), whatever
    /// the length of the buffer passed to [`Emulator::load`]. Empty if no
    /// cartridge is inserted.
    #[inline]
    pub fn cart_ram(&self) -> &[u8] {
        self.hw.sys.cart.ram()
    }

    /// The MBC3 RTC state, or `None` if the cartridge has no RTC.
    pub fn rtc_state(&self) -> Option<RtcState> {
        self.hw.sys.cart.has_rtc.then_some(self.hw.sys.cart.rtc)
    }

    /// Restores the MBC3 RTC state (e.g. from a save, advanced to the
    /// current wall-clock time). Ignored if the cartridge has no RTC.
    ///
    /// The registers are masked to their hardware widths and
    /// `subsecond_clocks` is clamped below one second (see [`RtcTime`] and
    /// [`RtcState`]), so `rtc_state()` afterwards may differ from `state`.
    pub fn set_rtc_state(&mut self, state: RtcState) {
        if self.hw.sys.cart.has_rtc {
            self.hw.sys.cart.rtc = state.masked();
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
    /// The ROM is NOPs, with XOR E at 0100 and JP HL at the end (HL is
    /// 007C after boot in DMG compatibility mode), so every instruction is
    /// one M-cycle and runs end exactly at their target or event.
    fn emulator() -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x100] = 0xAB;
        rom[0x7FFF] = 0xE9;
        let ram = vec![0u8; cart_ram_size(rom).unwrap()].leak();
        let emu = fresh();
        emu.load(rom, ram, Config::default()).unwrap();
        emu
    }

    /// A powered-off emulator with its own heap `Memory`, as a desktop
    /// front end builds it. Leaks both, which keeps lifetimes trivial.
    pub(crate) fn fresh() -> &'static mut Emulator<'static> {
        Box::leak(Box::new(Emulator::new(Box::leak(Box::new(Memory::new())))))
    }

    /// A CPU write to an I/O register, through the bus (no M-cycle).
    fn write_lcdc(emu: &mut Emulator, value: u8) {
        let mut fb = frame();
        let mut ctx = Ctx::new(
            emu.mem,
            &mut emu.hw.sys,
            &mut fb,
            &mut [],
            Events::EMPTY,
            u64::MAX,
        );
        ctx.write_mapped(0xFF40, value);
    }

    fn frame() -> Box<Frame> {
        vec![0u16; SCREEN_WIDTH * SCREEN_HEIGHT]
            .into_boxed_slice()
            .try_into()
            .unwrap()
    }

    #[test]
    fn emulator_is_small_enough_for_the_stack() {
        // Only `Memory` is big (see the `Hw` docs for the limit).
        assert!(size_of::<Hw<'_>>() < 1024, "{}", size_of::<Hw<'_>>());
        assert!(size_of::<Emulator<'_>>() < 1024 + 16);
        assert!(size_of::<Memory>() > 48 * 1024);
    }

    #[test]
    fn new_clears_memory_it_is_given() {
        // A `Memory` reused from another emulator gives the same state as a
        // fresh one.
        let a = fresh();
        let mem = Box::leak(Box::new(Memory::new()));
        mem.wram.fill(0x5A);
        mem.vram.fill(0xA5);
        mem.io.fill(4);
        let b = Emulator::new(mem);
        assert!(*a.mem == *b.mem);
        static NEW: Memory = Memory::new();
        assert!(*b.mem == NEW);
        assert_eq!(a.registers(), b.registers());
        assert_eq!(a.clock(), b.clock());
        assert_eq!(a.lcd_enabled(), b.lcd_enabled());
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
    fn memory_new_equals_cleared_state() {
        // `Memory::new()` is all zeros; that must equal what
        // `Memory::clear` (used by `new`, `reset` and `eject`) produces
        // after the RAM has been dirtied. `reset` then writes the post-boot
        // values of the stub I/O registers.
        let fresh = fresh();
        let emu = emulator();
        emu.mem.wram.fill(0x5A);
        emu.mem.vram.fill(0xA5);
        emu.mem.oam.fill(1);
        emu.mem.hram.fill(2);
        emu.mem.extra_oam.fill(3);
        emu.mem.io.fill(4);
        emu.mem.clear();
        assert!(*emu.mem == *fresh.mem);
        emu.mem.wram.fill(0x5A);
        emu.reset();
        assert!(emu.mem.wram == fresh.mem.wram);
        assert_eq!(emu.peek(0xFF47), 0xFC); // BGP after boot
        static NEW: Memory = Memory::new();
        assert!(*fresh.mem == NEW);
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
        let emu = fresh();
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
        let emu = fresh();
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
        let emu = fresh();
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
        assert_eq!(emu.hw.sys.config.dmg_palettes, p);
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
        write_lcdc(emu, 0x11); // LCD off
        assert!(emu.screen_blank());
        let r = emu.run_cycles(u64::from(FRAME_CLOCKS) * 2, &mut fb, &mut []);
        assert!(r.events.is_empty());
        write_lcdc(emu, 0x91); // back on: still blank
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
        write_lcdc(emu, 0x93);
        assert!(!emu.screen_blank());
        // Off and on again with no frame in between: blank, through the
        // first frame after.
        write_lcdc(emu, 0x13);
        write_lcdc(emu, 0x93);
        assert!(emu.screen_blank());
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(emu.screen_blank());
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(!emu.screen_blank());
    }

    #[test]
    fn lcd_on_from_power_off_skips_the_first_frame_too() {
        // The LCD was never on since `new`: turning it on still hides the
        // first frame.
        let emu = fresh();
        let mut fb = frame();
        assert!(emu.screen_blank());
        write_lcdc(emu, 0x91);
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
        let emu = fresh();
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
        let emu = fresh();
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

    /// Runs `program` (CGB mode) two ways, one `step` at a time and by
    /// `run_until` calls with odd targets, and checks that every event
    /// happens at the same clock and the end state is the same: the clock
    /// countdown (ADR 0008, amendments) must be invisible.
    fn run_calls_match_single_steps(program: &[u8], until: u64, expect: Events) {
        let stops = Events::SERIAL_TX | Events::LD_B_B;
        let a = with_program(program, 0x80);
        let b = with_program(program, 0x80);
        a.set_stop_events(stops);
        b.set_stop_events(stops);
        let mut fb = frame();
        let mut steps = std::vec::Vec::new();
        while a.clock() < until {
            let start = a.clock();
            let r = a.step(&mut fb, &mut []);
            assert_eq!(r.clocks, a.clock() - start);
            if !r.events.is_empty() {
                steps.push((a.clock(), r.events));
            }
        }
        let mut runs = std::vec::Vec::new();
        let mut target = 1;
        while b.clock() < until {
            target = (target + 997).min(until);
            let start = b.clock();
            let r = b.run_until(target, &mut fb, &mut []);
            assert_eq!(r.clocks, b.clock() - start);
            if r.events.is_empty() {
                assert!(b.clock() >= target);
            } else {
                runs.push((b.clock(), r.events));
            }
        }
        let seen = steps.iter().fold(Events::EMPTY, |acc, &(_, e)| acc | e);
        assert!(seen.contains(expect), "{seen:?}");
        assert_eq!(steps, runs);
        assert_eq!(a.clock(), b.clock());
        assert_eq!(a.registers(), b.registers());
        for reg in [0xFF02, 0xFF04, 0xFF05, 0xFF0F, 0xFF41, 0xFF44, 0xFF4D] {
            assert_eq!(a.peek(reg), b.peek(reg), "{reg:#06x}");
        }
    }

    #[test]
    fn run_calls_match_single_steps_with_timer_serial_and_speed_switch() {
        run_calls_match_single_steps(
            &[
                0x3E, 0x05, 0xE0, 0x07, // LD A,05; LDH (07),A: TIMA at 262144 Hz
                0x3E, 0xFF, 0xE0, 0xFF, // LD A,FF; LDH (FF),A: IE (IME stays off)
                0x3E, 0x01, 0xE0, 0x4D, // LD A,01; LDH (4D),A: arm the speed switch
                0x10, 0x00, // STOP: double speed
                0x3E, b'Z', 0xE0, 0x01, // LD A,'Z'; LDH (01),A
                0x3E, 0x83, 0xE0, 0x02, // LD A,83; LDH (02),A: fast serial
                0x04, 0x40, // INC B; LD B,B
                0xF0, 0x05, 0xF0, 0x44, // LDH A,(05); LDH A,(44)
                0x18, 0xF8, // JR -8 (back to INC B)
            ],
            3 * u64::from(FRAME_CLOCKS) + 12_345,
            Events::FRAME_READY | Events::SERIAL_TX | Events::LD_B_B,
        );
    }

    #[test]
    fn run_calls_match_single_steps_with_halt_and_lcd_writes() {
        run_calls_match_single_steps(
            &[
                0x3E, 0x04, 0xE0, 0x07, // LD A,04; LDH (07),A: TIMA at 4096 Hz
                0x3E, 0x05, 0xE0, 0xFF, // LD A,05; LDH (FF),A: IE = timer, VBlank
                0x76, 0x00, // HALT; NOP
                0xAF, 0xE0, 0x0F, // XOR A; LDH (0F),A
                0x3E, 0x11, 0xE0, 0x40, // LD A,11; LDH (40),A: LCD off
                0x40, // LD B,B
                0x3E, 0x91, 0xE0, 0x40, // LD A,91; LDH (40),A: LCD on
                0x18, 0xF0, // JR -16 (back to HALT)
            ],
            5 * u64::from(FRAME_CLOCKS) + 777,
            Events::FRAME_READY | Events::LD_B_B,
        );
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
        let emu = fresh();
        emu.load(rom, ram, Config::default()).unwrap();
        let mut fb = frame();
        let _ = emu.run_cycles(1000, &mut fb, &mut []);
        emu.reset();
        assert_eq!(emu.clock(), 0);
        assert_eq!(emu.cart_ram()[0], 0x5A);
        assert_eq!(emu.peek(0xA000), 0x5A);
    }

    /// Loads `program` at 0100 of an otherwise empty 32 KiB ROM.
    fn with_program(program: &[u8], cgb_flag: u8) -> &'static mut Emulator<'static> {
        let rom = vec![0u8; 0x8000].leak();
        rom[0x100..0x100 + program.len()].copy_from_slice(program);
        rom[0x143] = cgb_flag;
        let emu = fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        emu
    }

    #[test]
    fn post_boot_registers_cgb_and_dmg_compatibility() {
        let emu = with_program(&[], 0x80);
        let r = emu.registers();
        assert_eq!(
            (r.a, r.f, r.b, r.c, r.d, r.e, r.h, r.l, r.sp, r.pc),
            (
                0x11, 0x80, 0x00, 0x00, 0xFF, 0x56, 0x00, 0x0D, 0xFFFE, 0x0100
            )
        );
        // DMG compatibility, non-Nintendo licensee: B = 0, HL = 007C.
        let emu = with_program(&[], 0x00);
        let r = emu.registers();
        assert_eq!(
            (r.a, r.f, r.b, r.c, r.d, r.e, r.h, r.l),
            (0x11, 0x80, 0x00, 0x00, 0x00, 0x08, 0x00, 0x7C)
        );
        // Nintendo licensee (old code 01): B = title checksum.
        let rom = vec![0u8; 0x8000].leak();
        rom[0x134..0x13A].copy_from_slice(b"TETRIS");
        rom[0x14B] = 0x01;
        let emu = fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        let sum = b"TETRIS".iter().fold(0u8, |s, &x| s.wrapping_add(x));
        assert_eq!(emu.registers().b, sum);
        // New licensee "01" with old code 33; a sum of 0x58 gives HL = 991A.
        let rom = vec![0u8; 0x8000].leak();
        rom[0x134] = 0x58;
        rom[0x14B] = 0x33;
        rom[0x144..0x146].copy_from_slice(b"01");
        let emu = fresh();
        emu.load(rom, &mut [], Config::default()).unwrap();
        let r = emu.registers();
        assert_eq!((r.b, r.h, r.l), (0x58, 0x99, 0x1A));
    }

    #[test]
    fn serial_transfer_reports_the_byte() {
        // LD A,'X'; LDH (01),A; LD A,81; LDH (02),A; JR -2
        let emu = with_program(
            &[0x3E, b'X', 0xE0, 0x01, 0x3E, 0x81, 0xE0, 0x02, 0x18, 0xFE],
            0,
        );
        emu.set_stop_events(Events::SERIAL_TX);
        let mut fb = frame();
        let mut r = emu.run_cycles(20_000, &mut fb, &mut []);
        while r.events.contains(Events::FRAME_READY) {
            r = emu.run_cycles(20_000, &mut fb, &mut []);
        }
        assert_eq!(r.events, Events::SERIAL_TX);
        assert_eq!(emu.serial_byte(), b'X');
        // 8 bits at 8192 Hz: 8 x 512 clocks, plus up to one bit period of
        // phase (the bit clock comes from the timer's counter).
        assert!(
            (4096..4096 + 512 + 64).contains(&emu.clock()),
            "{}",
            emu.clock()
        );
        assert_eq!(emu.peek(0xFF02) & 0x80, 0);
        assert_eq!(emu.peek(0xFF0F) & 0x08, 0x08);
    }

    #[test]
    fn ld_b_b_stops_only_when_enabled() {
        let emu = with_program(&[0x00, 0x40, 0x18, 0xFE], 0);
        let mut fb = frame();
        let r = emu.run_cycles(100, &mut fb, &mut []);
        assert!(r.events.contains(Events::LD_B_B));
        assert!(emu.clock() >= 100);
        let emu = with_program(&[0x00, 0x40, 0x18, 0xFE], 0);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(100, &mut fb, &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        assert_eq!((r.clocks, emu.registers().pc), (8, 0x102));
    }

    #[test]
    fn invalid_opcode_locks_up_but_time_goes_on() {
        let emu = with_program(&[0x00, 0xDD], 0);
        let mut fb = frame();
        let r = emu.run_cycles(1000, &mut fb, &mut []);
        assert_eq!(r.events, Events::CPU_LOCKED);
        assert_eq!(r.clocks, 8);
        assert!(emu.cpu_locked());
        // Time and the PPU keep running; the event doesn't repeat.
        let r = emu.run_frame(&mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(emu.registers().pc, 0x102);
    }

    #[test]
    fn ly_follows_the_line_timing() {
        let emu = emulator();
        let mut fb = frame();
        assert_eq!(emu.peek(0xFF44), 0);
        let _ = emu.run_cycles(456 * 3 + 8, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF44), 3);
        let _ = emu.run_frame(&mut fb, &mut []); // to VBlank
        assert_eq!(emu.peek(0xFF44), 144);
        assert_eq!(emu.peek(0xFF41) & 0x03, 1);
        assert_eq!(emu.peek(0xFF0F) & 0x01, 0x01);
    }

    #[test]
    fn joypad_interrupt_ends_halt() {
        // LD A,10; LDH (FF),A (IE = joypad); XOR A; LDH (0F),A; LD A,10;
        // LDH (00),A (select the action buttons); HALT; LD B,B; JR -2.
        let emu = with_program(
            &[
                0x3E, 0x10, 0xE0, 0xFF, 0xAF, 0xE0, 0x0F, 0x3E, 0x10, 0xE0, 0x00, 0x76, 0x40, 0x18,
                0xFE,
            ],
            0x80,
        );
        emu.set_stop_events(Events::LD_B_B);
        let mut fb = frame();
        let r = emu.run_cycles(10_000, &mut fb, &mut []);
        assert!(r.events.is_empty());
        assert!(emu.hw.cpu.halted());
        // A d-pad button isn't selected: no interrupt, still halted.
        emu.set_buttons(Buttons::DOWN);
        let r = emu.run_cycles(1000, &mut fb, &mut []);
        assert!(r.events.is_empty() && emu.hw.cpu.halted());
        // IE & IF bit 4 ends HALT; IME is off, so execution continues
        // after it.
        emu.set_buttons(Buttons::DOWN | Buttons::START);
        assert_eq!(emu.peek(0xFF0F) & 0x10, 0x10);
        let r = emu.run_cycles(1000, &mut fb, &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        assert_eq!(emu.registers().pc, 0x10D);
    }
}
