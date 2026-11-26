//! The `lcd-probe` image (plan Stage 2 M3): first contact with the
//! shield's panel controller over the FMC. For each of the driver's timings
//! ([`lcd::FAST`], [`lcd::SPEC`], [`lcd::SAFE`]) it sets the FMC up, resets
//! and initialises the panel, then reports over the UART:
//!
//! - `fmc`: the timing's cycle counts and the registers as programmed;
//! - `id`: RDDID (0x04, 3 bytes) and RDID1-3 (0xDA-0xDC), which name the
//!   controller (85 85 52: ST7789V, datasheet V1.3 9.1.46-48); for any
//!   other answer also RDID4 (0xD3; 00 93 41 on an ILI9341V, §8.3.23; not
//!   an ST7789V command, so not sent to one); the status reads RDDPM (0x0A), RDDMADCTL
//!   (0x0B) and RDDCOLMOD (0x0C), which show the init's writes arrived;
//! - `te`: TE (FMARK) rising edges counted over 1 s (the refresh rate);
//! - `fill`: a full-screen fill by CPU stores, timed with the cycle
//!   counter (u32 stores and u8 stores), as bytes per second;
//! - `readback`: after a full-screen pattern (a pseudo-random one, then a
//!   gradient), the whole GRAM read back with RAMRD and compared with
//!   [`lcd::expected_readback`]: exact mismatches (D7..D2), mismatches of
//!   the 5/6 high bits only (to tell a 16 -> 18 bit rule difference from
//!   a bus error), and mismatches under the other candidate rule (MSB
//!   repeated); the first pixels' raw bytes.
//!
//! Then it leaves the gradient bars on the panel (see the reference note)
//! and repeats. Every line starts with `gbc-nucleo-h753`; the banner once
//! per round.
#![no_std]
#![no_main]

use core::fmt::Write;

use gbc_nucleo_h753::lcd::{self, HEIGHT, Lcd, READBACK_MASK, Timing, WIDTH, expected_readback};
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "lcd-probe";
const PIXELS: u32 = WIDTH as u32 * HEIGHT as u32;

/// The patterns, by name and pixel function.
type Pattern = fn(u16, u16) -> u16;

/// A pseudo-random pattern: every bit of both bytes changes, so a stuck
/// or swapped data line, or a dropped or doubled byte, shows.
fn noise(x: u16, y: u16) -> u16 {
    let mut v = u32::from(y) * u32::from(WIDTH) + u32::from(x);
    v = v.wrapping_mul(0x9E37_79B9);
    v ^= v >> 15;
    v = v.wrapping_mul(0x85EB_CA6B);
    (v >> 16) as u16
}

/// Eight vertical bars (top field 0xF800, middle 0x07E0, low 0x001F,
/// their pairs, white, black-to-grey), each fading from full at the top to
/// dark at the bottom; a white 16 x 16 square at memory (0, 0) marks the
/// first pixel of the scan.
fn bars(x: u16, y: u16) -> u16 {
    if x < 16 && y < 16 {
        return 0xFFFF;
    }
    let level = 31 - (u32::from(y) * 32 / u32::from(HEIGHT)) as u16; // 31..0
    let (r, g, b) = match x / 30 {
        0 => (1, 0, 0),
        1 => (0, 1, 0),
        2 => (0, 0, 1),
        3 => (1, 1, 0),
        4 => (0, 1, 1),
        5 => (1, 0, 1),
        6 => (1, 1, 1),
        _ => (1, 1, 1),
    };
    let lv = if x / 30 == 7 { 31 - level } else { level };
    (r * lv) << 11 | (g * lv * 2) << 5 | (b * lv)
}

fn draw(l: &mut Lcd, p: Pattern) {
    l.set_window(0, 0, WIDTH - 1, HEIGHT - 1);
    l.begin_pixels();
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            l.pixel(p(x, y));
        }
    }
}

