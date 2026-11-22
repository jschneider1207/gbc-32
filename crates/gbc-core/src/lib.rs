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
//! # Saves
//!
//! The caller owns persistence (ADR 0015). [`cart_info`]`(rom)` says,
//! before loading, whether there is anything to save
//! ([`CartInfo::has_save`]: a battery-backed cartridge with RAM or an
//! RTC); cartridges without a battery get no save file. The save is
//! [`Emulator::cart_ram`] plus, for MBC3+RTC carts,
//! [`Emulator::rtc_state`] (stored with the wall-clock time).
//! [`Emulator::save_dirty`] says when the game changed it, so a front end
//! (or the MCU, writing to flash) saves only then and calls
//! [`Emulator::clear_save_dirty`]. When the cartridge leaves,
//! [`Emulator::eject`] (or a `load` over it) returns an [`EjectedCart`]
//! with the buffers, the final RTC state and the dirty flag. On the next
//! run, [`Emulator::load`] first, then
//! `emu.set_rtc_state(saved.advance_seconds(elapsed_secs))`: `load` resets
//! the clock, so setting it earlier is lost.
//!
//! ```
//! use gbc_core::{Config, Emulator, Memory, cart_info};
//!
//! # let mut rom = vec![0u8; 0x8000];
//! # rom[0x147] = 0x03; // MBC1+RAM+BATTERY
//! # rom[0x149] = 0x02; // 8 KiB
//! let info = cart_info(&rom).unwrap();
//! let mut cart_ram = vec![0u8; info.ram_size]; // or read the .sav file
//! let mut mem = Box::new(Memory::new());
//! let mut emu = Emulator::new(&mut mem);
//! emu.load(&rom, &mut cart_ram, Config::default()).unwrap();
//! // ... run frames; now and then (e.g. once a second):
//! if info.has_save() && emu.save_dirty() {
//!     let _save: &[u8] = emu.cart_ram(); // write it out, with rtc_state()
//!     emu.clear_save_dirty();
//! }
//! // On exit or a game switch:
//! let ejected = emu.eject().unwrap();
//! if info.has_save() {
//!     if ejected.save_dirty {
//!         let _ram: &[u8] = ejected.save_ram(); // write the RAM out
//!     }
//!     // The RTC keeps counting without setting the dirty flag, so write
//!     // it at exit whether or not the game saved.
//!     if let Some(_rtc) = ejected.rtc {
//!         // write it out, with the wall-clock time
//!     }
//! }
//! ```
//!
//! Dropping the result of `eject` (or of `load` over a cartridge) loses the
//! RTC state, the dirty flag and the RAM buffer, so it is `#[must_use]`:
//!
//! ```compile_fail
//! #![deny(unused_must_use)]
//! use gbc_core::{Emulator, Memory};
//! let mut mem = Box::new(Memory::new());
//! let mut emu = Emulator::new(&mut mem);
//! emu.eject();
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
//!     let _ = emu.load(rom, save_ram, Config::default()).unwrap();
//!     let (mut frame_no, mut last_write): (u32, Option<u32>) = (0, None);
//!     // Once per frame (the firmware's main loop):
//!     emu.set_buttons(Buttons::EMPTY); // read from the GPIOs
//!     let _ = emu.run_frame(frame, &mut []);
//!     frame_no += 1;
//!     // Games write a save over 1-2 s (dozens of writes), so debounce:
//!     // note the frame of each new write and flash only once it has been
//!     // quiet for a second.
//!     if emu.save_dirty() {
//!         emu.clear_save_dirty(); // the next write sets it again
//!         last_write = Some(frame_no);
//!     }
//!     if last_write.is_some_and(|f| frame_no - f >= 60) {
//!         last_write = None;
//!         let _ = (emu.cart_ram(), emu.rtc_state()); // to flash / eMMC
//!     }
//!     // At power-off or a game switch, `emu.eject()` (or the result of
//!     // `load`) hands back the ROM and `save_ram` for the next `load`,
//!     // with the final RTC state (`EjectedCart`): flash it if a write is
//!     // pending (`last_write`) or `rtc` is `Some`.
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
mod dma;
mod events;
mod hdma;
mod joypad;
mod memory;
mod ppu;
mod schedule;
mod serial;
mod timer;

