//! Every PPU edge position the CPU can observe, in one place
//! (docs/design/lcd-offset-phase.md, section 3.1; ADR 0014).
//!
//! An *edge* is where a PPU signal changes, as a line-relative position
//! in **half-dots** ([`H`]): `H = 2*t + s`. A CPU M-cycle boundary always
//! sits on a whole dot (design doc 1.2), so an observer that samples at
//! the boundary (a read, interrupt dispatch, the first phase of a STAT
//! write) sees an edge from the first whole dot at or past it: [`t`].
//! Observers that sample later in the M-cycle (the LYC write and the
//! second phase of a STAT write, one T-cycle in) are [`Observer`]s; the
//! bus applies their offset as a whole-dot delay ([`Observer::delay`]).
//!
//! Everything here is `const` and compiles to the same immediate
//! comparisons the code made before the table existed. Where a reference
//! says the PPU itself differs in double speed (ADR 0014
//! "Consequences": the mode-0 STAT change one dot later, line 153's LY 0
//! one dot later), the edge has a per-speed value (`_SS`, `_DS`).
//!
//! Positions are ours (ADR 0014, ADR 0020 amendment): LY changes at
//! `t = 3`. The module docs of [`super`] ("Timeline") describe each line.

/// A line-relative position in half-dots.
#[derive(Clone, Copy)]
pub(crate) struct H(u16);

/// The edge at whole dot `t`.
const fn h(t: u16) -> H {
    H(2 * t)
}

/// The first whole-dot position at which an observer at the M-cycle
/// boundary sees edge `e`.
const fn t(e: H) -> u32 {
    (e.0 as u32).div_ceil(2)
}

/// Where in its M-cycle a CPU observer samples: `k` T-cycles after the
/// access (SameBoy `sm83_cpu.c` `cycle_write`, `cgb_conflict_map` and
/// `cgb_double_conflict_map`; design doc 1.3). A T-cycle is a dot in
/// single speed and half a dot in double speed.
#[derive(Clone, Copy)]
pub(crate) struct Observer {
    /// T-cycles after the access, single speed.
    ss: u8,
    /// T-cycles after the access, double speed.
    ds: u8,
}

impl Observer {
    /// The observer's sample point as a whole-dot delay after the access,
    /// rounded up: `ceil(k * (2 - double) / 2)` half-dots. In double speed
    /// a 1-T observer lands half a dot after the access, which the
    /// whole-dot timeline places a full dot late (design doc 3.4, step 4).
    #[inline]
    pub(crate) const fn delay(self, double: bool) -> u64 {
        if double {
            (self.ds as u64).div_ceil(2)
        } else {
            self.ss as u64
        }
    }
}

/// LYC write: `GB_CONFLICT_WRITE_CPU` in single speed (one T-cycle in),
/// `GB_CONFLICT_READ_OLD` in double speed (at the access).
pub(crate) const LYC_WRITE: Observer = Observer { ss: 1, ds: 0 };

/// The second phase of a STAT write (`GB_CONFLICT_STAT_CGB`, `_DOUBLE`):
/// one T-cycle after the first.
pub(crate) const STAT_PHASE2: Observer = Observer { ss: 1, ds: 1 };

// --- Line start (lines 1-153) ---

/// LY reads the new line from here (lines 1-152; line 153 reads 153).
pub(crate) const LY_NEW: u32 = t(h(3));

/// Dots before [`LY_NEW`] in which LY reads `LY & (LY + 1)` (the counter
/// mid-carry), single speed; one less in double speed. AGE
/// `lcd-align-ly`.
pub(crate) const LY_GLITCH: u32 = 2;

/// Line 153: LY reads 0 from here, single / double speed.
pub(crate) const LY153_ZERO_SS: u32 = t(h(7));
pub(crate) const LY153_ZERO_DS: u32 = t(h(8));

/// Lines 0-143: the walk's first point (`Pt::Ly3`): LY changes, the
/// comparison turns off for one dot, the mode-2 source rises.
pub(crate) const PT_LY: u32 = LY_NEW;

/// Lines 1-143: the comparison is off from here ...
pub(crate) const CMP_OFF: u32 = t(h(3));

/// ... to here, where it compares the new LY (all lines 1-152), the STAT
/// mode reads 2 (lines 0-143) and the mode-2 source ends (`Pt::Ly4`,
/// `Pt::V4`).
pub(crate) const CMP_ON: u32 = t(h(4));

/// STAT reads mode 2 from here (lines 0-143); before it, the previous
/// line's mode 0 (line 0: mode 1).
pub(crate) const MODE2: u32 = CMP_ON;

