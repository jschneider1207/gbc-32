//! Test-only oracle for the length of mode 3: one scanline's pixel
//! pipeline stepped dot by dot, and the differential test that compares
//! the core's closed form (`LineTiming`, `set_stalls`, `hold_fetch`'s
//! fine-scroll discard) with it. M10 review finding E1
//! (`docs/reviews/stage1-m10/E-holistic.md`); `docs/reference/mode3-oracle.md`
//! has the coverage and the exclusions.
//!
//! The model is the mode-3 loop of SameBoy's `Core/display.c`
//! (`GB_display_run` from `mode_3_start`, `render_pixel_if_possible`,
//! `advance_fetcher_state_machine`, `GB_update_wx_glitch`), for a CGB
//! (`GB_is_cgb`, revisions up to C), reduced to what decides *when* things
//! happen: no pixel data, palettes or VRAM. It is not derived from the
//! core's stall lists. What it steps:
//! - the background fetcher's seven states (tile, low plane, high plane:
//!   2 dots each, then a push that waits for an empty FIFO; Pan Docs:
//!   Pixel FIFO, "FIFO pixel fetcher"), giving the 6-dot fetch and the
//!   8-dot push cadence;
//! - the BG FIFO's fill count, starting with 8 junk pixels;
//! - `position_in_line` from -16: the junk tile, the `SCX & 7` discard
//!   compare at -16..-9 (Pan Docs: Rendering, "Mode 3 length"), then one
//!   pixel per dot to 160;
//! - the window: the sticky WY condition is an input, the WX compare runs
//!   every dot against `position_in_line + 7`, WX = 0 and WX = 166 have
//!   SameBoy's special cases, activation clears the FIFO and restarts the
//!   fetcher (Pan Docs: Rendering, "Window");
//! - object fetches in X order: each waits for the BG fetcher to reach
//!   its last step with a non-empty FIFO, then takes 6 dots (Pan Docs:
//!   Rendering, "OBJ penalty algorithm" describes the result).
//!
//! The code follows SameBoy's statement order, so its notice applies:
//!
//! SameBoy, Copyright (c) 2015-2026 Lior Halphon. Expat (MIT) License:
//! Permission is hereby granted, free of charge, to any person obtaining a
//! copy of this software and associated documentation files (the
//! "Software"), to deal in the Software without restriction, including
//! without limitation the rights to use, copy, modify, merge, publish,
//! distribute, sublicense, and/or sell copies of the Software, and to
//! permit persons to whom the Software is furnished to do so, subject to
//! the following conditions: The above copyright notice and this
//! permission notice shall be included in all copies or substantial
//! portions of the Software. THE SOFTWARE IS PROVIDED "AS IS", WITHOUT
//! WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
//! THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND
//! NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE
//! LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
//! OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION
//! WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

/// One visible line as the pipeline sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Line {
    /// SCX at the start of mode 3.
    pub(super) scx: u8,
    pub(super) wx: u8,
    /// LCDC.5.
    pub(super) window_on: bool,
    /// The sticky WY == LY condition (SameBoy `wy_triggered`).
    pub(super) wy_triggered: bool,
    /// X bytes of the objects the OAM scan selected (at most 10), any
    /// order.
    pub(super) xs: [u8; 10],
    pub(super) n: usize,
    /// An SCX write by the CPU: `(line position, value)`. The pipeline
    /// sees the new value in the steps it runs after that position: a
    /// CPU access at position `p` comes after the loop's step at `p`.
    /// That is the same ordering that makes a STAT read at the position
    /// where the loop ends see mode 0 (SameBoy advances the display to
    /// the access before a read or a `GB_CONFLICT_READ_OLD` write, which
    /// SCX is on a CGB in single speed), and the end is what
    /// [`LOOP_START`] is fixed by.
    pub(super) scx_write: Option<(u32, u8)>,
}

/// SameBoy `cycles_for_line` when the mode-3 loop starts: `MODE2_LENGTH +
/// 4`, then the 3- and 2-dot sleeps before `mode_3_start`. The core's
/// line positions use the same origin (`docs/architecture.md`, "PPU
/// timing": the pipeline start `Latch` at 89).
const LOOP_START: u32 = 89;

