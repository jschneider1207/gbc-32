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

## Implementation notes (2026-11-21)

Final construction API (the rest of ADR 0006's API is unchanged):

```text
pub struct Memory                                   // ~49 KiB of RAM arrays, fields private
  pub const fn new() -> Memory                      // all zero bytes; no Default impl
pub struct Emulator<'a> { mem: &'a mut Memory, hw: Hw<'a> }
  pub fn new(mem: &'a mut Memory) -> Emulator<'a>   // powered off, no cartridge; zero-fills mem
  pub fn load(&mut self, rom: &'a [u8], cart_ram: &'a mut [u8], Config)
      -> Result<Option<CartBuffers<'a>>, LoadError<'a>>   // unchanged
  pub fn eject(&mut self) -> Option<CartBuffers<'a>>      // unchanged; back to the `new` state
```

- **`Memory` is passed at construction, not at `load`.** An emulator
  without a cartridge still has RAM (`peek`, `eject` back to the `new`
  state), and one borrow for the emulator's life keeps the hot path and
  `load`/`eject` exactly as they were. `eject`/`load` still hand back only
  the cartridge buffers; the `Memory` borrow ends when the `Emulator` is
  dropped.
- **`Emulator::new` is not a `const fn`.** It zero-fills the `Memory` it is
  given, so the state never depends on what an earlier user left there. A
  `const` initializer could not write to another static's memory anyway,
  so `const` would buy nothing. The small `Emulator` is built at run time,
  on the stack or in a static cell.
- **`Memory` stays all-zero.** It has no field with a non-zero power-on
  value: the post-boot I/O register values are written by `reset` (via
  `Sys::reset`), as before. The `memory_new_is_all_zero_bytes` unit test
  checks every field of a compile-time `Memory::new()` is zero and that the
  fields cover all of `size_of::<Memory>()` (no padding). On
  `thumbv7em-none-eabihf` release, a scratch staticlib with
  `static mut MEM: Memory = Memory::new();` puts `MEM` in `.bss.MEM`
  (`NOBITS`, 0xC1FF = 49,663 bytes; `nm`: `B MEM`).
- **`hard_rules.rs`** now also fails on any `unsafe` token in
  `crates/gbc-core/src` (test modules included; comments and literals are
  ignored) and if `lib.rs` lacks `#![forbid(unsafe_code)]`.

Performance: unchanged, as expected, since run calls already reached
memory through the `&mut Memory` in the per-call `Ctx`. Blargg
`cpu_instrs` per-ROM wall time from romtest (host x86-64 release, best of
5, old and new binaries alternating, two rounds): 0.308 s before / 0.289 s
after, then 0.285 s / 0.276 s; the difference is noise. Code size
(`thumbv7em-none-eabihf` release, `-C target-cpu=cortex-m7`, `nm -S` on
the rlib's objects): `Cpu::run::<Ctx>` 37,064 B and
`Cpu::execute_cb::<Ctx>` 15,702 B, both identical before and after; crate
`.text` 63,805 B before, 64,025 B after (the new `Emulator::new`, 182 B,
replaces the inlined `init`). All unit tests, the 500,000 SM83
single-step cases and the full romtest manifest results (clocks, frames,
serial output, registers) are identical.
