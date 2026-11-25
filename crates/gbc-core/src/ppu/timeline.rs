//! The line timeline: one table of every line's update points, in order
//! (review E3, `docs/reviews/stage1-m10/E-holistic.md`).
//!
//! A [`Row`] is one update point of a line [`Class`]: which point
//! ([`Pt`]), where on the line (per speed), what the walk's STAT update
//! there evaluates (mode source and comparison value), how late its
//! interrupt request reaches IF, and from which dot the STAT reads and the
//! STAT/LYC write views see its comparison ([`Row::seen`]). The positions
//! themselves are [`edges`]' (with their sources); this table is where
//! they are put in order and given their meaning.
//!
//! Everything else that knows the timeline takes it from here:
//! - the walk's sequence, [`Ppu::step`](super::Ppu::step) and
//!   [`Ppu::step_work`](super::Ppu::step_work): each arm's distance and
//!   successor is a `const` lookup ([`next`], [`next_at`], [`first`],
//!   [`frame_wrap`]), so the code is the same immediates as when they were
//!   written by hand;
//! - the walk's STAT update per point, `Ppu::point_irq`: each arm
//!   evaluates its row ([`row`]), specialized at compile time;
//! - the cursor search, `Ppu::seek_point`: a line's first row
//!   ([`Class::first`]), then `step` (it was a second, hand-written copy
//!   of the sequence);
//! - the comparison value as reads and writes see it, `Ppu::cmp`: its
//!   positions are the rows' [`Row::seen`];
//! - the STAT/LYC write views (`Ppu::mode_source`, `Ppu::update_at`,
//!   `Ppu::early_cmp`, `Ppu::write_lyc`) and the STAT mode read
//!   (`Ppu::mode`), wherever they mean a walk point or the dot a point is
//!   seen at: the named dots below ([`CMP_WALK`], [`CMP_SEEN`], ...).
//!
//! What stays written by hand in those functions is the *dispatch*: which
//! class a `(Pt, line)` pair belongs to (`Pt::V0 if l == 144`, ...), the
//! order of `Ppu::cmp`'s comparisons, and the shape of
//! `Ppu::mode_source`. The tests below check all three against a plain
//! interpretation of the table for every line, dot and speed, so a row
//! edit that the code doesn't follow fails there. For the write views
//! that means: their mode source is the walk's, except in the windows the
//! test lists (`view_only`), which are those views' own calibrations.
//!
//! Not in the table: edges that only an observer has. The LY read
//! (`Ppu::ly`), the mode-3 STAT read and the CPU access windows, and the
//! write views' own offsets stay in [`edges`]; "is this clock in mode 3
//! of the latched line" stays with `LineTiming` (`Ppu::mode3_end` and
//! its users).

use super::{LINE, LINES, MODE_0_2, MODE_HOLD, Pt, edges};
use crate::speed::Speed;

/// The kinds of line: lines of one class have the same rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Class {
    /// Line 0 of the frame the LCD was enabled in: no OAM scan.
    First,
    /// Line 0 after a line 153.
    Line0,
    /// Lines 1-143.
    Visible,
    /// Line 144.
    L144,
    /// Lines 145-152.
    VBlank,
    /// Line 153.
    L153,
}

/// Where a row is on its line.
#[derive(Clone, Copy)]
pub(super) enum Pos {
    /// At this dot: single, double speed.
    At([u32; 2]),
    /// At the line's mode-3 end (`LineTiming::end`, fixed at `Latch`).
    End,
    /// This many dots after the mode-3 end.
    AfterEnd(u32),
}

/// The mode source of a row's STAT update (`IrqLine::eval`'s `mode`).
#[derive(Clone, Copy)]
pub(super) enum Src {
    /// No STAT update: the point does other work (`Latch`, `End`).
    Idle,
    /// This mode source.
    Mode(u8),
    /// This mode source for the one evaluation, then none.
    Pulse(u8),
    /// Line 0's `Ly3`: the mode-1 source; in double speed it ends here
    /// unless mode 2 is selected ([`edges::ly0_pt`]).
    Line0Mode1,
}

/// The comparison value of a row's STAT update (`ly_for_comparison`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Cmp {
    /// The comparison is off: the LY=LYC part of the line holds (and the
    /// STAT bit reads 0 in single speed, `Ppu::lyc_bit`).
    Off,
    /// Against the line's number.
    Ly,
    /// Against this value: where it isn't the line's number (0 on lines
    /// 153 and 0), and where the walk's code has the line's number as an
    /// immediate (144, 153).
    Val(u8),
}

