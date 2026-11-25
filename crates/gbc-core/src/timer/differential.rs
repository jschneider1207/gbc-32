//! The differential test of ADR 0011 "Verification": the event-scheduled
//! timer and serial clock, driven through the real bus (`Ctx`), against
//! the eager per-M-cycle model ([`super::reference`] plus the serial
//! port's per-M-cycle hook, wired as the bus did before ADR 0011 step 4).
//!
//! A fixed-seed xorshift generator drives both through the same random
//! sequences of M-cycles, SB/SC/DIV/TIMA/TMA/TAC writes, STOP (DIV reset)
//! and CGB speed switches, with writes biased to the clocks around a TIMA
//! overflow (`overflow_at - m`, `overflow_at` = cycle A, `reloaded_at` =
//! cycle B). After every access and every M-cycle it compares the clock,
//! DIV, TIMA, TMA, TAC, SB, SC, the timer and serial IF bits, the serial
//! port's bit count, bits sent and last byte, and the Timer and Serial
//! deadlines against the eager model's next reload and the end of its
//! serial transfer (found by ticking a copy ahead).
//!
//! The serial port's shifts are lazy too (ADR 0011, 2026-11-24 serial
//! amendment): only a transfer's completion is an event, and SB is read
//! here at every check without syncing the port (a read is a `peek`), so
//! the SB/SC/DIV writes, speed switches and STOPs of a sequence land on a
//! port with shifts not yet applied. STOP mode lasts a random number of
//! M-cycles, in which the eager model isn't ticked.
//!
//! Reloads are lazy while IF bit 2 is set and IE bit 2 clear (ADR 0011,
//! 2026-11-24 amendment), so the sequences also leave the IF bit set (per
//! sequence: the harness clears it after every check, after some, or
//! never), write IF with random values and IE with its bit 2 on or off,
//! acknowledge the timer interrupt as the dispatch does
//! (`push_acknowledge`, also with the push landing on IF or on IE), and
//! idle for thousands of M-cycles; the writes biased to an overflow then
//! land on reloads no event ran. While the timer is lazy the Timer
//! deadline must be `NEVER`. Two tests of `Timer::catch_up` alone compare
//! it with the reloads run one by one, field by field.

// Inline so the hard-rules test sees `std` inside a `#[cfg(test)] mod`.
#[cfg(test)]
mod tests {
    extern crate std;

    use crate::bus::{Ctx, Sys, read};
    use crate::cpu::Bus;
    use crate::memory::Memory;
    use crate::schedule::{EventKind, NEVER};
    use crate::serial::{INT_SERIAL, Serial};
    use crate::testkit::Rng;
    use crate::timer::reference::Timer as Eager;
    use crate::{Events, Frame};
    use std::boxed::Box;
    use std::format;
    use std::string::String;

    use crate::timer::{INT_TIMER, Timer};

    /// The eager machine: the reference timer and the serial port, ticked
    /// per M-cycle as the bus did before ADR 0011 step 4.
    #[derive(Clone)]
    struct Ref {
        timer: Eager,
        serial: Serial,
        iflags: u8,
        /// A serial byte completed since the last check.
        tx: bool,
        /// IE: 00 or 04 (timer).
        ie: u8,
        mcycle: u64,
        clock: u64,
        cgb: bool,
    }

    impl Ref {
        fn clock_serial(&mut self, fell: u16) {
            if self.serial.clock(u64::from(fell)) {
                self.iflags |= INT_SERIAL;
                self.tx = true;
            }
        }

        fn tick(&mut self) {
            let fell = self.timer.tick(&mut self.iflags);
            self.clock_serial(fell);
            self.clock += self.mcycle;
        }

        fn write(&mut self, reg: u8, value: u8) {
            match reg {
                1 => self.serial.sb = value,
                2 => self.serial.write_sc_eager(value, self.cgb),
                _ => {
                    let fell = self.timer.write(reg, value);
                    self.clock_serial(fell);
                }
            }
        }

