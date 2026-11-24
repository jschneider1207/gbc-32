# Architecture

This is the living description of how the code is structured. Update it when
the structure changes. The reasons behind decisions belong in
`docs/decisions/`; this file describes the current shape.

> Status: the workspace and the core's public API (ADR 0006) are concrete as
> of M0; the CPU, memory bus, timer and serial port as of M1 (ADR 0008).
> PPU mode timing and the DMG-compatibility renderer as of M3 (ADR 0014); the APU
> (four channels, frame sequencer, 32,768 Hz output) as of M6 (ADR 0021). Targets
> (dev board, then custom board) as of 2026-11-21.

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
(every `unsafe` block needs a `// SAFETY:` comment). `gbc-core` itself has
no `unsafe` at all: `#![forbid(unsafe_code)]`, checked by
`crates/gbc-core/tests/hard_rules.rs` (ADR 0009). `unsafe` belongs in front
ends and platform layers only.

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
| ITCM (fast code RAM) | 64 KB, fixed | 64 KB by default; configurable to 128, 192 or 256 KB, taken from AXI SRAM (option byte `TCM_AXI_SHARED[1:0]` in `FLASH_OPTSR2_PRG`, RM0468 §2.4 Table 8, §4.9.25). Plan: 128 KB ITCM / 256 KB AXI SRAM |
| Core code placement | Part of gbc-core in ITCM (`perf/itcm-sections.txt`), the rest in flash behind the I-cache | All of gbc-core in ITCM |
| Code-size gate | none of its own: the H753 ITCM set's fit is reported (`scripts/check-itcm-set.sh`); this board may run slower (human, 2026-11-24) | gbc-core total `.text` <= 122,880 B (128 KiB minus an 8 KiB platform reserve; provisional), `scripts/check-code-size.sh` |
| ROM source | Shield's QSPI NOR, memory-mapped | eMMC → copied at load into external HyperRAM on OCTOSPI, memory-mapped (human decision 2026-11-24; hardware ADR 0001 pending update) |
| Saves | (Stage 2 choice) | eMMC |
| Display | Shield's ILI9341V, 8-bit 8080 bus on FMC (stand-in only) | TBD by the hardware repo (FMC parallel or LTDC) |
| Audio | (Stage 2 choice) | SAI → external DAC/amplifier |

What carries over from Stage 2: proof that the core runs at full speed on a
Cortex-M7, the frame pacing / audio / input structure, and the cache and
memory-placement findings. The latter are a starting point only, because
the RAM layout and cache sizes differ. Reading the ROM from QSPI NOR is a
conservative stand-in for HyperRAM. The custom-board contract is
`../gbc-32-hardware/docs/firmware-interface.md`.

## Core ↔ platform boundary

The core is a library the platform drives. It never calls out to the
platform. The concrete API and its rationale are in ADR 0006 (construction:
ADR 0009; save persistence: ADR 0015 and 0032; unsupported mappers: ADR
0031); the
crate docs in
`crates/gbc-core/src/lib.rs` have desktop and
MCU usage examples.