/// One update point of a line class.
#[derive(Clone, Copy)]
pub(super) struct Row {
    pub(super) pt: Pt,
    pub(super) pos: Pos,
    pub(super) src: Src,
    /// Single, double speed.
    pub(super) cmp: [Cmp; 2],
    /// Single speed: the row's interrupt request reaches IF this many dots
    /// after the row ([`edges::LY0_PULSE_IRQ`], [`edges::L153_ZERO_IRQ`]).
    pub(super) irq_late: u64,
    /// The dot from which the STAT reads and the STAT/LYC write views see
    /// the row's comparison value (`Ppu::cmp`; the walk: at the row).
    /// `None`: the row doesn't change the value.
    pub(super) seen: Option<u32>,
}

const fn row_at(pt: Pt, t: [u32; 2], src: Src, cmp: Cmp) -> Row {
    Row {
        pt,
        pos: Pos::At(t),
        src,
        cmp: [cmp, cmp],
        irq_late: 0,
        seen: None,
    }
}

impl Row {
    /// The comparison value in double speed, where it differs.
    const fn double(mut self, cmp: Cmp) -> Self {
        self.cmp[1] = cmp;
        self
    }

    const fn irq_late(mut self, dots: u64) -> Self {
        self.irq_late = dots;
        self
    }

    const fn seen(mut self, t: u32) -> Self {
        self.seen = Some(t);
        self
    }
}

/// Both speeds at dot `t`.
const fn both(t: u32) -> [u32; 2] {
    [t, t]
}

// --- The table ---

/// A visible line from its pipeline start: `Latch` at `pipe`, the mode-3
/// end, the HBlank interrupt source.
const fn mode3_rows(pipe: u32) -> [Row; 3] {
    [
        row_at(Pt::Latch, both(pipe), Src::Idle, Cmp::Off),
        Row {
            pos: Pos::End,
            ..row_at(Pt::End, both(0), Src::Idle, Cmp::Off)
        },
        Row {
            pos: Pos::AfterEnd(edges::HBLANK_AFTER_END),
            ..row_at(Pt::HBlank, both(0), Src::Mode(0), Cmp::Ly)
        },
    ]
}

const ROWS_MODE3: [Row; 3] = mode3_rows(edges::PIPE);

const ROWS_FIRST: [Row; 3] = mode3_rows(edges::PIPE_FIRST);

const ROWS_LINE0: [Row; 5] = [
    // The comparison is against 0 since line 153 (`L153Zero`).
    row_at(
        Pt::Ly3,
        [edges::ly0_pt(Speed::Normal), edges::ly0_pt(Speed::Double)],
        Src::Line0Mode1,
        Cmp::Val(0),
    ),
    row_at(
        Pt::Ly4,
        [
            edges::ly0_pulse(Speed::Normal),
            edges::ly0_pulse(Speed::Double),
        ],
        Src::Pulse(2),
        Cmp::Ly,
    )
    .irq_late(edges::LY0_PULSE_IRQ)
    .seen(edges::CMP_ON),
    ROWS_MODE3[0],
    ROWS_MODE3[1],
    ROWS_MODE3[2],
];

const ROWS_VISIBLE: [Row; 5] = [
    row_at(Pt::Ly3, both(edges::PT_LY), Src::Mode(2), Cmp::Off).seen(edges::CMP_OFF),
    // The mode-2 source ends. It was up since `Ly3`, so the pulse can't
    // raise the line here (line 0's can).
    row_at(Pt::Ly4, both(edges::CMP_WALK), Src::Pulse(2), Cmp::Ly).seen(edges::CMP_ON),
    ROWS_MODE3[0],
    ROWS_MODE3[1],
    ROWS_MODE3[2],
];

const ROWS_L144: [Row; 4] = [
    row_at(Pt::V0, both(edges::VCMP_OFF), Src::Mode(0), Cmp::Off).seen(edges::CMP_OFF),
    row_at(
        Pt::Quirk2,
        both(edges::QUIRK2),
        Src::Mode(MODE_0_2),
        Cmp::Off,
    ),
    // The mode part holds from `Quirk2` (the selects as of there block
    // the mode-1 interrupt, `edges::QUIRK2`).
    row_at(
        Pt::V4,
        both(edges::CMP_WALK),
        Src::Mode(MODE_HOLD),
        Cmp::Val(144),
    )
    .seen(edges::CMP_ON),
    // Same dot as `V4`, after it: mode 1, the VBlank interrupt, the frame.
    row_at(Pt::VBlank, both(edges::VBLANK), Src::Mode(1), Cmp::Val(144)),
];

