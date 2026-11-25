//! The pipeline stalls of one line's mode 3: a bounded list in pixel
//! order, and the one walk over it that the timing code shares.
//!
//! A stall holds the pixel pipeline for some dots before a pixel is
//! output: an object's fetch, or the window's start (Pan Docs: Rendering,
//! "Mode 3 length"). The list holds at most [`Stalls::MAX`] of them, which
//! the type keeps: every index it hands out is in bounds, and "no window
//! stall" is an `Option`, not an index that happens to be valid.

use super::objects::MAX_PER_LINE;

/// A pipeline stall: the pixel x it comes before, and its dots.
pub(crate) type Stall = (u8, u8);

/// What a stall is ([`Stalls::walk`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StallKind {
    /// The window's start.
    Window,
    /// The line's earlier window start, after a re-trigger
    /// (`Ppu::retrigger`).
    EarlierWindow,
    /// The fetch of the line's object at this position in fetch order
    /// (the object stalls are in fetch order: ascending X).
    Object(usize),
}

/// A stall with its place in the line ([`Stalls::timed`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Timed {
    /// Its index in the list.
    pub(crate) index: usize,
    /// The pixel it comes before.
    pub(crate) x: u8,
    /// The line position it begins at: the pixels before `x` and the
    /// earlier stalls are out.
    pub(crate) begin: u32,
    pub(crate) dots: u8,
}

impl Timed {
    /// The line position at which pixel `x` is output: the stall is over.
    #[inline]
    pub(crate) fn end(&self) -> u32 {
        self.begin + u32::from(self.dots)
    }
}

/// The stalls of a line in x order: the objects' (fetch order) and the
/// window start's, which comes before the objects at its x. After a
/// re-trigger the earlier window start keeps its stall, so with two
/// window starts the list has room for 9 objects.
///
/// INVARIANT: `len <= MAX`. Only the methods here write the fields, and
/// every read goes through [`Self::as_slice`] or [`Self::window_index`],
/// which check them.
#[derive(Clone, Copy)]
pub(crate) struct Stalls {
    list: [Stall; Self::MAX],
    len: u8,
    /// Which stall is the window's; any value not below `len` (`NO_WIN`):
    /// none. A byte beside `len`, not an `Option<u8>`: `clear`, which runs
    /// on every line without a stall, is then one 16-bit store (measured:
    /// the `Option` cost 1 x86 Ir per line, up to +0.03% per bench).
    win: u8,
}

impl Stalls {
    /// 10 objects and the window start.
    pub(crate) const MAX: usize = MAX_PER_LINE + 1;

    const NO_WIN: u8 = u8::MAX;

    pub(crate) const NONE: Self = Self {
        list: [(0, 0); Self::MAX],
        len: 0,
        win: Self::NO_WIN,
    };

    /// No stall (most lines).
    #[inline]
    pub(crate) fn clear(&mut self) {
        self.len = 0;
        self.win = Self::NO_WIN;
    }

