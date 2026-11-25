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
- Our rule (`bus/ppu_io.rs`, FF41): SameBoy's two phases, but when the write sets
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
  (`ly == 153` branch, `2 < T <= 4` half-dots, t = 5 to 5.5; the walk's
  pulse is at 5 too, before the write, see below). CGB04C
  `m2enable/late_enable_ly0_ds_1`, `_ds_lcdoffset1_1`.

## Line 0's mode-2 pulse and line 153's LYC=0 (the walk)

The two references disagree by one dot on both points:

| point | SameBoy (`display.c`, MIT) | gambatte (behaviour only) | ours, both speeds |
|---|---|---|---|
| line 153, comparison against 0 (`L153Zero`) | 12: `ly_for_comparison = 0` after 2 + 4 + 2 + 4 dots | 13: the LYC=0 interrupt at line 153 cycle 6 (`lyc_irq.cpp`) | 13 (`edges::L153_ZERO_WALK`) |
| line 0, the mode-2 pulse (`Ly4`) | 4: `mode_for_interrupt = 2` one dot after the LY update | 5: `mode2_irq_line_cycle_ly0` = 454, two cycles after every other line's 452 (our t = 3) | 5 (`edges::LY0_PULSE`) |

CGB04C decides for gambatte's dots, in both speeds:

- single speed: `ly0/lycint152_lyc0irq_ifw_1` and
  `lyc153int_m2irq/lyc153int_m2irq_ifw_1` (an IF write landing on 153:12
  and on 0:4 does not clear the request; `_2`, 4 dots later, does);
  `lcd_offset/offset3_lyc99int_m2irq_count_1` (the one CPU phase with an
  M-cycle boundary on 0:4: its IF read there doesn't see the pulse; `_2`
  reads at 0:8);
- double speed: the `late_retrigger_ds` and `lcdoffset1` rows below.

Until 2026-11-24 the single-speed walk had SameBoy's dots (12 and 4) and
delayed only the IF request by a dot (review A2-02, E4). Now the walk
points are at 13 and 5 in both speeds and request at once.

gambatte's `doM2Event` blocks line 0's pulse by the mode-1 select
(`blockedByM1Irq`). Line 0's first point (`Ly3`, `edges::ly0_pt`):

| point | single speed | double speed |
|---|---|---|
| line 0 `Ly3`: comparison against 0; double speed: the mode-1 source ends, unless mode 2 is selected | 3 | 4 |

In single speed the mode-1 source holds the line to the pulse, which so
never rises with mode 1 selected. In double speed it ends a dot before
the pulse, except with mode 2 selected: then it lasts to the pulse as in
single speed, so a selected mode 1 blocks it. That models gambatte's
block without a selects test in the hot `Ly4` arm (measured: masking the
selects there cost ~0.4% Ir on cgb-acid2). The STAT and LYC write views
at 0:4 follow the same rule with the selects as of the write
(`Ppu::mode_source`): a write there that keeps modes 1 and 2 selected
leaves the line up and the pulse requests nothing, as in SameBoy
(`display.c`: `mode_for_interrupt` is 1 until the pulse) and gambatte
(no trigger without a newly set select; the event blocked by mode 1).
Until review E3 the views saw the source end at 4 whatever the selects,
and the pulse then requested an interrupt (reachable: a second phase
from an access at 0:3 on the default grid, a first phase or LYC write at
0:4 on the `lcdoffset1` grid; no ROM). Still unlike gambatte, no ROM: a
write at 0:4 clearing mode 1 unblocks the pulse (gambatte's latch keeps
the block; SameBoy's line also drops), and `10 -> 30` at 0:4 requests an
interrupt (gambatte: none). The read views are unchanged (LYC=0 from
153:12; line 0 sees mode 1 to t = 4). The default double-speed phase (M-cycle
boundaries at odd t) can't tell these from a dot earlier except through
the dispatch's IF clear, one dot into its last M-cycle; `ds_lcdoffset1`
(even t) can. CGB04C:

- `lyc153int_m2irq/lyc153int_m2irq_late_retrigger_ds_1` (IF cleared at
  0:4, the pulse after it: IF = 2), `_ds_2` (cleared at 0:6: 0);
- `ly0/lycint152_lyc0irq_late_retrigger_ds_1` (IF cleared at 153:12, the
  LYC=0 request after it), `_ds_2` (153:14);
- `window/late_enable_afterVblank_ds_lcdoffset1_2` and
  `lcd_offset/offset1_lyc99int_m2irq_count_ds_1` (the pulse dispatched at
  0:6, not 0:4);
- the mode-1 end at 4: `lycEnable/lycwirq_trigger_ly00_stat50_ds_lcdoffset1_2`,
  `miscmstatirq/lycstatwirq_trigger_ly00_10_50_ds_lcdoffset1_2` (LYC=0
  matched or selected at 0:4 with mode 1 selected: interrupt; both
  regressed with the mode-1 source lasting to 5);
- the select's block: `m2enable/m2_late_m1disable_ly0_ds_2` (mode 1
  cleared at 0:5, after the pulse: none; regressed without the block),
  `_ds_1` (0:3: interrupt).

