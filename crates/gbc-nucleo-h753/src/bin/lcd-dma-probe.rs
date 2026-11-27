//! The `lcd-dma-probe` image (plan Stage 2 M3): the frame path to the
//! panel, [`lcd_dma`], checked and measured on the board. FMC timing
//! [`lcd::SPEC`]. Reports over the UART, every line starting with
//! `gbc-nucleo-h753`:
//!
//! - `dma config`: the MDMA values and the window;
//! - `te default` / `te tuned`: the TE period (mean, min, max over at
//!   least 60 edges, CPU cycles and µs) and TE's high time (V-blank) with
//!   the panel's reset frame rate, then with the FRCTRL2/PORCTRL setting
//!   closest to the Game Boy's 59.7275 Hz ([`lcd::FrameRate::closest`],
//!   from the oscillator frequency the first measurement implies), and the
//!   write-ahead arithmetic for it;
//! - `frame n=...` (60 of them): a 160x144 RGB555 frame with a pattern
//!   that changes every frame, scaled into the AXI buffer
//!   (`scale_frame`, timed), sent by MDMA on TE, then the 240x216 window
//!   read back (RAMRD) and compared with the buffer: mismatching pixels
//!   (D7..D2 of each byte, [`lcd::expected_readback`]);
//! - `control`: one buffer word changed after the last transfer and the
//!   window read back without a new transfer: the comparison must report
//!   exactly the 2 changed pixels (shows it can fail);
//! - `steady`: 120 frames back to back without readback (convert, present,
//!   wait): the transfers per TE edge, the main loop's wait for the
//!   previous transfer, and the transfer figures;
//! - `summary`, then the test image (8 bars in `lcd-probe`'s order and a
//!   white Γ marker at the frame's top-left), sent the same way; the
//!   summary and everything before it are replayed every 5 s.
#![no_std]
#![no_main]

use core::fmt::Write;

use gbc_firmware::fmt_buf::append_truncated;
use gbc_firmware::video::{OUT_H, OutFrame, scale_frame};
use gbc_nucleo_h753::lcd::{self, FrameRate, Lcd};
use gbc_nucleo_h753::lcd_dma::{self, LcdDma, Transfer, Y0, Y1};
use gbc_nucleo_h753::pac::interrupt;
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led, wait_since};

/// Bytes of output kept for the replay.
const LOG_BYTES: usize = 24 << 10;

/// The UART, with every byte also appended to the log while it has room.
struct Out<'a> {
    uart: &'a mut Uart,
    log: &'static mut [u8; LOG_BYTES],
    len: usize,
}

impl Write for Out<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.uart.write_str(s);
        let _ = append_truncated(&mut self.log[..], &mut self.len, s);
        Ok(())
    }
}

impl Out<'_> {
    /// The kept output, again.
    fn replay(&mut self) {
        let _ = writeln!(
            self.uart,
            "{PREFIX} lcd dma replay begin bytes={}\r",
            self.len
        );
        for &b in &self.log[..self.len] {
            self.uart.write_byte(b);
        }
        let _ = writeln!(self.uart, "{PREFIX} lcd dma replay end\r");
    }
}

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "lcd-dma-probe";
const SW: usize = 160;
const SH: usize = 144;
type Frame = [u16; SW * SH];
/// Frames with readback.
const FRAMES: u32 = 60;
/// Frames back to back.
const STEADY: u32 = 120;
/// The Game Boy's frame: 70,224 clocks of 4,194,304 Hz = 16,742,706 ns
/// (59.7275 Hz).
const TARGET_NS: u32 = 16_742_706;
/// A wait longer than any frame: 50 ms.
const TIMEOUT: u32 = CPU_HZ / 20;
/// Two TE periods at 60 Hz (1/30 s), in CPU cycles (a margin, not a measurement).
const TE_PERIOD_CYCLES: u32 = CPU_HZ / 30;

#[interrupt]
fn EXTI15_10() {
    lcd_dma::on_te();
}

