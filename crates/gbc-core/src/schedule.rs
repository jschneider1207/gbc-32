//! The event table: a small fixed array of deadlines, one per kind of
//! scheduled work, and their minimum (ADR 0011, "The event table").
//!
//! The tick never looks at this table. It only counts down to the earliest
//! deadline (or the run target), and `Ctx::boundary` then runs the due
//! kinds in [`EventKind`] order. Deadlines change only when the work they
//! stand for is rescheduled (a PPU sync, a register write), so the minimum
//! is maintained there (incrementally, rescanning only when the earliest
//! entry moves later) and never per M-cycle.
//!
//! The dispatch (`Ctx::run_events`) tests the three hot kinds in place,
//! lets each handler store its next deadline, and folds the minimum once
//! at the end ([`Schedule::take_hot`], [`Schedule::rearm`],
//! [`Schedule::settle`]). The kinds after `Ppu` fire rarely (`Dma` at most
//! every ~160 M-cycles, `Input` per button change, the APU ones per
//! frame-sequencer step or audio buffer), so their minimum is cached in
//! [`Schedule::slow`], and that fold is over the three hot kinds and the
//! cache: four values, not eight.

/// No deadline.
pub(crate) const NEVER: u64 = u64::MAX;

/// A kind of scheduled work. The declaration order is the dispatch order
/// for deadlines on the same clock (ADR 0011), so it is part of the
/// emulated behavior: don't reorder.
///
/// `Timer`, `Serial`, `Ppu` and `Input` (ADR 0012) are scheduled so far.
/// The others are declared now to pin that order and get their handlers
/// with the work they stand for: `Dma` in ADR 0011 step 6 (M2), `DivApu`
/// and `AudioBuffer` in M6.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EventKind {
    /// TIMA reload + timer interrupt.
    Timer,
    /// One serial bit shift.
    Serial,
    /// The PPU's next sync (VBlank; mode/STAT events in M3).
    Ppu,
    /// OAM DMA completion.
    Dma,
    /// An HBlank starts a VRAM DMA block (HDMA, ADR 0017).
    VramDma,
    /// A scheduled button change (`set_buttons_at`).
    Input,
    /// A DIV-APU frame-sequencer step.
    DivApu,
    /// The audio buffer boundary.
    AudioBuffer,
}

impl EventKind {
    /// Every kind, in dispatch (index) order.
    const ALL: [Self; 8] = [
        Self::Timer,
        Self::Serial,
        Self::Ppu,
        Self::Dma,
        Self::VramDma,
        Self::Input,
        Self::DivApu,
        Self::AudioBuffer,
    ];
    const COUNT: usize = Self::ALL.len();
    /// Kinds below this index are folded on every dispatch; the others
    /// ("slow") through [`Schedule::slow`].
    const HOT: usize = Self::Dma as usize;
}

/// One deadline (a clock, or [`NEVER`]) per [`EventKind`], and the
/// earliest of them.
#[derive(Clone, Copy)]
pub(crate) struct Schedule {
    at: [u64; EventKind::COUNT],
    /// The minimum of `at`.
    next: u64,
    /// The minimum of the slow entries, `at[EventKind::HOT..]`.
    slow: u64,
}

impl Schedule {
    /// Nothing scheduled.
    pub(crate) const fn new() -> Self {
        Self {
            at: [NEVER; EventKind::COUNT],
            next: NEVER,
            slow: NEVER,
        }
    }

    /// Moves every set deadline `by` clocks later (the `clock-offset` test
    /// hook, `Emulator::start_clock_at`).
    #[cfg(feature = "clock-offset")]
    pub(crate) fn shift_clock(&mut self, by: u64) {
        for t in self.at.iter_mut().chain([&mut self.next, &mut self.slow]) {
            if *t != NEVER {
                *t += by;
            }
        }
    }

    /// The earliest deadline, or [`NEVER`].
    #[inline]
    pub(crate) const fn next(&self) -> u64 {
        self.next
    }

    /// The deadline of `kind`, or [`NEVER`].
    #[inline]
    pub(crate) const fn deadline(&self, kind: EventKind) -> u64 {
        self.at[kind as usize]
    }

