//! Precise waits for the window loop (ADR 0024, amendment "presentation
//! phase"). The event loop's timed wake-ups run late by 0.4 ms on
//! average and up to 4 ms (measured on Windows 11), which moves frame
//! starts and timed presents. For a deadline that matters, the loop asks
//! to wake [`SPIN_MARGIN`] early ([`wake_for`]) and spins the rest
//! ([`spin_target`], [`spin_until`]), yielding to other threads while it
//! does. A wake-up later than the margin is simply late, as before.

use std::time::{Duration, Instant};

/// How early the loop wakes for a precise deadline: above the measured
/// p99 lateness (1.75 ms), so almost every wake-up lands before it.
pub const SPIN_MARGIN: Duration = Duration::from_millis(2);

/// Spin at most this much more than the margin (a wake-up a little
/// earlier than asked, e.g. by an event); further off, wait again.
const SPIN_SLACK: Duration = Duration::from_micros(500);

/// When to wake for a deadline that should be met precisely.
pub fn wake_for(deadline: Instant, now: Instant) -> Instant {
    deadline
        .checked_sub(SPIN_MARGIN)
        .unwrap_or(deadline)
        .max(now)
}

/// The deadline to spin to at `now`, if it is ahead and near enough.
pub fn spin_target(now: Instant, deadline: Option<Instant>) -> Option<Instant> {
    let d = deadline?;
    (d > now && d - now <= SPIN_MARGIN + SPIN_SLACK).then_some(d)
}

/// Spins until `deadline` (at most [`SPIN_MARGIN`] plus a little when
/// called with a [`spin_target`]), yielding the CPU to any other ready
/// thread on each turn. Returns how long it spun.
pub fn spin_until(deadline: Instant) -> Duration {
    let start = Instant::now();
    let mut now = start;
    while now < deadline {
        for _ in 0..64 {
            std::hint::spin_loop();
        }
        std::thread::yield_now();
        now = Instant::now();
    }
    now - start
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: fn(u64) -> Duration = Duration::from_micros;

    #[test]
    fn wakes_a_margin_early_never_in_the_past() {
        let t = Instant::now() + Duration::from_secs(1);
        assert_eq!(wake_for(t, t - US(10_000)), t - SPIN_MARGIN);
        assert_eq!(
            wake_for(t, t - US(1_000)),
            t - US(1_000),
            "already in the margin"
        );
        assert_eq!(wake_for(t, t + US(5)), t + US(5), "already due");
    }

    #[test]
    fn spins_only_to_a_near_future_deadline() {
        let t = Instant::now() + Duration::from_secs(1);
        assert_eq!(spin_target(t - US(1_500), Some(t)), Some(t));
        assert_eq!(spin_target(t - SPIN_MARGIN, Some(t)), Some(t));
        assert_eq!(spin_target(t - SPIN_MARGIN - US(400), Some(t)), Some(t));
        assert_eq!(
            spin_target(t - US(5_000), Some(t)),
            None,
            "woken early: wait again"
        );
        assert_eq!(spin_target(t, Some(t)), None, "due: no spin");
        assert_eq!(spin_target(t + US(300), Some(t)), None, "late: no spin");
        assert_eq!(spin_target(t, None), None);
    }

    /// Deadlines every 16.74 ms; the OS wakes each requested wake-up 0 to
    /// `max_late` µs late (and every 50th 4 ms late). Returns how late the
    /// work started against each deadline, in µs (negative = early).
    fn simulate(precise: bool, max_late: u64) -> Vec<i64> {
        let t0 = Instant::now();
        let mut lcg = 3u64;
        let mut late = Vec::new();
        for i in 0..1000u32 {
            let deadline = t0 + Duration::from_secs(1) + Duration::from_micros(16_744) * i;
            let before = deadline - US(16_000);
            let asked = if precise {
                wake_for(deadline, before)
            } else {
                deadline
            };
            lcg = lcg.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let os_late = if i % 50 == 49 {
                4_000
            } else {
                (lcg >> 33) % (max_late + 1)
            };
            let woke = asked + US(os_late);
            // A spin ends exactly at its target (`spin_until` is checked
            // on the real clock below).
            let start = match spin_target(woke, precise.then_some(deadline)) {
                Some(t) => t,
                None => woke,
            };
            late.push(if start >= deadline {
                (start - deadline).as_micros() as i64
            } else {
                -((deadline - start).as_micros() as i64)
            });
        }
        late
    }

    #[test]
    fn precise_waits_meet_deadlines_the_os_would_miss() {
        let late = simulate(true, 1_750);
        let on_time = late.iter().filter(|&&l| l == 0).count();
        assert_eq!(on_time, 980, "all but the 4 ms outliers");
        assert!(late.iter().all(|&l| l == 0 || l == 2_000), "{late:?}");
        let plain = simulate(false, 1_750);
        assert!(plain.iter().filter(|&&l| l == 0).count() < 50);
    }

    #[test]
    fn spin_until_returns_at_the_deadline() {
        let t = Instant::now() + US(300);
        let spun = spin_until(t);
        assert!(Instant::now() >= t);
        assert!(spun <= US(300) + Duration::from_millis(50), "{spun:?}");
        assert_eq!(spin_until(Instant::now() - US(1)), Duration::ZERO);
    }
}
