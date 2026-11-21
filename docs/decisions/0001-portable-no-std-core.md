# 0001: Portable `no_std`, heap-free emulator core

- Status: Accepted
- Date: 2026-11-21

## Context
The emulator must run on a desktop harness (Stage 1) and on bare-metal
STM32H7 (Stage 2+). Maintaining two cores, or porting one later, invites
divergence and bugs that only appear on hardware.

## Decision
All emulation logic lives in `gbc-core`, which is `#![no_std]` and does not
use `alloc`. All buffers (ROM, cartridge RAM, framebuffer, audio) are
provided by the caller. The core contains no platform code.

## Consequences
- The same code, tested on desktop, runs on the MCU. The gate builds the
  core for `thumbv7em-none-eabihf` on every run to enforce this.
- No `Vec`/`Box` inside the core; sizes are fixed or come from the caller.
- Desktop conveniences (logging, file I/O) live in front ends and tools only.