/// The mode-2 interrupt source lasts until here (lines 1-143; line 0:
/// mode 1). Shared by the walk and the STAT/LYC write views.
pub(crate) const MODE2_SOURCE_END: u32 = CMP_ON;

/// Line 0's mode-0 read quirk (CGB B/C, single speed): mode 0 at this dot.
pub(crate) const LINE0_MODE0_QUIRK: u32 = 3;

// --- Mode 3 and the access windows (lines 0-143) ---

/// STAT reads mode 3 from here.
pub(crate) const MODE3: u32 = t(h(84));

/// The first line after LCD enable: mode 0 until here, then mode 3.
pub(crate) const MODE3_FIRST: u32 = t(h(86));

/// The pixel pipeline starts (`Pt::Latch`); the first line after LCD
/// enable: [`PIPE_FIRST`].
pub(crate) const PIPE: u32 = t(h(89));
pub(crate) const PIPE_FIRST: u32 = t(h(91));

/// The shortest mode 3 from the pipeline start: the earliest `End`.
pub(crate) const MIN_MODE3: u32 = 167;

/// Double speed: STAT reads mode 0 this many dots after the mode-3 end
/// (SameBoy changes STAT at state 22 there).
pub(crate) const MODE0_DS_LATE: u32 = 1;

/// The HBlank interrupt source rises this many dots after the mode-3 end.
pub(crate) const HBLANK_AFTER_END: u32 = 1;

/// The mode-0 interrupt ([`HBLANK_AFTER_END`] past the mode-3 end) sees
/// the STAT selects and LYC as they were shortly before it: a STAT write
/// whose access is at most [`M0_STAT_LATCH`] dots before it (single,
/// double speed) no longer changes the selects it sees (clearing mode 0
/// doesn't stop it, clearing LYC doesn't unblock it), and a LYC write
/// whose access is at most [`M0_LYC_LATCH`] dots before it can't end the
/// LY=LYC match that blocks it. gambatte (behaviour only, `mstat_irq.h`
/// `statRegChange`, `lycRegChange`, `doM0Event`): the selects and LYC its
/// mode-0 event sees are latched only while `cc + 2 * cgb` (STAT) and
/// `cc + 5 * cgb + 1 - ds` (LYC) is before the event, `cc` in T-cycles
/// (half-dots in double speed), with `cc` our access and the event our
/// mode-0 interrupt point. CGB04C, single speed:
/// `m0enable/disable_{,scx3_,scx4_,scx7_}2` (STAT `08 -> 00` 1-2 dots
/// before: interrupt), `disable_scx{1,5}_1` (3 dots: none);
/// `lycdisable_ff45_{,scx1_,scx2_,scx3_}2` (LYC `-> FF` 2-6 dots before:
/// no interrupt), `lycdisable_ff45_scx1_1` (7 dots: interrupt). Double
/// speed: `lycdisable_ff45_scx1_ds_2` (LYC 1 dot before: none),
/// `lycdisable_ff45_scx1_ds_1` (3 dots: interrupt);
/// `lycdisable_ff41_scx1_ds_2` (STAT `48 -> 08` 1 dot before: none),
/// `lycdisable_ff41_scx1_ds_1` (3 dots: interrupt). DMG (the `dmg08`
/// results) latches STAT writes up to the event and LYC writes up to 1
/// dot before it.
pub(crate) const M0_STAT_LATCH: [u32; 2] = [2, 1];
pub(crate) const M0_LYC_LATCH: [u32; 2] = [6, 2];

/// VRAM blocked for the CPU from here to the mode-3 end: single speed two
/// dots after STAT reads mode 3, double speed with it (first line after
/// LCD enable: `_FIRST`). gambatte (behaviour only) `vramReadable` /
/// `vramWritable`: `lineCycles + ds < 76 + 3cgb`, one `cc` earlier in
/// double speed, 7 dots before our grid as its `getStat` mode-2 read
/// (`>= 453`) is to our `t = 4`. The CGB04C captures bracket the single-
/// speed edge at every CPU phase: `vram_m3/pre{read,write}_lcdoffset2_1`
/// access at 85 and `dma/hdma_late_enable_lcdoffset3_1` reads at 84 (both
/// allowed), `pre{read,write}_lcdoffset1_2` at 86 (blocked).
/// Double speed: the same `cc` threshold one dot earlier (gambatte's `+ ds`)
/// gives 85: `vram_m3/pre{read,write}_ds_2` access at 85 (blocked),
/// `_ds_lcdoffset1_1` at 84 (allowed), `_ds_lcdoffset1_2` at 86.
pub(crate) const VRAM_FROM_SS: u32 = t(h(86));
pub(crate) const VRAM_FROM_DS: u32 = t(h(85));
pub(crate) const VRAM_FROM_FIRST_SS: u32 = t(h(91));
pub(crate) const VRAM_FROM_FIRST_DS: u32 = t(h(86));

