//! The board's USART baud math (`crates/gbc-nucleo-h753/src/baud.rs`,
//! compiled here on the host: the firmware crate only builds for
//! thumbv7em) at the board's 118 MHz kernel clock: the BRR written, the
//! rate that BRR really gives (RM0433 48.5.7) and whether `set_baud` takes
//! it (`docs/reference/qspi-nor.md` "UART").

#[path = "../../../crates/gbc-nucleo-h753/src/baud.rs"]
mod baud;

use baud::{BaudSetting, MAX_ERROR_PPM, baud_setting, brr_rate, effective_divisor};

/// rcc_pclk1 on the board (`crates/gbc-nucleo-h753/src/clock.rs`).
const KER: u32 = 118_000_000;

/// (bps, over8, brr, actual, error_ppm, accepted by `set_baud`)
const TABLE: [(u32, bool, u32, u32, i32, bool); 9] = [
    (115_200, false, 1024, 115_234, 295, true),
    (921_600, false, 128, 921_875, 298, true),
    (2_000_000, false, 59, 2_000_000, 0, true),
    (3_000_000, false, 39, 3_025_641, 8_547, true),
    // 29.5 clocks per bit: 30 (3,933,333) or 29 (4,068,966), never exact.
    (4_000_000, false, 30, 3_933_333, -16_666, true),
    (6_000_000, false, 20, 5_900_000, -16_666, true),
    // 16x would need USARTDIV 15 (< 16): 8x, USARTDIV 30, BRR 0x17.
    (8_000_000, true, 0x17, 7_866_667, -16_666, true),
    (12_000_000, true, 0x12, 11_800_000, -16_666, true),
    // 8x USARTDIV 12 (< 16): unreachable.
    (20_000_000, true, 0, 0, 0, false),
];

fn accepted(s: Option<BaudSetting>) -> bool {
    s.is_some_and(|s| s.error_ppm.unsigned_abs() <= MAX_ERROR_PPM)
}

#[test]
fn table_at_118_mhz() {
    for (bps, over8, brr, actual, error_ppm, ok) in TABLE {
        let s = baud_setting(KER, bps);
        assert_eq!(accepted(s), ok, "{bps}: accepted");
        if bps == 20_000_000 {
            assert_eq!(s, None, "{bps}");
            continue;
        }
        let s = s.unwrap_or_else(|| panic!("{bps}: no setting"));
        assert_eq!(
            (s.over8, s.brr, s.actual, s.error_ppm),
            (over8, brr, actual, error_ppm),
            "{bps}: (over8, brr, actual, error_ppm)"
        );
    }
}

/// Every setting's `actual` is the rate its BRR gives, decoded the way the
/// hardware reads BRR: at 8x the divisor is always even, so the
/// `(div & 0xF) >> 1` encoding never loses a bit.
#[test]
fn actual_is_what_the_register_gives() {
    for bps in (100_000..14_000_000).step_by(7_919) {
        let Some(s) = baud_setting(KER, bps) else {
            continue;
        };
        assert_eq!(s.actual, brr_rate(KER, s.brr, s.over8), "{bps}");
        if s.over8 {
            assert_eq!(s.brr & 0x8, 0, "{bps}: BRR[3] must be 0 at 8x");
            assert_eq!(effective_divisor(s.brr, true) % 2, 0, "{bps}");
        }
    }
}

/// The odd-USARTDIV cases the old code got wrong: BRR 0x35 (from USARTDIV
/// 59) is really USARTDIV 58, 4,068,966 bit/s (+1.72%).
#[test]
fn brr_decode() {
    assert_eq!(effective_divisor(0x35, true), 58);
    assert_eq!(brr_rate(KER, 0x35, true), 4_068_966);
    assert_eq!(effective_divisor(0x17, true), 30);
    assert_eq!(effective_divisor(30, false), 30);
    assert_eq!(brr_rate(KER, 30, false), 3_933_333);
}
