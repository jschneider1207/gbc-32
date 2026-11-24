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

use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering::Relaxed;

use crate::schedule::NEVER;

/// The PPU sees each step of a transfer this many dots before the CPU
/// access it is due by, in both speeds (gambatte `oamdma/late_sp*`, cgb04c:
/// single speed allows 0-2, double speed 2-3; SameBoy: half an M-cycle).
pub(crate) const PPU_LEAD: u64 = 2;

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
    /// The PPU sees the transfer active at clocks in `(ppu_from, ppu_to]`
    /// ([`Self::ppu_at`]); `NEVER` while a halt freezes that edge.
    pub(crate) ppu_from: u64,
    pub(crate) ppu_to: u64,
    /// The last halt during the transfer, as the PPU sees it: clocks in
    /// `(halt_from, halt_to]` (`halt_to` is `NEVER` while halted).
    pub(crate) halt_from: u64,
    pub(crate) halt_to: u64,
    /// Bytes transferred when that halt began.
    pub(crate) halt_dest: u8,
    /// After a restart, until the new transfer's start: the byte the old
    /// transfer moves in the M-cycle after the FF46 write (the first of
    /// the new start delay), else [`LEN`]. That M-cycle's CPU access
    /// conflicts as if it were byte `tail` of the new transfer (gambatte
    /// cgb04c `oamdma_src8000_srcchange0000_busyinc`): meanwhile `start`
    /// and `copied` describe such a transfer (`Ctx::oam_dma`,
    /// `Ctx::dma_tail_end`).
    pub(crate) tail: u8,
    /// The M-cycle length (4, or 2 in double speed) the transfer runs at.
    pub(crate) mcycle: u8,
    /// Set by the FF46 write; cleared by the PPU at the first line start
    /// past `ppu_to` (`Ppu::dma_touches`), so lines after the transfer
    /// skip the DMA-aware path with one flag test.
    pub(crate) ppu_live: bool,
}

/// The OAM bytes cleared by conflicting CPU reads during a transfer from
/// VRAM and not yet applied to OAM: bit `i % 32` of word `i / 32` for byte
/// `i` (`bus::dma_read_clear` records, `Ctx::dma_sync` applies; ADR 0028
/// amendment). Interior mutability, because the CPU read path takes shared
/// references and stays that way; atomics (relaxed loads and stores, plain
/// `ldr`/`str` on the MCU) rather than `Cell`, so `Memory`, where this
/// lives, stays `Sync` and can be a `static`. All zero when idle.
pub(crate) struct ReadClears([AtomicU32; 5]);

impl ReadClears {
    pub(crate) const fn new() -> Self {
        Self([const { AtomicU32::new(0) }; 5])
    }

    /// Records a clear of byte `i` (below 160).
    #[inline(always)]
    pub(crate) fn record(&self, i: u8) {
        if let Some(w) = self.0.get(usize::from(i >> 5)) {
            w.store(w.load(Relaxed) | 1 << (i & 31), Relaxed);
        }
    }

    /// Whether byte `i` (below 160) is recorded.
    #[cfg(test)]
    pub(crate) fn has(&self, i: usize) -> bool {
        self.0[i / 32].load(Relaxed) & 1 << (i % 32) != 0
    }

    /// Clears the recorded bytes in `oam` and empties the record; whether
    /// any was recorded. Cold and small: one out-of-line loop per word.
    pub(crate) fn apply(&self, oam: &mut [u8; LEN as usize]) -> bool {
        let mut any = false;
        for (k, w) in self.0.iter().enumerate() {
            let bits = w.load(Relaxed);
            if bits != 0 {
                w.store(0, Relaxed);
                clear_bits(&mut oam[k * 32..], bits);
                any = true;
            }
        }
        any
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.0.iter().all(|w| w.load(Relaxed) == 0)
    }
}

/// Zeroes `oam[b]` for each set bit `b` of `bits`.
#[inline(never)]
fn clear_bits(oam: &mut [u8], mut bits: u32) {
    while bits != 0 {
        if let Some(b) = oam.get_mut(bits.trailing_zeros() as usize) {
            *b = 0;
        }
        bits &= bits - 1;
    }
}

