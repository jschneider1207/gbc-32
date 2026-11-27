//! The CPU clock measured against the 32.768 kHz LSE crystal, an on-chip
//! reference independent of the HSI, the HSE and the PC (plan D11: every
//! banner records the clock a run actually had).
//!
//! The NUCLEO's X2 crystal drives OSC32_IN/OUT by default (UM2407 Rev 6
//! section 7.5.2: SB40 and SB41 off, R38 and R39 on). TIM16's TI1 input
//! can be the LSE itself (RM0433 section 41.6.18, TIM16_TISEL TI1SEL =
//! 0b0010), so channel 1 in input capture mode (section 41.4.6) sets CC1IF
//! on every rising LSE edge. The CPU polls that flag and reads DWT_CYCCNT
//! at the first and the last edge of a window of [`PERIODS`] LSE periods:
//! the polling latency is the same at both ends, so it cancels to a few
//! dozen cycles in a window of about a billion. TIM16's own capture
//! registers give a second count of the same window in timer kernel clocks
//! (latched in hardware, no polling at all).

use crate::clock::{CPU_HZ, HSI_HZ, wait_for};
use crate::{cycles, pac};

/// Nominal LSE frequency.
pub const LSE_HZ: u32 = 32_768;
/// LSE periods per window: 2 s.
pub const PERIODS: u32 = 2 * LSE_HZ;

/// The oscillator the system clock is derived from, and its nominal
/// frequency: the measured CPU clock scaled by `REF.1 / CPU_HZ` is that
/// oscillator's real frequency (the PLL multiplies exactly).
pub const REF: (&str, u32) = ("hsi", HSI_HZ);

/// TIM16's kernel clock, nominal. RM0433 section 8.5.6, Table 57 (TIMPRE =
/// 0, its reset value): with D2PPRE2 /2 it is rcc_hclk1 (twice pclk2), and
/// HPRE /2 makes that CPU_HZ / 2.
pub const TIM_HZ: u32 = CPU_HZ / 2;

/// Starts the LSE (if it is not already running: it lives in the backup
/// domain, which keeps it across resets) and TIM16. Returns the CPU cycles
/// waited for LSERDY (0 if it was already ready), or `None` if it did not
/// come up within about 5 s (DS12117 Table 131, rev V: tSU typically 2 s).
pub fn start() -> Option<u32> {
    // SAFETY: the clock check owns PWR_CR1.DBP, RCC_BDCR's LSE bits,
    // RCC_APB2ENR.TIM16EN (set through `crate::rcc::modify`) and TIM16;
    // no second thread, and no interrupt handler touches PWR, RCC or
    // TIM16 (the table in the crate docs).
    let (pwr, rcc, tim) = unsafe { (&*pac::PWR::ptr(), &*pac::RCC::ptr(), &*pac::TIM16::ptr()) };
    // RCC_BDCR is write-protected after reset until PWR_CR1.DBP = 1
    // (RM0433 section 8.7.26; PWR_CR1 bit 8, section 6.8.1).
    pwr.cr1.modify(|_, w| w.dbp().set_bit());
    if !wait_for(1_000_000, || pwr.cr1.read().dbp().bit_is_set()) {
        return None;
    }
    let mut waited = 0;
    if rcc.bdcr.read().lserdy().bit_is_clear() {
        // Crystal, not bypass (LSEBYP may only be written while the LSE is
        // off and not ready: section 8.7.26). Medium-high drive (LSEDRV
        // 0b10) for a sure start: DS12117 gives the critical gm per drive
        // level (Table 131) and UM2407 does not name the crystal. ES0392
        // section 2.2.15 (swapped LSEDRV bits) is revision Y only; this
        // board is revision V.
        if rcc.bdcr.read().lseon().bit_is_clear() {
            rcc.bdcr
                .modify(|_, w| w.lsebyp().clear_bit().lsedrv().medium_high());
        }
        rcc.bdcr.modify(|_, w| w.lseon().set_bit());
        let t0 = cycles();
        // 5 s at the nominal clock; at 480 MHz still under 2^32.
        let limit = CPU_HZ / 4 * 20;
        loop {
            if rcc.bdcr.read().lserdy().bit_is_set() {
                break;
            }
            if cycles().wrapping_sub(t0) > limit {
                return None;
            }
        }
        waited = cycles().wrapping_sub(t0);
    }
    // TIM16 (APB2, RCC_APB2ENR bit 17, section 8.7.47): TI1 = LSE, CC1 an
    // input capture of TI1 (CC1S = 0b01), no filter, no input prescaler,
    // rising edges (CC1P = 0), counter free-running over 16 bits at the
    // kernel clock (PSC 0, ARR 0xFFFF).
    crate::rcc::modify(|rcc| {
        rcc.apb2enr.modify(|_, w| w.tim16en().set_bit());
        let _ = rcc.apb2enr.read();
    });
    tim.cr1.write(|w| w.cen().clear_bit());
    // SAFETY: 0b0010 is the documented LSE value of TI1SEL (section
    // 41.6.18), 0b01 the documented "IC1 mapped on TI1" value of CC1S, and
    // any 16-bit PSC and ARR is valid.
    unsafe {
        tim.tim16_tisel.write(|w| w.ti1sel().bits(0b0010));
        tim.ccmr1_input()
            .write(|w| w.cc1s().bits(0b01).ic1psc().bits(0).ic1f().bits(0));
        tim.psc.write(|w| w.psc().bits(0));
        tim.arr.write(|w| w.arr().bits(0xFFFF));
    }
    tim.ccer.write(|w| w.cc1e().set_bit());
    tim.egr.write(|w| w.ug().set_bit());
    tim.cr1.write(|w| w.cen().set_bit());
    Some(waited)
}

