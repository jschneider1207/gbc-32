//! The `player` image's board-independent parts (Stage 2 M5): the one DMG
//! compatibility palette and the per-frame statistics window it reports
//! every few seconds (`docs/reference/player.md`).
//!
//! # Statistics
//!
//! For each loop iteration the board adds two cycle counts with
//! [`Window::add`]:
//! - `core`: around the `run_frame` call alone
//!   ([`crate::paced::FrameCycles::core`]);
//! - `busy`: the whole iteration **awake**: from one call of the loop's
//!   service hook to the next, minus the cycles spent asleep in `WFI` in
//!   between. That is everything the CPU did for the frame: the service
//!   hook (link, saves, statistics), input, the core, the audio push, the
//!   frame hash, the display hand-off and the idle hook's display and DAC
//!   checks. Interrupt handlers that run while awake are in it too.
//!
//! A frame is **over budget** when `busy` exceeds [`frame_budget`] (one
//! Game Boy frame of real time, 70,224 / 4,194,304 s = 1/59.7275 s:
//! 7,902,557 cycles at 472 MHz): the loop could not keep up with real time
//! if every frame cost that much (plan Stage 2 D7, M6 "the whole loop fits
//! real time"). The audio ring absorbs single slow frames, so an over-budget
//! frame is not yet an underrun; the underrun counters say whether one was
//! heard.
//!
//! `fe` (front end) is `busy - core`; `slack` is `budget - busy`.
//!
//! The statistics report itself (formatting the fields, and for a `Stats`
//! frame its COBS/CRC encoding: about 0.9 kB of text, u64 formatting
//! included) runs in the service hook of one frame per report (the
//! board's `STATS_FRAMES`), so it is in that frame's `busy` and `fe`
//! (review M4). It stays there: `busy` is what the over-budget count (and
//! the gates' `over_budget=0`) checks, so it keeps every cycle the CPU
//! spent awake. The board times
//! the report and passes it as `stats` ([`Window::add_with_stats`]):
//! `stats_max` is the largest such count in the window, and
//! `fe_max_ex_stats` the largest `fe` with the frame's `stats` cycles
//! taken out, the front end of the loop itself.
//!
//! `core_p99` comes from a fixed histogram of [`BINS`] bins of
//! 2^[`BIN_SHIFT`] cycles (no allocation): it is the upper edge of the bin
//! holding the 99th-percentile frame (rank `ceil(0.99 n)`), capped at
//! `core_max`, so it is at most 65,535 cycles above the exact value.

use core::fmt::{self, Write};

use gbc_core::{CLOCK_HZ, DmgPalettes, FRAME_CLOCKS, rgb888_to_555};

