# Architecture

This is the living description of how the code is structured. Update it when
the structure changes. The reasons behind decisions belong in
`docs/decisions/`; this file describes the current shape.

> Status: checked against the code on 2026-11-24 (Stage 1, M10: after the
> `bus/` split, the `Speed` enum, the PPU line timeline table and the
> budget reform). Targets (dev board, then custom board) as of 2026-11-21.

## Workspace

```
Cargo.toml                 workspace
rust-toolchain.toml        pinned stable toolchain + thumbv7em-none-eabihf
crates/
  gbc-core/                #![no_std], no alloc. All emulation logic.
  gbc-desktop/             std front end (Stage 1 only): window, audio, input, saves
  gbc-firmware/            #![no_std], no alloc, forbid(unsafe) (tests/hard_rules.rs), host-tested board-independent firmware; depends on gbc-core only (not gbc-link); ADR 0036. M1: CycleClock trait, scripted input (input.rs), FNV-1a, the headless loop and its line format (headless.rs). Later: main loop and pacing, RGB555 converter/scaler, audio conditioning, input debounce/mapping, stats, ROM image header; boards plug in drivers via traits
  gbc-link/                (Stage 2, not yet created) #![no_std], no deps, no unsafe, host-tested serial protocol (ROM load, save load/dump, PC input, stats); transport-agnostic
  gbc-nucleo-h753/         (Stage 2; M1: bring-up lib + `boot` bin, build.sh, flash.sh) not a workspace member (thumbv7em only, own profile/rustflags): lib + bins for the NUCLEO-H753ZI: PAC, bring-up at 472 MHz, drivers, fault/panic handlers, generated memory.x; depends on gbc-firmware, gbc-core, gbc-link; wires gbc-link to the UART; a Stage 3 board gets its own crate (gbc-handheld); ADR 0036
tools/
  romtest/                 acceptance runner (contract: acceptance/README.md)
  sm83-tests/              SM83 single-step CPU tests (data: scripts/fetch-sm83-tests.sh -> test-data/, gitignored)
  bench/                   headless runs for check-bench.sh (Ir), the behavior fingerprint, synthetic ROMs (docs/perf.md)
  fuzz/                    fuzz targets over the public API: no panic, no hang (scripts/check-fuzz.sh)
  headless/                host counterpart of the board's headless image: calls gbc-firmware's loop, so it prints the same `headless ` lines for a ROM + input script; expected/ holds the 600-frame board targets (cpu_instrs, cgb-acid2), checked by its tests
  h7-bench/                not a workspace member: bare-metal cycle measurement on a NUCLEO-H753ZI (first on a NUCLEO-H755ZI-Q; its README); from Stage 2 M1 it takes its bring-up from gbc-nucleo-h753's lib (ADR 0036)
  memory-x/                (Stage 2, not yet created) host lib, build-dependency of gbc-nucleo-h753 and h7-bench: generates memory.x (chip maps, placements, .itcm rules from perf/itcm-sections.txt; ADR 0026, 0036)
  gbc-pc/                  (Stage 2, not yet created) PC companion, native Windows .exe (x86_64-pc-windows-gnu): sends ROM/.sav, pulls saves, forwards keys, prints stats; deps gbc-link + windows-sys (plan D9: no serialport)
```

Workspace: edition 2024, resolver 3. The release profile keeps unwinding
panics (`romtest` catches them per ROM) and line-table debug info, and
builds with one codegen unit, as the firmware will. Workspace
lints deny `unsafe_op_in_unsafe_fn` and `clippy::undocumented_unsafe_blocks`
(every `unsafe` block needs a `// SAFETY:` comment). `gbc-core` itself has
no `unsafe` at all: `#![forbid(unsafe_code)]`, checked by
`crates/gbc-core/tests/hard_rules.rs` (ADR 0009). `unsafe` belongs in front
ends and platform layers only.

Dependency direction: front ends and tools depend on `gbc-core`. `gbc-core`
depends on nothing outside `core` without human approval (AGENTS.md).

## Targets

There are two MCU targets in sequence. Both use the same `gbc-core` build
(`thumbv7em-none-eabihf`), but each needs its own platform layer: one
firmware crate per board, sharing `gbc-firmware` (ADR 0005 amendment,
ADR 0036).

