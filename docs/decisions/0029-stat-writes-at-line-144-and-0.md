# 0029: STAT writes at lines 144 and 0 follow gambatte's CGB C trigger windows

Status: Accepted (2026-11-23)

## Context

Around the start of VBlank (line 144) and the start of line 0, a CGB STAT
or LYC write can raise or lower the STAT interrupt line in ways the
per-dot source model doesn't explain. The references disagree:

- **SameBoy** (`display.c`) requests the line-144 mode-2 "quirk"
  interrupt at the line start and again at VBlank, and models no
  write-specific hold of the mode part of the line.
- **gambatte** (`video.cpp` `statChangeTriggersM0LycOrM1StatIrqCgb`,
  `statChangeTriggersM2IrqCgb`, `getLycCmpLy`; `mstat_irq.h`) gives
  per-write trigger windows fitted to CGB C captures (`cgb04c` results of
  the `gambatte/m1/*`, `m2enable/*` and `lcd_offset/*` ROMs).

ADR 0003 makes CPU CGB C the target, and gambatte's captures were taken on
CGB C. `m1/m2m1irq_ifw_2` (an IF write at t = 3 leaves only the VBlank
request) rules out SameBoy's second quirk request.

## Decision

Follow gambatte's windows, mapped to our line position as t = 7 - T on
the next line (`docs/reference/stat-irq.md`, "Line 144 and line 0"):

- The line-144 mode-2 quirk fires once, at t = 3, and not again at VBlank.
  From t = 3 to VBlank the mode part of the line is modes 0 and 2 together.
- A STAT write in single speed at line 144 t = 3-4 or line 0 t = 3 (or at
  line 144 t = 4 in double speed, first phase) neither raises nor lowers
  the mode part (`edges::MODE_HOLD_SS`).
- In single speed, a STAT write whose first phase lands at t = 3 on
  lines 1-152 sees LY=LYC off. This applies to the first phase only.
- In double speed, the second phase of a STAT write compares LY with LYC
  as of the dot before its whole-dot position (it lands half a dot after
  the first phase).

Code: `crates/gbc-core/src/ppu/edges.rs`, `ppu/mod.rs` `point_irq`,
`update_at`. gambatte is GPL: behavior only, no code copied.

## Consequences

+23 ROMs (all 16 failing `gambatte/m1/*`, 6 in `m2enable`/`miscmstatirq`,
1 in `lcd_offset`), no regressions. +374 B `.text`. `IrqLine` became a
one-byte bit set, to keep `Hw` within its size test. Two pieces are pinned
only by ROMs, because the unit-test rig can't reach the CPU phase they
need: the quirk at t = 3 (`ly143_late_m0enable_lcdoffset1_1`,
`offset1_lyc99int_m2irq_count_1`) and the double-speed hold at line 144
t = 4 (`ly143_late_m2enable_ds_lcdoffset1_2`). Where SameBoy and gambatte
differ elsewhere on STAT, this ADR doesn't decide it.

## Amendment (2026-11-23): more STAT and LYC write windows

The gambatte-over-SameBoy rule extends to:

- (a) The STAT write mode-2 and mode-0 windows on lines 0-143. A write
  before t = 3 sees mode 0. A double-speed first phase sees mode 2 up to
  t = 6. In double speed, line 0 triggers at t = 4-5.
- (b) The selects latched for LYC-interrupt blocking in VBlank. A write
  1-2 dots before the comparison can't lower the mode-1 part
  (`MODE_1_STICKY`; gambatte `lyc_irq.cpp` `regChange`).
- (c) Single-speed LYC writes on line 153. They compare with 153 up to
  t = 7, and the held 152 comparison ends at t = 4
  (`edges::LYC153_SS_UNTIL`). SameBoy's dots 12-23 window stays, for
  double speed only.
- (d) The LCD-off LYC-select interrupt (gambatte `memory.cpp` FF41).

Same T to t mapping. SameBoy's two-phase STAT write stays. Where
gambatte's single write time and our phase 2 disagree
(`lycstatwirq_trigger_ly00_10_50_1`), it stays open. +20 ROMs, no
regressions, +438 B.

## Amendment (2026-11-23): writes just before the mode-0 interrupt

