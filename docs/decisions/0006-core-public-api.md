# 0006: Core public API (buffers, construction, events, debug access)

- Status: Accepted
- Date: 2026-11-21

## Context
`gbc-core` is `no_std` without `alloc` (ADR 0001), and the same API must
serve three callers: the MCU (emulator in a `static`, framebuffer likely
double-buffered for display DMA), the desktop front end, and `romtest`
(stop exactly on `LD B,B`, run exact amounts of emulated time, read the last
complete frame, collect all serial output, read CPU registers). The
`Emulator` is about 48 KiB of RAM arrays plus a little state, so it must
not have to live on the stack. ROM bytes are read on every opcode fetch.

## Decision

**Cartridge ROM and RAM are borrowed for the emulator's lifetime:**
`Emulator<'a>`, with `load(&mut self, rom: &'a [u8], cart_ram: &'a mut [u8],
config: Config)`. On the MCU both are `'static` (memory-mapped flash, a
static RAM buffer). The caller sizes the RAM with `cart_ram_size(rom)` and
reads it back for saving through `cart_ram()`. The alternative, passing
the cartridge on every call, would thread an extra argument through every
bus access and allow a different buffer to be passed than the mapper state
belongs to. It is no faster: a stored slice is one load from `self`, the
same cost as a spilled argument.

**Frame and audio buffers are passed to every run call:**
`run_frame/run_cycles/run_until/step(&mut self, frame: &mut Frame, audio:
&mut [StereoSample])`. `Frame` is `[u16; 160 * 144]` RGB555 (bits 0-4 red,
5-9 green, 10-14 blue, the CGB palette layout). The caller can swap
framebuffers between calls and read them while the core is idle without
fighting a stored `&mut` borrow. Pixels go into the buffer of the current
call; it holds a complete frame when `FRAME_READY` is reported. Audio is
written from `audio[0]` each call, with the count in
`RunResult::audio_samples`, so the core keeps no position into a buffer it
doesn't own. An empty audio slice discards audio. The sample rate is fixed
in M6.

**Construction without a stack temporary:** `Emulator` has exactly two
fields: `mem: Memory`, which holds only `u8` arrays (so all-zero is a
valid value), and `hw: Hw<'a>`, everything else (under 512 bytes, checked
by a test). `Emulator::init(slot: &mut MaybeUninit<Self>) -> &mut Self`
zero-fills `mem` in place and writes `hw` normally. A compile-time
exhaustive pattern breaks the build if a field is added to `Emulator`
without updating `init`. The API is safe for callers: std code keeps a
`Box<MaybeUninit<Emulator>>` from `Box::new_uninit()` and uses the returned
reference (`Emulator` has no destructor, so dropping the box as
`MaybeUninit` is fine). The MCU uses a `static MaybeUninit<Emulator<'static>>`
in a no-init RAM section, which also keeps a large initial image out of
flash and `.data`. `const fn new()` exists for tests and simple statics.
Alternatives rejected: `Box::new(Emulator::new())` builds on the stack
first. `Box::new_zeroed` needs every field, including the `&[u8]` slices,
to be valid when zeroed, which is not true. A per-field
`unsafe fn init_in_place(*mut Self)` would need every new field
remembered by hand.

**`Config` (DMG compatibility palettes)** is passed to `load`, not `new`.
The model is fixed (CGB C, ADR 0003), and the palettes may differ per
title, which isn't known until a ROM is chosen.

**Time is counted in base clocks** (4,194,304 Hz, `CLOCK_HZ`) regardless of
double speed, matching the manifest. `clock()` is base clocks since load or
reset. `run_until(target)` runs until `clock() >= target`, or until a stop
event. Runs end on instruction boundaries, so they may overshoot by part of
an instruction; `RunResult::clocks` and `clock()` give the exact time.
Callers needing exact totals loop on absolute targets so overshoot never
accumulates. `run_cycles(n)` is `run_until(clock() + n)` and `run_frame()`
is `run_cycles(FRAME_CLOCKS)`. Ending mid-instruction would cost a
resumable CPU on the hot path, which no test needs.

**Events** are a hand-written bit set (`Events`, also `Buttons`; no
`bitflags`). Each run returns every event that occurred. `FRAME_READY`
(start of VBlank), `AUDIO_BUFFER_FULL` and `CPU_LOCKED` always end the
call (`Events::ALWAYS_STOP`): the caller must consume a buffer before it is
overwritten, and a lockup needs attention. `LD_B_B` and `SERIAL_TX` end the
call only if enabled with `set_stop_events`, so a game that executes
`LD B,B` costs the desktop nothing. Calls end right after the instruction
that raised the event. `CPU_LOCKED` fires once. Afterwards time and the PPU
keep running with the CPU stuck, as on hardware.

**Debug access** is plain field reads with no hooks on the hot path:
`registers() -> Registers`, `peek(addr) -> u8` (side-effect free),
`clock()`, `serial_byte()` (the last byte sent, valid after `SERIAL_TX`),
`lcd_enabled()`, `cpu_locked()`. Serial output uses an event plus the last
byte instead of a buffer, so the core needs no storage sized for the
worst case. A caller that wants every byte enables the `SERIAL_TX` stop.
At most one transfer can complete per instruction, so no byte is lost.

**RTC**: `rtc_state() -> Option<RtcState>` and `set_rtc_state()`. The caller
persists it and may advance it to wall-clock time on load. The shape is
provisional until M4.

## Consequences
- The emulator type carries a lifetime. Desktop and `romtest` keep the ROM
  and RAM alive in the scope that owns the boxed emulator.
- Every run call takes the frame and audio buffers, even `step`.
- `romtest` gets the "last fully drawn frame" by keeping its own copy (or
  swapping two buffers) on each `FRAME_READY`, and treats `!lcd_enabled()`
  at exit as solid white.
- Any new top-level field in `Emulator` must go into `Memory` (if it is all
  integers) or `Hw`. The build fails until `init` accounts for it.