/// SameBoy `fetcher_step_t`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Step {
    TileT1,
    TileT2,
    LowT1,
    LowT2,
    HighT1,
    HighT2,
    Push,
}

struct Pipeline {
    line: Line,
    /// Line position (`cycles_for_line`).
    dot: u32,
    /// `position_in_line`, a wrapping byte from -16.
    pos: u8,
    /// Pixels in the BG FIFO.
    fifo: u8,
    step: Step,
    wx_triggered: bool,
    window_is_being_fetched: bool,
    cgb_wx_glitch: bool,
    insert_bg_pixel: bool,
    line_has_fractional_scrolling: bool,
}

const fn neg(n: u8) -> u8 {
    0u8.wrapping_sub(n)
}

impl Pipeline {
    fn scx(&self) -> u8 {
        match self.line.scx_write {
            Some((at, v)) if self.dot > at => v,
            _ => self.line.scx,
        }
    }

    /// `GB_update_wx_glitch` (CGB).
    fn update_wx_glitch(&mut self) {
        let l = &self.line;
        self.cgb_wx_glitch = if !l.window_on || !l.wy_triggered {
            false
        } else if l.wx == 0 {
            self.pos.wrapping_add(16) <= 8
                || (self.pos == neg(7) && self.line_has_fractional_scrolling)
        } else {
            self.pos
                .wrapping_add(7 + u8::from(self.window_is_being_fetched))
                == l.wx
        };
    }

    /// `advance_fetcher_state_machine`: one step. The VRAM reads are left
    /// out; with `cgb_wx_glitch` the last data step does not push in its
    /// own dot.
    fn advance(&mut self) {
        match self.step {
            Step::TileT1 => {
                self.update_wx_glitch();
                if !self.line.window_on {
                    self.wx_triggered = false;
                }
                self.step = Step::TileT2;
            }
            Step::TileT2 => self.step = Step::LowT1,
            Step::LowT1 => {
                self.update_wx_glitch();
                self.step = Step::LowT2;
            }
            Step::LowT2 => self.step = Step::HighT1,
            Step::HighT1 => {
                self.update_wx_glitch();
                self.step = Step::HighT2;
            }
            Step::HighT2 if self.cgb_wx_glitch => self.step = Step::Push,
            Step::HighT2 | Step::Push => {
                self.step = Step::Push;
                if self.fifo == 0 {
                    self.fifo = 8;
                    self.step = Step::TileT1;
                }
            }
        }
    }

    /// `render_pixel_if_possible`: pops a pixel and moves
    /// `position_in_line`, with the fine-scroll compare while it is in
    /// -16..-9. (`pending_x0`: an object at X = 0 not fetched yet holds
    /// it; the loop below fetches those first, so it never does here.)
    fn render(&mut self, pending_x0: bool) {
        if pending_x0 || self.fifo == 0 {
            return;
        }
        if self.insert_bg_pixel {
            self.insert_bg_pixel = false;
        } else {
            self.fifo -= 1;
        }
        if self.pos.wrapping_add(16) < 8 {
            let fine = self.scx() & 7;
            if self.pos & 7 == fine
                || (self.window_is_being_fetched && self.pos & 7 == 6 && fine == 7)
            {
                self.pos = neg(8);
            } else if self.pos == neg(9) {
                self.pos = neg(16);
                return;
            } else {
                self.line_has_fractional_scrolling = true;
            }
        }
        self.window_is_being_fetched = false;
        self.pos = self.pos.wrapping_add(1);
    }

    /// The window check at the top of the loop (CGB; no WY write on the
    /// line, so `wy_just_checked` is clear).
    fn check_window(&mut self) {
        let l = self.line;
        if self.wx_triggered || !l.wy_triggered || !l.window_on {
            return;
        }
        let activate = if l.wx == 0 {
            self.pos == neg(7)
                || (self.pos == neg(16) && self.scx() & 7 != 0)
                || (neg(15)..=neg(8)).contains(&self.pos)
        } else {
            // "166 on the CGB behaves a bit weird": it activates, at the
            // last pixel.
            l.wx < 167 && l.wx == self.pos.wrapping_add(7)
        };
        if activate {
            self.fifo = 0;
            self.wx_triggered = true;
            self.step = Step::TileT1;
            self.window_is_being_fetched = true;
        }
    }

