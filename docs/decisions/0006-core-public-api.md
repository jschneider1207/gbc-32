# 0006: Core public API (buffers, construction, events, debug access)

- Status: Accepted; the construction part (`Emulator::init` over `MaybeUninit`, `ZeroValid`) is superseded by 0009
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

## Amendments

### 2026-11-21: returning cartridge buffers, trimming, palettes, construction

An API review found that buffers lent to `load` could not be returned, and
a few construction and documentation gaps. Changes:

- **Buffers always come back.** `eject(&mut self) -> Option<CartBuffers<'a>>`
  unloads the cartridge, returns the ROM and the RAM buffer, and puts the
  emulator back in the state `init` leaves it in. `load` now returns
  `Result<Option<CartBuffers<'a>>, LoadError<'a>>`: on success the previous
  cartridge's buffers (if any), on failure a `LoadError { kind, rom,
  cart_ram }` holding the caller's buffers (the emulator is unchanged). The
  old error enum is now `LoadErrorKind`, which `cart_ram_size` returns.
  Without this, switching games on the MCU would need a second `&'static
  mut` to the same save buffer. `CartBuffers<'a>` is
  `(&'a [u8], &'a mut [u8])`.
- **Cartridge RAM is trimmed to the header size.** The cart keeps the whole
  buffer but uses only `cart_ram_size(rom)` bytes, so `cart_ram()` and all
  mapper behavior depend only on the header. `eject` (and the other paths
  above) return the whole, untrimmed buffer, so a caller with one static
  buffer sized for the largest game loses nothing. (Storing a trimmed slice
  plus the remainder would need `unsafe` to rejoin them.)
- **Unknown RAM size codes (0149) mean no RAM** instead of a load error,
  as in SameBoy (`Core/mbc.c`, `GB_configure_cart`). `UnknownRamSize` is
  gone.
- **`set_dmg_palettes(DmgPalettes)`** changes palettes without the reset
  `load` performs. `Config` is still passed to `load`.
- **Owned box for std callers.** The core has no `alloc`, so it cannot
  return a `Box`. The documented pattern is `Emulator::init(&mut slot)`
  followed by `unsafe { slot.assume_init() }` (sound because `init` fully
  initializes the value). Using the reference returned by `init` remains
  the no-`unsafe` option.
- **No `Default` for `Emulator`.** It built the ~49 KiB value on the stack.
  `const fn new()` is documented as for `const`/`static` initializers
  only. The MCU example now uses `static mut MaybeUninit<Emulator<'static>>`
  with `&raw mut` (edition 2024), or a static-cell pattern in the firmware.
- **`Memory` all-zero invariant is enforced**: every field must implement
  a crate-private `unsafe trait ZeroValid` (integers and arrays of them),
  checked by an exhaustive pattern at compile time, and a test compares
  zero-initialized memory with `Memory::new()` and with a dirtied-then-reset
  emulator.
- **`Hw` size limit raised from 512 bytes to 1 KiB.** `init` builds `Hw` on
  the stack once; the limit exists to protect the MCU's small stack (a few
  KiB, shared with interrupt handlers), and 1 KiB leaves room for PPU line
  buffers, APU and mapper state without awkward splits.
- **Frame buffer swapping is documented** on `run_until` and `Frame`: swap
  only after a `FRAME_READY` result, or the frame is torn across two
  buffers. If the LCD is turned off mid-frame, that frame is abandoned
  without `FRAME_READY`, the buffer contents are unspecified, and the
  screen should show white until the next `FRAME_READY`.

### 2026-11-21: `screen_blank()`

Added `Emulator::screen_blank() -> bool`: true while the LCD is off, and
after it is turned back on until the next `FRAME_READY`. It is a flag set
when the LCD is turned off and cleared when a frame completes, so it is
exact even when the LCD goes off and on within one run call, which polling
`lcd_enabled()` after each call can miss. Front ends and `romtest` show
white while it is true. (Requested by the romtest integrity review; see
ADR 0007, amendments.) Additive only; nothing else in the API changed.

### 2026-11-21: M0 hardware review (frame timing, LCD enable, RTC, short ROMs)

