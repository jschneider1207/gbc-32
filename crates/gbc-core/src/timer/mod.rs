//! DIV/TIMA/TMA/TAC and the system counter (Pan Docs: Timer and Divider
//! Registers; "Timer obscure behaviour"), event-scheduled (ADR 0011).
//!
//! Nothing here runs per M-cycle. The system counter that DIV, TIMA and
//! the serial bit clock derive from is a linear function of the clock
//! between DIV writes and speed switches, so the timer's state is computed
//! from the clock when a register is read or written, and its one effect
//! (the TIMA reload with the timer interrupt) is the `EventKind::Timer`
//! deadline, at [`Timer::deadline`]. The serial bit clock's deadlines come
//! from [`Timer::clock_at`].
//!
//! # The system counter
//! It counts CPU clocks, 4 per M-cycle in both speeds: 1 per base clock at
//! normal speed and 2 in double speed (Pan Docs FF4D KEY1). It is
//! unwrapped (u64), so falling edges can be counted by division:
//! - `sc(t) = base + (saturating(t - base_clock) << shift)`;
//! - bit k falls `(b >> (k+1)) - (a >> (k+1))` times while the counter goes
//!   from `a` to `b`, the next time at `((a >> (k+1)) + 1) << (k+1)`.
//!
//! `base` is a multiple of 4 taken at an M-cycle boundary, so every edge
//! of bit 3 or higher is at a multiple of 16 and [`Timer::clock_of`] maps
//! it to an exact M-cycle boundary in either speed. A DIV write rebases to
//! the next multiple of 2^16 (the low 16 bits, which the hardware clears,
//! become 0 while the counter stays monotonic); a speed switch also moves
//! `base_clock` past the pause, during which the counter is frozen.
//!
//! # TIMA overflow (Pan Docs: "Timer overflow behavior"; ADR 0011)
//! The overflow is the counter edge at which TIMA would wrap, at clock
//! `overflow_at`. The M-cycle whose access is at `overflow_at` is **cycle
//! A**: TIMA reads 00, a TIMA write cancels the reload and the interrupt,
//! DIV and TAC writes don't, and a TMA write is what the reload loads. The
//! reload and IF happen at the boundary that ends cycle A (the Timer
//! deadline, `overflow_at + mcycle`). The access at that clock,
//! `reloaded_at`, is **cycle B**: TIMA already holds TMA, a TIMA write is
//! ignored and a TMA write also reaches TIMA. `reloaded_at` is stored at
//! the reload, because by then `overflow_at` already names the next
//! overflow. SameBoy: `tima_reload_state` RELOADING/RELOADED; Mooneye
//! `tima_reload`, `tima_write_reloading` and `tma_write_reloading` test it.
//!
//! A DIV- or TAC-write glitch increment that overflows sets `overflow_at`
//! to the access clock itself, so the reload is at the end of that same
//! M-cycle and TIMA = 00 is never observable (except across a speed-switch
//! pause, which delays the reload with everything else). A glitch
//! increment *during* cycle A can't happen: the selected bit just fell, so
//! it and every bit below it are 0 at cycle A's access, and neither a DIV
//! nor a TAC write sees a 1 -> 0 edge on it. That is why this model never
//! syncs TIMA in cycle A.
//!
//! The eager per-M-cycle model this replaced is [`reference`], kept for
//! the differential test.

#[cfg(test)]
pub(crate) mod reference;

#[cfg(test)]
mod differential;

use crate::schedule::NEVER;

/// Counter bit per TAC clock select: 4096, 262144, 65536, 16384 Hz.
const TAC_BITS: [u64; 4] = [1 << 9, 1 << 3, 1 << 5, 1 << 7];

/// The system counter when the CGB boot ROM hands over (clock 0, PC =
/// 0100), for a DMG-mode cartridge on CGB rev. C: DIV = 26, 0x88 clocks
/// (34 M-cycles) before it ticks to 27. Mooneye `misc/boot_div-cgbABCDE`
/// (verified on CGB A-E) reads DIV = 27 "immediately after DIV has
/// incremented" with its read 34 M-cycles after PC = 0100 (NOP, JP 0150,
/// 27 NOPs, then LDH A,(04) reads in its third M-cycle), then checks the
/// phase with reads 63-65 M-cycles apart. The boot ROM runs longer for
/// DMG-mode cartridges than for CGB ones (Pan Docs: Power Up Sequence,
/// "the boot ROM's duration depends on the header"); no test pins the
/// CGB-mode value, so CGB mode uses the same one (unverified).
pub(crate) const POST_BOOT_COUNTER: u64 = 0x2700 - 34 * 4;