    /// `x_for_object_match`.
    fn object_match_x(&self) -> u8 {
        let x = self.pos.wrapping_add(8);
        if x > neg(16) { 0 } else { x }
    }
}

/// The line position at which mode 3 ends: SameBoy leaves the loop when
/// `position_in_line` reaches 160 and, in single speed, STAT reads mode 0
/// from that `cycles_for_line` on.
pub(super) fn mode0_start(line: &Line) -> u32 {
    // The objects in fetch order: ascending X (SameBoy keeps
    // `objects_x` sorted the other way and takes from its end).
    let mut xs = line.xs;
    let xs = &mut xs[..line.n.min(10)];
    xs.sort_unstable();
    let mut next = 0;
    let mut p = Pipeline {
        line: *line,
        dot: LOOP_START,
        pos: neg(16),
        fifo: 8,
        step: Step::TileT1,
        wx_triggered: false,
        window_is_being_fetched: false,
        cgb_wx_glitch: false,
        insert_bg_pixel: false,
        line_has_fractional_scrolling: false,
    };
    loop {
        p.check_window();
        // CGB: a pixel is inserted only for WX = 0.
        if line.wx == 0
            && p.pos == neg(7)
            && p.wx_triggered
            && !p.window_is_being_fetched
            && p.step == Step::TileT1
            && p.fifo == 8
        {
            p.insert_bg_pixel = true;
        }
        // Objects left of the position are dropped; those at it are
        // fetched (on a CGB with LCDC.1 clear too).
        while next < xs.len() && xs[next] < p.object_match_x() {
            next += 1;
        }
        while next < xs.len() && xs[next] == p.object_match_x() {
            while p.step < Step::HighT2 || p.fifo == 0 {
                p.advance();
                p.dot += 1;
            }
            p.advance();
            p.dot += 1;
            p.advance();
            // OAM tile and flags, low plane, high plane.
            p.dot += 2 + 2 + 1;
            next += 1;
        }
        p.render(next < xs.len() && xs[next] == 0);
        p.advance();
        // A line is 456 dots: the bound only stops a broken model.
        if p.pos == 160 || p.dot >= 456 {
            return p.dot;
        }
        p.dot += 1;
    }
}

// Inline so the hard-rules test sees `std` inside a `#[cfg(test)] mod`.
#[cfg(test)]
mod tests {
    extern crate std;

    use super::super::*;
    use super::{LOOP_START, Line, mode0_start};
    use std::boxed::Box;
    use std::format;
    use std::string::String;
    use std::vec::Vec;

    const fn line(scx: u8) -> Line {
        Line {
            scx,
            wx: 0xFF,
            window_on: false,
            wy_triggered: false,
            xs: [0; 10],
            n: 0,
            scx_write: None,
        }
    }

    fn with_objects(mut l: Line, xs: &[u8]) -> Line {
        l.n = xs.len();
        l.xs[..xs.len()].copy_from_slice(xs);
        l
    }

    fn with_window(mut l: Line, wx: u8) -> Line {
        l.wx = wx;
        l.window_on = true;
        l.wy_triggered = true;
        l
    }

    /// The oracle alone against lengths the references state.
    #[test]
    fn oracle_gives_the_documented_lengths() {
        // Pan Docs, "Mode 3 length": 172 dots from position 84, plus
        // SCX % 8.
        for scx in 0..=255u8 {
            assert_eq!(mode0_start(&line(scx)), 256 + u32::from(scx & 7));
        }
        // The window: 6 dots.
        assert_eq!(mode0_start(&with_window(line(0), 7)), 262);
        assert_eq!(mode0_start(&with_window(line(0), 100)), 262);
        // WX past the last compare: no window.
        assert_eq!(mode0_start(&with_window(line(0), 167)), 256);
        // Pan Docs, "OBJ penalty algorithm": 11 dots at X = 0; with
        // SCX = 0 an object at X = 8 waits 5 dots for its tile (11), one
        // at X = 12 (4 pixels into it) 1 dot (7), one at X = 13 none (6).
        for (x, dots) in [(0, 11), (8, 11), (12, 7), (13, 6), (15, 6), (16, 11)] {
            let l = with_objects(line(0), &[x]);
            assert_eq!(mode0_start(&l), 256 + dots, "X = {x}");
        }
        // Off screen: nothing.
        assert_eq!(mode0_start(&with_objects(line(0), &[168, 200])), 256);
        // Mooneye `intr_2_mode0_timing_sprites`: 10 objects at X = 0 cost
        // 16 M-cycles (11 + 9 * 6 = 65 dots, rounded up to the test's
        // 4-dot reads).
        assert_eq!(mode0_start(&with_objects(line(0), &[0; 10])), 256 + 65);
    }

