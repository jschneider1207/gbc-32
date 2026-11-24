# Design: PPU edges at every CPU/PPU clock phase (`lcd_offset`)

- Status: Proposed. Step 0 done (ba63b92); step 0b dropped; step 1
  measured and held (branch `lcd-offset-step1`, see 4.3). M9 triage
  groups 6 and 9, and the phase-dependent ROMs of groups 8 and 12.
- Date: 2026-11-23. Written against 65fe096 (main), with branches
  `hdma-trigger` (1a110de) and `lcd-offset-phase` (dfb1c67) read but not
  built. No romtest runs were made for this doc: every "ours" outcome
  below comes from `acceptance/results/stage1.json` and every position
  from reading `crates/gbc-core/src/ppu/mod.rs` and `bus.rs`.
- Model: CPU CGB C (ADR 0003). References: SameBoy (MIT) `Core/display.c`,
  `Core/sm83_cpu.c`, `Core/timing.c` (master, fetched 2026-11-23);
  gambatte-core (GPL, **read for behaviour only, no code copied**)
  `libgambatte/src/video.cpp`, `video.h`, `video/ppu.cpp`,
  `video/ly_counter.{h,cpp}`, `video/lyc_irq.cpp`, `memory.cpp`, `cpu.cpp`,
  `interruptrequester.cpp`, and the test sources in `test/hwtests/`.

## 0. Summary

The CPU can sit at four phases against the PPU in single speed and two in
double speed. Every phase is a whole number of dots. Our timeline already
represents all of them exactly: positions are whole dots, and so is every
M-cycle boundary. The `lcd_offset` failures are not a resolution problem.
They are a **parametrization** problem. Each PPU edge is one integer
position, and every CPU observer shares it: interrupt dispatch, IF reads,
STAT and LY reads, the two phases of a STAT write, LYC writes, the access
windows, the HDMA trigger and HALT wake-up. Calibration at the default
phase only fixes each edge to within the 4-dot (2-dot in double speed)
window between two M-cycle boundaries. The hardware, and gambatte's model
of it, has a separate predicate per observer, and these differ inside
that window.

The model proposed here:
1. Each edge is defined once, in **half-dots**, relative to the line.
2. Each observer has a **sample offset** in T-cycles: 2 half-dots in
   single speed, 1 in double speed.
3. Both compile to the same kind of whole-dot `const` thresholds the code
   compares against today.

The runtime representation (whole-dot clocks, the update-point walk of
ADR 0014) does not change. What changes is that the write paths and the
walk stop sharing one `mode_source`/`cmp` view. Half-dots only reach
runtime in one place, the double-speed second phase of a STAT write,
which lands half a dot after the access.

## 1. What the hardware does

### 1.1 Clocks

The CGB has one 8.39 MHz master clock. A PPU dot is 2 master ticks in
both speeds; the PPU does not change speed. A CPU T-cycle is 2 ticks in
single speed (1 dot) and 1 tick in double speed (half a dot). An M-cycle
is 4 T-cycles: 4 dots in single speed, 2 in double speed. Sources:
- Pan Docs, "CGB Registers / FF4D KEY1" and "Timing": the PPU is not
  affected by double speed.
- SameBoy `timing.c` `GB_advance_cycles`: the CPU advances in T-cycles,
  and `cycles <<= 1` in single speed before `GB_display_run`.
- SameBoy `display.c` `GB_BATCHABLE_STATE_MACHINE(gb, display, cycles, 2, ...)`:
  the display state machine counts master ticks with a divisor of 2 per
  dot.

Our clock (`now`, base clocks) counts dots. A double-speed M-cycle is 2.

### 1.2 Where the CPU grid sits after a speed switch

- **Gambatte.** `Memory::stop` calls `LCD::speedChange(cc_)` with
  `cc_ = cc + 8 * !isDoubleSpeed()`. `PPU::speedChange` does
  `p_.now -= isDoubleSpeed()` and then re-bases the LY counter
  (`LyCounter::reset(videoCycles, now)`, scaled by `<< ds`). Gambatte's `cc`
  advances 4 per M-cycle at either speed, so a `cc` unit is a dot in single
  speed and half a dot in double speed (`LyCounter::lineCycles`:
  `(time_ - cc) >> isDoubleSpeed()`).

  Two consequences follow:
  1. Every switch to double speed re-anchors the PPU's dot grid on the
     CPU's M-cycle grid (`update(cc_)` leaves `now = cc_`). Double-speed
     M-cycle boundaries therefore always fall on dot boundaries. Only their
     parity against the line position varies.
  2. Every switch back to single speed moves the PPU one dot (one
     single-speed `cc`) against the CPU. Single-speed phases are whole
     dots, 4 of them.
- **The test ROMs' own construction** confirms this. Every `lcd_offset`
  `.asm` starts with `ld a,1; ldff(4d),a; stop` pairs:

  | Name | Preamble | Our single-speed M-cycle boundaries |
  |---|---|---|
  | `offset1` | one round trip (double, then single) | `t ≡ 2 (mod 4)` |
  | `offset2` | two round trips | `t ≡ 1` |
  | `offset3` | one round trip with a `nop` in double speed between the two STOPs | `t ≡ 0` |
  | (none) | default | `t ≡ 3` |
  | `ds_lcdoffset1` | offset1, then back to double speed | even double-speed positions, not the default odd ones |

  The `nop` (2 dots) moves the double-to-single switch by 2 dots against
  the PPU, so offset3 = 1 + 2. This matches the measurement in m9-triage
  group 6 (our 0x20003-clock switch moves the CPU by -1 dot) and ADR 0020.
- **SameBoy does not model this.** `sm83_cpu.c` `stop()`: "Speed switch
  timing needs far more tests. Double to single is wrong to avoid odd
  mode". With the LCD on and `double_speed_alignment & 7`, it logs "ROM
  triggered a PPU odd mode, which is currently not supported. Reverting to
  even-mode" and freezes the CPU to restore the default alignment. Also:
  "speed switching takes 2 extra T-cycles (so 2 PPU ticks in
  single->double and 1 PPU tick in double->single)". So SameBoy can't
  serve as a reference for any non-default phase. Gambatte is the only
  model, and the cgb04c captures in the ROM names are the truth.
- **Pan Docs / GBCTR** document neither the switch's phase effect nor the
  "odd mode". GBCTR documents the M-cycle/T-cycle structure.

### 1.3 Where each observer samples

On the SM83 every bus access sits at a fixed T-cycle of its M-cycle. In
double speed a T-cycle is half a dot, so sub-M-cycle offsets become
half-dot offsets. SameBoy `sm83_cpu.c` `cycle_write` and the
`cgb_conflict_map` / `cgb_double_conflict_map` give the CGB offsets
relative to the access:

