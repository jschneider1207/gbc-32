# A1: CPU, bus, interrupts, timer, serial, joypad, scheduler, HALT/STOP

Reviewer A1, dimension A (emulation correctness). Tree: HEAD of `main` (49c0756).
Read-only; nothing was built or run. Every finding below was traced through the
code path by reading; none was confirmed by executing a test, so each says what
would confirm it. Paths are relative to `/home/jess/gbc-32/crates/gbc-core/src/`.

Overall: the CPU core, the lazy timer, the serial bit clock and the event table
are in good shape. I found no wrong M-cycle order, no missed or late event in
normal operation, and no clock-counter overflow on long runs. The defects are
at the edges: plain STOP mode (still a stub), one unimplemented register, and
one stale-deadline window inside the speed switch. The larger problem is
structural: the speed switch and the sub-M-cycle timing are fitted case by
case, and state is squeezed into unused bits because of the `Hw` size budget.

Counts: 0 high, 2 medium, 7 low.

---

### A1-01 Plain STOP mode is still a stub: the machine keeps running and any button wakes it   [severity: medium] [confidence: high]
- Where: `cpu/mod.rs:513-524` (`Mode::Stopped`), `cpu/mod.rs:654`
  (`StopKind::Stop`), `bus.rs:2126-2128` (`stop_wakeup`), `cpu/mod.rs:260`
  ("stub until M5"). Open plan items: `docs/plan/stage-1.md` lines 112, 113, 141
  (all unchecked, all from M5 or earlier).
- What: in `Mode::Stopped` the CPU just calls `idle_halted()`, so every
  deadline keeps firing. Three things differ from hardware:
  1. DIV, TIMA and the serial bit clock keep counting (they are functions of
     the clock), the timer keeps requesting interrupts into IF, the PPU keeps
     drawing and reporting `FRAME_READY`, the APU keeps playing, OAM DMA and
     HDMA keep running.
  2. `stop_wakeup` is `!buttons.is_empty()`: any held button wakes the CPU,
     whatever P1 selects. `stop_begin` (`bus.rs:2088-2098`) does use the
     selected lines (`p1_lines(..) != 0x0F`), so the two tests disagree.
  3. The wake-up costs one M-cycle; SameBoy's is 4 + 8 clocks.
- Scenario: `LD A,$30 / LDH (P1),A / STOP` with A held. `stop_begin` sees
  lines = 0F, so `StopKind::Stop`; the next step sees `buttons != 0` and wakes
  after one M-cycle. Hardware stays stopped (no P10-P13 line can go low).
  Second scenario: a game that parks in STOP with P1 = 00 until a key press,
  with the timer enabled: on wake DIV has advanced by the whole wait, IF bit 2
  is set, and music played throughout; on hardware the counter was frozen from
  the DIV reset.
- Reference: Pan Docs "Using the STOP instruction"; SameBoy `Core/timing.c`
  `timers_run` (returns early while `gb->stopped`), `GB_advance_cycles`
  (`if (!gb->stopped) GB_dma_run`), `Core/sm83_cpu.c` `GB_cpu_run` (the
  stopped branch wakes on `JOYP & 0xF != 0xF`, then `leave_stop_mode` and 8
  more cycles), `GB_display_vblank` (blank while stopped).
- Impact: no test ROM in the manifest enters plain STOP, so this is entirely
  unarbitrated. I did not check which commercial CGB titles use plain STOP;
  that is the open question for prioritising it.
- Proposed change: effort M, risk low (plain STOP is isolated from the
  speed-switch path).
  - Wake on `p1_lines(buttons, joyp_select) != 0x0F`, the same expression as
    `stop_begin`.
  - Freeze the counter with the mechanism the speed switch already has:
    `Timer` rebases with a future `base_clock`. Add `Timer::freeze(now)`
    (`base = sc(now)`, `base_clock = NEVER`) and `Timer::thaw(now)`
    (`base_clock = now`, `schedule_overflow`), and set the Timer, Serial and
    DivApu deadlines to `NEVER` on entry and recompute them on wake.
  - Blank the screen (`screen_blank`), stop HDMA triggers and OAM DMA steps
    (plan line 141).
  - Tests (unit, no ROM exists): DIV and TIMA read the same before and after
    1 s of STOP; IF bit 2 is not set during it; A held with P1 = 30 does not
    wake; A held with P1 = 10 does.

