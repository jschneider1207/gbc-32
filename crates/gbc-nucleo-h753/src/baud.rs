//! USART baud-rate math (RM0433 section 48.5.7), pure: no registers, no
//! `crate::` paths, so `tools/gbc-pc/tests/uart_baud.rs` compiles this file
//! on the host (`#[path]`) and checks the BRR values. `uart` re-exports it.
//!
//! RM0433 48.5.7: with OVER8 = 0, baud = fck / USARTDIV and BRR =
//! USARTDIV. With OVER8 = 1, baud = 2 * fck / USARTDIV, but BRR[15:4] =
//! USARTDIV[15:4], BRR[2:0] = USARTDIV[3:0] >> 1 and BRR[3] = 0: bit 0 of
//! USARTDIV has no place in BRR, so only even USARTDIV values exist. An
//! even USARTDIV = 2k gives baud = fck / k, the same rate as 16x with
//! USARTDIV = k: a bit is always a whole number of kernel clocks, and 8x
//! only adds reach (USARTDIV >= 16 means k >= 8 instead of 16), never
//! resolution. So the rate a setting gives is computed from the divisor
//! decoded back out of BRR ([`effective_divisor`]), not from the one asked
//! for.

/// The largest baud-rate error `Uart::set_baud` accepts, in parts per
/// million: 2%. The receiver's total tolerance for 8N1 (M = 00, ONEBIT =
/// 0; RM0433 48.5.8, Tables 406 and 407) is 3.75% (16x, BRR[3:0] = 0) or
/// 3.33% (16x, otherwise), and 2.50% or 2% with 8x; that budget covers
/// both ends' errors (`docs/reference/qspi-nor.md` "UART").
pub const MAX_ERROR_PPM: u32 = 20_000;

/// [`baud_setting`] keeps 16x oversampling while its error is at most
/// this (1%), even if 8x would be closer. (Since 8x has the same rate
/// steps, it is never closer where 16x is reachable; the rule only picks 8x
/// when 16x cannot reach the rate at all.)
const PREFER_16X_PPM: u32 = 10_000;

/// A BRR value for a rate (RM0433 section 48.5.7).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BaudSetting {
    /// The requested rate.
    pub bps: u32,
    /// USART_BRR as written (with OVER8 = 1: USARTDIV[3:0] >> 1 in
    /// BRR[2:0], BRR[3] = 0).
    pub brr: u32,
    /// Oversampling by 8 (CR1.OVER8) instead of 16.
    pub over8: bool,
    /// The rate the written BRR gives, rounded.
    pub actual: u32,
    /// (actual - bps) / bps, in parts per million.
    pub error_ppm: i32,
}

/// The USARTDIV the hardware uses for a BRR value (48.5.7): BRR itself at
/// 16x; at 8x, BRR[15:4] with BRR[2:0] shifted back up one bit (so always
/// even).
pub const fn effective_divisor(brr: u32, over8: bool) -> u32 {
    if over8 {
        (brr & !0xF) | ((brr & 0x7) << 1)
    } else {
        brr
    }
}

/// The rate (rounded) that `brr` gives at kernel clock `ker_hz`: fck /
/// USARTDIV at 16x, 2 * fck / USARTDIV at 8x, with USARTDIV decoded from
/// BRR. 0 for a zero divisor.
pub const fn brr_rate(ker_hz: u32, brr: u32, over8: bool) -> u32 {
    let div = effective_divisor(brr, over8) as u64;
    if div == 0 {
        return 0;
    }
    let num = ker_hz as u64 * if over8 { 2 } else { 1 };
    ((num + div / 2) / div) as u32
}

/// The best setting for `bps` at kernel clock `ker_hz`, or `None` if no
/// USARTDIV of at least 16 exists (48.5.7: "USARTDIV must be greater than
/// or equal to 16"). Oversampling by 16 (more tolerance and better noise
/// immunity, 48.5.6 and 48.5.8) unless its error is over
/// [`PREFER_16X_PPM`] and 8x does better.
pub fn baud_setting(ker_hz: u32, bps: u32) -> Option<BaudSetting> {
    if bps == 0 {
        return None;
    }
    let try_os = |over8: bool| -> Option<BaudSetting> {
        // Kernel clocks per bit, rounded: USARTDIV at 16x; at 8x USARTDIV
        // is twice this (the nearest even value, the only kind BRR holds).
        let k = (u64::from(ker_hz) + u64::from(bps) / 2) / u64::from(bps);
        let div = if over8 { 2 * k } else { k };
        if !(16..=0xFFFF).contains(&div) {
            return None;
        }
        let div = div as u32;
        let brr = if over8 {
            (div & !0xF) | ((div & 0xF) >> 1)
        } else {
            div
        };
        let actual = brr_rate(ker_hz, brr, over8);
        let error_ppm = ((i64::from(actual) - i64::from(bps)) * 1_000_000 / i64::from(bps)) as i32;
        Some(BaudSetting {
            bps,
            brr,
            over8,
            actual,
            error_ppm,
        })
    };
    match (try_os(false), try_os(true)) {
        (Some(a), Some(b))
            if a.error_ppm.unsigned_abs() > PREFER_16X_PPM
                && b.error_ppm.unsigned_abs() < a.error_ppm.unsigned_abs() =>
        {
            Some(b)
        }
        (Some(a), _) => Some(a),
        (None, b) => b,
    }
}
