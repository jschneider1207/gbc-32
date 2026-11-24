//! DIV/TIMA/TMA/TAC and the system counter (Pan Docs: Timer and Divider
//! Registers; "Timer obscure behaviour"), event-scheduled (ADR 0011).
//!
//! Nothing here runs per M-cycle. The system counter that DIV, TIMA and
//! the serial bit clock derive from is a linear function of the clock
//! between DIV writes and speed switches, so the timer's state is computed
//! from the clock when a register is read or written, and its one effect
//! on the rest of the machine (the timer interrupt requested at a TIMA
//! reload) is the `EventKind::Timer` deadline, at [`Timer::deadline`]. The
//! serial bit clock's deadlines come from [`Timer::clock_at`].
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
//! # Reloads are lazy (ADR 0011, 2026-11-24 amendment)
//! A reload changes only this struct (`tima`, `tima_from`, `reloaded_at`,
//! `overflow_at`) and requests the interrupt. The state part needs no
//! event: [`Timer::catch_up`] runs every reload due by a clock, in closed
//! form, and every access that reads or changes that state calls it first
//! (the TIMA read on a copy, the register writes, STOP and the speed
//! switch). The request part is the Timer event, which the bus schedules
//! at [`Timer::deadline`], except while IF bit 2 is already set and IE
//! bit 2 is clear: a request that is already pending, and that no
//! dispatch can acknowledge, makes later ones unobservable until one of
//! the two bits changes, where the bus catches up and schedules again
//! (`Ctx::schedule_timer`). A timer whose interrupt nobody takes (TMA =
//! FF at 262 kHz reloads every 4 M-cycles) costs nothing.
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
/// (a DMG-mode cartridge, header 0143 = 00; verified on CGB A-E) reads
/// DIV = 27 "immediately after DIV has incremented" with its read 34
/// M-cycles after PC = 0100 (NOP, JP 0150, 27 NOPs, then LDH A,(04) reads
/// in its third M-cycle), then checks the phase with reads 63-65 M-cycles
/// apart. The boot ROM runs longer for DMG-mode cartridges than for CGB
/// ones (Pan Docs: Power Up Sequence, "the boot ROM's duration depends on
/// the header"): see [`POST_BOOT_COUNTER_CGB`]. ADR 0022.
pub(crate) const POST_BOOT_COUNTER_DMG: u64 = 0x2700 - 34 * 4;

/// The system counter at hand-off for a CGB-mode cartridge (header 0143
/// bit 7): DIV = 1E, 0x60 clocks (24 M-cycles) before it ticks to 1F.
/// Gambatte `div/start_inc_1_cgb04c_out1E` and `start_inc_2_cgb04c_out1F`
/// (CGB-only cartridges, results from a CGB04C) read DIV with LDH A,(04)
/// after JP 0150, JP 1000 and 13 or 14 NOPs, i.e. in the 24th or 25th
/// M-cycle after PC = 0100 (offsets 23 and 24, counted as above): 1E at
/// offset 23 and 1F at offset 24, so the counter is 0x1F00 - 24 * 4 on the
/// M-cycle grid. Gambatte's CGB initial state has the same counter (DIV 1E, same
/// phase), as do its `sound/ch2_*_nr52_*` CGB results, which depend on the
/// DIV-APU phase at an NR52 power on. ADR 0022.
pub(crate) const POST_BOOT_COUNTER_CGB: u64 = 0x1F00 - 24 * 4;