const ROWS_VBLANK: [Row; 2] = [
    row_at(Pt::V0, both(edges::VCMP_OFF), Src::Mode(1), Cmp::Off).seen(edges::CMP_OFF),
    row_at(Pt::V4, both(edges::CMP_WALK), Src::Mode(1), Cmp::Ly).seen(edges::CMP_ON),
];

const ROWS_L153: [Row; 4] = [
    // Reads see the comparison off from the line start here, not from
    // `edges::CMP_OFF` as on lines 144-152 (`edges::VCMP_OFF`).
    row_at(Pt::V0, both(edges::VCMP_OFF), Src::Mode(1), Cmp::Off).seen(edges::VCMP_OFF),
    row_at(
        Pt::L153Cmp,
        both(edges::L153_CMP),
        Src::Mode(1),
        Cmp::Val(153),
    )
    .seen(edges::L153_CMP),
    // Off in single speed; double speed keeps 153.
    row_at(Pt::L153Off, both(edges::L153_OFF), Src::Mode(1), Cmp::Off)
        .double(Cmp::Val(153))
        .seen(edges::L153_OFF),
    // The walk is a dot later in double speed; the reads and write views
    // are not ([`edges::l153_zero_walk`]).
    row_at(
        Pt::L153Zero,
        [
            edges::l153_zero_walk(Speed::Normal),
            edges::l153_zero_walk(Speed::Double),
        ],
        Src::Mode(1),
        Cmp::Val(0),
    )
    .irq_late(edges::L153_ZERO_IRQ)
    .seen(edges::L153_ZERO),
];

impl Class {
    /// The class of line `l`; `first`: its frame began with the LCD being
    /// enabled.
    pub(super) const fn of(l: u32, first: bool) -> Self {
        match l {
            0 if first => Self::First,
            0 => Self::Line0,
            144 => Self::L144,
            153 => Self::L153,
            _ if l < LINES => Self::Visible,
            _ => Self::VBlank,
        }
    }

    /// The class's rows, in walk order.
    pub(super) const fn rows(self) -> &'static [Row] {
        match self {
            Self::First => &ROWS_FIRST,
            Self::Line0 => &ROWS_LINE0,
            Self::Visible => &ROWS_VISIBLE,
            Self::L144 => &ROWS_L144,
            Self::VBlank => &ROWS_VBLANK,
            Self::L153 => &ROWS_L153,
        }
    }

    /// The line's first update point: its dot (single, double speed) and
    /// kind. (Looked up per class at compile time: the cursor search calls
    /// this with a class it computed, and gets no panic path.)
    pub(super) const fn first(self) -> ([u32; 2], Pt) {
        const fn of(class: Class) -> ([u32; 2], Pt) {
            let r = &class.rows()[0];
            match r.pos {
                Pos::At(t) => (t, r.pt),
                _ => panic!("a line starts with a row at a fixed dot"),
            }
        }
        match self {
            Self::First => const { of(Class::First) },
            Self::Line0 => const { of(Class::Line0) },
            Self::Visible => const { of(Class::Visible) },
            Self::L144 => const { of(Class::L144) },
            Self::VBlank => const { of(Class::VBlank) },
            Self::L153 => const { of(Class::L153) },
        }
    }
}

// --- Lookups, for `const` contexts (a missing row fails the build) ---

const fn index(class: Class, pt: Pt) -> usize {
    let rows = class.rows();
    let mut i = 0;
    while i < rows.len() {
        if rows[i].pt as u8 == pt as u8 {
            return i;
        }
        i += 1;
    }
    panic!("the class has no such row")
}

/// The row of `pt` on a line of `class`.
pub(super) const fn row(class: Class, pt: Pt) -> Row {
    class.rows()[index(class, pt)]
}

/// The dot of `pt` on a line of `class`, the same in both speeds.
pub(super) const fn at(class: Class, pt: Pt) -> u32 {
    match row(class, pt).pos {
        Pos::At(t) if t[0] == t[1] => t[0],
        _ => panic!("the row is not at one fixed dot"),
    }
}

/// From `pt` to the next row of its line: the dots between them (single,
/// double speed) and that row's point.
pub(super) const fn next_by_speed(class: Class, pt: Pt) -> ([u32; 2], Pt) {
    let rows = class.rows();
    let i = index(class, pt);
    assert!(i + 1 < rows.len(), "the line's last row");
    let n = &rows[i + 1];
    let gap = match (rows[i].pos, n.pos) {
        (Pos::At(a), Pos::At(b)) => [b[0] - a[0], b[1] - a[1]],
        (Pos::End, Pos::AfterEnd(d)) => [d, d],
        _ => panic!("the distance is not fixed (the mode-3 end)"),
    };
    (gap, n.pt)
}

