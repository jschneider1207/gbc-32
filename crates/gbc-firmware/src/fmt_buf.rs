//! Bounded text buffers for [`core::fmt::Write`] (review Q9): one copy of
//! the append the board images used to repeat in each bin. Two overflow
//! rules, because the images need both:
//!
//! - **truncate** ([`append_truncated`], [`FmtBuf`]): keep the bytes that
//!   fit, drop the rest, and never fail the write. For status lines and
//!   kept logs, where a cut line is better than none. The cut is at a byte,
//!   so it may split a multi-byte UTF-8 character (the board's lines are
//!   ASCII).
//! - **whole** ([`append_whole`]): a string that does not fit in full is
//!   not written at all, and the caller fails the write. For a run's
//!   captured lines, where a partial line must not look like a complete
//!   one (the capture's `overflow` flag).

use core::fmt;

/// Appends as much of `s` as fits in `buf[*len..]` and advances `len`.
/// Returns whether all of `s` fit. A `len` at or past the end counts as
/// full and is left as it is.
pub fn append_truncated(buf: &mut [u8], len: &mut usize, s: &str) -> bool {
    let Some(room) = buf.len().checked_sub(*len) else {
        return s.is_empty();
    };
    let start = *len;
    let n = s.len().min(room);
    buf[start..start + n].copy_from_slice(&s.as_bytes()[..n]);
    *len = start + n;
    n == s.len()
}

/// Appends all of `s` to `buf[*len..]` and advances `len`, or, if it does
/// not fit in full, leaves both unchanged and returns false.
pub fn append_whole(buf: &mut [u8], len: &mut usize, s: &str) -> bool {
    let Some(dst) = len
        .checked_add(s.len())
        .and_then(|end| buf.get_mut(*len..end))
    else {
        return false;
    };
    dst.copy_from_slice(s.as_bytes());
    *len += s.len();
    true
}

/// Text built in memory, at most `N` bytes, that truncates
/// ([`append_truncated`]): writes never fail; [`FmtBuf::truncated`] tells
/// whether any byte was dropped.
///
/// [`FmtBuf::reserving_nul`] keeps the last byte free, so a text line
/// can end in the 0x00 of the link protocol ([`FmtBuf::push_nul`]) even
/// when the text filled the buffer.
#[derive(Clone)]
pub struct FmtBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
    /// How many bytes text may fill: `N`, or `N - 1` with the NUL reserve.
    cap: usize,
    truncated: bool,
}

impl<const N: usize> Default for FmtBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> FmtBuf<N> {
    /// Empty; text may fill all `N` bytes.
    pub const fn new() -> Self {
        FmtBuf {
            buf: [0; N],
            len: 0,
            cap: N,
            truncated: false,
        }
    }

    /// Empty; text may fill `N - 1` bytes, the last kept for
    /// [`FmtBuf::push_nul`].
    pub const fn reserving_nul() -> Self {
        FmtBuf {
            buf: [0; N],
            len: 0,
            cap: N.saturating_sub(1),
            truncated: false,
        }
    }

    /// What was written (and kept).
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether a write since the last [`FmtBuf::clear`] dropped bytes.
    pub fn truncated(&self) -> bool {
        self.truncated
    }

    /// Empties the buffer (and clears [`FmtBuf::truncated`]).
    pub fn clear(&mut self) {
        self.len = 0;
        self.truncated = false;
    }

    /// Appends one 0x00 byte, into the reserve if the text filled the rest.
    /// Returns false (and appends nothing) if all `N` bytes are used.
    pub fn push_nul(&mut self) -> bool {
        match self.buf.get_mut(self.len) {
            Some(b) => {
                *b = 0;
                self.len += 1;
                true
            }
            None => false,
        }
    }
}

impl<const N: usize> fmt::Write for FmtBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let cap = self.cap;
        if !append_truncated(&mut self.buf[..cap], &mut self.len, s) {
            self.truncated = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::fmt::Write;

    #[test]
    fn truncated_keeps_what_fits() {
        let mut buf = [0u8; 5];
        let mut len = 0;
        assert!(append_truncated(&mut buf, &mut len, "ab"));
        assert_eq!(len, 2);
        assert!(!append_truncated(&mut buf, &mut len, "cdef"));
        assert_eq!((len, &buf), (5, b"abcde"));
        // Full: nothing more, still reported.
        assert!(!append_truncated(&mut buf, &mut len, "g"));
        assert_eq!(len, 5);
        // The empty string always fits.
        assert!(append_truncated(&mut buf, &mut len, ""));
    }

    #[test]
    fn truncated_len_past_end_is_full() {
        let mut buf = [0u8; 3];
        let mut len = 7;
        assert!(!append_truncated(&mut buf, &mut len, "x"));
        assert_eq!(len, 7);
    }

    #[test]
    fn whole_is_all_or_nothing() {
        let mut buf = [0u8; 5];
        let mut len = 0;
        assert!(append_whole(&mut buf, &mut len, "abc"));
        assert!(!append_whole(&mut buf, &mut len, "def"));
        assert_eq!((len, &buf[..3]), (3, &b"abc"[..]));
        assert_eq!(&buf[3..], [0, 0]);
        // Exactly to the end fits.
        assert!(append_whole(&mut buf, &mut len, "de"));
        assert_eq!((len, &buf), (5, b"abcde"));
        assert!(!append_whole(&mut buf, &mut len, "f"));
        assert!(append_whole(&mut buf, &mut len, ""));
        let mut huge = usize::MAX;
        assert!(!append_whole(&mut buf, &mut huge, "f"));
        assert_eq!(huge, usize::MAX);
    }

    #[test]
    fn fmtbuf_truncates_silently_and_says_so() {
        let mut t = FmtBuf::<4>::new();
        assert!(write!(t, "{}", 12).is_ok());
        assert!(!t.truncated());
        assert!(write!(t, "{}", 345).is_ok());
        assert_eq!(t.as_bytes(), b"1234");
        assert!(t.truncated());
        assert!(!t.push_nul());
        assert_eq!(t.len(), 4);
        t.clear();
        assert!(t.is_empty() && !t.truncated());
        let _ = t.write_str("xy");
        assert_eq!(t.as_bytes(), b"xy");
    }

    #[test]
    fn fmtbuf_nul_reserve() {
        let mut t = FmtBuf::<4>::reserving_nul();
        let _ = t.write_str("abcdef");
        assert_eq!(t.as_bytes(), b"abc");
        assert!(t.truncated());
        assert!(t.push_nul());
        assert_eq!(t.as_bytes(), b"abc\0");
        // The reserve is used; text and a second NUL get no room.
        let _ = t.write_str("d");
        assert!(!t.push_nul());
        assert_eq!(t.as_bytes(), b"abc\0");
        // A short text takes its NUL right after it.
        let mut s = FmtBuf::<4>::reserving_nul();
        let _ = s.write_str("a");
        assert!(s.push_nul());
        assert_eq!(s.as_bytes(), b"a\0");
    }
}
