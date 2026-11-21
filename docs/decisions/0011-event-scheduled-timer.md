# 0011: Event-scheduled timer, serial clock and DIV-APU; HALT idles to the next event

- Status: Accepted
- Date: 2026-11-21

## Context
ADR 0008 ticks the timer every M-cycle: the 16-bit system counter += 4,
"bits that fell" against a watch mask (TIMA input and serial bit clock),
and a "reload pending" check, before the one compare against the next
deadline. The M2 plan asks whether to keep that or derive the timer from
the clock and schedule its effects as events, before M2 builds the timer
reload window, OAM DMA and HALT timing on top of it.

**Measured cost of the eager tick** (Ir, `tools/bench` under callgrind,
`--toggle-collect='*run_emulation*'`, 20 emulated seconds, HEAD 459f291):

| ROM | Ir | M-cycles | Ir / M-cycle |
|---|---|---|---|
| cpu_instrs.gb | 1,926,303,053 | 36,122,537 (mostly double speed) | 53.3 |
| cgb_sound.gb | 922,727,228 | 20,971,521 | 44.0 |
| cgb-acid2.gbc | 928,451,269 | 20,971,520 | 44.3 |

M-cycles were counted with a throwaway counter in a scratch copy.
`--dump-instr=no` with line-level annotation (`callgrind_annotate --auto=yes`)
attributes the inlined tick in `Cpu::run` on cpu_instrs as:
reload-pending check 100.7 M, counter advance and fell mask 136.5 M, watch
test 81.6 M (timer total 318.8 M, **16.5%**, 8.8 Ir/M-cycle), and the
budget countdown and test 163.4 M (4.5 Ir/M-cycle). Line attribution
undercounts what the timer really costs: it also takes registers, spills
and code in `run` (see the prototype below). In the same run, the timer and
serial slow paths ran 39 reloads and 16,056 edges in total, so almost all of
those per-M-cycle instructions do nothing.

**Prototype** (scratch copy only, not committed): the timer and serial
clock derived from the clock as below, the tick reduced to the countdown,
and in a second step HALT idling to the next deadline. Results:

| ROM | Eager | Lazy timer | Δ | + HALT skip | Δ vs eager |
|---|---|---|---|---|---|
| cpu_instrs.gb | 1,926.3 M | 1,382.6 M (38.3 Ir/M-cycle) | −28.2% | 1,382.6 M | −28.2% |
| cgb_sound.gb | 922.7 M | 621.8 M (29.6) | −32.6% | 621.8 M | −32.6% |
| cgb-acid2.gbc | 928.5 M | 697.1 M (33.2) | −24.9% | 13.0 M | −98.6% |

Code size on thumbv7em (`scripts/check-code-size.sh`): `Cpu::run` 37,064 →
32,016 B (−13.6%), `execute_cb` 15,702 → 14,286 B. The timer register logic
inlined into the cold I/O paths grew `write_high` 3,080 → 5,168 B and
`read_high` 1,080 → 1,352 B. The implementation keeps it out of line (see
below).

Equivalence: both prototypes were checked against HEAD with a fingerprint
run. 295 ROMs (Mooneye acceptance/misc/emulator-only, Blargg, AGE,
SameSuite, cgb-acid2) ran 3 emulated seconds each in 456-clock `run_until`
chunks, with LD_B_B and SERIAL_TX stops on. After every chunk the
fingerprint hashed the clock, events, registers, SB/SC/DIV/TIMA/TMA/TAC/IF/LY
and serial output, then WRAM at the end. It was **identical on all 295**,
including the 13 `mooneye-test-suite/acceptance/timer/*` ROMs (11 pass, 2
fail, as at HEAD). Host wall time on cpu_instrs dropped about 10-30%, but
noisily. Ir is the number of record.