        /// STOP: DIV reset, then (if `switch`) the speed switch with an
        /// interrupt pending and IME on: to double speed, one M-cycle that
        /// doesn't tick the timer, then (CGB B/C) the held DIV reset, one
        /// more at the new speed (`Ctx::speed_switch`).
        fn stop(&mut self, switch: bool) {
            // With nothing pending, STOP is two bytes.
            if self.ie & self.iflags == 0 {
                self.timer.stop_edge();
            }
            let fell = self.timer.write_div();
            self.clock_serial(fell);
            if switch {
                self.mcycle = if self.mcycle == 4 { 2 } else { 4 };
                if self.mcycle == 2 {
                    self.clock += self.mcycle;
                }
                self.clock += self.mcycle;
            }
        }

        /// The last M-cycle of an interrupt dispatch
        /// (`Bus::push_acknowledge`): `ie & IF` is sampled, then the
        /// M-cycle, then the sampled bit clears (a reload in that M-cycle
        /// is absorbed). `push`: the push writes IF (SP = FF10; the sample
        /// is of IF before it) or IE (SP = 0000; of IE before it) first.
        fn acknowledge(&mut self, push: Option<(u16, u8)>) -> u8 {
            let pending = self.ie & self.iflags & INT_TIMER;
            match push {
                Some((0xFF0F, v)) => self.iflags = v & (INT_TIMER | INT_SERIAL),
                Some((_, v)) => self.ie = v,
                None => {}
            }
            self.tick();
            self.iflags &= !pending;
            pending
        }

        fn read(&self, reg: u8) -> u8 {
            match reg {
                1 => self.serial.sb,
                2 => self.serial.read_sc(self.cgb),
                _ => self.timer.read(reg),
            }
        }

        /// The clocks of the next reload (timer IF) within `w` M-cycles
        /// and of the running serial transfer's completion (serial IF),
        /// by ticking a copy.
        fn next_events(&self, w: u32) -> (u64, u64) {
            let mut c = self.clone();
            let (mut timer, mut serial) = (NEVER, NEVER);
            for _ in 0..w {
                c.iflags = 0;
                c.tick();
                if c.iflags & INT_TIMER != 0 && timer == NEVER {
                    timer = c.clock;
                }
            }
            let mut c = self.clone();
            // 8 bits of 128 M-cycles on the normal clock, and up to 128
            // more to the first edge.
            for _ in 0..SERIAL_WINDOW {
                if !c.serial.running() {
                    break;
                }
                c.iflags = 0;
                c.tick();
                if c.iflags & INT_SERIAL != 0 {
                    serial = c.clock;
                }
            }
            assert!(!c.serial.running(), "a transfer longer than the window");
            (timer, serial)
        }
    }

    /// Look-ahead window for the Timer deadline comparison, in M-cycles.
    const WINDOW: u32 = 8;
    /// And for the Serial deadline: longer than any transfer.
    const SERIAL_WINDOW: u32 = 1200;

    struct Pair<'r, 'a> {
        ctx: Ctx<'r, 'a>,
        eager: Ref,
        log: String,
        cov: &'r mut Coverage,
        /// The harness clears IF bit 2 after a check once in this many
        /// (1: always, so every request is pinned to its M-cycle; 0:
        /// never, only the sequence's own IF writes and acknowledges do).
        clear_1_in: u64,
        rng: Rng,
    }

    /// How often the corners the test is for were hit, so it can't silently
    /// stop covering them.
    #[derive(Default, Debug)]
    struct Coverage {
        /// Writes in cycle A and in cycle B.
        write_a: u32,
        write_b: u32,
        /// DIV/TAC writes whose glitch increment overflowed TIMA.
        glitch_overflow: u32,
        /// STOPs in cycle A or B, and speed switches.
        stop_in_window: u32,
        switch: u32,
        /// Serial bytes completed.
        serial_done: u32,
        /// Writes in cycle A and in cycle B of a reload no event ran (IF
        /// bit 2 was set: the write's catch-up ran it).
        lazy_a: u32,
        lazy_b: u32,
        /// Accesses whose catch-up ran two or more reloads.
        lazy_multi: u32,
        /// Acknowledges of a pending timer interrupt, and those whose
        /// push cleared IE bit 2 (the Timer deadline was off after it).
        ack: u32,
        ack_ie_off: u32,
        /// IE writes that set bit 2 with IF bit 2 set and a reload to
        /// catch up.
        ie_on_lazy: u32,
        /// IF writes that cleared a set bit 2 with a reload to catch up.
        if_clear_lazy: u32,
        /// Checks with IF bit 2 set (the Timer deadline must be off).
        if_set: u32,
        /// Checks, SB/SC writes, DIV writes, STOPs and speed switches with
        /// serial shifts not yet applied (no event ran them).
        serial_lazy: u32,
        serial_lazy_write: u32,
        serial_lazy_div: u32,
        serial_lazy_stop: u32,
        serial_lazy_switch: u32,
        /// STOP modes of 100 M-cycles or more in a running transfer.
        serial_stopped: u32,
    }

