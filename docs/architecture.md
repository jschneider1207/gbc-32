# Architecture

This is the living description of how the code is structured. Update it when
the structure changes. The reasons behind decisions belong in
`docs/decisions/`; this file describes the current shape.

> Status: the workspace and the core's public API (ADR 0006) are concrete as
> of M0; the CPU, memory bus, timer and serial port as of M1 (ADR 0008).
> The PPU and APU are still skeletons. Targets (dev board, then custom
> board) as of 2026-11-21.

## Workspace

```
Cargo.toml                 workspace
rust-toolchain.toml        pinned stable toolchain + thumbv7em-none-eabihf
crates/
  gbc-core/                #![no_std], no alloc. All emulation logic.
  gbc-desktop/             std front end (Stage 1 only): window, audio, input, saves
  gbc-stm32/               (Stage 2, not yet created) bare-metal platform layer(s); see Targets
tools/
  romtest/                 acceptance runner (contract: acceptance/README.md)
  sm83-tests/              SM83 single-step CPU tests (data: scripts/fetch-sm83-tests.sh -> test-data/, gitignored)
```

Workspace: edition 2024, resolver 3. The release profile keeps unwinding
panics (`romtest` catches them per ROM) and line-table debug info. Workspace
lints deny `unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks`
(every `unsafe` block needs a `// SAFETY:` comment).

Dependency direction: front ends and tools depend on `gbc-core`. `gbc-core`
depends on nothing outside `core` without human approval (AGENTS.md).

## Targets

There are two MCU targets in sequence. Both use the same `gbc-core` build
(`thumbv7em-none-eabihf`), but each needs its own platform layer. How that
code is split (one crate with per-board modules, or separate crates) is
decided when Stage 2 starts.

| | Stage 2: dev board | Stage 3+: custom board |
|---|---|---|
| Board | NUCLEO-H753ZI + X-NUCLEO-GFX02Z1 | Custom PCB (`../gbc-32-hardware`) |
| MCU | STM32H753ZI, 480 MHz, RM0433 | STM32H735IGK6, 520 MHz (550 with `CPU_FREQ_BOOST`), RM0468 |
| Core supply | LDO | On-chip SMPS (different supply config at boot) |
| Internal RAM / L1 cache | ~1 MB / 16 KB I + 16 KB D | 564 KB / 32 KB I + 32 KB D |
| ROM source | Shield's QSPI NOR, memory-mapped | eMMC → copied into external FMC SDRAM (32 MiB) at load |
| Saves | (Stage 2 choice) | eMMC |
| Display | Shield's ILI9341V, 8-bit 8080 bus on FMC (stand-in only) | TBD by the hardware repo (FMC parallel or LTDC) |
| Audio | (Stage 2 choice) | SAI → external DAC/amplifier |

What carries over from Stage 2: proof that the core runs at full speed on a
Cortex-M7, the frame pacing / audio / input structure, and the cache and
memory-placement findings. The latter are a starting point only, because
the RAM layout and cache sizes differ. Reading the ROM from QSPI NOR is a
conservative stand-in for SDRAM. The custom-board contract is
`../gbc-32-hardware/docs/firmware-interface.md`.

## Core ↔ platform boundary

The core is a library the platform drives. It never calls out to the
platform. The concrete API and its rationale are in ADR 0006; the crate
docs in `crates/gbc-core/src/lib.rs` have a usage example.

```text
Emulator<'a>                       // 'a = lifetime of the cartridge ROM/RAM borrows
  const fn new() -> Self                            // powered off, no cartridge; const/static only
  fn init(&mut MaybeUninit<Self>) -> &mut Self      // same, built in place (no stack temporary)
  fn load(&mut self, rom: &'a [u8], cart_ram: &'a mut [u8], Config)
      -> Result<Option<CartBuffers<'a>>, LoadError<'a>>  // Ok: previous cart's buffers; Err: hands back rom/cart_ram
  fn eject(&mut self) -> Option<CartBuffers<'a>>    // unload; back to the init state
  fn set_dmg_palettes(&mut self, DmgPalettes)       // no reset
  fn reset(&mut self)                               // power cycle; cart RAM/RTC kept
  fn set_buttons(&mut self, Buttons)
  fn set_stop_events(&mut self, Events)             // opt-in stops: LD_B_B, SERIAL_TX
  fn run_frame (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_cycles(&mut self, clocks: u64, &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_until (&mut self, clock: u64,  &mut Frame, &mut [StereoSample]) -> RunResult
  fn step      (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  // debug: clock(), registers(), peek(addr), serial_byte(), lcd_enabled(), screen_blank(), cpu_locked()
  // cartridge: cart_ram(), rtc_state(), set_rtc_state()
RunResult { events: Events, clocks: u64, audio_samples: usize }
cart_ram_size(rom) -> Result<usize, LoadErrorKind>
type CartBuffers<'a> = (&'a [u8], &'a mut [u8])     // RAM buffer as passed (untrimmed)
LoadError<'a> { kind: LoadErrorKind, rom: &'a [u8], cart_ram: &'a mut [u8] }

```