### A1-02 The speed switch to single speed runs the event loop with stale deadlines; a fast-clock serial bit can shift twice   [severity: low] [confidence: medium]
- Where: `bus.rs:1554-1633` (`Ctx::speed_switch`), specifically the
  `self.hot.budget -= 3; if self.hot.budget <= 0 { self.boundary(); }` block
  at `bus.rs:1601-1610`, which runs before `schedule_inputs`,
  `set_timer_deadline` and `schedule_serial` at `bus.rs:1611-1616`.
  `bus.rs:788-793` (`serial_event`), `serial.rs:124-131` (`next_shift`).
- What: `timer.speed_switch(now, pause)` has already rebased the counter, but
  the Serial deadline in the table is still the one computed for the old
  speed. `boundary()` then dispatches it at `now + 3`. The handler shifts a
  bit and re-arms from `next_shift(sc(now + 3))`. If the counter is still
  frozen there (IME off: `pause` = 3, so `base_clock = now + 3`), the counter
  has not reached the edge that "fired", so `next_shift` returns the same edge
  again and it fires a second time 4 clocks later.
  The same block has a second, smaller ordering fault: `set_apu_deadline(now)`
  (`bus.rs:1598`) changes the DivApu deadline without a reschedule, so a
  DivApu edge at `now + 3` is handled at `now + 7` when the stale budget was
  above 3.
- Scenario (by reading; not run): CGB mode, double speed, IME off (the usual
  state around a switch), SC = 83 (fast clock, transfer running), nothing
  pending, KEY1 armed, STOP. Let X be the end of STOP's fetch.
  - X: `stop_begin` resets DIV, counter = B (a multiple of 2^16). If the
    flip-flop is 1 after the reset, `phase` = 1 and the next shift is at
    counter B+8, deadline X+4.
  - X+2 (`now`): second byte read; `speed_switch`. Counter = B+4.
    `Timer::speed_switch(now, 3)`: base = B+4, `base_clock` = now+3.
  - Budget is at most 2 (the stale Serial deadline is now+2), so `-= 3` calls
    `boundary()` at now+3. `serial_event(now+3)`: one shift. `sc(now+3)` is
    still B+4, so `next_shift` gives B+8 again, deadline now+7.
  - now+7: the same edge shifts a second bit. The byte completes one bit
    period (16 clocks) early, and so does the serial interrupt.
  With IME on (`pause` = -1) the stale dispatch happens to land on the right
  clock (`sc(now+3)` = B+8), so it is correct by coincidence.
- Reference: the module's own invariant (ADR 0011: each handler sets its next
  deadline from the counter at the event's clock; one edge, one event).
- Impact: needs a fast-clock transfer in flight across a switch to single
  speed with a particular flip-flop phase. Not plausible in a real game. It
  matters as the one place I found where an event can fire twice, and as a
  sign that this function interleaves "change the grid" and "run events".
- Proposed change: effort S, risk low. Recompute every counter-derived
  deadline (Timer, Serial, DivApu, Input) right after
  `timer.speed_switch(..)`, call `reschedule()`, and only then idle the 3 odd
  clocks. Test sketch: set up the scenario above through `Ctx`, run to the
  end of the halt, assert `serial.bits` advanced by exactly one per
  flip-flop period (or compare against `timer::reference` extended with the
  pause).

### A1-03 FF56 (RP, infrared port) is not implemented   [severity: low] [confidence: high]
- Where: `bus.rs:1233-1274` (`write_io`: FF56 falls into `_ => {}`),
  `bus.rs:2329-2343` (`read_io_other`: FF56 reads FF).
- What: in CGB mode RP has writable bits 0 (LED), 6 and 7 (read enable), bit
  1 reads 1 with no light received, and the rest read 1. Here writes are
  dropped and reads return FF in both modes.
- Scenario: CGB-mode code writes 00 to FF56 and reads it back: hardware 3E,
  here FF. Writes C0 and reads back: hardware FE, here FF. Post-boot read:
  hardware 3E (Pan Docs power-up table, CGB column), here FF.
  Games that probe the port by read-back (the Pokemon Gold/Silver/Crystal
  Mystery Gift code and others use RP) see bits 0, 6, 7 stuck at 1. Bit 1
  ("no signal") is right, so the likely result is only "no partner found";
  I did not verify any title.
