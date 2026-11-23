# M9 triage: failing manifest ROMs by root cause

- Date: 2026-11-23. Input: full-manifest run at f200ab4
  (`target/acceptance/stage1.{md,json}`, failure images in
  `target/acceptance/stage1/failed/`). 506 failing: should 1, stretch 505
  (must 86/86). Analysis only, no code changed.
- Method: grouped by directory and name stem, then sampled ROMs per group
  (failure images, the `dmg08`/`cgb04c` expected values in the gambatte
  names, pass/fail patterns inside a directory). The gambatte `.asm`
  sources are not in `test-roms/` (only `.gbc` + PNGs). Where a group says
  "hypothesis", the first fix step is to confirm it against the source.
- Budgets at HEAD: gbc-core `.text` 95,730 of 96,000 B (**270 B left**);
  `synth:cgb-raster` +2.60% of a 3% guard; worst case ~97% of the 4.02 M
  Ir/frame line. Most groups below are cold-path code, so the binding
  constraint is `.text`, not cycles. Beyond the first one or two small
  fixes, everything needs the `.text` raise already proposed in
  `docs/design/mid-line-ppu.md` section 6 (96,000 -> 97,000 B), or equal
  savings. Bring that to the human before the third fix.
- Cost: S = one session, a contained rule; M = 2-3 sessions or a
  calibration sweep; L = a design step or several mechanisms.

## Summary (ordered by ROMs per cost)

| # | Group | ROMs | Subsystem | Cost | .text / perf risk | Design step |
|---|---|---|---|---|---|---|
| 1 | CGB palette RAM blocked in mode 3 | 33 | bus (BCPD/OCPD) | S | ~+80 B, cold; none | none (M5 task) |
| 2 | OAM DMA: CPU write during a conflicting transfer | 58 | `dma.rs` conflict table | S-M | ~+50 B, cold; none | none (**needs human: reference choice**) |
| 3 | LCDC.2 (object size) sampled per object | 20 | PPU OAM scan (mode 2) + object fetch | S-M | ~+150 B, cold; low | partly F |
| 4 | Window stall/start in mode 3 (+ WX=A6/obj X=A7 edge) | 97 | PPU mode-3 length, renderer | L | ~+350-450 B; cgb-raster low | **E1 done** (41 flips) |
| 5 | Live SCX&7 discard at mode-3 start | 19 | renderer line start, mode-3 length | M | ~+100 B; check on the line-start path | **done** (21 flips; 5 residual) |
| 6 | STAT/LY sub-dot edges at the other CPU phase (`lcd_offset`) | 33 | PPU lazy timing (ADR 0014) | M | ~+100 B; medium (STAT event math) | none (existing M5 task) |
| 7 | APU square trigger/duty phase (gambatte `sound`, `speedchange`) | 27 | APU trigger delay, 1 MHz phase | M | ~+100 B; low (trigger path) | none |
| 8 | HDMA timing (4 sub-groups) | 75 | `vdma`, CPU HALT/speed switch, IRQ dispatch | L (M each) | ~+100-300 B, cold; low | HALT/wake: **6 flips** |
| 9 | STAT/LY edges in double speed | 31 | PPU lazy timing | M | as 6 | none |
| 10 | Line-0 mid-line SCX write | 7 | PPU line 0 start timing | S-M | small | A residual |
| 11 | Object stall vs mid-line writes | 12 | renderer stall/fetch state | M | ~+150 B; cgb-raster | **F** |
| 12 | STAT IRQ edges, single speed (late STAT/LYC writes, LY 143/144/153) | 47 | PPU STAT line | L | ~+200 B; medium | none |
| 13 | OAM DMA: VRAM source vs CPU reads; misc | 10 | `dma.rs`, HDMA interplay | M | small | none |
| 14 | Mealybug/AGE residuals (A-C, G, acid-hell) | 9 | renderer | M-L (diagnose each) | ~+100-250 B | A-C residual, **G** |
| 15 | SameSuite APU, applicable to CGB C | 20 | APU (noise, sweep, envelope, NRx2) | L | +300-600 B; medium (integrate hot set) | none |
| 16 | SameSuite APU for other revisions | 7 | n/a | **sign-off** | none | none |
| 17 | AGE `spsw-interrupts-cgbBC` (should tier) | 1 | CPU speed switch + IRQ | **fixed** (1543bd8) | 0 B | none |

Total 506 (group 4 = 93 gambatte + 4 Mealybug).