    /// A PPU with its own memory and frame, as `ppu::tests::Rig`.
    struct Rig {
        ppu: Ppu,
        mem: Box<Memory>,
        frame: Box<Frame>,
        iflags: u8,
        /// Clock of frame position 0 of the first frame.
        origin: u64,
    }

    /// Line position at which the next line's registers and OAM are set:
    /// in HBlank, after the longest mode 3 (84 + 172 + 7 + 7 + 10 * 11).
    const SETUP_AT: u32 = 400;

    impl Rig {
        /// LCD on with the window enabled and WY = `wy`.
        fn new(wy: u8) -> Self {
            let on = 1000;
            let mut r = Self {
                ppu: Ppu::new(),
                mem: Box::new(Memory::new()),
                frame: Box::new([0; SCREEN_WIDTH * SCREEN_HEIGHT]),
                iflags: 0,
                origin: on - u64::from(ON_POS),
            };
            r.mem.io[0x4A] = wy;
            r.mem.io[0x4B] = 0xFF;
            r.sync(on);
            r.ppu.write_lcdc(on, 0xB3, &mut r.iflags, Speed::Normal);
            r
        }

        fn sync(&mut self, now: u64) {
            let (p, m, f) = (&mut self.ppu, &mut *self.mem, &mut *self.frame);
            p.sync(now, m, f, &mut self.iflags, Speed::Normal);
        }

        /// Clock of position 0 of line `ly` in frame `frame`.
        fn line_clock(&self, frame: u64, ly: u32) -> u64 {
            self.origin + frame * u64::from(FRAME) + u64::from(ly * LINE)
        }

        /// In the HBlank before the line at clock `l` (number `ly`): the
        /// state `c` asks for. Registers are stored as the bus stores
        /// them outside mode 3.
        fn set_up(&mut self, l: u64, ly: u32, c: &Case) {
            let now = l - u64::from(LINE - SETUP_AT);
            self.sync(now);
            let lcdc =
                0x91 | if c.line.window_on { 0x20 } else { 0 } | if c.obj_on { 0x02 } else { 0 };
            if lcdc != self.ppu.lcdc {
                self.ppu
                    .write_lcdc(now, lcdc, &mut self.iflags, Speed::Normal);
            }
            self.mem.io[0x43] = c.line.scx;
            self.mem.io[0x4B] = c.line.wx;
            // The objects in the OAM slots `c.slots` says, the other
            // slots off every line (Y = 0).
            let mut oam = [0u8; 160];
            for (i, &x) in c.line.xs[..c.line.n].iter().enumerate() {
                let at = usize::from(c.slots[i]) * 4;
                oam[at] = ly as u8 + 16;
                oam[at + 1] = x;
            }
            if self.mem.oam[..] != oam[..] {
                self.mem.oam.copy_from_slice(&oam);
                self.ppu.oam_written();
            }
        }

        /// Runs the line at clock `l` and returns where STAT first reads
        /// mode 0 after mode 3. `stepped`: synced and read dot by dot
        /// instead of once after the line's mode 3.
        fn mode0_start(&mut self, l: u64, c: &Case, stepped: bool) -> Option<u32> {
            if stepped {
                let mut end = None;
                for t in 80..SETUP_AT {
                    let now = l + u64::from(t);
                    self.sync(now);
                    if end.is_none() && t >= 84 && self.ppu.stat(now, Speed::Normal) & 3 == 0 {
                        end = Some(t);
                    }
                    self.write_scx(l, c, t);
                }
                end
            } else {
                if let Some((t, _)) = c.line.scx_write {
                    self.sync(l + u64::from(t));
                    self.write_scx(l, c, t);
                }
                self.sync(l + u64::from(SETUP_AT));
                (84..SETUP_AT).find(|&t| self.ppu.stat(l + u64::from(t), Speed::Normal) & 3 == 0)
            }
        }