| | Stage 2: dev board | Stage 3+: custom board |
|---|---|---|
| Board | NUCLEO-H753ZI + X-NUCLEO-GFX02Z1 | Custom PCB (`../gbc-32-hardware`) |
| MCU | STM32H753ZI, 472 MHz from the HSI (VOS0 limit 480; D11), RM0433 | STM32H735IGK6, 520 MHz (550 with `CPU_FREQ_BOOST`), RM0468 |
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
Memory                             // the ~50 KiB of RAM arrays, provided by the caller
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
  - Stage 3+: external HyperRAM on OCTOSPI (human decision 2026-11-24;
    hardware ADR 0001 still says FMC SDRAM), filled from eMMC when the
    game loads.

  The core must not assume the ROM is in internal RAM, or that it is
  aligned beyond a byte. It reads the ROM with loads of any width at any
  address (a VRAM DMA block is a 16-byte copy, and the compiler may merge
  adjacent byte reads), so the platform must put the ROM in memory that
  supports unaligned reads, mapped as normal, cacheable memory through
  the MPU. Memory-mapped HyperBus reads do (ES0491 §2.8.4 Table 5); the
  FMC does not (ES0491 §2.7.3), and the byte-wide rule that covered it
  (ADR 0027) was retired on 2026-11-24.
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
  sample). Each sample is the mixed signal low-pass filtered over the
  512 clocks that end with it (64-clock sums, then an 8-tap FIR at
  65,536 Hz: ADR 0035), 64 per unit of mixed level, DC kept: a steady
  full scale is ±30,720, the filter rings past it around edges, and
  the sample saturates at the ends of `i16` (`StereoSample` rustdoc,
  `apu/mix.rs` `fir`).
  When the last slot's window completes, `AUDIO_BUFFER_FULL` ends the
  call at the next instruction boundary, or at once inside a VRAM DMA
  stall (the next call resumes the stall), so no sample is dropped and a
  partial window carries over: for non-empty buffers, the stream and all
  state are independent of how the caller splits calls and buffers. A
  call of `n` clocks completes at most `n / 128 + 2` samples (550 for
  `run_frame`). An empty buffer discards audio: the APU only advances,
  and the next call with a buffer restarts the window grid at its start,
  the filter's history being its first window's level.
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


Core unit tests live in `#[cfg(test)]` modules, which may use `std`. The
gate only forbids `alloc` in `gbc-core/src`, and the `thumbv7em` build
enforces `no_std` for non-test code.

## Who owns what state

```text
Emulator<'a>
  mem: &'a mut Memory        the caller's large arrays (ADR 0009)
  hw:  Hw<'a>
    cpu: Cpu                 registers, IME, mode (running, halted, stopped, speed switch, latched opcode)
    sys: Sys<'a>             everything the bus reaches except the arrays
    stop_on: Events          optional stop events
```

- **`Memory`** (50,984 B on thumbv7em): WRAM (32 KiB), VRAM (16 KiB), OAM,
  the OAM copy an OAM DMA started from (`oam_dma_old`), HRAM, FEA0-FEFF,
  the I/O backing store `io` (registers that are plain storage: the sound
  register bytes and wave RAM, SCY/SCX/WY/WX, the DMG palettes, BCPS/OCPS
  and the undocumented CGB registers), CGB palette RAM (`bg_pal`,
  `obj_pal`), the renderer's line scratch (`obj_buf`: the line's object
  pixels and their priority ranks; `bg_pairs`: a pixel-pair table per BG
  palette, kept up to date on palette writes) and the APU's state (`apu`,
  ADR 0021 section 2). Every field's power-on value is zero, so
  `Memory::new()` is all zero bytes: on the MCU a `static MEM: Memory`
  goes to `.bss` (no flash image, no boot copy); the desktop and `romtest`
  use `Box::new(Memory::new())`. It must never be required to live on the
  stack. `Memory::clear` (`new`, `load`, `reset`, `eject`) restores exactly
  `Memory::new()`; non-zero post-boot values are written by `reset`. Two
  tests check both, field by field. `repr(C)`: the field order is the
  layout.