/// The core's frame, in DTCM (`.bss`), as the emulator's will be.
fn frame() -> &'static mut Frame {
    gbc_nucleo_h753::take_once!(Frame = [0; SW * SH]).expect("frame taken twice")
}

fn us(c: u32) -> u32 {
    (u64::from(c) * 1_000_000 / u64::from(CPU_HZ)) as u32
}

fn ns(c: u32) -> u32 {
    (u64::from(c) * 1_000_000_000 / u64::from(CPU_HZ)) as u32
}

/// Frame `n`'s pattern: every bit of every pixel changes from frame to
/// frame (a stale or partial transfer shows), and neighbours differ.
fn noise(n: u32, i: usize) -> u16 {
    let mut v = (i as u32).wrapping_mul(0x9E37_79B9) ^ n.wrapping_mul(0x85EB_CA6B);
    v ^= v >> 15;
    v = v.wrapping_mul(0xC2B2_AE35);
    v ^= v >> 13;
    (v as u16) & 0x7FFF
}

/// The test image in the core's RGB555 (`r | g << 5 | b << 10`): 8 bars
/// of 20 px in `lcd-probe`'s order of panel fields (top field 15-11 =
/// core blue, 10-5 green, 4-0 core red, their pairs, white, a grey ramp
/// the other way), fading from full at the top; a white Γ (a 12 x 12
/// square at the frame's top-left with an arm along the top edge to x =
/// 48, 4 rows) tells rotation from mirroring.
fn bars(x: usize, y: usize) -> u16 {
    if (x < 12 && y < 12) || (x < 48 && y < 4) {
        return 0x7FFF;
    }
    let level = 31 - (y * 32 / SH) as u16;
    let (b, g, r) = match x / 20 {
        0 => (1, 0, 0),
        1 => (0, 1, 0),
        2 => (0, 0, 1),
        3 => (1, 1, 0),
        4 => (0, 1, 1),
        5 => (1, 0, 1),
        _ => (1, 1, 1),
    };
    let lv = if x / 20 == 7 { 31 - level } else { level };
    (r * lv) | (g * lv) << 5 | (b * lv) << 10
}

/// A mismatching pixel: (x, y, got, want).
type Bad = (u16, u16, [u8; 3], [u8; 3]);

/// Reads the window back and compares it with `buf` ([`lcd_dma::readback`]):
/// (mismatching pixels, the first one).
fn readback(l: &mut Lcd, buf: &OutFrame) -> (u32, Option<Bad>) {
    let r = lcd_dma::readback(l, buf);
    (r.mismatches, r.first)
}

fn hex3(out: &mut Out, b: [u8; 3]) {
    let _ = write!(out, "{:02x}{:02x}{:02x}", b[0], b[1], b[2]);
}

/// Waits for `edges` TE edges (or 3 s) after a reset and returns the
/// statistics; also TE's mean high time over 8 pulses (polled).
fn measure_te(dma: &mut LcdDma, edges: u32) -> (lcd_dma::TeStats, u32) {
    let l = dma.lcd();
    // TE high time: rising to falling edge, polled (the TE interrupt adds
    // well under 1 µs).
    let mut high = 0u32;
    for _ in 0..8 {
        let t0 = cycles();
        while l.te() && cycles().wrapping_sub(t0) < TIMEOUT {}
        while !l.te() && cycles().wrapping_sub(t0) < TIMEOUT {}
        let r = cycles();
        while l.te() && cycles().wrapping_sub(t0) < TIMEOUT {}
        high += cycles().wrapping_sub(r);
    }
    lcd_dma::te_reset();
    let t0 = cycles();
    while lcd_dma::te_stats().edges < edges && cycles().wrapping_sub(t0) < 3 * CPU_HZ {}
    (lcd_dma::te_stats(), high / 8)
}