fn hex(out: &mut Uart, bytes: &[u8]) {
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
}

/// The other candidate 16 -> 18 bit rule: a 5-bit field's MSB repeated
/// into the new LSB.
fn expected_msb(rgb565: u16) -> [u8; 3] {
    let r = (rgb565 >> 11) as u8 & 0x1F;
    let g = (rgb565 >> 5) as u8 & 0x3F;
    let b = rgb565 as u8 & 0x1F;
    [(r << 1 | r >> 4) << 2, g << 2, (b << 1 | b >> 4) << 2]
}

/// Reads the whole GRAM back and compares it with `p`.
fn readback(out: &mut Uart, l: &mut Lcd, name: &str, p: Pattern) {
    l.set_window(0, 0, WIDTH - 1, HEIGHT - 1);
    let t0 = cycles();
    l.begin_readback();
    let (mut exact, mut high, mut msb) = (0u32, 0u32, 0u32);
    let mut first = [0u8; 12];
    let mut first_bad: Option<(u16, u16, [u8; 3], [u8; 3])> = None;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let got = [l.read_data(), l.read_data(), l.read_data()];
            let i = usize::from(y) * usize::from(WIDTH) + usize::from(x);
            if i < 4 {
                first[i * 3..i * 3 + 3].copy_from_slice(&got);
            }
            let want = expected_readback(p(x, y));
            let m = |a: [u8; 3], b: [u8; 3], k: [u8; 3]| (0..3).any(|j| a[j] & k[j] != b[j] & k[j]);
            let k = [READBACK_MASK; 3];
            if m(got, want, k) {
                exact += 1;
                if first_bad.is_none() {
                    first_bad = Some((x, y, got, want));
                }
            }
            if m(got, want, [0xF8, 0xFC, 0xF8]) {
                high += 1;
            }
            if m(got, expected_msb(p(x, y)), k) {
                msb += 1;
            }
        }
    }
    l.end_read();
    let dt = cycles().wrapping_sub(t0);
    let _ = write!(
        out,
        "{PREFIX} lcd readback pattern={name} pixels={PIXELS} mismatches={exact} \
         high_bit_mismatches={high} msb_rule_mismatches={msb} cycles={dt} first="
    );
    hex(out, &first);
    if let Some((x, y, got, want)) = first_bad {
        let _ = write!(out, " first_bad=({x},{y}) got=");
        hex(out, &got);
        let _ = write!(out, " want=");
        hex(out, &want);
    }
    let _ = writeln!(out, "\r");
}

/// A full-screen solid fill, timed: `wide` uses u32 stores, else u8.
fn timed_fill(out: &mut Uart, l: &mut Lcd, colour: u16, wide: bool) {
    l.set_window(0, 0, WIDTH - 1, HEIGHT - 1);
    l.begin_pixels();
    let t0 = cycles();
    if wide {
        l.fill(colour, PIXELS);
    } else {
        for _ in 0..PIXELS {
            l.pixel(colour);
        }
    }
    // The last store has reached the FMC (its write FIFO holds at most 16
    // words, 64 bytes, out of 153,600: under 0.05%).
    cortex_m::asm::dsb();
    let dt = cycles().wrapping_sub(t0);
    let bytes = u64::from(PIXELS) * 2;
    let bps = bytes * u64::from(CPU_HZ) / u64::from(dt.max(1));
    let _ = writeln!(
        out,
        "{PREFIX} lcd fill colour={colour:#06x} stores={} bytes={bytes} cycles={dt} \
         us={} bytes_per_s={bps}\r",
        if wide { "u32" } else { "u8" },
        u64::from(dt) * 1_000_000 / u64::from(CPU_HZ),
    );
}