- Reference: Pan Docs "FF56 - RP"; SameBoy `Core/memory.c` `read_high_memory`
  `GB_IO_RP`: `if (!cgb_mode) return 0xFF; ret = (RP & 0xC1) | 0x2E` plus
  `0x10` on non-E revisions; own LED is read back as a signal when bits 6-7
  are set.
- Proposed change: effort S, risk none. Store `value & 0xC1` in `mem.io[0x56]`
  in CGB mode; read `0x3E | stored`, with bit 1 cleared when bits 6-7 are set
  and bit 0 is set (the LED sees itself, per SameBoy). DMG compatibility mode
  stays FF. Unit test next to `undocumented_cgb_registers`.

### A1-04 `VramDma::switch_end` is a 32-bit clock with two meanings and no validity bound   [severity: low] [confidence: medium]
- Where: `hdma.rs:94-101`, `bus.rs:1626`, `bus.rs:1638-1640`, `bus.rs:1670`,
  `bus.rs:1787-1794` (`vdma_hblank`), `bus.rs:1922-1925`.
- What: the field holds the low 32 bits of a clock and is compared with
  `wrapping_sub(..) as i32`. That is exact only within 2^31 clocks (512 s of
  emulated time). The speed-switch use is bounded (the halt is 0x20000
  clocks). The `refetch == RAN` use is not: `RAN` is only cleared by the next
  `VramDma` event or the next HALT (`hdma_refetch`, `bus.rs:2141-2152`).
- Scenario: the last block of an HBlank transfer was requested in a HALT's
  M-cycle (`refetch` = `RAN`, transfer now inactive, so no more `VramDma`
  events). The game does not HALT again and starts the next HDMA between
  2^31 and 2^32 clocks later (8.5 to 17 minutes). The first trigger reads the
  wrapped difference as negative and drops that block. A speed switch in
  between also overwrites `switch_end` while `RAN` still refers to it.
  A game that halted once almost certainly halts every frame, so this is a
  robustness issue, not a likely failure.
- Proposed change: effort S, risk none. Clear `refetch` when the transfer
  ends (`v.hblank = false` in `vram_dma`) once the stall is over, or store
  the end as a `u64`. Give the speed-switch halt its own field (see A1-05 on
  why it shares one).