/// [`next_by_speed`] where the distance is the same in both speeds.
pub(super) const fn next(class: Class, pt: Pt) -> (u32, Pt) {
    let (gap, pt) = next_by_speed(class, pt);
    assert!(gap[0] == gap[1], "the distance depends on the speed");
    (gap[0], pt)
}

/// The row after `pt` on its line: its dot (the same in both speeds) and
/// point.
pub(super) const fn next_at(class: Class, pt: Pt) -> (u32, Pt) {
    let n = &class.rows()[index(class, pt) + 1];
    (at(class, n.pt), n.pt)
}

/// [`Class::first`] where the dot is the same in both speeds.
pub(super) const fn first(class: Class) -> (u32, Pt) {
    let (t, pt) = class.first();
    assert!(t[0] == t[1], "the dot depends on the speed");
    (t[0], pt)
}

/// From line 153's last row to the next frame's first: the dots between
/// them (the same in both speeds: both rows are a dot later in double
/// speed) and that row's point.
pub(super) const fn frame_wrap() -> (u32, Pt) {
    let rows = Class::L153.rows();
    let Pos::At(last) = rows[rows.len() - 1].pos else {
        panic!("line 153 ends with a row at a fixed dot")
    };
    let (first, pt) = Class::Line0.first();
    let gap = [LINE + first[0] - last[0], LINE + first[1] - last[1]];
    assert!(gap[0] == gap[1], "the distance depends on the speed");
    (gap[0], pt)
}

/// The dot from which reads and write views see the comparison value of
/// `pt`'s row ([`Row::seen`]).
pub(super) const fn seen(class: Class, pt: Pt) -> u32 {
    match row(class, pt).seen {
        Some(t) => t,
        None => panic!("the row doesn't change the comparison value"),
    }
}

/// `speed.pick(v)` as arithmetic on the speed (no table load in the walk).
#[inline(always)]
pub(super) const fn by_speed(v: [u32; 2], speed: Speed) -> u32 {
    v[0] + (v[1] - v[0]) * speed.extra()
}

/// The dot of `pt` on a line of `class` at `speed`.
const fn at_speed(class: Class, pt: Pt, speed: Speed) -> u32 {
    match row(class, pt).pos {
        Pos::At(t) => speed.pick(t),
        _ => panic!("the row is not at a fixed dot"),
    }
}

// --- The rows' dots by name, for the reads and the write views ---
//
// A read or a write view that means "where the walk's point is" or "where
// that point's comparison is seen" takes the dot from here, so it moves
// with the row. Edges that only an observer has (the mode-3 STAT read,
// the access windows, a view's own offsets) stay in `edges`.

/// Lines 0-143: the pixel pipeline starts (`Latch`); the first line after
/// LCD enable: [`PIPE_FIRST`].
pub(super) const PIPE: u32 = at(Class::Visible, Pt::Latch);
pub(super) const PIPE_FIRST: u32 = at(Class::First, Pt::Latch);

/// The HBlank interrupt source rises this many dots after the mode-3 end.
pub(super) const HBLANK_AFTER_END: u32 = next(Class::Visible, Pt::End).0;

/// Lines 1-143: the walk's `Ly3` (LY changes, the mode-2 source rises).
pub(super) const LY: u32 = at(Class::Visible, Pt::Ly3);

/// Lines 1-152: the walk compares the new LY (`Ly4`, `V4`; line 153:
/// `L153Cmp`), and on lines 1-143 its mode-2 source ends.
pub(super) const CMP_WALK: u32 = at(Class::Visible, Pt::Ly4);

/// Lines 1-152: reads and write views see the comparison off from here ...
pub(super) const CMP_OFF_SEEN: u32 = seen(Class::Visible, Pt::Ly3);
/// ... and against the new LY from here, [`CMP_WALK`]` - CMP_SEEN` dots
/// before the walk (`Ppu::early_cmp`). Lines 0-143: STAT reads mode 2 and
/// the write views' mode-2 source ends here too ([`edges::MODE2`],
/// [`edges::MODE2_SOURCE_END`]).
pub(super) const CMP_SEEN: u32 = seen(Class::Visible, Pt::Ly4);

/// Line 0, single speed: the mode-2 pulse (`Ly4`).
pub(super) const LY0_PULSE_SS: u32 = at_speed(Class::Line0, Pt::Ly4, Speed::Normal);