    /// Sets the deadline of `kind` (a clock, or [`NEVER`]). The caller
    /// reschedules the countdown if it is running (`Ctx::reschedule`).
    ///
    /// The minimum is kept incrementally: a deadline at or before it
    /// becomes it, and the table is rescanned only when the entry that
    /// held it moves later.
    #[inline(never)]
    pub(crate) fn set(&mut self, kind: EventKind, at: u64) {
        self.set_inline(kind, at);
    }

    /// [`Self::set`], inlined: for the event handlers in `Ctx::boundary`.
    #[inline(always)]
    pub(crate) fn set_inline(&mut self, kind: EventKind, at: u64) {
        let i = kind as usize;
        let old = self.at[i];
        self.at[i] = at;
        // `i` is a constant here (every caller names its kind), so this
        // branch folds away.
        if i >= EventKind::HOT {
            if at <= self.slow {
                self.slow = at;
            } else if old == self.slow {
                self.recompute_slow();
            }
        }
        if at <= self.next {
            self.next = at;
        } else if old == self.next {
            self.recompute();
        }
    }

    /// `next` from the hot entries and the cached slow minimum.
    #[inline]
    fn recompute(&mut self) {
        let hot = &self.at[..EventKind::HOT];
        self.next = hot.iter().fold(self.slow, |m, &t| m.min(t));
    }

    #[inline]
    fn recompute_slow(&mut self) {
        let slow = &self.at[EventKind::HOT..];
        self.slow = slow.iter().fold(NEVER, |m, &t| m.min(t));
    }

    /// The first kind, in [`EventKind`] order, whose deadline is at or
    /// before `now`, with its deadline cleared to [`NEVER`] (its handler
    /// sets the next one). `None` once nothing is due, at the cost of one
    /// compare: nothing is due while `now` is before the minimum.
    ///
    /// The test-only composition of the three steps `Ctx::run_events`
    /// uses: [`Self::take_hot`] per hot kind, [`Self::take_slow`], and
    /// [`Self::settle`].
    #[cfg(test)]
    pub(crate) fn take_due(&mut self, now: u64) -> Option<EventKind> {
        if now < self.next {
            return None;
        }
        let hot = &EventKind::ALL[..EventKind::HOT];
        let kind = match hot.iter().find(|&&k| self.take_hot(k, now)) {
            Some(&k) => Some(k),
            None => self.take_slow(now),
        };
        self.settle(self.hot_min());
        kind
    }

    /// The event loop's test of one hot kind (`Timer`, `Serial`, `Ppu`):
    /// whether its deadline is at or before `now`, clearing it to
    /// [`NEVER`] if so (its handler sets the next one).
    ///
    /// This does not maintain the minimum: [`Self::next`] is stale from
    /// here until [`Self::settle`], which the event loop calls once after
    /// its last handler. One fold per `Ctx::boundary` instead of one per
    /// event and another in the handler's `set` (docs/perf.md, "One-pass
    /// scheduler").
    #[inline(always)]
    pub(crate) fn take_hot(&mut self, kind: EventKind, now: u64) -> bool {
        let i = kind as usize;
        debug_assert!(i < EventKind::HOT);
        let due = self.at[i] <= now;
        if due {
            self.at[i] = NEVER;
        }
        due
    }

    /// The first slow kind (`Dma` on) due at `now`, cleared to [`NEVER`],
    /// or `None`: one compare while none is due. Keeps [`Self::slow`]
    /// exact; like [`Self::take_hot`], leaves the minimum to
    /// [`Self::settle`].
    #[inline(always)]
    pub(crate) fn take_slow(&mut self, now: u64) -> Option<EventKind> {
        if self.slow > now {
            return None;
        }
        let slow = &self.at[EventKind::HOT..];
        let i = EventKind::HOT + slow.iter().position(|&t| t <= now)?;
        self.at[i] = NEVER;
        self.recompute_slow();
        Some(EventKind::ALL[i])
    }

