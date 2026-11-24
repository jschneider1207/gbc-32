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

### 2026-11-24: one test between two instructions; the countdown in M-cycles

Stage 1 M10 review, findings B6, B7, B9 (`docs/reviews/stage1-m10/B-perf.md`)
and C6, C7 (`C-size-mcu.md`). Behaviour is unchanged: the SM83 single-step
suite, the romtest manifest and the fingerprint of every corpus run are
identical.

**The loop.** Before every instruction `Cpu::run` tested four things, each
a load and a branch: the run's end (`Ctx::done`), the mode, `ime && IE &
IF != 0`, and EI's delay. Now:

```
loop {
    let Some(mut op) = self.checked_fetch(bus) else { return };  // out of line
    loop {
        self.execute(op, bus);              // the one inlined 256-arm dispatch
        if bus.attention() { break }        // the one test
        op = bus.read(pc); pc += 1;
    }
    if bus.run_done() { return }
}
```

- `Bus::attention()` is one flag (`Ctx::attention`). It is true whenever
  the next step may be anything but a plain fetch: the run is done, the
  CPU is not in `Mode::Running`, EI's delay is pending, or `ime && IE &
  IF != 0`. True more often than that is allowed and only slower.
- `Cpu::checked_fetch` is the old per-step sequence, unchanged and in the
  same order (the mode's step, the interrupt check and dispatch, EI's
  delay), then the opcode fetch. It loops over steps that are no
  instruction (a halted idle, a dispatch), testing `run_done` after each
  as `run` did, and returns the opcode of the first that is. On the way to
  that fetch it calls `Bus::settle(ime)`, which lowers the flag to `done
  || (ime && IE & IF != 0)`. The first step of every `run` call goes
  through it, so CPU state set from outside between calls
  (`Emulator::run` setting `done` for a single step, `latch_fetch`, the
  setters) needs no flag. `Cpu::step` is one checked step.
- **Who raises it, and when.** The CPU, in the instruction that changes
  its own state: EI (`Bus::attend`), RETI (`Bus::attend_ime`: only with
  `IE & IF != 0`, IME being on from here), HALT, STOP and an invalid
  opcode. The bus (`Ctx::irq_attention`: if `IE & IF != 0`, whatever IME
  is, since the CPU tests IME itself), right after anything that can set
  an IF bit or change IE while instructions run: the events of a
  `boundary` (timer, serial, PPU sync with its timed requests of ADR
  0033, scheduled joypad input), a serial byte completed by a DIV write,
  and a write to IF, IE, P1 or a PPU register (LCDC, STAT, LYC). And
  `Ctx::end_run` (a stop event, the target, a VRAM DMA request) with
  `done`.
- **Why the timing is the same.** The interrupt check itself didn't move:
  it reads IME, IE and IF in `checked_fetch` at the same point between
  two instructions as before. The flag only decides whether that check is
  made, and it is raised inside the M-cycle whose access or tick changed
  the inputs, always before the instruction ends. With IME off and a
  request waiting, `settle` lowers the flag; nothing can be dispatched
  until IME changes (EI, RETI: the CPU raises it) or IE or IF do (the bus
  raises it again). A PPU sync to the current clock sets no IF bit (a
  point that requests an interrupt has the PPU deadline at or before it,
  so a boundary walked it); only the syncs ahead of the clock do, in the
  IF/STAT/LYC writes, which raise the flag after it.
- **Tests.** `run_calls_match_single_steps_for_every_interrupt_request`
  (run calls against single steps, each of which is a checked step, with
  the dispatch clocks compared) and
  `run_on_a_bus_with_low_attention_matches_single_steps` (the CPU's side,
  M-cycle by M-cycle, on a test bus). Removing any one raise fails one of
  them (15 mutations, each seen failing). In builds with debug
  assertions the loop also asserts, on the path that skips the check,
  that the check would have found nothing; the `checked`-profile runs of
  `scripts/check-sync-invariance.sh` exercise that over the whole corpus.

Rejected: forcing the countdown to zero instead of a flag (review C6), so
that the per-instruction test would be the fetch M-cycle's own tick. It
saves the flag test (2 x86 instructions) but every raise then costs a
`boundary` call per M-cycle until the instruction ends, about 100 Ir,
which `synth:halt-irq` (three raises per interrupt) would pay.

**The countdown.** `Hot::budget` (clocks left, minus `mcycle` per tick,
test `<= 0`) is now `Hot::t_minus`: minus the M-cycles until `deadline`,
plus one per tick, test `>= 0`. `deadline` is the first M-cycle boundary
at or after the earliest due clock, so the boundary runs on the same
M-cycle as before, and the clock is `deadline + t_minus * mcycle`, exact
at every point. Counting up makes the test the sign of the sum, which the
increment's own flags give (a `<= 0` test of a wrapping subtraction needs
a second instruction). `Hot` and `Ctx` are `repr(C)` with the count
first: the tick is `inc [ctx]; js` on x86 (was 7 instructions) and
`ldr; adds; str; bmi` on thumb (was 7, 3 of them loads). A change of the
M-cycle length keeps the clock (`Hot::set_mcycle`); the speed switch's 3
odd clocks are `Ctx::skip_clocks`.

**Why field addresses mattered.** With one address used by more than 100
memory accesses in a function (the ~140 inlined ticks, the ~90 inlined
reads), LLVM's CodeGenPrepare stops folding it into the accesses
(`-cgp-max-address-users-to-scan`, default 100) and keeps `&ctx.field`
as a value: rematerialized before each use on x86 (`lea; mov`), spilled
and reloaded on thumb (612 stack-slot references in `Cpu::run`, review
B9/C6). Offset 0 needs no address, which is why the count is first. The
other hot fields (`hot.dma`, the `sys` and `mem` pointers) still pay one
instruction per access. Raising the limit (`-C
llvm-args=-cgp-max-address-users-to-scan=100000`) folds them all: on the
tree of this amendment it takes cpu_instrs from -12.32% to -18.19%
against the baseline, cgb_sound to -15.06%, event-storm to -14.12%,
worst-case to -10.42%, cgb-raster to -10.69%, halt-irq to -3.62%, and
thumb `.text` from 111,490 to 110,360 B (`Cpu::run` 26,544 -> 25,414 B).
It is a hidden LLVM option and a build-configuration decision (the bench,
the size check and the firmware would all have to build with it): not
taken here, recorded for the human. Any further hot field in `Ctx` or
`Hot` (review B8: ROM windows and bank offsets by value) meets the same
limit.

**Register pairs** (review C7). `Registers` is `repr(C)` with the low
register of each pair first (F, A, C, B, E, D, L, H), so a pair is one
16-bit load or store instead of two bytes swapped (`rev` on thumb: 71 ->
28 in `Cpu::run`).

Measurements (x86 Ir, `scripts/check-bench.sh`, against the baseline of
the same day; thumbv7em sizes from `scripts/check-code-size.sh`):

| | baseline | countdown | + attention | + register pairs |
|---|---|---|---|---|
| cpu_instrs | 1,694,717,771 | -6.03% | -11.94% | -12.32% |
| cgb_sound | 1,130,295,021 | -5.90% | -9.92% | -9.96% |
| cgb-acid2 | 689,083,500 | +0.21% | -0.25% | -0.28% |
| event-storm | 208,386,107 | -6.83% | -10.32% | -10.32% |
| dma-storm | 428,664,099 | -0.63% | -2.66% | -2.99% |
| halt-irq | 286,184,478 | -1.87% | -2.41% | -2.41% |
| worst-case | 313,479,719 | -4.15% | -7.98% | -7.98% |
| cgb-raster | 324,867,705 | -4.31% | -7.94% | -8.15% |
| apu-writes | 82,421,047 | -1.04% | -1.57% | -1.59% |
| total `.text` (B) | 108,656 | 108,242 | 112,368 | 111,490 |
| `Cpu::run` (B) | 24,512 | 23,784 | 27,400 | 26,544 |

(`checked_fetch` is 1,088 B from the attention step on; `step_not_running`,
538 B, is inlined into it.)

Thumb, from the end of one instruction's handler to the `tbh` of the
next, opcode in ROM: 28 instructions, 8 loads, 2 stores, 4 conditional
branches (was 40, 14, 2, 11). The size went up with the second step
although the per-instruction code went down: with the test at the end of
each handler LLVM places the code differently and copies the 4-instruction
tick into each exit of an inlined read (~410 copies, were ~160) and the
3-instruction test into 113 handler ends. `Cart::read_ram` is
`#[inline(never)]` now: the inliner had made it a call at 88 of the 89
reads before and a copy at all 89 after (4.6 KB).