On the MCU this is about 15 fewer instructions per M-cycle. At 2.1 M
M-cycles/s in double speed, that is roughly 30 M instructions/s, about 6%
of a 480 MHz M7 at IPC 1 (an estimate, not a measurement), against a 50%
budget. HALT-heavy games, which is most games waiting for VBlank, save far
more.

## Decision
**Lazy timer.** Nothing about the timer, serial port or DIV-APU runs per
M-cycle. The tick is only `budget -= mcycle; if budget <= 0 { boundary() }`.
Their state is computed from the clock when it is read or written. Their
effects (TIMA reload + IF, a serial bit shift, a DIV-APU step) are deadlines
in the event table that `boundary()` already serves.

### The system counter
The counter is unwrapped (u64). Between DIV writes and speed switches, it is
a linear function of the base clock:

```text
sc(t)        = base + (saturating(t - base_clock) << shift)  // shift: 0 normal, 1 double speed
DIV(t)       = (sc(t) >> 8) as u8
falls(k,a,b) = (b >> (k+1)) - (a >> (k+1))   // falling edges of bit k while the counter goes a -> b
next_fall(k,a) = ((a >> (k+1)) + 1) << (k+1) // first counter value > a at which bit k falls
clock_of(v)  = base_clock + ((v - base) >> shift)
```

The counter counts CPU clocks: 4 per M-cycle in both speeds, so 1 per base
clock at normal speed and 2 in double speed (Pan Docs: Timer and Divider
Registers; FF4D KEY1). `base` is always a multiple of 4, taken at an M-cycle
boundary. Every edge of bit 3 or higher is therefore at a counter multiple
of 16, and `clock_of` gives an exact M-cycle boundary in either speed. A
u64 counter won't wrap: 2^64 / 2^23 per second is about 70,000 years.

- **DIV write** (FF04, and STOP, which resets DIV: Pan Docs FF04) at clock
  `t`, with `old = sc(t)`:
  1. Sync TIMA to `t`.
  2. Apply the falling edges the reset causes on every watched bit:
     - `old & tima_bit`: TIMA glitch increment;
     - `old & serial_bit`: one serial shift;
     - `old & apu_bit`: an early DIV-APU step.

     All three go through the same handlers as the scheduled events (Pan
     Docs: Timer obscure behaviour; SameBoy `GB_set_internal_div_counter`,
     `triggers = div_counter & ~value`).
  3. Rebase: `base = (old | 0xFFFF) + 1` (low 16 bits zero, still
     monotonic, so `falls` never runs backwards), `base_clock = t`.
  4. Reschedule all three deadlines.
- **Speed switch** (STOP with KEY1 armed): the DIV reset above, then `shift`
  flips and `base_clock = t + pause`, so the counter stays frozen through
  the pause (2050 M-cycles today; exact in M5, Pan Docs FF4D). The
  `saturating` in `sc` covers reads that land in the pause. Reschedule
  everything; DIV-APU moves between bit 12 and bit 13 (below).
- **Power-on phase** (M3 task, boot_div): `base` becomes the post-boot
  counter value. That is one constant; nothing to replay.

### TIMA, TMA, TAC
State:
- `tima` and `tima_from`: the TIMA value is exact at counter value
  `tima_from`.
- `tma`, `tac`, `tima_bit` (0 when disabled).
- `overflow_at`: the clock of the M-cycle boundary at which TIMA wrapped,
  or `NEVER`.
- `reloaded_at`: the clock of the reload, i.e. `overflow_at + mcycle`.

Reading TIMA at clock `t`:
- If `overflow_at <= t < reloaded_at`: `0`.
- Otherwise: `tima + falls(k, tima_from, sc(t))`.

This never wraps, because the overflow is an event.

When enabled, the overflow is at counter value
`((tima_from >> (k+1)) + (256 - tima)) << (k+1)`. Its clock is
`overflow_at`, and the scheduled timer deadline is `reloaded_at`. At that
event: `TIMA = TMA`, `IF |= 0x04`, `tima_from = sc(now)`, then schedule the
next overflow.