| Observer | Single speed | Double speed | SameBoy conflict type |
|---|---|---|---|
| Read (STAT, LY, IF, VRAM, OAM) | 0 | 0 | `cycle_read` |
| SCX, SCY, WY write | 0 | 0 (SCX: `_SCX_DMG_AND_CGB_DOUBLE`) | `READ_OLD` |
| LYC write | +1 T = **+1 dot** | 0 | `WRITE_CPU` / `READ_OLD` |
| IF write | +1 T = +1 dot | +1 T = **+1/2 dot** | `WRITE_CPU` |
| STAT write, phase 1 (all selects but LYC; double speed: all but mode 0) | 0 | 0 | `STAT_CGB`, `STAT_CGB_DOUBLE` |
| STAT write, phase 2 (the rest) | +1 T = +1 dot | +1 T = **+1/2 dot** | same |
| BGP/OBP write | -1 T (`PALETTE_CGB`) | n/a | |
| LCDC write, phase 2 | +1 T | +2 T = +1 dot | `LCDC_CGB`, `_DOUBLE` |

`bus.rs` FF41 applies phase 2 at `now + 1` in both speeds. In double
speed that is a full dot, where SameBoy has half a dot. This is the one
place where our whole-dot timeline misplaces a CPU action. Section 3.4
fixes it without a half-dot clock.

Interrupt dispatch samples IF at the instruction boundary. Gambatte:
`flagIrq(bit, cc)` sets `intevent_interrupts` at `max(minIntTime_, cc)`,
and the CPU's event check runs at the `cc` of the next fetch. Ours:
`Cpu::step_inline` checks `IE & IF` before the fetch, at the clock after
the previous M-cycle's tick. Reads in our bus happen after the tick, at
the end of their M-cycle. Gambatte reads at `cc`, the start, then adds
4. Section 2.2 shows what this means for the IF-read observer.

### 1.4 Edges as gambatte expresses them

Gambatte has no STAT "line". It has an event per IRQ source plus a
trigger predicate per write. Times are relative to `LyCounter::time()`,
the LY event at the end of a line, written `G = 0` below; `<< ds` keeps
them in dots at both speeds unless noted. Our positions are frame
positions, with LY changing at `t = 3` (ADR 0014, ADR 0020 amendment).

| Edge | Gambatte (behaviour) | Ours today (single / double) |
|---|---|---|
| LY read changes | `getLyReg`: next LY when `time-cc <= 6+4ds`, glitch `ly&(ly+1)` at `== 6+4ds` | `t >= 3`; glitch t = 1-2 (double speed: 2) (`Ppu::ly`) |
| STAT mode 2 (read) | `getStat`: `lineCycles >= 453` (G = -3) | `t >= 4` |
| Coincidence bit (read) | `getLycCmpLy`: `timeToNextLy -= 2+2ds`, flag needs `> 2` | off at t = 3, on at 4 (lines 1-143); off at 0-3 on 144-152 (`Ppu::cmp`) |
| Mode-2 IRQ, lines 1-143 | `mode2_irq_line_cycle = 456-4` (G = -4) | `Pt::Ly3`, t = 3 |
| Mode-2 IRQ, line 0 | `mode2_irq_line_cycle_ly0 = 456-2` (G = -2) | `Pt::Ly4` line 0, t = 4 |
| LYC IRQ, LYC = 1-153 | `lyc_irq.cpp` `schedule`: `lyc*456 - 2` (G = -2) | `Pt::Ly4`/`V4` t = 4; LYC 153: `L153Cmp` t = 5 (was 6, step 5) |
| LYC IRQ, LYC = 0 | `153*456 + 6` | `L153Zero`, line 153 t = 12 |
| Mode-1 IRQ | `mode1_irq_frame_cycle = 144*456 - 2` (G = -2) | `Pt::VBlank`, 144:5 |
| LYC IRQ blocked by | `lycIrqBlockedByM2OrM1StatIrq`: m2 select (LY 1-144), m1 select | STAT line high (the mode-2 source ends at `Ly4`) |
| STAT write enabling m2 triggers | `statChangeTriggersM2IrqCgb`: `2 < timeToNextLy <= 4(1+ds)` (G in [-4,-3]) | `mode_source(t) == 2` for `t < 4` at either phase |
| LYC write triggers | `lycRegChangeTriggersStatIrq`: blocked by m0/m1 only, **never by m2**; `cc+5` one-shot in single speed | the STAT line, with the mode-2 source live for t < 4 |
| HDMA period | `isHdmaPeriod`: `cc + 3 + 3ds < time` and `cc >= m0Time` | `hdma::HBLANK_DELAY` (3; `hdma-trigger`: 2) after mode-0 STAT |
| VRAM read/write block | `vramReadable`/`vramWritable`: `lineCycles(cc) + ds < 76+3cgb` / `< 79` | `vram_blocked`: from 84 (first line 91; double speed 86) |
| OAM read/write block | `oamReadable`: `lineCycles + 4 - ds >= 456`; `oamWritable`: `+ 3 + cgb` | `oam_blocked`: `[0,84] [2,84] [3,86] [4,86]` |

Gambatte's predicates use odd `cc` constants in double speed:
- `getStat`: `- 4 + isDoubleSpeed()`
- `oamReadable`: `+ 4 - isDoubleSpeed()`
- `vramExactlyReadable`: `cc + 2 + isDoubleSpeed()`
- `inactivePeriodAfterDisplayEnable(cc + 1 - cgb + ds)`
- `video.cpp`: `now + 2 - isDoubleSpeed()`

These are half-dot thresholds. Gambatte's double-speed M-cycle boundaries
are even `cc` against the dot grid (1.2), so for observers at the access
point they round to whole dots. They matter only for sub-M-cycle
observers.

## 2. Constraints

### 2.1 The phase calculus

Positions `t` are line-relative dots. An observer sampling at M-cycle
boundaries `b ≡ r (mod g)` (single speed g = 4, r = 3/2/1/0 for offsets
0/1/2/3; double speed g = 2, r = 1 default, 0 for `ds_lcdoffset1`) sees an
edge at `E` from the first boundary `ceil_r(E) >= E`. Two rules follow:

- **Move rule.** Moving an edge from `E` to `E'` changes the outcome only
  at phases with a grid point in `[min(E,E'), max(E,E'))`. Default single
  speed (r = 3) can't tell `4k..4k+3` apart. Default double speed can't
  tell `2k, 2k+1` apart. An observer sampled at `b + 1` (LYC write and
  STAT phase 2 in single speed) has grid `≡ 0`, so `4..7` are not
  equivalent for it. That is how dfb1c67 regressed phase-0 ROMs (2.3).
