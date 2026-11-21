//! OAM DMA (FF46): the transfer's state and the CGB bus-conflict rules.
//! Notes and sources: `docs/reference/oam-dma.md`.
//!
//! Progress is lazy (ADR 0011, "OAM DMA and scheduled inputs"): nothing
//! runs per M-cycle. The bytes due by a clock follow from `start`, and
//! `Ctx::dma_sync` copies them before anything can observe or change them
//! (a CPU write during the transfer, the end of a run call, the `Dma`
//! deadline at `end`). The CPU's `read`/`write` take a separate path while
//! `Hot::dma` is set (`Ctx::read_during_dma`), so the conflict rules cost
//! the normal path one flag test.

use crate::schedule::NEVER;

/// Bytes per transfer: one per M-cycle.
pub(crate) const LEN: u8 = 0xA0;

/// The transfer started by the last FF46 write.
#[derive(Clone, Copy)]
pub(crate) struct OamDma {
    /// The source page (FF46 << 8).
    pub(crate) base: u16,
    /// The clock of the CPU M-cycle in which byte 0 is transferred: two
    /// M-cycles after the start of the FF46 write's M-cycle.
    pub(crate) start: u64,
    /// CPU reads of FE00-FEFF return FF from this clock to `end`: `start`,
    /// or the FF46 write's clock when it restarted a running transfer.
    pub(crate) block_from: u64,
    /// The first clock after the transfer (`start` + 160 M-cycles), the
    /// `Dma` deadline.
    pub(crate) end: u64,
    /// Bytes written to OAM so far.
    pub(crate) copied: u8,
    /// While the CPU is halted: the clock the transfer paused at (it makes
    /// no progress past it), else `NEVER`. See `Ctx::dma_pause`.
    pub(crate) paused: u64,
}

impl OamDma {
    pub(crate) const fn new() -> Self {
        Self {
            base: 0,
            start: 0,
            block_from: 0,
            end: 0,
            copied: 0,
            paused: NEVER,
        }
    }

    /// The number of bytes transferred by the CPU access at `now` (an
    /// M-cycle boundary): byte `i` is read and written during the M-cycle
    /// before the CPU access at `start + i * mcycle` (SameBoy
    /// `GB_dma_run`, half an M-cycle earlier), so it counts from that
    /// access on.
    pub(crate) fn due(&self, now: u64, mcycle: u64) -> u8 {
        if now < self.start {
            return 0;
        }
        // `mcycle` is 2 or 4: a shift, not a u64 division (a libcall on
        // thumbv7em).
        debug_assert!(mcycle == 2 || mcycle == 4);
        (((now - self.start) >> mcycle.trailing_zeros()) + 1).min(u64::from(LEN)) as u8
    }
}

/// What a CPU access to `addr` (below FE00) does while the DMA reads
/// `cur` from the source page `base` (SameBoy `is_addr_in_dma_use` and
/// the CGB branches of `GB_read_memory`/`GB_write_memory`; CGB C only,
/// ADR 0003).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Conflict {
    /// The access is unaffected.
    None,
    /// Reads FF, writes are dropped (an external-bus access while the DMA
    /// reads E000-FFFF).
    Open,
    /// The access goes to this WRAM address instead: the CPU's low 12 bits
    /// with bit 12 (the bank half) from the DMA's address.
    Wram(u16),
    /// The access sees the DMA's own address `cur`: a read returns the
    /// byte the DMA reads; a write is dropped, and on CGB C clears the OAM
    /// byte just written if `cur` is below A000.
    Dma,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Bus {
    /// ROM, cartridge RAM (and on CGB nothing else).
    External,
    Vram,
    /// C000-FFFF on CGB.
    Wram,
}

fn bus(addr: u16) -> Bus {
    match addr {
        0x8000..=0x9FFF => Bus::Vram,
        0xC000..=0xFFFF => Bus::Wram,
        _ => Bus::External,
    }
}