### A1-05 State is stored in unused bits and other structs' padding to stay under the `Hw` size budget   [severity: low] [confidence: high]
- Where: `bus.rs:35-39` and `bus.rs:838-865` (a pending LYC interrupt and its
  M-cycle parity in IF bits 5-6), `timer/mod.rs:120-133` (`apu_ds_delay` in
  `Timer`: "fits this struct's padding"), `hdma.rs:94-101` (`switch_end`:
  "not VRAM DMA state"), `bus.rs:2054-2056`. STATUS already lists the IF
  bits and the TILE_SEL latch in `Memory` for the same reason ("`Hw` at its
  864 B limit").
- What: three pieces of state live where they don't belong because `Hw` may
  not grow. The IF case is a hazard: `Ctx::set_interrupt_flags`
  (`bus.rs:2054`) assigns `value & INT_MASK` and would silently drop a
  pending LYC interrupt. It is unreachable today only because `Ctx` overrides
  `push_acknowledge`, the single caller. Any new code that assigns `iflags`
  has the same trap; every reader must remember `& INT_MASK`.
- Scenario: none today (I checked every `iflags` assignment in non-test
  code: `bus.rs:122, 175, 444, 759, 841, 860, 1201, 1225, 1228, 2046, 2049`
  all preserve bits 5-6 or run where none can be pending, `reset` aside).
- Reference: `lib.rs:264-271` says the 864 B limit "is not tight" and exists
  to keep `Emulator` stack-friendly on the MCU.
- Proposed change: effort S, risk low. Either raise the `Hw` limit by 16-32 B
  (needs the human: it is a stated budget), or move cold scalar state into
  `Memory` as zero-initialised fields, which the architecture already allows.
  Then give the LYC delay a real field (`lyc_irq_at: u64`, which also removes
  the parity trick and `lyc_irq_due`'s second branch), move `apu_ds_delay` to
  the APU and split `switch_end`. Until then, make
  `Ctx::set_interrupt_flags` preserve the high bits.

### A1-06 Speed-switch and STOP timing is a composite of three emulators' models, joined by fix-ups   [severity: medium] [confidence: medium]
- Where: `bus.rs:1574-1626` (`freeze`, `short`, the 3-clock idle,
  `switch_end`), `timer/mod.rs:359-384` (`stop_edge`), `timer/mod.rs:386-407`
  (`Timer::speed_switch` with a signed pause), `bus.rs:526-535`
  (`stop_div_reset`), ADR 0020.
- What: the base model is SameBoy's (DIV reset at the end of STOP's fetch;
  halt of 0x20008 T-cycles). SameBoy models CGB E. The target is CGB C, so
  AGE results verified on B/C were added on top: `stop_edge` re-creates the
  TIMA edge that a DIV reset one M-cycle later would have made (with a
  carve-out for the 4 kHz timer), and `freeze += 4` for the pending case to
  double speed. Gambatte supplies the PPU's one-clock offset (`short`). Each
  constant is correct for the ROMs that fixed it, but nothing ties them
  together: `stop_edge` exists only because the reset is at the wrong
  M-cycle for the revision being emulated.
- Scenario: no failing ROM (all speed-switch ROMs pass). The risk is in
  untested combinations, e.g. a DIV-APU edge or a serial edge inside STOP's
  second byte: `stop_edge` patches TIMA only, so the frame sequencer and the
  serial flip-flop still see the reset one M-cycle early relative to AGE's
  model. A1-02 comes from the same function.
- Reference: ADR 0020 (its "Open" section describes AGE's model: DIV reset
  after STOP's second byte, halt of 0x8000 new-speed M-cycles with the timer
  running, the 4 kHz carve-out); AGE `gb_timer::after_div_reset`.
- Proposed change: effort M-L, risk medium (242 gambatte speedchange ROMs and
  the AGE spsw set are the guard). Move the DIV reset to after the second
  byte, as on the verified revision, and delete `stop_edge`; express the
  holds as one table (IME, pending, direction) in one place with the source
  of each row. Do it as an experiment on a branch: if the ROM count drops,
  the current fit stays, and the experiment still documents which rows are
  derived and which are fitted.

### A1-07 Sub-M-cycle access timing is decided separately at each site   [severity: low] [confidence: medium]
- Where: `bus.rs:1190-1202` (IF write lands at +1 clock), `bus.rs:2032-2051`
  and `bus.rs:1213-1230` (dispatch acknowledge: PPU bits cleared at
  mid-M-cycle, timer/serial/joypad bits at its end), `bus.rs:665-696` (NR52
  power-on tests the counter 4 ticks on), `bus.rs:838-865` (LYC interrupt 5
  dots after the access), plus the PPU register writes "one dot later".
- What: the model is M-cycle granular, and each register that needs finer
  timing carries its own offset in its handler, each with its own comment and
  ROM list. The acknowledge asymmetry is the clearest sign: SameBoy clears
  the IF bit 2 T-cycles into the last M-cycle for every source; here the
  timer and serial get "after the M-cycle" because their events only exist
  on boundaries and the gambatte ROMs need them absorbed. That says the
  timer's IF edge is placed up to half an M-cycle late relative to the clear
  point, and the rule compensates.
- Scenario: none failing. The cost is that a new register or a corrected
  offset has no single place to go, and offsets can't be checked against
  each other.
- Reference: SameBoy `cgb_conflict_map` / `cycle_write` (one table of write
  phases per register), `GB_cpu_run` (`pending_cycles - 2`).
- Proposed change: effort M, risk low if done as a pure refactor. One
  `const` table (or small enum per register) giving each I/O write its phase
  inside the M-cycle, and one constant for the IF-acknowledge point; give the
  timer and serial IF edges an explicit phase so the acknowledge needs no
  per-source rule. Guard: the full manifest, bit-identical baseline.

### A1-08 `idle_speed_switch` makes no progress if the halt end is ever off the M-cycle grid   [severity: low] [confidence: medium]
- Where: `bus.rs:1668-1676`, `bus.rs:1638-1640`.
- What: `left = (switch_end - clock) / m`, `n = min(.., left)`. With 0 <
  `switch_end - clock` < `m`, `n` is 0, no time passes, `boundary()` is never
  reached, `done` is never set, and `Cpu::run` spins forever. Today
  `switch_end` is always a whole number of M-cycles ahead (to single speed
  the 3 odd clocks are idled first), so this is unreachable. It depends on
  that alignment holding through every future change to the function A1-02
  and A1-06 are about. On the MCU a hang here is a frozen console with no
  event reported.
- Proposed change: effort S. `let n = n.max(1)` (then `speed_switch_over`
  ends the halt on the next step), plus a `debug_assert!` on the alignment in
  `Ctx::speed_switch`.

### A1-09 Stale comments in the reviewed files   [severity: low] [confidence: high]
- `cpu/mod.rs:260`: "STOP (stub until M5)"; M5 is tagged and it is still a
  stub (A1-01).
- `bus.rs:2308`: "TODO(M3): reads 0xFF while the PPU blocks OAM";
  `read_oam` (`bus.rs:2358`) already does this for FEA0-FEFF.
- `schedule.rs:25-28`: "`Timer`, `Serial`, `Ppu` and `Input` are scheduled so
  far. The others are declared now..."; all eight kinds have handlers.
- `timer/mod.rs:305-310`: TAC-disable glitch "kept until a test decides";
  the manifest has gambatte `tima/tc0x_late_stop_inc_*` and
  `tc0x_1stopstart_*` (cgb04c), which exercise exactly this. Either cite
  them or say what is still open.
- Proposed change: effort S; fix with the next commit that touches each file.

---

## Edge cases no test covers (in this subsystem)

- Plain STOP mode in any form (A1-01).
- FF56 in CGB mode (A1-03).
- A serial transfer or a DIV-APU edge in flight across a speed switch
  (A1-02); the timer differential test covers the timer across a switch, not
  the serial deadline ordering inside `Ctx::speed_switch`.
- TIMA write in cycle A cancelling the *interrupt* as well as the reload.
  `Timer::write_tima` follows Pan Docs (both cancelled). SameBoy
  (`advance_tima_state_machine`) still sets IF. Mooneye
  `tima_write_reloading` checks TIMA values only, as far as I remember; worth
  confirming which gbmicrotest/gambatte ROM pins the IF half, and citing it
  in `timer/mod.rs`.
- Emulated runs longer than 2^31 clocks (A1-04); the countdown cap itself is
  tested (`clock_countdown_is_capped_and_exact`).
- IF high-bit storage against a future `iflags` assignment (A1-05).
- Header 0143 values with bit 7 set and bits 2-3 non-zero (KEY0 "PGB"
  modes): treated as plain CGB mode. No commercial cartridge uses them.

## Bigger-picture recommendations

Ranked by value over cost.

1. **Finish STOP (A1-01).** It is the only place in this subsystem where
   behaviour is knowingly wrong and visible, the plan items are three
   milestones old, and the counter-freeze mechanism it needs already exists.
   Small, isolated, testable with unit tests.
2. **Lift the `Hw` budget pressure, then undo the workarounds (A1-05,
   A1-04).** Three pieces of state are in the wrong place for one reason.
   Cheap once the human agrees to 16-32 more bytes or to cold state in
   `Memory`; removes a latent IF hazard and a 32-bit clock.
3. **Make `Ctx::speed_switch` two phases: rebase everything, then advance
   time (A1-02, A1-08).** Small change, removes the only double-fire I found
   and the alignment dependency.
4. **Re-derive the speed switch on the CGB B/C model (A1-06)** as a branch
   experiment. Highest uncertainty, but it is the one area of this subsystem
   built by fitting rather than from a model, and the target revision has a
   verified description available.
5. **One table for sub-M-cycle phases (A1-07).** A refactor with no
   behaviour change; worth doing before any further timing work adds a sixth
   and seventh local offset.
6. **CPU modes for a pre-fetched opcode (lower priority, investigate
   only).** `Mode::HaltBug`, `Mode::Latched` and `Mode::Prefetched`, the
   `LATCHED` sentinel, `Bus::fetch(latched)`, `fetch_peek`, `hdma_refetch`
   and the two PC rewinds (`halt`, the latched dispatch) are all versions of
   "the next opcode was already read when something intervened". They differ
   in whether PC advanced, whether the fetch M-cycle reads or idles, and
   whether an interrupt check runs, so a single "pre-fetched opcode" slot
   would still need those three flags. I am not confident a unified model
   is simpler or as fast in `Cpu::run`; I list it because the `Bus` trait now
   has about ten methods that exist only for HDMA and speed-switch cases, and
   that growth is worth stopping. Not a defect.

## Looked at and found fine

- **Instruction set (`cpu/exec.rs`)**: M-cycle order of every opcode family
  against GBCTR (PUSH/CALL/RST internal cycle first, RET cc, ADD SP,e8,
  LD HL,SP+e8, LD (nn),SP, JP HL, CB (HL) forms, BIT (HL)); flags of ALU,
  DAA, ADD HL, SP+e8, rotates; POP AF low nibble; invalid opcodes lock.
- **Interrupt dispatch**: 5 M-cycles, IE sampled after the high-byte push and
  IF after the low-byte push, cancellation to 0000 with no IF bit cleared,
  vector re-selected from IF at the last M-cycle, push onto IF itself.
  Matches SameBoy `GB_cpu_run`.
- **EI/DI/RETI**: EI delay of one instruction, DI cancelling a pending EI,
  dispatch clearing `ei_pending`, RETI immediate.
- **HALT**: check after the fetch M-cycle; IME=1 with pending rewinds PC;
  HALT bug (including HALT; HALT hanging, as on hardware); `EI; HALT`; CGB
  +1 M-cycle wake-up; the `debug_assert` in `Mode::HaltBug` holds on every
  path, including `Prefetched`.
- **Halted skip (`idle_halted`)**: wakes on the same M-cycle as per-M-cycle
  idling; capped countdown handled in i64; inputs, run target and stop events
  all end the skip.
- **Event table (`schedule.rs`)**: incremental minimum and the slow-kind
  cache are correct; `run_events` cannot loop (every handler re-arms strictly
  after `now`: timer, serial, input checked); same-clock order is fixed.
- **Countdown (`Hot`, `reschedule_at`, `boundary`)**: clock = deadline -
  budget is exact; a deadline set at or before `now` by a write is handled at
  that M-cycle's end; no path leaves a moved deadline without a reschedule,
  except inside `speed_switch` (A1-02).
- **Timer (`timer/mod.rs`)**: unwrapped u64 counter (no overflow in any
  realistic run, even with a DIV write per instruction); falling-edge
  counting; cycle A / cycle B for TIMA, TMA, TAC and DIV writes; DIV- and
  TAC-write glitch increments including an overflow in the write's own
  M-cycle; double speed; post-boot phase (ADR 0022). The freeze window after
  a switch cannot be reached by a register access.
- **Serial (`serial.rs`)**: flip-flop as counter bit XOR phase; SC write
  forcing the flip-flop low with the extra shift; DIV reset edge;
  `next_shift` arithmetic; fast clock only in CGB mode; SB = FF after a
  transfer; SC read masks in both modes.
- **Joypad**: P1 line logic, interrupt on a selected line going low from a
  button or from a select write, `InputQueue` ordering and rounding (ADR
  0012), post-boot select bits per mode.
- **Memory map and I/O decode**: echo RAM with the SVBK bank; FEA0-FEFF on
  CGB C; HRAM/IE; unused I/O reads FF; unused bits of P1, SC, TAC, IF, KEY1,
  VBK, SVBK, BCPS/OCPS, FF6C, FF72-FF75; CGB-only registers locked in DMG
  compatibility mode (KEY1, VBK, SVBK, HDMA, BCPD/OCPD, FF74).
- **Run loop and public API (`lib.rs`)**: `run_until`/`run_cycles`/`step`
  semantics, overshoot, early return when the target has passed,
  `set_buttons_at` edge cases, state independent of how calls are split.
- **Determinism**: no host time or randomness; all state is reset from
  constants and the ROM header.
- Not reviewed (other reviewers): PPU register writes and STAT/LY timing,
  OAM DMA conflicts, VRAM DMA internals, APU, cartridge mappers.
