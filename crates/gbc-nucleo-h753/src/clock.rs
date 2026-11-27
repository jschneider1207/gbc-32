//! The system clock: 472 MHz from the HSI at VOS0 (plan D11).
//!
//! DS12117 Rev 10 Table 111 (rev V): VOS0 allows a 480 MHz CPU, a 240 MHz
//! AHB/AXI clock and 120 MHz APB clocks; note 4: VOS0 only with the LDO on
//! (this board); note 5 and Table 112: TJ at most 105 °C. ES0392 section
//! 2.2.21: no VOS0 on revisions Y and W; this board is revision V
//! (`idcode=0x20036450`).
//!
//! Reference: the HSI, not the ST-LINK's MCO. PLL1 from the HSE in bypass
//! (8 MHz MCO, UM2407 section 7.5.1) at 8 / 2 x 240 / 2 = 480 MHz measured,
//! against the LSE crystal (2026-11-25), 483.86 MHz (+0.80%, MCO
//! 8.064 MHz), over the CPU and AXI maxima. The MCO has no stated tolerance
//! (UM2407: "fixed at 8 MHz", changeable by an ST-LINK firmware upgrade)
//! and behaves like an RC oscillator, so no margin on it is provable. The
//! HSI has one: DS12117 Table 133 (rev V), 63.7 to 64.3 MHz at 3.3 V and TJ
//! 30 °C (production tested), drift +1% at most for TJ -20 to 105 °C and
//! +0.03% at most over VDD: at most 64.3 x 1.0103 = 64.96 MHz (+1.5%). The
//! board's HSI measured 64.230 MHz (+0.36%).
//!
//! PLL1: 64 MHz / DIVM1 5 = 12.8 MHz reference (see [`PLL_REF_DIV`]),
//! x 73.75 (fractional mode: DIVN1 73, FRACN1 6144 / 8192) = 944 MHz VCO
//! (wide VCO, 192 to 960 MHz, RM0433 section 8.7.12), / DIVP1 2 =
//! 472 MHz; AXI 236 MHz, APB 118 MHz. At the HSI's upper bound
//! (64.96 MHz): reference 12.99 MHz, VCO 958.2 MHz, CPU 479.1 MHz, AXI
//! 239.6 MHz, all under their limits. The next integer step, x 60 from a
//! 16 MHz reference, is the 480 MHz that leaves no margin at all. Flash
//! wait states: AXI in ]225; 240] MHz at VOS0 is 4 (RM0433 Table 17), and
//! 4 stay valid (more than needed) down to the HSI's lower bound (AXI
//! 229.9 MHz).
//!
//! PLL2: the QUADSPI kernel clock (review Q7: the clock plan owns every
//! PLL, so no driver stops or retunes a PLL another consumer may share,
//! e.g. SAI on PLL2_P on Stage 3). 64 MHz / DIVM2 5 = 12.8 MHz, x DIVN2 62
//! = 793.6 MHz VCO, / DIVR2 4 = [`PLL2_R_HZ`] 198.4 MHz, integer mode,
//! only the R output; RCC_D1CCIPR QSPISEL = pll2_r_ck. The QUADSPI driver
//! only checks it ([`qspi_kernel_ready`]); `docs/reference/qspi-nor.md`
//! "Clock".

use core::sync::atomic::{AtomicU32, Ordering};

use crate::pac;

/// The CPU clock, nominal.
pub const CPU_HZ: u32 = 472_000_000;
/// The HSI, the PLL's reference and the clock after reset (RM0433 section
/// 8.5.6: HSIDIV /1).
pub const HSI_HZ: u32 = 64_000_000;
/// The AXI/AHB clock (HPRE /2), nominal.
pub const HCLK_HZ: u32 = CPU_HZ / 2;
/// The banner's name of this clock tree.
pub const CLOCK_NAME: &str = "pll472";