/// See [`Conflict`]. `cur` is the byte being transferred in the access's
/// M-cycle (`base + i`); `addr` is below FE00.
pub(crate) fn conflict(base: u16, cur: u16, addr: u16) -> Conflict {
    let next = cur.wrapping_add(1);
    let high = base >= 0xE000;
    // SameBoy: "Shortcut for DMA access flow".
    if addr == next || (high && next & !0x2000 == addr) {
        return Conflict::None;
    }
    let (src, cpu) = (bus(base), bus(addr));
    let conflicts = if cpu == Bus::Wram {
        src != Bus::Vram
    } else if high {
        cpu != Bus::Vram
    } else {
        cpu == src
    };
    if !conflicts {
        Conflict::None
    } else if high && cpu == Bus::External {
        Conflict::Open
    } else if cpu == Bus::Wram && (src != Bus::Wram || high) {
        Conflict::Wram((base & 0x1000) | (addr & 0x0FFF) | 0xC000)
    } else {
        Conflict::Dma
    }
}

#[cfg(test)]
mod tests {
    use super::{Conflict, LEN, OamDma, conflict};

    #[test]
    fn due_counts_from_start_and_stops_at_160() {
        let d = OamDma {
            start: 100,
            ..OamDma::new()
        };
        assert_eq!(d.due(96, 4), 0);
        assert_eq!(d.due(99, 4), 0);
        assert_eq!(d.due(100, 4), 1);
        assert_eq!(d.due(104, 4), 2);
        assert_eq!(d.due(100 + 159 * 4, 4), LEN);
        assert_eq!(d.due(100 + 400 * 4, 4), LEN);
        assert_eq!(d.due(102, 2), 2);
    }

    #[test]
    fn cgb_conflicts_by_source_bus() {
        use Conflict::*;
        // ROM source: the external bus conflicts, VRAM doesn't, WRAM is
        // redirected by the DMA's bit 12 (0x4xxx: bit 12 clear).
        assert_eq!(conflict(0x4000, 0x4010, 0x0123), Dma);
        assert_eq!(conflict(0x4000, 0x4010, 0xA123), Dma);
        assert_eq!(conflict(0x4000, 0x4010, 0x8123), None);
        assert_eq!(conflict(0x4000, 0x4010, 0xD123), Wram(0xC123));
        assert_eq!(conflict(0x5000, 0x5010, 0xC123), Wram(0xD123));
        // The address the DMA reads next doesn't conflict.
        assert_eq!(conflict(0x4000, 0x4010, 0x4011), None);
        // VRAM source: only VRAM conflicts.
        assert_eq!(conflict(0x8000, 0x8010, 0x9000), Dma);
        assert_eq!(conflict(0x8000, 0x8010, 0x0100), None);
        assert_eq!(conflict(0x8000, 0x8010, 0xC100), None);
        // WRAM source: WRAM conflicts with the DMA's address, the rest
        // doesn't.
        assert_eq!(conflict(0xC100, 0xC110, 0xD000), Dma);
        assert_eq!(conflict(0xC100, 0xC110, 0xE000), Dma);
        assert_eq!(conflict(0xC100, 0xC110, 0x0100), None);
        assert_eq!(conflict(0xC100, 0xC110, 0x8100), None);
        // E000-FFFF source: the external bus is open, WRAM is redirected,
        // VRAM is free.
        assert_eq!(conflict(0xFE00, 0xFE10, 0x0100), Open);
        assert_eq!(conflict(0xFE00, 0xFE10, 0xA100), Open);
        assert_eq!(conflict(0xFE00, 0xFE10, 0xC456), Wram(0xD456));
        assert_eq!(conflict(0xE000, 0xE010, 0xD456), Wram(0xC456));
        assert_eq!(conflict(0xFE00, 0xFE10, 0x8100), None);
        assert_eq!(conflict(0xE000, 0xE010, 0xC011), None);
    }
}