fn report(out: &mut Uart, l: &mut Lcd, n: u32) {
    let t = l.timing();
    let (bcr, btr, bwtr, ccipr) = l.registers();
    let _ = writeln!(
        out,
        "{PREFIX} lcd fmc n={n} timing={} kernel={:?} ker_hz={} slack_pct={} \
         w_addset={} w_datast={} w_cycles={} r_addset={} r_addhld={} r_datast={} r_busturn={} \
         bus_bytes_per_s={} bcr1={bcr:#010x} btr1={btr:#010x} bwtr1={bwtr:#010x} \
         d1ccipr={ccipr:#010x}\r",
        t.name,
        t.kernel,
        t.kernel.hz(),
        t.slack_pct,
        t.w_addset,
        t.w_datast,
        t.write_cycles(),
        t.r_addset,
        t.r_addhld,
        t.r_datast,
        t.r_busturn,
        t.write_bytes_per_s(),
    );

    let (mut rddid, mut rdid4) = ([0u8; 3], [0xFFu8; 3]);
    let (mut pm, mut mad, mut col) = ([0u8; 1], [0u8; 1], [0u8; 1]);
    l.read(0x04, &mut rddid);
    l.read(0x0A, &mut pm);
    l.read(0x0B, &mut mad);
    l.read(0x0C, &mut col);
    let mut ids = [0u8; 3];
    for (i, c) in [0xDA, 0xDB, 0xDC].into_iter().enumerate() {
        l.read(c, &mut ids[i..i + 1]);
    }
    let panel = if rddid == [0x85, 0x85, 0x52] {
        "st7789v"
    } else {
        l.read(0xD3, &mut rdid4);
        if rdid4 == [0x00, 0x93, 0x41] {
            "ili9341v"
        } else {
            "unknown"
        }
    };
    let _ = write!(out, "{PREFIX} lcd id panel={panel} rddid=");
    hex(out, &rddid);
    let _ = write!(out, " rdid1_3=");
    hex(out, &ids);
    if panel != "st7789v" {
        let _ = write!(out, " rdid4=");
        hex(out, &rdid4);
    }
    let _ = writeln!(
        out,
        " rddpm={:#04x} rddmadctl={:#04x} rddcolmod={:#04x} madctl_ok={}\r",
        pm[0],
        mad[0],
        col[0],
        u8::from(mad[0] == lcd::MADCTL),
    );

    // TE: rising edges over 1 s.
    let t0 = cycles();
    let (mut edges, mut last) = (0u32, l.te());
    while cycles().wrapping_sub(t0) < CPU_HZ {
        let now = l.te();
        if now && !last {
            edges += 1;
        }
        last = now;
    }
    let _ = writeln!(out, "{PREFIX} lcd te rising_edges_per_s={edges}\r");

    // Solid colours: the three fields alone, white, black; timed.
    for (c, wide) in [
        (0xF800, true),
        (0x07E0, true),
        (0x001F, true),
        (0xFFFF, false),
        (0x0000, true),
    ] {
        timed_fill(out, l, c, wide);
        wait_since(cycles(), CPU_HZ / 4);
    }

    let pats: [(&str, Pattern); 2] = [("noise", noise), ("bars", bars)];
    for (name, p) in pats {
        draw(l, p);
        readback(out, l, name, p);
    }
    let _ = writeln!(out, "{PREFIX} lcd end n={n} timing={}\r", t.name);
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let timings: [Timing; 3] = [lcd::FAST, lcd::SPEC, lcd::SAFE];
    let mut l = Lcd::init(timings[0]);
    let mut n = 0u32;
    loop {
        if n.is_multiple_of(timings.len() as u32) {
            board.banner(IMAGE);
        }
        let t = timings[n as usize % timings.len()];
        if n > 0 {
            l.set_timing(t);
            l.init_panel();
        }
        report(&mut board.uart, &mut l, n);
        // The bars stay up 3 s, LD1 toggling.
        for _ in 0..6 {
            wait_since(cycles(), CPU_HZ / 2);
            led::toggle();
        }
        n += 1;
    }
}