/// The post-boot system counter for CGB mode (`cgb_mode`) or DMG
/// compatibility mode.
pub(crate) const fn post_boot_counter(cgb_mode: bool) -> u64 {
    if cgb_mode {
        POST_BOOT_COUNTER_CGB
    } else {
        POST_BOOT_COUNTER_DMG
    }
}

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
    /// The frame sequencer runs one double-speed M-cycle (2 clocks) behind
    /// the DIV-APU edge in double speed (`set_div_apu_deadline`). Each
    /// switch to double speed with the APU on toggles it: the 1st and 3rd
    /// delay, the 2nd doesn't. The square channels' 1 MHz phase in a
    /// double-speed trigger follows it (`Apu::trigger_delay`). NR52 power
    /// on/off clears it (AGE
    /// `spsw-ch2-lc-delay`, verified on CGB B/C/E; AGE emulator
    /// `gb_sound::after_speed_change`, `m_current_ds_delay`). Single speed
    /// ignores it but keeps it for the next switch. Toggled here even with
    /// the APU off, where it is unused until power-on clears it. Placed
    /// here, not in `Sys`, in this struct's padding while `Hw` had a size
    /// cap (dropped 2026-11-24).
    pub(crate) apu_ds_delay: bool,
    /// STOP mode froze the counter in cycle A ([`Self::freeze`]): the
    /// reload is held with it and ends one M-cycle after the wake-up.
    held: bool,
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
            apu_ds_delay: false,
            held: false,
        }
    }

    /// The timer as the CGB boot ROM leaves it at clock 0 (PC = 0100) in
    /// CGB mode (`cgb_mode`) or DMG compatibility mode: every register 0,
    /// the system counter at [`post_boot_counter`].
    pub(crate) const fn post_boot(cgb_mode: bool) -> Self {
        let mut t = Self::new();
        t.base = post_boot_counter(cgb_mode);
        t.tima_from = t.base;
        t
    }

    /// Moves the timer `by` clocks later: the same counter, TIMA and
    /// reload window at clock `t + by` as it had at `t` (the
    /// `clock-offset` test hook, `Emulator::start_clock_at`).
    #[cfg(feature = "clock-offset")]
    pub(crate) fn shift_clock(&mut self, by: u64) {
        self.base_clock += by;
        for t in [&mut self.overflow_at, &mut self.reloaded_at] {
            if *t != NEVER {
                *t += by;
            }
        }
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

    /// The system-counter bit whose falling edge clocks the APU frame
    /// sequencer: DIV bit 4, bit 5 in double speed, i.e. counter bit 12 or
    /// 13 (Pan Docs "Audio details", DIV-APU; SameBoy `apu_bit`). The
    /// counter runs twice as fast in double speed, so the edge stays at
    /// 512 Hz.
    #[inline]
    pub(crate) fn div_apu_bit(&self) -> u32 {
        12 + self.shift
    }

    /// Reads FF04-FF07 (by the low address bits) at clock `now`. Out of
    /// line, so the counter arithmetic stays off `read_high` (ADR 0011).
    #[inline(never)]
    pub(crate) fn read(&self, now: u64, reg: usize) -> u8 {
        match reg & 3 {
            0 => self.div(now),
            1 => {
                // The reloads since the last access, on a copy: reads
                // have no side effects (`peek`). `sync` (00 in cycle A)
                // rather than `tima`: shared code, out of line.
                let mut t = *self;
                t.catch_up(now);
                t.sync(now);
                t.tima
            }
            2 => self.tma,
            _ => 0xF8 | self.tac,
        }
    }

    /// The access at `now` is in cycle A (module docs). As everything
    /// below that takes `now`: of a timer caught up to `now`
    /// ([`Self::catch_up`]).
    #[inline]
    fn in_cycle_a(&self, now: u64) -> bool {
        self.overflow_at <= now
    }

    /// TIMA at clock `now`.
    #[inline]
    fn tima(&self, now: u64) -> u8 {
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

    /// The clock of the next reload, at the end of cycle A, or `NEVER`:
    /// the Timer deadline, when the bus sets one (module docs, "Reloads
    /// are lazy").
    #[inline]
    pub(crate) fn deadline(&self) -> u64 {
        self.overflow_at.saturating_add(self.mcycle())
    }

    /// Runs every reload due at or before `now` (module docs, "Reloads are
    /// lazy"): the state is then what running each at its own clock would
    /// have left. After the first, TIMA counts from TMA, and nothing else
    /// changes in between (every access catches up first): the reloads
    /// are `256 - TMA` falls of the selected bit apart, a whole number of
    /// clocks, so only the second and the last need running. The last is
    /// the second plus the whole periods that fit before `now`.
    // Out of line (code size): one copy for the register accesses, STOP,
    // the speed switch and the IF paths.
    #[inline(never)]
    pub(crate) fn catch_up(&mut self, now: u64) {
        let first = self.deadline();
        if first > now {
            return;
        }
        // Reloads are at M-cycle boundaries, as are the accesses.
        debug_assert!((now - first).is_multiple_of(self.mcycle()));
        self.reload(first);
        // `NEVER` if the timer is off.
        let second = self.deadline();
        if second > now {
            return;
        }
        // log2 of the clocks per fall of the selected bit.
        let fall = self.tima_bit.trailing_zeros() + 1 - self.shift;
        let falls = (now - second) >> fall;
        let whole = falls - rem(falls, 256 - u32::from(self.tma));
        self.reload(second + (whole << fall));
    }

    /// Makes `tima` exact at `now` (not in cycle A, except for a read on
    /// a copy).
    // Out of line (code size): shared by the register writes and STOP,
    // none of them on a hot path.
    #[inline(never)]
    fn sync(&mut self, now: u64) {
        self.tima = self.tima(now);
        self.tima_from = self.sc(now);
    }

    /// Sets `overflow_at` from the synced `tima`/`tima_from`.
    #[inline(never)] // See `sync`.
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

    /// The reload at the end of cycle A, at clock `now`: TIMA = TMA. The
    /// timer interrupt it requests is the bus's ([`Self::deadline`]).
    #[inline(never)] // See `sync`.
    pub(crate) fn reload(&mut self, now: u64) {
        self.tima = self.tma;
        self.tima_from = self.sc(now);
        self.reloaded_at = now;
        self.schedule_overflow();
    }

    /// Pan Docs "Timer overflow behavior": a TIMA write in cycle A cancels
    /// the reload and the interrupt; in cycle B it is ignored (TMA wins).
    pub(crate) fn write_tima(&mut self, now: u64, value: u8) {
        self.catch_up(now);
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
        // First: the reloads up to `now` loaded the old TMA.
        self.catch_up(now);
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
        self.catch_up(now);
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
        self.catch_up(now);
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

    /// Before STOP's DIV reset at `now` (the end of its fetch) when STOP
    /// is two bytes (nothing pending): the TIMA edge the reset would make
    /// one M-cycle later. AGE (Apache-2.0, read only) resets DIV after
    /// STOP's second byte; there a timer bit that rises during the second
    /// byte falls at the reset (an increment our earlier reset misses),
    /// except for the 4 kHz timer on CGB B/C, where a bit that rose on the
    /// reset's M-cycle doesn't count (`gb_timer::after_div_reset`, "speed
    /// change glitch"). Our reset then sees the bit as AGE does in every
    /// other case: a fall during the byte is our reset's fall.
    /// Verified by AGE `spsw-tima-cgbBC` (CGB B/C), ADR 0020.
    #[inline(never)]
    pub(crate) fn stop_edge(&mut self, now: u64) {
        self.catch_up(now);
        let b = self.tima_bit;
        // The 4 kHz timer (bit 9) and a stopped timer (0) have none.
        if b & 0xA8 == 0 || self.in_cycle_a(now) {
            return;
        }
        if self.sc(now) & (2 * b - 1) == b - 4 {
            self.sync(now);
            self.increment(now);
        }
    }

    /// STOP mode begins at `now`, after STOP's DIV reset: the oscillator
    /// stops, so the counter holds its value until [`Self::thaw`]
    /// (SameBoy `timers_run` returns while `stopped`; Pan Docs "Using the
    /// STOP instruction"). `base_clock` is `NEVER` meanwhile, so `sc` is
    /// constant and there is no overflow to schedule: nothing may ask for
    /// a counter value's clock (`clock_of`) until the thaw.
    ///
    /// A reload in flight (cycle A at `now`: the overflow fell on STOP's
    /// fetch, or the DIV reset's glitch increment overflowed) is held
    /// with the counter, as across the speed-switch pause: TIMA reads 00
    /// and the reload and its interrupt come one M-cycle after the thaw.
    /// A choice: no reference covers it.
    #[cold]
    #[inline(never)]
    pub(crate) fn freeze(&mut self, now: u64) {
        self.catch_up(now);
        self.held = self.in_cycle_a(now);
        self.base = self.sc(now);
        if self.held {
            self.tima = 0;
        } else {
            self.sync(now);
        }
        self.tima_from = self.base;
        self.base_clock = NEVER;
        self.overflow_at = NEVER;
    }

    /// STOP mode ends at `now`: the counter runs again from the value it
    /// held, and a held cycle A is this M-cycle.
    #[cold]
    #[inline(never)]
    pub(crate) fn thaw(&mut self, now: u64) {
        self.base_clock = now;
        if self.held {
            self.held = false;
            self.overflow_at = now;
        } else {
            self.schedule_overflow();
        }
    }

    /// A CGB speed switch at `now`: the counter (and the overflow window,
    /// which the eager model advanced only in M-cycle ticks) stays frozen
    /// for `pause` base clocks (0 for none), then runs at the other speed.
    ///
    /// `short`: the speed-switch halt to single speed, one clock short of
    /// whole M-cycles, so the CPU's M-cycle grid moves by 3 clocks
    /// (`Ctx::speed_switch`). The counter then runs from one clock before
    /// `now + pause`, counting a clock the CPU doesn't see, which keeps
    /// the timer on the CPU's grid. A cycle A open at such a switch isn't
    /// held for the pause: it ends at the first boundary of the moved
    /// grid, `now + 3`. (The reload ran from the deadline set before the
    /// switch when every reload was an event; kept, and pinned by
    /// `differential`'s
    /// `switch_to_single_speed_in_cycle_a_reloads_at_the_moved_grid`.)
    pub(crate) fn speed_switch(&mut self, now: u64, pause: u64, short: bool) {
        self.catch_up(now);
        let from = now + pause - u64::from(short);
        self.base = self.sc(now);
        self.shift ^= 1;
        self.apu_ds_delay ^= self.shift != 0;
        self.base_clock = from;
        if self.in_cycle_a(now) {
            self.overflow_at = if short { now - 1 } else { from.max(now) };
        } else {
            self.schedule_overflow();
        }
        if self.reloaded_at == now {
            self.reloaded_at = from.max(now);
        }
    }
}

/// `x % p` for `p` in 1..=256, in 32-bit divisions (16 bits of `x` at a
/// time: a remainder below 2^8 shifted up by 16 still fits), so the MCU
/// build needs no 64-bit division routine.
#[inline]
fn rem(x: u64, p: u32) -> u64 {
    let mut r = 0;
    for i in (0..4).rev() {
        r = ((r << 16) | ((x >> (16 * i)) as u32 & 0xFFFF)) % p;
    }
    u64::from(r)
}