/// The timer interrupt bit in IF.
pub(crate) const INT_TIMER: u8 = 0x04;

#[derive(Clone, Copy)]
pub(crate) struct Timer {
    /// The system counter at `base_clock`.
    base: u64,
    /// The clock from which the counter runs again (after a speed-switch
    /// pause, later than the clock it was set at).
    base_clock: u64,
    /// log2 of counter steps per base clock: 0 normal, 1 double speed.
    shift: u32,
    /// TIMA as of counter value `tima_from` (outside cycle A).
    tima: u8,
    tima_from: u64,
    pub(crate) tma: u8,
    /// TAC, low 3 bits.
    pub(crate) tac: u8,
    /// The counter bit TIMA counts falling edges of, or 0 if TAC disables
    /// the timer.
    tima_bit: u64,
    /// The clock of the next (or current: cycle A) overflow, or `NEVER`.
    overflow_at: u64,
    /// The clock of the last reload: cycle B's access. `NEVER` before one.
    reloaded_at: u64,
}

impl Timer {
    pub(crate) const fn new() -> Self {
        Self {
            base: 0,
            base_clock: 0,
            shift: 0,
            tima: 0,
            tima_from: 0,
            tma: 0,
            tac: 0,
            tima_bit: 0,
            overflow_at: NEVER,
            reloaded_at: NEVER,
        }
    }

    /// The timer as the CGB boot ROM leaves it at clock 0 (PC = 0100):
    /// every register 0, the system counter at [`POST_BOOT_COUNTER`].
    pub(crate) const fn post_boot() -> Self {
        let mut t = Self::new();
        t.base = POST_BOOT_COUNTER;
        t.tima_from = POST_BOOT_COUNTER;
        t
    }

    /// The system counter at clock `now`.
    #[inline]
    pub(crate) fn sc(&self, now: u64) -> u64 {
        self.base + (now.saturating_sub(self.base_clock) << self.shift)
    }

    /// The clock at which the counter reaches `v` (`v >= base`).
    #[inline]
    fn clock_of(&self, v: u64) -> u64 {
        self.base_clock + ((v - self.base) >> self.shift)
    }

    /// Base clocks per M-cycle.
    #[inline]
    fn mcycle(&self) -> u64 {
        4 >> self.shift
    }

    /// The clock at which the counter reaches `v` (`v >= sc(now)`, a
    /// multiple of 4): the serial bit clock's next shift.
    #[inline]
    pub(crate) fn clock_at(&self, v: u64) -> u64 {
        self.clock_of(v)
    }

    #[inline]
    pub(crate) fn div(&self, now: u64) -> u8 {
        (self.sc(now) >> 8) as u8
    }

    /// Reads FF04-FF07 (by the low address bits) at clock `now`. Out of
    /// line, so the counter arithmetic stays off `read_high` (ADR 0011).
    #[inline(never)]
    pub(crate) fn read(&self, now: u64, reg: usize) -> u8 {
        match reg & 3 {
            0 => self.div(now),
            1 => self.tima(now),
            2 => self.tma,
            _ => 0xF8 | self.tac,
        }
    }

    /// The access at `now` is in cycle A (module docs).
    #[inline]
    fn in_cycle_a(&self, now: u64) -> bool {
        self.overflow_at <= now
    }

    /// TIMA at clock `now`.
    #[inline]
    pub(crate) fn tima(&self, now: u64) -> u8 {
        if self.in_cycle_a(now) {
            return 0;
        }
        if self.tima_bit == 0 {
            return self.tima;
        }
        let s = self.tima_bit.trailing_zeros() + 1;
        let falls = (self.sc(now) >> s) - (self.tima_from >> s);
        // Below 256: the overflow is cycle A, handled above.
        (u64::from(self.tima) + falls) as u8
    }

    /// The Timer deadline: the reload at the end of cycle A, or `NEVER`.
    #[inline]
    pub(crate) fn deadline(&self) -> u64 {
        self.overflow_at.saturating_add(self.mcycle())
    }

    /// Makes `tima` exact at `now` (not in cycle A).
    fn sync(&mut self, now: u64) {
        self.tima = self.tima(now);
        self.tima_from = self.sc(now);
    }

    /// Sets `overflow_at` from the synced `tima`/`tima_from`.
    fn schedule_overflow(&mut self) {
        self.overflow_at = if self.tima_bit == 0 {
            NEVER
        } else {
            let s = self.tima_bit.trailing_zeros() + 1;
            let n = 256 - u64::from(self.tima);
            self.clock_of(((self.tima_from >> s) + n) << s)
        };
    }