- **`Sys`** (`bus/mod.rs`): the cartridge (`Cart`: ROM bank windows, RAM,
  mapper, RTC), IE and IF, the bank registers (VBK, SVBK), KEY1, the
  joypad lines and the scheduled inputs, the serial port, the timer, the
  PPU, the VRAM DMA, the event table, the clock (`Hot`) and the OAM DMA
  read clears. `repr(C)` with the fields the inlined bus path reads
  first: Thumb-2 has short loads only for small offsets.
- **`Ppu`** (`Sys::ppu`): LCDC and LYC, the frame's start clock, the walk
  cursor, the current line's mode-3 timing (`LineTiming`), the STAT
  interrupt line, the timed interrupt requests (`irq::PendingIrq`), the
  resolved colors (`Colors`), the renderer (`Renderer`, with the
  fetcher's per-line state for mid-mode-3 writes) and the OAM DMA
  transfer (`dma::OamDma`: the bus drives it, the PPU reads it).
- **`Ctx`** (`bus/mod.rs`) exists for one run call: the live clock
  countdown (`Hot`, copied from `Sys::hot` and back on drop), `&mut
  Memory`, `&mut Sys`, the caller's frame and audio buffers, the events
  raised, the target, and the `done` and `attention` flags.

There is no size cap on `Hw` or `Memory` (ADR 0006, 2026-11-24
amendment). `Emulator` is 952 B on thumbv7em (2026-11-24) and is meant to
fit on the stack; `scripts/check-code-size.sh` prints both sizes. State
belongs with the component that owns it. Only something large (a buffer,
a table) goes in `Memory`, as a field whose power-on value is zero.

## Timing model

See ADR 0002. In short:
- The CPU is the master clock (ADR 0008). All times are base clocks
  (4,194,304 Hz). The per-M-cycle tick is only a clock countdown.
- Nothing else runs per M-cycle. The timer and the serial port are
  functions of the clock, computed on access; the PPU and APU are caught
  up when one of their registers is written and at their deadlines in the
  event table (ADR 0011). PPU register reads are computed from the clock;
  APU register reads advance a copy of the channel.
- A component's state is a function of the clock and the writes, never
  of when it was last synced. The `eager-sync` test build syncs
  everything on every M-cycle and must behave identically
  (`scripts/check-sync-invariance.sh`).
- PPU mode timing is exact (ADR 0014). Pixels are drawn a scanline at a
  time, with a flush up to the current pixel before a register write that
  changes the picture.
- The CPU speed is a `Speed` (`speed.rs`: normal or double). Its methods
  are the only conversions to numbers (M-cycle length and its log2, the
  system counter's rate, KEY1 bit 7). `Hot::mcycle` is the one place it
  is kept as a number, because the tick's clock arithmetic reads it.

## Module map (`gbc-core/src`)

```
lib.rs        Emulator, Hw, RunResult, the run call (`Emulator::run`, `vdma_turn`), public re-exports, sm83 test API
memory.rs     Memory: the large arrays
config.rs     Config, DmgPalettes, RGB555 helpers
bitset.rs     bitset! macro behind Buttons and Events
events.rs     Events
speed.rs      Speed and its conversions
schedule.rs   event table (ADR 0011): EventKind (dispatch order), Schedule (one u64 deadline per kind, their minimum, the slow kinds' cached minimum)
irq.rs        PendingIrq: interrupt requests that reach IF at a later clock (ADR 0033)
joypad.rs     Buttons, P1 line logic, the joypad interrupt edge, InputQueue (ADR 0012)
cpu/          SM83: Registers, Bus trait, Cpu (run loop, checked steps, interrupts, HALT and the HALT bug, STOP)
  exec.rs       opcode bodies behind a 256-arm match; the CB page out of line
bus/          mod.rs: Sys, Hot, Ctx, the tick, event dispatch, `impl Bus for Ctx`, memory map, I/O decode, peek.
              Each file below is a further `impl Ctx` block for one subsystem's glue
  reset.rs      `Sys::new`, `Sys::reset`: the post-boot state (I/O values, boot logo tiles)
  timer_io.rs   timer and serial: register writes, DIV reset, their deadlines, the Serial event
  apu_io.rs     APU: register writes, DIV-APU and audio-buffer events, the lazy sync
  ppu_io.rs     PPU: syncs to the clock, its deadline, `write_ppu`, palette data writes
  oam_dma.rs    OAM DMA: start, lazy copy, pause/resume/retime, CPU reads and writes during a transfer
  vram_dma.rs   VRAM DMA: FF55 writes, arming, the HBlank trigger, the stall, the block copy
  stop.rs       STOP mode (freeze, wake-up) and the CGB speed switch
timer/        DIV/TIMA/TMA/TAC, lazy (ADR 0011): system counter from the clock, TIMA reloads, serial edge clocks
  reference.rs, differential.rs   (test only) the eager per-M-cycle model, and the lazy one checked against it
serial.rs     SB/SC internal-clock transfers, lazy: shifts caught up on access, completion as the Serial event
ppu/          mod.rs: Frame, Ppu, mode timing (ADR 0014): LY/STAT/LYC, the STAT and VBlank interrupts, mode-3 length,
              access blocking, mid-mode-3 write handling
  edges.rs      the dots: where each edge sits and where each observer sees it, with sources
  timeline.rs   the line timeline: one row per update point of each line class; the walk derives from it
  stalls.rs     `Stalls`: one line's mode-3 pipeline stalls, a bounded list and the one timed walk over it
  objects.rs    OAM scan (`LineObjects`: up to 10 per line) and DMG-style drawing priority
  render.rs     scanline renderer (`Renderer`, spans of a line), resolved colors (`Colors`, ADR 0013), DMG and CGB mode
  mode3_oracle.rs   (test only) a dot-stepped mode 3 the closed-form timing is checked against
apu/          mod.rs: Apu (ADR 0021): power, frame sequencer (`Fs`), `sync`, register writes, pure reads (`read_apu`)
  units.rs      Length and Envelope
  square.rs     Square (CH1 and CH2) and Sweep (CH1)
  wave.rs       Wave (CH3)
  noise.rs      Noise (CH4): the LFSR as its output sequence, stepped a word at a time
  mix.rs        Output: spans, the window integrator, NR50/NR51 weights, the decimation FIR, the buffer deadline
dma.rs        OAM DMA state (`OamDma`), the CGB C conflict table, what the PPU sees (`ppu_at`), `ReadClears`
hdma.rs       VRAM DMA state (`VramDma`): registers, the HBlank period constants, stall length (ADR 0017)
cart/         mod.rs: header, `cart_info`, ROM bank windows, MBC1/1M/2/3/5, HuC1, HuC3, save-dirty tracking
  rtc.rs        the MBC3 clock and `RtcState`
  huc3.rs       the HuC3 clock chip
```

### CPU and bus (ADR 0008)

Each run call builds one `Ctx` and calls `cpu.run(&mut ctx)`, which loops
over instructions until the context reports the call done (target reached
or a stop event raised); the opcode dispatch is inlined into that loop.
The CPU is generic over the `Bus` trait. The same CPU runs against a flat
test bus in `tools/sm83-tests` (data from `scripts/fetch-sm83-tests.sh`,
verified against `tools/sm83-tests/sm83-v1.sha256`;
`GBC_REQUIRE_TEST_DATA=1` makes missing data a failure instead of a skip).

Between two instructions the loop tests one flag, `Bus::attention`. The
context raises it when the call is done or `IE & IF` may have become
nonzero (after a boundary's events, and after a write to IF, IE, P1 or a
PPU register); the CPU raises it for its own state (EI, RETI, HALT,
STOP). Only then does the CPU look at the mode, IME and the interrupts
(`Cpu::checked_fetch`; ADR 0008, 2026-11-24 amendment).

Every `read`/`write`/`idle` is one M-cycle: the access at the current
clock, then the tick. The tick increments `Hot::t_minus`, an `i32` count
of the M-cycles to the next deadline (the first M-cycle boundary at or
after the earlier of the event table's minimum and the target), and calls
`Ctx::boundary` when it reaches zero. The u64 clock is `deadline +
t_minus * mcycle`.

The address decode is inlined for reads (`bus::read`: ROM, then WRAM,
then `read_high`, cartridge RAM, VRAM, in the order code reads them) and
one out-of-line function for writes (`Ctx::write_mapped`); the measured
alternatives are in `docs/perf.md` ("CPU write path", "CPU read path").
`Hot::dma` is FE00 while an OAM DMA runs and is ORed into the region
test, so during a transfer every access takes the E000-FFFF arm, the
only one that tests it. VRAM and OAM accesses check the PPU's blocking
windows out of line. The CPU and `Emulator::peek` share the read path,
which takes shared references: the one read side effect (a CPU read that
conflicts with an OAM DMA from VRAM clears an OAM byte, ADR 0028) is
recorded through `&` in `Sys::dma_read_clears` and applied by the next
`Ctx::dma_sync`. `peek` and the DMA's own reads use `read::<false>`,
which ignores the PPU's blocking and the conflicts.

A halted, stopped, locked-up or powered-off CPU idles through
`Bus::idle_halted`, one skip straight to the next deadline: only a
deadline can change `IE & IF`, wake a stopped CPU or end the call, so the
wake-up lands on the same M-cycle as idling one M-cycle at a time.
`Emulator::step` targets `clock + 1`, so its halted step is one M-cycle.

### Scheduler (ADR 0011)

`Sys::sched` holds one u64 deadline per `EventKind` (`NEVER` = none) and
their minimum. A deadline changes only when its work is rescheduled (a
sync, a register write), never per M-cycle. `Ctx::boundary` runs every
kind due at the current clock in `EventKind` declaration order (fixed, so
same-clock events are deterministic), each handler storing its next
deadline, then sets the countdown again (`run_events`: the three hot
kinds are tested in place; the others go through a cached minimum).

| Kind | Deadline | Handler |
|---|---|---|
| `Timer` | the next TIMA reload, while it has an interrupt to request | sets IF bit 2, reloads |
| `Serial` | a transfer's completion | `Ctx::serial_event` |
| `Ppu` | `Ppu::next_event` | `Ctx::sync_ppu` |
| `Dma` | an OAM DMA's end (or the end of a restarted transfer's tail) | last copy, clears `Hot::dma` |
| `VramDma` | the HBlank trigger of an armed HDMA | `Ctx::vdma_hblank` |
| `Input` | the earliest scheduled button change (ADR 0012) | `Ctx::apply_inputs` |
| `DivApu` | the next DIV-APU falling edge, or a pending sweep event | `Ctx::div_apu_event` |
| `AudioBuffer` | the end of the audio buffer's last free window | `Ctx::audio_buffer_event` |

Register writes that move a deadline reschedule the countdown themselves.

### Timer and serial

The timer (`Sys::timer`) keeps an unwrapped u64 system counter as a linear
function of the clock (rebased on DIV writes, STOP and speed switches)
and computes DIV and TIMA from it on reads. Reloads are caught up on
access (`Timer::catch_up`). The `Timer` deadline exists only for the
interrupt request: while IF bit 2 is set and IE bit 2 is clear there is
none, and a write to either bit sets it again (`Ctx::schedule_timer`;
ADR 0011, 2026-11-24 amendment). A serial transfer has one deadline, its
completion; the bits before it are shifted when SB or SC is accessed or
DIV is reset (`Serial::catch_up`). Their register writes go through the
out-of-line `Ctx::write_timer_io`, and reads through `Timer::read` and
`read_sb`, off the inlined bus path.

### APU (ADR 0021)

`apu/`. State is `Memory::apu` (`Apu`: `Output`, `Fs`, `[Square; 2]`,
`Sweep`, `Wave`, `Noise`); its register bytes stay in `Memory::io`.
`Apu::sync` advances every channel exactly in base clocks from
`Output::synced` to `now` and, for a non-empty buffer, integrates the span
into samples. It is called (`Ctx::apu_sync`) at:

- every APU register write, FF10-FF3F (`Ctx::write_apu`: sync, then the
  write);
- each `EventKind::DivApu` deadline (`div_apu_event`: sync, then the
  frame-sequencer step or the pending sweep event), the falling edge of
  the system counter's bit 12 (13 in double speed); a DIV write or STOP
  that is such an edge steps it at once (`apu_div_reset`); a speed switch
  and reset recompute the deadline; it is `NEVER` while the APU is off;
- the `EventKind::AudioBuffer` deadline (`audio_buffer_event`: sync, raise
  `AUDIO_BUFFER_FULL`), set at call start (`start_audio`) where the last
  free slot's window ends, `NEVER` for an empty buffer;
- the end of every run call (`Ctx::finish`).

Reads are pure: FF10-FF25 are `mem.io | mask`; NR52, wave RAM and
PCM12/PCM34 go through `read_apu`, which advances a copy of the channel to
the access clock. Register-visible state is a function of the clock and
the register writes only, never of where syncs happened or whether a
buffer was passed (unit tests split calls randomly; `bench --fingerprint
--audio-buffer` checks the whole machine).

Output: one stereo sample per 128 base clocks (32,768 Hz), in two exact
integer stages (`apu/mix.rs`, ADR 0035). The mixed signal is summed over
64-clock windows (`WINDOW`), the spans of a sync being cut at the window
ends and at NR50/NR51 writes; at the end of every second window an 8-tap
symmetric FIR over the last 8 window sums (`mix::FIR`, sum 1,024, DC
gain 1) makes the sample, saturated to `i16`. The window history lives in
`Output` beside the open window's sums, so the stream does not depend on
where syncs, batches or buffers end. The filter delays the stream by 2
samples against the clock (1.5 more than a plain 128-clock average). The
core keeps DC and does no high-pass filtering. Alias levels and the
alternatives are in `docs/reference/audio-aliasing.md`; hardware rules
and the SameBoy count conversions in `docs/reference/apu.md`; cost in
`docs/perf.md` ("APU cost", "Output filter").

### PPU timing (ADR 0014)

`ppu/mod.rs`. The PPU keeps `frame_start` (the clock of frame position 0;
LCD enable is position 7) and computes LY, the STAT mode and coincidence
bits and the VRAM/OAM/palette blocking windows from the position `now -
frame_start` on every read, without side effects. Positions are SameBoy's
nominal action times converted so that the state at a position includes
the actions at it (an M-cycle boundary in single speed is at position 3
mod 4).

Three files hold the timing, each with one job:

- `edges.rs`: the dots. Where each edge sits on a line, per speed, and
  from which dot each observer (a read, the dispatch, a STAT or LYC
  write) sees it, with the source for each.
- `timeline.rs`: the order and meaning. One row per *update point* (`Pt`)
  of each line class: its dot, what the STAT line evaluates there (mode
  source, comparison value), how late its request reaches IF, and where
  reads see its comparison. The walk's code takes each arm's numbers from
  the table by `const` lookup, and the table's tests interpret it for
  every line, dot and speed and compare with that code.
- `mod.rs`: the walk. `Ppu::cursor` is the next point after the last
  sync; `Ppu::step` advances it point by point with no division.
  `Ppu::sync(now)` commits the points up to `now`, keeping the STAT
  interrupt line exact (SameBoy `mode_for_interrupt`, `ly_for_comparison`
  and the held LYC line; a rising edge requests IF bit 1).
  `Ppu::next_event` walks ahead without committing to the first point
  that requests an interrupt, completes the frame (VBlank, `FRAME_READY`)
  or ends a visible line's mode 3: the `EventKind::Ppu` deadline. With no
  STAT source selected the walk is separate code that visits only the
  points with work (`step_work`); the choice is made once per call
  (`run_points::<QUIET>`). `Ppu::seek` finds the cursor again after LCD
  enable, reset, a speed switch or a STAT write that changes whether any
  source is selected. `Ppu::sync` and `walk_to` inline only the test for
  "no point to run" (most register and palette writes); the deadline's
  own sync (`Ppu::sync_due`) skips it.

IF (`Sys::iflags`) holds bits 0-4 and nothing else, so the CPU's
interrupt checks are `IE & IF` with no mask. A request that reaches IF at
a later clock than the point or write that makes it (single speed: a LYC
write's request, line 0's mode-2 pulse, line 153's LYC=0 point) waits as
"bits B at clock T" in `Ppu::irq` (`irq::PendingIrq`, ADR 0033):
`Ppu::next_event` is at most T, and `Ppu::sync` to T or later posts it,
so IF never depends on when the PPU was last synced. Rules:
`docs/reference/stat-irq.md`, "Timed requests".

Mode 3 of a visible line is fixed at its pipeline start (the `Latch`
point, `Ppu::latch`): 172 dots plus `SCX % 8`, 6 for the window and 6-11
per object (Pan Docs "OBJ penalty algorithm"; an object in the window
waits on the tile grid the renderer draws, ADR 0034), as `LineTiming`
with the per-pixel stalls so `Ppu::mode3_pixel(now)` can say which pixel
is being drawn. The stalls are a `Stalls` list (`ppu/stalls.rs`: at most
11, the window's as an `Option`), and everything that needs a stall's
place in the line walks it through one iterator (`Stalls::timed`,
`LineTiming::walk`). Objects come from one OAM scan per line
(`objects.rs`, `LineObjects::scan`) that feeds both the stalls and the
renderer. The scan is skipped for lines a cached object-line map
(`ObjectLines`, invalidated by `Ppu::oam_written` on CPU and DMA OAM
writes; rebuilt one line after a write, if no other write follows:
`docs/perf.md`, "Object-line summary") shows empty, and on the first
line after LCD enable (the hardware does no OAM scan there). The closed-form mode-3 timing is
checked against a dot-stepped model in tests (`mode3_oracle.rs`).

Every PPU register write goes through `Ctx::write_ppu`: sync first, then
the write (STAT in two phases one dot apart, LYC one dot into the M-cycle
in single speed, as SameBoy's conflict maps), then reschedule.

### Scanline renderer

`ppu/render.rs`, driven by the PPU (`Ppu::renderer`); pixels go straight
into the caller's `Frame`. The `Latch` point starts the line
(`Renderer::start_line`: SCX & 7, the line's objects in drawing order),
the `End` point draws whatever of the line is left up to pixel 160
(`Ppu::draw`), and `Ppu::next_frame` and LCD enable start a frame (window
line counter and WY condition; the renderer's WY condition is the only
copy, which the window stall reads too). `Ppu::drawn` counts the pixels
of the current line already in the frame.

Mid-line writes (ADR 0002 catch-up): `Ctx::write_ppu` for LCDC, SCY, SCX,
BGP, OBP0, OBP1, WY and WX calls `Ppu::flush(at)` before the write lands,
which draws the line up to `mode3_pixel(at)` with the old value. `at` is
where the write lands in the M-cycle (SameBoy `cgb_conflict_map`, single
speed): the access clock for LCDC/SCY/SCX/WY, one dot earlier for the
palettes (CGB C), one dot later for WX. Drawing at the mode-3 end is exact
for VRAM and OAM because the CPU can't write them during mode 3 (and OAM
during mode 2, when the scan happens); OAM DMA is the exception, below.

The fetcher has already read some tiles when such a write lands, so the
renderer owns the fetcher's per-line state (`docs/design/mid-line-ppu.md`):
the held parts of the line (`Held`: pixels fetched before a map-select,
tile-data, SCX or SCY write, drawn with the old values, and which planes
of the last tile were read after it, `Mixed`), the object fetch cursor,
the CGB TILE_SEL glitch latch (`SelLatch`) and the window zero pixel. The
timing code decides them (`Ppu::hold_fetch`, `Ppu::window_write`) and
hands them over (`Renderer::push_hold`, `sel_set`/`sel_clear`,
`set_zero`); `Memory` only supplies registers, VRAM, OAM, palette RAM and
the object line buffer.

Registers: LCDC lives in `Ppu`; SCY, SCX, WY, WX and the raw BGP/OBP0/OBP1
stay in `mem.io` (reads unchanged on the bus path), and the renderer reads
them through `Ppu::regs` when it draws. Colors are resolved when written
(ADR 0013): `Ppu::colors` holds RGB555 tables, rebuilt on BGP/OBPx writes
(DMG compatibility mode, through the caller's `DmgPalettes`) and on
BCPD/OCPD writes (CGB mode, from palette RAM). In CGB mode each tile
fetch reads its attribute byte (palette, VRAM bank, flips, BG-to-OBJ
priority) and objects win pixels by OAM index instead of X; compatibility
mode has its own copy of the tile loops with no attribute read. Spans
without object pixels go straight from tile bits to RGB555; others mix
with the object buffer. Cost: `docs/perf.md`.

### OAM DMA

`docs/reference/oam-dma.md` has the hardware rules. The transfer's state
(`dma::OamDma`) lives in the `Ppu` (`Ppu::dma`): the bus drives it
(`bus/oam_dma.rs`: the FF46 write, HALT pause and wake-up, the speed
switch's retime, CPU conflicts) and both read it, so the PPU's sync and
draw calls take no extra arguments.

For the CPU: `Hot::dma` routes every access during a transfer to
`read_during_dma`/`write_during_dma` (`dma::conflict`, the CGB C table).
The bytes are copied lazily (`Ctx::dma_sync`): before a CPU write during
the transfer, a restart, every PPU sync, the end of a run call and the
`Dma` deadline.

For the PPU: `OamDma::ppu_at(clock)` says what a PPU OAM read at a clock
sees (off, running, halted) and how many bytes are written by then; it is
a pure function of the state since its last change. So before each change
(an FF46 write, a halt during a transfer) the bus calls
`Ctx::dma_ppu_event`: the PPU syncs, scans the OAM entries read so far
(`Ppu::scan_to`, keeping the partial scan in `Ppu::scan`), fixes the
attributes of the objects fetched so far and draws the line up to now
(`Ppu::fix_attrs`, `flush`). The wake-up from HALT needs no sync: it
changes nothing up to it.

A line is DMA-aware from the FF46 write (`OamDma::ppu_live`) until the
first line that starts after the transfer's end as the PPU sees it
(`Ppu::dma_touches` clears the flag), so the normal path costs one flag
test per line. On an aware line the OAM scan runs entry by entry in runs
of equal state, tracking the mode-2 bus (`Ppu::bus`), and the objects' tile
index and attributes come from `Ppu::obj_attr` (passed to the renderer as
`Sources::attrs`), filled lazily at each draw from the clock of each
object's fetch in its stall. OAM bytes the transfer hasn't written yet are
read from `Memory::oam_dma_old` (OAM at the FF46 write); the written ones
are in `mem.oam`.

### VRAM DMA (ADR 0017)

State is `Sys::vdma` (`hdma::VramDma`: the running source and destination
counters, the block count, and the flags below); the glue is
`bus/vram_dma.rs`. FF51-FF54 only store. An FF55 write
(`Ctx::write_hdma5`) starts a general-purpose transfer, or arms an HBlank
one: `Ctx::vdma_arm` then sets the `VramDma` deadline once per visible
line, `hdma::HBLANK_DELAY` dots after the line's mode-0 start (every PPU
sync during an HBlank transfer checks it, and `write_ppu` arms it again
when a mid-mode-3 write moves that start).

A transfer step does not run inside an instruction. The trigger sets
`VramDma::pending`, which ends `Cpu::run` after the current instruction;
`Emulator::run` then calls `vdma_turn`, which latches the next opcode
fetch (`Cpu::latch_fetch`), runs the step (`Ctx::vram_dma`: all blocks of
a GDMA or one HDMA block, copied at once, 16 bytes at a time from the ROM
window, cartridge RAM or WRAM, after the PPU has drawn everything before
it) and then the CPU's stall (`Ctx::vdma_stall`). The stall is skipped a
deadline at a time and is resumable: `VramDma::stall` holds the M-cycles
left, so a stop event or the call's target can end the run call inside
it and the next call continues (ADR 0021, GDMA stall amendment). HALT
holds HBlank transfers (`vdma_halt`, `vdma_wake`, `Bus::wake_stalls`).

### STOP and the speed switch (ADR 0020)

`bus/stop.rs`. STOP with a selected button held does nothing. Otherwise
it resets DIV after its last byte (the second, when nothing is pending;
AGE's model, verified on CGB B/C), and either enters STOP mode (`stop_freeze`: the
system counter and its deadlines freeze, frames and samples keep their
rate, frames are black unless the STOP was in mode 3; `stop_wake` ends
it on a button) or, with KEY1 armed in CGB mode, switches speed.

The speed is an input to every lazy component and every deadline, so
`Ctx::speed_switch` goes in a fixed order: bring everything lazy up to
the switch clock under the old speed's rules, change the speed
(`Hot::set_speed`, the timer's counter rate, the PPU's walk, a paused OAM
DMA), set every speed-dependent deadline again, and only then let time
pass. The CPU then halts for about 0x8000 M-cycles unless an interrupt
is pending (ADR 0020 has the exact counts; `Bus::speed_switch_halt`,
skipped a deadline at a time like HALT).

### Test-only builds

Two cargo features of `gbc-core` exist for the invariance checks in
`docs/perf.md` and are never in a product build: `eager-sync`
(`Ctx::eager_sync` before every access) and `clock-offset`
(`Emulator::start_clock_at`: power on with the clock at an arbitrary
value).