    impl Pair<'_, '_> {
        fn now(&self) -> u64 {
            self.ctx.clock()
        }

        /// The timer as an access at the current clock finds it.
        fn timer(&self) -> crate::timer::Timer {
            let mut t = self.ctx.sys.timer;
            t.catch_up(self.now());
            t
        }

        /// The reloads up to `now` that no event ran.
        fn lazy_reloads(&self, now: u64) -> u32 {
            let mut t = self.ctx.sys.timer;
            let mut n = 0;
            while t.deadline() <= now {
                t.reload(t.deadline());
                n += 1;
            }
            n
        }

        /// Serial shifts up to `now` that no access has applied yet.
        fn serial_lazy(&self) -> bool {
            let s = &self.ctx.sys;
            let (c, mut port) = (s.timer.sc(self.now()), s.serial.clone());
            port.catch_up(c);
            port.bits != s.serial.bits
        }

        fn check(&mut self, at: &str) {
            self.cov.serial_lazy += u32::from(self.serial_lazy());
            let c = &self.ctx;
            let lazy_read = |reg: u8| read::<true>(c.mem, c.sys, &c.hot, 0xFF00 | u16::from(reg));
            let mut fail = String::new();
            if c.clock() != self.eager.clock {
                fail += &format!("clock {} vs eager {}; ", c.clock(), self.eager.clock);
            }
            for (name, reg) in [
                ("SB", 1),
                ("SC", 2),
                ("DIV", 4),
                ("TIMA", 5),
                ("TMA", 6),
                ("TAC", 7),
                ("IF", 0x0F),
            ] {
                let mask = if reg == 0x0F {
                    INT_TIMER | INT_SERIAL
                } else {
                    0xFF
                };
                let e = if reg == 0x0F {
                    self.eager.iflags
                } else {
                    self.eager.read(reg)
                };
                let (l, e) = (lazy_read(reg) & mask, e & mask);
                if l != e {
                    fail += &format!("{name} {l:02x} vs eager {e:02x}; ");
                }
            }
            let mask = INT_TIMER | INT_SERIAL;
            let (li, ei) = (c.sys.iflags & mask, self.eager.iflags & mask);
            self.cov.serial_done += u32::from(ei & INT_SERIAL != 0);
            if li != ei {
                fail += &format!("IF {li:02x} vs eager {ei:02x}; ");
            }
            // The serial port as an access now would find it. A catch-up
            // here completes nothing: the event ran first.
            let mut port = c.sys.serial.clone();
            let done = port.catch_up(c.sys.timer.sc(c.clock()));
            let (l, e) = (port.state(), self.eager.serial.state());
            if done || l != e {
                fail += &format!("serial {l:02x?} (done: {done}) vs eager {e:02x?}; ");
            }
            let tx = c.events.contains(Events::SERIAL_TX);
            if tx != self.eager.tx {
                fail += &format!("SERIAL_TX {tx} vs eager {}; ", self.eager.tx);
            }
            let now = c.clock();
            let horizon = now + u64::from(WINDOW) * self.eager.mcycle;
            let (et, es) = self.eager.next_events(WINDOW);
            for (name, kind, e) in [
                ("Timer", EventKind::Timer, et),
                ("Serial", EventKind::Serial, es),
            ] {
                let d = c.sys.sched.deadline(kind);
                let l = if d <= horizon { d } else { NEVER };
                if kind == EventKind::Timer && li & !self.eager.ie & INT_TIMER != 0 {
                    // No event while the request is already pending and
                    // can't be acknowledged.
                    if d != NEVER {
                        fail += &format!("Timer deadline {d} with IF bit 2 set, IE bit 2 clear; ");
                    }
                } else if kind == EventKind::Serial {
                    // Exactly the completion; no deadline otherwise.
                    if d != e {
                        fail += &format!("Serial deadline {d} vs eager completion {e}; ");
                    }
                } else if l != e {
                    fail += &format!("{name} deadline {d} vs eager next {e}; ");
                }
            }
            assert!(
                fail.is_empty(),
                "{at} at clock {now}: {fail}\nlog:{}",
                self.log
            );
            self.cov.if_set += u32::from(li & !self.eager.ie & INT_TIMER != 0);
            assert_eq!(self.ctx.sys.ie, self.eager.ie);
            // Clear the serial bit so every request is pinned to its
            // M-cycle, and the timer's as the sequence says (`clear_1_in`),
            // by an IF write: the bus re-arms the Timer deadline there.
            self.ctx.sys.iflags &= INT_TIMER;
            self.eager.iflags &= INT_TIMER;
            self.ctx.events = Events::EMPTY;
            self.eager.tx = false;
            if self.clear_1_in != 0 && self.rng.below(self.clear_1_in) == 0 {
                self.ctx.write_mapped(0xFF0F, 0);
                self.eager.iflags = 0;
            }
        }