/// PLL1's reference divider (DIVM1).
///
/// The reference must stay inside the PLL1RGE band at every HSI frequency
/// the part allows. DS12117 Table 133 (rev V): 63.7 to 64.3 MHz at 3.3 V
/// and TJ 30 °C, drift over temperature -2% to +1% (TJ -40 to 105 °C),
/// over VDD -0.12% to +0.03%: 62.35 to 64.96 MHz. DIVM1 4, a 16 MHz
/// reference at nominal, is exactly the top of the band (RM0433 section
/// 8.7.12, PLL1RGE 0b11: 8 to 16 MHz; DS12117 Table 136: fPLL_IN 2 to
/// 16 MHz), and over it on this board (HSI measured 64.228 MHz:
/// 16.06 MHz). DIVM1 5 gives 12.47 to 12.99 MHz over the whole tolerance
/// (12.85 MHz on this board), well inside 8 to 16 MHz, with the VCO
/// selection unchanged (RM0433 section 8.5.5: above 2 MHz, the wide VCO).
///
/// x 73.75 is not an integer, so PLL1 runs in fractional mode: VCO = ref x
/// (DIVN1 + FRACN1 / 2^13) (RM0433 section 8.7.14), the DIVN1 field
/// holding N - 1 (section 8.7.13). DS12117 Table 136: sigma-delta mode
/// locks within 166 us (CKIN >= 8 MHz), and its long-term jitter is
/// +-0.8% against +-0.7% in integer mode: a spread of the period, not a
/// shift of the mean frequency, which is what the margins are about.
const PLL_REF_DIV: u8 = 5;
/// x 73.75: DIVN1 field 72 (N 73), FRACN1 0.75 x 8192.
const PLL_N_FRAC: (u16, u16) = (73 - 1, 6144);

/// The HSI over its whole tolerance (DS12117 Rev 10 Table 133 (rev V): 63.7
/// to 64.3 MHz at 3.3 V and TJ 30 C, -2% to +1% over TJ -40 to 105 C,
/// -0.12% to +0.03% over VDD), as [`PLL_REF_DIV`] uses it.
const HSI_MIN_HZ: u32 = 62_350_000;
const HSI_MAX_HZ: u32 = 64_960_000;

/// PLL2's dividers for [`PLL2_R_HZ`] (RM0433 section 8.7.11 PLLCKSELR
/// DIVM2; section 8.7.15 PLL2DIVR, whose fields hold N - 1 and R - 1).
/// DIVM2 5 is PLL1's reference (12.8 MHz, see [`PLL_REF_DIV`]); DIVM2 4
/// (16 MHz nominal) sat exactly on the 16 MHz input maximum, and over it on
/// this board (HSI measured 64.228 MHz, 16.06 MHz).
const PLL2_DIVM: u8 = 5;
const PLL2_DIVN: u16 = 62;
const PLL2_DIVR: u8 = 4;

/// pll2_r_ck for an HSI at `hsi` Hz.
const fn pll2_r_hz(hsi: u32) -> u32 {
    (hsi / PLL2_DIVM as u32) * PLL2_DIVN as u32 / PLL2_DIVR as u32
}

/// pll2_r_ck, nominal: 198.4 MHz, the QUADSPI kernel clock.
pub const PLL2_R_HZ: u32 = pll2_r_hz(HSI_HZ);
/// pll2_r_ck at the fastest HSI the tolerance allows (201.4 MHz).
pub const PLL2_R_MAX_HZ: u32 = pll2_r_hz(HSI_MAX_HZ);

// PLL2's limits at both ends of the HSI tolerance (DS12117 Rev 10 Table 136
// (rev V), wide VCO: fPLL_IN 2 to 16 MHz, RM0433 PLL2RGE range8 8 to
// 16 MHz; VCO 192 to 836 MHz (the rev Y limit, Table 39; rev V allows
// 960); R output <= 400 MHz), and the QUADSPI kernel clock's maximum at
// VOS0, 250 MHz (RM0433 section 8.5.8, Table 60; VOS1 allows only
// 200 MHz, which the fastest HSI exceeds, so QSPISEL = pll2_r_ck needs
// VOS0 first, as [`clock_up`] orders it). Input 12.47 to 12.99 MHz, VCO
// 773 to 806 MHz.
const _: () = {
    let (lo, hi) = (HSI_MIN_HZ / PLL2_DIVM as u32, HSI_MAX_HZ / PLL2_DIVM as u32);
    assert!(lo >= 8_000_000 && hi <= 16_000_000);
    let n = PLL2_DIVN as u32;
    assert!(lo * n >= 192_000_000 && hi * n <= 836_000_000);
    assert!(PLL2_R_MAX_HZ <= 250_000_000);
    assert!(PLL2_R_HZ == 198_400_000);
};