## Groups

### 1. CGB palette RAM blocked in mode 3 (33)
`cgbpal_m3/*` (28: `cgbpal_m3end_*`, `cgbpal_m3start_*`,
`cgbpal_{read,write}_m3start_*`, 11 ds, 4 lcdoffset) and
`enable_display/ly{0,1}_late_cgbp{r,w}_*` (5). Reads expect FF and writes
are dropped while the PPU reads palettes (outFF / out00 / out55 names).
Cause: not implemented, `TODO(M5)` at `bus.rs` FF69/FF6B read and write
(plan M5 "Palette RAM access blocked in mode 3"). Refs: Pan Docs "CGB
Palettes" / "Accessing VRAM and OAM" (palette RAM inaccessible in mode
3); SameBoy `memory.c` BGPD/OBPD access (blocked flag set by `display.c`
around mode 3; check whether the index still auto-increments on a blocked
write). The block window's start/end dots are calibrated by the
`m3start`/`m3end` variants (the `_1` siblings that pass pin one side).
Fast path untouched: the check sits behind the PPU sync the palette write
already does.

### 2. OAM DMA: CPU write during a conflicting transfer (58)
**Done (session 10, ADR 0028):** gambatte rule implemented; 57 flip to
pass. Left: `src0000_busyint0002` (want `FF941234`; the interrupt
dispatch's pushes, not the bus rule; undiagnosed).
`oamdma_src{0000,7F00,A000,BF00,E000,EF00,F000,FE00,FF00}_busy{push,call,int,rst}*`
and `srcA000_busywrite4000`. The pass/fail pattern is sharp: for every
source, **all `busypop*`/`busyread*` pass; the failures are pushes whose
target is on the conflicting bus** (`push0001`, `8001`, `A001`, `C001`
write 0000/7FFF/9FFF/BFFF; `E001`/`F001`/`FE01`/`FF01`/`FF81` pass), and
WRAM sources (C000, DF00) pass everything. Our screens differ in one byte:
`src0000_busypush0001` shows `0076AA34` (want `5576AA34`),
`src0000_busypushC001` `65001255` (want `65AA1255`), `srcE000_busypush0001`
`FFFFAA34` (want `55FFAA34`). The `00` matches our CGB C rule "a write is
dropped and, if the source is below A000, clears the OAM byte just
transferred" (`docs/reference/oam-dma.md`, from SameBoy `GB_write_memory`
model checks); the `FF` matches "E000-FFFF source: open". Hypothesis:
on CGB C the CPU's write data reaches OAM (DMG-like) instead.
**Reference conflict:** gambatte `cgb04c` results are hardware captures
of the target revision (ADR 0003); SameBoy's zeroing is a model check its
author applies to CGB C. They disagree, and nothing else in the manifest
pins SameBoy's version (the 350 passing `oamdma` ROMs include every pop
and read variant). **Recommendation:** gambatte wins for CGB C (behavior
only, no gambatte code); record the deviation in `oam-dma.md`. This is
fixable, so it is a *decision* for the human, not an unfixable group.
Ask before implementing (M9 bullet 3 names this group).

### 3. LCDC.2 sampled per object (20)
`sprites/late_sizechange_*` (13) and `late_sizechange2_*` (7); `sp00`,
`sp01`, `sp02`, `sp39` are the OAM index whose scan time the write lands
on, and most outputs are identical on DMG (`dmg08_cgb04c`). The OAM scan
checks each object's height with LCDC.2 as of that object's scan dot
(SameBoy `display.c` `add_object_from_index`; Pan Docs "OAM scan"), so a
mode-2 write splits the scan. `late_sizechange2` likely also needs the
object fetch to read LCDC.2 at the fetch (design step F). Cold: rescan only
when LCDC.2 changes in mode 2.