```text
Memory                             // the ~49 KiB of RAM arrays, provided by the caller
  const fn new() -> Self                            // all zero bytes: a static goes to .bss
Emulator<'a>                       // 'a = lifetime of the Memory and cartridge ROM/RAM borrows
  fn new(mem: &'a mut Memory) -> Self               // powered off, no cartridge; zero-fills mem
  fn load(&mut self, rom: &'a [u8], cart_ram: &'a mut [u8], Config)
      -> Result<Option<EjectedCart<'a>>, LoadError<'a>>  // Ok: previous cart (as eject); Err: hands back rom/cart_ram
  fn eject(&mut self) -> Option<EjectedCart<'a>>    // unload; back to the `new` state
  fn set_dmg_palettes(&mut self, DmgPalettes)       // no reset
  fn reset(&mut self)                               // power cycle; cart RAM/RTC kept
  fn set_buttons(&mut self, Buttons)
  fn set_buttons_at(&mut self, clock: u64, Buttons) -> bool  // lands on its M-cycle; false = queue full (ADR 0012)
  fn set_stop_events(&mut self, Events)             // opt-in stops: LD_B_B, SERIAL_TX
  fn run_frame (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_cycles(&mut self, clocks: u64, &mut Frame, &mut [StereoSample]) -> RunResult
  fn run_until (&mut self, clock: u64,  &mut Frame, &mut [StereoSample]) -> RunResult
  fn step      (&mut self,              &mut Frame, &mut [StereoSample]) -> RunResult
  // debug: clock(), registers(), peek(addr), serial_byte(), lcd_enabled(), screen_blank(), cpu_locked()
  // cartridge: cart_ram(), cart_info(), rtc_state(), set_rtc_state(),
  //            huc3_rtc_state(), set_huc3_rtc_state(),
  //            save_dirty(), clear_save_dirty()
RunResult { events: Events, clocks: u64, audio_samples: usize }
cart_ram_size(rom) -> Result<usize, LoadErrorKind>
cart_info(rom) -> Result<CartInfo, LoadErrorKind>  // before load: mapper? battery? RTC? RAM size
CartInfo { mapper: MapperKind, ram_size, battery, rtc, huc3_rtc }, fn has_save()  // has_save = battery && (RAM || a clock)
MapperKind { RomOnly, Mbc1, Mbc2, Mbc3, Mbc5, HuC1, HuC3,   // emulated
             Mmm01, Mbc6, Mbc7, PocketCamera, Tama5, Unknown }  // refused at load
  fn from_cart_type(u8) -> Self, fn is_supported(self) -> bool, fn name(self) -> &'static str
EjectedCart<'a> { rom, cart_ram, rtc: Option<RtcState>, huc3_rtc: Option<Huc3RtcState>, save_dirty }  // cart_ram untrimmed
  fn save_ram(&self) -> &[u8]                       // the header-sized part (the save file)
  fn into_buffers(self) -> CartBuffers<'a>
type CartBuffers<'a> = (&'a [u8], &'a mut [u8])     // RAM buffer as passed (untrimmed)
LoadError<'a> { kind: LoadErrorKind, rom: &'a [u8], cart_ram: &'a mut [u8] }
LoadErrorKind { RomTooSmall { len }, CartRamTooSmall { needed, len },
                UnsupportedMapper { mapper: MapperKind, cart_type: u8 } }

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
- **Mapper:** resolved once at load from the cartridge type (0147), plus
  the ROM contents for MBC1 multicarts (ADR 0016). Emulated: no mapper,
  MBC1 (and MBC1M), MBC2, MBC3/MBC30 (with the RTC), MBC5, HuC1 and HuC3
  (`docs/reference/hudson-mappers.md`; their infrared ports see no light
  and the HuC3 speaker and alarm do nothing). A header that names a
  mapper the core does not emulate (MBC6, MBC7, MMM01, TAMA5, Pocket
  Camera) or an unassigned type code is refused:
  `load`, `cart_ram_size` and `cart_info` return
  `LoadErrorKind::UnsupportedMapper`, which names the mapper, so a front
  end can tell the user before it touches a save file (ADR 0031). It
  never runs as "no MBC".
- **Cartridge RAM:** a `&'a mut [u8]` owned by the caller, at least
  `cart_ram_size(rom)` bytes (up to 128 KiB). Only the header's size is
  used; `cart_ram()` (the save file) is exactly that long. The caller
  persists it, reading it through `cart_ram()` while the emulator holds the
  borrow, and gets the whole buffer back from `eject`, a later `load`, or a
  `LoadError`. Only battery-backed cartridges have a save
  (`cart_info(rom).has_save()`, usable before `load`). `save_dirty()` is
  set when the game changes a RAM byte or writes an RTC register; the
  caller saves and calls `clear_save_dirty()`, so the MCU writes flash
  only when needed. MBC2 RAM is stored as read, `0xF0 | nibble`.