pub use apu::StereoSample;
pub use cart::{
    CartBuffers, CartInfo, EjectedCart, LoadError, LoadErrorKind, MIN_ROM_SIZE, RtcState, RtcTime,
    cart_info, cart_ram_size,
};
pub use config::{Config, DmgPalettes, rgb555, rgb555_to_888, rgb888_to_555};
pub use cpu::Registers;
pub use events::Events;
pub use joypad::{Buttons, SCHEDULED_INPUTS};
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
/// must stay small: at most 864 B (832 now), checked by a test. The limit protects the
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
    /// Returns the previously inserted cartridge, if any, as
    /// [`Emulator::eject`] would (buffers, RTC state, dirty flag), so a
    /// game switch never loses them. Unlike `eject` followed by `load`,
    /// loading over a cartridge keeps the held buttons, stop events and
    /// palettes (as [`Emulator::reset`] does). The new cartridge starts
    /// with [`Emulator::save_dirty`] clear. On error the emulator is
    /// unchanged (the previous cartridge stays inserted) and the error
    /// hands back `rom` and `cart_ram`.
    #[must_use = "the returned cartridge holds the previous RTC state, dirty flag and RAM buffer"]
    pub fn load(
        &mut self,
        rom: &'a [u8],
        cart_ram: &'a mut [u8],
        config: Config,
    ) -> Result<Option<EjectedCart<'a>>, LoadError<'a>> {
        let cart = Cart::new(rom, cart_ram)?;
        let now = self.hw.sys.hot.clock();
        let previous = core::mem::replace(&mut self.hw.sys.cart, cart).into_ejected(now);
        self.hw.sys.config = config;
        // Not `reset`: the new cartridge's RTC must not count the old
        // cartridge's run time.
        self.power_cycle();
        Ok(previous)
    }

    /// Removes the cartridge and returns it: the ROM and the whole
    /// cartridge RAM buffer as passed to [`Emulator::load`] (not trimmed to
    /// the header size; [`EjectedCart::save_ram`] is the save file part),
    /// the RTC state at this moment, and the [`Emulator::save_dirty`]
    /// flag. Everything a save needs is in the result.
    ///
    /// Afterwards the emulator is in the state [`Emulator::new`] leaves it
    /// in: powered off, no cartridge, default settings (stop events, held
    /// buttons and palettes are cleared). Returns `None`, changing nothing,
    /// if no cartridge is inserted.
    #[must_use = "the ejected cartridge holds the RTC state, dirty flag and RAM buffer"]
    pub fn eject(&mut self) -> Option<EjectedCart<'a>> {
        if !self.hw.sys.cart.is_loaded() {
            return None;
        }
        self.mem.clear();
        let now = self.hw.sys.hot.clock();
        core::mem::replace(&mut self.hw, Hw::new())
            .sys
            .cart
            .into_ejected(now)
    }

    /// Changes the DMG compatibility palettes without a reset. Takes effect
    /// from the next pixel drawn (pixels are drawn lazily, so this can
    /// include the rest of a line the PPU was in the middle of).
    pub fn set_dmg_palettes(&mut self, palettes: DmgPalettes) {
        let sys = &mut self.hw.sys;
        sys.config.dmg_palettes = palettes;
        sys.ppu.resolve_palettes(self.mem, &palettes, sys.cgb_mode);
    }

    /// Power cycle: the post-boot state (no boot ROM is run; registers are
    /// set as the CGB boot ROM leaves them) and the clock back to 0.
    /// Cartridge RAM and the RTC keep their contents; the held buttons and
    /// stop events are kept. Without a cartridge this does nothing: the
    /// emulator stays powered off, in the state [`Emulator::new`] leaves it.
    pub fn reset(&mut self) {
        let sys = &mut self.hw.sys;
        // The RTC counts the time up to the reset; the clock restarts at 0.
        sys.cart.rtc_sync(sys.hot.clock());
        self.power_cycle();
    }

    /// [`Emulator::reset`] without first counting the elapsed time on the
    /// RTC.
    fn power_cycle(&mut self) {
        if !self.hw.sys.cart.is_loaded() {
            return;
        }
        self.mem.clear();
        let sys = &mut self.hw.sys;
        sys.reset(self.mem);
        self.hw.cpu.reset(sys.cart.rom, sys.cgb_mode);
    }

    /// Sets the buttons currently held. Pressing a button in a group the
    /// game has selected in P1 requests the joypad interrupt (Pan Docs:
    /// Interrupt Sources, "INT $60 - Joypad interrupt"), which also ends
    /// HALT if enabled in IE.
    ///
    /// The same as `set_buttons_at(self.clock(), buttons)`: changes
    /// already scheduled with [`Emulator::set_buttons_at`] stay pending.
    #[inline]
    pub fn set_buttons(&mut self, buttons: Buttons) {
        let sys = &mut self.hw.sys;
        sys.set_joypad(buttons, sys.joyp_select);
    }

    /// Schedules the held buttons to become `buttons` at emulated time
    /// `clock` (base clocks, like [`Emulator::clock`]), during a later run
    /// call (ADR 0012).
    ///
    /// The time is rounded down to an M-cycle: the change lands at the
    /// start of the M-cycle that `clock` falls in (at the speed in effect
    /// then). CPU accesses from that boundary on (P1 reads, the IF read,
    /// HALT's and STOP's wake-up checks) see it, earlier ones don't, and
    /// it requests the joypad interrupt there as [`Emulator::set_buttons`]
    /// would. This is independent of where run calls end.
    ///
    /// - If that boundary is not after [`Emulator::clock`] (`clock` is in
    ///   the past or the current M-cycle), the change applies now, exactly
    ///   like `set_buttons`.
    /// - A speed switch between the call and `clock` moves the M-cycle
    ///   grid; the change still lands on the boundary at or before `clock`
    ///   on the new grid. That includes a boundary inside the switch's
    ///   pause: the change lands there, during the pause, and requests the
    ///   joypad interrupt there. Only if that boundary is already past when
    ///   the grid changes (before the switch's clock) does it land at the
    ///   first boundary after the switch.
    /// - Up to [`SCHEDULED_INPUTS`] changes can be pending; they apply in
    ///   clock order. Scheduling at a clock that already has a pending
    ///   change replaces it.
    /// - Changes landing on the same M-cycle boundary apply as one (the
    ///   last of them), so a press and release inside one M-cycle request
    ///   no interrupt. This is a modelling choice, not a measured hardware
    ///   behaviour: SameBoy (`GB_set_key_state`, `GB_update_joyp`) sets IF
    ///   on every falling edge of the P1 lines as the key state changes.
    /// - Returns `false`, changing nothing, if the queue is full and
    ///   `clock` is in the future and not already pending, or if `clock`
    ///   is `u64::MAX` (reserved: it marks empty queue slots and would
    ///   never be reached).
    /// - `load`, `reset` and `eject` drop pending changes (the clock
    ///   restarts at 0).
    #[must_use = "a full queue drops the change"]
    pub fn set_buttons_at(&mut self, clock: u64, buttons: Buttons) -> bool {
        let sys = &mut self.hw.sys;
        // Between run calls the clock is on an M-cycle boundary.
        let mcycle = sys.hot.mcycle as u64;
        if clock < sys.hot.clock() + mcycle {
            sys.set_joypad(buttons, sys.joyp_select);
            return true;
        }
        // `NEVER` marks empty queue slots: a change there would never fire.
        if clock == schedule::NEVER {
            return false;
        }
        if !sys.inputs.push(clock, buttons) {
            return false;
        }
        sys.schedule_inputs(mcycle);
        true
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
    /// while halted, stopped or locked up). The step that leaves HALT also
    /// includes the CGB wake-up M-cycle before the next instruction or
    /// dispatch (`docs/reference/halt.md`). The frame buffer rules of
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
        // A single step's target is one clock ahead, so a halted CPU's
        // skip to the next deadline (`Bus::idle_halted`) stops after one
        // M-cycle.
        let end = target.unwrap_or(start + 1);
        let mut ctx = Ctx::new(self.mem, sys, frame, audio, stop, end);
        let single = target.is_none();
        // The call is over: a single step, a stop event or the target.
        let finished = |ctx: &Ctx| single || ctx.events.intersects(stop) || ctx.clock() >= end;
        // A VRAM DMA step (ADR 0017) runs between instructions, where the
        // next one would fetch its opcode: a pending one ends `cpu.run`
        // (`Ctx::done`) after the current instruction. Steps one at a time
        // while it waits for a dispatch or a wake-up.
        loop {
            if ctx.sys.vdma.pending && cpu.fetches_next(&ctx) {
                ctx.vram_dma();
                if !single && finished(&ctx) {
                    break;
                }
            }
            // A single step: done before it starts, so `run` stops after one.
            ctx.done = finished(&ctx) || ctx.sys.vdma.pending;
            cpu.run(&mut ctx);
            if finished(&ctx) {
                break;
            }
        }
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
    ///
    /// The bytes are what the CPU reads at A000-BFFF. For MBC2's 512x4-bit
    /// RAM that is one byte per nibble with the upper four bits set
    /// (`0xF0 | value`; Pan Docs: MBC2), as SameBoy stores it. A loaded
    /// save is normalized to that form (upper bits forced to 1), so saves
    /// with the upper bits clear also load.
    #[inline]
    pub fn cart_ram(&self) -> &[u8] {
        self.hw.sys.cart.ram()
    }

    /// The inserted cartridge's [`CartInfo`] (battery, RTC, RAM size), or
    /// `None` if there is none.
    pub fn cart_info(&self) -> Option<CartInfo> {
        let cart = &self.hw.sys.cart;
        if cart.is_loaded() {
            cart_info(cart.rom).ok()
        } else {
            None
        }
    }

    /// Whether the game changed its save since the cartridge was loaded or
    /// [`Emulator::clear_save_dirty`] was last called: a cartridge RAM write
    /// that changed a byte, or a write to an RTC register. The RTC counting
    /// on its own, RTC latches and [`Emulator::set_rtc_state`] don't set it
    /// (save the RTC state whenever the RAM is saved, and at exit). It is
    /// set whatever the battery, so check [`CartInfo::has_save`] too.
    ///
    /// Lets the MCU write flash only when there is something new. A front
    /// end usually waits until it has been set for a while (games write a
    /// save over several frames) before saving: clear the flag on each
    /// sighting, remember the frame, and save once no new write came for a
    /// while.
    #[inline]
    pub fn save_dirty(&self) -> bool {
        self.hw.sys.cart.save_dirty
    }

    /// Clears [`Emulator::save_dirty`]. Call it only once the data is
    /// captured (copied out or written). If an asynchronous flash write
    /// later fails, the caller must keep its own pending flag: there is no
    /// way to set this one again.
    #[inline]
    pub fn clear_save_dirty(&mut self) {
        self.hw.sys.cart.save_dirty = false;
    }

    /// The MBC3 RTC state at [`Emulator::clock`], or `None` if the
    /// cartridge has no RTC.
    ///
    /// Save this with the cart RAM; restore with [`Emulator::set_rtc_state`].
    pub fn rtc_state(&self) -> Option<RtcState> {
        let sys = &self.hw.sys;
        sys.cart.has_rtc().then(|| sys.cart.rtc_at(sys.hot.clock()))
    }

    /// Restores the MBC3 RTC state (e.g. from a save, advanced to the
    /// current wall-clock time). Ignored if the cartridge has no RTC.
    ///
    /// To sync to wall-clock time, call this after [`Emulator::load`]:
    /// `emu.set_rtc_state(saved.advance_seconds(elapsed))` (see
    /// [`RtcState::advance_seconds`]). Loading first matters: `load`
    /// starts the clock from zero, so a state set before it is lost.
    ///
    /// The registers are masked to their hardware widths and
    /// `subsecond_clocks` is clamped below one second (see [`RtcTime`] and
    /// [`RtcState`]), so `rtc_state()` afterwards may differ from `state`.
    pub fn set_rtc_state(&mut self, state: RtcState) {
        let sys = &mut self.hw.sys;
        if sys.cart.has_rtc() {
            sys.cart.set_rtc(state.masked(), sys.hot.clock());
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

    /// A CPU write to LCDC, through the bus (no M-cycle).
    fn write_lcdc(emu: &mut Emulator, value: u8) {
        write_reg(emu, 0xFF40, value);
    }

    /// A CPU write, through the bus (no M-cycle).
    fn write_reg(emu: &mut Emulator, addr: u16, value: u8) {
        let mut fb = frame();
        let mut ctx = Ctx::new(
            emu.mem,
            &mut emu.hw.sys,
            &mut fb,
            &mut [],
            Events::EMPTY,
            u64::MAX,
        );
        ctx.write_mapped(addr, value);
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
        assert!(size_of::<Hw<'_>>() <= 864, "{}", size_of::<Hw<'_>>());
        assert!(size_of::<Emulator<'_>>() < 864 + 16);
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
        rom[0x147] = 0x08; // ROM+RAM
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
    fn reset_without_cartridge_does_nothing() {
        let emu = fresh();
        emu.reset();
        assert_eq!(emu.clock(), 0);
        assert!(!emu.lcd_enabled());
        assert_eq!(emu.peek(0x0100), 0xFF);
        assert!(emu.eject().is_none());
    }

    #[test]
    fn eject_returns_buffers_and_allows_reload() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x100] = 0xAB;
        rom[0x147] = 0x08; // ROM+RAM
        rom[0x149] = 0x02; // 8 KiB
        let ram = vec![0u8; 0x8000].leak(); // larger than needed
        ram[0] = 0x5A;
        let emu = fresh();
        assert!(emu.eject().is_none());
        assert!(emu.load(rom, ram, Config::default()).unwrap().is_none());
        let mut fb = frame();
        let _ = emu.run_cycles(1000, &mut fb, &mut []);

        let ej = emu.eject().unwrap();
        assert_eq!(ej.save_ram().len(), 0x2000);
        let (rom, ram) = ej.into_buffers();
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
        let (old_rom, old_ram) = emu
            .load(rom2, &mut [], Config::default())
            .unwrap()
            .unwrap()
            .into_buffers();
        assert_eq!(old_rom[0x100], 0xAB);
        assert_eq!(old_ram.len(), 0x8000);
        assert_eq!(emu.peek(0x0100), 0x00);
    }

    /// An MBC3+TIMER+RAM+BATTERY ROM with 8 KiB of RAM.
    fn rtc_rom() -> &'static mut [u8] {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x147] = 0x10;
        rom[0x149] = 0x02;
        rom
    }

    /// Runs whole run calls until at least `clocks` from now.
    fn run_for(emu: &mut Emulator, clocks: u64) {
        let mut fb = frame();
        let target = emu.clock() + clocks;
        while emu.clock() < target {
            let _ = emu.run_until(target, &mut fb, &mut []);
        }
    }

    #[test]
    fn eject_and_load_over_return_the_rtc_state() {
        let sec = u64::from(CLOCK_HZ);
        let emu = fresh();
        emu.load(rtc_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap();
        let mut start = RtcState::default();
        start.live.minutes = 5;
        start.live.seconds = 10;
        emu.set_rtc_state(start);
        run_for(emu, 2 * sec);
        // Loading over the RTC cart hands back its clock, caught up.
        let ej = emu
            .load(vec![0u8; MIN_ROM_SIZE].leak(), &mut [], Config::default())
            .unwrap()
            .unwrap();
        let rtc = ej.rtc.unwrap();
        assert_eq!((rtc.live.minutes, rtc.live.seconds), (5, 12));
        assert!(!ej.save_dirty); // set_rtc_state and ticking don't dirty
        let (rom, ram) = ej.into_buffers();

        // Eject: the state at the moment of removal.
        emu.load(rom, ram, Config::default()).unwrap();
        emu.set_rtc_state(rtc);
        run_for(emu, sec);
        let before = emu.rtc_state();
        let ej = emu.eject().unwrap();
        assert_eq!(ej.rtc, before);
        assert_eq!(ej.rtc.unwrap().live.seconds, 13);
        // Carts without an RTC eject `None`.
        emu.load(vec![0u8; MIN_ROM_SIZE].leak(), &mut [], Config::default())
            .unwrap();
        assert_eq!(emu.eject().unwrap().rtc, None);
    }

    #[test]
    fn save_dirty_tracks_game_writes_until_cleared() {
        let emu = fresh();
        emu.load(rtc_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap();
        assert!(!emu.save_dirty());
        write_reg(emu, 0x0000, 0x0A); // RAM/RTC enable
        write_reg(emu, 0xA000, 0x00); // same value: no change
        assert!(!emu.save_dirty());
        write_reg(emu, 0xA000, 0x33);
        assert!(emu.save_dirty());
        emu.clear_save_dirty();
        assert!(!emu.save_dirty());
        // An RTC register write is a save change too.
        write_reg(emu, 0x4000, 0x08);
        write_reg(emu, 0xA000, 0x20);
        assert!(emu.save_dirty());
        // A reset keeps the flag (the save is still unsaved)...
        emu.reset();
        assert!(emu.save_dirty());
        // ...and it is handed back with the cartridge; the next one starts
        // clean.
        let ej = emu
            .load(rtc_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap()
            .unwrap();
        assert!(ej.save_dirty);
        assert_eq!(ej.save_ram()[0], 0x33);
        assert!(!emu.save_dirty());
    }

    #[test]
    fn cart_info_of_the_inserted_cartridge() {
        let emu = fresh();
        assert_eq!(emu.cart_info(), None);
        emu.load(rtc_rom(), vec![0u8; 0x2000].leak(), Config::default())
            .unwrap();
        let info = emu.cart_info().unwrap();
        assert_eq!(info, cart_info(rtc_rom()).unwrap());
        assert!(info.battery && info.rtc && info.has_save());
        assert_eq!(info.ram_size, 0x2000);
        let _ = emu.eject();
        assert_eq!(emu.cart_info(), None);
    }

    #[test]
    fn cart_ram_is_trimmed_to_header_size() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x147] = 0x08; // ROM+RAM
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

    /// The compatibility colors are resolved through BGP at `load` (reset)
    /// and again by `set_dmg_palettes`: the blank BG (color 0, BGP FC:
    /// shade 0) shows the BG palette's shade 0.
    #[test]
    fn frames_use_the_compatibility_palettes() {
        let rom = vec![0u8; 0x8000].leak();
        let emu = fresh();
        let mut config = Config::default();
        config.dmg_palettes.bg = [1, 2, 3, 4];
        emu.load(rom, &mut [], config).unwrap();
        let mut fb = frame();
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(fb.iter().all(|&p| p == 1));
        emu.set_dmg_palettes(DmgPalettes {
            bg: [5, 6, 7, 8],
            ..DmgPalettes::GRAYSCALE
        });
        let _ = emu.run_frame(&mut fb, &mut []);
        assert!(fb.iter().all(|&p| p == 5));
    }

    /// The first `FRAME_READY` after reset: the next frame's VBlank (line
    /// 144, dot 5), from the post-boot position, on the M-cycle grid.
    const VBLANK: u64 =
        ((FRAME_CLOCKS - ppu::POST_BOOT_POS + ppu::VBLANK_POS) as u64).next_multiple_of(4);

    /// Clocks from an LCD enable to its first `FRAME_READY`: the frame
    /// starts 7 dots before the enabling write (`ppu::ON_POS`).
    const VBLANK_AFTER_ON: u64 = ((ppu::VBLANK_POS - ppu::ON_POS) as u64).next_multiple_of(4);

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
        assert_eq!(emu.clock() - start, VBLANK_AFTER_ON);
        assert!(emu.screen_blank());
        // The second frame is displayed.
        let r = emu.run_cycles(100_000, &mut fb, &mut []);
        assert_eq!(r.events, Events::FRAME_READY);
        assert_eq!(
            emu.clock() - start,
            VBLANK_AFTER_ON + u64::from(FRAME_CLOCKS)
        );
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
    fn rtc_state_counts_emulated_time_across_reset_not_load() {
        let rtc_rom = || {
            let rom = vec![0u8; 0x8000].leak();
            rom[0x147] = 0x0F; // MBC3+TIMER+BATTERY
            &*rom
        };
        let hz = u64::from(CLOCK_HZ);
        let emu = fresh();
        emu.load(rtc_rom(), &mut [], Config::default()).unwrap();
        let mut fb = frame();
        while emu.clock() < 3 * hz + hz / 2 {
            let _ = emu.run_until(3 * hz + hz / 2, &mut fb, &mut []);
        }
        let s = emu.rtc_state().unwrap();
        assert_eq!(s.live.seconds, 3);
        assert_eq!(u64::from(s.subsecond_clocks), emu.clock() - 3 * hz);
        // Counting resumes from the state set, at the clock it was set.
        emu.set_rtc_state(RtcState {
            live: RtcTime {
                seconds: 59,
                minutes: 59,
                hours: 23,
                ..RtcTime::default()
            },
            ..RtcState::default()
        });
        let set_at = emu.clock();
        while emu.clock() < set_at + hz {
            let _ = emu.run_until(set_at + hz, &mut fb, &mut []);
        }
        let s = emu.rtc_state().unwrap();
        assert_eq!((s.live.days, s.live.hours, s.live.seconds), (1, 0, 0));
        assert_eq!(u64::from(s.subsecond_clocks), emu.clock() - set_at - hz);
        // A reset restarts the clock at 0 but keeps the time so far.
        emu.reset();
        assert_eq!(emu.clock(), 0);
        assert_eq!(emu.rtc_state(), Some(s));
        // Loading another cartridge does not count the old one's time.
        let _ = emu.run_cycles(hz, &mut fb, &mut []);
        emu.load(rtc_rom(), &mut [], Config::default()).unwrap();
        assert_eq!(emu.rtc_state(), Some(RtcState::default()));
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
    fn run_calls_match_single_steps_through_speed_switch_halts() {
        run_calls_match_single_steps(
            &[
                0x3E, 0x05, 0xE0, 0x07, // LD A,05; LDH (07),A: TIMA at 262144 Hz
                0x3E, 0x04, 0xE0, 0xFF, // LD A,04; LDH (FF),A: IE = timer
                0x3E, 0x01, 0xE0, 0x4D, // LD A,01; LDH (4D),A: arm the speed switch
                0x10, 0x00, // STOP 00: double speed, a halt the timer IRQ ends
                0xAF, 0xE0, 0x0F, 0xE0, 0xFF, // XOR A; LDH (0F),A; LDH (FF),A
                0x3E, 0x01, 0xE0, 0x4D, // LD A,01; LDH (4D),A
                0x10, 0x00, // STOP 00: normal speed, the whole halt
                0x04, 0x40, // INC B; LD B,B
                0x18, 0xFC, // JR -4 (back to INC B)
            ],
            4 * u64::from(FRAME_CLOCKS) + 4321,
            Events::FRAME_READY | Events::LD_B_B,
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

    /// Forwards every bus call to the emulator's [`Ctx`], but idles a
    /// halted, stopped, locked or powered-off CPU one M-cycle at a time
    /// (the provided [`cpu::Bus::idle_halted`]): the reference the
    /// skip-to-deadline idling must match exactly (ADR 0011 step 5).
    struct PerMcycle<'c, 'r, 'a>(&'c mut Ctx<'r, 'a>);

    impl cpu::Bus for PerMcycle<'_, '_, '_> {
        fn read(&mut self, addr: u16) -> u8 {
            self.0.read(addr)
        }
        fn write(&mut self, addr: u16, value: u8) {
            self.0.write(addr, value);
        }
        fn idle(&mut self) {
            self.0.idle();
        }
        fn interrupt_enable(&self) -> u8 {
            self.0.interrupt_enable()
        }
        fn interrupt_flags(&self) -> u8 {
            self.0.interrupt_flags()
        }
        fn set_interrupt_flags(&mut self, value: u8) {
            self.0.set_interrupt_flags(value);
        }
        fn event(&mut self, event: Events) {
            self.0.event(event);
        }
        fn stop_begin(&mut self) -> cpu::StopKind {
            self.0.stop_begin()
        }
        fn speed_switch(&mut self, ime: bool, pending: bool) -> bool {
            cpu::Bus::speed_switch(self.0, ime, pending)
        }
        fn speed_switch_over(&self) -> bool {
            self.0.speed_switch_over()
        }
        fn speed_switch_end(&mut self) {
            self.0.speed_switch_end();
        }
        fn stop_wakeup(&self) -> bool {
            self.0.stop_wakeup()
        }
        fn halt_begin(&mut self) {
            self.0.halt_begin();
        }
        fn halt_end(&mut self) {
            self.0.halt_end();
        }
        fn run_done(&self) -> bool {
            self.0.run_done()
        }
    }

    /// `run_until(target)` as [`Emulator::run`] does it, but through
    /// [`PerMcycle`].
    fn run_until_per_mcycle(emu: &mut Emulator, target: u64, fb: &mut Frame) -> RunResult {
        let Hw { cpu, sys, stop_on } = &mut emu.hw;
        let start = sys.hot.clock();
        let stop = Events::ALWAYS_STOP | *stop_on;
        let mut ctx = Ctx::new(emu.mem, sys, fb, &mut [], stop, target);
        cpu.run(&mut PerMcycle(&mut ctx));
        RunResult {
            events: ctx.events,
            clocks: ctx.clock() - start,
            audio_samples: 0,
        }
    }

    /// Runs a HALT loop (`IE = ie`, `TAC = tac`, optionally in double
    /// speed) by `run_until` calls with odd targets, once with the
    /// skip-to-deadline idling and once one M-cycle at a time, and checks
    /// that every call ends on the same clock with the same events and
    /// state. With `dma`, each HALT follows an OAM DMA start from C100, so
    /// the transfer pauses in the HALT (and OAM must match too). Returns
    /// the number of wake-ups (LD B,B after the HALT).
    fn halted_skip_matches_per_mcycle(ie: u8, tac: u8, double: bool) -> usize {
        halted_skip_matches_per_mcycle_dma(ie, tac, double, false)
    }

    fn halted_skip_matches_per_mcycle_dma(ie: u8, tac: u8, double: bool, dma: bool) -> usize {
        let mut program = vec![];
        if double {
            // LD A,01; LDH (4D),A; STOP
            program.extend_from_slice(&[0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00]);
        }
        program.extend_from_slice(&[
            0x3E, tac, 0xE0, 0x07, // LD A,tac; LDH (07),A
            0x3E, ie, 0xE0, 0xFF, // LD A,ie; LDH (FF),A (IME stays off)
            0xAF, 0xE0, 0x0F, // XOR A; LDH (0F),A
        ]);
        if dma {
            // LD A,C1; LDH (46),A
            program.extend_from_slice(&[0x3E, 0xC1, 0xE0, 0x46]);
        }
        program.extend_from_slice(&[
            0x76,
            0x40, // HALT; LD B,B
            0x18,
            if dma { 0xF5 } else { 0xF9 }, // JR back to XOR A
        ]);
        let a = with_program(&program, 0x80);
        let b = with_program(&program, 0x80);
        a.set_stop_events(Events::LD_B_B);
        b.set_stop_events(Events::LD_B_B);
        let mcycle = if double { 2 } else { 4 };
        let mut fb = frame();
        let (mut target, mut wakeups) = (0, 0);
        for _ in 0..40 {
            target = target.max(a.clock()) + 12_345;
            let ra = a.run_until(target, &mut fb, &mut []);
            let rb = run_until_per_mcycle(b, target, &mut fb);
            assert_eq!((ra.clocks, ra.events), (rb.clocks, rb.events));
            assert_eq!(a.clock(), b.clock());
            assert_eq!(a.registers(), b.registers());
            assert_eq!(a.hw.cpu.halted(), b.hw.cpu.halted());
            assert_eq!(a.mem.oam, b.mem.oam);
            let (da, db) = (&a.hw.sys.ppu.dma, &b.hw.sys.ppu.dma);
            assert_eq!((da.start, da.end, da.copied), (db.start, db.end, db.copied));
            for reg in [0xFF04, 0xFF05, 0xFF0F, 0xFF41, 0xFF44] {
                assert_eq!(a.peek(reg), b.peek(reg), "{reg:#06x}");
            }
            if ra.events.contains(Events::LD_B_B) {
                wakeups += 1;
            } else if ra.events.is_empty() {
                // Still halted: the call ends on the first M-cycle
                // boundary at or after the target.
                assert!(a.clock() >= target && a.clock() - target < mcycle);
            }
        }
        wakeups
    }

    #[test]
    fn halted_skip_wakes_like_per_mcycle_idling() {
        // Timer IRQ (TIMA at 262144 Hz and 4096 Hz).
        assert!(halted_skip_matches_per_mcycle(0x04, 0x05, false) > 10);
        assert!(halted_skip_matches_per_mcycle(0x04, 0x04, true) >= 2);
        // VBlank IRQ.
        assert!(halted_skip_matches_per_mcycle(0x01, 0x00, false) > 3);
        assert!(halted_skip_matches_per_mcycle(0x01, 0x00, true) > 3);
        // No IRQ enabled: every call ends at its run target, mid-skip.
        assert_eq!(halted_skip_matches_per_mcycle(0x00, 0x05, false), 0);
        assert_eq!(halted_skip_matches_per_mcycle(0x00, 0x05, true), 0);
        // An OAM DMA paused in each HALT (the skip passes its deadline).
        assert!(halted_skip_matches_per_mcycle_dma(0x04, 0x05, false, true) > 10);
        assert!(halted_skip_matches_per_mcycle_dma(0x04, 0x04, true, true) >= 2);
        assert_eq!(
            halted_skip_matches_per_mcycle_dma(0x00, 0x05, false, true),
            0
        );
    }

    #[test]
    fn stopped_and_locked_skip_matches_per_mcycle() {
        // XOR A; LDH (00),A (select both button groups); STOP; LD B,B;
        // JR -5 (back to STOP); then an invalid opcode (D3) locks up.
        let program = [0xAF, 0xE0, 0x00, 0x10, 0x00, 0x40, 0x18, 0xFB];
        let locked = [0xD3];
        for (program, held) in [
            (&program[..], Buttons::START),
            (&program[..], Buttons::EMPTY),
            (&locked[..], Buttons::EMPTY),
        ] {
            let a = with_program(program, 0x80);
            let b = with_program(program, 0x80);
            a.set_stop_events(Events::LD_B_B);
            b.set_stop_events(Events::LD_B_B);
            let mut fb = frame();
            let mut target = 0;
            for i in 0..30 {
                // A button held from the start wakes STOP after one idle
                // M-cycle; otherwise a press between calls wakes it.
                let buttons = if i % 7 == 6 { Buttons::A } else { held };
                a.set_buttons(buttons);
                b.set_buttons(buttons);
                target = target.max(a.clock()) + 5_001;
                let ra = a.run_until(target, &mut fb, &mut []);
                let rb = run_until_per_mcycle(b, target, &mut fb);
                assert_eq!((ra.clocks, ra.events), (rb.clocks, rb.events));
                assert_eq!(a.registers(), b.registers());
            }
        }
    }

    #[test]
    fn step_while_halted_is_one_mcycle() {
        for (double, mcycle) in [(false, 4), (true, 2)] {
            let mut program = vec![];
            if double {
                program.extend_from_slice(&[0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00]);
            }
            // LD A,01; LDH (FF),A (IE = VBlank); XOR A; LDH (0F),A; HALT
            program.extend_from_slice(&[0x3E, 0x01, 0xE0, 0xFF, 0xAF, 0xE0, 0x0F, 0x76]);
            let emu = with_program(&program, 0x80);
            let mut fb = frame();
            while !emu.hw.cpu.halted() {
                let _ = emu.step(&mut fb, &mut []);
            }
            // Every step is one idle M-cycle until VBlank sets IF. The step
            // that wakes up is the extra wake-up M-cycle plus the NOP after
            // the HALT (IME off).
            let mut steps = 0;
            while emu.hw.cpu.halted() {
                let start = emu.clock();
                let r = emu.step(&mut fb, &mut []);
                let n = if emu.hw.cpu.halted() { 1 } else { 2 };
                assert_eq!((r.clocks, emu.clock() - start), (n * mcycle, n * mcycle));
                steps += 1;
            }
            assert!(steps > 100, "{steps}");
            assert_eq!(emu.peek(0xFF44), 144);
        }
    }

    /// Runs `emu` to its next LD B,B; returns the clock, PC and the low 16
    /// bits of the system counter (DIV is bits 8-15) there.
    fn to_ld_b_b(emu: &mut Emulator) -> (u64, u16, u64) {
        emu.set_stop_events(Events::LD_B_B);
        let start = emu.clock();
        // Frame-ready also ends a run call.
        while !emu
            .run_cycles(1000, &mut frame(), &mut [])
            .events
            .contains(Events::LD_B_B)
        {
            assert!(emu.clock() - start < 300_000);
        }
        let now = emu.clock();
        (now, emu.hw.cpu.regs.pc, emu.hw.sys.timer.sc(now) & 0xFFFF)
    }

    #[test]
    fn speed_switch_halts_0x8002_mcycles_with_the_timer_running() {
        // LD A,01; LDH (4D),A; STOP 00 (to double speed); LD B,B; LDH
        // (4D),A; STOP 00 (back to normal speed); LD B,B. IE = 0, IME off.
        let program = [
            0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x40, 0xE0, 0x4D, 0x10, 0x00, 0x40,
        ];
        let emu = with_program(&program, 0x80);
        // SameBoy `stop()`: STOP's fetch (20-24, DIV reset at its end),
        // its second byte (24-28), then the switch. The halt is 0x20008
        // T-cycles (0x8002 M-cycles) by the timer; the PPU is frozen for
        // the first 5 (`speed_switch_freeze`), so it sees 0x8001
        // double-speed M-cycles (the last the wake-up), then LD B,B's
        // fetch. The timer counts 0x20008 T-cycles from the DIV reset to
        // the end of LD B,B: none for the second byte (`timers_run`
        // returns while `stopped`), 4 fewer for the held DIV reset (IME
        // off: `div_cycles = -4`), 4 more for LD B,B.
        assert_eq!(to_ld_b_b(emu), (28 + 0x8001 * 2 + 2, 0x107, 0x0008));
        assert_eq!(emu.hw.sys.hot.mcycle, 2);
        assert_eq!(emu.peek(0xFF4D) & 0x81, 0x80, "double speed, disarmed");
        // Back: LDH (6 clocks), STOP's two M-cycles (4), the halt (0x8001
        // normal-speed M-cycles less one clock: SameBoy counts 0x20003
        // T-cycles after its 5 frozen ones, Gambatte's PPU gains a clock;
        // ADR 0020), LD B,B (4). The timer counts that clock: DIV is as
        // without it.
        let back = 65568 + 10 + 0x8001 * 4 - 1 + 4;
        assert_eq!(to_ld_b_b(emu), (back, 0x10C, 0x0008));
        assert_eq!(emu.hw.sys.hot.mcycle, 4);
    }

    #[test]
    fn speed_switch_with_ime_on_does_not_hold_the_div_reset() {
        // EI first: the same switch as above, one M-cycle later, and the
        // timer is 4 T-cycles further on (no `div_cycles = -4`).
        let program = [0xFB, 0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x40];
        let emu = with_program(&program, 0x80);
        assert_eq!(to_ld_b_b(emu), (32 + 0x8001 * 2 + 2, 0x108, 0x000C));
    }

    #[test]
    fn speed_switch_with_an_interrupt_pending_does_not_halt() {
        // LD A,01; LDH (FF),A; LDH (0F),A (IE = IF = VBlank, IME off);
        // LDH (4D),A; STOP; LD B,B. STOP is one byte (its "second byte"
        // runs as the next opcode) and no halt follows (SameBoy `stop()`).
        // To double speed the PPU sees the next two M-cycles take 6 clocks
        // (`speed_switch_countdown = 6`, `speed_switch_freeze = 1`): one
        // M-cycle more here, which the timer doesn't count.
        let program = [0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0xE0, 0x4D, 0x10, 0x40];
        let emu = with_program(&program, 0x80);
        // STOP's fetch ends at 48 (DIV reset), then the idle M-cycle and
        // LD B,B. The timer: the 4 T-cycles of LD B,B, less the held reset.
        assert_eq!(to_ld_b_b(emu), (48 + 2 + 2, 0x10A, 0));
        assert_eq!(emu.hw.sys.hot.mcycle, 2);
    }

    #[test]
    fn speed_switch_halt_ends_early_on_an_interrupt() {
        // TAC = 05 (TIMA every 16 T-cycles), IE = timer, IME off; then
        // the switch to double speed. TIMA overflows 256 increments after
        // the DIV reset, about 0x1000 T-cycles: an interrupt wakes the halt
        // as it wakes HALT (SameBoy `GB_cpu_run` clears the countdown).
        let program = [
            0x3E, 0x05, 0xE0, 0x07, 0x3E, 0x04, 0xE0, 0xFF, 0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00,
            0x40,
        ];
        let emu = with_program(&program, 0x80);
        let (now, pc, _) = to_ld_b_b(emu);
        assert_eq!(pc, 0x10F);
        assert!((2000..2200).contains(&now), "{now}");
        assert_eq!(emu.peek(0xFF0F) & 0x04, 0x04);
    }

    #[test]
    fn stop_with_a_button_held_neither_switches_nor_resets_div() {
        // P1 selects both groups and A is held: STOP (KEY1 armed) is a
        // HALT with nothing pending: no DIV reset, no speed switch (SameBoy
        // `stop()`: `exit_by_joyp`; Pan Docs: "Using the STOP
        // instruction"). Its second byte is still read.
        let program = [0xAF, 0xE0, 0x00, 0x3C, 0xE0, 0x4D, 0x10, 0x00, 0x40];
        let emu = with_program(&program, 0x80);
        emu.set_buttons(Buttons::A);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(20_000, &mut frame(), &mut []);
        assert!(r.events.is_empty() && emu.hw.cpu.halted());
        assert_eq!(emu.hw.cpu.regs.pc, 0x108);
        assert_eq!(emu.hw.sys.hot.mcycle, 4);
        assert_eq!(emu.peek(0xFF4D) & 0x81, 0x01, "still armed");
        // DIV ran on from its post-boot value through the STOP.
        let now = emu.clock();
        assert_eq!(emu.hw.sys.timer.sc(now), timer::POST_BOOT_COUNTER + now);
    }

    /// Steps `emu` `n` instructions; returns (clock, PC, whether DIV was
    /// reset since power-on, i.e. the timer counter isn't the post-boot
    /// value plus the elapsed clocks).
    fn steps_to(emu: &mut Emulator, n: usize) -> (u64, u16, bool) {
        let mut fb = frame();
        for _ in 0..n {
            let _ = emu.step(&mut fb, &mut []);
        }
        let now = emu.clock();
        let reset = emu.hw.sys.timer.sc(now) != timer::POST_BOOT_COUNTER + now;
        (now, emu.hw.cpu.regs.pc, reset)
    }

    // SameBoy `stop()` branch 1 with an interrupt pending (branch 1 with
    // nothing pending: `stop_with_a_button_held_neither_switches_nor_resets_div`;
    // 2 and 3: the speed-switch tests above).
    #[test]
    fn stop_with_a_button_held_and_an_interrupt_pending_is_one_byte() {
        // XOR A; LDH (00),A (both groups); INC A; LDH (FF),A; LDH (0F),A
        // (IE = IF = VBlank, IME off); LDH (4D),A (KEY1 armed); STOP; LD
        // B,B. A held: no DIV reset, no switch, no halt, and the byte after
        // STOP is the next opcode.
        let program = [
            0xAF, 0xE0, 0x00, 0x3C, 0xE0, 0xFF, 0xE0, 0x0F, 0xE0, 0x4D, 0x10, 0x40,
        ];
        let emu = with_program(&program, 0x80);
        emu.set_buttons(Buttons::A);
        let (_, pc, reset) = steps_to(emu, 7);
        assert_eq!(pc, 0x10B);
        assert!(!reset);
        assert!(!emu.hw.cpu.halted() && !emu.hw.cpu.stopped());
        assert_eq!(emu.peek(0xFF4D) & 0x81, 0x01, "still armed");
    }

    #[test]
    fn plain_stop_with_nothing_pending_is_two_bytes() {
        // SameBoy `stop()` branch 4: STOP 00 with no button held, KEY1 not
        // armed and IE & IF = 0: DIV reset, the second byte read (fetch
        // 0-4, read 4-8), then STOP mode.
        let emu = with_program(&[0x10, 0x00, 0x40], 0x80);
        assert_eq!(steps_to(emu, 1), (8, 0x102, true));
        assert!(emu.hw.cpu.stopped());
        assert_eq!(emu.peek(0xFF4D) & 0x81, 0x00, "no switch");
    }

    #[test]
    fn plain_stop_with_an_interrupt_pending_is_one_byte() {
        // SameBoy `stop()` branch 5: LD A,01; LDH (FF),A; LDH (0F),A (IE =
        // IF = VBlank, IME off); STOP: DIV reset, one byte (fetch 32-36),
        // then STOP mode; the 40 after it is the next opcode.
        let emu = with_program(&[0x3E, 0x01, 0xE0, 0xFF, 0xE0, 0x0F, 0x10, 0x40], 0x80);
        assert_eq!(steps_to(emu, 4), (36, 0x107, true));
        assert!(emu.hw.cpu.stopped());
    }

    #[test]
    fn speed_switch_halt_pauses_oam_dma() {
        // A transfer from C000 starts, then the switch to double speed
        // with nothing pending: the DMA pauses for the halt (SameBoy
        // `GB_dma_run` returns while `halted`), and what was left takes
        // the same M-cycle count at double speed after it.
        let emu = with_program(&[], 0x80);
        write_reg(emu, 0xFF46, 0xC0);
        let mut fb = frame();
        let Emulator { mem, hw } = &mut *emu;
        let mut ctx = Ctx::new(mem, &mut hw.sys, &mut fb, &mut [], Events::EMPTY, u64::MAX);
        cpu::Bus::idle(&mut ctx);
        let (now, end) = (ctx.clock(), ctx.sys.ppu.dma.end);
        assert!(end > now + 4);
        assert!(cpu::Bus::speed_switch(&mut ctx, false, false));
        assert_eq!(ctx.sys.ppu.dma.paused, now);
        while !cpu::Bus::speed_switch_over(&ctx) {
            cpu::Bus::idle_speed_switch(&mut ctx);
        }
        let wake = ctx.clock();
        assert_eq!(wake, now + 0x8000 * 2);
        cpu::Bus::speed_switch_end(&mut ctx);
        assert_eq!(ctx.sys.ppu.dma.paused, schedule::NEVER);
        assert_eq!(ctx.sys.ppu.dma.end, wake + (end - now) / 2);
    }

    #[test]
    fn reset_rewinds_clock_and_keeps_cart_ram() {
        let rom = vec![0u8; MIN_ROM_SIZE].leak();
        rom[0x147] = 0x08; // ROM+RAM
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
        // LDH (04),A (DIV reset: the bit clock's phase no longer depends
        // on the post-boot counter); LD A,'X'; LDH (01),A; LD A,81;
        // LDH (02),A; JR -2
        let emu = with_program(
            &[
                0xE0, 0x04, 0x3E, b'X', 0xE0, 0x01, 0x3E, 0x81, 0xE0, 0x02, 0x18, 0xFE,
            ],
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
        // The boot ROM hands over in VBlank.
        assert_eq!(emu.peek(0xFF44), 144);
        assert_eq!(emu.peek(0xFF41) & 0x03, 1);
        // Line 3 of the next frame, 8 dots in.
        let line3 = u64::from(FRAME_CLOCKS - ppu::POST_BOOT_POS) + 456 * 3 + 8;
        let _ = emu.run_cycles(line3, &mut fb, &mut []);
        assert_eq!(emu.peek(0xFF44), 3);
        assert_eq!(emu.peek(0xFF41) & 0x03, 2);
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

    // --- Scheduled inputs (ADR 0012) ---

    /// [`emulator`] with both P1 button groups selected (a DMG-mode
    /// cartridge starts with neither, `post_boot_state_of_a_dmg_mode_cartridge`).
    fn input_emulator() -> &'static mut Emulator<'static> {
        let emu = emulator();
        write_reg(emu, 0xFF00, 0x00);
        emu
    }

    /// The P1 input lines.
    fn p1(emu: &Emulator) -> u8 {
        emu.peek(0xFF00) & 0x0F
    }

    fn run_to(emu: &mut Emulator, target: u64) {
        // Frame-ready also ends a run call.
        while emu.clock() < target {
            let _ = emu.run_until(target, &mut frame(), &mut []);
        }
        assert_eq!(emu.clock(), target, "NOP ROM: runs end at the target");
    }

    #[test]
    fn scheduled_input_lands_on_its_mcycle() {
        let emu = input_emulator();
        run_to(emu, 400);
        assert!(emu.set_buttons_at(1000, Buttons::A));
        assert_eq!(p1(emu), 0x0F);
        run_to(emu, 996);
        assert_eq!(p1(emu), 0x0F, "not before its M-cycle");
        run_to(emu, 1000);
        assert_eq!(p1(emu), 0x0E, "from its M-cycle on");
        // Between boundaries: rounded down to the M-cycle it falls in.
        assert!(emu.set_buttons_at(1203, Buttons::EMPTY));
        run_to(emu, 1196);
        assert_eq!(p1(emu), 0x0E);
        run_to(emu, 1200);
        assert_eq!(p1(emu), 0x0F);
    }

    #[test]
    fn scheduled_input_rounds_down_on_the_grid_after_a_speed_switch() {
        // LD A,01; LDH (4D),A; STOP 00 (to double speed); LD B,B; LDH
        // (4D),A; STOP 00 (back to normal speed); NOPs. The first halt
        // ends at 2 mod 4 and an even number of double-speed M-cycles
        // follow; the halt back is one clock short of whole M-cycles
        // (ADR 0020), so the normal-speed grid is at 1 mod 4.
        let program = [
            0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x40, 0xE0, 0x4D, 0x10, 0x00,
        ];
        let emu = with_program(&program, 0x80);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(100_000, &mut frame(), &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        assert_eq!(emu.hw.sys.hot.mcycle, 2, "in double speed");
        // Scheduled in double speed, landing after the switch back.
        let t = (emu.clock() + 40_000) & !3;
        assert!(emu.set_buttons_at(t, Buttons::A));
        run_to(emu, t - 7);
        assert_eq!(emu.hw.sys.hot.mcycle, 4, "back in normal speed");
        assert_eq!(p1(emu), 0x0F);
        // t is not a boundary on this grid: the M-cycle it falls in
        // starts at t - 3.
        run_to(emu, t - 3);
        assert_eq!(p1(emu), 0x0E);
    }

    #[test]
    fn scheduled_input_wakes_stop_on_its_mcycle() {
        // STOP (DMG mode: no speed switch); LD B,B; JR -2.
        let program = [0x10, 0x00, 0x40, 0x18, 0xFE];
        let mut fb = frame();
        let mut clocks = [0; 2];
        for (i, single_steps) in [false, true].into_iter().enumerate() {
            let emu = with_program(&program, 0x00);
            emu.set_stop_events(Events::LD_B_B);
            let _ = emu.run_cycles(10_000, &mut fb, &mut []);
            let d = emu.clock() + 4000;
            assert!(emu.set_buttons_at(d, Buttons::START));
            if single_steps {
                // One idle M-cycle per step, no skip: the reference.
                while !emu.step(&mut fb, &mut []).events.contains(Events::LD_B_B) {}
            } else {
                let r = emu.run_cycles(100_000, &mut fb, &mut []);
                assert_eq!(r.events, Events::LD_B_B);
            }
            clocks[i] = emu.clock();
            // The stopped step's check after its idle M-cycle ending at d
            // sees the press; STOP read its 00 before stopping (nothing
            // pending: two bytes), so LD B,B's fetch follows.
            assert_eq!(emu.clock(), d + 4);
        }
        assert_eq!(clocks[0], clocks[1]);
    }

    #[test]
    fn scheduled_input_p1_read_and_irq_from_its_mcycle_on() {
        use crate::cpu::Bus;
        let emu = input_emulator();
        let now = emu.clock();
        assert!(emu.set_buttons_at(now + 8, Buttons::START));
        emu.hw.sys.iflags = 0;
        let mut fb = frame();
        let mut ctx = Ctx::new(
            emu.mem,
            &mut emu.hw.sys,
            &mut fb,
            &mut [],
            Events::EMPTY,
            u64::MAX,
        );
        // Each read is one M-cycle, at the clock it starts on: the change
        // at now + 8 is seen by the third (P1 lines and IF bit 4).
        let seen: [(u64, u8, u8); 3] = core::array::from_fn(|_| {
            let (at, irq) = (ctx.clock(), ctx.interrupt_flags() & 0x10);
            (at, Bus::read(&mut ctx, 0xFF00) & 0x0F, irq)
        });
        assert_eq!(
            seen,
            [(now, 0x0F, 0), (now + 4, 0x0F, 0), (now + 8, 0x07, 0x10)]
        );
    }

    #[test]
    fn scheduled_input_wakes_halt_on_its_mcycle() {
        // IE = joypad, IF = 0, action buttons selected; HALT; LD B,B.
        let program = [
            0x3E, 0x10, 0xE0, 0xFF, 0xAF, 0xE0, 0x0F, 0x3E, 0x10, 0xE0, 0x00, 0x76, 0x40, 0x18,
            0xFE,
        ];
        let mut fb = frame();
        let mut clocks = [0; 3];
        // Scheduled with the halted skip; scheduled with one idle M-cycle
        // per step (no skip); and `set_buttons` once the clock is at d.
        for (i, how) in [0, 1, 2].into_iter().enumerate() {
            let emu = with_program(&program, 0x80);
            emu.set_stop_events(Events::LD_B_B);
            let _ = emu.run_cycles(10_000, &mut fb, &mut []);
            assert!(emu.hw.cpu.halted());
            // Far from any other deadline (the next VBlank), so only the
            // Input deadline can stop a halted skip there.
            let d = emu.clock() + 4000;
            if how == 2 {
                run_to(emu, d);
                emu.set_buttons(Buttons::START);
            } else {
                assert!(emu.set_buttons_at(d, Buttons::START));
            }
            if how == 1 {
                while !emu.step(&mut fb, &mut []).events.contains(Events::LD_B_B) {}
            } else {
                let r = emu.run_cycles(100_000, &mut fb, &mut []);
                assert_eq!(r.events, Events::LD_B_B);
            }
            assert_eq!(emu.registers().pc, 0x10D);
            clocks[i] = emu.clock();
        }
        assert_eq!(clocks[0], clocks[2], "same as set_buttons at d");
        assert_eq!(clocks[0], clocks[1], "same as without the skip");
    }

    #[test]
    fn scheduled_input_in_the_past_applies_now() {
        let emu = input_emulator();
        run_to(emu, 400);
        emu.hw.sys.iflags = 0;
        assert!(emu.set_buttons_at(100, Buttons::B));
        assert_eq!((p1(emu), emu.peek(0xFF0F) & 0x10), (0x0D, 0x10));
        // The current clock, and any in the M-cycle starting now, count
        // as the past.
        assert!(emu.set_buttons_at(400, Buttons::EMPTY));
        assert_eq!(p1(emu), 0x0F);
        assert!(emu.set_buttons_at(403, Buttons::A));
        assert_eq!(p1(emu), 0x0E);
        let input = emu.hw.sys.sched.deadline(schedule::EventKind::Input);
        assert_eq!(input, schedule::NEVER);
    }

    #[test]
    fn scheduled_inputs_reschedule_queue_and_collapse() {
        let emu = input_emulator();
        run_to(emu, 400);
        // Scheduled out of order: they still apply in clock order.
        assert!(emu.set_buttons_at(2000, Buttons::B));
        assert!(emu.set_buttons_at(1000, Buttons::A));
        // Same clock: the later call replaces the change.
        assert!(emu.set_buttons_at(3000, Buttons::UP));
        assert!(emu.set_buttons_at(3000, Buttons::SELECT));
        assert!(emu.set_buttons_at(4000, Buttons::EMPTY));
        // Full: a new future clock is refused and changes nothing; a
        // replacement or a past clock still works.
        assert!(!emu.set_buttons_at(5000, Buttons::START));
        assert!(emu.set_buttons_at(4000, Buttons::EMPTY));
        // `set_buttons` applies now and keeps the queue.
        emu.set_buttons(Buttons::START);
        assert_eq!(p1(emu), 0x07);
        run_to(emu, 1000);
        assert_eq!(p1(emu), 0x0E);
        run_to(emu, 2000);
        assert_eq!(p1(emu), 0x0D);
        run_to(emu, 3000);
        assert_eq!(p1(emu), 0x0B);
        run_to(emu, 5000);
        assert_eq!(p1(emu), 0x0F, "the refused change never applies");
        // Two changes landing on the same M-cycle boundary apply as one:
        // a press and release within it raise no interrupt.
        emu.hw.sys.iflags = 0;
        assert!(emu.set_buttons_at(5201, Buttons::A));
        assert!(emu.set_buttons_at(5203, Buttons::EMPTY));
        run_to(emu, 5196);
        assert_eq!(emu.hw.sys.sched.deadline(schedule::EventKind::Input), 5198);
        run_to(emu, 5200);
        assert_eq!((p1(emu), emu.peek(0xFF0F) & 0x10), (0x0F, 0));
        // Reset drops pending changes.
        assert!(emu.set_buttons_at(6000, Buttons::A));
        emu.reset();
        run_to(emu, 8000);
        assert_eq!(p1(emu), 0x0F);
    }

    /// Runs the double-speed program of
    /// `scheduled_input_rounds_down_on_the_grid_after_a_speed_switch` to
    /// its LD B,B, schedules `changes` at clocks relative to `T` (the
    /// clock where its second STOP switches back to normal speed), and
    /// runs past the switch. Returns IF bit 4 and the P1 lines.
    fn across_switch_to_normal(changes: &[(u64, Buttons)]) -> (u8, u8) {
        let program = [
            0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00, 0x40, 0xE0, 0x4D, 0x10, 0x00,
        ];
        let emu = with_program(&program, 0x80);
        emu.set_stop_events(Events::LD_B_B);
        let r = emu.run_cycles(100_000, &mut frame(), &mut []);
        assert_eq!(r.events, Events::LD_B_B);
        // LDH (6 clocks), STOP's two M-cycles (4), then the switch. T is
        // 2 mod 4; the halt idles 3 clocks first (ADR 0020), so the
        // normal-speed grid after it is at T + 3.
        const T: u64 = 65578;
        assert_eq!(emu.clock(), T - 10);
        emu.set_stop_events(Events::EMPTY);
        for &(dt, buttons) in changes {
            assert!(emu.set_buttons_at(T + dt, buttons));
        }
        emu.hw.sys.iflags = 0;
        // IE = 0 and IME = 0 after boot: IF bit 4 stays as set.
        let _ = emu.run_cycles(20_000, &mut frame(), &mut []);
        assert_eq!(emu.hw.sys.hot.mcycle, 4);
        (emu.peek(0xFF0F) & 0x10, p1(emu))
    }

    #[test]
    fn scheduled_input_inside_the_speed_switch_pause_lands_on_its_mcycle() {
        // Each change lands on its own boundary on the new grid inside
        // the speed-switch halt (it idles a deadline at a time), not
        // after it: a press and a release 2000 clocks apart raise
        // the joypad interrupt.
        let (a, none) = (Buttons::A, Buttons::EMPTY);
        assert_eq!(
            across_switch_to_normal(&[(1001, a), (3001, none)]),
            (0x10, 0x0F)
        );
        // T + 1001 and T + 1002 share the normal-speed M-cycle starting at
        // T + 999 and collapse (no interrupt); T + 1003 starts the next.
        assert_eq!(
            across_switch_to_normal(&[(1001, a), (1002, none)]),
            (0, 0x0F)
        );
        assert_eq!(
            across_switch_to_normal(&[(1001, a), (1003, none)]),
            (0x10, 0x0F)
        );
        assert_eq!(across_switch_to_normal(&[(1001, a)]), (0x10, 0x0E));
    }

    #[test]
    fn scheduled_input_already_past_after_a_speed_switch_lands_next() {
        // Scheduled in double speed for T + 2; on the normal-speed grid
        // after the switch at T (grid at T + 3, ADR 0020) that M-cycle
        // started at T - 1, already past, so it lands at the next
        // boundary, T + 3: with a release at T + 6 (also T + 3) it
        // collapses, with one at T + 7 it doesn't.
        let (a, none) = (Buttons::A, Buttons::EMPTY);
        assert_eq!(across_switch_to_normal(&[(2, a), (6, none)]), (0, 0x0F));
        assert_eq!(across_switch_to_normal(&[(2, a), (7, none)]), (0x10, 0x0F));
    }

    #[test]
    fn scheduled_input_lands_on_the_double_speed_grid() {
        // LD A,01; LDH (4D),A; STOP 00 (to double speed at clock 28,
        // after the halt); NOPs. A change scheduled in normal speed
        // lands on the double-speed grid (24 + 2k).
        let program = [0x3E, 0x01, 0xE0, 0x4D, 0x10, 0x00];
        let emu = with_program(&program, 0x80);
        assert!(emu.set_buttons_at(4167, Buttons::A));
        run_to(emu, 4164);
        assert_eq!(emu.hw.sys.hot.mcycle, 2, "in double speed");
        assert_eq!(p1(emu), 0x0F, "not before its M-cycle");
        run_to(emu, 4166);
        assert_eq!(p1(emu), 0x0E, "from its M-cycle on");
    }

    #[test]
    fn load_and_eject_drop_scheduled_inputs() {
        let emu = input_emulator();
        run_to(emu, 400);
        assert!(emu.set_buttons_at(1000, Buttons::A));
        let rom = vec![0u8; 0x8000].leak();
        emu.load(rom, &mut [], Config::default()).unwrap();
        assert_eq!(emu.hw.sys.inputs.len(), 0, "load drops the queue");
        run_to(emu, 2000);
        assert_eq!(p1(emu), 0x0F);
        assert!(emu.set_buttons_at(3000, Buttons::A));
        let _ = emu.eject();
        assert_eq!(emu.hw.sys.inputs.len(), 0, "eject drops the queue");
        let input = emu.hw.sys.sched.deadline(schedule::EventKind::Input);
        assert_eq!(input, schedule::NEVER);
    }

    #[test]
    fn scheduled_input_at_u64_max_is_refused() {
        // u64::MAX is the queue's empty-slot sentinel: it would never
        // fire and hold a slot until reset.
        let emu = input_emulator();
        assert!(!emu.set_buttons_at(u64::MAX, Buttons::A));
        assert_eq!(emu.hw.sys.inputs.len(), 0, "no slot taken");
        assert!(emu.set_buttons_at(u64::MAX - 1, Buttons::A));
    }
}