/// Polls `ready` up to `tries` times.
pub(crate) fn wait_for(tries: u32, mut ready: impl FnMut() -> bool) -> bool {
    for _ in 0..tries {
        if ready() {
            return true;
        }
    }
    false
}

/// Programs PLL1 (off after reset) from the HSI as `PLL_REF_DIV` and
/// `PLL_N_FRAC` say, P output /2 only, and turns it on. RM0433 section
/// 8.5.5, "PLL initialization phase" and Figure 48 (fractional mode):
/// source and DIVM1; FRACN1; PLLCFGR with PLL1FRACEN = 1 (its 0 to 1 edge
/// latches FRACN1 into the modulator, section 8.7.12; it is 0 after reset
/// with the PLL off, so the edge is this write); the dividers; PLL1ON.
/// Returns whether PLL1RDY rose.
fn pll1_on(rcc: &pac::RCC, tries: u32) -> bool {
    // Source HSI (64 MHz, HSIDIV /1 at reset), DIVM1 (RM0433 section
    // 8.7.11). The HSE is not used (see the module docs).
    rcc.pllckselr
        .modify(|_, w| w.pllsrc().hsi().divm1().bits(PLL_REF_DIV));
    rcc.pll1fracr.write(|w| w.fracn1().bits(PLL_N_FRAC.1));
    // Reference range 8 to 16 MHz (PLL1RGE = 0b11), wide VCO (192 to
    // 960 MHz), fractional latch on, only the P output (RM0433 section
    // 8.7.12).
    rcc.pllcfgr.modify(|_, w| {
        w.pll1rge()
            .range8()
            .pll1vcosel()
            .wide_vco()
            .pll1fracen()
            .set()
            .divp1en()
            .enabled()
            .divq1en()
            .disabled()
            .divr1en()
            .disabled()
    });
    // DIVN1 field = N - 1, DIVP1 field = P - 1 = 1 (/2). RM0433 section
    // 8.7.13. DIVQ1 and DIVR1 are 0 (reset value 1): RM0433 section 8.5.5,
    // "PLL description": for an unused post-divider "the application shall
    // set the enable bit (DIVyEN) as well as the corresponding post-divider
    // bits (DIVP, DIVQ or DIVR) to '0'. If the above rules are not
    // respected, the PLL output frequency is not guaranteed."
    // SAFETY: documented field values (DIVN1 4 to 512, DIVP1 /2, DIVQ1 and
    // DIVR1 0 for a disabled output).
    rcc.pll1divr.modify(|_, w| unsafe {
        w.divn1()
            .bits(PLL_N_FRAC.0)
            .divp1()
            .bits(1)
            .divq1()
            .bits(0)
            .divr1()
            .bits(0)
    });
    rcc.cr.modify(|_, w| w.pll1on().set_bit());
    wait_for(tries, || rcc.cr.read().pll1rdy().bit_is_set())
}