/// One window: the CPU cycles and the TIM16 kernel clocks between two
/// rising LSE edges [`PERIODS`] apart.
#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub cycles: u32,
    pub tim_clocks: u32,
    /// CC1OF seen: an edge was captured while the previous one was still
    /// unread, so the window may be short of an edge.
    pub overcapture: bool,
}

impl Window {
    /// The CPU clock this window measured, in Hz (rounded).
    pub fn cpu_hz(&self) -> u32 {
        let periods = u64::from(PERIODS);
        ((u64::from(self.cycles) * u64::from(LSE_HZ) + periods / 2) / periods) as u32
    }
}

/// Measures one window; `None` if an edge did not come within about 10 ms
/// (1/100 s at the nominal clock; the period is 30.5 us). Needs [`start`].
pub fn window() -> Option<Window> {
    // SAFETY: as in `start`.
    let tim = unsafe { &*pac::TIM16::ptr() };
    let limit = CPU_HZ / 100;
    // Waits for the next capture; returns (DWT at detection, CCR1).
    // Reading CCR1 clears CC1IF (section 41.6.4, TIMx_SR).
    let edge = || {
        let t = cycles();
        loop {
            if tim.sr.read().cc1if().bit_is_set() {
                let now = cycles();
                return Some((now, tim.ccr[0].read().bits() as u16));
            }
            if cycles().wrapping_sub(t) > limit {
                return None;
            }
        }
    };
    // Drop a stale capture, clear the flags (rc_w0: write 0).
    let _ = tim.ccr[0].read();
    // SAFETY: writing 0 to TIMx_SR clears every flag; no bit is set.
    tim.sr.write(|w| unsafe { w.bits(0) });
    let (t0, c0) = edge()?;
    let (mut t1, mut prev) = (t0, c0);
    let mut tim_clocks = 0u32;
    for _ in 0..PERIODS {
        let (t, c) = edge()?;
        // Each period is under 2^16 kernel clocks (7,202 at 236 MHz).
        tim_clocks += u32::from(c.wrapping_sub(prev));
        prev = c;
        t1 = t;
    }
    Some(Window {
        cycles: t1.wrapping_sub(t0),
        tim_clocks,
        overcapture: tim.sr.read().cc1of().bit_is_set(),
    })
}

/// The CPU clock measured over one window, in Hz: [`start`] then
/// [`window`]. `None` if the LSE did not start or an edge did not come, or
/// the window saw an overcapture (it may be short of an edge). Takes about
/// 2 s, plus the LSE's start-up after a power cycle (typically 2 s).
pub fn cpu_hz() -> Option<u32> {
    start()?;
    let w = window()?;
    (!w.overcapture).then(|| w.cpu_hz())
}
