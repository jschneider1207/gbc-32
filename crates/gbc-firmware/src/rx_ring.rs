//! The reader's side of a receive ring that a circular DMA stream fills
//! (the board's link UART, `gbc-nucleo-h753/src/link_uart.rs`): which
//! bytes are new since the last poll, and whether the stream may have
//! lapped the reader (M7 review C5). Pure arithmetic, so it is tested on
//! the host; the board reads the registers and passes them in.
//!
//! **Position.** The stream's NDTR counts down from the ring length and
//! reloads after reaching 0 (circular mode, RM0433 15.3.10 and 15.5.6),
//! so the next byte goes to index `len - NDTR`.
//!
//! **Laps.** The position alone cannot tell `k` bytes from `k + len`. The
//! stream's two sticky flags can, to the degree this module needs: HTIF is
//! set when NDTR reaches `len / 2` (index `len / 2 - 1` written) and TCIF
//! when it reaches 0 (index `len - 1` written), on every lap, whether or
//! not their interrupts are enabled (RM0433 15.3.19, the paragraph after
//! step 10; 15.4 Table 116; 15.5.1). The board reads and clears them at
//! each poll, together with NDTR (see `link_uart.rs` for the ordering), so
//! each poll sees exactly the marks crossed since the previous one. Then,
//! with `T` the bytes the stream wrote between two polls:
//! - `T >= len`: every index was written, so **both** marks were crossed;
//! - `T <= len / 2`: the two marks are `len / 2` bytes apart, so **at most
//!   one** was crossed.
//!
//! [`RxRing::poll`] counts a poll that saw both marks. That counts every
//! lap (unread bytes overwritten; `T == len` also loses the whole ring,
//! since a full ring reads as empty), never a poll with at most half a
//! ring, and sometimes one with `len / 2 < T < len` (no loss, but more than
//! half the ring in one poll, already outside the link's design margin:
//! [`RxRing::high`] shows such a poll as `> len / 2`). An interrupt-driven
//! lap counter would be exact but needs an ISR, its vector in every image
//! and an NVIC priority, for a condition that should never happen; the
//! flags cost two register accesses per poll.
//!
//! **Stream errors** (comprehensive review K11). A bus error during a
//! transfer sets the stream's TEIF and the hardware clears its EN, as does
//! a FIFO threshold/burst mismatch (RM0433 15.3.20); NDTR then freezes and
//! no byte arrives again: the link would be dead with nothing counted. A
//! FIFO overrun (FEIF) or direct mode error (DMEIF) leaves the stream
//! running and loses no data (same section), so they are cleared, not
//! counted. After each poll the board passes in whether the stream was
//! enabled before the poll's snapshot and whether TEIF was set;
//! [`RxRing::check_stream`] counts a stopped stream and moves the reader to
//! index 0, where the board re-arms the stream (15.3.19).

/// What one [`RxRing::poll`] found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Poll {
    /// Ring index of the first new byte.
    pub start: usize,
    /// New bytes, at `start`, `start + 1`, ... modulo the ring length.
    pub avail: usize,
    /// Both marks were crossed since the last poll: the stream may have
    /// lapped the reader (module docs); `avail` bytes are still the newest
    /// ones, but older ones were lost.
    pub lapped: bool,
}

/// The reader's position and counters (module docs).
#[derive(Clone, Debug)]
pub struct RxRing {
    len: usize,
    pos: usize,
    high: usize,
    laps: u32,
    restarts: u32,
}

impl RxRing {
    /// A reader at index 0 of a `len`-byte ring (the stream starts there).
    ///
    /// # Panics
    /// If `len` is odd or less than 2 (HTIF needs a half).
    pub const fn new(len: usize) -> RxRing {
        assert!(
            len >= 2 && len.is_multiple_of(2),
            "ring length must be even, >= 2"
        );
        RxRing {
            len,
            pos: 0,
            high: 0,
            laps: 0,
            restarts: 0,
        }
    }

    /// Takes what the stream has written since the last poll: `ndtr` is
    /// the stream's remaining count, `half` and `full` whether HTIF and
    /// TCIF were set (they are then cleared) since the last poll. Moves
    /// the reader to the stream's position.
    pub fn poll(&mut self, ndtr: usize, half: bool, full: bool) -> Poll {
        let write = (self.len - ndtr.min(self.len)) % self.len;
        let avail = (write + self.len - self.pos) % self.len;
        let start = self.pos;
        let lapped = half && full;
        self.pos = write;
        self.high = self.high.max(avail);
        self.laps = self.laps.saturating_add(u32::from(lapped));
        Poll {
            start,
            avail,
            lapped,
        }
    }

    /// The most bytes one poll found waiting.
    pub fn high(&self) -> usize {
        self.high
    }

    /// Polls that found the ring possibly lapped ([`Poll::lapped`]).
    pub fn laps(&self) -> u32 {
        self.laps
    }

    /// After a [`RxRing::poll`] (which took the bytes the stream wrote
    /// before it stopped): whether the stream stopped by itself, `enabled`
    /// being its EN read before the poll's snapshot and `teif` its
    /// transfer error flag from that snapshot (module docs, "Stream
    /// errors"). If so, counts it and moves the reader to index 0, and the
    /// caller must re-arm the stream with its full length (it then writes
    /// index 0 first).
    pub fn check_stream(&mut self, enabled: bool, teif: bool) -> bool {
        let stopped = teif || !enabled;
        if stopped {
            self.pos = 0;
            self.restarts = self.restarts.saturating_add(1);
        }
        stopped
    }

