//! The event table: a small fixed array of deadlines, one per kind of
//! scheduled work, and their minimum (ADR 0011, "The event table").
//!
//! The tick never looks at this table. It only counts down to the earliest
//! deadline (or the run target), and `Ctx::boundary` then runs the due
//! kinds in [`EventKind`] order. Deadlines change only when the work they
//! stand for is rescheduled (a PPU sync, a register write), so the minimum
//! is recomputed there and never per M-cycle.

/// No deadline.
pub(crate) const NEVER: u64 = u64::MAX;

/// A kind of scheduled work. The declaration order is the dispatch order
/// for deadlines on the same clock (ADR 0011), so it is part of the
/// emulated behavior: don't reorder.
///
/// Only `Ppu` is scheduled so far. The others are declared now to pin that
/// order and get their handlers with the work they stand for: `Timer` and
/// `Serial` in ADR 0011 step 4 (they still tick eagerly), `Dma` and
/// `Input` in step 6 (M2), `DivApu` and `AudioBuffer` in M6.
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
    /// A scheduled button change (`set_buttons_at`).
    Input,
    /// A DIV-APU frame-sequencer step.
    DivApu,
    /// The audio buffer boundary.
    AudioBuffer,
}

impl EventKind {
    /// Every kind, in dispatch (index) order.
    const ALL: [Self; 7] = [
        Self::Timer,
        Self::Serial,
        Self::Ppu,
        Self::Dma,
        Self::Input,
        Self::DivApu,
        Self::AudioBuffer,
    ];
    const COUNT: usize = Self::ALL.len();
}

/// One deadline (a clock, or [`NEVER`]) per [`EventKind`], and the
/// earliest of them.
#[derive(Clone, Copy)]
pub(crate) struct Schedule {
    at: [u64; EventKind::COUNT],
    /// The minimum of `at`.
    next: u64,
}

impl Schedule {
    /// Nothing scheduled.
    pub(crate) const fn new() -> Self {
        Self {
            at: [NEVER; EventKind::COUNT],
            next: NEVER,
        }
    }

    /// The earliest deadline, or [`NEVER`].
    #[inline]
    pub(crate) const fn next(&self) -> u64 {
        self.next
    }

    /// The deadline of `kind`, or [`NEVER`].
    #[cfg(test)]
    pub(crate) const fn deadline(&self, kind: EventKind) -> u64 {
        self.at[kind as usize]
    }

    /// Sets the deadline of `kind` (a clock, or [`NEVER`]). The caller
    /// reschedules the countdown if it is running (`Ctx::reschedule`).
    #[inline(never)]
    pub(crate) fn set(&mut self, kind: EventKind, at: u64) {
        self.at[kind as usize] = at;
        self.recompute();
    }

    fn recompute(&mut self) {
        self.next = self.at.iter().fold(NEVER, |m, &t| m.min(t));
    }

    /// The first kind, in [`EventKind`] order, whose deadline is at or
    /// before `now`, with its deadline cleared to [`NEVER`] (its handler
    /// sets the next one). `None` once nothing is due.
    pub(crate) fn take_due(&mut self, now: u64) -> Option<EventKind> {
        let i = self.at.iter().position(|&t| t <= now)?;
        self.at[i] = NEVER;
        self.recompute();
        Some(EventKind::ALL[i])
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
}