/// The DMG compatibility palette the player gives every game that runs in
/// DMG mode: romtest's `common_palette()` (tools/romtest/src/machine.rs),
/// one constant for every title (no per-title palettes on the dev board,
/// plan Stage 2 M5).
pub const COMPAT_PALETTE: DmgPalettes = {
    const fn c(rgb: u32) -> u16 {
        rgb888_to_555((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }
    let obj = [c(0xFFFFFF), c(0xFF8484), c(0x943939), c(0x000000)];
    DmgPalettes {
        bg: [c(0xFFFFFF), c(0x7BFF31), c(0x0063C6), c(0x000000)],
        obj0: obj,
        obj1: obj,
    }
};

/// One Game Boy frame of real time in counts of a `hz` clock, rounded
/// down: 7,902,557 at 472 MHz.
pub const fn frame_budget(hz: u32) -> u32 {
    (hz as u64 * FRAME_CLOCKS as u64 / CLOCK_HZ as u64) as u32
}

/// log2 of the histogram's bin width in cycles (65,536: 0.14 ms at
/// 472 MHz).
pub const BIN_SHIFT: u32 = 16;
/// Histogram bins: up to 8,388,608 cycles, a little over one frame's
/// budget at 472 MHz; the last bin also takes everything above.
pub const BINS: usize = 128;

/// The statistics of the frames since the last [`Window::reset`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Window {
    frames: u32,
    core_min: u32,
    core_max: u32,
    core_sum: u64,
    hist: [u32; BINS],
    fe_max: u32,
    fe_sum: u64,
    busy_max: u32,
    busy_sum: u64,
    over: u32,
    stats_max: u32,
    fe_ex_stats_max: u32,
}

impl Default for Window {
    fn default() -> Self {
        Self::new()
    }
}

impl Window {
    pub const fn new() -> Self {
        Self {
            frames: 0,
            core_min: u32::MAX,
            core_max: 0,
            core_sum: 0,
            hist: [0; BINS],
            fe_max: 0,
            fe_sum: 0,
            busy_max: 0,
            busy_sum: 0,
            over: 0,
            stats_max: 0,
            fe_ex_stats_max: 0,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// One frame: its `core` and `busy` counts (module docs) against
    /// `budget` ([`frame_budget`]).
    pub fn add(&mut self, core: u32, busy: u32, budget: u32) {
        self.add_with_stats(core, busy, 0, budget);
    }

    /// [`Window::add`] for a frame whose `busy` includes `stats` cycles
    /// of statistics reporting (module docs): they count in `busy` and
    /// `fe` as any other work, and are taken out of `fe_max_ex_stats`
    /// only.
    pub fn add_with_stats(&mut self, core: u32, busy: u32, stats: u32, budget: u32) {
        self.frames += 1;
        self.stats_max = self.stats_max.max(stats);
        self.core_min = self.core_min.min(core);
        self.core_max = self.core_max.max(core);
        self.core_sum += u64::from(core);
        let bin = ((core >> BIN_SHIFT) as usize).min(BINS - 1);
        self.hist[bin] += 1;
        let fe = busy.saturating_sub(core);
        self.fe_max = self.fe_max.max(fe);
        self.fe_sum += u64::from(fe);
        self.fe_ex_stats_max = self.fe_ex_stats_max.max(fe.saturating_sub(stats));
        self.busy_max = self.busy_max.max(busy);
        self.busy_sum += u64::from(busy);
        if busy > budget {
            self.over += 1;
        }
    }

    pub fn frames(&self) -> u32 {
        self.frames
    }

    /// 0 for an empty window.
    pub fn core_min(&self) -> u32 {
        if self.frames == 0 { 0 } else { self.core_min }
    }

    pub fn core_max(&self) -> u32 {
        self.core_max
    }

    pub fn core_mean(&self) -> u32 {
        mean(self.core_sum, self.frames)
    }

    /// Frames whose `busy` count exceeded the budget.
    pub fn over_budget(&self) -> u32 {
        self.over
    }

    pub fn busy_max(&self) -> u32 {
        self.busy_max
    }

    /// The 99th percentile of `core`, as an upper bound (module docs); 0
    /// for an empty window.
    pub fn core_p99(&self) -> u32 {
        if self.frames == 0 {
            return 0;
        }
        let rank = (u64::from(self.frames) * 99).div_ceil(100);
        let mut seen = 0u64;
        for (i, &n) in self.hist.iter().enumerate() {
            seen += u64::from(n);
            if seen >= rank {
                if i == BINS - 1 {
                    return self.core_max;
                }
                let edge = (((i as u32) + 1) << BIN_SHIFT) - 1;
                return edge.min(self.core_max);
            }
        }
        self.core_max
    }

    /// `frames=<n> core_min=<c> core_mean=<c> core_p99=<c> core_max=<c>
    /// fe_mean=<c> fe_max=<c> busy_mean=<c> busy_max=<c> slack_min=<+-c>
    /// over_budget=<n> budget=<c> stats_max=<c> fe_max_ex_stats=<c>`, no
    /// line end.
    pub fn write_fields(&self, out: &mut dyn Write, budget: u32) -> fmt::Result {
        write!(
            out,
            "frames={} core_min={} core_mean={} core_p99={} core_max={} fe_mean={} fe_max={} \
             busy_mean={} busy_max={} slack_min={} over_budget={} budget={budget} stats_max={} \
             fe_max_ex_stats={}",
            self.frames,
            self.core_min(),
            self.core_mean(),
            self.core_p99(),
            self.core_max,
            mean(self.fe_sum, self.frames),
            self.fe_max,
            mean(self.busy_sum, self.frames),
            self.busy_max,
            i64::from(budget) - i64::from(self.busy_max),
            self.over,
            self.stats_max,
            self.fe_ex_stats_max,
        )
    }
}

fn mean(sum: u64, n: u32) -> u32 {
    (sum / u64::from(n.max(1))) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    struct S {
        buf: [u8; 512],
        len: usize,
    }
    impl Write for S {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.len + s.len();
            self.buf
                .get_mut(self.len..end)
                .ok_or(fmt::Error)?
                .copy_from_slice(s.as_bytes());
            self.len = end;
            Ok(())
        }
    }

    fn fields(w: &Window, budget: u32) -> S {
        let mut s = S {
            buf: [0; 512],
            len: 0,
        };
        w.write_fields(&mut s, budget).unwrap();
        s
    }

    #[test]
    fn budget_at_472_mhz() {
        assert_eq!(frame_budget(472_000_000), 7_902_557);
        assert_eq!(frame_budget(CLOCK_HZ), FRAME_CLOCKS);
    }

    #[test]
    fn palette_is_romtests_common_palette() {
        // tools/romtest/src/machine.rs common_palette(), in RGB555.
        assert_eq!(COMPAT_PALETTE.bg, [0x7FFF, 0x1BEF, 0x6180, 0x0000]);
        assert_eq!(COMPAT_PALETTE.obj0, [0x7FFF, 0x421F, 0x1CF2, 0x0000]);
        assert_eq!(COMPAT_PALETTE.obj1, COMPAT_PALETTE.obj0);
    }

    #[test]
    fn empty_window() {
        let w = Window::new();
        assert_eq!(
            (w.frames(), w.core_min(), w.core_max(), w.core_p99()),
            (0, 0, 0, 0)
        );
        let s = fields(&w, 100);
        assert_eq!(
            core::str::from_utf8(&s.buf[..s.len]).unwrap(),
            "frames=0 core_min=0 core_mean=0 core_p99=0 core_max=0 fe_mean=0 fe_max=0 \
             busy_mean=0 busy_max=0 slack_min=100 over_budget=0 budget=100 stats_max=0 fe_max_ex_stats=0"
        );
    }

    #[test]
    fn min_mean_max_front_end_and_over_budget() {
        let mut w = Window::new();
        w.add(1000, 1500, 2000);
        w.add(3000, 2000, 2000); // busy == budget: not over
        w.add(2000, 2001, 2000); // over by one
        w.add(10, 5, 2000); // busy < core: front end 0, not negative
        assert_eq!(
            (w.core_min(), w.core_mean(), w.core_max()),
            (10, 1502, 3000)
        );
        assert_eq!(w.over_budget(), 1);
        let s = fields(&w, 2000);
        assert_eq!(
            core::str::from_utf8(&s.buf[..s.len]).unwrap(),
            "frames=4 core_min=10 core_mean=1502 core_p99=3000 core_max=3000 fe_mean=125 \
             fe_max=500 busy_mean=1376 busy_max=2001 slack_min=-1 over_budget=1 budget=2000 \
             stats_max=0 fe_max_ex_stats=500"
        );
        w.reset();
        assert_eq!(w, Window::new());
    }

    #[test]
    fn stats_cycles_stay_in_busy_and_fe_and_leave_fe_max_ex_stats() {
        let mut w = Window::new();
        w.add_with_stats(1000, 1900, 600, 1800); // fe 900, 300 without stats
        w.add(1000, 1400, 1800); // fe 400
        w.add_with_stats(1000, 1100, 700, 1800); // stats > fe: 0, not negative
        assert_eq!(w.busy_max(), 1900);
        assert_eq!(w.over_budget(), 1, "the stats frame stays over budget");
        let s = fields(&w, 1800);
        assert_eq!(
            core::str::from_utf8(&s.buf[..s.len]).unwrap(),
            "frames=3 core_min=1000 core_mean=1000 core_p99=1000 core_max=1000 fe_mean=466 \
             fe_max=900 busy_mean=1466 busy_max=1900 slack_min=-100 over_budget=1 budget=1800 \
             stats_max=700 fe_max_ex_stats=400"
        );
    }

    #[test]
    fn p99_is_the_bin_edge_of_rank_ceil_99_percent() {
        let bin = 1u32 << BIN_SHIFT;
        let mut w = Window::new();
        // 99 frames in bin 3, one in bin 50: rank 99 is in bin 3.
        for _ in 0..99 {
            w.add(3 * bin + 7, 0, u32::MAX);
        }
        w.add(50 * bin, 0, u32::MAX);
        assert_eq!(w.core_p99(), 4 * bin - 1);
        // A 101st frame in bin 50: rank ceil(99.99) = 100 is now there.
        w.add(50 * bin + 1, 0, u32::MAX);
        assert_eq!(w.core_p99(), 50 * bin + 1);
        // Capped at the maximum when the bin's edge is above it.
        let mut w = Window::new();
        w.add(5, 0, u32::MAX);
        assert_eq!(w.core_p99(), 5);
        // The last bin holds everything above the histogram's range.
        let mut w = Window::new();
        w.add(u32::MAX - 1, 0, u32::MAX);
        assert_eq!(w.core_p99(), u32::MAX - 1);
    }
}