### 4. Window stall and start in mode 3 (97, step E)
`window/*` 86 (`late_disable*`, `late_reenable*`, `late_wx*`,
`arg/late_wy*`, `late_enable_afterVblank*`, `on_screen/*`; 21 ds, 5
lcdoffset; 44 have the same result on DMG, so no CGB glitch is needed for
them), plus the WX=A6 / object X=A7 right-edge cases
`m0enable/enable_wxA6_2x_spxA7_*` (6), `window/m2int_wxA6_*` (3),
`sprites/10spritesPrLine_10xposA7_m0irq_2` (1). All are `out` ROMs reading
STAT/IRQ timing, i.e. mode-3 length: the 6-dot window stall and the
window start are latched at `t = 89` (`ppu/mod.rs`) and not recomputed on a
WX/WY/LCDC.5 write. Refs: SameBoy `display.c` window activation
(`wx_triggered`, `GB_update_wx_glitch`), Pan Docs "Mode 3 length". Exactly
design step E (`docs/design/mid-line-ppu.md` section 5, step 4); Mealybug
`m3_lcdc_win_en_change_multiple`, `m3_lcdc_win_map_change`,
`m3_lcdc_tile_sel_win_change`, `m3_wx_4_change_sprites` also depend on it (counted here).
Needs the `.text` raise.

