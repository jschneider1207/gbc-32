# 0009: `gbc-core` contains no `unsafe` code

- Status: Accepted (approved by the human 2026-11-21)
- Date: 2026-11-21
- Supersedes: the "construction" part of ADR 0006 (`Emulator::init` over
  `MaybeUninit`, the `ZeroValid` marker trait). The rest of 0006 stands.

## Context
The README's quality bar asks for a core that is memory-safe by
construction. Hardware access needs `unsafe`, but that lives in the
platform layers. The core's only `unsafe` came from ADR 0006's in-place
construction: `Emulator::init(&mut MaybeUninit<Self>)` zero-fills the
~49 KiB `Memory` in place (an `unsafe` block, plus an `unsafe trait
ZeroValid` asserting that all-zero is valid). The goal was to never build
the large struct on the MCU's small stack, and to keep a large initial
image out of flash and `.data`.

A plain `static EMU: Emulator = Emulator::new()` avoids the stack, but
`Emulator` contains non-zero values (slice pointers, registers). The whole
~49 KiB would then land in `.data`: flash space plus a copy at boot.

## Decision
- **Split the large, all-zero state out of `Emulator`.** `Memory` (WRAM,
  VRAM, OAM, HRAM and other byte arrays: everything large) is its own
  public type, with `const fn new()` that is all zero bytes. `Emulator<'a>`
  holds the small remaining state and borrows `&'a mut Memory`, alongside
  the ROM and cart RAM borrows it already has.
  - MCU: `static mut MEM: Memory = Memory::new();` is all zeros, so it goes
    to `.bss`: no flash image, no boot copy, no stack. The small
    `Emulator` can live on the stack or in a static. Obtaining
    `&'static mut` from a static is the platform's business
    (`static_cell` or a small audited `unsafe` there).
  - Desktop and tools: `Box::new(Memory::new())`. A transient ~49 KiB
    stack temporary in unoptimized builds is harmless on a PC.
- **`#![forbid(unsafe_code)]` in `crates/gbc-core/src/lib.rs`.** `forbid`
  can't be overridden by an inner `allow`. `crates/gbc-core/tests/hard_rules.rs`
  checks that the attribute is present, and that no `unsafe` token appears
  anywhere in the core's sources.
- **Exceptions need the human.** If a measured performance win ever
  requires `unsafe` in the core, it is escalated, not added quietly.

## Consequences
- The core is memory-safe by construction, so Miri is not needed for it.
  Miri still applies to `unsafe` in front ends and platform layers.
- The API changes once (romtest, tests, docs) before M2–M7 build on it.
- Memory may sit behind one more pointer than when it was an inline field.
  The hot path already reaches state through the per-call context, so no
  measurable cost is expected. This is verified with the M1 benchmark
  (cpu_instrs wall time) before and after. A regression is reported, not
  hidden.