        /// `n` M-cycles with no access, compared at the end only.
        fn gap(&mut self, n: u64) {
            self.log += &format!(" [{}] gap{n}", self.now());
            for _ in 0..n {
                self.ctx.idle();
                self.eager.tick();
            }
            self.check("gap");
        }

        /// One M-cycle whose access writes IF, or (`free`) the CPU-side
        /// setter (`Bus::set_interrupt_flags`), then an M-cycle.
        fn write_if(&mut self, value: u8, free: bool) {
            let lazy = self.lazy_reloads(self.now()) != 0;
            let clears = self.ctx.sys.iflags & !value & INT_TIMER != 0;
            self.cov.if_clear_lazy += u32::from(lazy && clears);
            self.log += &format!(" [{}] if={value:02x}", self.now());
            if free {
                Bus::set_interrupt_flags(&mut self.ctx, value);
            } else {
                self.ctx.write_mapped(0xFF0F, value);
            }
            self.eager.iflags = value & (INT_TIMER | INT_SERIAL);
            self.check("write IF");
            self.idle();
        }

        /// One M-cycle whose access writes IE (00 or 04).
        fn write_ie(&mut self, value: u8) {
            let lazy = self.lazy_reloads(self.now()) != 0;
            let on = !self.eager.ie & value & self.eager.iflags & INT_TIMER != 0;
            self.cov.ie_on_lazy += u32::from(lazy && on);
            self.log += &format!(" [{}] ie={value:02x}", self.now());
            self.ctx.write_mapped(0xFFFF, value);
            self.eager.ie = value;
            self.check("write IE");
            self.idle();
        }

        /// The last M-cycle of a dispatch (`Ref::acknowledge`).
        fn acknowledge(&mut self, push: Option<(u16, u8)>) {
            self.log += &format!(" [{}] ack{push:x?}", self.now());
            let (addr, value) = push.unwrap_or((0xC000, 0));
            let ie = self.ctx.sys.ie;
            let got = Bus::push_acknowledge(&mut self.ctx, addr, value, ie);
            let pending = self.eager.acknowledge(push);
            assert_eq!(got & INT_TIMER, pending, "log:{}", self.log);
            self.cov.ack += u32::from(pending != 0);
            self.cov.ack_ie_off += u32::from(pending != 0 && self.eager.ie == 0);
            self.check("acknowledge");
        }

        /// One M-cycle with no access.
        fn idle(&mut self) {
            self.ctx.idle();
            self.eager.tick();
            self.check("tick");
        }

