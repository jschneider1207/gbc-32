//! The caller's ROM, read one byte at a time (ADR 0026).
//!
//! On the Stage 3 MCU the ROM sits in FMC SDRAM, and the STM32H735 does
//! not support unaligned reads from the FMC (ES0491 2.7.3: "a half-word
//! read access starting at odd address"). The Cortex-M7 itself allows
//! unaligned `ldrh`/`ldr`, so LLVM may merge two byte reads at `i` and
//! `i + 1` into one halfword load, or copy a run of bytes with word loads,
//! and nothing guarantees the address is aligned. Every ROM read in the
//! core therefore goes through [`Rom::get`]: the field is private, so the
//! slice can't be indexed anywhere else, and each read is one byte load
//! with a compiler fence after it, so no two of them can be merged.
//! `scripts/check-rom-loads.sh` checks the MCU build's code for this.

use core::sync::atomic::{Ordering, compiler_fence};

/// The caller's ROM. Read it only through [`Rom::get`].
#[derive(Clone, Copy)]
pub(crate) struct Rom<'a>(&'a [u8]);

impl<'a> Rom<'a> {
    /// No ROM.
    pub(crate) const EMPTY: Self = Self(&[]);

    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// The caller's slice, to give back on eject or in a load error.
    /// Never read ROM bytes through it.
    pub(crate) const fn into_inner(self) -> &'a [u8] {
        self.0
    }

    pub(crate) const fn len(self) -> usize {
        self.0.len()
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0.is_empty()
    }

    /// Bytes `start..end`, or `None` if that isn't inside the ROM.
    pub(crate) fn sub(self, start: usize, end: usize) -> Option<Self> {
        self.0.get(start..end).map(Self)
    }

    /// Byte `i`, or `None` past the end.
    ///
    /// The only read of ROM bytes in the core. The fence is a compiler
    /// barrier only (no instruction): LLVM doesn't move or merge memory
    /// accesses across it, so two reads of adjacent bytes stay two byte
    /// loads. `check-rom-loads.sh` finds this load by the marker comment
    /// on its line; keep the marker on the line that reads the slice.
    #[inline(always)]
    pub(crate) fn get(self, i: usize) -> Option<u8> {
        if i >= self.0.len() {
            return None;
        }
        // Plain indexing (not `slice::get`), so the debug line of the load
        // is this one and not one in `core`; the bounds check folds into
        // the compare above.
        let v = self.0[i]; // check-rom-loads: the ROM load
        compiler_fence(Ordering::SeqCst);
        Some(v)
    }
}