Cycle-exact window, mapped onto ADR 0008's access-then-tick M-cycle. Pan
Docs "Timer obscure behaviour", Timer overflow behavior. SameBoy
`tima_reload_state`: RELOADING is cycle A and RELOADED is cycle B
(`advance_tima_state_machine` in Core/timing.c; `GB_IO_TIMA`/`GB_IO_TMA` in
`write_high_memory`, Core/memory.c). Mooneye `tima_reload`,
`tima_write_reloading` and `tma_write_reloading` are the arbiters.
- **Cycle A** is the access at clock `overflow_at`:
  - TIMA reads 00.
  - A TIMA write cancels the reload and the interrupt: store the value,
    `tima_from = sc(t)`, clear `overflow_at`, reschedule.
  - DIV and TAC writes do *not* cancel it.
  - The reload + IF happen at the boundary that ends cycle A (the timer
    deadline `reloaded_at`).
- **Cycle B** is the access at clock `reloaded_at`, after that boundary:
  - TIMA already reads TMA and IF is set.
  - A TIMA write is ignored.
  - A TMA write also stores into TIMA.

  So cycle B needs `t == reloaded_at` kept until the next overflow is
  scheduled. Store it, don't infer it.
- **TAC write**:
  1. Sync TIMA to `t` (unless in cycle A).
  2. Apply the multiplexer glitch: the old input `sc(t) & old_bit` was 1
     and the new input is 0 → one increment (Pan Docs: Timer obscure
     behaviour; SameBoy `GB_emulate_timer_glitch`). Pan Docs says
     *disabling* the timer doesn't tick on CGB, while SameBoy ticks.
     Today's eager rule (tick) is kept until a test decides; it is one line
     in either model.
  3. Set `tima_from = sc(t)` and reschedule.
- **Glitch increment that overflows**:
  - `overflow_at = t` (the access clock) and `reloaded_at = t + mcycle`.
    This equals today's eager result: reload + IF visible from the next
    M-cycle, TIMA = 00 never observable.
  - A glitch increment *during* cycle A can't happen (clarified in step
    3/4): the selected counter bit just fell, so it and every bit below
    it are 0 at cycle A's access, and neither a DIV nor a TAC write sees
    a 1 -> 0 edge on it. The lazy model therefore never syncs TIMA in
    cycle A; the differential test checks it against the eager reference.