        /// One M-cycle whose access writes FF00 | `reg`.
        fn write(&mut self, reg: u8, value: u8) {
            let (now, t) = (self.now(), &self.timer());
            let was_a = t.in_cycle_a(now);
            let was_b = t.reloaded_at == now && !was_a;
            self.cov.write_a += u32::from(was_a);
            self.cov.write_b += u32::from(was_b);
            let serial_lazy = u32::from(self.serial_lazy());
            self.cov.serial_lazy_write += u32::from(reg < 4) * serial_lazy;
            self.cov.serial_lazy_div += u32::from(reg == 4) * serial_lazy;
            if (4..=7).contains(&reg) {
                let lazy = self.lazy_reloads(now);
                self.cov.lazy_a += u32::from(was_a && lazy != 0);
                self.cov.lazy_b += u32::from(was_b && lazy != 0);
                self.cov.lazy_multi += u32::from(lazy >= 2);
            }
            self.log += &format!(" [{}] w{reg}={value:02x}", self.now());
            self.ctx.write_mapped(0xFF00 | u16::from(reg), value);
            self.eager.write(reg, value);
            self.cov.glitch_overflow += u32::from(!was_a && self.timer().in_cycle_a(now));
            self.check("write");
            self.idle();
        }

        fn stop(&mut self, switch: bool) {
            let (now, t) = (self.now(), &self.timer());
            self.cov.stop_in_window += u32::from(t.in_cycle_a(now) || t.reloaded_at == now);
            self.cov.switch += u32::from(switch);
            let serial_lazy = u32::from(self.serial_lazy());
            self.cov.serial_lazy_stop += u32::from(!switch) * serial_lazy;
            self.cov.serial_lazy_switch += u32::from(switch) * serial_lazy;
            self.log += &format!(
                " [{}] stop{}",
                self.now(),
                if switch { "+switch" } else { "" }
            );
            self.ctx.sys.key1 = u8::from(switch);
            let kind = crate::cpu::Bus::stop_begin(&mut self.ctx);
            assert_eq!(kind == crate::cpu::StopKind::Switch, switch);
            // With an interrupt pending (no halt) and IME on: the only
            // switch this harness can mirror per M-cycle. The held DIV
            // reset's M-cycle is an idle, as the dispatch's first M-cycle
            // that follows on hardware.
            if switch {
                if crate::cpu::Bus::speed_switch(&mut self.ctx, true, true) {
                    crate::cpu::Bus::idle(&mut self.ctx);
                }
                crate::cpu::Bus::idle(&mut self.ctx);
            }
            if !switch {
                // STOP mode stops the counter: for the eager model, whose
                // M-cycles are its counter's ticks, no time passes (after
                // its DIV reset, below). Mostly ended on the same clock (a
                // selected button pressed at once): a freeze and thaw
                // with no time between must change nothing.
                let n = if self.rng.below(3) == 0 {
                    self.rng.below(400)
                } else {
                    0
                };
                self.log += &format!("({n})");
                let running = self.ctx.sys.serial.running();
                self.cov.serial_stopped += u32::from(n >= 100 && running);
                for _ in 0..n {
                    assert!(!self.ctx.stop_wake());
                    self.ctx.idle();
                    let s = &self.ctx.sys.sched;
                    assert_eq!(s.deadline(EventKind::Serial), NEVER);
                }
                self.eager.clock += n * self.eager.mcycle;
                let s = &mut *self.ctx.sys;
                let saved = (s.buttons, s.joyp_select);
                (s.buttons, s.joyp_select) = (crate::Buttons::A, 0);
                assert!(self.ctx.stop_wake());
                let s = &mut *self.ctx.sys;
                (s.buttons, s.joyp_select) = saved;
            }
            self.eager.stop(switch);
            self.check("stop");
            if !switch {
                // The stopped CPU idles: STOP's DIV reset was this
                // M-cycle's access.
                self.idle();
            }
        }
    }

    fn random_tima(rng: &mut Rng) -> u8 {
        if rng.below(3) == 0 {
            rng.byte()
        } else {
            0xF8 | rng.byte()
        }
    }

    fn random_write(rng: &mut Rng) -> (u8, u8) {
        match rng.below(12) {
            0 => (1, rng.byte()),
            1 | 2 => {
                let v = [0x81, 0x83, 0x80, 0x01, 0x00, 0x82][rng.below(6) as usize];
                (2, v | (rng.byte() & 0x7C))
            }
            3 | 4 => (4, rng.byte()),
            5 | 6 => (5, random_tima(rng)),
            7 | 8 => (6, random_tima(rng)),
            // TAC: mostly enabled, all four rates.
            _ => (
                7,
                (rng.byte() & 0xF8) | rng.below(8) as u8 | if rng.below(4) != 0 { 4 } else { 0 },
            ),
        }
    }