- **Pair rule.** A gambatte `_1`/`_2` pair runs identical code apart from
  the access, which is one M-cycle later in `_2` (verified by diffing
  `offset1_lyc99int_m2irq_count_{1,2}`, `offset3_lyc8fint_m1irq_{1,2}`).
  It is synchronised by an interrupt at `S` and measures an edge `E` with
  another observer. It checks `ceil_r(E) - ceil_r(S) = Δ` for a fixed
  `Δ ≡ 0 (mod g)`. With `x = E - S` and `d = ceil_r(S) - S`, this is
  `x + ((d - x) mod g) - d = Δ`. Over the phases a family covers, this
  pins `x`.

Worked example, `lcd_offset/offset*_lyc8fint_m1irq` (sync: LYC = 143
dispatch; measured: the mode-1 IRQ as seen by an IF read):
- Ours: `S = 143:4`, `E = 144:5`, so `x = 457`. That gives `Δ` = 456 at
  r = 3 and r = 1 (pass), and 460 at r = 0 (`offset3_..._2` fails).
- Double speed: d = 1 gives 456 (pass); `ds_lcdoffset1` (d = 0) gives 458
  (`_2` fails).
- Every outcome is consistent with `x = 456` at both speeds, which is
  gambatte's `x` (both edges at G = -2). So the LYC dispatch edge moves to
  t = 5 (lines 1-152), or the mode-1 edge to 144:4. dfb1c67 found the
  first empirically. The calculus predicts it from the reference.

### 2.2 A warning from `lyc99int_m2irq_count`

`offset{1,2,3}_lyc99int_m2irq_count_1` and `_ds_1` all fail; `_2` all
pass. The test does not test one edge:
- `_1` reads IF just before the mode-2 IRQ and clears IF just after it,
  on 152 consecutive lines (line 0, then 1-151; loop period 456 dots).
- `_2` reads one M-cycle later and must see it every line.

So `_1` constrains line 0's mode-2 IRQ and lines 1-143's IRQ together
against an IF read and an IF write (+1 T). Ours puts line 0 at t = 4 and
the other lines at t = 3, one dot apart. Gambatte puts them 2 dots apart
(G = -2 and -4). This family is the main constraint on the line-0 edge
and on the IF-read observer. It is listed as a step with a sweep (4.3),
not solved by hand here.

### 2.3 The dfb1c67 regressions explained

dfb1c67 moved the walk's compare point to t = 5 and dropped the mode-2
evaluation there, so the walk's mode-2 source lasts 3..5. A STAT write at
t = 4 (offset3, phase 1) now needs that source live, which matches
gambatte's `statChangeTriggersM2IrqCgb` window G in [-4,-3]. But
`mode_source` is shared, and at phase 0 the observers at `b + 1` (LYC
writes `now+1`, STAT phase 2) sample t = 4 too. The three phase-0 ROMs
that broke when the source was extended are exactly LYC-write and
STAT-phase-2 cases:
- `lyc1_m2irq_late_lyc255_3` (LYC write)
- `lyc1_m2irq_late_lycdisable_2` (phase 2 clears LYC)
- `m2_late_m0disable_2` (phase 2)

Gambatte never lets the mode-2 source block a LYC-write trigger
(`lycRegChangeStatTriggerBlockedByM0OrM1Irq`). The fix is a per-observer
source view, not a choice of threshold.

### 2.4 Table

Columns:
- **r**: single-speed boundary residue `t mod 4` (default 3), or `ds`
  with its parity (default odd).
- **Sync / measured**: the edges and the observer that sees each.
- **Ours**: from `stage1.json`.
- **Needed**: the direction. "`_1` fails" means the outcome `_2` should
  show appears already at `_1`'s access: the measured edge is early
  against the sync, or the sync is late. "`_2` fails" means the opposite.
- **Step**: the plan step in 4.3 that owns it.

Families whose members all pass are at risk and listed after the failing
ones. The full pairing is reproducible with the one-liner in Appendix A.

Failing ROMs (44 on main):

