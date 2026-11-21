# Architecture

This is the living description of how the code is structured. Update it when
the structure changes. The reasons behind decisions belong in
`docs/decisions/`; this file describes the current shape.

> Status: the workspace and the core's public API (ADR 0006) are concrete as
> of M0. Internals beyond the skeleton follow the module sketch below.

## Workspace

```
Cargo.toml                 workspace
rust-toolchain.toml        pinned stable toolchain + thumbv7em-none-eabihf
crates/
  gbc-core/                #![no_std], no alloc. All emulation logic.
  gbc-desktop/             std front end (Stage 1 only): window, audio, input, saves
  gbc-stm32/               (Stage 2, not yet created) bare-metal platform layer
tools/
  romtest/                 acceptance runner (contract: acceptance/README.md)
```

Workspace: edition 2024, resolver 3. The release profile keeps unwinding
panics (`romtest` catches them per ROM) and line-table debug info. Workspace
lints deny `unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks`
(every `unsafe` block needs a `// SAFETY:` comment).

Dependency direction: front ends and tools depend on `gbc-core`. `gbc-core`
depends on nothing outside `core` without human approval (AGENTS.md).

## Core ↔ platform boundary

The core is a library the platform drives. It never calls out to the
platform. The concrete API and its rationale are in ADR 0006; the crate
docs in `crates/gbc-core/src/lib.rs` have a usage example.

```text
Emulator<'a>                       // 'a = lifetime of the cartridge ROM/RAM borrows
  const fn new() -> Self                            // powered off, no cartridge
  fn init(&mut MaybeUninit<Self>) -> &mut Self      // same, built in place (no stack temporary)
  fn load(&mut self, rom: &'a [u8], cart_ram: &'a mut [u8], Config) -> Result<(), LoadError>
  fn reset(&mut self)                               // power cycle; cart RAM/RTC kept
  fn set_buttons(&mut self, Buttons)
  fn set_stop_events(&mut self, Events)             // opt-in stops: LD_B_B, SERIAL_TX
  fn run_frame (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_cycles(&mut self, clocks: u64, &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_until (&mut self, clock: u64,  &mut Frame, &mut [StereoSample]) -> RunResult
  fn step      (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  // debug: clock(), registers(), peek(addr), serial_byte(), lcd_enabled(), cpu_locked()
  // cartridge: cart_ram(), rtc_state(), set_rtc_state()
RunResult { events: Events, clocks: u64, audio_samples: usize }
cart_ram_size(rom) -> Result<usize, LoadError>
```

- **ROM:** a `&'a [u8]` borrowed from the caller. On the MCU this is the
  memory-mapped QSPI/eMMC-cached region (`'static`); on desktop, a `Vec`.
- **Cartridge RAM:** a `&'a mut [u8]` owned by the caller, at least
  `cart_ram_size(rom)` bytes (up to 128 KiB). The caller persists it, reading
  it through `cart_ram()` while the emulator holds the borrow.
- **RTC:** state is advanced by emulated time. The caller can read and write
  the RTC state (`rtc_state`/`set_rtc_state`) to save it or to sync it to
  wall-clock time on load.
- **Video:** the caller passes a `Frame` (160×144 RGB555 `u16`, bits 0–4
  red, 5–9 green, 10–14 blue) to every run call, so it can double-buffer.
  The core draws into it line by line; it holds a whole frame when the
  result contains `FRAME_READY` (start of VBlank). Converting to the display's
  color format is the platform's job. For DMG games (compatibility mode), the
  caller passes the BG/OBJ0/OBJ1 compatibility palettes in `Config` at
  `load`, in place of the boot ROM's choice. `romtest` passes the reference
  "common palette" (acceptance/README.md); the desktop front end may choose
  per title.
- **Audio:** the caller passes a `&mut [StereoSample]` to every run call;
  the core writes from index 0 and reports the count. A full buffer ends
  the call. The internal rate is fixed and resampling is the platform's job
  (exact approach decided in M6).
- **Input:** `set_buttons(Buttons)`, where `Buttons` is a hand-written bit set
  (not the `bitflags` crate; the core has no dependencies). So is `Events`.
- **Driving and time:** all times are base clocks (4,194,304 Hz) regardless
  of CGB double speed. `run_until(t)` runs until `clock() >= t`, ending on an
  instruction boundary (it may overshoot slightly) or earlier on a stop event.
  `run_cycles(n)` = `run_until(clock() + n)`, `run_frame()` =
  `run_cycles(70_224)`. `FRAME_READY`, `AUDIO_BUFFER_FULL` and `CPU_LOCKED`
  always end a call; `LD_B_B` and `SERIAL_TX` only when enabled.
- **Debug access** for `romtest` and tests: registers, the last serial byte
  (with the `SERIAL_TX` event), memory peek, the clock, LCD on/off. These are
  plain field reads; nothing is on the hot path (no callbacks or dynamic
  dispatch).

The `Emulator` struct is large (WRAM 32 KiB + VRAM 16 KiB + OAM, etc.) and
must never be required to live on the stack. It has exactly two fields:
`mem: Memory` (only `u8` arrays, so all-zero is valid) and `hw: Hw<'a>`
(everything else, kept under 512 bytes). `Emulator::init` zero-fills `mem`
in place and writes `hw`, so desktop and `romtest` use `Box::new_uninit()`
plus `init`, and the MCU uses a `static MaybeUninit<Emulator<'static>>` in
a no-init RAM section. New large state goes in `Memory`, everything else in
`Hw`.

Core unit tests live in `#[cfg(test)]` modules, which may use `std`. The
gate only forbids `alloc` in `gbc-core/src`, and the `thumbv7em` build
enforces `no_std` for non-test code.

## Timing model

See ADR 0002. In short:
- The CPU is the master clock and ticks the timer/DMA every M-cycle.
- The PPU and APU are lazily caught up to the current cycle whenever one of
  their registers is accessed, and at scheduled events (mode changes, frame
  end, audio buffer boundaries).
- PPU mode timing is exact. Pixel output is drawn a scanline at a time, with
  a mid-line flush on register writes.

## Module sketch (`gbc-core`)

Modules marked * exist as skeletons (M0); the others are planned.

```
lib.rs      * Emulator, Hw, RunResult, run loop, public re-exports
memory.rs   * Memory: the large RAM arrays (WRAM, VRAM, OAM, HRAM)
bitset.rs   * bitset! macro behind Buttons and Events
events.rs   * Events
joypad.rs   * Buttons; later P1/JOYP
config.rs   * Config, DmgPalettes, RGB555 helpers
cpu/        * Registers; later SM83 decode/execute, interrupts, HALT/STOP
bus.rs      * debug peek; later memory map, I/O dispatch, banking
timer.rs      DIV/TIMA
ppu/        * Frame, stub frame timing; later mode state machine, renderer, palettes, OAM scan
apu/        * StereoSample; later channels, frame sequencer, mixer
dma.rs        OAM DMA, GDMA/HDMA
cart/       * header (cart_ram_size), RtcState; later MBC1/2/3(+RTC)/5
serial.rs   * last byte sent; later SB/SC transfers
```
