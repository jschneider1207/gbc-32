//! The `dac-probe` image (plan Stage 2 M4): the audio output,
//! [`audio_dac`], checked and measured on the board by its registers and
//! timing (nobody has to listen). Reports over the UART, every line
//! starting with `gbc-nucleo-h753`:
//!
//! - `dac config`: the DAC, TIM6, DMAMUX1 and DMA1 stream 0 registers after
//!   `AudioDac::new`, the buffer and its MPU region;
//! - `dac ramp`: the start ramp (250 ms): its duration and refill cost;
//! - `dac halves`: for about 1 s the source fills each half with a tag of
//!   the half (right channel) and its call number (left), and the main
//!   loop samples NDTR and DOR1/DOR2 together: away from the half
//!   boundaries, DOR must hold the tag of the half NDTR says the DMA
//!   reads and the call number of the refill before the last (the last
//!   refill went to the other half). `bad` > 0 means a refill hit the half
//!   being played, or the DMA read the wrong one;
//! - `dac tone`: 440 Hz left, 660 Hz right, ±600 codes around mid-scale
//!   (±0.48 V), for 10 s; over 8 s of it the refill interrupts against the
//!   cycle counter give the sample rate (and, with the LSE measurement of
//!   the CPU clock from the banner, the rate in real time), the
//!   interrupt's cost, and 8 DOR readings (in range, changing);
//! - `dac silence`: 2 s at mid-scale, DOR sampled;
//! - `dac stop`: the stop ramp, then DOR (0 expected), a restart and a
//!   second stop;
//! - `dac summary ... ok=<0|1>`, then the whole report replayed every 5 s
//!   with the DAC idle at code 0.
#![no_std]
#![no_main]

use core::fmt::Write;
use core::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use gbc_firmware::fmt_buf::append_truncated;
use gbc_nucleo_h753::audio_dac::{self, AudioDac, FRAMES, HALF, MID, SILENCE, frame};
use gbc_nucleo_h753::pac::interrupt;
use gbc_nucleo_h753::{self as board, CPU_HZ, Uart, cycles, led, mpu, wait_since};

const PREFIX: &str = "gbc-nucleo-h753";
const IMAGE: &str = "dac-probe";

/// Bytes of output kept for the replay.
const LOG_BYTES: usize = 8 << 10;
/// The UART, with every byte also appended to `log` while it has room
/// (truncating, `fmt_buf::append_truncated`). `log` is the run's output,
/// kept (a `take_once!` buffer in DTCM `.bss`) and replayed every 5 s: the
/// run is over before a serial reader started after the flash can catch it.
struct Out<'a> {
    uart: &'a mut Uart,
    log: &'static mut [u8; LOG_BYTES],
    len: usize,
}

impl Write for Out<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let _ = self.uart.write_str(s);
        append_truncated(&mut self.log[..], &mut self.len, s);
        Ok(())
    }
}

impl Out<'_> {
    fn replay(&mut self) {
        let _ = writeln!(self.uart, "{PREFIX} dac replay begin bytes={}\r", self.len);
        for &b in &self.log[..self.len] {
            self.uart.write_byte(b);
        }
        let _ = writeln!(self.uart, "{PREFIX} dac replay end\r");
    }
}

/// What the source plays ([`source`]).
const MODE_HALVES: u8 = 0;
const MODE_TONE: u8 = 1;
const MODE_SILENCE: u8 = 2;
static MODE: AtomicU8 = AtomicU8::new(MODE_HALVES);
/// Source calls in [`MODE_HALVES`].
static CALLS: AtomicU32 = AtomicU32::new(0);
/// The tone's phase accumulators (ISR only).
static PHASE_L: AtomicU32 = AtomicU32::new(0);
static PHASE_R: AtomicU32 = AtomicU32::new(0);

/// The halves test's codes: left = `CALL_BASE` + call number mod 256,
/// right = the half's tag. Within ±256 of mid-scale (±0.2 V).
const CALL_BASE: u16 = MID - 128;
const TAG: [u16; 2] = [MID - 256, MID + 256];