    /// The stalls, in x order.
    #[inline]
    pub(crate) fn as_slice(&self) -> &[Stall] {
        // In bounds by the invariant; no panic path either way.
        self.list.get(..usize::from(self.len)).unwrap_or(&[])
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// No room for another stall.
    #[inline]
    pub(crate) fn is_full(&self) -> bool {
        usize::from(self.len) >= Self::MAX
    }

    /// Stall `i`, if there is one.
    #[inline]
    pub(crate) fn get(&self, i: usize) -> Option<Stall> {
        self.as_slice().get(i).copied()
    }

    /// The last stall (the largest x).
    #[inline]
    pub(crate) fn last(&self) -> Option<Stall> {
        self.as_slice().last().copied()
    }

    /// The index of the window's stall.
    #[inline]
    pub(crate) fn window_index(&self) -> Option<usize> {
        (self.win < self.len).then_some(usize::from(self.win))
    }

    /// The window's stall.
    #[inline]
    pub(crate) fn window(&self) -> Option<Stall> {
        self.get(self.window_index()?)
    }

    /// The window's stall, to cut it short (`Ppu::window_write`).
    #[inline]
    pub(crate) fn window_mut(&mut self) -> Option<&mut Stall> {
        let i = self.window_index()?;
        self.list.get_mut(..usize::from(self.len))?.get_mut(i)
    }

    /// Stall `i` takes `dots` more (`Ppu::unshare_junk_tile`).
    pub(crate) fn add_dots(&mut self, i: usize, dots: u8) {
        let list = self.list.get_mut(..usize::from(self.len));
        if let Some(s) = list.and_then(|l| l.get_mut(i)) {
            s.1 += dots;
        }
    }

    /// Replaces the list with the object stalls that `write` writes, in
    /// x order from the first entry, returning how many: no window stall.
    /// (Written in place: the list is rebuilt on every line with objects.)
    #[inline]
    pub(crate) fn set_objects(&mut self, write: impl FnOnce(&mut [Stall; MAX_PER_LINE]) -> usize) {
        let (objs, _) = self.list.split_first_chunk_mut().unwrap();
        self.len = write(objs).min(MAX_PER_LINE) as u8;
        self.win = Self::NO_WIN;
    }

    /// Adds the window start `w`, which becomes the window's stall: before
    /// the first stall at or after its x (after all of them if none is).
    /// The list is in x order, so the ones at or after it shift up.
    /// Returns whether there was room.
    #[inline]
    pub(crate) fn insert_window(&mut self, w: Stall) -> bool {
        let mut i = usize::from(self.len);
        if i >= Self::MAX {
            return false;
        }
        while i > 0 && self.list[i - 1].0 >= w.0 {
            self.list[i] = self.list[i - 1];
            i -= 1;
        }
        self.list[i] = w;
        self.win = i as u8;
        self.len += 1;
        true
    }

    /// The window's stall becomes an earlier window start's, which ran as
    /// `ran` (`None`: as it is): the list has no window stall after this.
    pub(crate) fn window_ran(&mut self, ran: Option<Stall>) {
        if let (Some(w), Some(ran)) = (self.window_mut(), ran) {
            *w = ran;
        }
        self.win = Self::NO_WIN;
    }

    /// With this list built for the window start it holds and `earlier`
    /// built for an earlier one (same objects; [`Self::window_ran`]): the
    /// stalls before this list's window start come from `earlier` (its
    /// objects before that start and its own window stall, one entry
    /// more), then this list's from its window start on. Returns whether
    /// that was done: this list has a window stall and room for one more.
    pub(crate) fn splice_earlier(&mut self, earlier: &Self) -> bool {
        let (Some(j), n) = (self.window_index(), usize::from(self.len)) else {
            return false;
        };
        if j >= n || n >= Self::MAX {
            return false;
        }
        self.list.copy_within(j..n, j + 1);
        self.list[..=j].copy_from_slice(&earlier.list[..=j]);
        self.len += 1;
        self.win = j as u8 + 1;
        true
    }

    /// The dots of all the stalls.
    pub(crate) fn total_dots(&self) -> u16 {
        self.as_slice().iter().map(|s| u16::from(s.1)).sum()
    }

    /// The stalls with the line position each begins at, the first pixel
    /// being output at `first_pixel` with no stall: pixel `x` leaves the
    /// pipeline at `first_pixel + x` plus the dots of the stalls at or
    /// before it.
    #[inline]
    pub(crate) fn timed(&self, first_pixel: u16) -> TimedIter<'_> {
        TimedIter {
            stalls: self.as_slice().iter(),
            index: 0,
            clock: u32::from(first_pixel),
            x: 0,
        }
    }

    /// [`Self::timed`] with what each stall is; `earlier` is the index of
    /// the earlier window start's stall (`LineTiming::earlier_window`).
    /// Cold callers only (mid-mode-3 writes, OAM DMA lines).
    pub(crate) fn walk(
        &self,
        first_pixel: u16,
        earlier: Option<usize>,
    ) -> impl Iterator<Item = (StallKind, Timed)> + '_ {
        let win = self.window_index();
        self.timed(first_pixel).map(move |s| {
            let i = s.index;
            let kind = if win == Some(i) {
                StallKind::Window
            } else if earlier == Some(i) {
                StallKind::EarlierWindow
            } else {
                // Less the window stalls before it.
                let before = |w: Option<usize>| usize::from(w.is_some_and(|w| i > w));
                StallKind::Object(i - before(win) - before(earlier))
            };
            (kind, s)
        })
    }
}