- **RTC:** state is advanced by emulated time, lazily: caught up on
  register writes, the latch, `rtc_state` and reset
  (`docs/reference/mbc3-rtc.md`). The caller can read and write the RTC
  state (`rtc_state`/`set_rtc_state`) to save it. To sync to wall-clock
  time on load: `load`, then
  `set_rtc_state(saved.advance_seconds(elapsed))`
  (`RtcState::advance_seconds`: halt-aware, hardware rollover, day
  carry). `eject` and `load` over a cartridge return its RTC state at
  removal in the `EjectedCart`, so a game switch never loses it.
  The HuC3 clock (a minute-of-day and a day counter behind a command
  mailbox) works the same way through its own type and calls:
  `huc3_rtc_state`/`set_huc3_rtc_state`, `Huc3RtcState::advance_seconds`,
  `EjectedCart::huc3_rtc` (ADR 0032).
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
  the core writes from index 0 and reports the count (ADR 0021 sections
  8-9). The rate is fixed at 32,768 Hz in both CPU speeds
  (`AUDIO_SAMPLE_RATE`; `AUDIO_SAMPLE_CLOCKS` = 128 base clocks per
  sample). Each sample is the box-filtered average of the mixed signal
  over its window, 64 per unit of mixed level: full scale ±30,720, no
  clamping, DC kept (`StereoSample` rustdoc, `apu/mix.rs` `sample`).
  When the last slot's window completes, `AUDIO_BUFFER_FULL` ends the
  call at the next instruction boundary, or at once inside a VRAM DMA
  stall (the next call resumes the stall), so no sample is dropped and a
  partial window carries over: for non-empty buffers, the stream and all
  state are independent of how the caller splits calls and buffers. A
  call of `n` clocks completes at most `n / 128 + 2` samples (550 for
  `run_frame`). An empty buffer discards audio: the APU only advances,
  and the next call with a buffer restarts the window grid at its start.
  Resampling to the device rate and DC blocking (the high-pass filter)
  are the platform's job.
- **Input:** `set_buttons(Buttons)` (now) and `set_buttons_at(clock, Buttons)`
  (scheduled on the `Input` deadline, exact to the M-cycle, up to
  `SCHEDULED_INPUTS` pending; ADR 0012), where `Buttons` is a hand-written bit set
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