/// Programs PLL2 for [`PLL2_R_HZ`] (RM0433 section 8.5.5 "PLL
/// initialization phase": DIVM2, the input range and VCO, the dividers,
/// then PLL2ON and a wait for PLL2RDY), integer mode, only the R output,
/// and selects it as the QUADSPI kernel clock (RCC_D1CCIPR QSPISEL = 10,
/// section 8.7.19). The source is the PLLSRC [`pll1_on`] set (HSI; PLLSRC
/// is writable only with every PLL off, section 8.7.11, so PLL1 first).
/// DIVM2, PLLCFGR's PLL2 bits and PLL2DIVR are writable only with PLL2 off
/// (sections 8.7.11 to 8.7.15). PLL2 is off after reset; if it runs
/// anyway (a debugger), the QUADSPI is moved to rcc_hclk3 and PLL2
/// stopped first. QUADSPIEN is off here (reset), so the mux switch has no
/// consumer yet.
fn pll2_on(rcc: &pac::RCC, tries: u32) -> Result<(), &'static str> {
    if rcc.cr.read().pll2on().bit_is_set() {
        rcc.d1ccipr.modify(|_, w| w.qspisel().rcc_hclk3());
        rcc.cr.modify(|_, w| w.pll2on().clear_bit());
        if !wait_for(tries, || rcc.cr.read().pll2rdy().bit_is_clear()) {
            return Err("pll2 stop");
        }
    }
    rcc.pllckselr.modify(|_, w| w.divm2().bits(PLL2_DIVM));
    rcc.pllcfgr.modify(|_, w| {
        w.pll2rge()
            .range8()
            .pll2vcosel()
            .wide_vco()
            .pll2fracen()
            .reset()
            .divp2en()
            .disabled()
            .divq2en()
            .disabled()
            .divr2en()
            .enabled()
    });
    // SAFETY: DIVN2 field 61 (x62, valid 4..512), DIVR2 field 3 (/4);
    // DIVP2 and DIVQ2 0 for disabled outputs (RM0433 section 8.5.5, as
    // `pll1_on` does for PLL1).
    rcc.pll2divr.write(|w| unsafe {
        w.divn2()
            .bits(PLL2_DIVN - 1)
            .divp2()
            .bits(0)
            .divq2()
            .bits(0)
            .divr2()
            .bits(PLL2_DIVR - 1)
    });
    rcc.cr.modify(|_, w| w.pll2on().set_bit());
    if !wait_for(tries, || rcc.cr.read().pll2rdy().bit_is_set()) {
        return Err("pll2 lock");
    }
    rcc.d1ccipr.modify(|_, w| w.qspisel().pll2_r());
    Ok(())
}

/// Whether the QUADSPI kernel clock is what [`pll2_on`] set up: PLL2
/// locked with DIVM2, DIVN2 and DIVR2 as planned and its R output on, and
/// QSPISEL = pll2_r_ck. So the QUADSPI's [`PLL2_R_HZ`] holds. Read-only.
pub(crate) fn qspi_kernel_ready(rcc: &pac::rcc::RegisterBlock) -> bool {
    let divr = rcc.pll2divr.read();
    rcc.cr.read().pll2rdy().bit_is_set()
        && rcc.pllckselr.read().divm2().bits() == PLL2_DIVM
        && rcc.pllcfgr.read().divr2en().is_enabled()
        && divr.divn2().bits() == PLL2_DIVN - 1
        && divr.divr2().bits() == PLL2_DIVR - 1
        && rcc.d1ccipr.read().qspisel().is_pll2_r()
}

/// What [`clock_up`] saw of the VOS0 ramp, for the banner: SYSCFG_PWRCR
/// before ODEN was written; the polls until VOSRDY or ACTVOSRDY first read
/// 0 after it (`u32::MAX`: never seen low); the polls that then read
/// not-ready until both were 1 again.
static VOS0_DIAG: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

/// [`VOS0_DIAG`]: (SYSCFG_PWRCR before ODEN, polls to drop, polls low).
pub fn vos0_diag() -> (u32, u32, u32) {
    (
        VOS0_DIAG[0].load(Ordering::Relaxed),
        VOS0_DIAG[1].load(Ordering::Relaxed),
        VOS0_DIAG[2].load(Ordering::Relaxed),
    )
}