The gambatte-over-SameBoy rule extends to the mode-0 interrupt on lines
0-143: it sees the STAT selects and LYC as latched up to a short time
before it (gambatte `mstat_irq.h`: `cc + 2` for STAT, `cc + 5 + 1 - ds`
for LYC, with `cc` our write's access and the event our `HBlank` point).
A STAT write whose access is at most 2 dots (double speed: 1) before the
point no longer stops it by clearing mode 0, nor unblocks it by clearing
the LYC select; a LYC write at most 6 dots (double speed: 2) before it
no longer unblocks it (`edges::M0_STAT_LATCH`, `M0_LYC_LATCH`). Also, in
double speed, a STAT write's phase 2 sees the previous line's mode 0
while its access is before t = 3. Details and ROMs:
`docs/reference/stat-irq.md`, "Writes just before the mode-0 interrupt".
The interrupt a late mode-0 clear can't stop is requested at the write,
not at the point 1-2 dots later in the same M-cycle (the CPU can't tell).
+11 ROMs (`gambatte/m0enable`), no regressions, +534 B.

## Amendment (2026-11-23): line 0 and LYC=0 in double speed

The gambatte-over-SameBoy rule extends to the walk's line-0 points in
double speed: line 0's mode-2 pulse is at t = 5 (gambatte's
`mode2_irq_line_cycle_ly0`, T = 2), the mode-1 source ends a dot before
it unless mode 2 is selected (which models `doM2Event`'s block by the
mode-1 select), and line 153 compares LYC with 0 from t = 13 (gambatte's
LYC=0 time, line 153 cycle 6). Single speed and the read and write views
are unchanged. A speed switch re-seeks the walk and moves the PPU
deadline (`Ppu::speed_changed`). Details and ROMs:
`docs/reference/stat-irq.md`, "Line 0 and line 153's LYC=0 in double
speed". +4 ROMs, no regressions. +438 B `.text` (`next_point` walks VBlank
lines with an out-of-line `step_vblank` to stay within `seek`'s budget;
`next_event` no longer skips the `point_irq` call at `Latch`, for its
budget), cgb-acid2 +0.35% Ir, worst-case +0.05%.

## Amendment (2026-11-23): IF writes and the LYC-write request

The gambatte-over-SameBoy rule, with SameBoy where it is the finer
model, extends to when IF changes against PPU interrupts:

- An IF write lands one dot after its access (SameBoy
  `GB_CONFLICT_WRITE_CPU`). In single speed it doesn't see line 153's
  LYC=0 point or line 0's mode-2 pulse at that dot: those walk points
  are a dot before gambatte's events (13 and 5), which only the
  double-speed walk follows; the STAT and LYC write views still assume
  the single-speed points.
- CGB single speed: a LYC write's STAT request reaches IF 5 dots after
  the access (gambatte `memevent_oneshot_statirq`, `cc + 5`), one
  M-cycle later than the write's own boundary. It waits in IF's unused
  bits 5-6, because `Hw` is at its size limit.
- A STAT write's phase 2 on line 0's single-speed pulse sees the
  mode-1 source still holding the line (same one-dot gap to gambatte),
  after the pulse has run for a newly selected mode 2.
- A dispatch push onto IF chooses from IF as it was before the write
  (SameBoy `cycle_write_if`), out of line: inside the inlined dispatch
  it cost ~1.5% Ir in every bench ROM through `Cpu::run`'s codegen.

The single-speed walk points themselves stay (moving them regressed
`lycEnable/lyc0_ff4{1,5}_disable_2` and the `m2enable/*ly0*` rows).
Details and ROMs: `docs/reference/stat-irq.md`, "IF writes, the
LYC-write request, line 0 and the dispatch's IF push". +6 ROMs
(`m2int_m0irq_scx3_ifw_{2,4,ds_2}`, `lyc_ff45_trigger_delay_2`,
`lycstatwirq_trigger_ly00_10_50_1`, `if_and_ie_0_vector_4`), no
regressions. +898 B `.text` (96,550 of 97,500); cgb-acid2 +0.28% Ir
(686.27 M), worst-case +0.04% (476.78 M).

## Amendment (2026-11-24): where the delayed requests live

The decisions above stand. Their storage changed (ADR 0032): the
LYC-write request no longer waits in IF bits 5-6 but as a timed request
in the PPU (`irq::PendingIrq`), 5 dots after the write lands, and line
0's single-speed mode-2 pulse reaches IF through the same mechanism
instead of a late PPU deadline. `Ppu::early_point` no longer serves IF
writes on the pulse.
