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
//! DIV, TIMA, TMA, TAC, SB, SC, the timer and serial IF bits, and the
//! Timer and Serial deadlines against the eager model's next reload and
//! next serial shift (found by ticking a copy ahead).

// Inline so the hard-rules test sees `std` inside a `#[cfg(test)] mod`.
#[cfg(test)]
mod tests {
    extern crate std;

    use crate::bus::{Ctx, Sys, read};
    use crate::cpu::Bus;
    use crate::memory::Memory;
    use crate::schedule::{EventKind, NEVER};
    use crate::serial::{INT_SERIAL, Serial};
    use crate::timer::reference::Timer as Eager;
    use crate::{Events, Frame};
    use std::boxed::Box;
    use std::format;
    use std::string::String;

    use crate::timer::INT_TIMER;

    /// The eager machine: the reference timer and the serial port, ticked
    /// per M-cycle as the bus did before ADR 0011 step 4.
    #[derive(Clone)]
    struct Ref {
        timer: Eager,
        serial: Serial,
        iflags: u8,
        mcycle: u64,
        clock: u64,
        cgb: bool,
    }

    impl Ref {
        fn clock_serial(&mut self, fell: u16) {
            if self.serial.clock(u64::from(fell)) {
                self.iflags |= INT_SERIAL;
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
            // IE = 0 in this harness: STOP is two bytes.
            self.timer.stop_edge();
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

        fn read(&self, reg: u8) -> u8 {
            match reg {
                1 => self.serial.sb,
                2 => self.serial.read_sc(self.cgb),
                _ => self.timer.read(reg),
            }
        }

        /// The clocks of the next reload (timer IF) and serial shift within
        /// `w` M-cycles, by ticking a copy.
        fn next_events(&self, w: u32) -> (u64, u64) {
            let mut c = self.clone();
            let (mut timer, mut serial) = (NEVER, NEVER);
            for _ in 0..w {
                c.iflags = 0;
                let before = (c.serial.bits, c.serial.sc);
                c.tick();
                if c.iflags & INT_TIMER != 0 && timer == NEVER {
                    timer = c.clock;
                }
                if (c.serial.bits, c.serial.sc) != before && serial == NEVER {
                    serial = c.clock;
                }
            }
            (timer, serial)
        }
    }

    /// xorshift64 (Marsaglia), fixed seed: no dependency, reproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
        fn byte(&mut self) -> u8 {
            self.next() as u8
        }
    }

    /// Look-ahead window for the deadline comparison, in M-cycles.
    const WINDOW: u32 = 8;

    struct Pair<'r, 'a> {
        ctx: Ctx<'r, 'a>,
        eager: Ref,
        log: String,
        cov: &'r mut Coverage,
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
    }

    impl Pair<'_, '_> {
        fn now(&self) -> u64 {
            self.ctx.clock()
        }

        fn check(&mut self, at: &str) {
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
            ] {
                let (l, e) = (lazy_read(reg), self.eager.read(reg));
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
            let now = c.clock();
            let horizon = now + u64::from(WINDOW) * self.eager.mcycle;
            let (et, es) = self.eager.next_events(WINDOW);
            for (name, kind, e) in [
                ("Timer", EventKind::Timer, et),
                ("Serial", EventKind::Serial, es),
            ] {
                let d = c.sys.sched.deadline(kind);
                let l = if d <= horizon { d } else { NEVER };
                if l != e {
                    fail += &format!("{name} deadline {d} vs eager next {e}; ");
                }
            }
            assert!(
                fail.is_empty(),
                "{at} at clock {now}: {fail}\nlog:{}",
                self.log
            );
            // Clear IF so every request is pinned to its M-cycle.
            self.ctx.sys.iflags = 0;
            self.eager.iflags = 0;
        }

        /// One M-cycle with no access.
        fn idle(&mut self) {
            self.ctx.idle();
            self.eager.tick();
            self.check("tick");
        }

        /// One M-cycle whose access writes FF00 | `reg`.
        fn write(&mut self, reg: u8, value: u8) {
            let (now, t) = (self.now(), &self.ctx.sys.timer);
            let was_a = t.in_cycle_a(now);
            self.cov.write_a += u32::from(was_a);
            self.cov.write_b += u32::from(t.reloaded_at == now && !was_a);
            self.log += &format!(" [{}] w{reg}={value:02x}", self.now());
            self.ctx.write_mapped(0xFF00 | u16::from(reg), value);
            self.eager.write(reg, value);
            self.cov.glitch_overflow += u32::from(!was_a && self.ctx.sys.timer.in_cycle_a(now));
            self.check("write");
            self.idle();
        }

        fn stop(&mut self, switch: bool) {
            let (now, t) = (self.now(), &self.ctx.sys.timer);
            self.cov.stop_in_window += u32::from(t.in_cycle_a(now) || t.reloaded_at == now);
            self.cov.switch += u32::from(switch);
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
                mcycle: 4,
                clock: 0,
                cgb,
            },
            log: String::new(),
            cov,
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
            match rng.below(20) {
                0..=5 => {
                    for _ in 0..rng.below(9) {
                        p.idle();
                    }
                }
                6..=13 => {
                    // Up to the M-cycle before, of (cycle A) or after (cycle
                    // B) the next overflow, then a write there.
                    let o = p.ctx.sys.timer.overflow_at;
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
                _ => {
                    let switch = cgb && rng.below(2) == 0;
                    p.stop(switch);
                }
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
        ];
        assert!(floors.iter().all(|&n| n >= 20), "{cov:?}");
    }
}