        /// The case's SCX write, if it is at line position `t` (the PPU
        /// synced to it), as `Ctx::write_ppu` makes it in single speed:
        /// it lands at the access.
        fn write_scx(&mut self, l: u64, c: &Case, t: u32) {
            if let Some((at, v)) = c.line.scx_write
                && at == t
            {
                let now = l + u64::from(t);
                let (m, f) = (&mut *self.mem, &mut *self.frame);
                self.ppu.scroll_write(now, m, f, 0x43, v);
                self.mem.io[0x43] = v;
            }
        }
    }

    #[derive(Clone, Copy, Debug)]
    struct Case {
        line: Line,
        /// LCDC.1. A CGB fetches objects with it clear too (SameBoy:
        /// "this process is skipped entirely on the DMG, but not on the
        /// CGB").
        obj_on: bool,
        /// The OAM slot of each object.
        slots: [u8; 10],
    }

    const SLOTS: [u8; 10] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

    fn case(line: Line) -> Case {
        Case {
            line,
            obj_on: true,
            slots: SLOTS,
        }
    }

    /// Numerical Recipes' 32-bit LCG, fixed seed: no host randomness.
    struct Lcg(u32);

    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            self.0 >> 8
        }

        fn below(&mut self, n: u32) -> u32 {
            self.next() % n
        }
    }

    /// Why a case is left out of the asserting sweep (`None`: it is
    /// asserted). Every entry is a disagreement between the core and the
    /// oracle that no reference available here settles in the oracle's
    /// favour or against it; `docs/reference/mode3-oracle.md` lists them
    /// with examples. Nothing else is skipped.
    fn excluded(c: &Case) -> Option<&'static str> {
        let l = &c.line;
        let starts = l.window_on && l.wy_triggered;
        let on_screen = l.xs[..l.n].iter().any(|x| (1..168).contains(x));
        // 1. The window at WX = 0 with a fine scroll (f = SCX % 8 != 0),
        //    and an object on the line. The totals without objects agree
        //    (6 + 1 dots). An object's wait depends on where the window's
        //    tile grid lies (how many window pixels are cut off at the
        //    left), and no hardware capture here shows that. The
        //    references give three grids:
        //    - 7 cut, whatever f is: Gambatte's model (WX = 0 matches at
        //      its xpos 0, after the discard; none of its tests in the
        //      manifest has WX = 0, a fine scroll and an object). 11 dots
        //      at X = 1 (mod 8). The core's stalls did this until
        //      ADR 0034.
        //    - 7 + f cut: the core, pixels (`Renderer::trigger_skip`) and
        //      stalls (`sorted_object_stalls`) alike since ADR 0034, from
        //      Pan Docs, Window: at WX = 0 the window "is switched to
        //      before the initial fine scroll adjustment, causing it to
        //      be shifted left by SCX % 8 pixels". 11 dots at X = 1 - f
        //      (mod 8).
        //    - 8 + f cut for f = 1..=6, 14 at f = 7 (a pixel inserted at
        //      position -7): SameBoy's loop, which the oracle follows
        //      (marked "TODO: Verify fetcher access timings in this
        //      case" there). 11 dots at X = 8 - f (mod 8); at f = 7 at
        //      X = 1 and from X = 10 on at X = 2 (mod 8).
        //    What is left is the one pixel between 7 + f and 8 + f: an
        //    object's wait is one dot longer in the core, or 6 against
        //    11 where its pixel is the last of a core tile and the first
        //    of an oracle tile; at f = 7 only objects at X = 1..=8
        //    differ (14 = 7 + f there). E.g. SCX 3, X = 5: core 272,
        //    oracle 277. The static sweep leaves out 1,348 lines for
        //    this, 892 of them disagreeing (1,197 before ADR 0034).
        //    Mealybug `m3_window_timing_wx_0` (SCX = LY, no objects)
        //    cannot tell: its window is one solid tile, and it passes
        //    with any of the three cuts in the renderer (tried). No
        //    acceptance ROM has an object on such a line (probed: the
        //    full manifest never builds stalls for one).
        //    What settles it, on a CGB: WX = 0, WY = 0, SCX % 8 = f for
        //    each f in 1..=7, the window's tile map showing a tile whose
        //    8 columns differ (and a different tile next to it). The
        //    window pixel shown at screen x = 0 is its pixel 7
        //    (Gambatte's grid), 7 + f (the core is right) or 8 + f, 14
        //    at f = 7 (SameBoy and the oracle are right). An independent
        //    check of the stalls alone: the length of mode 3 with one
        //    object at each X in 9..=16 is longest (11 dots) at the X on
        //    the grid: 9, 17 - f, or 16 - f (10 at f = 7).
        //    With an SCX write it is the same dispute, counted apart. The
        //    window starts before the discard when SCX % 8 is not 0 as
        //    the pipeline starts (position 89: a write before that is
        //    in), and SameBoy's extra pixel belongs to that early start,
        //    whatever the discard then comes to: the cut is 8 + n for a
        //    discard of n pixels (0..=14) against the core's 7 + n % 8.
        //    Without an on-screen object those lines are asserted (the
        //    discard's compares wait for the window's tile and the stall
        //    keeps its 7 dots: `Ppu::hold_fetch`). With one, the same +1
        //    or 6-against-11. Tried: with the core's grid one pixel
        //    further left, 6,104 of these 6,615 lines (objects at X = 4,
        //    8 or 12, every write) agree, and the rest are a discard of 0
        //    or 8 pixels, which that trial grid did not cover (231), and
        //    X = 12 at a discard of 7 (280: SameBoy's 14, above).
        let early_scx = match l.scx_write {
            Some((at, v)) if at < LOOP_START => v,
            _ => l.scx,
        };
        if starts && l.wx == 0 && early_scx & 7 != 0 && on_screen {
            return Some(if l.scx_write.is_none() {
                "WX = 0 with SCX % 8 != 0 and an object"
            } else {
                "WX = 0 with SCX % 8 != 0 and an object, with an SCX write"
            });
        }
        None
    }

    #[derive(Default)]
    struct Tally {
        compared: usize,
        failures: Vec<String>,
        /// Per exclusion: cases, and how many of them disagree.
        excluded: Vec<(&'static str, usize, usize)>,
    }

    impl Tally {
        fn check(&mut self, c: &Case, core: Option<u32>, how: &str) {
            let want = mode0_start(&c.line);
            let agree = core == Some(want);
            if let Some(why) = excluded(c) {
                let e = match self.excluded.iter_mut().find(|e| e.0 == why) {
                    Some(e) => e,
                    None => {
                        self.excluded.push((why, 0, 0));
                        self.excluded.last_mut().unwrap()
                    }
                };
                e.1 += 1;
                e.2 += usize::from(!agree);
                return;
            }
            self.compared += 1;
            if !agree {
                let l = &c.line;
                self.failures.push(format!(
                    "scx {} wx {} win {} wy {} obj_on {} xs {:?} write {:?} ({how}): core {:?}, oracle {}",
                    l.scx,
                    l.wx,
                    l.window_on,
                    l.wy_triggered,
                    c.obj_on,
                    &l.xs[..l.n],
                    l.scx_write,
                    core,
                    want
                ));
            }
        }

        fn finish(self, name: &str) {
            for (why, n, bad) in &self.excluded {
                std::eprintln!("{name}: excluded {n} cases ({bad} disagree): {why}");
                // An exclusion that no longer disagrees must be removed.
                assert!(*bad > 0, "{name}: exclusion without a disagreement: {why}");
            }
            std::eprintln!("{name}: {} cases compared", self.compared);
            let n = self.failures.len();
            let max = if std::env::var_os("MODE3_ORACLE_ALL").is_some() {
                usize::MAX
            } else {
                40
            };
            let shown: Vec<&str> = self.failures.iter().take(max).map(String::as_str).collect();
            assert!(n == 0, "{name}: {n} disagreements:\n{}", shown.join("\n"));
        }
    }

    /// Runs `cases` through the core on consecutive lines (1..=143 of
    /// each frame; line 0 runs with no objects and no window start), one
    /// rig for all. Each case's `wy_triggered` must be `wy == 0`: with
    /// WY = 0 and LCDC.5 set on line 0 the condition holds for the frame.
    fn run(cases: &[Case], wy: u8, tally: &mut Tally) {
        let mut r = Rig::new(wy);
        let idle = case(Line {
            window_on: true,
            wy_triggered: wy == 0,
            ..line(0)
        });
        let mut cases = cases.iter().enumerate();
        'frames: for frame in 0.. {
            // Line 0 of the first frame is the line after LCD enable.
            for ly in 0..LINES {
                let l = r.line_clock(frame, ly);
                if ly == 0 {
                    if frame != 0 {
                        r.set_up(l, ly, &idle);
                    }
                    continue;
                }
                let Some((i, c)) = cases.next() else {
                    break 'frames;
                };
                assert_eq!(c.line.wy_triggered, wy == 0);
                r.set_up(l, ly, c);
                let stepped = i % 8 == 3;
                let core = r.mode0_start(l, c, stepped);
                tally.check(c, core, if stepped { "stepped" } else { "one sync" });
            }
        }
    }

    /// The static sweep: registers and OAM fixed for the whole line.
    fn static_cases(random: u32) -> Vec<Case> {
        let mut out = Vec::new();
        // No objects: every fine scroll with every WX, window on and off.
        for scx in 0..8u8 {
            for wx in (0..=167u8).chain([0xFF]) {
                out.push(case(with_window(line(scx), wx)));
                let mut off = with_window(line(scx), wx);
                off.window_on = false;
                out.push(case(off));
            }
        }
        // One object at every X, with and without a window around it.
        for scx in [0u8, 1, 2, 3, 4, 5, 6, 7, 0x58, 0xFD] {
            for x in 0..=168u8 {
                out.push(case(with_objects(line(scx), &[x])));
                for wx in [0u8, 3, 7, 8, 50, 166] {
                    out.push(case(with_objects(with_window(line(scx), wx), &[x])));
                }
            }
        }
        // Two objects: every distance 0..=16 at every phase of the tile
        // grid.
        for scx in [0u8, 3, 5, 7] {
            for x in (0..=40u8).chain(150..=168) {
                for d in 0..=16u8 {
                    out.push(case(with_objects(line(scx), &[x, x + d])));
                }
            }
        }
        // 1..=10 objects at one X, and spaced 1..=9 apart.
        for scx in 0..8u8 {
            for n in 1..=10usize {
                for x in (0..=20u8).chain(158..=168) {
                    out.push(case(with_objects(line(scx), &[x; 10][..n])));
                }
                for gap in 1..=9u8 {
                    let xs: Vec<u8> = (0..n as u8).map(|i| 5 + scx + i * gap).collect();
                    out.push(case(with_objects(line(scx), &xs)));
                }
            }
        }
        // Pseudo-random lines.
        let mut g = Lcg(0x6BC3_2001);
        for _ in 0..random {
            let mut l = line(g.below(256) as u8);
            if g.below(3) != 0 {
                l = with_window(l, g.below(172) as u8);
                l.window_on = g.below(4) != 0;
            }
            let n = g.below(11) as usize;
            // Spread over the line, clustered in a few tiles, or at the
            // left edge.
            let (base, span) = match g.below(4) {
                0 => (0, 176),
                1 => (g.below(150), 24),
                2 => (g.below(160), 9),
                _ => (0, 12),
            };
            let mut xs = [0u8; 10];
            for x in &mut xs[..n] {
                *x = (base + g.below(span)) as u8;
            }
            let mut c = case(with_objects(l, &xs[..n]));
            c.obj_on = g.below(8) != 0;
            // The objects anywhere in OAM, in any order.
            let mut slots: Vec<u8> = (0..40).collect();
            for (i, s) in c.slots.iter_mut().enumerate() {
                let j = i + g.below(40 - i as u32) as usize;
                slots.swap(i, j);
                *s = slots[i];
            }
            out.push(c);
        }
        out
    }

    /// The same cases with the WY condition not met: the window never
    /// starts.
    fn untriggered(cases: &[Case]) -> Vec<Case> {
        cases
            .iter()
            .filter(|c| c.line.wy_triggered)
            .step_by(7)
            .map(|c| {
                let mut c = *c;
                c.line.wy_triggered = false;
                c
            })
            .collect()
    }

    fn compare_static(random: u32, name: &str) {
        let cases = static_cases(random);
        let mut tally = Tally::default();
        // WY = 0: the condition holds from line 0; no line reaches 0xFF.
        let (met, unmet): (Vec<Case>, Vec<Case>) = cases.iter().partition(|c| c.line.wy_triggered);
        run(&met, 0, &mut tally);
        run(&unmet, 0xFF, &mut tally);
        run(&untriggered(&cases), 0xFF, &mut tally);
        tally.finish(name);
    }

    #[test]
    fn core_mode3_length_matches_the_dot_stepped_oracle() {
        compare_static(6_000, "static");
    }

    /// One SCX write in mode 3 (or just before it), as `Ctx::write_ppu`
    /// makes it in single speed.
    fn scx_write_cases(random: u32) -> Vec<Case> {
        let mut out = Vec::new();
        let objects: [&[u8]; 9] = [
            &[],
            &[0],
            &[0, 0, 40],
            &[0, 4],
            &[8],
            &[12],
            &[3, 20],
            &[9, 9, 30],
            &[100, 167],
        ];
        // Every dot around the discard, and a few after it.
        let dots = (80..=124u32).chain([140, 150, 200, 250, 262]);
        for t in dots {
            for from in 0..8u8 {
                for to in 0..8u8 {
                    if from == to {
                        continue;
                    }
                    for xs in objects {
                        let mut l = with_objects(line(from), xs);
                        l.scx_write = Some((t, to));
                        out.push(case(l));
                    }
                    for wx in [7u8, 20] {
                        let mut l = with_window(line(from), wx);
                        l.scx_write = Some((t, to));
                        out.push(case(l));
                    }
                    // The window at WX = 0: with a fine scroll at the
                    // line's start it begins before the discard.
                    let at_wx0: [&[u8]; 5] = [&[], &[0], &[0, 0], &[4], &[12]];
                    for xs in at_wx0 {
                        let mut l = with_objects(with_window(line(from), 0), xs);
                        l.scx_write = Some((t, to));
                        out.push(case(l));
                    }
                    // A window stall at pixel 0 beside the objects'.
                    for wx in [3u8, 7] {
                        let mut l = with_objects(with_window(line(from), wx), &[0, 4]);
                        l.scx_write = Some((t, to));
                        out.push(case(l));
                    }
                }
            }
        }
        let mut g = Lcg(0x5C8_2002);
        for _ in 0..random {
            let mut l = line(g.below(256) as u8);
            if g.below(3) == 0 {
                l = with_window(l, 1 + g.below(170) as u8);
            }
            let n = g.below(5) as usize;
            let mut xs = [0u8; 10];
            for x in &mut xs[..n] {
                let span = if g.below(2) == 0 { 176 } else { 24 };
                *x = g.below(span) as u8;
            }
            l = with_objects(l, &xs[..n]);
            let t = if g.below(4) == 0 {
                80 + g.below(200)
            } else {
                84 + g.below(30)
            };
            l.scx_write = Some((t, g.below(256) as u8));
            out.push(case(l));
        }
        out
    }

    fn compare_scx_writes(random: u32, name: &str) {
        let cases = scx_write_cases(random);
        let mut tally = Tally::default();
        let (met, unmet): (Vec<Case>, Vec<Case>) = cases.iter().partition(|c| c.line.wy_triggered);
        run(&met, 0, &mut tally);
        run(&unmet, 0xFF, &mut tally);
        tally.finish(name);
    }

    #[test]
    fn core_mode3_length_with_an_scx_write_matches_the_oracle() {
        compare_scx_writes(4_000, "SCX write");
    }

    #[test]
    #[ignore = "exhaustive variant: about half a million lines"]
    fn core_mode3_length_with_an_scx_write_matches_the_oracle_exhaustive() {
        compare_scx_writes(500_000, "SCX write, exhaustive");
    }

    /// The same sweep with many more pseudo-random lines.
    #[test]
    #[ignore = "exhaustive variant: about a million lines"]
    fn core_mode3_length_matches_the_dot_stepped_oracle_exhaustive() {
        compare_static(1_000_000, "static, exhaustive");
    }
}
