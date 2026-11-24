//! Display refresh timing from the Windows compositor (DWM), for
//! `GBC_FRAME_LOG`: which refresh a present landed on. Only a query
//! (`DwmGetCompositionTimingInfo`); nothing here renders. Elsewhere
//! [`now`] and [`refresh_rate`] return `None`.
//!
//! Since Windows 8.1 the query takes no window and reports the
//! compositor's global clock: the time of the vertical blank of its last
//! composition (`qpcVBlank`) and the refresh period (`qpcRefreshPeriod`),
//! on the QPC clock. Refreshes are numbered from time alone, from the
//! first vblank seen: `cRefresh` is not used, because measured on Windows
//! 11 it counts compositions, not vblanks (over 3,000 presents at 59.725/s
//! on a 59.973 Hz display it advanced exactly once per present, hiding
//! every repeated refresh). Per-window present statistics are not
//! available to a GDI (softbuffer) window, so where a frame lands is an
//! estimate: see [`Refresh`].

/// Where "now" sits on the display's refresh grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Refresh {
    /// The refresh period, in µs.
    pub period_us: f64,
    /// The number of the last vertical blank at or before now, counted
    /// in periods from the first vblank seen (rounded at each DWM vblank,
    /// then extrapolated with the period).
    pub count: u64,
    /// Time since that vertical blank, in µs (`0..period_us`).
    pub phase_us: f64,
}

/// `count` and `phase_us` for a time `qpc`, given a recent vertical blank
/// at `vblank_qpc`, vblank 0 at `anchor_qpc` and the period, in QPC ticks
/// (`freq` per second). `None` if the period is 0.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn on_grid(
    qpc: i64,
    vblank_qpc: i64,
    anchor_qpc: i64,
    period: u64,
    freq: u64,
) -> Option<Refresh> {
    if period == 0 || freq == 0 {
        return None;
    }
    let p = i128::from(period);
    let from_anchor = i128::from(vblank_qpc) - i128::from(anchor_qpc);
    let vblank_count = (from_anchor + p / 2).div_euclid(p);
    let since = i128::from(qpc) - i128::from(vblank_qpc);
    let n = since.div_euclid(i128::from(period));
    let phase = since.rem_euclid(i128::from(period));
    let to_us = |ticks: i128| ticks as f64 * 1e6 / freq as f64;
    Some(Refresh {
        period_us: to_us(i128::from(period)),
        count: (vblank_count + n).max(0) as u64,
        phase_us: to_us(phase),
    })
}

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicI64, Ordering};

    use windows_sys::Win32::Graphics::Dwm::{DWM_TIMING_INFO, DwmGetCompositionTimingInfo};
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };

    use super::Refresh;

    fn freq() -> u64 {
        static F: OnceLock<u64> = OnceLock::new();
        *F.get_or_init(|| {
            let mut f = 0i64;
            // SAFETY: `f` is a valid, writable i64 for the call.
            unsafe { QueryPerformanceFrequency(&mut f) };
            f.max(0) as u64
        })
    }

    fn timing() -> Option<DWM_TIMING_INFO> {
        let mut info = DWM_TIMING_INFO {
            cbSize: size_of::<DWM_TIMING_INFO>() as u32,
            ..Default::default()
        };
        // SAFETY: a null HWND is what Windows 8.1+ requires (global
        // timing); `info` is a valid, writable DWM_TIMING_INFO with
        // `cbSize` set, which the call only writes into.
        let hr = unsafe { DwmGetCompositionTimingInfo(std::ptr::null_mut(), &mut info) };
        (hr >= 0).then_some(info)
    }

    pub fn now() -> Option<Refresh> {
        let info = timing()?;
        let mut qpc = 0i64;
        // SAFETY: `qpc` is a valid, writable i64 for the call.
        unsafe { QueryPerformanceCounter(&mut qpc) };
        // Copies: the struct is packed, so no references into it.
        let (vblank, period) = (info.qpcVBlank as i64, info.qpcRefreshPeriod);
        static ANCHOR: AtomicI64 = AtomicI64::new(0);
        let anchor = match ANCHOR.compare_exchange(0, vblank, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => vblank,
            Err(a) => a,
        };
        super::on_grid(qpc, vblank, anchor, period, freq())
    }

    pub fn refresh_rate() -> Option<(u32, u32)> {
        let r = timing()?.rateRefresh;
        let (n, d) = (r.uiNumerator, r.uiDenominator);
        (d != 0).then_some((n, d))
    }
}

#[cfg(not(windows))]
mod imp {
    use super::Refresh;

    pub fn now() -> Option<Refresh> {
        None
    }

    pub fn refresh_rate() -> Option<(u32, u32)> {
        None
    }
}

/// Where now is on the display's refresh grid (Windows, compositor on).
pub fn now() -> Option<Refresh> {
    imp::now()
}

/// The display refresh rate DWM reports, as numerator / denominator Hz.
pub fn refresh_rate() -> Option<(u32, u32)> {
    imp::refresh_rate()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_position() {
        // 10 MHz QPC, 60 Hz: period 166,667 ticks; vblank 100 at tick 1e6.
        let p = 166_667;
        let anchor = 1_000_000 - 100 * p as i64;
        let r = on_grid(
            1_000_000 + 2 * p as i64 + 50_000,
            1_000_000,
            anchor,
            p,
            10_000_000,
        )
        .unwrap();
        assert_eq!(r.count, 102);
        assert!((r.phase_us - 5000.0).abs() < 1e-6, "{r:?}");
        assert!((r.period_us - 16_666.7).abs() < 1e-6, "{r:?}");
        // DWM's last vblank can be a little after the QPC read.
        let r = on_grid(1_000_000 - 10_000, 1_000_000, anchor, p, 10_000_000).unwrap();
        assert_eq!(r.count, 99);
        assert!((r.phase_us - 15_666.7).abs() < 1e-6, "{r:?}");
        // A later vblank (a few ticks of timestamp jitter) keeps its number,
        // however many vblanks had no composition in between.
        let v = 1_000_000 + 7 * p as i64 - 30;
        let r = on_grid(v + 10, v, anchor, p, 10_000_000).unwrap();
        assert_eq!(r.count, 107);
        assert_eq!(on_grid(0, 0, 0, 0, 10_000_000), None);
    }
}