| ROM family (gambatte/...) | r | Sync (observer) -> measured (observer) | Ours | Needed | Step |
|---|---|---|---|---|---|
| `lcd_offset/offset3_lyc8fint_m1irq` | 0 | LYC143 (dispatch) -> m1 IRQ (IF read) | `_2` F | x = 456: LYC dispatch t = 5 | 1 |
| `lcd_offset/offset1_lyc8fint_m1irq_ds` | ds even | same | `_2` F | x = 456 in double speed: LYC t = 5 (double speed) | 3 |
| `lcd_offset/offset3_lyc8fint_m1stat` | 0 | LYC143 (dispatch) -> STAT mode 1 (read) | `_2` F | sync later (step 1) | 1 |
| `lcd_offset/offset{1,2}_lyc8fint_m1stat` | 2, 1 | LYC143 -> STAT `C4` (mode 0 + LYC flag) | `_1` F | coincidence bit on line 144 must stay on through t = 2 (read view; gambatte `getLycCmpLy` `> 2`) | 5 |
| `lcd_offset/offset1_lyc8fint_m1stat_ds` | ds even | same | `_1`, `_2` F | double-speed coincidence hold + LYC t = 5 (double speed) | 3, 5 |
| `lcd_offset/offset3_lyc98int_ly_count` | 0 | LYC152 (dispatch) -> LY (read) | `_1`, `_2` F | sync later (dfb1c67 flips both with LYC t = 5) | 1 |
| `lcd_offset/offset1_lyc98int_ly_count_ds` | ds even | same | `_2`, `_3` F | LYC t = 5 (double speed); then re-check the LY read edge | 3, 6 |
| `lcd_offset/offset{1,2,3}_lyc99int_m2irq_count` | 2, 1, 0 | LYC153 (dispatch) -> m2 IRQ, line 0 and 1-151 (IF read, IF write) | `_1` F | IRQ inside (read, write] every line (2.2) | 5 |
| `lcd_offset/offset1_lyc99int_m2irq_count_ds` | ds even | same | `_1` F | same | 5 |
| `lcd_offset/offset2_lyc99int_m0stat_count_scx{1,2}` | 1 | LYC153 -> STAT mode 0 per SCX (read) | `_1` F | mode-0 read edge early at r = 1 | 8 |
| `m0enable/late_enable_lcdoffset1` | 2 | STAT write enabling m0 at mode-0 start | `_1` F | m0 write view: the trigger window closes too early | 8 |
| `m0enable/late_enable_ds_lcdoffset1` | ds even | same, double speed (m0 select is the phase-2 select) | `_1` F | phase 2 at +1/2 dot (1.3) | 4 |
| `m1/ly143_late_m{0,2}enable_ds_lcdoffset1` | ds even | STAT write at line 143/144 | `_2` F | phase 2 at +1/2 dot | 4 |
| `m2enable/late_enable_ly0_lcdoffset2` | 1 | STAT write enabling m2 near line 0 | `_1` F | line-0 m2 write window (gambatte `statChangeTriggersM2IrqCgb`, `ly == 153` branch: `2 < timeToNextLy <= 2(1+ds)`, empty in single speed, one half-dot step in double speed) | 5 |
| `m2enable/late_enable_ly0_ds_lcdoffset1` | ds even | same | `_1` F | same + step 4 | 4, 5 |
| `lycEnable/lyc153_late_ff45_enable_lcdoffset1` | 2 | LYC write (b+1) at line 153 | `_1` F | LYC-write view at line 153 (`write_lyc` 153 windows) | 5 |
| `lycEnable/late_ff45_enable_ds_lcdoffset1`, `lyc153_late_ff45_enable_ds_lcdoffset1`, `ff45_enable_weirdpoint_ds_lcdoffset1_2` | ds even | LYC write (at access) | `_2` F | LYC compare edge in double speed (t = 5) | 3 |
| `lycEnable/late_ff41_enable_ds_lcdoffset1`, `lyc153_late_ff41_enable_ds_lcdoffset1` | ds even | STAT write setting LYC select (phase 1 in double speed) | `_2` F | LYC edge in double speed + phase 2 | 3, 4 |
| `vram_m3/pre{read,write}_lcdoffset2` | 1 | VRAM access at mode-3 start | `_1` F | VRAM block starts too early at r = 1 (gambatte `76+3cgb`, `79` with `+ds`) | 7 |
| `oam_access/prewrite_lcdoffset1` | 2 | OAM write at line start | `_1` F | OAM write block starts too early at r = 2 | 7 |
| `oam_access/preread_ds_lcdoffset1` | ds even | OAM read at line start | `_2` F | OAM read block, double speed (`+4 - ds`, a half-dot threshold) | 7 |
| `cgbpal_m3/cgbpal_{read,write}_m3start_ds_lcdoffset1` | ds even | BCPD access at mode-3 start | `_2` F | palette block start, double speed (`+ ds < 80`) | 7 |
| `window/late_wy_lcdoffset1`, `_ds_lcdoffset1`, `window/arg/late_wy_1toFF_{,ds_}lcdoffset1` | 2, ds even | WY write vs the line's WY check | `_2` F | WY check (`WY_CHECK_AT [7,3]`) late at these phases | 9 |
| `window/late_enable_afterVblank_ds_lcdoffset1` | ds even | LCDC.5 write after VBlank | `_2` F | window enable view, double speed | 9 |
| `dma/hdma_late_enable_lcdoffset3` | 0 | FF55 enable vs HBlank period | `_2` F | HDMA period edge (+ LYC sync, step 1) | 1, 10 |
| `dma/hdma_late_enable_ds_lcdoffset1` | ds even | same | `_2` F | same, double speed | 3, 10 |
| `dma/hdma_late_m0halt_lcdoffset3` | 0 | HALT in mode 0, wake: HDMA | `_2` F | HALT-wake HDMA sample | 10 |
| `dma/hdma_late_m0halt_ds_lcdoffset1` | ds even | same | `_1` F (`hdma-trigger`: `_2` F) | both halves need the right phase; blocked on 3 and 10 | 10 |

At-risk passing ROMs (each must still pass after every step):

| Family | r | Why at risk |
|---|---|---|
| `lcd_offset/offset{1,2}_lyc98int_ly_count*`, `offset2_lyc8fint_m1irq`, `offset1_lyc99int_m{0,2,3}*stat*`, `offset3_lyc99int_m{0,2,3}stat*` | 2, 1, 0 | same sync edges; move rule says a LYC 4 -> 5 move affects only r = 0 |
| `m2enable/late_enable_lcdoffset{2,3}`, `_ds_lcdoffset1` | 1, 0, ds | the dfb1c67 regression (`lcdoffset3_1`) |
| `m1/m1irq_late_enable_{,ds_}lcdoffset1`, `m1/ly143_late_m0enable_lcdoffset1` | 2, ds | mode-1 write view |
| `lycEnable/*_lcdoffset1` (single speed, 6 pass), `lycwirq_trigger_ly00_stat50_*` | 2, ds | LYC-write view |
| `miscmstatirq/lycstatwirq_trigger_ly00_10_50_{lcdoffset3,ds_lcdoffset1}` | 0, ds | STAT-write view at line 0 (see `docs/reference/stat-irq.md` "Open") |
| `vram_m3/*_lcdoffset1`, `*_ds_lcdoffset1`, `oam_access/{preread_lcdoffset1,prewrite_ds_lcdoffset1}`, `cgbpal_m3/*_m3start_lcdoffset1` | 2, ds | access windows |
| `window/late_enable_afterVblank_lcdoffset1` | 2 | window view |
| **Every default-phase PPU ROM** (Mooneye `ppu/*`, `intr_2_*`, AGE `stat-*`, `lcd-align-ly`, `spsw-*`, gambatte `m*enable`, `lycEnable`, `miscmstatirq`, `irq_precedence`, `display_startstate`) | 3, ds odd | by the move rule, only moves across a default grid point (t ≡ 3, or odd in double speed), or through an observer at b+1, can touch them |
| dfb1c67 phase-0 sentinels: `m2_late_m0disable_2`, `lyc1_m2irq_late_lyc255_3`, `lyc1_m2irq_late_lycdisable_2` | 3 (b+1 = 0) | they break if the write views follow the walk |

## 3. Proposed model

### 3.1 Representation

`ppu/edges.rs` (new, cold data only) holds every PPU edge as a
**line-relative half-dot** `H` (`u16`, `H = 2*t + s`, `s` in {0,1}), one per
physical signal, with a per-speed override only where a reference says
the PPU itself differs in double speed. Examples: SameBoy's mode-0 state
22 one dot later, line 153's LY 0 two dots later (ADR 0014
"Consequences").

It also holds a table of **observers**, each with a sample offset `k` in
T-cycles:

```text
READ = 0, DISPATCH = 0, WRITE_OLD = 0, LYC_WRITE = +1 (single) / 0 (double),
STAT_PHASE1 = 0, STAT_PHASE2 = +1, IF_WRITE = +1, PALETTE_WRITE = -1, ...
```

The observer samples at half-dot `2*b + k*(2 - double)`. It sees the edge
iff `2*b + k*(2 - double) >= H`. Every runtime check compares a whole-dot
`t` against `ceil((H - k*(2 - double)) / 2)`, a `const fn` evaluated at
compile time per (edge, observer, speed). So the hot path keeps today's
`t >= CONST` comparisons. The only new runtime input is which `(observer,
speed)` constant to pick, and every call site already knows both.