- **TMA write** outside cycle B just stores. The reload reads `tma` at the
  event, so a write in cycle A is used (SameBoy: TMA writes reach TIMA when
  the state isn't RUNNING).

### Serial bit clock and DIV-APU
- **Serial**, while a transfer runs on the internal clock:
  - deadline = `clock_of(next_fall(k, sc(now)))`, with k = 8, or 3 for the
    CGB fast clock (as today; SameBoy `GB_serial_master_edge`);
  - set on SC writes, after each bit, after a DIV write and after a speed
    switch;
  - one event per bit.
- **DIV-APU (M6)**: deadline = the next fall of counter bit 12, or 13 in
  double speed (DIV bit 4/5; Pan Docs Audio details "DIV-APU"; SameBoy
  `apu_bit = double_speed ? 0x2000 : 0x1000`). The APU's frame-sequencer
  step runs in that event. If M6 needs SameBoy's rising-edge
  `GB_apu_div_secondary_event`, it is a second deadline computed the same
  way.

### The event table (pulled forward from the M3 plan)
`Sys::next_event` becomes the minimum of a small fixed array of deadlines
(u64 clocks, `NEVER = u64::MAX`), indexed by a fieldless enum:
- `Timer`
- `Serial`
- `Ppu` (VBlank now, mode/STAT events in M3)
- `Dma`
- `Input` (the M2 scheduled button change)
- `DivApu` and `AudioBuffer` (M6)

The table is a few entries, not a heap or a sorted queue.
- `set_deadline(kind, t)` stores the entry and recomputes the minimum (≤ 8
  compares). It is called only when a deadline changes, never per M-cycle.
- `boundary()` runs every kind whose deadline is `<= now`, in that **fixed
  index order** (deterministic for events on the same clock), then
  recomputes the minimum and calls `reschedule()`.
- A register write that moves a deadline calls `reschedule()`, as LCDC
  writes already do.

### Interrupt visibility
Every scheduled IF source (timer, serial, PPU, and later DMA/APU) sets IF
inside `boundary()`, which runs at the end of the tick of the M-cycle whose
end reaches the deadline. That is the same point at which the eager tick
sets them today. So an event at clock `e`:
- is visible to every access at a clock `>= e`, rounded up to an M-cycle
  boundary. This covers IF reads, the dispatch's IE/IF samples after its
  pushes, and the HALT check;
- is visible to the interrupt check at the next instruction boundary.

Timer and serial deadlines are always on M-cycle boundaries. PPU deadlines
can fall inside an M-cycle and are observed at its end (unchanged from ADR
0008). Computed registers (DIV, TIMA, LY, STAT) use the exact access clock.

### HALT, STOP, lockup and power-off idle to the next deadline
With every IF source event-driven, nothing can change `IE & IF` while the
CPU is halted except an event or the caller between run calls. So the
halted step consumes the whole countdown in one go:
```text
n = max(1, ceil(budget / mcycle)); budget -= n * mcycle; boundary()
```
It then re-checks `IE & IF` as before. Every deadline still stops the skip,
including the capped one and the run target, so wake-up is on the same
M-cycle as with per-M-cycle idling. Measured on acid2: −98%. The same
applies to `Stopped`, `Locked`, `Off` and the speed-switch pause, which
becomes one budget subtraction plus `boundary()` calls at the deadlines in
between.

`Emulator::step` keeps its documented "one idle M-cycle while halted"
behavior: it builds its `Ctx` with `target = clock + 1`, so the countdown
is at most one M-cycle.

The M2 HALT tasks plug in unchanged:
- **IME=1 with an interrupt pending at HALT's check**: the check is after
  HALT's fetch M-cycle, so events at the end of that fetch are visible. PC
  is rewound to the HALT, so the pushed return address re-executes it
  (SameBoy `halt()`: `gb->pc--`).
- **`EI; HALT`**: the same path, because EI's delay has already set IME
  when HALT executes.
- **The CGB wake-up latency**: one extra idle M-cycle in the wake-up
  branch, after the skip has stopped at the deadline that set IF.

### OAM DMA and scheduled inputs (M2)
These must not put work back into the tick.
- DMA progress is lazy (bytes copied = elapsed M-cycles since the start,
  less the start delay). It is synced before an OAM/PPU access and
  finished at a `Dma` deadline.
- A bus-conflict check, if needed, goes in `read` (not the tick). `peek`
  stays conflict-free. Its cost is measured with `check-bench`.
- `set_buttons_at` is the `Input` deadline.

### Implementation notes
- The timer lives in `Sys` (`Sys::timer`), no longer in `Hot`. `Hot`
  shrinks to `budget`, `mcycle` and `deadline`, and `watch` goes away.
  `read_io` computes DIV/TIMA from `sys.timer` and `hot.clock()`.
  `read` stays side-effect free, so `peek` is unchanged.
- Timer, serial and DIV register writes go through one
  `#[inline(never)]` `Ctx::write_timer_io(reg, value)` (cold). This keeps
  `write_high` and `read_high` within their budgets. The prototype's
  inlined version grew them 68% and 25%.
- `Hw` grows by about 100 B (u64 counter references and the deadline
  table) from 264 B today; the `< 1 KiB` test still holds.

### Verification
"Bit-for-bit the same as the eager model" is checked at two levels:
1. **Unit differential test** (`timer.rs`, `#[cfg(test)]`).
   - The current eager `Timer` moves to a test-only `reference` module. It
     is first extended to the M2 reload-window rules and checked on the
     Mooneye timer ROMs. Eager per-M-cycle code is the easy place to get
     those rules right.
   - A fixed-seed xorshift generator (no dependency) drives both models
     through the same sequence:
     - advance N M-cycles;
     - write DIV/TIMA/TMA/TAC/SC with random values;
     - switch speed.
   - Writes are biased toward the clocks `overflow_at - m`, `overflow_at`
     and `reloaded_at`.
   - After every M-cycle it compares DIV, TIMA, TMA, TAC, the IF timer and
     serial bits, SB/SC, and the event clock. Thousands of sequences run in
     well under a second.
2. **Whole-machine fingerprint**: the fingerprint run described in Context,
   made permanent as `bench --fingerprint` plus
   `scripts/diff-fingerprint.sh <rev>` (builds `<rev>` in a temporary
   worktree and compares all corpus ROMs). The switch commit must show 0
   differences against its parent. The romtest baseline must show no
   status change, and the Mooneye timer group must keep its results.

## Consequences
- The per-M-cycle path is one subtract and a branch.
  - −28% to −33% Ir on the CPU/APU benchmarks.
  - Halted time costs almost nothing (−98% on acid2).
  - `run` is 5 KB smaller.
  - The benchmark baseline and the `run` budget drop in the switch commit;
    the cold I/O budgets are re-measured.
- Correctness now depends on every state change that moves a deadline
  calling `set_deadline` + `reschedule`: TAC/TIMA/DIV/SC writes, STOP,
  speed switch, reset/load. A missed one is a timing bug that the eager
  model couldn't have. The differential test and fingerprint are the
  safety net, as acid2/Mealybug are for PPU catch-up (ADR 0002).
- Event density is now the cost driver instead of M-cycle count:
  - a timer at 262 kHz with TMA = FF overflows every 4 M-cycles, and each
    overflow is a trip through `boundary()`;
  - the CGB fast serial clock shifts a bit every 4 M-cycles during a
    transfer.

  Neither is common in games. If a profile shows it, the fix is local to
  the timer event (e.g. reload in place without re-sorting), not a
  return to the eager tick.
- The edge arithmetic uses u64 shifts and subtractions on register
  accesses, cheap on a Cortex-M7, and only on the cold I/O paths.
- ADR 0008's "anything that must see every M-cycle goes into the tick" is
  tightened: nothing goes into the tick. OAM DMA and HDMA use deadlines and
  lazy progress, and any per-access cost is measured.
- Open behavior questions are independent of this decision and are the
  same one-line change in either model:
  - whether disabling the timer ticks TIMA on CGB (Pan Docs vs SameBoy);
  - the serial clock's phase after a DIV reset (SameBoy toggles a master
    clock on bit 7 edges, we use bit 8 falls).

### Public API
No change. `Emulator::step` keeps its documented one-M-cycle idle step (via
its run target), and `clock()`, `peek` and the run methods are
unaffected. `gbc_core::sm83::Bus` gains no required method: the halted skip
is a provided method `idle_halted()` defaulting to `idle()`, so the
sm83-tests bus is unchanged.

### Implementation order (M2 plan tasks)
1. Event table: `set_deadline`, ordered dispatch, PPU moved onto it. No
   behavior change; fingerprint identical.
2. `bench --fingerprint` + `scripts/diff-fingerprint.sh`.
3. Eager timer: the cycle A/B reload-window rules (Mooneye
   `tima_write_reloading`, `tma_write_reloading`); it then becomes the
   test-only reference.
4. Lazy timer + serial on the event table, the differential unit test,
   fingerprint identical to step 3, cold `write_timer_io`, then a new bench
   baseline and code-size budgets.
5. HALT/STOP/locked/off idle-to-deadline (`idle_halted`, `step` target),
   with a fingerprint check.
6. The HALT tasks (IME=1 re-execute, `EI; HALT`, CGB wake-up latency),
   then OAM DMA and `set_buttons_at` on deadlines.