    /// Times [`RxRing::check_stream`] found the stream stopped (a DMA
    /// transfer error); 0 means the stream never stopped.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 16;

    /// A model of the DMA stream: `total` bytes written since enable, the
    /// sticky HTIF/TCIF as the hardware sets them (RM0433 15.3.19).
    struct Stream {
        total: usize,
        ht: bool,
        tc: bool,
    }

    impl Stream {
        fn write(&mut self, bytes: usize) {
            for _ in 0..bytes {
                self.total += 1;
                match self.total % N {
                    x if x == N / 2 => self.ht = true,
                    0 => self.tc = true,
                    _ => {}
                }
            }
        }
        fn ndtr(&self) -> usize {
            // Reloaded to N the moment it reaches 0.
            N - self.total % N
        }
        /// The board's read-and-clear.
        fn poll(&mut self, r: &mut RxRing) -> Poll {
            let p = r.poll(self.ndtr(), self.ht, self.tc);
            self.ht = false;
            self.tc = false;
            p
        }
    }

    #[test]
    fn position_and_wrap() {
        let mut r = RxRing::new(N);
        let mut s = Stream {
            total: 0,
            ht: false,
            tc: false,
        };
        assert_eq!(
            s.poll(&mut r),
            Poll {
                start: 0,
                avail: 0,
                lapped: false
            }
        );
        s.write(5);
        assert_eq!(
            s.poll(&mut r),
            Poll {
                start: 0,
                avail: 5,
                lapped: false
            }
        );
        // Across the end of the ring: 14 bytes from index 5 end at 3.
        s.write(14);
        assert_eq!(
            s.poll(&mut r),
            Poll {
                start: 5,
                avail: 14,
                lapped: true
            }
        );
        s.write(1);
        assert_eq!(
            s.poll(&mut r),
            Poll {
                start: 3,
                avail: 1,
                lapped: false
            }
        );
        assert_eq!(r.high(), 14);
        assert_eq!(r.laps(), 1);
    }

    #[test]
    fn ndtr_zero_reads_as_index_zero() {
        // A read the instant NDTR is 0 (before the reload) is the end of
        // the ring, index 0.
        let mut r = RxRing::new(N);
        let p = r.poll(0, true, true);
        assert_eq!((p.start, p.avail), (0, 0));
        let p = r.poll(N - 3, false, false);
        assert_eq!((p.start, p.avail), (0, 3));
    }

    /// Every lap is counted, no poll with at most half a ring is, from
    /// every start index and every gap up to three laps.
    #[test]
    fn counts_every_lap_and_nothing_within_half() {
        for start in 0..N {
            for t in 0..3 * N {
                let mut r = RxRing::new(N);
                let mut s = Stream {
                    total: 0,
                    ht: false,
                    tc: false,
                };
                s.write(start);
                s.poll(&mut r);
                s.write(t);
                let p = s.poll(&mut r);
                assert_eq!(p.start, start % N);
                assert_eq!(p.avail, t % N, "start {start} t {t}");
                if t >= N {
                    assert!(p.lapped, "lap missed: start {start} t {t}");
                }
                if t <= N / 2 {
                    assert!(!p.lapped, "false lap: start {start} t {t}");
                }
                assert_eq!(r.laps(), u32::from(p.lapped));
            }
        }
    }

    /// Steady polling well inside half a ring never counts, over many laps.
    #[test]
    fn steady_reader_never_counts() {
        let mut r = RxRing::new(N);
        let mut s = Stream {
            total: 0,
            ht: false,
            tc: false,
        };
        let mut got = 0;
        for i in 0..10 * N {
            s.write(1 + i % (N / 2));
            got += s.poll(&mut r).avail;
        }
        assert_eq!(got, s.total);
        assert_eq!(r.laps(), 0);
        assert_eq!(r.high(), N / 2);
    }

    /// A running stream is never restarted; a stopped one (EN cleared, or
    /// TEIF) is counted once per check, and the reader then follows a
    /// stream re-armed at index 0.
    #[test]
    fn stopped_stream_restarts_at_zero() {
        let mut r = RxRing::new(N);
        let mut s = Stream {
            total: 0,
            ht: false,
            tc: false,
        };
        s.write(5);
        assert_eq!(s.poll(&mut r).avail, 5);
        assert!(!r.check_stream(true, false));
        assert_eq!(r.restarts(), 0);
        // The stream stops after 3 more bytes (frozen NDTR): the poll
        // still takes them, then the check restarts.
        s.write(3);
        let p = s.poll(&mut r);
        assert_eq!((p.start, p.avail), (5, 3));
        assert!(r.check_stream(false, true));
        assert_eq!(r.restarts(), 1);
        // Re-armed: a fresh stream from index 0.
        let mut s = Stream {
            total: 0,
            ht: false,
            tc: false,
        };
        assert_eq!(s.poll(&mut r).avail, 0);
        s.write(4);
        let p = s.poll(&mut r);
        assert_eq!((p.start, p.avail, p.lapped), (0, 4, false));
        // TEIF with EN still read set (the error after the EN read) and EN
        // clear without TEIF each count.
        assert!(r.check_stream(true, true));
        assert!(r.check_stream(false, false));
        assert_eq!(r.restarts(), 3);
        assert_eq!(r.laps(), 0);
    }

    #[test]
    #[should_panic(expected = "ring length")]
    fn odd_length_refused() {
        let _ = RxRing::new(15);
    }
}