The M0 milestone-close hardware review found four places where the
skeleton or the API documentation disagreed with the hardware. Changes:

- **`FRAME_READY` fires at the start of VBlank** (LY=144, 144 x 456 =
  65,664 clocks into the frame; Pan Docs "LCD Status"), as the `Events`
  documentation already said, instead of at the end of the frame (70,224).
  The frame period is still `FRAME_CLOCKS` = 70,224. After `load`/`reset`
  the first frame is ready at clock 65,664, then every 70,224 clocks. The
  PPU still starts at LY=0 dot 0 after a reset; starting at the phase where
  the CGB boot ROM hands over is an M3 task (`TODO(M3)` in `ppu/mod.rs`).
- **The first frame after the LCD is turned on is not displayed.**
  `screen_blank()` now stays true through the first `FRAME_READY` after an
  LCD enable and is cleared only by the second (Pan Docs LCDC.7; SameBoy
  `Core/display.c`: enabling the LCD sets `frame_skip`, and
  `GB_display_vblank` outputs a blank frame instead). `FRAME_READY` is
  still reported for that frame, since its timing is real; front ends must
  check `screen_blank()` after each `FRAME_READY` rather than assume the
  frame is shown. This also applies when the LCD was off since power-on.
- **`set_rtc_state` masks to the register widths** (Pan Docs MBC3: S and M
  6 bits, H 5 bits, DL 8 bits; DH bit 0 is day bit 8, bit 6 halt, bit 7
  day carry, the other DH bits read 0) and clamps `subsecond_clocks` to
  `CLOCK_HZ - 1`. `RtcTime` fields are documented as raw register bits:
  values such as S=60-63 are legal and wrap at the register width without
  carrying. New: `RtcTime::masked` and `RtcState::masked` (what
  `set_rtc_state` stores).
- **ROMs shorter than 32 KiB load.** `MIN_ROM_SIZE` is now 0x150 (enough
  for the header) instead of 0x8000, as SameBoy (`GB_load_rom`) accepts
  short files and pads them with 0xFF. Reads past the end of a short ROM
  return 0xFF; `peek` already does, and the M1 bus must too.
  `LoadErrorKind::RomTooSmall` now means "can't hold the header".

### 2026-11-22: save persistence (ADR 0015)

`load` and `eject` now return an `EjectedCart` (the buffers plus the RTC
state at removal and the save-dirty flag; `into_buffers()` gives the old
tuple), and the API gained `cart_info`, `save_dirty` and
`clear_save_dirty`. "Buffers always come back" stands and now covers the
RTC state too. Details and rationale: ADR 0015.

### 2026-11-22: the audio sample rate (ADR 0021)

The rate "fixed in M6" is **32,768 Hz** stereo (`CLOCK_HZ / 128`), the
same in both CPU speeds. New public constants: `AUDIO_SAMPLE_RATE`
(32,768) and `AUDIO_SAMPLE_CLOCKS` (128 base clocks per sample). Each
sample is the average of the mixed signal over its 128-clock window; the
core keeps DC, and resampling and the DC-blocking high-pass filter are the
front end's job. The buffer contract is unchanged: samples are written from
`audio[0]`, `RunResult::audio_samples` counts them, and an empty slice
discards audio (the APU only advances; the next call with a buffer
restarts the window grid). When the last slot's window completes,
`AUDIO_BUFFER_FULL` ends the call at the next instruction boundary, or at
once if the CPU is inside a VRAM DMA stall: that is the one place a run
call ends between instruction boundaries, and the next call resumes the
stall before the CPU runs (`run_until` and `step` docs; the stall also
stops at the call's target, so a GDMA no longer overshoots it by up to
4,100 clocks). So no sample is ever dropped and a partial window carries
over: for any non-empty buffers, the samples and all state are identical
however the caller splits calls and buffers. A call of `clocks` completes
at most `clocks / 128 + 2` samples (the carried partial window and the
overshoot can each complete one), so that size never fills (550 for
`run_frame`). Sample scale: 64 per unit of mixed level, full scale
±30,720, DC kept (`StereoSample` rustdoc). Details and rationale: ADR
0021 sections 8-11 and its GDMA stall amendment.