/// The tone's amplitude in codes (±0.48 V of the 3.3 V full scale).
const AMP: i32 = 600;
/// One period of a sine, 256 steps, scaled to [`AMP`]: Bhaskara I's
/// approximation 16t(P - t) / (5P² - 4t(P - t)) per half period P = 128
/// (error under 0.2% of the amplitude), all integer.
const SINE: [i16; 256] = sine();
const fn sine() -> [i16; 256] {
    let mut t = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        let x = (i % 128) as i32;
        let q = x * (128 - x);
        let v = AMP * 16 * q / (81_920 - 4 * q);
        t[i] = if i < 128 { v as i16 } else { -v as i16 };
        i += 1;
    }
    t
}
/// Phase increments per sample at 32,768 Hz: f x 2^32 / 2^15.
const INC_L: u32 = 440 << 17;
const INC_R: u32 = 660 << 17;

/// The source `on_dma_codes` calls with the half to fill (DAC words).
fn source(half: usize, out: &mut [u32]) {
    match MODE.load(Ordering::Relaxed) {
        MODE_HALVES => {
            let n = CALLS.load(Ordering::Relaxed);
            CALLS.store(n.wrapping_add(1), Ordering::Relaxed);
            out.fill(frame(CALL_BASE + (n & 0xFF) as u16, TAG[half]));
        }
        MODE_TONE => {
            let (mut l, mut r) = (
                PHASE_L.load(Ordering::Relaxed),
                PHASE_R.load(Ordering::Relaxed),
            );
            for f in out.iter_mut() {
                let a = (i32::from(MID) + i32::from(SINE[(l >> 24) as usize])) as u16;
                let b = (i32::from(MID) + i32::from(SINE[(r >> 24) as usize])) as u16;
                *f = frame(a, b);
                l = l.wrapping_add(INC_L);
                r = r.wrapping_add(INC_R);
            }
            PHASE_L.store(l, Ordering::Relaxed);
            PHASE_R.store(r, Ordering::Relaxed);
        }
        _ => out.fill(SILENCE),
    }
}

#[interrupt]
fn DMA1_STR0() {
    audio_dac::on_dma_codes(source);
}

fn ms(c: u32) -> u32 {
    (u64::from(c) * 1000 / u64::from(CPU_HZ)) as u32
}

/// Waits up to `ms` for `cond`.
fn wait_for(ms: u32, mut cond: impl FnMut() -> bool) -> bool {
    let t0 = cycles();
    while !cond() {
        if cycles().wrapping_sub(t0) > ms * (CPU_HZ / 1000) {
            return false;
        }
    }
    true
}

/// Waits `ms` milliseconds (at most about 9 s: the cycle counter wraps).
fn sleep_ms(ms: u32) {
    wait_since(cycles(), ms * (CPU_HZ / 1000));
}

fn stats_line(out: &mut Out, what: &str) {
    let s = audio_dac::stats();
    let _ = writeln!(
        out,
        "{PREFIX} dac {what} refills={} late={} spurious={} te={} dme={} fe={} udr1={} udr2={} \
         isr_cycles_min={} isr_cycles_max={}\r",
        s.refills, s.late, s.spurious, s.te, s.dme, s.fe, s.udr1, s.udr2, s.isr_min, s.isr_max
    );
}