/// The clock tree (see the module docs). On a failed wait it returns the
/// step's name, leaving the system clock on the HSI (or, if only the final
/// read-backs failed, wherever the RCC reports it: `pclk1_hz` follows
/// RCC_CFGR.SWS), and `init` panics with it once the UART is set up.
///
/// Order: supply valid; VOS1 then VOS0 (voltage before frequency, RM0433
/// section 6.8.6 VOS); PLL1 (it sets PLLSRC, writable only with every PLL
/// off); PLL2 and the QUADSPI kernel mux (after VOS0: the kernel clock's
/// maximum, see [`PLL2_R_MAX_HZ`]); flash wait states up (before the
/// faster clock, RM0433 section 4.3.8 "Adjusting system frequency");
/// prescalers; the switch.
pub(crate) fn clock_up(
    pwr: &pac::PWR,
    rcc: &pac::RCC,
    flash: &pac::FLASH,
    syscfg: &pac::SYSCFG,
) -> Result<(), &'static str> {
    const TRIES: u32 = 4_000_000;
    // RM0433 section 6.4.1: no VOS change before ACTVOSRDY.
    if !pwr.csr1.read().actvosrdy().bit_is_set() {
        return Err("actvosrdy");
    }
    // RM0433 section 6.6.2 "VOS0 activation/deactivation sequence":
    // 1. VOS1 in PWR_D3CR (VOS = 0b11, section 6.8.6), here also waited
    //    for; 2. SYSCFGEN in RCC_APB4ENR (section 8.7.48); 3. ODEN in
    //    SYSCFG_PWRCR (section 12.3.10: "must be written only in VOS1");
    //    4. wait for VOSRDY. Only then may the frequency go up. PWR_D3CR's
    //    VOS field keeps 0b11 at VOS0 (it has no VOS0 code) and so does
    //    PWR_CSR1.ACTVOS: ODEN = 1 with VOSRDY = 1 is what says VOS0.
    // SAFETY: 0b11 is a documented value of the 2-bit VOS field.
    pwr.d3cr.modify(|_, w| unsafe { w.vos().bits(0b11) });
    if !wait_for(TRIES, || pwr.d3cr.read().vosrdy().bit_is_set()) {
        return Err("vos1 vosrdy");
    }
    crate::rcc::modify(|rcc| {
        rcc.apb4enr.modify(|_, w| w.syscfgen().set_bit());
        let _ = rcc.apb4enr.read(); // the clock is on before the first access
    });
    VOS0_DIAG[0].store(syscfg.pwrcr.read().bits(), Ordering::Relaxed);
    syscfg.pwrcr.modify(|_, w| w.oden().set_bit());
    if !syscfg.pwrcr.read().oden().bit_is_set() {
        return Err("oden write");
    }
    // RM0433 Figure 31: VOSRDY and ACTVOSRDY drop a little after ODEN
    // rises and come back once VCORE has ramped from 1.26 to 1.35 V, about
    // 20 us later. So the ready flags are still VOS1's for a moment after
    // the write: a plain "wait for VOSRDY" (step 4, and ST's HAL) can pass
    // at once, before the ramp (seen on the board, 2026-11-25: 0 polls
    // low). First wait, bounded and not fatal, for a flag to drop; then for
    // both to be 1 again.
    let ready =
        || pwr.d3cr.read().vosrdy().bit_is_set() && pwr.csr1.read().actvosrdy().bit_is_set();
    let mut to_drop = 0u32;
    let dropped = wait_for(100_000, || {
        to_drop += 1;
        !ready()
    });
    VOS0_DIAG[1].store(if dropped { to_drop } else { u32::MAX }, Ordering::Relaxed);
    let mut low = 0u32;
    let vos0 = wait_for(TRIES, || {
        let r = ready();
        low += u32::from(!r);
        r
    });
    VOS0_DIAG[2].store(low, Ordering::Relaxed);
    if !vos0 {
        return Err("vos0 vosrdy");
    }
    // And in any case at least 100 us since ODEN (64 MHz HSI: 6,400
    // cycles; `asm::delay` takes at least that many), five times the ramp
    // of Figure 31, so the frequency never goes up on the flags alone.
    cortex_m::asm::delay(6_400);
    // PLL1: 12.8 MHz reference x 73.75 = VCO 944 MHz, /2 = 472 MHz (see
    // `PLL_REF_DIV`).
    if !pll1_on(rcc, TRIES) {
        return Err("pll1 lock");
    }
    // PLL2: 12.8 MHz x 62 = VCO 793.6 MHz, /4 = 198.4 MHz, the QUADSPI
    // kernel clock (see `PLL2_DIVM`).
    pll2_on(rcc, TRIES)?;
    // RM0433 Table 17: AXI clock in ]225; 240] MHz at VOS0 (236 MHz) is 4
    // wait states with WRHIGHFREQ = 0b10 (FLASH_ACR 0x24). Section 4.3.8: set
    // and read back before the clock goes up; more wait states than needed
    // are harmless on the 64 MHz HSI meanwhile.
    // SAFETY: documented values of the LATENCY and WRHIGHFREQ fields.
    flash
        .acr
        .write(|w| unsafe { w.latency().bits(4).wrhighfreq().bits(0b10) });
    if !wait_for(TRIES, || {
        let acr = flash.acr.read();
        acr.latency().bits() == 4 && acr.wrhighfreq().bits() == 0b10
    }) {
        return Err("flash latency");
    }
    // CPU 472 MHz (D1CPRE /1); AXI/AHB 236 MHz (HPRE /2); every APB
    // 118 MHz (D1PPRE, D2PPRE1/2, D3PPRE /2). DS12117 Table 111, VOS0.
    // RM0433 sections 8.7.8 to 8.7.10.
    rcc.d1cfgr
        .modify(|_, w| w.d1cpre().div1().hpre().div2().d1ppre().div2());
    rcc.d2cfgr
        .modify(|_, w| w.d2ppre1().div2().d2ppre2().div2());
    rcc.d3cfgr.modify(|_, w| w.d3ppre().div2());
    // RM0433 section 8.7.7: SW = PLL1, then SWS reports it.
    rcc.cfgr.modify(|_, w| w.sw().pll1());
    if !wait_for(TRIES, || rcc.cfgr.read().sws().is_pll1()) {
        return Err("switch to pll1");
    }
    // The I/O compensation cell (M7 review), before any pin is driven
    // fast: QUADSPI runs at 99.2 MHz and FMC/GPIO at OSPEEDR very high,
    // and DS12117 Rev 10 Table 170 (QUADSPI timing) and Table 149 note 5
    // (I/O AC characteristics) hold only with the cell activated. RM0433
    // section 11.3.11: 1. the CSI on (RCC_CR.CSION, section 8.7.2; CSIRDY
    // waited for: SYSCFG_CCCSR.READY never sets without the CSI, section
    // 12.3.7); 2. the SYSCFG clock (on since the VOS0 ramp above); 3.
    // CCCSR.EN; then READY. CS stays 0 (the cell's own code, not
    // SYSCFG_CCCR's) and HSLV stays 0: it is for VDD below 2.7 V only and
    // "might be destructive" above (section 12.3.7); VDD is 3.3 V here.
    // The CSI then runs for good (nothing in this crate clears CSION; it
    // stops only in Stop/Standby, which no image uses): DS12117 Table 134,
    // 23 uA typical, 30 uA max.
    rcc.cr.modify(|_, w| w.csion().set_bit());
    if !wait_for(TRIES, || rcc.cr.read().csirdy().bit_is_set()) {
        return Err("csi ready");
    }
    syscfg.cccsr.modify(|_, w| w.en().set_bit());
    if !wait_for(TRIES, || syscfg.cccsr.read().ready().bit_is_set()) {
        return Err("io compensation ready");
    }
    Ok(())
}

/// rcc_pclk1 as the clock registers stand (only the values this lib sets:
/// HSI or PLL1 at [`CPU_HZ`], HPRE and D2PPRE1 /1 or /2), so the fault path
/// gets the baud rate right wherever the clock setup stopped. Nothing uses
/// the HSE, so SWS is HSI or PLL1.
pub(crate) fn pclk1_hz(rcc: &pac::rcc::RegisterBlock) -> u32 {
    let sys = if rcc.cfgr.read().sws().is_pll1() {
        CPU_HZ
    } else {
        HSI_HZ
    };
    let hpre = u32::from(rcc.d1cfgr.read().hpre().bits() & 0b1000 != 0);
    let ppre = u32::from(rcc.d2cfgr.read().d2ppre1().bits() & 0b100 != 0);
    sys >> hpre >> ppre
}