    fn run_sequence(rng: &mut Rng, cov: &mut Coverage) {
        let mut mem = Box::new(Memory::new());
        let mut sys = Box::new(Sys::new());
        let cgb = rng.below(4) != 0;
        sys.cgb_mode = cgb;
        let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
        let ctx = Ctx::new(
            &mut mem,
            &mut sys,
            &mut frame,
            &mut [],
            Events::EMPTY,
            u64::MAX,
        );
        let mut p = Pair {
            ctx,
            eager: Ref {
                timer: Eager::new(),
                serial: Serial::new(),
                iflags: 0,
                tx: false,
                ie: 0,
                mcycle: 4,
                clock: 0,
                cgb,
            },
            log: String::new(),
            cov,
            clear_1_in: [1, 1, 4, 64, 0][rng.below(5) as usize],
            rng: Rng(rng.next() | 1),
        };
        // Start somewhere in the counter's cycle, with the timer running.
        for _ in 0..rng.below(300) {
            p.ctx.idle();
            p.eager.tick();
        }
        p.check("start");
        p.write(7, 4 | rng.below(4) as u8);
        p.write(6, random_tima(rng));
        p.write(5, random_tima(rng));
        for _ in 0..40 {
            match rng.below(30) {
                0..=5 => {
                    for _ in 0..rng.below(9) {
                        p.idle();
                    }
                }
                6..=13 => {
                    // Up to the M-cycle before, of (cycle A) or after (cycle
                    // B) the next overflow, then a write there.
                    let o = p.timer().overflow_at;
                    let m = p.eager.mcycle;
                    let target = o.saturating_add(m * rng.below(3)).saturating_sub(m);
                    if o != NEVER && target >= p.now() && target - p.now() < 600 * m {
                        while p.now() < target {
                            p.idle();
                        }
                    }
                    let (reg, v) = random_write(rng);
                    p.write(reg, v);
                }
                14..=17 => {
                    let (reg, v) = random_write(rng);
                    p.write(reg, v);
                }
                18 | 19 => {
                    let switch = cgb && rng.below(2) == 0;
                    p.stop(switch);
                }
                20..=22 => p.write_if(rng.byte(), rng.below(3) == 0),
                // Mostly with IE bit 2 set: something to acknowledge.
                23 | 24 => {
                    if rng.below(4) != 0 && p.eager.ie == 0 {
                        p.write_ie(INT_TIMER);
                    }
                    p.acknowledge(match rng.below(4) {
                        0 => Some((0xFF0F, rng.byte())),
                        1 => Some((0xFFFF, INT_TIMER * rng.below(2) as u8)),
                        _ => None,
                    });
                }
                25..=27 => p.write_ie(INT_TIMER * rng.below(2) as u8),
                _ => p.gap(100 + rng.below(1500)),
            }
        }
    }