/// OAM writes / reads blocked from here (single / double speed), to the
/// mode-3 end; the first line after LCD enable from `_FIRST`. Double-speed
/// writes from 3: `oam_access/prewrite_ds_lcdoffset1_1` writes at 2
/// (allowed); `prewrite_ds_2` (default phase) stays blocked.
pub(crate) const OAM_WRITE_FROM_SS: u32 = t(h(0));
pub(crate) const OAM_WRITE_FROM_DS: u32 = t(h(3));
pub(crate) const OAM_WRITE_FROM_FIRST: u32 = t(h(84));
pub(crate) const OAM_READ_FROM_SS: u32 = t(h(3));
pub(crate) const OAM_READ_FROM_DS: u32 = t(h(4));
pub(crate) const OAM_READ_FROM_FIRST: u32 = t(h(86));

/// Palette RAM blocked from here (first line after LCD enable:
/// `_FIRST`) to this many dots past the mode-3 end (single / double
/// speed, where the end is already [`MODE0_DS_LATE`] later).
pub(crate) const PALETTE_FROM: u32 = t(h(87));
pub(crate) const PALETTE_FROM_FIRST: u32 = t(h(88));
pub(crate) const PALETTE_EXTRA_SS: u32 = 4;
pub(crate) const PALETTE_EXTRA_DS: u32 = 2;

/// Line dot from which the line's sticky WY == LY check has seen WY: line
/// 0, other lines (see the use in `wy_write`); double speed
/// [`WY_DS_LATE`] later.
pub(crate) const WY_CHECK_AT: [u32; 2] = [t(h(7)), t(h(3))];

/// Line dot from which a WY write no longer matches the current LY (see
/// `Ppu::wy_written`): gambatte (ppu.cpp, behaviour only) checks WY == LY
/// at line cycle 450 and WY == LY + 1 at 454; with the latter as
/// [`WY_CHECK_AT`]\[1\] (next line's 3), the former is 455. Double speed
/// [`WY_DS_LATE`] later: the whole line (`window/arg/late_wy_FFto0_ly2_ds_1`
/// writes at 455 and still matches).
pub(crate) const WY_LAST_AT: u32 = t(h(455));

/// Double speed: the WY checks ([`WY_CHECK_AT`], [`WY_LAST_AT`]) half a
/// dot later (gambatte's double-speed thresholds are odd `cc`, design doc
/// 1.4), so the first whole dot is one later. CGB04C:
/// `window/late_wy_ds_1` writes at 7 (before the line-0 check),
/// `_ds_lcdoffset1_2` at 8 (after); `window/arg/late_wy_1toFF_ds_1` at 3
/// (before), `_ds_lcdoffset1_2` at 4.
pub(crate) const WY_DS_LATE: u32 = t(H(1));

/// A LYC write is deferred to the PPU's next update before this dot
/// (lines 0-152, while LY changes): single speed at the write's view (one
/// dot after the access), double speed at the access. Double speed:
/// `lycEnable/ff45_enable_weirdpoint_ds_lcdoffset1_{2,3}` write at 0 and 2
/// (deferred: no interrupt); `_1`, `_4` and the default-phase
/// `ff45_enable_weirdpoint_ds_*` keep their results.
pub(crate) const LYC_DEFER_UNTIL_SS: u32 = 2;
pub(crate) const LYC_DEFER_UNTIL_DS: u32 = 3;

// --- VBlank (lines 144-153) ---

/// Lines 144-152: the comparison is off from the line start to
/// [`CMP_ON`].
pub(crate) const VCMP_OFF: u32 = 0;

/// Line 144: the mode-2 interrupt quirk, where the mode-2 interrupt of
/// lines 1-143 is ([`PT_LY`]): gambatte (behaviour only, `video.cpp`)
/// schedules line 144's at frame cycle `143 * 456 + 452`
/// (`mode2_irq_line_cycle`), the same line cycle as every other line's.
/// From here to [`VBLANK`] the mode-2 and mode-0 sources are both live for
/// the walk (`MODE_0_2`): a mode-1 interrupt is blocked by either select
/// (gambatte `mstat_irq.h` `doM1Event`), and no IRQ at [`VBLANK`] repeats
/// the quirk. CGB04C: `m1/m2m1irq_ifw_2` (an IF write here clears it and
/// nothing follows), `m1/ly143_late_m2enable_2` (a write enabling mode 2
/// here is too late for it).
pub(crate) const QUIRK2: u32 = t(h(3));