/// Line 144: the mode-2 interrupt quirk (`Quirk2`).
pub(super) const QUIRK2: u32 = at(Class::L144, Pt::Quirk2);
/// Line 144: mode 1, the VBlank interrupt, the frame (`VBlank`).
pub(super) const VBLANK: u32 = at(Class::L144, Pt::VBlank);

/// Line 153: reads and write views see the comparison against 153 from
/// here, off from here (single speed) and against 0 from here.
pub(super) const L153_CMP_SEEN: u32 = seen(Class::L153, Pt::L153Cmp);
pub(super) const L153_OFF_SEEN: u32 = seen(Class::L153, Pt::L153Off);
pub(super) const L153_ZERO_SEEN: u32 = seen(Class::L153, Pt::L153Zero);
/// Line 153, single speed: the walk compares against 0 (`L153Zero`).
pub(super) const L153_ZERO_SS: u32 = at_speed(Class::L153, Pt::L153Zero, Speed::Normal);

// What the code assumes of the table beyond the lookups it makes.
const _: () = {
    // `Ppu::mode_source`'s sticky window before the walk's comparison:
    // one dot for lines 145-152 (`V4`) and 153 (`L153Cmp`).
    assert!(at(Class::VBlank, Pt::V4) == CMP_WALK);
    assert!(at(Class::L153, Pt::L153Cmp) == CMP_WALK);
    assert!(at(Class::L144, Pt::V4) == CMP_WALK);
    // Line 0 reads mode 2 where lines 1-143 do (`Ppu::mode`), and the
    // observers' own names for that dot are this row's.
    assert!(seen(Class::Line0, Pt::Ly4) == CMP_SEEN);
    assert!(edges::MODE2 == CMP_SEEN && edges::MODE2_SOURCE_END == CMP_SEEN);
    // The single-speed STAT-write hold starts on a walk point on both
    // lines it applies to (`edges::MODE_HOLD_SS`).
    assert!(edges::MODE_HOLD_SS == at_speed(Class::Line0, Pt::Ly3, Speed::Normal));
    assert!(edges::MODE_HOLD_SS == QUIRK2);
    // `Ly4` to `Latch`, and `step_work`'s `Latch`: lines 0 and 1-143 alike.
    assert!(next_at(Class::Line0, Pt::Ly4).0 == next_at(Class::Visible, Pt::Ly4).0);
    assert!(at(Class::Line0, Pt::Latch) == at(Class::Visible, Pt::Latch));
    // The line after a VBlank line starts the same whether it is 153.
    assert!(first(Class::VBlank).0 == first(Class::L153).0);
    assert!(first(Class::VBlank).1 as u8 == first(Class::L153).1 as u8);
    // `Ppu::cmp`: lines 144-152 see the comparison go off and on where
    // lines 1-143 do, and line 153 sees it off from its start.
    assert!(seen(Class::L144, Pt::V0) == seen(Class::Visible, Pt::Ly3));
    assert!(seen(Class::VBlank, Pt::V0) == seen(Class::Visible, Pt::Ly3));
    assert!(seen(Class::L144, Pt::V4) == seen(Class::Visible, Pt::Ly4));
    assert!(seen(Class::VBlank, Pt::V4) == seen(Class::Visible, Pt::Ly4));
    assert!(seen(Class::L153, Pt::V0) == 0);
};

#[cfg(test)]
mod tests {
    extern crate std;
    use super::super::{At, Cursor, FRAME, IrqLine, MODE_1_STICKY, NO_MODE, Ppu, View};
    use super::*;
    use std::vec::Vec;

    /// The earliest mode-3 end, where an unlatched line's `End` is.
    fn unlatched_end(class: Class) -> u32 {
        at(class, Pt::Latch) + edges::MIN_MODE3
    }

    /// One frame's update points from the table alone, then the next
    /// frame's first: `(frame position, line, point)`. `end`: the mode-3
    /// end of every line (`None`: not latched).
    fn frame_points(first: bool, speed: Speed, end: Option<u32>) -> Vec<(u32, u32, Pt)> {
        let mut out = Vec::new();
        for l in 0..154 {
            let class = Class::of(l, first);
            let end = || end.unwrap_or_else(|| unlatched_end(class));
            for r in class.rows() {
                let t = match r.pos {
                    Pos::At(t) => speed.pick(t),
                    Pos::End => end(),
                    Pos::AfterEnd(d) => end() + d,
                };
                out.push((l * LINE + t, l, r.pt));
            }
        }
        let (t, pt) = Class::Line0.first();
        out.push((FRAME + speed.pick(t), 0, pt));
        out
    }