    /// A glitch increment at the access clock `now` (synced, not in cycle
    /// A). An overflow makes this M-cycle cycle A; the caller schedules.
    fn increment(&mut self, now: u64) {
        let (v, overflow) = self.tima.overflowing_add(1);
        self.tima = v;
        if overflow {
            self.overflow_at = now;
        }
    }

    /// The Timer event at the end of cycle A: TIMA = TMA. Returns the IF
    /// bit to request.
    pub(crate) fn reload(&mut self, now: u64) -> u8 {
        self.tima = self.tma;
        self.tima_from = self.sc(now);
        self.reloaded_at = now;
        self.schedule_overflow();
        INT_TIMER
    }

    /// Pan Docs "Timer overflow behavior": a TIMA write in cycle A cancels
    /// the reload and the interrupt; in cycle B it is ignored (TMA wins).
    pub(crate) fn write_tima(&mut self, now: u64, value: u8) {
        if now == self.reloaded_at && !self.in_cycle_a(now) {
            return;
        }
        self.tima = value;
        self.tima_from = self.sc(now);
        self.schedule_overflow();
    }

    /// The reload reads `tma` at the end of cycle A, so a write in cycle A
    /// is used; in cycle B the write also reaches TIMA (SameBoy stores TMA
    /// writes into TIMA whenever `tima_reload_state` isn't RUNNING).
    pub(crate) fn write_tma(&mut self, now: u64, value: u8) {
        self.tma = value;
        if now == self.reloaded_at && !self.in_cycle_a(now) {
            self.tima = value;
            self.tima_from = self.sc(now);
            self.schedule_overflow();
        }
    }

    /// Changing TAC can turn the timer's input from 1 to 0, which counts as
    /// a falling edge (Pan Docs: "Timer obscure behaviour"; SameBoy
    /// `GB_emulate_timer_glitch`). That includes disabling the timer:
    /// SameBoy ticks there; Pan Docs says CGBs don't (ADR 0011 open
    /// question, kept until a test decides).
    // Out of line: keeps the code-size budgets of its callers independent
    // of codegen-unit partitioning (it is not on a hot path).
    #[inline(never)]
    pub(crate) fn write_tac(&mut self, now: u64, value: u8) {
        let new_bit = if value & 4 != 0 {
            TAC_BITS[usize::from(value & 3)]
        } else {
            0
        };
        self.tac = value & 7;
        if self.in_cycle_a(now) {
            // No glitch is possible (module docs); the reload's own
            // scheduling picks up the new input.
            self.tima_bit = new_bit;
            return;
        }
        self.sync(now);
        let sc = self.tima_from;
        let glitch = sc & self.tima_bit != 0 && sc & new_bit == 0;
        self.tima_bit = new_bit;
        if glitch {
            self.increment(now);
        }
        if !self.in_cycle_a(now) {
            self.schedule_overflow();
        }
    }

    /// A DIV write (or STOP) at `now`: the TIMA glitch increment if its
    /// input bit was 1, then the counter's low 16 bits clear (a rebase).
    /// Returns the counter before the write: its set bits all fall, which
    /// the caller applies to the serial bit clock (SameBoy
    /// `GB_set_internal_div_counter`: `triggers = div_counter & ~value`).
    #[inline(never)] // See `write_tac`.
    pub(crate) fn write_div(&mut self, now: u64) -> u64 {
        let old = self.sc(now);
        let cycle_a = self.in_cycle_a(now);
        if !cycle_a {
            self.sync(now);
            if old & self.tima_bit != 0 {
                self.increment(now);
            }
        }
        self.base = (old | 0xFFFF) + 1;
        self.base_clock = now;
        if !self.in_cycle_a(now) {
            self.tima_from = self.base;
            self.schedule_overflow();
        }
        old
    }

    /// A CGB speed switch at `now`: the counter (and the overflow window,
    /// which the eager model advanced only in M-cycle ticks) stays frozen
    /// for `pause` base clocks (0 for none), then runs at the other speed.
    ///
    /// `pause` may be negative: -1 counts one clock the CPU doesn't see
    /// (the speed-switch halt to single speed is one clock short of whole
    /// M-cycles, `Ctx::speed_switch`), keeping the timer on the CPU's grid.
    pub(crate) fn speed_switch(&mut self, now: u64, pause: i64) {
        let from = now.wrapping_add_signed(pause);
        self.base = self.sc(now);
        self.shift ^= 1;
        self.base_clock = from;
        if self.in_cycle_a(now) {
            self.overflow_at = from.max(now);
        } else {
            self.schedule_overflow();
        }
        if self.reloaded_at == now {
            self.reloaded_at = from.max(now);
        }
    }
}