### Single speed: a write a dot before either point

Moving the single-speed points alone fails (measured 2026-11-24, full
manifest): when a write a dot before the point changes what the point
sees, `m2enable/late_enable_ly0_1`, `disable_by_m1enable_ly0_1`,
`lycEnable/lycwirq_trigger_ly00_stat50_2` and
`lycEnable/lyc0_ff4{1,5}_disable_2` fail; when the walk's pulse at 5 sees
a mode 2 selected at 0:4, `m2enable/late_enable_m1disable_ly0_2`,
`late_m1disable_ly0_2` and `lyc0_late_m2enable_lycdisable_2` do. All of
them write STAT or LYC with the access at 0:3 or 153:11, so the second
phase or the LYC write lands a dot before the point, and all are
gambatte's latch
(`mstat_irq.h` `statRegChange`/`lycRegChange`, `lyc_irq.cpp` `regChange`,
behaviour only): its events see the selects and LYC as of the last write
more than 2 cycles before them (LYC: more), so such a write changes the
registers but not what the event sees.

The rule (`Ppu::early_cmp`, `Ppu::point_before_write`): a STAT or LYC
write landing a dot before the point (0:4, 153:12; the dots from which
the write views see the pulse done and the comparison against 0,
`edges::LY0_PULSE_SEEN`, `edges::L153_ZERO`) has the point evaluated
first, with the selects and LYC from before the write; its request
reaches IF at the point's own dot (a timed request, below). The write
then sees the point done: the mode-1 source over and no mode-2 source at
0:4 (a mode 2 it selects is too late for the pulse; the walk's pulse at 5
finds the line held up), LYC compared with 0 at 153:12. This is the same
mechanism as every other line's comparison, which the write views see at
t = 4 and the walk makes at t = 5.

It is one dot of gambatte's window, the one the ROMs above pin. The rest
of it is modelled where a ROM needed it, each as its own rule: a first
phase two dots before the pulse (`MODE_HOLD` at 0:3, above), a STAT write
2-3 dots before line 153's comparison (`MODE_1_STICKY`, below). A write
earlier in the window that neither rule covers takes effect at once (no
ROM).

No ROM we have reads IF or checks interrupts at an M-cycle boundary on
153:12 (only a CPU phase with boundaries at `t = 0 mod 4`, or an
acknowledge at `t = 2 mod 4`, can tell 12 from 13 there): the read side
of line 153's point rests on gambatte's model and the IF-write rows
above.

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

## Writes just before the mode-0 interrupt (lines 0-143)

gambatte (`mstat_irq.h`, behaviour only) evaluates the mode-0 interrupt
with the STAT selects and LYC latched at the last write that landed early
enough: a STAT write only while `cc + 2` is before the event, a LYC write
only while `cc + 5 + 1 - ds` is (CGB; `cc` in T-cycles, half-dots in
double speed). Its event (`doM0Event`) fires if mode 0 is selected now or
in the latch, unless the latched LYC select and LYC match LY. Mapped to
ours, `cc` is the write's access and the event is the walk's `HBlank`
point (mode-3 end + 1). The CGB04C captures of `m0enable/*` pin this
mapping at each SCX (`edges::M0_STAT_LATCH`, `M0_LYC_LATCH`):

| write, access before `HBlank` | single speed | double speed |
|---|---|---|
| STAT, clearing mode 0 | 1-2 dots: interrupt still comes | 1 dot (phase 2 lands on the point: already right) |
| STAT, clearing LYC while LY=LYC | 1-2 dots: still blocked | 1 dot: still blocked |
| LYC, ending LY=LYC | 1-6 dots: still blocked | 1-2 dots: still blocked |

ROMs: `disable_{,scx3_,scx4_,scx7_}2` (2) and `disable_scx{1,2,5}_1`
(0) in single speed; `lycdisable_ff45_{,scx1_,scx2_,scx3_}2` (0),
`_scx1_1` (2); `lycdisable_ff45_scx1_ds_{1,2}`,
`lycdisable_ff41_scx1_ds_{1,2}`. DMG differs (`dmg08` results: no STAT
latch, a 1-dot LYC one), the reason these ROMs' CGB and DMG results
differ.