Why half-dots, if they compile away?
1. **One parameter per physical edge** instead of one per (edge,
   observer, speed). Single-speed and double-speed constants come from
   the same `H`, so a calibration at one speed transfers to the other.
   Today the double-speed differences are scattered `+ double` and
   `if double` literals (`ly`, `mode3_end`, `oam_blocked`,
   `palette_blocked`, `WY_CHECK_AT`, `cmp`, `write_lyc`).
2. **Sub-M-cycle observers in double speed** (STAT phase 2, IF write)
   sample at odd half-dots. An edge at odd `H` is then seen one dot
   earlier by them than by a read at the same boundary. Gambatte's odd
   double-speed constants (1.4) are such edges.

A half-dot runtime clock (doubling every `u64` clock, timer, DMA, APU) is
**rejected**. It buys nothing, because M-cycle boundaries are always on
dots (1.2), and it costs conversions on the hottest paths.

### 3.2 Observer views of the STAT line sources

`Ppu::update_at`, `mode_source` and `cmp` take a `View` (a `Copy` enum or
a `const` generic, decided by measured code size):
- `View::Walk`: the update-point walk and the IRQ edges as dispatch and
  IF reads see them. Mode-2 source for lines 1-143 from `Ly3` until the
  compare point. Compare point `CMP` (lines 1-152).
- `View::StatPhase1`: a STAT write's first phase. The mode-2 window is
  gambatte's `statChangeTriggersM2IrqCgb`, G in [-4,-3], mapped into our
  positions (step 2 fixes the end at t = 4, so the window is t in [3,4]).
- `View::LycWrite` and `View::StatPhase2`: today's `mode_source` and
  `cmp`, unchanged at single speed. The mode-2 source never extends past
  t = 3, so the three phase-0 sentinels keep passing.
- `View::Read`: `mode`, `lyc_bit`, `ly`, unchanged except where a step
  says otherwise. `Ppu::cmp` for the STAT read on lines 144-152 turns off
  at t = 3, like lines 1-143 (step 5).

The STAT line state (`IrqLine`) stays shared and physical: a write raises
it under its own view, and the walk's next point re-evaluates it under
`Walk`. That is enough for blocking. Gambatte's predicates have no line
at all, so where they and the line disagree, the view decides.

### 3.3 Edge changes this design expects

These are hypotheses, each verified by its step and kept only with zero
regressions:

| Edge | Today | Proposed | Phases it changes (move rule) | Reference |
|---|---|---|---|---|
| LYC compare IRQ, lines 1-152 (Walk), single speed | 4 (`Ly4`, `V4`) | 5 | r = 0 only | gambatte G = -2 vs m2 G = -4; 2.1 example; dfb1c67 |
| Same, double speed | 4 | 5 | ds even only | 2.1 ds example; dfb1c67 note (+13/-6, before the views) |
| Mode-2 source end (Walk) | 4 | = LYC compare point | r = 0, ds even | gambatte `lycIrqBlockedByM2OrM1StatIrq` (m2 blocks LYC IRQ on 1-144) |
| Mode-2 window (StatPhase1) | t < 4 | t <= 4 | r = 0 (phase 1 only) | `statChangeTriggersM2IrqCgb` |
| Coincidence bit, lines 144-152 (Read), single speed | off t = 0-3 | off t = 3 | r = 0, 1, 2 (never r = 3) | `getLycCmpLy`; `offset{1,2}_lyc8fint_m1stat_1` |
| Line-0 mode-2 IRQ (Walk) | 4 | 4-5, by sweep | r = 0 / ds even | gambatte G = -2 (line 0) vs -4 (others) |
| STAT phase 2, double speed | `now + 1` | `now`, after phase 1 and after the PPU's dot-`now` points (half-dot) | ds both parities, only where an edge sits at `now+1` | SameBoy `GB_CONFLICT_STAT_CGB_DOUBLE` `GB_advance_cycles(gb, 1)` |
| HBlank / HDMA trigger | `HBLANK_DELAY` 3 (`hdma-trigger`: 2) | from the HDMA view after steps 1-3 | all | gambatte `isHdmaPeriod` |

### 3.4 Composition with double speed

`View` x speed selects the constant. Double-speed positions are the same
half-dot `H` with `k` scaled by 1 instead of 2. The default double-speed
grid is odd and `ds_lcdoffset1` is even, so the move rule with `g = 2`
gives the phases each change touches.

The STAT phase-2 fix: `bus.rs` FF41 calls `run_ppu_to_call(now)` once and
applies phase 2 at `now` (single speed: `now + 1` as today). This is
exact for whole-dot edges, since `2*now + 1 >= 2*E` iff `now >= E`. For an
edge at odd `H = 2*now + 1` the phase-2 evaluation must see the PPU's
update first: `run_ppu_to_call(now)` already ran every point at `now`, and
the walk's point positions are whole dots. If the sweep shows an
odd-`H` edge is needed, it gets a point of its own.

### 3.5 Constants replaced

These literals move into `edges.rs` as derived constants:

| Location | Constants |
|---|---|
| `ppu/mod.rs` `Pt` positions | 3, 4, 5, 2, 6, 8, 12, the `+1` HBlank, `89`/`91` latch |
| `Ppu::ly` | `t >= 3`, glitch `t + 2 - double` |
| `Ppu::cmp` | `t < 3`, `t < 4`, `t < 6`, `8`, `12` |
| `Ppu::mode` | `t < 4`, `84`, `86`, `l == 144 && t < 5` |
| `Ppu::mode_source` | `t < 4`, `l == 144 && t < 5` |
| `Ppu::mode3_end` | the `+ double` |
| `vram_blocked` | `[84, 91 / 86]` |
| `oam_blocked` | `[0,84] [2,84] [3,86] [4,86]` |
| `palette_blocked` | `[87, 88]`, `extra` 2/4 |
| `write_lyc` | `t < 2`, 153 windows `2..5`, `5..12`, `12..24` (were `2..6`, `6..12`; step 5) |
| other | `WY_CHECK_AT`, `hdma::HBLANK_DELAY`, `bus.rs` FF41 `now + 1`, FF45 `now + !double` |

Step 0 moves them with no value change, proven by `diff-fingerprint.sh`.

### 3.6 Walking the table

Step by step, each claim checked with the move rule:

- **Step 1** (LYC Walk 4 -> 5, single speed, plus StatPhase1 m2 window
  t <= 4, plus Walk mode-2 end at 5). By the move rule this only changes
  r = 0 and the b+1 observers' view (untouched by design).
  - Fixes: `offset3_lyc8fint_m1irq_2` (2.1), `offset3_lyc8fint_m1stat_2`,
    `offset3_lyc98int_ly_count_{1,2}`. With `hdma-trigger` underneath (as
    dfb1c67 was), it also fixes the r = 0 HDMA pair
    `hdma_late_m0halt_lcdoffset3_2` and `hdma_late_enable_lcdoffset3_2`.
    These are the 6 that dfb1c67 measured; on main alone, expect the 4
    `lcd_offset` ones.
  - Keeps `m2enable/late_enable_lcdoffset3_1` and
    `dma/hdma_late_enable_lcdoffset3_1`: their write at t = 4 is phase 1,
    and the StatPhase1 window covers t = 4.
  - The three phase-0 sentinels sample at b+1 under `LycWrite` /
    `StatPhase2`, which don't change.
  - Not walked: the r = 0 `lcd_offset/offset3_lyc99int_*` (LYC = 153 uses
    `L153Cmp`, not moved) and `miscmstatirq/..._lcdoffset3` (line 0,
    `Ly4` line 0 not moved).
- **Step 3** (double speed, the same two moves). This changes only ds
  even. The double-speed rows above with LYC sync or LYC write (2.1 ds
  example) are its targets. dfb1c67's -6 has to be re-measured with the
  views in place before any claim.
- **Step 4** (double-speed phase 2 at the half-dot). These rows are its
  targets: `m0enable/late_enable_ds_lcdoffset1_1`,
  `m1/ly143_late_m{0,2}enable_ds_lcdoffset1_2`,
  `lycEnable/*ff41*_ds_lcdoffset1_2`. In double speed the mode-0 select is
  the phase-2 select, and these are the write-side double-speed failures
  with no LYC-sync explanation.
- **Step 5** (line 144 coincidence read view, line-0 m2 edges, the line
  153 LYC write).
  - Covers `offset{1,2}_lyc8fint_m1stat_1`. The move rule says r = 3 is
    never touched, since the bit's off-start moves inside [0, 3).
  - Covers the `m2irq_count` family (2.2), `late_enable_ly0_*`, and
    `lyc153_late_ff45_enable_lcdoffset1_1`. These **are not proven
    here**: 2.2 shows the constraint couples two IRQ edges with IF
    read/write observers, and the sweep (4.2) finds the values.
- **Steps 6-10** cover rows that don't depend on the IRQ sync alone (LY
  read, access windows, m0 per SCX, WY, HDMA/HALT). Each row names its
  observer, and its direction (early/late at one phase) tells the sweep
  which way to move.

**Weakest point:** step 5 and later are not derived. The table gives only
directions, and some may need an odd-`H` edge or a new observer (IF
reads at a different sub-M-cycle point than dispatch, 2.2).

## 4. Cost and plan

### 4.1 Hot path

| Path | Cost |
|---|---|
| STAT/LY reads | unchanged: the same `t >= CONST` compares, other constants |
| `Bus::boundary`, scheduler | unchanged |
| `Ppu::step` / `next_point` | compare-point offsets become speed-dependent (dfb1c67 threads `double` through `step`/`seek`): ~2-4 Ir per point, ~3 non-quiet points per line, ~1.5-2k Ir/frame worst case (< 0.05% of the 4.02 M line) |
| Splitting mode-2 end and compare into two points | **not planned**: the move keeps one point per line. If a sweep forces a split, +1 point per visible line while a source is selected: ~40 Ir x 144 ~ 6k Ir/frame (0.15%; worst-case headroom ~31k). Measure before accepting. |
| `write_stat` / `write_lyc` | a view argument: cold paths, ~0 |
| FF41 double-speed phase 2 | one fewer `run_ppu_to_call` in double speed (a saving) |

Bench budgets to watch:
- `synth:worst-case` (99.2% of the line)
- `Ppu::point_irq` (494/520 B)
- `Ppu::next_event` (562/590 B)
- `Ppu::sync` (914/960 B)
- `Ppu::seek` (576/605 B)

dfb1c67's `double` parameter on `step` is inlined into `next_event` and
`sync`. Measure both at step 1.

### 4.2 Code size

Estimates from reading the code, before the 2-3x historical factor:
- edges table and `const fn`s: ~0 B (compile time)
- views on `mode_source`/`cmp`/`update_at`: +80-150 B
- dfb1c67-style `early_cmp` on writes: +150 B
- line-0, line-144 and 153 tweaks: +40 B
- FF41 double-speed: -20 B
- HDMA view: +40 B

That is **~300-400 B**, so plan for **~1,000 B** with the factor. The
budget is 97,500 B with ~94,970 used (~2,530 B left), shared with the
other M9 groups. Measure at every step (`check-code-size.sh`). Stop and
report if steps 1-4 together pass 600 B.

Tuning harness (step 0b):
- A `tune` Cargo feature in `gbc-core`, off in every gate build, so 0 B
  there.
- It reads each `H` from a `static` `AtomicI16` override table (safe, no
  `alloc`, no `unsafe`).
- A `romtest --edge NAME=H` flag sets the overrides.
- A sweep script (Elixir, per AGENTS.md) runs the lcdoffset subset plus
  the phase-0 sentinels for each candidate.

This replaces the hand fitting that stalled dfb1c67. The engineer
decides it (no new dependency, no public API change). Record it in an
ADR.

### 4.3 Steps

Each step is one behaviour. Verify each by:
- `romtest --filter '*lcdoffset*'` and `--filter 'lcd_offset/**'`
- the family's default-phase directory
- before merging: the full gate and `diff-fingerprint.sh` with every
  changed ROM explained

Zero regressions per commit. Work in a worktree, one cargo job at a time.

0. **Edges table, no value change.** Move the constants in 3.5 into
   `edges.rs` (half-dots and observers, `const fn` thresholds). Verify:
   fingerprint identical, size within +50 B.
   **Done** (ba63b92): fingerprint identical (4,020 runs), `.text` and
   every budgeted function byte-identical, bench within noise.
   `hdma::HBLANK_DELAY` stays in `hdma.rs` (step 10 replaces it).
   0b. The `tune` feature, `romtest --edge`, and the sweep script. Verify:
   default build byte-identical; a sweep of `CMP` over 8-11 on
   `lcd_offset/**` reproduces dfb1c67's 4 `lcd_offset` flips.
   **Dropped** (orchestrator, session 11): no runtime knobs in gbc-core;
   sweeps edit the constants in `edges.rs` and rebuild.