**Status (E1, `ppu-midline-e`):** 41 fixed (window stall rebuilt on
WX/WY/LCDC.5 writes in mode 3; design doc section 7 "Step E1").
**Status (E2, `ppu-window-e2`):** 25 more fixed (CGB LCDC.5 clear during the
stall shortens it; a WY write after the line's WY check keeps the old
match; section 7 "Step E2"). 31 left: double-speed `late_wy_1toFF_ds_2`/
`late_wy_ds_2`, `late_wy_FFto*`/`10to0`, `late_enable_afterVblank*`,
`late_wx_ff_*`, the WX=A6/obj X=A7 edge (10), re-trigger (`on_screen/*`,
Mealybug `m3_lcdc_win_*`, `m3_wx_4_change_sprites`).
**Status (`m9-win-a6`):** the WX=A6/obj X=A7 edge (10) fixed: stalls at
pixel 159 delay the STAT mode-0 change but not the HBlank interrupt
(gambatte xpos 166 vs 168; section 7 "HBlank interrupt vs stalls at pixel
159"). The other leftovers above are unchanged by it.
**Status (`m9-late-wy`):** 5 single-speed `arg/late_wy*` fixed (a WY
write from dot 455 no longer matches the current LY; `WY_LEAD` measured
after the SCX discard; a write before the pipeline starts is decided at
`latch`; section 7 "Late WY writes"). Left in the WY cluster: 5 double
speed (`late_wy_ds_2`, `arg/late_wy_1toFF_ds_2`,
`arg/late_wy_FFto{0,2}_ly2_ds_1`, `arg/late_wy_FFto2_ly2_scx5_ds_1`):
their LYC=153 sync looks 3 dots late in double speed (lcd-offset step 5),
not a WY rule. Group 4 now has 16 left.
**Status (`m9-ly153-ds`):** the 5 double-speed rows fixed (LYC=153 sync
one M-cycle late, then double-speed WY checks half a dot later and
`WY_LEAD_DS` = 1; lcd-offset step 5). Group 4: 11 left.

### 5. Live SCX&7 discard at mode-3 start (19)
`scx_during_m3/scx_0*c0/*_{3,4}` and `*_ds_{2..5}` (13 PNG ROMs, 6,600-
14,900 px: the whole frame is shifted by the fine-scroll difference;
e.g. `scx_0360c0_3` draws our light columns on the left, the reference on
the right), `scx_m3_extend_*` (2), `m2int_m3stat/scx/late_scx4_ds`,
`sprites/space/10spritesPrLine_late_scx4_ds`,
`enable_display/ly0_late_scx7_m3stat_*` (2). Hypothesis: the discard of
the first SCX&7 pixels compares against the **live** SCX each dot (SameBoy
`display.c` line-start discard loop; gambatte behavior agrees), while we
latch it once at line start (design doc section 1 says "once"). A write
inside the discard window changes both the image offset and mode-3
length. Not in steps A-G; a small extension of `hold_fetch` plus the
mode-3 length recompute.
**Status: done** (hypothesis confirmed). `hold_fetch` moves the discard
on an SCX write seen before its match; the compare runs 8 dots before the
pixel's output slot (`DISCARD_LEAD`). 21 fail->pass, 0 regressions
(`docs/design/mid-line-ppu.md` section 7, "Live fine-scroll discard").
Residual: `scx_0367c0` `_2`/`_ds_2` (1 px), `_3`/`_ds_3` (143 px),
`10spritesPrLine_late_scx4_ds` (fixed by step F: the stalls now follow a discard
move).

### 6. STAT/LY edges at the other CPU clock phase (33)
`lcd_offset/offset{1,2,3}_*` (17) and the `*lcdoffset*` variants in
`m0enable`, `m2enable`, `m1`, `lycEnable`, `vram_m3`, `oam_access` (16).
Existing plan task (M5, session 07): thresholds were tuned at one CPU
phase; after a speed switch to single speed the CPU sits at the other
phase. Fix the PPU thresholds (LY, LYC, mode 1/2 edges, VRAM/OAM block
edges) to half-dot resolution, not the halt length. Refs: SameBoy
`display.c` (8 MHz `cycles` resolution), gambatte `lcd_offset` naming.
Also flips some `dma` `lcdoffset` ROMs (group 8).

Status (session 11): design `docs/design/lcd-offset-phase.md`. Step 0
(edge table, no value change) merged. Step 1 (LYC comparison at t = 5,
single speed, with write views) held on branch `lcd-offset-step1`: +5 /
-1 (`dma/hdma_late_enable_lcdoffset3_1`), +718 B, cgb-acid2 +3.24%;
with `hdma-trigger` on top +36 / -2. Next: cut its size/Ir (design 4.3
step 1 options) and pair it with the step-10 HDMA edge.

### 7. APU square trigger and duty phase (27)
`speedchange/speedchange*_ch1_duty0_pos6_to_pos7_timing_*` (17),
`sound/ch1_duty0_*`, `ch1_init_pos_*`, `ch1_init_reset_sweep_counter_*`,
`ch2_init_reset_env_counter_*` (10); 23 report silence where sound is
expected (we are one duty step off). Known deferral
(`docs/reference/apu.md` "Not modelled": trigger at an odd base clock,
the extra delay when retriggering an active channel; `lf_div` derived from
`now >> 1` and the power-on phase, which a speed switch shifts). Refs:
SameBoy `apu.c` (`lf_div`, square `sample_countdown` on trigger), ADR
0021 section 12. Trigger path only, not the integrate loop.

**Status (2026-11-23): 23 of 27 done**; plus 2 SameSuite ROMs outside
the group's count, and 4 SameSuite ROMs signed off (below). The cause was not one mechanism
(`docs/reference/apu.md` has the details):
- Done: the duty output bit latches at each step, so an NRx1 change
  shows from the next step (`ch1_duty0_to_duty3_pos3{,_2}`, 2); the
  post-boot CH1 keeps running at f = 0x7C1 in a phase fitted to
  `ch1_init_pos_*` (`ch1_init_pos_{1,2,3,8}`, 4). The same round also
  flipped SameSuite `channel_{1,2}_duty_delay` (2, group 15's set).
- Done: in single speed a trigger's first duty step lands on the APU's
  1 MHz grid (clocks `1 mod 4`, 3 clocks before SameBoy's step), and the
  post-boot CH1 phase moves to that grid (145, inside its fitted window):
  11 of the 12 `speedchange*_ch1_duty0_pos6_to_pos7_timing{,_nop}_2`
  (trigger in single speed). Not the switch itself: each `_1`/`_2` pair
  brackets the step to the same grid after 1-5 switches.
- Done: in double speed a trigger follows gambatte's CGB-C linear rule
  (one period plus `6 + 2 * lf_div` clocks, off channel; 4 fewer on):
  `ch1_duty0_pos6_to_pos7_timing_ds_6` and the 5 `speedchange*_ch1_duty0_
  pos6_to_pos7_timing*ds_2` (cgb04c), 6. It regresses SameSuite
  `channel_{1,2}_align{,_cpu}` (4), which expect SameBoy's CGB-E sign.
  **Signed off by the human (2026-11-23, session 11):** "CGB E result"
  (SameSuite `apu/README.md`: CPU-CGB-C fails most channel 1/2 tests;
  ADR 0003: CPU CGB C); excluded from `same-suite-apu` and kept running
  in the stretch group `same-suite-apu-cgbE`, not blocking Stage 1.
- Open: `speedchange_ch1_nr4init_..._timing_2` triggers in double speed
  after a switch with the APU on and still fails with the linear rule;
  the grid rule in double speed flips it but also costs SameSuite
  `channel_{1,2}_duty` and 4 `speedchange*_ds_1` (measured).
- Done (2026-11-23): the 2 sweep ROMs (`ch1_init_reset_sweep_counter_timing_10`,
  `_nr52_3`) and `ch2_init_reset_env_counter_timing_15`: the sweep step
  4 clocks after its edge, and the trigger windows for the sweep (8
  clocks) and the envelope (step 7, 4-clock lead), from gambatte's
  cgb04c captures (`docs/reference/apu.md`, frame sequencer). Also flips
  SameSuite `channel_1_sweep_restart_2` (group 15). All 161
  `gambatte/sound` ROMs pass.

### 8. HDMA timing (75)
`dma/*` 64 + `irq_precedence/{,late_}hdma_vs_*` 11. Sub-groups, each M:
- **HDMA across a speed switch** (26): `hdma_late_m3speedchange_*`,
  `hdma_transition_speedchange_*`, `hdma_m0speedchange_*`. 12 of them are
  the phase-luck ROMs of group 6's plan task.
- **HDMA vs HALT/wake** (22): `hdma_*halt*unhalt*`, `hdma_transition_*halt*`.
  ADR 0017 deferral "HALT wake with IME off runs the instruction before
  the block (SameBoy: block first)".
  **Status (`hdma-halt`, ADR 0017 amendment):** 6 fixed: block before the
  woken instruction, wake-up mode-0 check at the end of the wake-up
  M-cycle, HALT sample against the HDMA trigger (`late_m0unhalt_{1,ds_1}`,
  `late_m3halt_m0unhalt_scx{1_3,2_2}`, `transition_halt_hdmadst_unhalt`,
  `transition_oamdma_2` from Misc). 16 left, not yet analysed: wake in
  mode 2 after a mode-3 HALT (`late_{,ei_}m3halt_m2unhalt_*` 5), the
  `transition_*halt*` set (8: `late_unhalt` with/without `ei`/`ldaaimm`,
  `7fffhalt_inc_m3unhalt`, `halt_m0unhalt_ldaaimm`),
  `m0halt_late_m3unhalt_scx1_2`, `late_if_and_ie_halt_2`, and the
  `lcd_offset`-phase pair `late_m0halt_{ds_lcdoffset1_1,lcdoffset3_2}`
  (SameBoy's HBlank suppression in the wake-up M-cycle would swap
  `lcdoffset1_1` for `_2`; wait for the lcd_offset phase fix).
- **Trigger vs mode-0 start and IRQ dispatch** (19): `hdma_late_enable*`,
  `hdma_start_scx5*`, `hdma_late_disable_scx5*`, `irq_precedence` hdma.
  **Status (`hdma-trigger`, ADR 0017 trigger amendment):** 17 of 19
  fixed (trigger at STAT mode 0 + 2 dots, block before a due dispatch,
  HBlank period for FF55 enable/HALT/wake-up, triggers lost in a block's
  stall or in a speed-switch STOP's last M-cycle), plus 13 from the HALT
  and speed-switch sub-groups. Left: `hdma_late_enable_{lcdoffset3,
  ds_lcdoffset1}_2` (lcd_offset phase). Swapped by the same phase:
  `hdma_late_m0halt_ds_lcdoffset1_1` now passes, `_2` fails.
- **GDMA length in double speed** (4): `gdma_cycles_*_ds` (S).
- **Misc** (4): `hdma_pc_7ffe`, `late_gdma_pc_7ffe`,
  `hdma_transition_oamdma_{1,2}` (ADR 0017 "OAM DMA during VRAM DMA").
Refs: SameBoy `memory.c`/`sm83_cpu.c` HDMA, Pan Docs "VRAM DMA". Cold.

### 9. STAT/LY edges in double speed (31)
The `_ds` variants of `m0enable`, `m1`, `m2enable`, `lycEnable`,
`lcd_offset`, `ly0` (all 4), `lyc153int_m2irq` (2), `m2int_*`,
`irq_precedence/late_m0irq_retrigger_ds`. Same mechanism as group 6 at
half-dot CPU accesses; do after 6 (likely shares the fix).
**Status (`m9-ly153-ds`):** LYC=153 compares at `t = 5` (was 6) and the
dispatch clears a VBlank/STAT IF bit half-way into its last M-cycle:
+27 over the manifest (`lyc153int_m2irq_ifw_ds_1`, `ly0/lycint152_lyc153*_ds_2`
x3, 4 `*_late_retrigger_ds_1`, `lycEnable/lyc153_*_ds` x2,
`gdma_cycles_*_ds_1` x4, `lycwirq`/`lycstatwirq_trigger_ly00_*_ds_1`,
`lcd_offset/offset2_lyc99int_*_1` x3, `m2enable/late_enable_ly0_lcdoffset2_1`,
single-speed `m2int_m2irq_late_retrigger_1`, `late_m0irq_retrigger_scx1_1`,
and the 5 WY rows of group 4). Left here: `lyc153int_m2irq_late_retrigger_ds_1`,
`lycint152_lyc0irq_late_retrigger_ds_1` (edge one dot before the boundary).

### 10. Line-0 mid-line SCX write (7)
`scx_0*c0/scx_during_m3_2` and `_ds_1` (46-104 px): the diff image is
the first line only. Line 0's mode-3 start (or the fetch frontier origin
on it) is off by a few dots for the hold model. Refs: SameBoy line-0
special case in `display.c`. Step A residual.

### 11. Object stall vs mid-line writes (12, step F)
gambatte `scx_during_m3_spx{0,1,2}`, `spx2_ds`,
`scx_attrib_during_m3_spx{1,2}_ds` (6; 8-72 px) and Mealybug
`m3_bgp_change_sprites`, `m3_obp0_change`, `m3_lcdc_obj_en_change{,_variant}`,
`m3_lcdc_obj_size_change{,_scx}` (6). Design step F.
**Status: SCX part done** (`docs/design/mid-line-ppu.md` section 7, "Step F"):
5 fail->pass, 0 regressions (`scx_during_m3_spx{0,1}`,
`scx_attrib_during_m3_spx1_ds`, plus `10spritesPrLine_late_scx4_ds_1` and
AGE `m3-bg-lcdc-ds`). Left: `scx_during_m3_spx2`, `spx2_ds`,
`scx_attrib_during_m3_spx2_ds` fail only by the object's color at x 0
(power-on OBJ palette RAM, not timing; needs a human decision on a
power-on pattern). The 6 Mealybug ROMs are a different mechanism: palette
(BGP/OBP0) and LCDC.1/LCDC.2 writes landing inside an object's fetch or
stall (object enable checked at pixel output on CGB, LCDC.2 read at the
object's tile fetch, SameBoy `get_object_line_address`), not diagnosed
further here. `m3_lcdc_tile_sel_change` lines with X = 5-7 objects are a
new lead (section 7).
**Status (2026-11-23): the 3 palette-RAM ROMs are signed off (human); manifest group `gambatte-uninit-palram`.**

### 12. STAT IRQ edges, single speed (47)
`miscmstatirq/lycstatwirq_trigger_*` (9, all `outE0`: no IRQ expected
when a STAT write switches the source while the line stays high),
`m0enable/disable_*` and `lycdisable_ff45_*` (8, CGB differs from DMG),
`m1/*` (10: late mode-0/2 enable at LY 143/144, `m2m1irq_ifw`),
`m2enable/*` (7: LY 0 and after an LYC IRQ), `lycEnable/*` (8: LY 153,
`lcdoff_lycirqen`, `lyc_ff45_trigger_delay`), `m2int_*` (3),
`irq_precedence/if_and_ie_0_vector_4` and `late_m0irq_retrigger_scx1`.
Many need the CGB-specific landing of STAT/LYC writes against each
source's edge; `bus.rs` already models `GB_CONFLICT_STAT_CGB`, so these
are edge-position errors per source (mode 0 start, mode 1 start, LY 153
-> 0 LYC). Refs: SameBoy `GB_STAT_update` and `display.c` line 153;
Pan Docs "STAT interrupt" (blocking). Split per source when started.
`if_and_ie_0_vector_4` is CPU dispatch (IE cleared during the push ->
vector 0000; SameBoy `sm83_cpu.c`), S.

**Source switch to LYC, done:** a CGB STAT write that sets the delayed
select (LYC; double speed mode 0) keeps the selects it clears for the
first phase too, so switching from an active source to an active LY=LYC
keeps the line high (`docs/reference/stat-irq.md`). Flipped 7 of the 9
`lycstatwirq_trigger_*` (`m0_*_08_40`, `m0_*_bf_40`, `ly94_10_40`) and
`lycEnable/lyc153_late_enable_m1disable_3`. Left:
`lycstatwirq_trigger_ly00_10_50_1`, `_ds_1`, a different mechanism (the
mode-1 end at line 0 dot 4 and the LYC select landing at the same dot
count as simultaneous; reference note "Open"). `m0enable/disable_{2,
scx3_2,scx4_2,scx7_2}` and `lycdisable_ff45_{2,scx1_2,scx1_ds_2,scx2_2,
scx3_2}` are not this mechanism: they clear the mode-0 source (STAT write
`00`, or LYC -> `FF` with `48` selected) one M-cycle around the mode-0
start, so they test where mode 0 starts against the write per SCX (CGB C
differs from DMG), an edge-position question.

### 13. OAM DMA: VRAM source vs CPU reads, misc (10)
**Done (session 10, ADR 0028 amendment):** the 6
`src{8000,9F00}_busypop{7FFF,9FFF}*` pass (a conflicting read with a VRAM
source clears OAM byte `i`; recorded through atomics in `Memory`, applied
by `dma_sync`; no bench ROM slower). `src8000_srcchange0000_busyinc` is
re-diagnosed: not VRAM-vs-external but a restart, where the access one
M-cycle after the FF46 write already conflicts on the new source's bus
(gambatte keeps the old transfer running through the new start delay;
`oam-dma.md` "Restart"). We show 1, want 0. Not done.
Original diagnosis: the 6 `src{8000,9F00}_busypop*` need
gambatte's read side effect (CGB, VRAM source, conflicting CPU read
clears OAM byte `i`; ADR 0028 "Not modelled"). Needs a side effect on
the shared-reference read path: a design task.
`oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*` (6; e.g. `src8000_busypop7FFF`
shows the DMG answer `65765576`, CGB wants `65005576`) and
`src8000_srcchange0000_busyinc` (1): our table has VRAM-source vs external
access "free"; gambatte cgb04c shows a conflict. Same reference question
as group 2. Plus `oamdma_late_speedchange_stat_1` (group 6),
`oamdmasrcC000_hdmasrc0000` (HDMA + OAM DMA), `oamdmasrcC0_speedchange_readC000`.

### 14. Mealybug/AGE residuals (9)
Mealybug `m3_lcdc_bg_map_change` (466 px), `m3_scx_low_3_bits` (540 px,
pixels 152-159 undiagnosed, design step 2 notes), `m3_scy_change` (560),
`m3_lcdc_tile_sel_change` (846) after steps 1-3/D; `m3_lcdc_tile_sel_change2`,
`m3_lcdc_tile_sel_win_change2` (step **G**, CGB TILE_SEL glitch, SameBoy
`data_for_tile_sel_glitch`); AGE `m3-bg-lcdc{,-ds}` (256/512 px);
`cgb-acid-hell` (2 px, plan M5 bullet: attribute latch on an LCDC write
every 8 dots). Diagnose each against the Mealybug `*_cgb_c.png` and
SameBoy fetcher; some may resolve with E/F.

### 15. SameSuite APU, applicable to CGB C (20)
ch1/ch2 `duty_delay`, `nrx2_glitch`, `nrx2_speed_change`, `volume`;
ch1 `sweep`, `sweep_restart{,_2}`, `freq_change_timing-cgb0BC`; ch4
`align`, `delay`, `equivalent_frequencies`, `freq_change`,
`frequency_alignment`, `lfsr_7_15`, `lfsr_restart{,_fast}`. All are the
`apu.md` "Not modelled" list (envelope lock, CGB-C NRx2 glitch, sweep
calculation delay, noise counter phase and NR43 glitches). Refs: SameBoy
`apu.c` (MIT; the suite's author). Caveat: the unsuffixed SameSuite APU
tests were developed against SameBoy's default CGB E, and the suite notes
"some apu tests only work on CPU CGB E"
(`test-roms/same-suite/game-boy-test-roms-howto.md`). Any that turns out
E-only joins group 16. Touches the integrate hot set (`Noise::integrate`
etc.), so measure `synth:worst-case` APU cost (budget 230k Ir).

**Status (2026-11-23): 3 of 20 pass** (`channel_{1,2}_duty_delay`, group
7's duty latch; `channel_1_sweep_restart_2`, the sweep lag and trigger
window). Applicability of the 17 still failing, for a later sign-off (no
manifest change proposed here). Evidence keys: README = SameSuite
`apu/README.md` ("CPU-CGB-C - passes the channel 3 tests and
non-channel-specific tests. Most other tests fail", from a PCM12/PCM34
read glitch on C and older; "The NRX2 glitch ... behaves differently
across CPU-CGB revisions. Currently, only revision E is tested"); SB-C =
SameBoy `apu.c` has a separate `model <= GB_MODEL_CGB_C` path for it.
- `channel_1_freq_change_timing-cgb0BC`: **applies to C** (suffix names
  0/B/C). Missing: the frequency-change timing against the duty step on
  C and older (not investigated; its `-cgbDE` sibling is signed off).
- `channel_1_nrx2_glitch`, `channel_2_nrx2_glitch`: **E-only**. README
  (NRX2 glitch only tested on E); SB-C: `nrx2_glitch` on C goes through an
  intermediate 0xFF write; our zombie rule is Pan Docs' CGB-02/04 one.
- `channel_1_nrx2_speed_change`, `channel_2_nrx2_speed_change`:
  **E-only**. Same NRx2 glitch across a speed switch; same evidence.
- `channel_4_lfsr_7_15`, `channel_4_lfsr_restart`,
  `channel_4_lfsr_restart_fast`: **E-only**. README names "the LFSR
  tests" as failing on C from the PCM34 glitch (to be rewritten to pass
  on all revisions); SB-C: `nr43_write` glitch steps on C.
- `channel_4_freq_change`, `channel_4_frequency_alignment`: **E-only**.
  SB-C: the NR43 alignment term is `{2, 1, 4, 3}` on C vs `{2, 1, 0, 3}`
  on D+ (`docs/reference/apu.md`, noise), plus the C-only
  `countdown_reloaded` counter; README: SameBoy itself fails
  `channel_4_freq_change` on E.
- `channel_1_sweep`, `channel_1_sweep_restart`: **unclear**. Missing:
  SameBoy's shift-dependent calculation delay (PCM12 shows the
  intermediate frequency); README says C fails "most" ch1 tests without
  naming them, and no cgb04c capture covers the delay (all gambatte
  sweep captures pass). SB-C: `nr10_write_glitch` differs on C.
- `channel_1_volume`, `channel_2_volume`: **unclear**. Missing: SameBoy's
  envelope lock/clock and secondary DIV event; README as above; the
  gambatte cgb04c envelope captures all pass.
- `channel_4_align`, `channel_4_delay`, `channel_4_equivalent_frequencies`:
  **unclear**. Missing: the noise counter phase at trigger (SameBoy
  `r * 4 + 6` plus alignment); README: C fails most ch4 tests; SB-C has
  C-only noise counter paths.
Tally: 1 applies to C, 9 E-only, 7 unclear. **The 9 E-only ROMs were signed
off by the human (2026-11-23, session 12): "CGB E result", group
`same-suite-apu-cgbE` (cde49e4).** Fix targets: the 1 C ROM and the 7
unclear.

### 16. SameSuite APU for other revisions (7) - sign-off candidate
`channel_{1,2,4}_extra_length_clocking-cgb0B`,
`channel_3_extra_length_clocking-cgb0`, `-cgbB`,
`channel_1_freq_change_timing-A`, `-cgbDE`. The suffix names the
revisions the expected result holds for (0/B, D/E, AGB "A"); CGB C is not
among them, and `freq_change_timing` ships a separate `-cgb0BC` ROM for
our revision (group 15). Evidence: file-name convention of SameSuite plus
the howto note above; ADR 0003 (target CPU CGB C). Passing them would mean
modelling another revision. **Signed off by the human (2026-11-23,
session 10):** "targets another CGB revision"; kept in the manifest,
not blocking Stage 1.

### 17. AGE `spsw-interrupts-cgbBC` (1, should tier)
**Fixed** (session 12, 1543bd8, ADR 0020 amendment). The "garbage
registers" were AGE's normal failure exit (`B=0`), not a divergence. The
halt-interrupt rows already passed; the "immediate interrupt" rows (IE&IF
pending, `ei; stop`) were one M-cycle early at both speeds, i.e. the CGB E
table (SameBoy's model). On CGB B/C the held DIV reset also applies with
an interrupt pending and IME on. Not a sign-off candidate.

## Sign-off candidates (M9 bullet 3)
1. Group 16 (7 ROMs): other revisions. Strong evidence.
2. Groups 2 and 13 (65 ROMs): reference conflict, gambatte cgb04c vs
   SameBoy's CGB C model. Recommended resolution is to follow gambatte
   (hardware on our revision), so this needs a decision, not a waiver.
3. Possibly group 17 (1) and any group-15 ROM shown to be CGB-E-only,
   after the source is read.

## Recommended order
1. Group 1 (33 ROMs, S, fits in the remaining 270 B).
2. Group 2 (58, S-M) once the human picks the reference; then group 13.
3. Ask for the `.text` raise, then group 4 (step E, 97) and group 3
   (20, S-M); group 5 (19, M) next as it shares the mode-3 length
   recompute with E.