The latch exists only where a mode-0 interrupt comes: on a line whose
mode-3 end is known (`Ppu::m0_latched`), i.e. lines 0-143, the first line
after LCD enable included (its interrupt is 2 dots later, as its mode 3;
gambatte latches against the event's time on every line alike; no ROM
writes STAT there). Lines 144-153 have no mode-0 event and no latch. Until
2026-11-24 (M10 review) an unlatched line counted as having the interrupt
at its end, dot 456, so on lines 144-153 a STAT write with its access at
dots 454-455 that cleared the mode-0 select requested a STAT interrupt
(dot 455 is on the default single-speed grid), and a STAT write clearing
the LYC select there, or a LYC write at 450-454 ending LY=LYC, left the
line up until the walk's next point. No ROM did either.

Double speed, line start: a STAT write's phase 2 (half a dot after the
access, a whole dot on our grid) sees the previous line's mode 0 while
the access is before `t = 3` (gambatte `T > 8` half-dots), i.e. phase 2
to `t < 4`. `m0enable/late_enable_ds_lcdoffset1_1` (enable with the
access at t = 2: interrupt), `late_enable_ds_2` (t = 3: none). Moving
every double-speed phase-2 mode view half a dot (as the comparison is)
passed the same ROMs on lines 0-143 but would delay a phase-2 mode-0
enable at the mode-0 start past the walk's point, which gambatte's event
still takes; line 144's windows already assume the whole-dot phase 2.

## IF writes, the LYC-write request, line 0 and the dispatch's IF push

- An IF write lands one dot after its access (SameBoy `cgb_conflict_map`:
  `GB_IO_IF` is `GB_CONFLICT_WRITE_CPU`, both speeds): a PPU interrupt
  requested at that dot is cleared by it, one a dot later is not
  (`Ctx::write_if`). CGB04C `m2int_m0irq/m2int_m0irq_scx3_ifw_{1,2,3,4}`,
  `_ds_{1,2}`: the SCX 3 mode-0 interrupt falls one dot past an M-cycle
  boundary; `_2`, `_4`, `_ds_2` write IF with that boundary as access
  and expect it cleared.
- Line 153's LYC=0 point (t = 13) and line 0's mode-2 pulse (t = 5) are
  two dots past an M-cycle boundary on the default single-speed grid, so
  an IF write with that boundary as access lands a dot before them and
  doesn't clear them. CGB04C `ly0/lycint152_lyc0irq_ifw_1`,
  `lyc153int_m2irq/lyc153int_m2irq_ifw_1` (interrupt).
- CGB single speed: the STAT interrupt a LYC write requests (LY = LYC
  newly true with the LYC select set) reaches IF 5 dots after the write
  lands (`edges::LYC_WRITE_IRQ`; the write lands one dot after its
  access), so the interrupt check at the end of the write's M-cycle
  misses it and the one after the next instruction's first M-cycle takes
  it. gambatte (behaviour only) `LCD::lycRegChange`:
  `memevent_oneshot_statirq` at `cc + 5` on CGB in single speed, at once
  otherwise. CGB04C `lycEnable/lyc_ff45_trigger_delay_2` (out2; DMG
  out0). A timed request (`Ctx::write_ppu`, FF45). gambatte's `cc` is
  taken as the landing dot, the grid IF writes land on too: an IF write
  in the M-cycle after the LYC write (not reachable by an instruction)
  lands 4 dots after it, before the request, as in gambatte, where the
  write at `cc + 4` precedes the event.
- The delay is for the interrupt the write itself requests, not for the
  walk's own LY=LYC interrupt. A LYC write landing on t = 4 of lines
  1-152 (the access at t = 3, the default grid) lands a dot before the
  walk's comparison (t = 5), which the write views already see: the
  comparison is evaluated first, with the old LYC (`Ppu::early_cmp`), and
  its request reaches IF at t = 5, the comparison's own dot (a timed
  request), so the interrupt check at the end of the write's M-cycle
  (t = 7) takes it, as with no write. gambatte (behaviour only,
  `lyc_irq.cpp` `regChange`, `doEvent`): the LYC event keeps its time and,
  for a write of another value at most 2 cycles before it, the old LYC; a
  write of the same value does nothing (`LCD::lycRegChange` returns).
  Until 2026-11-24 (M10 review) this request took the write's 5 dots too
  (t = 9), one M-cycle late, also for a write of the same value.
  Line 153's comparison with 153 (t = 5) has no such rule: the write views
  see it at 5, not 4, so a LYC write landing on 153:4 comes before it. One
  that changes LYC loses the interrupt (gambatte keeps it), and one that
  writes 153 again requests it as its own, at t = 9 (gambatte: at 5). No
  ROM (`unpinned.md`, `l153-lyc-write-before-cmp`).