    #[test]
    fn lazy_timer_matches_the_eager_reference() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        let mut cov = Coverage::default();
        for _ in 0..2000 {
            run_sequence(&mut rng, &mut cov);
        }
        let c = &cov;
        let floors = [
            c.write_a,
            c.write_b,
            c.glitch_overflow,
            c.stop_in_window,
            c.switch,
            c.serial_done,
            c.ack,
            c.ack_ie_off,
            c.if_set,
        ];
        assert!(floors.iter().all(|&n| n >= 20), "{cov:?}");
        assert!(c.serial_stopped >= 20, "{cov:?}");
        // The lazy catch-up's cases. With the `eager-sync` test feature
        // the timer and the serial port are caught up every M-cycle, so
        // none is ever reached there (the comparison above still ran).
        let lazy = [
            c.lazy_a,
            c.lazy_b,
            c.lazy_multi,
            c.ie_on_lazy,
            c.if_clear_lazy,
            c.serial_lazy,
            c.serial_lazy_write,
            c.serial_lazy_div,
            c.serial_lazy_stop,
            c.serial_lazy_switch,
        ];
        assert!(
            cfg!(feature = "eager-sync") || lazy.iter().all(|&n| n >= 20),
            "{cov:?}"
        );
    }

    /// Every field `Timer::catch_up` may touch, and the rest.
    fn fields(t: &Timer) -> [u64; 10] {
        [
            t.base,
            t.base_clock,
            u64::from(t.speed.counter_shift()),
            u64::from(t.tima),
            t.tima_from,
            u64::from(t.tma),
            u64::from(t.tac),
            t.tima_bit,
            t.overflow_at,
            t.reloaded_at,
        ]
    }

    /// A running timer at a random point, in either speed; returns it
    /// with the clock (an M-cycle boundary).
    fn random_timer(rng: &mut Rng) -> (Timer, u64) {
        let mut t = Timer::new();
        let mut now = 4 * rng.below(1000);
        if rng.below(2) == 0 {
            t.speed_switch(now, 2 * rng.below(3), false);
            now += 2 * rng.below(8);
        }
        t.write_tac(now, 4 | rng.below(4) as u8);
        t.write_tma(now, random_tima(rng));
        t.write_tima(now, random_tima(rng));
        (t, now)
    }

    /// `catch_up` leaves exactly what the reloads, run one by one at
    /// their own clocks (as the Timer event did), leave; and every access
    /// that changes the timer catches up by itself: one timer only ever
    /// gets the accesses, the other the reloads first.
    #[test]
    fn catch_up_matches_the_reloads_one_by_one() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        let (mut some, mut many) = (0, 0);
        for _ in 0..4000 {
            let (mut lazy, mut now) = random_timer(&mut rng);
            let mut eager = lazy;
            for _ in 0..8 {
                let m = eager.mcycle();
                now += m * match rng.below(8) {
                    0 | 1 => rng.below(8),
                    2..=4 => rng.below(700),
                    5 => rng.below(40_000),
                    _ if eager.deadline() == NEVER => rng.below(8),
                    // On, or an M-cycle either side of, a reload.
                    _ => {
                        (eager.deadline().saturating_sub(now) / m + rng.below(3)).saturating_sub(1)
                    }
                };
                let mut n = 0;
                while eager.deadline() <= now {
                    eager.reload(eager.deadline());
                    n += 1;
                }
                some += u32::from(n == 1);
                many += u32::from(n > 1);
                let mut caught_up = lazy;
                caught_up.catch_up(now);
                assert_eq!(fields(&caught_up), fields(&eager), "{n} reloads to {now}");
                assert_eq!(lazy.read(now, 5), eager.read(now, 5));
                let (op, v) = (rng.below(9), random_tima(&mut rng));
                let pause = (2 + 2 * u64::from(eager.speed.counter_shift())) * rng.below(3);
                // To single speed: the halt that moves the CPU's grid.
                let short = eager.speed.is_double() && rng.below(2) == 0;
                let mut results = [0; 2];
                for (t, r) in [&mut lazy, &mut eager].into_iter().zip(&mut results) {
                    match op {
                        0 => t.write_tma(now, v),
                        1 => t.write_tac(now, v & 7),
                        2 => t.write_tima(now, v),
                        3 => *r = t.write_div(now),
                        // STOP: the edge, then the DIV reset schedules.
                        4 => {
                            t.stop_edge(now);
                            *r = t.write_div(now);
                        }
                        5 => t.speed_switch(now, pause, short),
                        _ => {}
                    }
                }
                assert_eq!(results[0], results[1], "op {op} at {now}");
                if op < 6 {
                    assert_eq!(fields(&lazy), fields(&eager), "op {op} at {now}");
                }
                if op == 5 {
                    // The next access is after the pause, as on the bus
                    // (the CPU can't reach another STOP inside it).
                    now += pause + 3 * u64::from(short);
                }
            }
        }
        assert!(some >= 1000 && many >= 1000, "{some} {many}");
    }

    /// The same over gaps too long to run one by one (the fall count
    /// passes 2^32 after 4.5 h at 262 kHz): the last reload is a whole
    /// number of periods after the first, at or before the clock and
    /// less than a period before it, and the next one a period later.
    #[test]
    fn catch_up_over_a_very_long_gap() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..4000 {
            let (mut t, start) = random_timer(&mut rng);
            let m = t.mcycle();
            let first = t.deadline();
            // Up to 2^50 clocks (8 years).
            let now = start + m * (rng.next() >> (16 + rng.below(40)));
            let period = ((256 - u64::from(t.tma)) * 2 * t.tima_bit) >> t.speed.counter_shift();
            let mut two_steps = t;
            two_steps.catch_up(start + m * rng.below((now - start) / m + 1));
            two_steps.catch_up(now);
            t.catch_up(now);
            assert_eq!(fields(&t), fields(&two_steps));
            if first > now {
                continue;
            }
            assert!(t.reloaded_at <= now && now - t.reloaded_at < period);
            assert_eq!((t.reloaded_at - first) % period, 0);
            assert_eq!(t.deadline(), t.reloaded_at + period);
            assert_eq!((t.tima, t.tima_from), (t.tma, t.sc(t.reloaded_at)));
        }
    }

    #[test]
    fn rem_is_the_remainder() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        for p in 1..=256u32 {
            for _ in 0..200 {
                let x = rng.next() >> rng.below(64);
                assert_eq!(crate::timer::rem(x, p), x % u64::from(p), "{x} % {p}");
            }
            assert_eq!(crate::timer::rem(u64::MAX, p), u64::MAX % u64::from(p));
        }
    }

    /// A switch to single speed with nothing pending, in cycle A: the
    /// CPU's M-cycle grid moves by 3 clocks (`Ctx::speed_switch`), and the
    /// reload that ends cycle A is at that first boundary, 3 clocks after
    /// the switch, whatever the DIV-reset hold (IME). The values are those
    /// of the eager-event implementation (ADR 0011 as first built), which
    /// ran the reload from the deadline set before the switch. The same
    /// with the timer interrupt already requested, where no event runs.
    #[test]
    fn switch_to_single_speed_in_cycle_a_reloads_at_the_moved_grid() {
        for (ime, pending) in [(false, false), (true, false), (false, true), (true, true)] {
            let mut mem = Box::new(Memory::new());
            let mut sys = Box::new(Sys::new());
            sys.cgb_mode = true;
            let mut frame: Box<Frame> = Box::new([0; 160 * 144]);
            let mut ctx = Ctx::new(
                &mut mem,
                &mut sys,
                &mut frame,
                &mut [],
                Events::EMPTY,
                u64::MAX,
            );
            // To double speed, then TIMA one edge from overflowing.
            ctx.sys.key1 = 1;
            assert_eq!(Bus::stop_begin(&mut ctx), crate::cpu::StopKind::Switch);
            if Bus::speed_switch(&mut ctx, true, true) {
                ctx.idle();
            }
            ctx.idle();
            for (reg, v) in [(0xFF07, 0x05), (0xFF06, 0xF0), (0xFF05, 0xFF)] {
                ctx.write_mapped(reg, v);
                ctx.idle();
            }
            while !ctx.sys.timer.in_cycle_a(ctx.clock()) {
                ctx.idle();
            }
            let now = ctx.clock();
            assert_eq!(ctx.sys.iflags & INT_TIMER, 0);
            if pending {
                // The request already pending (IE = 0): no Timer event.
                ctx.write_mapped(0xFF0F, INT_TIMER);
            }
            ctx.sys.key1 = 1;
            assert_eq!(Bus::stop_begin(&mut ctx), crate::cpu::StopKind::Switch);
            assert!(Bus::speed_switch(&mut ctx, ime, false));
            // 3 clocks on, at the first boundary of the moved grid: the
            // reload, and the interrupt request.
            assert_eq!(ctx.clock(), now + 3);
            assert_eq!(ctx.sys.iflags & INT_TIMER, INT_TIMER);
            let mut t = ctx.sys.timer;
            t.catch_up(now + 3);
            assert_eq!(t.reloaded_at, now + 3);
            // The next overflow: 16 edges of 16 clocks, the first one 4
            // clocks later with the DIV reset held (IME off).
            assert_eq!(t.overflow_at, now + if ime { 255 } else { 259 });
            let mut tima = [0; 5];
            for v in &mut tima {
                *v = read::<true>(ctx.mem, ctx.sys, &ctx.hot, 0xFF05);
                ctx.idle();
            }
            let f1_from = if ime { 3 } else { 4 };
            for (i, v) in tima.into_iter().enumerate() {
                assert_eq!(v, if i < f1_from { 0xF0 } else { 0xF1 }, "ime {ime} +{i}");
            }
        }
    }
}