/// See [`Stalls::timed`].
pub(crate) struct TimedIter<'a> {
    stalls: core::slice::Iter<'a, Stall>,
    index: usize,
    /// The line position and pixel after the stalls so far.
    clock: u32,
    x: u32,
}

impl Iterator for TimedIter<'_> {
    type Item = Timed;

    #[inline]
    fn next(&mut self) -> Option<Timed> {
        let &(x, dots) = self.stalls.next()?;
        // The pixels up to the stall's (none for a second stall at one x).
        self.clock += u32::from(x).saturating_sub(self.x);
        self.x = self.x.max(u32::from(x));
        let s = Timed {
            index: self.index,
            x,
            begin: self.clock,
            dots,
        };
        self.clock = s.end();
        self.index += 1;
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::vec::Vec;

    fn list(objects: &[Stall], window: Option<Stall>) -> Stalls {
        let mut s = Stalls::NONE;
        s.set_objects(|out| {
            out[..objects.len()].copy_from_slice(objects);
            objects.len()
        });
        if let Some(w) = window {
            assert!(s.insert_window(w));
        }
        s
    }

    #[test]
    fn window_goes_before_the_objects_at_its_x() {
        let s = list(&[(8, 6), (20, 9), (20, 6), (40, 6)], Some((20, 6)));
        assert_eq!(s.as_slice(), &[(8, 6), (20, 6), (20, 9), (20, 6), (40, 6)]);
        assert_eq!(s.window_index(), Some(1));
        assert_eq!(s.window(), Some((20, 6)));
        // After all of them when none is at or after it.
        let s = list(&[(8, 6)], Some((30, 6)));
        assert_eq!(s.as_slice(), &[(8, 6), (30, 6)]);
        assert_eq!(s.window_index(), Some(1));
        // No stall: no window stall, not "stall 2".
        assert_eq!(Stalls::NONE.window(), None);
        assert_eq!(Stalls::NONE.last(), None);
        assert!(Stalls::NONE.is_empty());
    }

    #[test]
    fn the_list_never_grows_past_its_room() {
        let ten = [(5, 6); MAX_PER_LINE];
        let mut s = list(&ten, Some((0, 6)));
        assert!(s.is_full());
        assert_eq!(s.as_slice().len(), Stalls::MAX);
        // A second window start does not fit (`Ppu::retrigger` asks first).
        assert!(!s.insert_window((3, 6)));
        assert_eq!(s.as_slice().len(), Stalls::MAX);
        assert_eq!(s.window_index(), Some(0));
        let earlier = s;
        assert!(!s.splice_earlier(&earlier));
        // A count past the room is cut.
        s.set_objects(|_| 200);
        assert_eq!(s.as_slice().len(), MAX_PER_LINE);
    }

    #[test]
    fn walk_times_and_names_each_stall() {
        // First pixel at 96. Object at pixel 8 (6 dots), the earlier
        // window start at 20 (6, cut to 2 as it ran), the window at 40,
        // an object at 40 (9) and one at 100 (6).
        let objects = [(8, 6), (40, 9), (100, 6)];
        let mut earlier = list(&objects, Some((20, 6)));
        earlier.window_ran(Some((20, 2)));
        assert_eq!(earlier.window_index(), None);
        let mut s = list(&objects, Some((40, 6)));
        assert!(s.splice_earlier(&earlier));
        assert_eq!(s.as_slice(), &[(8, 6), (20, 2), (40, 6), (40, 9), (100, 6)]);
        assert_eq!(s.window_index(), Some(2));
        assert_eq!(s.total_dots(), 29);
        let got: Vec<_> = s
            .walk(96, Some(1))
            .map(|(k, t)| (k, t.x, t.begin, t.end()))
            .collect();
        assert_eq!(
            got,
            [
                (StallKind::Object(0), 8, 104, 110),
                (StallKind::EarlierWindow, 20, 122, 124),
                (StallKind::Window, 40, 144, 150),
                (StallKind::Object(1), 40, 150, 159),
                (StallKind::Object(2), 100, 219, 225),
            ]
        );
        // No earlier start: the same list is objects around one window.
        let kinds: Vec<_> = s.walk(96, None).map(|(k, _)| k).collect();
        assert_eq!(
            kinds,
            [
                StallKind::Object(0),
                StallKind::Object(1),
                StallKind::Window,
                StallKind::Object(2),
                StallKind::Object(3),
            ]
        );
    }
}