1. **LYC compare (Walk) to t = 5, single speed, lines 1-152, with the
   view split (Walk mode-2 to 5, StatPhase1 m2 window to t <= 4,
   LycWrite/StatPhase2 unchanged).** Expect the 4 `lcd_offset` flips of dfb1c67 (6 on top of
   `hdma-trigger`) and no regressions (3.6). Rebase `hdma-trigger` on it and re-check the
   `hdma_late_m0halt_*` pair.
   **Measured, held** (branch `lcd-offset-step1`, 32c017b; full manifest
   against step 0, which is fingerprint-identical to main):
   - On main: +5 (the 4 `lcd_offset` ROMs predicted, plus
     `dma/hdma_late_m0halt_lcdoffset3_2`, predicted only with
     `hdma-trigger`), **-1: `dma/hdma_late_enable_lcdoffset3_1`**. The
     prediction "its write at t = 4 is phase 1" was wrong: it is an FF55
     write, which no STAT view covers. At offset 3 the LYC sync moves one
     M-cycle and the HDMA period edge would have to move with it (step
     10). `HBLANK_UNTIL` 3, 5, 6, 7 on top of `hdma-trigger` don't fix it
     (5 loses `_2` and `hdma_late_m0halt_lcdoffset3_2`; 6-7 lose 5; 3
     loses 6 default-phase ROMs). Its source isn't in our ROM set.
   - With `hdma-trigger` rebased on top (clean rebase): +36 / -2 (the
     same ROM plus `hdma_late_m0halt_ds_lcdoffset1_2`, the known swap).
     The doc predicted +6 over `hdma-trigger`'s +30: measured +6 flips,
     but one regression stays.
   - The `m2enable/late_enable_lcdoffset3_1` and the three phase-0
     sentinels held, as 3.6 predicted (the views work).
   - Cost, as built (`edges::cmp_walk(double)` threaded through `step`,
     `next_point` and `seek`; `point_irq` splits `Ly4` for line 0; an
     `early_cmp` on writes; a re-seek on speed switches): `.text`
     +718 B (over the 600 B stop line); over budget `Ppu::seek` 672/605,
     `next_event` 626/590, `point_irq` 560/520, `sync` 962/960; bench
     cgb-acid2 +3.24% (guard 3%), worst-case +0.16%. The 4.1/4.2
     estimates (~0.05% Ir, 300-400 B) were low again.
   - Unit tests for the views, mutation-checked (6 mutations caught).
   Options: (a) a per-speed comparison offset kept in a `Ppu` field set
   at speed changes, instead of computing from `double` in the walk, and
   keep `point_irq`'s `Ly4` arm uniform, to win back size and Ir; (b)
   land step 1 together with a step-10 HDMA fix for
   `hdma_late_enable_lcdoffset3_1`; (c) accept the 1-ROM regression
   (the human's call).
   **Step 1b** (branch `ppu-edges-step1b`, option (a); held, not merged):
   - The walk's comparison point rides in `Cursor::cmp` (the struct's
     padding; a `Ppu` field grew `Ppu` 416 -> 424 B and shifted field
     offsets, +200 B of MCU code), set by `Ppu::speed_changed`, so
     `step`/`next_point`/`seek` take no speed. `sync` writes back only
     `pos`/`line`/`pt` (-96 B).
   - `point_irq` keeps main's `Ly4` arm (`eval(2) | eval(NO_MODE)`):
     instead of splitting it by line, `update_at` keeps the line up for
     the walk's still-live mode-2 source when a write's view saw it end
     (lines 1-143, `MODE2_SOURCE_END <= t < cmp`), with no interrupt. Same
     results as the split, and the split cost +2.8 M x86 Ir on cgb-acid2.
   - `early_cmp` tests the cursor (synced to the write) instead of
     dividing; `speed_changed` moves a pending `Ly4`/`V4` and runs it at
     once if it is then due, instead of a full re-seek.
   - Line 0 stays at `t = 4`: moving it too measured +2/-2
     (`offset3_lyc99int_m2irq_count_1`, `lycstatwirq_trigger_ly00_10_50_1`
     vs `lycwirq_trigger_ly00_stat50_2`, `lyc0_late_m2enable_lycdisable_2`;
     step 5).
   - Result: the same +5/-1 as 32c017b. `.text` +412 B vs main (95,654);
     `point_irq` 502 (= main), `seek` 560, `next_event` 570, `sync` 882
     (below main's 914); no budget raised. Bench vs baseline: cgb-acid2
     +2.44% (main alone +2.10%), worst-case +0.04%. 7 mutations caught.
   - Still held for `dma/hdma_late_enable_lcdoffset3_1` (FF55=80 written
     97 M-cycles into a LYC=1 handler; out 1 = the block runs at once,
     its `_2` one M-cycle later expects 0, and the default-phase `_2`
     fails on main too). It needs the HDMA-period edge (step 10).
2. *(merged into 1 if 1 alone regresses
   `m2enable/late_enable_lcdoffset3_1`)*
3. **The same in double speed.** Targets: the ds-even rows with LYC sync
   or LYC write. Re-measure dfb1c67's -6 and fix or move each one under
   the views.
   **Done with step 10** (branch `m9-step1b-hdma`): `CMP_WALK_DS` = 5
   alone measured +13 / -5 against the branch (all 4 `hdma_*ds_lcdoffset1`
   rows pass); the -5 were write views that moved with it, each
   fixed by one double-speed constant: the STAT phase-1 mode-2 window to
   `t <= 4` (as single speed), VRAM from 85, OAM writes from 3, LYC-write
   deferral to `t < 3`. Unit tests mutation-checked (5 of 5 caught).
4. **STAT phase 2 at the half-dot in double speed.** Targets: 5-6 ds-even
   STAT-write rows.
5. **Line-0 / line-144 / line-153 edges.** Coincidence read view on lines
   144-152 (2 ROMs), the line-0 m2 IRQ and write windows (sweep;
   `m2irq_count` x4, `late_enable_ly0` x2), the LYC write at line 153
   (1).
   **Line 153, done (branch `m9-ly153-ds`).** Traced (temporary
   instrumentation): at the default double-speed phase (M-cycle
   boundaries at odd `t`) the LYC=153 compare at `t = 6` dispatched at
   `t = 7`; `lyc153int_m2irq_ifw_ds_1` needs the line-0 mode-2 IRQ
   (0:4) after its IF write, which landed at 0:5, so the sync must be one
   M-cycle (2 dots, not the estimated 3) earlier. `L153_CMP` 6 -> 5 (walk,
   read and LYC-write views; gambatte `lyc*456 - 2` for LYC 1-153 alike,
   so the same dot as `CMP_WALK`). Two companions it needed: (a) the
   dispatch clears a VBlank/STAT IF bit half-way into its last M-cycle
   (SameBoy `GB_cpu_run`, 2 T-cycles; `Bus::acknowledge`), as
   `lycint152_lyc153irq_late_retrigger_ds_1` regressed without it and six
   `*_late_retrigger_ds_1` / `_scx1_1` rows flip with it (timer and serial
   keep the after-the-M-cycle clear: `tc00_irq_late_retrigger_ds_2`,
   `start_wait_trigger_int8_read_if_ds_2` regress otherwise); (b) the WY
   checks half a dot later in double speed (`WY_DS_LATE`) with
   `WY_LEAD_DS` = 1 (`mid-line-ppu.md` section 7, "Late WY writes").
   Full manifest +27 / -0; `diff-fingerprint.sh main` 54 ROMs (the 27,
   plus 27 LYC=153- or line-0-synced siblings that keep their status).
   Left: `lyc153int_m2irq_late_retrigger_ds_1` and
   `lycint152_lyc0irq_late_retrigger_ds_1` (an edge one dot before the
   boundary, line 0 `t = 4` and `L153_ZERO`, absorbed here but not on
   hardware: likely half-dot edges), `m2enable/late_enable_ly0_ds_1`.
   **Line 0 and LYC=0 in double speed, done (branch `m9-ds-m2`):** the
   walk's line-0 mode-2 pulse at `t = 5` (mode-1 end at 4, the mode-1
   select blocks the pulse) and LYC=0 at 153:13, double speed only
   (`docs/reference/stat-irq.md`); +4 incl. `offset1_lyc99int_m2irq_count_ds_1`.
   **Line 144 coincidence read, done (branch `m9-lcd-offset`):** lines
   144-152 keep the previous line's comparison to t = 2 for the reads and
   write views, as lines 1-143 (`edges::VCMP_OFF`; gambatte
   `getLycCmpLy`); the walk keeps its off-start at 0. Fixes
   `offset{1,2}_lyc8fint_m1stat_1` (C4 at 144:2 and 144:1), +2 / -0,
   -154 B (the arms merged).
   **Line-0 pulse for the CPU, done (same branch):** single speed, the
   pulse stays at 4 in the walk, but its deadline is one past it, so an
   IF read or interrupt check at a boundary on 0:4 (offset 3 only) misses
   it, at gambatte's 0:5. Fixes `offset3_lyc99int_m2irq_count_1`, +1 / -0,
   +16 B. Tried first and rejected: the single-speed LYC=153 walk compare
   at 4 (+1 / -7: every `offset3_lyc99int_*_2` and
   `lycint152_lyc153irq_ifw_1`, `lycstatwirq_trigger_ly00_10_50_lcdoffset3_2`);
   the walk's pulse at 5 (+1 / -4, gambatte's select latch, see
   `docs/reference/stat-irq.md`).
