# Architecture

This is the living description of how the code is structured. Update it when
the structure changes. The reasons behind decisions belong in
`docs/decisions/`; this file describes the current shape.

> Status: initial sketch written before any code existed. M0 makes it concrete.

## Workspace

```
Cargo.toml                 workspace
rust-toolchain.toml        pinned stable toolchain + thumbv7em-none-eabihf
crates/
  gbc-core/                #![no_std], no alloc. All emulation logic.
  gbc-desktop/             std front end (Stage 1 only): window, audio, input, saves
  gbc-stm32/               (Stage 2) bare-metal platform layer
tools/
  romtest/                 acceptance runner (contract: acceptance/README.md)
```

Dependency direction: front ends and tools depend on `gbc-core`. `gbc-core`
depends on nothing outside `core` without human approval (AGENTS.md).

## Core ↔ platform boundary

The core is a library the platform drives. It never calls out to the
platform.

- **ROM:** a `&[u8]` borrowed from the caller. On the MCU this is the
  memory-mapped QSPI/eMMC-cached region; on desktop, a `Vec`.
- **Cartridge RAM:** a `&mut [u8]` owned by the caller (sized from the
  header, up to 128 KiB). The caller persists it.
- **RTC:** state is advanced by emulated time. The caller can read and write
  the RTC state to save it or to sync it to wall-clock time on load.
- **Video:** the core writes 160×144 RGB555 (`u16`) pixels into a caller-provided
  framebuffer, line by line. Converting to the display's color format is
  the platform's job. For DMG games (compatibility mode), the caller passes
  the BG/OBJ0/OBJ1 compatibility palettes at construction, in place of the
  boot ROM's choice. `romtest` passes the reference "common palette"
  (acceptance/README.md); the desktop front end may choose per title.
- **Audio:** the core writes stereo samples into a caller-provided buffer at
  a fixed internal rate. Resampling is the platform's job (exact approach
  decided in M6).
- **Input:** `set_buttons(Buttons)`, where `Buttons` is a hand-written bit set (not the `bitflags` crate; the core has no dependencies).
- **Driving:** `run_frame()` runs until VBlank. `run_cycles(n)` and
  `step()` exist for tools. All three return an event set (frame ready, audio
  buffer full, `LD B,B` breakpoint hit, CPU locked up, ...).
- **Debug access** for `romtest` and tests: registers, serial output bytes,
  and memory peek. This must cost nothing in release embedded builds
  (plain fields and flags, no callbacks or dynamic dispatch on hot paths).

The `Emulator` struct is large (WRAM 32 KiB + VRAM 16 KiB + OAM, etc.). It
must never be required to live on the stack: desktop boxes it, and the MCU
puts it in a `static` placed in a chosen RAM region. Note that
`Box::new(Emulator::new())` still builds it on the stack first. The API
therefore needs a `const fn` constructor or initialization in place
(e.g. `init(&mut self, ...)`). How ROM and cart RAM borrows coexist with a
`static` emulator (`'static` borrows vs. passing buffers on each call) is an
M0 decision; record it in an ADR.

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

```
cpu/        SM83 decode/execute, interrupts, HALT/STOP
bus.rs      memory map, I/O dispatch, banking
timer.rs    DIV/TIMA
ppu/        mode state machine, renderer, palettes, OAM scan
apu/        channels, frame sequencer, mixer
dma.rs      OAM DMA, GDMA/HDMA
cart/       header, MBC1/2/3(+RTC)/5
serial.rs   SB/SC; transfer capture for tests
```