- **ROM:** a `&'a [u8]` borrowed from the caller; on desktop, a `Vec`. On
  the MCU it is `'static` and lives in external memory:
  - Stage 2: the shield's QSPI NOR, memory-mapped.
  - Stage 3+: external FMC SDRAM, filled from eMMC when the game loads.

  The core must not assume the ROM is in internal RAM, or that it is
  aligned beyond a byte. On the custom board, unaligned reads from the FMC
  region are unsupported (STM32H735 erratum ES0491 §2.7.3). Code that reads
  the ROM therefore has to be built so the compiler never merges byte reads
  into an unaligned halfword or word read, and the platform has to map the
  SDRAM as normal, cacheable memory through the MPU.
- **Cartridge RAM:** a `&'a mut [u8]` owned by the caller, at least
  `cart_ram_size(rom)` bytes (up to 128 KiB). Only the header's size is
  used; `cart_ram()` (the save file) is exactly that long. The caller
  persists it, reading it through `cart_ram()` while the emulator holds the
  borrow, and gets the whole buffer back from `eject`, a later `load`, or a
  `LoadError`.
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
(everything else, kept under 1 KiB). `Emulator::init` zero-fills `mem`
in place and writes `hw`, so desktop and `romtest` use `Box::new_uninit()`
plus `init`, and the MCU uses a `static MaybeUninit<Emulator<'static>>` in
a no-init RAM section. New large state goes in `Memory`, everything else in
`Hw`.

Core unit tests live in `#[cfg(test)]` modules, which may use `std`. The
gate only forbids `alloc` in `gbc-core/src`, and the `thumbv7em` build
enforces `no_std` for non-test code.

## Timing model

See ADR 0002. In short:
- The CPU is the master clock and ticks the timer/serial (later DMA) every M-cycle (ADR 0008).
- The PPU and APU are lazily caught up to the current cycle whenever one of
  their registers is accessed, and at scheduled events (mode changes, frame
  end, audio buffer boundaries).
- PPU mode timing is exact. Pixel output is drawn a scanline at a time, with
  a mid-line flush on register writes.

## Module sketch (`gbc-core`)

Modules marked * exist; the others are planned. Skeleton parts are noted.

```
lib.rs      * Emulator, Hw (cpu + sys), RunResult, run loop, public re-exports, sm83 test API
memory.rs   * Memory: the large RAM arrays (WRAM, VRAM, OAM, HRAM, FEA0 area, stub I/O storage)
bitset.rs   * bitset! macro behind Buttons and Events
events.rs   * Events
joypad.rs   * Buttons (P1/JOYP reads are in bus.rs)
config.rs   * Config, DmgPalettes, RGB555 helpers
cpu/        * SM83: Registers, Bus trait, Cpu (step, interrupts, HALT/HALT bug, STOP stub)
  exec.rs   *   opcode tables: per-opcode monomorphized bodies behind a 256-arm match
bus.rs      * Sys (state the bus reaches), Ctx (per-run-call context), memory map, I/O, peek
timer.rs    * DIV/TIMA/TMA/TAC (falling-edge counter; its fallen bits clock serial)
ppu/        * Frame, lazy stub timing (VBlank, LY, STAT mode); later modes, renderer, palettes
apu/        * StereoSample; later channels, frame sequencer, mixer
dma.rs        OAM DMA (instant-copy stub in bus.rs until M2), GDMA/HDMA
cart/       * header (cart_ram_size), RtcState, ROM bank windows, MBC1; later MBC2/3(+RTC)/5
serial.rs   * SB/SC internal-clock transfers, last byte sent
```

### CPU and bus (ADR 0008)

`Hw` holds `cpu: Cpu` and `sys: Sys<'a>` (everything the bus reaches:
clock, PPU, timer, serial, cartridge, interrupt and banking registers).
Each run call builds one `Ctx { mem, sys, frame, audio, .. }` and calls
`cpu.step(&mut ctx)` per instruction. The CPU is generic over the `Bus`
trait; every `read`/`write`/`idle` is one M-cycle: the access at the
current clock, then a tick (clock, timer, serial, and one compare against
`Sys::next_event` for lazily-run components such as the PPU). Reads have no
side effects, so the CPU and `Emulator::peek` share one `read` function.
The same CPU runs against a flat test bus in `tools/sm83-tests`.