#[cortex_m_rt::entry]
fn main() -> ! {
    let mut board = board::init(PREFIX);
    let lse_cpu_hz = board.measure_clock();
    board.banner(IMAGE);
    // `.bss` (all zeros, no `link_section`): DTCM.
    let log =
        gbc_nucleo_h753::take_once!([u8; LOG_BYTES] = [0; LOG_BYTES]).expect("log taken twice");
    let mut out = Out {
        uart: &mut board.uart,
        log,
        len: 0,
    };
    let out = &mut out;
    let mut ok = true;

    let mut dac = AudioDac::new();
    let r = dac.registers();
    let (buf, bytes) = dac.buffer();
    let (rbar, rasr) = mpu::region(mpu::REGION_AUDIO_BUF);
    let _ = writeln!(
        out,
        "{PREFIX} dac config tim_hz={} tim_div={} rate_mhz={} frames={FRAMES} half={HALF} \
         buf={buf:#010x} bytes={bytes} mpu_region={} rbar={rbar:#010x} rasr={rasr:#010x}\r",
        audio_dac::TIM_HZ,
        audio_dac::TIM_DIV,
        audio_dac::RATE_MILLIHZ,
        mpu::REGION_AUDIO_BUF,
    );
    let _ = writeln!(
        out,
        "{PREFIX} dac regs dac_cr={:#010x} dac_mcr={:#010x} dac_sr={:#010x} dac_ccr={:#010x} \
         tim6_cr1={:#06x} tim6_cr2={:#06x} tim6_psc={} tim6_arr={} dmamux_c0cr={:#010x} \
         dma_s0cr={:#010x} ndtr={} par={:#010x} m0ar={:#010x} fcr={:#04x}\r",
        r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], r[9], r[10], r[11], r[12], r[13]
    );
    // The fixed configuration as the driver documents it.
    let cfg_ok = r[0] == audio_dac::DAC_CR
        && r[1] == audio_dac::DAC_MCR
        && r[5] == audio_dac::TIM_CR2
        && r[6] == 0
        && r[7] == audio_dac::TIM_DIV - 1
        && r[8] == audio_dac::DMAMUX_REQ_DAC_CH1
        && rasr & 1 == 1
        && rbar & !0x1F == buf as u32;
    ok &= cfg_ok;

    // Start: the ramp, then the halves test.
    let t0 = cycles();
    dac.start();
    let r = dac.registers();
    let _ = writeln!(
        out,
        "{PREFIX} dac started dac_cr={:#010x} tim6_cr1={:#06x} dma_s0cr={:#010x} ndtr={}\r",
        r[0], r[4], r[9], r[10]
    );
    let up = wait_for(1000, || {
        dac.phase() == 2 && CALLS.load(Ordering::Relaxed) >= 3
    });
    let _ = writeln!(
        out,
        "{PREFIX} dac ramp done={} ms={} (ramp {} frames = {} ms nominal)\r",
        u8::from(up),
        ms(cycles().wrapping_sub(t0)),
        audio_dac::RAMP_FRAMES,
        audio_dac::RAMP_FRAMES * 1000 / audio_dac::TARGET_HZ,
    );
    ok &= up;
    stats_line(out, "ramp");
    audio_dac::reset_isr_cost();

    // The halves test.
    let (mut good, mut bad, mut skipped) = (0u32, 0u32, 0u32);
    let mut first_bad = None;
    let t0 = cycles();
    while cycles().wrapping_sub(t0) < CPU_HZ {
        let ((_, ndtr, d1, d2), calls) =
            cortex_m::interrupt::free(|_| (audio_dac::position(), CALLS.load(Ordering::Relaxed)));
        // Frames already moved to DHR; DOR holds one or two before.
        let p = (FRAMES - usize::from(ndtr)) % FRAMES;
        let k = p % HALF;
        if !(8..HALF - 4).contains(&k) {
            skipped += 1;
            wait_since(cycles(), CPU_HZ / 100_000);
            continue;
        }
        let h = p / HALF;
        let want = (TAG[h], CALL_BASE + (calls.wrapping_sub(2) & 0xFF) as u16);
        if (d2, d1) == want {
            good += 1;
        } else {
            bad += 1;
            first_bad.get_or_insert((ndtr, d1, d2, calls));
        }
        // About 10 us between samples (spread over the half).
        wait_since(cycles(), CPU_HZ / 100_000);
    }
    let _ = write!(
        out,
        "{PREFIX} dac halves good={good} bad={bad} skipped={skipped} calls={}",
        CALLS.load(Ordering::Relaxed)
    );
    if let Some((n, d1, d2, c)) = first_bad {
        let _ = write!(out, " first_bad=ndtr:{n},dor1:{d1},dor2:{d2},calls:{c}");
    }
    let _ = writeln!(out, "\r");
    ok &= bad == 0 && good > 1000;
    stats_line(out, "halves");

    // The tone: 10 s; the measurement over 8 s of it.
    MODE.store(MODE_TONE, Ordering::Relaxed);
    sleep_ms(500);
    audio_dac::reset_isr_cost();
    let a = audio_dac::stats();
    let mut dor = [(0u16, 0u16); 8];
    for d in dor.iter_mut() {
        sleep_ms(1000);
        let (_, _, d1, d2) = audio_dac::position();
        *d = (d1, d2);
    }
    // The last refill interrupt after the 8 s: the window ends on one.
    let n = audio_dac::stats().refills;
    wait_for(20, || audio_dac::stats().refills != n);
    let b = audio_dac::stats();
    let frames = u64::from(b.refills.wrapping_sub(a.refills)) * HALF as u64;
    let cyc = u64::from(b.last_at.wrapping_sub(a.last_at));
    let rate_dwt = frames * u64::from(CPU_HZ) * 1000 / cyc;
    let ppm = |mhz: u64| (mhz as i64 - 32_768_000) * 1_000_000 / 32_768_000;
    let _ = write!(
        out,
        "{PREFIX} dac tone left_hz=440 right_hz=660 amp={AMP} window_ms={} frames={frames} \
         rate_mhz_dwt={rate_dwt} ppm_dwt={}",
        ms(cyc as u32),
        ppm(rate_dwt)
    );
    if let Some(hz) = lse_cpu_hz {
        let rate = frames * u64::from(hz) * 1000 / cyc;
        let _ = write!(
            out,
            " lse_cpu_hz={hz} rate_mhz_lse={rate} ppm_lse={}",
            ppm(rate)
        );
    }
    let _ = write!(out, " dor=");
    let lo = (i32::from(MID) - AMP - 1) as u16;
    let hi = (i32::from(MID) + AMP + 1) as u16;
    let mut in_range = true;
    for (i, &(d1, d2)) in dor.iter().enumerate() {
        let _ = write!(out, "{}{d1}/{d2}", if i == 0 { "" } else { "," });
        in_range &= (lo..=hi).contains(&d1) && (lo..=hi).contains(&d2);
    }
    let changing = dor.iter().any(|&d| d != dor[0]);
    let _ = writeln!(
        out,
        " dor_in_range={} dor_changing={}\r",
        u8::from(in_range),
        u8::from(changing)
    );
    // The DWT rate shows the divider (both clocks come from the HSI):
    // within 1 ppm of the nominal 32,768.675 Hz.
    ok &= in_range && changing && rate_dwt.abs_diff(u64::from(audio_dac::RATE_MILLIHZ)) <= 33;
    stats_line(out, "tone");
    sleep_ms(1500);

    // Silence at mid-scale.
    MODE.store(MODE_SILENCE, Ordering::Relaxed);
    sleep_ms(100);
    let mut silent = true;
    for _ in 0..20 {
        sleep_ms(95);
        let (_, _, d1, d2) = audio_dac::position();
        silent &= d1 == MID && d2 == MID;
    }
    let _ = writeln!(
        out,
        "{PREFIX} dac silence mid={} ms=2000\r",
        u8::from(silent)
    );
    ok &= silent;

    // Stop, restart, stop.
    let t0 = cycles();
    let s1 = dac.stop();
    let stop_ms = ms(cycles().wrapping_sub(t0));
    let (_, _, d1, d2) = audio_dac::position();
    let r = dac.registers();
    let _ = writeln!(
        out,
        "{PREFIX} dac stop ok={} ms={stop_ms} dor={d1}/{d2} dac_cr={:#010x} tim6_cr1={:#06x} \
         dma_s0cr={:#010x} running={}\r",
        u8::from(s1.is_ok()),
        r[0],
        r[4],
        r[9],
        u8::from(dac.running()),
    );
    ok &= s1.is_ok() && d1 == 0 && d2 == 0 && !dac.running();
    dac.start();
    let again = wait_for(1000, || dac.phase() == 2);
    sleep_ms(300);
    let (_, _, a1, a2) = audio_dac::position();
    let s2 = dac.stop();
    let (_, _, z1, z2) = audio_dac::position();
    let _ = writeln!(
        out,
        "{PREFIX} dac restart playing={} dor={a1}/{a2} stop_ok={} dor_after={z1}/{z2}\r",
        u8::from(again),
        u8::from(s2.is_ok())
    );
    ok &= again && a1 == MID && a2 == MID && s2.is_ok() && z1 == 0 && z2 == 0;
    stats_line(out, "final");
    let s = audio_dac::stats();
    ok &= s.late == 0
        && s.spurious == 0
        && s.te == 0
        && s.dme == 0
        && s.fe == 0
        && s.udr1 == 0
        && s.udr2 == 0;
    let _ = writeln!(
        out,
        "{PREFIX} dac summary config={} ok={}\r",
        u8::from(cfg_ok),
        u8::from(ok)
    );
    let _ = writeln!(out, "{PREFIX} dac end\r");
    loop {
        for _ in 0..10 {
            wait_since(cycles(), CPU_HZ / 2);
            led::toggle();
        }
        out.replay();
    }
}