fn report_te(out: &mut Out, name: &str, f: FrameRate, s: lcd_dma::TeStats, high: u32) {
    let _ = writeln!(
        out,
        "{PREFIX} lcd te {name} rtna={:#04x} fpa={:#04x} bpa={:#04x} lines={} line_clocks={} \
         edges={} mean_cycles={} mean_ns={} hz_milli={} min_cycles={} max_cycles={} \
         high_cycles={high} high_us={}\r",
        f.rtna,
        f.fpa,
        f.bpa,
        f.lines(),
        f.line_clocks(),
        s.edges,
        s.mean,
        ns(s.mean),
        u64::from(CPU_HZ) * 1000 / u64::from(s.mean.max(1)),
        s.min,
        s.max,
        us(high),
    );
}

/// The panel oscillator implied by a measured period with setting `f`.
fn osc_hz(f: FrameRate, period_cycles: u32) -> u32 {
    (u64::from(f.frame_clocks()) * u64::from(CPU_HZ) / u64::from(period_cycles.max(1))) as u32
}

/// Min / max / sum of a series of cycle counts.
#[derive(Clone, Copy)]
struct Range {
    min: u32,
    max: u32,
    sum: u64,
    n: u32,
}

impl Range {
    const fn new() -> Range {
        Range {
            min: u32::MAX,
            max: 0,
            sum: 0,
            n: 0,
        }
    }
    fn add(&mut self, v: u32) {
        self.min = self.min.min(v);
        self.max = self.max.max(v);
        self.sum += u64::from(v);
        self.n += 1;
    }
    fn mean(&self) -> u32 {
        (self.sum / u64::from(self.n.max(1))) as u32
    }
}

fn range(out: &mut Out, name: &str, r: Range) {
    let _ = write!(
        out,
        " {name}_min={} {name}_max={} {name}_mean={} {name}_max_us={}",
        r.min,
        r.max,
        r.mean(),
        us(r.max)
    );
}

struct Summary {
    te_default: lcd_dma::TeStats,
    te_tuned: lcd_dma::TeStats,
    rate: FrameRate,
    frames: u32,
    mismatches: u32,
    errors: u32,
    control: u32,
    aborts: u32,
    scale: Range,
    xfer: Range,
    delay: Range,
}

fn summary(out: &mut Out, s: &Summary) {
    let _ = write!(
        out,
        "{PREFIX} lcd dma summary frames={} mismatches={} transfer_errors={} aborts={} control_mismatches={} \
         te_default_ns={} te_tuned_ns={} target_ns={TARGET_NS} rtna={:#04x} fpa={:#04x} bpa={:#04x}",
        s.frames,
        s.mismatches,
        s.errors,
        s.aborts,
        s.control,
        ns(s.te_default.mean),
        ns(s.te_tuned.mean),
        s.rate.rtna,
        s.rate.fpa,
        s.rate.bpa,
    );
    range(out, "scale", s.scale);
    range(out, "mdma", s.xfer);
    range(out, "te_to_start", s.delay);
    let _ = writeln!(out, "\r");
}

