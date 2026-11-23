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

/// VRAM blocked from here (first line after LCD enable: single / double
/// speed) to the mode-3 end.
pub(crate) const VRAM_FROM: u32 = MODE3;
pub(crate) const VRAM_FROM_FIRST_SS: u32 = t(h(91));
pub(crate) const VRAM_FROM_FIRST_DS: u32 = t(h(86));

/// OAM writes / reads blocked from here (single / double speed), to the
/// mode-3 end; the first line after LCD enable from `_FIRST`.
pub(crate) const OAM_WRITE_FROM_SS: u32 = t(h(0));
pub(crate) const OAM_WRITE_FROM_DS: u32 = t(h(2));
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
/// 0, other lines (see the use in `wy_write`).
pub(crate) const WY_CHECK_AT: [u32; 2] = [t(h(7)), t(h(3))];

/// A LYC write is deferred to the PPU's next update before this dot
/// (lines 0-152, while LY changes).
pub(crate) const LYC_DEFER_UNTIL: u32 = 2;

// --- VBlank (lines 144-153) ---

/// Lines 144-152: the comparison is off from the line start to
/// [`CMP_ON`].
pub(crate) const VCMP_OFF: u32 = 0;

/// Line 144: the mode-2 interrupt quirk.
pub(crate) const QUIRK2: u32 = t(h(2));

/// Line 144: mode 1, the VBlank interrupt and the frame.
pub(crate) const VBLANK: u32 = t(h(5));

/// Line 153: the comparison against 153 from here ...
pub(crate) const L153_CMP: u32 = t(h(6));
/// ... off from here (single speed; double speed keeps 153) ...
pub(crate) const L153_OFF: u32 = t(h(8));
/// ... against 0 from here.
pub(crate) const L153_ZERO: u32 = t(h(12));

/// Line 153: a LYC write in `LYC153_OLD_FROM..LYC153_OLD_UNTIL` (outside
/// the deferred `L153_CMP..L153_ZERO`) is first compared with 153
/// (SameBoy's states 14 and 29 "hacks").
pub(crate) const LYC153_OLD_FROM: u32 = 2;
pub(crate) const LYC153_OLD_UNTIL: u32 = 24;

// The HDMA trigger (`hdma::HBLANK_DELAY`) stays with the HDMA model: it is
// relative to the mode-0 start, and design step 10 replaces it.
