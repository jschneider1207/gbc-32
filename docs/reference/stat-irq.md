# STAT interrupt line and STAT writes (CGB)

The STAT interrupt is one line: the OR of every selected source (mode 0,
mode 1, mode 2, LY=LYC). An interrupt is requested only on its rising
edge, so a source that turns on while another one already holds the line
high requests nothing ("STAT blocking"). Pan Docs, "Interrupt Sources",
INT $48 STAT: https://gbdev.io/pandocs/Interrupt_Sources.html

## STAT writes on CGB C

- SameBoy writes STAT in two phases one dot apart: in the first the LYC
  select (double speed: the mode-0 select) keeps its old value, in the
  second the whole value lands (`Core/sm83_cpu.c`, `GB_CONFLICT_STAT_CGB`
  and `GB_CONFLICT_STAT_CGB_DOUBLE`). So a newly set LYC select acts one
  dot after the other selects, and in SameBoy the selects the write clears
  are cleared in the first phase.
- Hardware (gambatte `test/hwtests/miscmstatirq/lycstatwirq_trigger_*`,
  CGB results `E0`, measured on cgb04c): a write that switches from an
  active source to an active LY=LYC (mode 0 -> LYC at `LY = LYC = 44`,
  `08 -> 40` and `bf -> 40`; mode 1 -> LYC at `LY = LYC = 148`,
  `10 -> 40`) requests no interrupt. The line never goes low. With
  SameBoy's first phase it would be low for one dot (old source cleared,
  LYC select not yet set), and the second phase would raise it.
  gambatte's model is consistent with this: `statChangeTriggersStatIrqCgb`
  (`libgambatte/src/video.cpp`) compares the old and new selects as one
  step and never triggers while an old select's source is active
  (behavior only; gambatte is GPL).
- Our rule (`bus.rs`, FF41): SameBoy's two phases, but when the write sets
  the delayed select, the first phase also keeps the selects being
  cleared. They are cleared in the second phase, together with the
  delayed select's arrival. Writes that don't set the delayed select keep
  SameBoy's timing: clearing a select in the first phase is what gambatte
  `m2enable/*m0disable*`, `lycEnable/*disable*` need (making every
  clear land in the second phase regressed 10 of them).

## Line 144 and line 0 (single speed)

Sources: gambatte `video.cpp` `statChangeTriggersM0LycOrM1StatIrqCgb`,
`statChangeTriggersM2IrqCgb`, `mstat_irq.h` `doM1Event` and
`statRegChange` (behavior only, GPL); CGB04C results of gambatte
`m1/*`. gambatte's `timeToNextLy` T maps to our line position as
t = 7 - T on the next line (its line-144 mode-1 interrupt at T = 2 is our
VBlank at t = 5; its mode-2 interrupt at T = 4 is our `Ly3`, t = 3).

- The line-144 mode-2 quirk interrupt is at t = 3, as every line's
  mode-2 interrupt, and only once: SameBoy also requests it at VBlank,
  but `m1/m2m1irq_ifw_2` (an IF write at t = 3 leaves only VBlank) rules
  that out.
- The mode-1 interrupt is blocked by a mode-0 or mode-2 select as of
  t = 3 (`doM1Event` uses the selects latched at the last event or STAT
  write landing at `T > 4`). Our walk holds the line's mode part from
  t = 3 (sources modes 0 and 2) to VBlank.
- STAT writes at line 144 t = 3-4 and line 0 t = 3 neither raise nor
  lower the mode part (`MODE_HOLD`): a mode-0 enable at line 143 and a
  mode-1 enable at line 153 trigger only while `T > 4`, an old mode-1
  select blocks while `T > 3`, and such a write no longer changes the
  selects the mode-1 interrupt is blocked by. Double speed: the same
  from `T <= 6` half-dots, i.e. a first phase at line 144 t = 4 (the
  second phase lands half a dot before its whole dot).