- Single speed, a STAT write accessed at line 0 t = 3 lands its phase 2
  a dot before line 0's pulse (t = 4), where the write views see the
  pulse done and the mode-1 source over ("a write a dot before either
  point" above). The write still sees mode 1 holding the line: if the
  line was high before the pulse and stays high, the write requests
  nothing (`Ppu::on_ly0_pulse`, `Ctx::write_ppu`). The pulse itself is
  evaluated before the write, so a mode 2 the write newly selects is past
  it.
  CGB04C `miscmstatirq/lycstatwirq_trigger_ly00_10_50_1` (`10 -> 50`,
  LYC = 0: E0; `_2`, accessed 4 dots later: E2),
  `m2enable/late_enable_m1disable_ly0_2`, `late_m1disable_ly0_2`,
  `lyc0_late_m2enable_lycdisable_2` (pulse first: none; they regress if
  the pulse runs after the write). Earlier, making every phase 2
  simultaneous with the PPU update of its dot regressed 13 ROMs.
- Interrupt dispatch with SP = FF11 pushes PC low onto IF: the vector
  comes from IF as it was before that write (SameBoy `cycle_write_if`
  returns the old IF). CGB04C `irq_precedence/if_and_ie_0_vector_4`
  (timer vector, not 0000).

## Timed requests

A request whose IF bit is set at a later clock than the walk point or
write that makes it is explicit state: "IF bits B are set at clock T"
(`irq::PendingIrq`, owned by the PPU as `Ppu::irq`; ADR 0033). Three
rules use it, all single speed: the LYC-write request (T = landing + 5);
line 0's mode-2 pulse or line 153's comparison against 0 when a STAT or
LYC write lands a dot before it (T = the point's dot; without such a
write the walk's point requests at once); and the LY=LYC comparison of
lines 1-152 when a LYC write lands a dot before it (T = the comparison's
dot, t = 5).

- Posting: `Ppu::next_event` is the earlier of the walk's next event and
  T, so the `EventKind::Ppu` deadline covers it, and every `Ppu::sync`
  to T or later sets the bits. Each IF observer already syncs the PPU to
  its own clock when that deadline has passed (the M-cycle boundary's
  event before the interrupt check and an IF read; an IF write at its
  landing dot; the dispatch's acknowledge at the middle of its M-cycle),
  so IF at any clock is the same however often the PPU was synced before
  (`scripts/check-sync-invariance.sh` has no known difference).
- IF holds bits 0-4 only. A waiting request is not in IF: an IF write,
  `Bus::set_interrupt_flags` and the acknowledge neither see nor drop
  it; from T on it is an ordinary IF bit, cleared by an IF write landing
  at T or later and absorbed by an acknowledge whose middle is at T or
  later.
- Turning the LCD off drops waiting requests (gambatte, behaviour only:
  `LCD::lcdcChange` sets every LCD event, `memevent_oneshot_statirq`
  included, to `disabled_time`). Nothing else cancels one: a later STAT,
  LYC or other PPU write only reschedules the PPU.
- At most two wait at once (a LYC write's with the pulse's, or with line
  153's: LYC writes are at least two M-cycles apart, and the two walk
  points 448 dots). A LYC write landing a dot before the comparison of
  lines 1-152 adds at most one: the comparison's request needs the old
  LYC to match, the write's own the new one and the line low.

## Open

- gambatte's latch (the selects and LYC its interrupt events see) is
  modelled window by window, not as one latched copy of the registers
  (review A2-02's proposal): `MODE_HOLD` (144:3-4, 0:3), `MODE_1_STICKY`
  (145-153:3-4, 153:10-11), the mode-0 latch (`edges::M0_STAT_LATCH`,
  `M0_LYC_LATCH`), the dot before line 0's pulse and line 153's
  comparison against 0 (`Ppu::point_before_write`), and the phase-2
  mode-1 rule on that dot (`Ppu::on_ly0_pulse`). Each window is where a
  CGB04C row pins it. Known gaps, no ROM for any: a write at 0:4 clearing
  mode 1 in double speed unblocks the pulse; `10 -> 30` at 0:4 in double
  speed requests an interrupt; double speed has no rule for a write a dot
  before line 153's point (the write views see the comparison against 0
  at 12, the walk at 13, and a write at 12 that ends the match loses the
  interrupt gambatte's latch would keep).
- No ROM reads IF or checks interrupts on 153:12 at a CPU phase with a
  boundary there (see "Line 0 and line 153's LYC=0" above): a capture of
  `lycint152_lyc0irq`-style rows at lcd offset 3 would settle the read
  side directly.
- A LYC write landing a dot before line 153's comparison with 153
  (153:4) is not too late for it, unlike on lines 1-152 ("IF writes, the
  LYC-write request" above); gambatte's latch says it is. No ROM
  (`unpinned.md`, `l153-lyc-write-before-cmp`).
- The mode-0 latch on the first line after LCD enable follows gambatte's
  model alone (`unpinned.md`, `m0-latch-first-line`).
