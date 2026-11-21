# 0012: Scheduled button changes (`set_buttons_at`)

- Status: Accepted
- Date: 2026-11-21

## Context
The acceptance contract (`acceptance/README.md`) applies a script's button
changes at emulated times rounded down to an M-cycle. `run_until` ends only
on instruction boundaries, so a runner that calls `set_buttons` between run
calls applies each change up to one instruction late (and up to the whole
2050 M-cycle speed-switch pause late). That moves P1 reads and the joypad
interrupt by an amount that depends on where run calls end. ADR 0011
reserved the `Input` deadline of the event table for this; the per-M-cycle
tick must not grow.

## Decision
`Emulator::set_buttons_at(clock, buttons) -> bool` schedules a change of
the held buttons at emulated time `clock` (base clocks).

- **Queue:** up to `SCHEDULED_INPUTS` (= 4, public) pending changes, in a
  fixed array sorted by clock (`joypad::InputQueue` in `Sys`, 40 B; no
  alloc). The earliest is the `Input` deadline. A second change scheduled
  before the first fires is simply queued in clock order, whatever the
  call order. A change at a clock that already has one replaces it (the
  later call wins). When the queue is full a new future clock is refused:
  the call returns `false` and changes nothing (`#[must_use]`). One slot
  would be enough for a front end; four let a script runner keep several
  changes queued, so changes closer together than one run call's overshoot
  still land exactly (see romtest below).
- **Rounded down to an M-cycle, as in the contract.** A change at `clock`
  lands at `d`, the start of the M-cycle that `clock` falls in (the last
  boundary at or before it). The queue keeps the raw clock; the `Input`
  deadline is `clock - (mcycle - 1)`, so the countdown stops exactly at
  `d`, and the handler takes every change with `clock < now + mcycle`.
  The deadline depends on the M-cycle length, so the speed switch resets
  it (`Sys::schedule_inputs` in `Ctx::stop`) before the pause. Without
  that, a change scheduled in double speed for after a switch back to a
  normal-speed grid at 2 mod 4 would land late (test
  `scheduled_input_rounds_down_on_the_grid_after_a_speed_switch`). For a
  switch to double speed the stale deadline is only early: the handler
  rounds with the live M-cycle, so it would wake at a boundary with
  nothing due and land the change on its own one anyway (checked by a
  brute force over the clocks after the pause; the reset keeps the
  deadline exact rather than relying on that). `d` is computed on the new
  grid even when it falls inside the switch's pause: the pause idles one
  deadline at a time and runs `boundary()` at each, so the change lands
  on `d` during the pause and sets IF bit 4 there
  (`scheduled_input_inside_the_speed_switch_pause_lands_on_its_mcycle`).
  Only if `d` is before the switch's clock (already past when the grid
  changes, e.g. double to normal speed at `T` with `clock = T + 2`, whose
  normal-speed M-cycle starts at `T - 2`) does it land at the first
  boundary after the switch, `T + 4`
  (`scheduled_input_already_past_after_a_speed_switch_lands_next`). Cost:
  a subtraction where the deadline is set, all off the tick.
- **Past clocks apply now.** If `d <= clock()`, i.e. `clock < clock() +
  mcycle`, the call behaves exactly like `set_buttons(buttons)` and always
  succeeds. Between run calls the clock is on an M-cycle boundary and
  every queued change is in the future, so "now" is the correct M-cycle
  and the timeline order is kept. Rejecting would only make callers branch
  on something they can't avoid (a change at time 0, or one that a long
  instruction overshot).
- **Exact timing.** The `Input` handler runs in `Ctx::boundary` at `d`
  (the end of the M-cycle before it), like every scheduled IF source (ADR
  0011, "Interrupt visibility"). So:
  - a P1 read whose access starts at `d` or later sees it; one at `d - 4`
    (`d - 2` in double speed) doesn't;
  - the joypad interrupt (a P10-P13 line going low, `p1_interrupt`) sets
    IF bit 4 at `d`: visible to IF reads and HALT's check from `d`, and to
    the interrupt check at the next instruction boundary.
- **Same-boundary changes collapse (a modelling choice).** All changes
  due at one boundary are applied as one transition to the last of them,
  so a press and release inside one M-cycle raises no interrupt. No CPU
  access can sample P1 between them, but whether the hardware's
  interrupt edge detector would catch such a glitch is not established;
  we don't claim this is hardware-exact. SameBoy differs: `GB_set_key_state`
  calls `GB_update_joyp`, which sets IF on every falling edge at once,
  however close together the changes are.
- **`u64::MAX` is refused.** It is the empty-slot sentinel (`NEVER`), so a
  change there would hold a slot and never fire; `set_buttons_at` returns
  `false` for it.
- **Interactions.**
  - `set_buttons` is `set_buttons_at(clock(), ..)`: it applies now and
    leaves pending changes queued.
  - Halted, stopped, locked and powered-off CPUs idle with ADR 0011's
    skip (`idle_halted`: consume the countdown to the next deadline in one
    go), which stops at every deadline in the table, the `Input` one
    included.
  - HALT: the skip ends at `d`, where `boundary()` applies the change and
    sets IF; the next step's `IE & IF` check sees it at `d` and wakes the
    CPU (with the CGB's extra idle M-cycle). This is the same clock as
    `set_buttons` once the clock is at `d`, and as stepping one M-cycle at
    a time (`Emulator::step`, no skip); the unit test
    `scheduled_input_wakes_halt_on_its_mcycle` checks both equivalences.
  - STOP: the stopped step idles (skips) to `d` and then checks
    `stop_wakeup` (any button held), so a scheduled press wakes STOP at
    `d`, as stepping one M-cycle at a time does
    (`scheduled_input_wakes_stop_on_its_mcycle`). A `set_buttons` at `d`
    instead wakes it one M-cycle later, because the stopped step's check
    at `d` ran before the call; the scheduled change is the one that
    matches a press landing at `d`.
  - `load`, `reset` and `eject` rebuild `Sys`, dropping pending changes
    (the clock restarts at 0); held buttons are kept as before.
- **Determinism and save states.** Changes are keyed to the emulated clock
  and applied in `boundary()` in `EventKind` order, so a run is the same
  however it is split into run calls. The queue is emulated state: a save
  state (not designed yet) must store it with the event table.
- **No tick cost.** Nothing is added to the per-M-cycle tick; the handler
  is one more arm in `run_events`, reached only at the `Input` deadline.

### romtest
The runner hands the machine every change its queue takes before each run
call and runs to the exit limit. When the queue is full it runs to the
earliest queued change, so the queue drains and is refilled. Only more
than four changes inside one run call's overshoot (up to ~8200 clocks in
a speed-switch pause) can still be late. The runner never hands over a
change whose clock (before the exit limit) has already passed: the run
ends as `crash` with the detail "input at clock <t> could not be
scheduled on its M-cycle (queue limit) ...". No stage 1 group comes close
(at most 6 changes, 0.1 s apart).

## Consequences
- Button changes in acceptance runs land on their M-cycle, independent of
  run-call boundaries. The acceptance README's interim note ("a change
  lands at the first instruction boundary at or after its M-cycle") is
  removed.
- The `Machine` trait (ADR 0007) gains `set_buttons_at`; the fake
  emulates the queue.
- Public API: `Emulator::set_buttons_at`, `SCHEDULED_INPUTS`.
