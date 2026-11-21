# 0008: CPU/bus structure and hot-path design

- Status: Accepted
- Date: 2026-11-21

## Context
M1 adds the SM83 CPU and the memory bus. The core runs on a Cortex-M7 at
~480 MHz (Stage 2), so the per-M-cycle path must be cheap: no `dyn`, no
floats, no allocation, small stack frames. ADR 0002 makes the CPU the
master clock with every memory access on its own M-cycle, and M2 needs
exact access timing, so that structure has to exist from the start. Two
API review findings had to be settled before the bus signature was fixed:
a per-run-call context so lazy PPU/APU catch-up on any bus access doesn't
thread the frame and audio buffers through every call (finding 6), and
ROM size handling so opcode fetch avoids per-access work (finding 11).
The CPU also has to be testable instruction by instruction against the
SM83 single-step suite, which assumes a flat 64 KiB RAM.

## Decision

**The CPU is generic over a `Bus` trait** (`cpu/mod.rs`). `read`, `write`
and `idle` are each exactly one M-cycle, called in the order GBCTR gives
for each instruction; `interrupt_enable/flags`, `event`, `stop` are free.
Static dispatch only: the emulator's bus is monomorphized and inlined, and
`tools/sm83-tests` drives the same CPU with a flat test bus. The CPU and
trait are public as `gbc_core::sm83` for that purpose (front ends don't
need it).