- Single speed, a STAT write whose first phase lands at the
  comparison-off dot (t = 3, lines 1-152) sees LY=LYC off: the line
  drops to its mode part first (gambatte `getLycCmpLy`, `lycperiod`
  needs `timeToNextLy > 2` after the 2-cycle lead; `m1/m1irq_enable_
  after_lyc144_2`). Only the first phase: gambatte writes at one time,
  the first phase (`lycEnable/late_ff41_enable_lcdoffset1_1` regressed
  when the second phase also did it).
- Double speed, the second phase of a STAT write compares LY with LYC as
  of the dot before its whole-dot position (it lands half a dot after
  the first phase; `m1/lyc143_late_m0enable_lycdisable_ds_1`).

## Mode-2 write windows (lines 0-143)

From gambatte `statChangeTriggersM2IrqCgb` and `statChangeTriggersM0LycOrM1StatIrqCgb` (behavior only), same T mapping:

- Before a line's `t = 3` (its mode-2 interrupt) a STAT write sees the
  previous line's mode 0, not mode 2 (a mode-0 enable triggers while
  `T > 4`). CGB04C `m2enable/lyc1_m2irq_late_lycdisable_1`,
  `late_enable_after_lycint_disable_1`.
- A first phase sees the mode-2 source while `2 < T <= 4(1 + ds)`: to
  t = 5 in single speed (the walk's `CMP_WALK`), to t = 6 in double speed.
  CGB04C `m2enable/late_enable_ds_1`.
- Line 0, double speed: a first phase at t = 4-5 newly selecting mode 2
  (mode 0 not selected) requests the interrupt if the line was low
  (`ly == 153` branch, `2 < T <= 4` half-dots, t = 5 to 5.5; a write at
  t = 4 is before gambatte's event, after our walk's pulse). CGB04C
  `m2enable/late_enable_ly0_ds_1`, `_ds_lcdoffset1_1`.

## VBlank LYC interrupts and LYC writes (single speed)

- gambatte `lyc_irq.cpp`: the LYC interrupt for LYC 0 and 145-153 is
  blocked by the mode-1 select latched at the last STAT write landing
  more than 2 cycles before the event (`regChange`, `time_ - cc > 2`). A
  STAT write 1-2 dots before a VBlank comparison point (lines 145-153 at
  t = 3-4, line 153 at t = 10-11) may raise the mode part but not lower
  it (`MODE_1_STICKY`). CGB04C `lycEnable/lyc153_late_m1disable_2`,
  `lyc0_m1disable_2`, `lyc153_late_enable_m1disable_2`.
- LYC writes at line 153 (`lycRegChangeTriggersStatIrq`): up to our
  access t = 6 (view 7) the new value is compared with 153; from access
  t = 3 the old LYC (152) no longer holds the line; from line 153's
  gambatte start (access t = 7) on it is compared with 0, so SameBoy's
  12-23 window comparing 153 stays for double speed only.
  CGB04C `lycEnable/lyc153_late_ff45_enable_{3,5,lcdoffset1_1}`.
- LCD off: a STAT write newly selecting LY=LYC while the kept coincidence
  bit is set requests the interrupt (gambatte `memory.cpp` FF41).
  `lycEnable/lcdoff_lycirqen_1`.

## Open

- `lycstatwirq_trigger_ly00_10_50_1` and `_ds_1` (want `E0`): the write
  `10 -> 50` (adds LYC) at the end of line 153 with LYC = 0. The mode-1
  source ends at line 0 dot 4 in our model and in SameBoy (display.c,
  line 0's `ly_for_comparison = 0` / `mode_for_interrupt = 2` update).
  The LYC select lands at dot 4 in single speed (no interrupt expected,
  dot 5 expected one), so hardware treats the mode-1 end and the select's
  arrival at the same dot as simultaneous. Making every second-phase
  write simultaneous with the PPU update of its dot regressed 13 ROMs.
  Double speed needs the mode-1 source to last past dot 5. Not modelled.