#[cfg(test)]
impl PartialEq for ReadClears {
    fn eq(&self, other: &Self) -> bool {
        (0..usize::from(LEN)).all(|i| self.has(i) == other.has(i))
    }
}

/// What a PPU read of OAM at some clock sees of the transfer
/// ([`OamDma::ppu_at`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PpuView {
    /// Not transferring (before it, after it, or in the start delay).
    Off,
    /// Transferring: the OAM scan can't read OAM.
    Running,
    /// Paused by HALT with bytes left: on CGB C the OAM scan finds no
    /// object.
    Halted,
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
            ppu_from: 0,
            ppu_to: 0,
            halt_from: NEVER,
            halt_to: NEVER,
            halt_dest: 0,
            tail: LEN,
            mcycle: 4,
            ppu_live: false,
        }
    }

    /// Sets the PPU's window from `start` and `end`: the PPU sees each
    /// step of the transfer [`PPU_LEAD`] dots before the CPU access it is
    /// due by ([`Self::due`]), from byte 0's write to the last step.
    pub(crate) fn set_ppu_window(&mut self) {
        self.ppu_from = self.start - PPU_LEAD;
        self.ppu_to = self.end - PPU_LEAD;
    }

    /// How a PPU read of OAM at clock `c` sees the transfer, and the
    /// number of bytes written by then as the PPU sees it (0 before the
    /// transfer, 160 after it). Valid for clocks since the last change of
    /// the transfer's state (an FF46 write, a halt, a wake-up): the bus
    /// brings the PPU up to each change first. `docs/reference/oam-dma.md`,
    /// "What the PPU sees".
    pub(crate) fn ppu_at(&self, c: u64) -> (PpuView, u8) {
        if c <= self.ppu_from {
            return (PpuView::Off, 0);
        }
        if c > self.ppu_to {
            return (PpuView::Off, LEN);
        }
        if self.halt_from < c && c <= self.halt_to {
            return (PpuView::Halted, self.halt_dest);
        }
        // A restart's stand-in `start` (`tail`) lasts up to the real
        // start, by which the PPU sees byte 0 written.
        if self.tail < LEN {
            return (PpuView::Running, 1);
        }
        // Byte `i` is written at `start + i * mcycle - PPU_LEAD` and seen
        // after it (`start` has moved out past any earlier halt).
        // `mcycle` is 2 or 4: a shift.
        debug_assert!(self.mcycle == 2 || self.mcycle == 4);
        let since = c.saturating_sub(self.start - PPU_LEAD + 1);
        let n = (since >> self.mcycle.trailing_zeros()) + 1;
        (PpuView::Running, n.min(u64::from(LEN)) as u8)
    }

    /// A PPU OAM read of `addr` (0-9F) at a clock where it sees `dest`
    /// bytes written ([`Self::ppu_at`]), with `oam` as it is now and `old`
    /// as it was at the FF46 write. While bytes 1-159 are being written,
    /// the read gets the 16-bit word being written instead (SameBoy
    /// `oam_read`; Pan Docs: OAM DMA bus conflicts, "the PPU reads
    /// whatever 16-bit word the DMA unit is writing").
    pub(crate) fn ppu_read(dest: u8, addr: u8, oam: &[u8; 0xA0], old: &[u8; 0xA0]) -> u8 {
        let a = if (1..LEN).contains(&dest) {
            (dest & !1) | (addr & 1)
        } else {
            addr
        };
        let a = usize::from(a) % 0xA0;
        if a < usize::from(dest) {
            oam[a]
        } else {
            old[a]
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
    /// Reads FF; a write puts its data in the OAM byte just transferred
    /// (an external-bus access while the DMA reads E000-FFFF; ADR 0028).
    Open,
    /// The access goes to this WRAM address instead: the CPU's low 12 bits
    /// with bit 12 (the bank half) from the DMA's address.
    Wram(u16),
    /// The access sees the DMA's own address `cur`: a read returns the
    /// byte the DMA reads; a write never reaches `addr`, and the OAM byte
    /// just written gets its data (ROM/SRAM source), 00 (VRAM source) or
    /// stays (WRAM source). gambatte cgb04c captures, ADR 0028.
    Dma,
    /// [`Self::Dma`] with a VRAM source: a read also clears the OAM byte
    /// being transferred (gambatte cgb04c, ADR 0028 amendment).
    DmaVram,
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
    } else if cpu == Bus::Vram {
        // Here `cpu == src` (both VRAM): tested on `cpu`, so `src` is
        // dead by now (one saved register less on every read during a
        // transfer, measured on `synth:dma-storm`).
        Conflict::DmaVram
    } else {
        Conflict::Dma
    }
}