**Access, then tick.** A bus call performs the access at the start of the
M-cycle, seeing the state as of the current clock, then advances the
machine by one M-cycle (the SameBoy model: `cycle_read` flushes pending
cycles, accesses, then defers 4 clocks). The tick is: `clock += mcycle`
(4 base clocks, 2 in double speed), the timer (16-bit counter += 4,
falling-edge detection; the returned "bits that fell" also clock the
serial port, which hardware derives from the same divider), and one
compare against `next_event`, the earliest scheduled work (today the
PPU's VBlank). Everything else is lazy.

**`Hw` is split into `cpu: Cpu` and `sys: Sys`** (what the bus reaches),
so a run call borrows both mutably. Each run call builds one context,
`Ctx { mem, sys, frame, audio, audio_len, events }`, on the stack and
passes `&mut Ctx` to `cpu.step` in a loop. Catch-up code (PPU now, APU in
M6) reaches the caller's buffers through that one pointer. Events raised
anywhere are ORed into `ctx.events`; the loop checks them after each
step, so calls still end on instruction boundaries (ADR 0006).

**The PPU is caught up lazily** (ADR 0002): it is synced when the clock
reaches its next event and before an LCDC write; LY and STAT reads are
computed from the last sync point without mutation. Reads therefore have
no side effects anywhere, so one `read(mem, sys, addr)` function serves
both the CPU and `Emulator::peek`.

**Opcode dispatch** is a 256-arm `match` (a jump table) whose arms call
`op::<OP>` / `cb::<OP>`, functions generic over the constant opcode. Their
bodies decode registers and conditions from `OP` with plain arithmetic,
which the compiler folds, so each arm is straight-line code with no
second-level decode. Registers are plain `u8` fields; flags are computed
eagerly.

**ROM banking.** At load, the ROM size is rounded up to a whole bank and
then to a power of two (at least 32 KiB), and that minus one is kept as
`rom_mask`; bank offsets wrap with it, as in SameBoy (`GB_load_rom`). The
two visible 16 KiB windows are sub-slices of the ROM recomputed only when
a bank register is written, so a read is "pick window by address bit 14,
index by the low 14 bits". Reads past the end of a short or odd-sized ROM
return 0xFF (the M0 rule): the window is shorter than 16 KiB or empty and
`get()` falls back to 0xFF. That costs one compare against the window
length per ROM read. Eliminating it would need fixed-size array windows,
and a partial last bank can only be one by copying it into a 16 KiB
padded buffer (RAM we don't want to spend on the MCU) or by a flag check,
which costs the same compare. To be revisited with numbers from the M8
benchmark if ROM fetch shows up in profiles.

**Memory map.** `addr >> 13` selects the 8 KiB region. WRAM/VRAM bank
offsets are precomputed on SVBK/VBK writes and indices are masked to the
array size so they compile without bounds checks. CGB mode comes from the
header (0143 bit 7); in DMG compatibility mode the CGB-only registers
(KEY1, VBK, SVBK, palettes) ignore writes and read FF. FEA0-FEFF is the
CGB-C extra RAM with address bits 3-4 ignored (SameBoy `GB_read_oam`).
I/O registers without a component yet (sound, most PPU registers) are
plain storage in `Memory::io`.

**STOP and speed switch.** STOP with KEY1 armed in CGB mode switches
speed (Blargg's `cpu_instrs` does this), pausing 2050 M-cycles without
ticking DIV (Pan Docs: KEY1). Otherwise STOP is a stub that idles until a
button is pressed. Exact behavior is M5.

## Consequences
- M2 work is local: timer reload-window writes, OAM DMA timing (now an
  instant copy), HALT exit latency and the `EI; HALT` case, and scheduled
  inputs plug into the tick or the CPU without restructuring.
- New lazily-run components add their next event to `next_event`; the
  tick stays one compare. Anything that must see every M-cycle (OAM DMA,
  HDMA) goes into the tick and costs every cycle, so keep it behind a
  cheap "active" check.
- 512 monomorphized opcode bodies cost code size (flash/ITCM on the MCU);
  to be measured in Stage 2.
- `gbc_core::sm83` is public API surface, but only test tools use it.
- Measured on the host (release, x86-64): Blargg `cpu_instrs` runs 31
  emulated seconds in ~0.38 s, about 80x real time (~4,800 frames/s).
  The proper benchmark is M3/M8 (`docs/perf.md`).

## Amendments

### 2026-11-21: hot path after the M1 milestone-close review

The M1 performance review found per-instruction and per-M-cycle overhead
that the structure above didn't need. Changes (behavior is unchanged:
every unit test, all 500,000 SM83 single-step cases and the full romtest
manifest results, including end clocks, frame counts, serial output and
registers, are bit-identical):

- **The instruction loop runs inside the CPU.** `Cpu::run(bus)` steps
  until `Bus::run_done()` (default true: one step) and is the only caller
  of the 256-arm dispatch, which is `#[inline(always)]`, so a run call
  saves and restores callee-saved registers once, not once per
  instruction. `Emulator::step` uses the same `run` with the context
  already marked done. `Cpu::step` remains for `tools/sm83-tests` and
  tests. The HALT-bug step now goes through the normal fetch with a PC
  increment of 0 instead of calling the dispatch a second time (IME and
  the EI delay are always off in that state).
- **32-bit clock countdown.** The tick no longer compares a u64 clock with
  `next_event`. `Hot::budget: i32` holds the base clocks left until
  `deadline = min(Sys::next_event, run target)`, capped at `i32::MAX`
  ahead; the tick subtracts the M-cycle length (4 or 2) and calls the
  out-of-line `Ctx::boundary` when it is `<= 0`, which runs due events,
  ends the call if the target was reached, and sets the next deadline.
  The clock is `deadline - budget`, exact at every point, so `clock()`,
  LY/STAT reads and event timing are unchanged. Anything that moves
  `next_event` (PPU sync, LCDC writes) reschedules.
- **Hot state in `Ctx` by value.** The countdown, M-cycle length and timer
  (`bus::Hot`) are copied into `Ctx` when it is built and back to
  `Sys::hot` when it is dropped, so the tick reaches them through the one
  pointer the CPU holds and never loads `ctx.sys`. `read` takes the live
  `Hot` (the context's during a run, `Sys::hot` for `peek`). One mask,
  `Hot::watch`, holds the TIMA input bit and the serial bit clock, so one
  test per M-cycle covers both; the work behind it is out of line. Stop
  events set a `done` flag when raised, so the loop checks one bool per
  instruction. The timer's behavior is unchanged (the lazy timer is an
  M2 decision).
- **No bounds-check panics in the dispatch.** Cartridge RAM accesses use
  `get`/`get_mut` with an unreachable fallback instead of indexing (the
  index is masked to the header size, which the compiler can't relate to
  the buffer length). `run`, `step_not_running` and `write_mapped` have
  no `panic_bounds_check` relocations on thumbv7em; `read_high`,
  `write_high` (OAM/HRAM/FEA0 indexing) and `Cpu::reset` still do.

Measurements (host x86-64 release, Blargg `cpu_instrs` per-ROM wall time
from romtest, best of 5, interleaved runs): **0.373 s before, 0.275 s
after** (~26% less; about 113x real time for its 31 emulated seconds).

Code size in bytes (`thumbv7em-none-eabihf`, release, `RUSTFLAGS="-C
target-cpu=cortex-m7"`; `nm --size-sort -S` on the rlib's objects):

| Function | Before | After |
|---|---|---|
| `Cpu::execute::<Ctx>` | 50,076 + 50,068 (two CGU-local copies) | inlined into `run` |
| `Cpu::step::<Ctx>` | 452 | inlined into `run` |
| `Cpu::dispatch::<Ctx>` | 792 | inlined into `run` |
| `Cpu::run::<Ctx>` | - | 37,064 |
| `Cpu::execute_cb::<Ctx>` | 18,092 | 15,702 |
| `Cpu::step_not_running::<Ctx>` | 902 | 216 |
| crate `.text` total | 131,021 | 63,805 |

Inlining `execute_cb` into `run` as well was measured and gave no host
speedup, so it stays out of line.