The large RAM arrays (WRAM 32 KiB + VRAM 16 KiB + OAM, etc.) are the
caller's `Memory`, which must never be required to live on the stack
(ADR 0009). `Memory::new()` is all zero bytes, so on the MCU a
`static MEM: Memory` goes to `.bss` (no flash image, no boot copy); the
desktop and `romtest` use `Box::new(Memory::new())`. `Emulator` holds
`mem: &'a mut Memory` and `hw: Hw<'a>` (everything else; 896 B with the pointer on thumbv7em, printed by `scripts/check-code-size.sh`, no fixed cap)
and is small enough for the stack. Run calls copy the `mem` pointer into
the per-call `Ctx`, so the hot path is the same as with an inline field.
The APU's internal state is `Memory::apu` (all zero = powered off, at most 128 B, checked by a test; ADR 0021 section 2); its register bytes stay in `Memory::io`. The renderer's large line scratch lives in `Memory` too: the object line buffer `Memory::obj_buf` (`ppu::ObjBuf`: the line's object pixels and their drawing-priority ranks, 352 B), which the renderer gets as one `&mut` per `render` call, and the BG pixel-pair tables `Memory::bg_pairs` (512 B). The per-line fetcher state (held fetches, the object fetch cursor, the TILE_SEL glitch latch, the window zero pixel) is not there: it belongs to `Renderer`, in `Hw`. `Memory::clear` (`new`, `load`, `reset`, `eject`) restores exactly `Memory::new()`, checked field by field by a test.
New large state goes in `Memory` as fields whose power-on value is zero
(non-zero post-boot values are written by `reset`); everything else goes in
`Hw`.

Core unit tests live in `#[cfg(test)]` modules, which may use `std`. The
gate only forbids `alloc` in `gbc-core/src`, and the `thumbv7em` build
enforces `no_std` for non-test code.

## Timing model

See ADR 0002. In short:
- The CPU is the master clock (ADR 0008). The per-M-cycle tick is only a
  clock countdown; the timer, serial bit clock (and the DIV-APU edge)
  are computed from the clock on access and act through deadlines in the
  event table (ADR 0011). A serial transfer has one deadline, its
  completion; the bits before it are shifted on access.
- The PPU and APU are lazily caught up to the current cycle whenever one of
  their registers is written, and at scheduled events (PPU: STAT interrupt
  edges, VBlank, each visible line's mode-3 end; APU: `DivApu` and
  `AudioBuffer`, below). PPU register reads are computed from the clock.
  APU register reads are pure (they advance a copy of the channel).
- PPU mode timing is exact (ADR 0014, below). Pixel output is drawn a
  scanline at a time, with
  a mid-line flush on register writes.

## Module sketch (`gbc-core`)

All modules listed exist.

```
lib.rs      * Emulator, Hw (cpu + sys), RunResult, run loop, public re-exports, sm83 test API
memory.rs   * Memory: the large RAM arrays (WRAM, VRAM, OAM, HRAM, FEA0 area, stub I/O storage)
bitset.rs   * bitset! macro behind Buttons and Events
events.rs   * Events
schedule.rs * event table (ADR 0011): EventKind (dispatch order), Schedule (one u64 deadline per kind + their minimum)
joypad.rs   * Buttons, P1 line logic, the joypad interrupt edge, InputQueue (scheduled changes, ADR 0012; P1/JOYP register access is in bus.rs)
config.rs   * Config, DmgPalettes, RGB555 helpers
cpu/        * SM83: Registers, Bus trait, Cpu (run loop, step, interrupts, HALT/HALT bug, STOP, STOP mode)
  exec.rs   *   opcode tables: per-opcode monomorphized bodies behind a 256-arm match
bus.rs      * Sys (state the bus reaches), Hot (clock countdown), Ctx (per-run-call context), memory map, I/O, peek
timer/      * DIV/TIMA/TMA/TAC, lazy (ADR 0011): u64 system counter from the clock, TIMA reload as the Timer event, serial edge clocks
  reference.rs  (test only) the eager per-M-cycle model; differential.rs checks the lazy one against it
ppu/        * Frame, mode timing (ADR 0014): LY/STAT/LYC, STAT and VBlank interrupts, mode-3 length, VRAM/OAM blocking windows
  objects.rs *   OAM scan (`LineObjects`: up to 10 per line) and DMG-style drawing priority
  stalls.rs  *   `Stalls`: one line's mode-3 pipeline stalls (objects, window starts), a bounded list and the one timed walk over it
  render.rs  *   scanline renderer (`Renderer`, spans of a line), resolved palettes (`Colors`, ADR 0013); CGB mode in M5
apu/        * Apu (ADR 0021): power, frame sequencer (`Fs`), `sync`, register write entry, pure reads (`read_apu`), StereoSample, output constants
  units.rs  *   Length and Envelope, shared by the channels that have them
  square.rs *   Square (CH1 and CH2, one impl) and Sweep (CH1)
  wave.rs   *   Wave (CH3): period, wave RAM position, cached waveform sum
  noise.rs  *   Noise (CH4): LFSR as its output sequence (word-parallel steps)
  mix.rs    *   Output: window integrator, NR50/NR51 weights, buffer deadline
dma.rs      * OAM DMA: transfer state (`OamDma`, owned by `Ppu`, driven by the bus), CGB C conflict table, what the PPU sees (`ppu_at`)
hdma.rs     * VRAM DMA (GDMA/HDMA): `VramDma`, block copy, the resumable CPU stall (ADR 0021 GDMA amendment)
cart/       * header (cart_ram_size), RtcState, ROM bank windows, MBC1/2/3(+RTC)/5, HuC1/HuC3 (huc3.rs: the HuC3 clock chip)
serial.rs   * SB/SC internal-clock transfers, lazy (ADR 0011): shifts caught up on access, completion as the Serial event, last byte sent
```

### CPU and bus (ADR 0008)

`Hw` holds `cpu: Cpu` and `sys: Sys<'a>` (everything the bus reaches:
clock, PPU, timer, serial, cartridge, interrupt and banking registers).
Each run call builds one `Ctx` (`Ctx::new`: the memory, `sys`, the frame
and audio buffers, the stop events and the target) and calls
`cpu.run(&mut ctx)`, which loops over instructions until the context
reports the call done (target reached or a stop event raised); the opcode
dispatch is inlined into that loop. The CPU is generic over the `Bus`
trait; every `read`/`write`/`idle` is one M-cycle: the access at the
current clock, then a tick: only an `i32` count of the M-cycles to the
next deadline (the first M-cycle boundary at or after the earlier of the
event table's minimum and the target), which the tick increments towards
zero (`Hot::t_minus`).
The countdown (`Hot`) is copied into the `Ctx` for the call
and back into `Sys::hot` when it is dropped, so the tick doesn't go
through `ctx.sys`; the u64 clock is `deadline + t_minus * mcycle`. Scheduled work
lives in the event table `Sys::sched` (ADR 0011; `schedule.rs`): one u64
deadline per `EventKind` (`NEVER` = none) and their minimum, maintained
only when a deadline is set (incrementally: the table is rescanned only
when the earliest entry moves later or is taken), never per M-cycle. When the countdown runs
out, `Ctx::boundary` runs every kind due at the current clock in
`EventKind` declaration order (fixed, so same-clock events are
deterministic), each handler setting its next deadline, then reschedules
the countdown; register writes that move a deadline (LCDC, SC, DIV, TIMA,
TMA, TAC, STOP) reschedule too. A halted, stopped, locked-up or powered-off CPU
idles through `Bus::idle_halted`, which `Ctx` implements as one skip
straight to the next deadline (`-t_minus` M-cycles, at least
one, then `boundary`): only a deadline can change `IE & IF`, a stop
wake-up or the end of the call, so the wake-up lands on the same M-cycle
as idling one M-cycle at a time. The CGB speed-switch pause skips the same
way, a deadline at a time. `Emulator::step` targets `clock + 1`, so its
halted step stays one M-cycle. On the event table so far: the TIMA reload (`Timer`),
a serial transfer's completion (`Serial`; its earlier bit shifts are
computed from the system counter when SB or SC is accessed or DIV is
reset, `Serial::catch_up`), the PPU (`Ppu`: STAT interrupt edges,
VBlank and each visible line's mode-3 end) and scheduled
button changes (`Input`, `Sys::inputs`, ADR 0012). The timer
(`Sys::timer`) keeps an unwrapped u64 system counter as a linear function
of the clock (rebased on DIV writes and speed switches), computes DIV and
TIMA from it on reads, and stores the overflow clock and the last reload
clock for the cycle A/B window; timer, serial and DIV writes go through
the out-of-line `Ctx::write_timer_io` and reads through `Timer::read`
and `Serial::read_sb`, off
the inlined bus path. `Sys` is `repr(C)` with the
fields the inlined bus path reads first, to keep their Thumb-2 load
offsets small (the event table at the front cost `Cpu::run` about 1 KB).
Reads have no
side effects, so the CPU and `Emulator::peek` share one `read` function
(given the live `Hot`). The same CPU runs against a flat test bus in
`tools/sm83-tests`, which runs only with the data from
`scripts/fetch-sm83-tests.sh`, verified file by file against the committed
`tools/sm83-tests/sm83-v1.sha256` (`GBC_REQUIRE_TEST_DATA=1` makes missing
data a failure instead of a skip).

### APU (ADR 0021)

`apu/`. State is `Memory::apu` (`Apu`: `Output`, `Fs`, `[Square; 2]`,
`Sweep`, `Wave`, `Noise`); nothing runs per M-cycle. `Apu::sync(now, io,
audio, len)` advances every channel exactly in base clocks from
`Output::synced` to `now` and, for a non-empty buffer, integrates the span
into samples. It is called (out of line, `Ctx::apu_sync`) at:

- every APU register write, FF10-FF3F (`Ctx::write_apu`: sync, then the
  write; NR52 power via `write_nr52`);
- each `EventKind::DivApu` deadline (`div_apu_event`: sync, then the
  frame-sequencer step), the falling edge of the system counter's bit 12
  (13 in double speed); a DIV write or STOP that is such an edge steps it
  at once (`apu_div_reset`); a speed switch and reset recompute the
  deadline (`Sys::set_div_apu_deadline`); it is `NEVER` while off;
- the `EventKind::AudioBuffer` deadline (`audio_buffer_event`: sync, raise
  `AUDIO_BUFFER_FULL`), set at call start (`start_audio`) where the last
  free slot's window ends, `NEVER` for an empty buffer;
- the end of every run call (`Ctx::finish`).

Reads are pure: FF10-FF25 are `mem.io | mask`; NR52, wave RAM and
PCM12/PCM34 go through `read_apu`, which advances a copy of the channel to
the access clock. The invariant is that register-visible state is a
function of the clock and the register writes only, never of where syncs
happened or whether a buffer was passed (unit tests split calls randomly;
`bench --fingerprint --audio-buffer` checks the whole machine).

Output: one stereo sample per 128 base clocks (32,768 Hz), the exact
average of the mixed signal over the window; the core keeps DC and does no
filtering. Hardware rules and the SameBoy count conversions are in
`docs/reference/apu.md`; cost and code size in `docs/perf.md` ("APU cost").

### PPU timing (ADR 0014)

`ppu/mod.rs`. The PPU keeps `frame_start` (the clock of frame position 0;
LCD enable is position 7, the post-boot state line 144 dot 163) and
computes LY, the STAT mode and coincidence bits and the VRAM/OAM blocking
windows from the position `now - frame_start` on every read, without side
effects. Positions are SameBoy's nominal action times converted so that
the state at a position includes the actions at it (an M-cycle boundary in
single speed is at position 3 mod 4).

The STAT interrupt line is walked over *update points*, 3-5 per line
(`Pt`: LY change and mode 2 at `t = 3, 4`, the pipeline start `Latch` at
89, mode-3 `End`, the HBlank source at `End + 1`, the VBlank-line and
line-153 comparison points), with SameBoy's `mode_for_interrupt`,
`ly_for_comparison` and held LYC-line semantics; rising edges request IF
bit 1. `Ppu::sync(now)` commits the points up to `now`; `Ppu::next_event`
walks ahead without committing to the first point that requests an
interrupt, completes the frame (VBlank, `FRAME_READY`) or ends a visible
line's mode 3, which is the `EventKind::Ppu` deadline. With no STAT source
selected the walk skips to `Latch`/`End`/`VBlank` only.

IF (`Sys::iflags`) holds bits 0-4 and nothing else, so the CPU's
interrupt checks are `IE & IF` with no mask. A request that reaches IF at
a later clock than the point or write that makes it (single speed: a LYC
write's request, line 0's mode-2 pulse) waits as "bits B at clock T" in
`Ppu::irq` (`irq::PendingIrq`, ADR 0033): `Ppu::next_event` is at most T,
and `Ppu::sync` to T or later posts it, so IF never depends on when the
PPU was last synced. Rules: `docs/reference/stat-irq.md`, "Timed
requests".

Mode 3 of a visible line is latched at its pipeline start: 172 dots plus
`SCX % 8`, 6 for the window and 6-11 per object (`object_stalls`, Pan Docs
"OBJ penalty algorithm"), as `LineTiming` with the per-pixel stalls so
`Ppu::mode3_pixel(now)` can say which pixel is being drawn. The stalls
are a `Stalls` list (`ppu/stalls.rs`: at most 11, the window's as an
`Option`), and everything that needs a stall's place in the line walks
it through one iterator (`Stalls::timed`, `LineTiming::walk`). Objects come
from one OAM scan per line (`objects.rs`, `LineObjects::scan`) that feeds
both the stalls and the renderer, skipped for lines a cached object-line
map (`ObjectLines`, invalidated by `Ppu::oam_written` on CPU and DMA OAM
writes) shows empty, and on the first line after LCD enable (no OAM scan).

Every PPU register write goes through `Ctx::write_ppu`: sync first, then
the write (STAT in two phases one dot apart, LYC one dot into the M-cycle
in single speed, as SameBoy's conflict maps), then reschedule. CPU VRAM
and OAM accesses check `Ppu::vram_blocked`/`oam_blocked` out of line
(`read_vram`, `Ctx::write_vram`, `read_oam`, `Ctx::write_oam`); `peek` and
the OAM DMA's reads use `read::<false>` and see memory as it is.

### OAM DMA as the PPU sees it

`docs/reference/oam-dma.md` has the hardware rules. The transfer's state
(`dma::OamDma`) lives in the `Ppu` (`Ppu::dma`): the bus drives it (FF46
write, HALT pause and wake-up, CPU conflicts, lazy copying) and both read
it, so the PPU's sync and draw calls take no extra arguments.
`OamDma::ppu_at(clock)` says what a PPU OAM read at a clock sees (off,
running, halted) and how many bytes are written by then; it is a pure
function of the state since its last change. So before each change (an
FF46 write, a halt during a transfer) the bus calls `Ctx::dma_ppu_event`:
the PPU syncs, scans the OAM entries read so far (`Ppu::scan_to`, keeping
the partial scan in `Ppu::scan`), fixes the attributes of the objects
fetched so far and draws the line up to now (`Ppu::fix_attrs`, `flush`).
The wake-up from HALT needs no sync: it changes nothing up to it.

A line is DMA-aware from the FF46 write (`OamDma::ppu_live`) until the
first line that starts after the transfer's end as the PPU sees it
(`Ppu::dma_touches` clears the flag), so the normal path costs one flag
test per line. On an aware line the OAM scan runs entry by entry in runs
of equal state, tracking the mode-2 bus (`Ppu::bus`), and the objects' tile
index and attributes come from `Ppu::obj_attr` (passed to the renderer as
`Sources::attrs`), filled lazily at each draw from the clock of each
object's fetch in its stall. OAM bytes the transfer hasn't written yet are
read from `Memory::oam_dma_old` (OAM at the FF46 write); the written ones
are in `mem.oam`, because the bus copies the bytes due before every PPU
sync.

### Scanline renderer

`ppu/render.rs`, driven by the PPU (`Ppu::renderer`); pixels go straight
into the caller's `Frame` row by row. The `Latch` point starts the line
(`Renderer::start_line`: SCX & 7, the line's objects in drawing order),
the `End` point draws whatever of the line is left up to pixel 160
(`Ppu::draw`), and `Ppu::next_frame` and LCD enable start a frame (window
line counter and WY condition; the renderer's WY condition is the only
copy, which the window stall reads too). `Ppu::drawn` counts the pixels
of the current line already in the frame.

The renderer owns the fetcher's per-line state for mid-mode-3 writes
(`docs/design/mid-line-ppu.md`): the held parts of the line (`Held`:
pixels fetched before a map-select, tile-data, SCX or SCY write, drawn
with the old values, and which planes of the last tile were read after
it, `Mixed`), the object fetch cursor, the CGB TILE_SEL glitch latch
(`SelLatch`) and the window zero pixel. The timing code decides them
(`Ppu::hold_fetch`, `Ppu::window_write`) and hands them over
(`Renderer::push_hold`, `sel_set`/`sel_clear`, `set_zero`); `Memory` only
supplies registers, VRAM, OAM and the object line buffer.

Mid-line writes (ADR 0002 catch-up): `Ctx::write_ppu` for LCDC, SCY, SCX,
BGP, OBP0, OBP1, WY and WX calls `Ppu::flush(at)` before the write lands,
which draws the line up to `mode3_pixel(at)` with the old value. `at` is
where the write lands in the M-cycle (SameBoy `cgb_conflict_map`, single
speed): the access clock for LCDC/SCY/SCX/WY, one dot earlier for the
palettes (CGB C), one dot later for WX. Drawing at the mode-3 end is exact
for VRAM and OAM because the CPU can't write them during mode 3 (and OAM
during mode 2, when the scan happens); OAM DMA is the exception, below.

Registers: LCDC lives in `Ppu`; SCY, SCX, WY, WX and the raw BGP/OBP0/OBP1
stay in the `mem.io` backing store (reads unchanged on the bus path), and
the renderer reads them through `Ppu::regs` when it draws. Colors are
resolved when written (ADR 0013): `Ppu::colors` holds RGB555 tables, rebuilt
on BGP/OBPx writes, at reset and by `Emulator::set_dmg_palettes`. Spans
without object pixels go straight from tile bits to RGB555; others mix an
index buffer with the object buffer. `frame_done` no longer writes the
frame; `screen_blank` and the LCD-off rules are unchanged (ADR 0006
amendments). Cost and code size: `docs/perf.md`. CGB-mode rendering
(attributes, VRAM bank 1, CGB palettes, OAM-index priority) is M5.