#[cfg(test)]
mod tests {
    use super::{Conflict, LEN, OamDma, PpuView, conflict};

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
        // VRAM source: only VRAM conflicts (and a read clears OAM).
        assert_eq!(conflict(0x8000, 0x8010, 0x9000), DmaVram);
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

    /// A transfer written at clock 1000 at M-cycle length `m`, as the bus
    /// sets it up.
    fn written_at_1000(m: u64) -> OamDma {
        let start = 1000 + 2 * m;
        let mut d = OamDma {
            start,
            end: start + u64::from(LEN) * m,
            mcycle: m as u8,
            ppu_live: true,
            ..OamDma::new()
        };
        d.set_ppu_window();
        d
    }

    /// The PPU sees each step 2 dots before the CPU access it is due by,
    /// in both speeds (gambatte `oamdma/late_sp*`, cgb04c).
    #[test]
    fn ppu_window_leads_the_cpu_by_two_dots() {
        use PpuView::*;
        // Single speed: byte 0 is due by the access at 1008, seen after
        // 1006; the last step is due by 1648, seen after 1646.
        let d = written_at_1000(4);
        assert_eq!(d.ppu_at(1006), (Off, 0));
        assert_eq!(d.ppu_at(1007), (Running, 1));
        assert_eq!(d.ppu_at(1010), (Running, 1));
        assert_eq!(d.ppu_at(1011), (Running, 2));
        assert_eq!(d.ppu_at(1006 + 159 * 4), (Running, 159));
        assert_eq!(d.ppu_at(1007 + 159 * 4), (Running, 160));
        assert_eq!(d.ppu_at(1646), (Running, 160));
        assert_eq!(d.ppu_at(1647), (Off, 160));
        // Double speed: due by 1004, seen after 1002; a byte per 2 dots.
        let d = written_at_1000(2);
        assert_eq!(d.ppu_at(1002), (Off, 0));
        assert_eq!(d.ppu_at(1003), (Running, 1));
        assert_eq!(d.ppu_at(1005), (Running, 2));
        assert_eq!(d.ppu_at(1004 + 320 - 2), (Running, 160));
        assert_eq!(d.ppu_at(1004 + 320 - 1), (Off, 160));
    }

    #[test]
    fn ppu_read_gets_the_word_being_written() {
        let oam: [u8; 0xA0] = core::array::from_fn(|i| 0x80 | i as u8);
        let old: [u8; 0xA0] = core::array::from_fn(|i| i as u8);
        // 5 bytes written: any read gets the word 4-5, byte 4 new, byte 5
        // still old.
        assert_eq!(OamDma::ppu_read(5, 0x10, &oam, &old), 0x84);
        assert_eq!(OamDma::ppu_read(5, 0x23, &oam, &old), 0x05);
        // 6 written: the word 6-7, both old.
        assert_eq!(OamDma::ppu_read(6, 0x10, &oam, &old), 0x06);
        assert_eq!(OamDma::ppu_read(6, 0x11, &oam, &old), 0x07);
        // Before the transfer: OAM as it was; after it: as it is.
        assert_eq!(OamDma::ppu_read(0, 0x23, &oam, &old), 0x23);
        assert_eq!(OamDma::ppu_read(LEN, 0x23, &oam, &old), 0xA3);
    }
}