    /// Sets the deadline of a hot kind from its own handler, between
    /// [`Self::take_hot`] and [`Self::settle`]: a plain store, the minimum
    /// being recomputed by `settle`.
    #[inline(always)]
    pub(crate) fn rearm(&mut self, kind: EventKind, at: u64) {
        let i = kind as usize;
        debug_assert!(i < EventKind::HOT);
        self.at[i] = at;
    }

    /// Whether the deadline of `kind` is at or before `now`.
    #[inline(always)]
    pub(crate) fn due(&self, kind: EventKind, now: u64) -> bool {
        self.at[kind as usize] <= now
    }

    /// The minimum of the hot entries.
    #[inline(always)]
    pub(crate) fn hot_min(&self) -> u64 {
        let hot = &self.at[..EventKind::HOT];
        hot.iter().fold(NEVER, |m, &t| m.min(t))
    }

    /// Makes the minimum exact again after the event loop's takes and
    /// re-arms. `hot` is the current [`Self::hot_min`].
    #[inline(always)]
    pub(crate) fn settle(&mut self, hot: u64) {
        debug_assert_eq!(hot, self.hot_min());
        self.next = hot.min(self.slow);
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::{EventKind, NEVER, Schedule};
    use std::vec::Vec;

    #[test]
    fn kinds_are_in_index_order() {
        for (i, &k) in EventKind::ALL.iter().enumerate() {
            assert_eq!(k as usize, i);
        }
    }

    #[test]
    fn empty_table_is_never() {
        let mut s = Schedule::new();
        assert_eq!(s.next(), NEVER);
        // The clock never reaches NEVER (u64 base clocks: ~70,000 years).
        assert_eq!(s.take_due(NEVER - 1), None);
        for k in EventKind::ALL {
            assert_eq!(s.deadline(k), NEVER);
        }
    }

    #[test]
    fn next_is_the_minimum_and_follows_changes() {
        let mut s = Schedule::new();
        s.set(EventKind::Ppu, 500);
        assert_eq!(s.next(), 500);
        s.set(EventKind::Dma, 300);
        assert_eq!(s.next(), 300);
        s.set(EventKind::Timer, 400);
        assert_eq!(s.next(), 300);
        // Moving the earliest deadline later uncovers the next one.
        s.set(EventKind::Dma, 900);
        assert_eq!(s.next(), 400);
        s.set(EventKind::Timer, NEVER);
        assert_eq!(s.next(), 500);
        s.set(EventKind::Ppu, NEVER);
        assert_eq!(s.next(), 900);
        s.set(EventKind::Dma, NEVER);
        assert_eq!(s.next(), NEVER);
    }

    #[test]
    fn due_kinds_run_in_index_order() {
        let mut s = Schedule::new();
        // Set in reverse order, some on the same clock, one in the future.
        s.set(EventKind::AudioBuffer, 100);
        s.set(EventKind::Input, 100);
        s.set(EventKind::Ppu, 90);
        s.set(EventKind::Serial, 100);
        s.set(EventKind::Dma, 101);
        let mut order = Vec::new();
        while let Some(k) = s.take_due(100) {
            assert_eq!(s.deadline(k), NEVER, "taken deadline is cleared");
            order.push(k);
        }
        assert_eq!(
            order,
            [
                EventKind::Serial,
                EventKind::Ppu,
                EventKind::Input,
                EventKind::AudioBuffer
            ]
        );
        assert_eq!(s.next(), 101);
        assert_eq!(s.take_due(100), None);
        assert_eq!(s.take_due(101), Some(EventKind::Dma));
        assert_eq!(s.next(), NEVER);
    }

    #[test]
    fn rescheduling_inside_the_loop_is_seen() {
        // A handler that sets a deadline still due at `now` runs again in
        // the same loop; one set in the future waits.
        let mut s = Schedule::new();
        s.set(EventKind::Timer, 10);
        s.set(EventKind::Ppu, 10);
        assert_eq!(s.take_due(10), Some(EventKind::Timer));
        s.set(EventKind::Timer, 14);
        assert_eq!(s.take_due(10), Some(EventKind::Ppu));
        assert_eq!(s.take_due(10), None);
        assert_eq!(s.next(), 14);
    }

    // The incremental minimum (`Schedule::set`, `take_due`).

    #[test]
    fn minimum_moving_later_rescans() {
        let mut s = Schedule::new();
        s.set(EventKind::Ppu, 500);
        s.set(EventKind::Timer, 400);
        // The holder moves later but stays the earliest.
        s.set(EventKind::Timer, 450);
        assert_eq!(s.next(), 450);
        // The holder moves past another entry: that one is the minimum.
        s.set(EventKind::Timer, 900);
        assert_eq!(s.next(), 500);
    }

    #[test]
    fn earlier_deadline_becomes_the_minimum_later_does_not() {
        let mut s = Schedule::new();
        s.set(EventKind::Ppu, 500);
        s.set(EventKind::Dma, 300);
        assert_eq!(s.next(), 300);
        // A later deadline of another kind leaves the minimum alone.
        s.set(EventKind::Timer, 600);
        assert_eq!(s.next(), 300);
        s.set(EventKind::Input, NEVER);
        assert_eq!(s.next(), 300);
    }

    #[test]
    fn equal_deadlines_keep_the_minimum() {
        let mut s = Schedule::new();
        s.set(EventKind::Timer, 100);
        s.set(EventKind::Ppu, 100);
        // One of two holders moves later: the other still holds it.
        s.set(EventKind::Ppu, 200);
        s.set(EventKind::Ppu, 100);
        assert_eq!(s.next(), 100);
        // Taking one of two due entries leaves the other due.
        assert_eq!(s.take_due(100), Some(EventKind::Timer));
        assert_eq!(s.next(), 100);
        assert_eq!(s.take_due(100), Some(EventKind::Ppu));
        assert_eq!(s.next(), NEVER);
    }

    #[test]
    fn taking_or_clearing_the_last_deadline_leaves_never() {
        let mut s = Schedule::new();
        s.set(EventKind::Timer, 10);
        assert_eq!(s.take_due(10), Some(EventKind::Timer));
        assert_eq!(s.next(), NEVER);
        assert_eq!(s.take_due(NEVER - 1), None);
        s.set(EventKind::Serial, 20);
        s.set(EventKind::Serial, NEVER);
        assert_eq!(s.next(), NEVER);
        assert_eq!(s.take_due(NEVER - 1), None);
    }

    #[test]
    fn handler_rearming_at_or_before_now_runs_again() {
        let mut s = Schedule::new();
        s.set(EventKind::Timer, 10);
        s.set(EventKind::Ppu, 30);
        assert_eq!(s.take_due(10), Some(EventKind::Timer));
        s.set(EventKind::Timer, 10);
        assert_eq!(s.take_due(10), Some(EventKind::Timer));
        s.set(EventKind::Timer, 6);
        assert_eq!(s.take_due(10), Some(EventKind::Timer));
        s.set(EventKind::Timer, 14);
        assert_eq!(s.take_due(10), None);
        assert_eq!(s.next(), 14);
    }

    #[test]
    fn minimum_matches_a_full_scan_under_random_operations() {
        // A fixed LCG: deterministic, no host randomness.
        let mut x: u64 = 0x2545_F491_4F6C_DD1D;
        let mut rand = move |n: u64| {
            x = x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (x >> 33) % n
        };
        let mut s = Schedule::new();
        let mut now = 0;
        for _ in 0..20_000 {
            match rand(4) {
                0 => now += rand(8),
                1 => {
                    let k = EventKind::ALL[rand(7) as usize];
                    let at = if rand(5) == 0 { NEVER } else { now + rand(16) };
                    s.set(k, at);
                }
                _ => {
                    let before = s.at;
                    let want = before.iter().position(|&t| t <= now);
                    assert_eq!(s.take_due(now).map(|k| k as usize), want);
                }
            }
            assert_eq!(s.next(), s.at.iter().fold(NEVER, |m, &t| m.min(t)));
            let slow = &s.at[EventKind::HOT..];
            assert_eq!(s.slow, slow.iter().fold(NEVER, |m, &t| m.min(t)));
        }
    }
}