    /// A PPU whose every line has its mode-3 end latched at `end`
    /// (`None`: no line latched), in a frame that starts at clock 0.
    struct Latched {
        ppu: Ppu,
        end: Option<u32>,
    }

    impl Latched {
        fn new(first: bool, end: Option<u32>, select: u8) -> Self {
            let mut ppu = Ppu::new();
            ppu.lcdc = 0x91;
            ppu.frame_start = 0;
            ppu.first_frame = first;
            ppu.stat_select = select;
            Self { ppu, end }
        }

        /// Latches line `l`, as its `Latch` point does.
        fn on_line(&mut self, l: u32) -> &Ppu {
            if let Some(e) = self.end {
                self.ppu.line.start = u64::from(l * LINE);
                self.ppu.line.end = e as u16;
            }
            &self.ppu
        }
    }

    const ENDS: [Option<u32>; 3] = [None, Some(260), Some(389)];

    #[test]
    fn step_follows_the_table() {
        for (first, speed, end) in cases() {
            let want = frame_points(first, speed, end);
            let mut p = Latched::new(first, end, 0x08);
            let (pos, line, pt) = want[0];
            let mut c = Cursor {
                pos,
                line: line as u8,
                pt,
            };
            for &(pos, line, pt) in &want[1..] {
                c = p.on_line(u32::from(c.line)).step(c, 0, speed);
                let w = Cursor {
                    pos,
                    line: line as u8,
                    pt,
                };
                assert_eq!(c, w, "first {first}, {speed:?}, end {end:?}");
            }
        }
    }

    #[test]
    fn step_work_follows_the_table_s_work_rows() {
        let work = |pt| matches!(pt, Pt::Latch | Pt::End | Pt::VBlank);
        for (first, speed, end) in cases() {
            let mut want = frame_points(first, speed, end);
            // The next frame's first work point.
            want.push((FRAME + at(Class::Line0, Pt::Latch), 0, Pt::Latch));
            want.retain(|&(_, _, pt)| work(pt));
            let mut p = Latched::new(first, end, 0);
            let (pos, line, pt) = want[0];
            let mut c = Cursor {
                pos,
                line: line as u8,
                pt,
            };
            for &(pos, line, pt) in &want[1..] {
                c = p.on_line(u32::from(c.line)).step_work(c, 0);
                let w = Cursor {
                    pos,
                    line: line as u8,
                    pt,
                };
                assert_eq!(c, w, "first {first}, {speed:?}, end {end:?}");
            }
        }
    }

    fn cases() -> Vec<(bool, Speed, Option<u32>)> {
        let mut v = Vec::new();
        for first in [false, true] {
            for speed in [Speed::Normal, Speed::Double] {
                for end in ENDS {
                    v.push((first, speed, end));
                }
            }
        }
        v
    }

    /// The cursor search finds, from every frame position, the point the
    /// table has next; with no source selected, the next work point.
    #[test]
    fn seek_follows_the_table() {
        for (first, speed, end) in cases() {
            for select in [0x08, 0] {
                let mut want = frame_points(first, speed, end);
                if select == 0 {
                    want.push((FRAME + at(Class::Line0, Pt::Latch), 0, Pt::Latch));
                    want.retain(|&(_, _, pt)| matches!(pt, Pt::Latch | Pt::End | Pt::VBlank));
                }
                let mut p = Latched::new(first, end, select);
                let mut next = 0;
                for after in 0..FRAME {
                    while want[next].0 <= after {
                        next += 1;
                    }
                    let (pos, line, pt) = want[next];
                    let l = after / LINE;
                    // A line is latched from its `Latch` on (LCD off and
                    // on drop it: the search then starts on the first
                    // line, before its `Latch`).
                    let class = Class::of(l, first);
                    let latched = l < LINES && after % LINE >= at(class, Pt::Latch);
                    if end.is_none() && latched {
                        continue;
                    }
                    p.on_line(l);
                    if !latched {
                        p.ppu.line.start = crate::schedule::NEVER;
                    }
                    p.ppu.synced = u64::from(after);
                    let w = Cursor {
                        pos,
                        line: line as u8,
                        pt,
                    };
                    assert_eq!(
                        p.ppu.seek_point(speed),
                        w,
                        "after {after}, first {first}, {speed:?}, end {end:?}, select {select:#x}"
                    );
                }
            }
        }
    }