/// Sends the buffer on the next TE and waits; counts the figures.
fn send(out: &mut Out, dma: &mut LcdDma, s: &mut Summary) -> Option<Transfer> {
    dma.present();
    match dma.wait(TIMEOUT) {
        Ok(Some(t)) => {
            s.xfer.add(t.start_to_done);
            s.delay.add(t.te_to_start);
            Some(t)
        }
        Ok(None) => None,
        Err(e) => {
            s.errors += 1;
            let r = dma.registers();
            let _ = writeln!(
                out,
                "{PREFIX} lcd dma error {e:?} tcr={:#010x} bndtr={:#010x} sar={:#010x} \
                 dar={:#010x} cr={:#010x} isr={:#010x} esr={:#010x}\r",
                r[0], r[1], r[2], r[3], r[4], r[5], r[6]
            );
            None
        }
    }
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    board.banner(IMAGE);
    // The run's output, kept (in DTCM `.bss`) and replayed every 5 s with
    // the summary: the run is over before a serial reader started after
    // the flash can catch it.
    let log =
        gbc_nucleo_h753::take_once!([u8; LOG_BYTES] = [0; LOG_BYTES]).expect("main runs once");
    let mut out = Out {
        uart: &mut board.uart,
        log,
        len: 0,
    };
    let out = &mut out;
    let frame = frame();
    let mut l = Lcd::init(lcd::SPEC);
    // The whole panel black: the rows outside the window stay so.
    l.set_window(0, 0, lcd::WIDTH - 1, lcd::HEIGHT - 1);
    l.begin_pixels();
    l.fill(0, u32::from(lcd::WIDTH) * u32::from(lcd::HEIGHT));
    let mut dma = LcdDma::new(l);
    let t = dma.lcd().timing();
    let _ = writeln!(
        out,
        "{PREFIX} lcd dma config timing={} w_cycles={} bus_bytes_per_s={} tcr={:#010x} \
         bndtr={:#010x} window=0..{},{}..{} bytes={} row_ns={}\r",
        t.name,
        t.write_cycles(),
        t.write_bytes_per_s(),
        lcd_dma::TCR,
        lcd_dma::BNDTR,
        lcd::WIDTH - 1,
        Y0,
        Y1,
        gbc_firmware::video::OUT_BYTES,
        row_ns(t),
    );

    // TE with the reset frame rate, then the closest setting to the Game
    // Boy's, from the oscillator that measurement implies; a second round
    // from the tuned measurement if it moves the choice.
    let (te_default, high) = measure_te(&mut dma, 121);
    report_te(out, "default", FrameRate::DEFAULT, te_default, high);
    // A row must be written in at most 5/6 of a scanned line (20% margin).
    let min_line_ns = row_ns(t) * 6 / 5;
    let mut rate = FrameRate::DEFAULT;
    let mut te_tuned = te_default;
    for round in 0..2 {
        let osc = osc_hz(rate, te_tuned.mean);
        let Some(next) = FrameRate::closest(osc, TARGET_NS, min_line_ns, 64) else {
            let _ = writeln!(out, "{PREFIX} lcd te no_setting osc_hz={osc}\r");
            break;
        };
        let _ = writeln!(
            out,
            "{PREFIX} lcd te choose round={round} osc_hz={osc} rtna={:#04x} fpa={:#04x} \
             bpa={:#04x} predicted_ns={} min_line_ns={min_line_ns} same_as_game_boy_const={}\r",
            next.rtna,
            next.fpa,
            next.bpa,
            u64::from(next.frame_clocks()) * 1_000_000_000 / u64::from(osc.max(1)),
            u8::from(next == FrameRate::GAME_BOY),
        );
        if round > 0 && next == rate {
            break;
        }
        rate = next;
        dma.lcd().set_frame_rate(rate);
        wait_since(cycles(), CPU_HZ / 10);
        let (s, high) = measure_te(&mut dma, 121);
        te_tuned = s;
        report_te(out, "tuned", rate, s, high);
        write_ahead(out, t, rate, s, high);
    }

    let mut s = Summary {
        te_default,
        te_tuned,
        rate,
        frames: 0,
        mismatches: 0,
        errors: 0,
        aborts: 0,
        control: 0,
        scale: Range::new(),
        xfer: Range::new(),
        delay: Range::new(),
    };

    // Timeout recovery self-test (M3 review P-1): queue the buffer and give
    // `wait` one cycle, so it times out with the transfer pending (TE is up
    // to a frame away). It must abandon the transfer: the channel disabled,
    // the state idle (the accessors do not spin), counted. The frames below
    // then show the next transfers work (mismatches stay 0).
    {
        dma.present();
        let r = dma.wait(1);
        let timed_out = matches!(r, Err(lcd_dma::Error::Timeout { .. }));
        let idle = dma.try_buffer().is_some();
        // Two TE periods: a stale pending state would start a transfer.
        let t0 = cycles();
        while cycles().wrapping_sub(t0) < TE_PERIOD_CYCLES {}
        let en = dma.registers()[4] & 1;
        let _ = writeln!(
            out,
            "{PREFIX} lcd dma timeout-selftest timed_out={} aborts={} idle={} en_after={en} ok={}\r",
            u8::from(timed_out),
            dma.aborts(),
            u8::from(idle),
            u8::from(
                timed_out
                    && dma.aborts() == 1
                    && idle
                    && en == 0
                    && dma.try_wait() == Some(Ok(None))
            )
        );
    }

    // Abort-while-running self-test (M4 review P-1): queue the buffer, let
    // TE start it, then time out about 10 us into the transfer, so `abort`
    // disables a channel that is streaming. It must wait for that buffer to
    // finish (CTCIF) before returning; the buffer is then presented again
    // at once (window commands right after the abort) and read back: 0
    // mismatches shows CASET/RASET were not interleaved with the tail of
    // the old data. (Measured: EN reads 0 about 32 cycles after the clear,
    // CTCIF about 8,700 cycles later, 18 us.)
    {
        for (i, p) in frame.iter_mut().enumerate() {
            *p = noise(u32::MAX, i);
        }
        scale_frame(frame, dma.buffer());
        dma.present();
        let started = dma.wait_started(2 * TE_PERIOD_CYCLES);
        let r = dma.wait(CPU_HZ / 50_000);
        let timed_out = matches!(r, Err(lcd_dma::Error::Timeout { pending: false, .. }));
        let a = dma.last_abort();
        let en = dma.registers()[4] & 1;
        // At once: the window commands, a whole transfer, the readback.
        dma.present();
        let next = dma.wait(TIMEOUT);
        let (l, buf) = dma.parts();
        let (next_bad, _) = readback(l, buf);
        let next_ok = matches!(next, Ok(Some(_))) && next_bad == 0;
        let _ = writeln!(
            out,
            "{PREFIX} lcd dma abort-running-selftest started={} timed_out={} aborts={} running={} \
             isr={:#x} abort_cycles={} en_after={en} next_mismatches={next_bad} ok={}\r",
            u8::from(started),
            u8::from(timed_out),
            dma.aborts(),
            u8::from(a.running),
            a.isr,
            a.cycles,
            u8::from(
                started
                    && timed_out
                    && dma.aborts() == 2
                    && a.running
                    && a.isr & 0b10 != 0
                    && en == 0
                    && next_ok
            )
        );
    }

    // Frames with readback.
    for n in 0..FRAMES {
        for (i, p) in frame.iter_mut().enumerate() {
            *p = noise(n, i);
        }
        let buf = dma.buffer();
        let t0 = cycles();
        scale_frame(frame, buf);
        let dt = cycles().wrapping_sub(t0);
        s.scale.add(dt);
        let Some(tr) = send(out, &mut dma, &mut s) else {
            continue;
        };
        let (l, buf) = dma.parts();
        let (bad, first) = readback(l, buf);
        s.frames += 1;
        s.mismatches += bad;
        let _ = write!(
            out,
            "{PREFIX} lcd dma frame n={n} scale_cycles={dt} te_to_start_cycles={} \
             mdma_cycles={} mdma_us={} mismatches={bad}",
            tr.te_to_start,
            tr.start_to_done,
            us(tr.start_to_done),
        );
        if let Some((x, y, got, want)) = first {
            let _ = write!(out, " first_bad=({x},{y}) got=");
            hex3(out, got);
            let _ = write!(out, " want=");
            hex3(out, want);
        }
        let _ = writeln!(out, "\r");
    }
    if s.frames > 0 {
        let r = dma.registers();
        let _ = writeln!(
            out,
            "{PREFIX} lcd dma after tcr={:#010x} bndtr={:#010x} sar={:#010x} dar={:#010x} \
             cr={:#010x} isr={:#010x} esr={:#010x}\r",
            r[0], r[1], r[2], r[3], r[4], r[5], r[6]
        );
    }

    // Control: the GRAM holds the last frame; one buffer word inverted
    // (both its pixels, every bit) must give exactly 2 mismatches.
    {
        let (l, buf) = dma.parts();
        buf[100][60] ^= 0xFFFF_FFFF;
        let (bad, first) = readback(l, buf);
        buf[100][60] ^= 0xFFFF_FFFF;
        s.control = bad;
        let _ = write!(
            out,
            "{PREFIX} lcd dma control changed_pixels=2 mismatches={bad} ok={}",
            u8::from(bad == 2)
        );
        if let Some((x, y, ..)) = first {
            let _ = write!(out, " first_bad=({x},{y})");
        }
        let _ = writeln!(out, "\r");
    }

    // Steady state: the main loop's pattern without the readback.
    let mut wait = Range::new();
    let mut scale = Range::new();
    let (mut xfer, mut delay) = (Range::new(), Range::new());
    lcd_dma::te_reset();
    let mut sent = 0u32;
    for n in 0..STEADY {
        for (i, p) in frame.iter_mut().enumerate() {
            *p = noise(n + 1000, i);
        }
        let t0 = cycles();
        let buf = dma.buffer();
        let t1 = cycles();
        scale_frame(frame, buf);
        let t2 = cycles();
        dma.present();
        wait.add(t1.wrapping_sub(t0));
        scale.add(t2.wrapping_sub(t1));
        if n > 0 {
            let last = dma.last();
            xfer.add(last.start_to_done);
            delay.add(last.te_to_start);
        }
        sent += 1;
    }
    if let Err(e) = dma.wait(TIMEOUT) {
        s.errors += 1;
        let _ = writeln!(out, "{PREFIX} lcd dma error {e:?}\r");
    }
    let te = lcd_dma::te_stats();
    let _ = write!(
        out,
        "{PREFIX} lcd dma steady frames={sent} te_edges={} te_mean_ns={}",
        te.edges,
        ns(te.mean)
    );
    range(out, "wait", wait);
    range(out, "scale", scale);
    range(out, "mdma", xfer);
    range(out, "te_to_start", delay);
    let _ = writeln!(out, "\r");

    // The test image, the same way.
    for (i, p) in frame.iter_mut().enumerate() {
        *p = bars(i % SW, i / SW);
    }
    scale_frame(frame, dma.buffer());
    let _ = send(out, &mut dma, &mut s);
    let (l, buf) = dma.parts();
    let (bad, _) = readback(l, buf);
    let _ = writeln!(out, "{PREFIX} lcd dma image bars mismatches={bad}\r");
    s.mismatches += bad;
    s.aborts = dma.aborts();
    summary(out, &s);
    let _ = writeln!(out, "{PREFIX} lcd dma end\r");
    loop {
        for _ in 0..10 {
            wait_since(cycles(), CPU_HZ / 2);
            led::toggle();
        }
        out.replay();
    }
}