6. **LY read edge re-check** after 1/3 (`ly_count_ds_{2,3}`). Possibly no
   change.
7. **Access windows:**
   - VRAM read/write start at r = 1 (2). **Done** (with step 10): single
     speed from 86, two dots after the mode-3 STAT change (gambatte
     `vramReadable` `< 79`); double speed 85 (with step 3).
   - OAM write start at r = 2 (1)
   - OAM read in double speed (1)
   - palette mode-3 start in double speed (2)

   One sweep per observer. These are independent of the IRQ views.
8. **Mode-0 edges:** the m0 STAT read per SCX at r = 1 (2), the m0 write
   view (1).
9. **WY check and window enable** (5).
10. **HDMA view and HALT wake:** merge `hdma-trigger` and fix the
    `hdma_late_m0halt` pair, `hdma_late_enable_*lcdoffset*` (4). This
    takes the human's open "merge with a 1-ROM swap?" item off the table.
    **Done, without an HDMA change** (branch `m9-step1b-hdma`, two
    commits on 39fb3cb). Traced sample positions (next line's `t`) fit
    one HDMA period at every phase once the syncs are right: in up to
    t = 3, out from 4, both speeds (`HBLANK_UNTIL` 4 holds).
    - `hdma_late_enable_lcdoffset3_1`: the enable is in the period (block
      at once); the block's stall moves the test's VRAM read to t = 84,
      which we blocked. Fixed by the VRAM edge (step 7).
    - `hdma_late_{enable,m0halt}_ds_lcdoffset1_*`: their LYC=1 sync
      dispatched one M-cycle early (double-speed compare at 4), putting
      the samples 2 dots early (the "~3 clocks off" of ADR 0017). Fixed
      by step 3.
    - Full manifest vs baseline: +51 / -0 (the branch's 36, plus 15);
      `.text` +24 B, ITCM set +16 B.

## 5. Open questions, risks, human decisions

- **Gambatte as the only reference.** SameBoy explicitly does not model
  odd phases. Gambatte is GPL: transcribe behaviour (thresholds), never
  code. Its predicate-per-write structure doesn't map one-to-one onto our
  STAT line. Views are the bridge and may need more variants than the
  five in 3.2.
- **IF read vs dispatch.** In 2.1 the two sit consistently with
  gambatte's `x`. In 2.2 they may not. If the sweep shows IF reads need
  their own IRQ visibility (an observer at a different sub-M-cycle point
  than dispatch), the IF register gets a per-source "visible from"
  position. That is a design change to `iflags` and a hot-path cost.
  Re-plan it if it happens.
- **Double-speed PPU-side differences.** ADR 0014's double-speed
  deviations (mode 0 +1, line 153) may be odd-`H` edges and not true
  speed differences. Step 0 keeps them as per-speed overrides. Don't
  unify them without a sweep.
- **Size.** Past PPU estimates ran 2-3x low. If steps 1-4 pass 600 B,
  stop and bring the trade-off to the human. That isn't a decision yet.
- **Human decisions:** none needed to start.
  - The existing "merge `hdma-trigger` with a 1-ROM swap?" item becomes
    moot if step 10 lands. Recommend holding the branch until step 1
    and rebasing it then.
  - Any step whose best sweep result regresses a passing ROM is
    escalated as usual, not merged.

## Appendix A: reproducing the pairing

```sh
grep -i "lcdoffset\|lcd_offset" acceptance/results/stage1.json \
 | sed 's/^ *"gambatte\///; s/",*$//; s/": "/ /' \
 | sed -E 's/_cgb04c_out([0-9A-Fa-f]+)\.gbc/ \1/' \
 | sed -E 's/^(.*)_([0-9]) ([0-9A-Fa-f]+) (pass|fail)$/\1 \2 \3 \4/' \
 | awk '{k=$1; r[k]=r[k] sprintf(" _%s:%s(%s)", $2, ($4=="pass"?"P":"F"), $3)}
        END{for(k in r) print k r[k]}' | sort
```

165 ROMs in 80 families on main (121 pass, 44 fail).