    /// `Ppu::point_irq`'s arms pick the row of the point's line class.
    #[test]
    fn point_irq_evaluates_the_point_s_row() {
        let mut ppu = Ppu::new();
        for l in 0..154u32 {
            for first in [false, true] {
                let class = Class::of(l, first);
                for r in class.rows() {
                    for speed in [Speed::Normal, Speed::Double] {
                        for select in (0..16).map(|s| s << 3) {
                            for lyc in [0, l as u8, (l as u8).wrapping_sub(1), 144, 153, 77] {
                                for bits in 0..4 {
                                    for held in [false, true] {
                                        ppu.stat_select = select;
                                        ppu.lyc = lyc;
                                        let state = IrqLine { bits, lyc: held };
                                        let (mut a, mut b) = (state, state);
                                        let ra = ppu.point_irq(&mut a, r.pt, l, speed);
                                        let rb = ppu.row_irq(&mut b, *r, l as u8, speed);
                                        assert_eq!(
                                            (ra, a.bits, a.lyc),
                                            (rb, b.bits, b.lyc),
                                            "{:?} line {l} first {first} {speed:?} \
                                             select {select:#x} lyc {lyc} from {bits}/{held}",
                                            r.pt
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// A row's `Cmp::Val` is the line's number wherever the line has one
    /// class of its own (so `Val` there only spells `Ly` as an immediate).
    #[test]
    fn immediate_comparison_values_are_the_line_s() {
        for (class, l) in [(Class::L144, 144), (Class::L153, 153)] {
            for r in class.rows() {
                for c in r.cmp {
                    assert!(
                        matches!(c, Cmp::Off | Cmp::Ly) || c == Cmp::Val(l) || c == Cmp::Val(0)
                    );
                }
            }
        }
    }

    /// The comparison value at dot `t` of line `l` as the table's `seen`
    /// column gives it: `Ppu::cmp`'s result.
    fn cmp_seen(l: u32, t: u32, first: bool, speed: Speed) -> (Option<u8>, u8) {
        // Carried over the line start: the previous line's number (line
        // 0: the 0 of line 153's last row).
        let mut v = l.saturating_sub(1) as u8;
        let mut on = true;
        for r in Class::of(l, first).rows() {
            if r.seen.is_some_and(|s| t >= s) {
                match speed.pick(r.cmp) {
                    Cmp::Off => on = false,
                    Cmp::Ly => (v, on) = (l as u8, true),
                    Cmp::Val(x) => (v, on) = (x, true),
                }
            }
        }
        if on { (Some(v), 0) } else { (None, v) }
    }

    #[test]
    fn cmp_follows_the_table_s_seen_column() {
        for l in 0..154 {
            for t in 0..LINE {
                for first in [false, true] {
                    for speed in [Speed::Normal, Speed::Double] {
                        let a = At { line: l, t, first };
                        assert_eq!(
                            Ppu::cmp(a, speed),
                            cmp_seen(l, t, first, speed),
                            "line {l} dot {t} first {first} {speed:?}"
                        );
                    }
                }
            }
        }
    }

    /// The walk's mode source at dot `t` of line `l`: the source of the
    /// last row at or before `t` that has one (over the line start: the
    /// previous line's last; the first line starts with none, as the LCD
    /// enable leaves it). `end`: the line's mode-3 end; `m2`: mode 2 is
    /// selected (line 0's `Ly3` in double speed).
    fn walk_src(l: u32, t: u32, first: bool, speed: Speed, end: u32, m2: bool) -> u8 {
        let class = Class::of(l, first);
        let mut src = match class {
            Class::First => NO_MODE,
            Class::Visible | Class::L144 => 0,
            Class::Line0 | Class::VBlank | Class::L153 => 1,
        };
        for r in class.rows() {
            let at = match r.pos {
                Pos::At(t) => speed.pick(t),
                Pos::End => end,
                Pos::AfterEnd(d) => end + d,
            };
            if at <= t {
                src = match r.src {
                    Src::Idle => src,
                    Src::Mode(m) => m,
                    Src::Pulse(_) => NO_MODE,
                    Src::Line0Mode1 if speed.is_double() && !m2 => NO_MODE,
                    Src::Line0Mode1 => 1,
                };
            }
        }
        src
    }

    /// Where a STAT or LYC write's view of the mode source
    /// (`Ppu::mode_source`) is not the walk's ([`walk_src`]): the view's
    /// value there. This is the whole list; each entry is a calibration of
    /// that view against test ROMs (the sources are at the code named).
    fn view_only(l: u32, t: u32, first: bool, speed: Speed, view: View, end: u32) -> Option<u8> {
        let class = Class::of(l, first);
        let double = speed.is_double();
        // A single-speed STAT write (either phase).
        let stat_ss = !double && view != View::LycWrite;
        let before = |e: u32| (e - 2..e).contains(&t);
        match class {
            // Every visible line, single speed: the views see the mode-0
            // source from the mode-3 end, a dot before the walk's `HBlank`
            // (double speed: from `HBlank`; `Ppu::mode3_end`).
            _ if l < LINES && !double && t == end => Some(0),
            // Lines 1-143. A STAT write's second phase and a LYC write see
            // the mode-2 source end where they see the comparison, before
            // the walk's `Ly4` (`edges::MODE2_SOURCE_END`; `Ppu::update_at`
            // keeps the line up for the walk).
            Class::Visible if view != View::StatPhase1 && (CMP_SEEN..CMP_WALK).contains(&t) => {
                Some(NO_MODE)
            }
            // Double speed: a first phase sees the mode-2 source a dot
            // past `Ly4`, a second phase sees it start a dot after `Ly3`
            // (`Ppu::mode_source`).
            Class::Visible if double && view == View::StatPhase1 && t == CMP_WALK => Some(2),
            Class::Visible if double && view == View::StatPhase2 && t == LY => Some(0),
            // Line 0, single speed: a STAT write on `Ly3` holds the mode
            // part (`edges::MODE_HOLD_SS`).
            Class::Line0 if stat_ss && t == edges::MODE_HOLD_SS => Some(MODE_HOLD),
            // Line 144 from `Quirk2` to `VBlank`: single-speed STAT writes
            // hold the mode part; a double-speed first phase from the dot
            // after `Quirk2` (`edges::MODE_HOLD_SS`, `Ppu::mode_source`).
            Class::L144 if stat_ss && (QUIRK2..VBLANK).contains(&t) => Some(MODE_HOLD),
            Class::L144
                if double && view == View::StatPhase1 && (QUIRK2 + 1..VBLANK).contains(&t) =>
            {
                Some(MODE_HOLD)
            }
            // Lines 145-153, single-speed STAT writes: the two dots before
            // the walk compares LY=LYC, mode 1 can't be cleared
            // (`Ppu::mode_source`); line 153 also before `L153Zero`.
            Class::VBlank | Class::L153 if stat_ss && before(CMP_WALK) => Some(MODE_1_STICKY),
            Class::L153 if stat_ss && before(L153_ZERO_SS) => Some(MODE_1_STICKY),
            _ => None,
        }
    }

    /// The write views' mode source is the walk's, row for row, except in
    /// the windows [`view_only`] lists; with and without mode 2 selected
    /// (line 0 in double speed: the mode-1 source to `Ly3` or to the
    /// pulse, `Src::Line0Mode1`).
    #[test]
    fn write_views_see_the_walk_s_mode_source() {
        for ((first, speed, end), select) in
            cases().into_iter().flat_map(|c| [(c, 0x08), (c, 0x28)])
        {
            let Some(end) = end else { continue };
            let m2 = select & 0x20 != 0;
            let mut p = Latched::new(first, Some(end), select);
            for l in 0..154 {
                let class = Class::of(l, first);
                for t in 0..LINE {
                    // Latched from the line's `Latch` on; before it, the
                    // end is unknown and past `t`.
                    p.on_line(l);
                    if l >= LINES || t < at(class, Pt::Latch) {
                        p.ppu.line.start = crate::schedule::NEVER;
                    }
                    let a = At {
                        line: l,
                        t,
                        first: first && l == 0,
                    };
                    let now = u64::from(l * LINE + t);
                    for view in [View::StatPhase1, View::StatPhase2, View::LycWrite] {
                        let want = view_only(l, t, first, speed, view, end)
                            .unwrap_or_else(|| walk_src(l, t, first, speed, end, m2));
                        assert_eq!(
                            p.ppu.mode_source(now, a, speed, view),
                            want,
                            "line {l} dot {t} first {first} {speed:?} {view:?} end {end} m2 {m2}"
                        );
                    }
                }
            }
        }
    }

    /// `Ppu::irq_late`'s arms pick the rows that have a delay.
    #[test]
    fn irq_delay_is_the_point_s_row_s() {
        for l in 0..154 {
            for first in [false, true] {
                for r in Class::of(l, first).rows() {
                    assert_eq!(
                        Ppu::irq_late(r.pt, l, Speed::Normal),
                        r.irq_late,
                        "{l} {:?}",
                        r.pt
                    );
                    assert_eq!(Ppu::irq_late(r.pt, l, Speed::Double), 0, "{l} {:?}", r.pt);
                }
            }
        }
    }
}