/// Single speed: STAT writes at line 144 from [`QUIRK2`] to [`VBLANK`],
/// and at line 0's [`PT_LY`], neither raise nor lower the mode part of the
/// STAT line (`MODE_HOLD`). gambatte (behaviour only, `video.cpp`
/// `statChangeTriggersM0LycOrM1StatIrqCgb`): with `timeToNextLy` T = 7 - t
/// on our grid, a mode-0 enable triggers at line 143 only while
/// `T > 4`, a mode-1 enable at line 153 only while `T > 4`, while an old
/// mode-1 select blocks until `T <= 3`; a write at `T <= 4` no longer
/// updates the selects the mode-1 interrupt is blocked by
/// (`mstat_irq.h` `statRegChange`: `cc + 2 < m1 time`). CGB04C:
/// `m1/ly143_late_m0enable_2`, `m1irq_m0disable_2`, `m1irq_late_enable_2`.
pub(crate) const MODE_HOLD_SS: u32 = t(h(3));

/// Line 144: mode 1, the VBlank interrupt and the frame.
pub(crate) const VBLANK: u32 = t(h(5));

/// Line 153: the comparison against 153 from here (the walk, the read and
/// the LYC write views), as lines 1-152 ([`CMP_WALK`]): gambatte (behaviour
/// only) schedules the LYC interrupt at `lyc*456 - 2` for LYC 1-153 alike
/// (`lyc_irq.cpp`). At 6 (SameBoy's state 14 length on CGB C, on its own
/// grid) the double-speed default phase dispatched one M-cycle late:
/// CGB04C `lyc153int_m2irq/lyc153int_m2irq_ifw_ds_1` (the line-0 mode-2
/// interrupt after its IF write), `ly0/lycint152_lyc153{irq,flag}_ds_2`,
/// `lcd_offset/offset2_lyc99int_*_1` (single speed, boundaries at
/// `t = 1 mod 4`). Design doc step 5.
pub(crate) const L153_CMP: u32 = t(h(5));
/// ... off from here (single speed; double speed keeps 153) ...
pub(crate) const L153_OFF: u32 = t(h(8));
/// ... against 0 from here.
pub(crate) const L153_ZERO: u32 = t(h(12));

/// Line 153, single speed: a LYC write (view) before this dot is compared
/// with 153 (see `Ppu::write_lyc`).
pub(crate) const LYC153_SS_UNTIL: u32 = 8;

/// Line 153: a LYC write in `LYC153_OLD_FROM..LYC153_OLD_UNTIL` (outside
/// the deferred `L153_CMP..L153_ZERO`) is first compared with 153
/// (SameBoy's states 14 and 29 "hacks").
pub(crate) const LYC153_OLD_FROM: u32 = 2;
pub(crate) const LYC153_OLD_UNTIL: u32 = 24;

// The HDMA trigger (`hdma::HBLANK_DELAY`) stays with the HDMA model: it is
// relative to the mode-0 start, and design step 10 replaces it.

// --- Design step 1: the walk's comparison point ---

/// Lines 1-152: the walk (interrupt dispatch and IF reads)
/// compares the new LY, and the mode-2 source of lines 1-143 ends, here:
/// one dot after the STAT bit ([`CMP_ON`]). gambatte (behaviour only) puts
/// the LYC interrupt 2 dots after the mode-2 interrupt (`lyc_irq.cpp`
/// `lyc*456 - 2` vs `mode2_irq_line_cycle = 456 - 4`), which with the
/// sync of `lcd_offset/offset3_lyc8fint_m1irq` (CGB04C) gives this dot.
/// The default CPU phase (M-cycle boundaries at `t = 3 mod 4`) cannot
/// tell 4 from 5; offset 3 (boundaries at `t = 0 mod 4`) can. Design doc
/// `docs/design/lcd-offset-phase.md` 2.1, step 1.
///
/// Double speed compares at the same dot (design step 3, which moved it from
/// 4): only `ds_lcdoffset1` (M-cycle boundaries at even `t`) can tell 4 from
/// 5 there. Its LYC=1 handler syncs `dma/hdma_late_{enable,m0halt}_ds_lcdoffset1_*`:
/// at 4 the FF55 enables and HALTs land 3 dots off the same tests at the
/// default phase; at 5 one HDMA period fits both phases. Also
/// `lcd_offset/offset1_*_ds_*` and the `lycEnable/*_ds_lcdoffset1_2` rows.
/// The write views stay per speed: [`VRAM_FROM_DS`], [`OAM_WRITE_FROM_DS`],
/// [`LYC_DEFER_UNTIL_DS`] and the STAT phase-1 mode-2 window.
pub(crate) const CMP_WALK: u32 = t(h(5));