/// One 240-pixel row (480 bytes) at the timing's nominal bus rate, ns.
fn row_ns(t: lcd::Timing) -> u32 {
    (480u64 * 1_000_000_000 / u64::from(t.write_bytes_per_s())) as u32
}

/// The write-ahead condition for setting `f` with the measured period and
/// TE high time: the scan reaches window row k (panel row 52 + k) at
/// `high + (52 + k) L` after the TE edge at the earliest (assuming the
/// scan starts when TE falls), the transfer finishes it at `d + (k + 1) W`
/// (d: the start delay, W: a row's write time). With W < L the tightest
/// row is k = 0; the slack grows by L - W per row.
fn write_ahead(out: &mut Out, t: lcd::Timing, f: FrameRate, s: lcd_dma::TeStats, high: u32) {
    let line_ns = u64::from(ns(s.mean)) / u64::from(f.lines());
    let w = u64::from(row_ns(t));
    let d = 2_000u64; // a start within 2 µs of the edge (measured below)
    let scan0 = u64::from(ns(high)) + u64::from(Y0) * line_ns;
    let _ = writeln!(
        out,
        "{PREFIX} lcd te write_ahead line_ns={line_ns} row_write_ns={w} per_row_slack_ns={} \
         first_row_scanned_ns={scan0} first_row_written_ns={} margin_ns={} \
         frame_written_ns={} last_row_scanned_ns={} ok={}\r",
        line_ns as i64 - w as i64,
        d + w,
        scan0 as i64 - (d + w) as i64,
        d + w * OUT_H as u64,
        u64::from(ns(high)) + u64::from(Y1) * line_ns,
        u8::from(line_ns > w && scan0 > d + w),
    );
}
