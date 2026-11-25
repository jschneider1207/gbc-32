//! Aliasing of the audio output, measured (M10 review A3-1).
//!
//! The core decimates the 4,194,304 Hz mixed signal to 32,768 Hz with a
//! 128-clock box filter (ADR 0021 section 8). Its gain is
//! `|sinc(f / 32768)|`: harmonics between 16 and 30 kHz fold back into the
//! audible band with little attenuation. These tests put numbers on that,
//! guard them (a ratchet: any change of the output filter moves them), and
//! `candidates` prototypes replacement filters on a clock-rate model of
//! the same signals. Results and method: `docs/reference/audio-aliasing.md`.
//!
//! Method. Each case plays one channel alone, through register writes run
//! by the public API (as `apu_output.rs` does), with a waveform period
//! chosen so that the output is exactly periodic over `N` samples and the
//! aliases fall between the true harmonics (see `Case::grid`). An `N`-point
//! DFT then has no leakage: each bin holds either one true harmonic or
//! folded ones. The reference is the ideal band-limited signal: the
//! Fourier series of the piecewise-constant waveform, cut at 16,384 Hz.
//!
//! Floats are fine here: this is host-only test code, and the measurement
//! is deterministic (no clock, no randomness).

use std::f64::consts::PI;

use gbc_core::{
    AUDIO_SAMPLE_CLOCKS, AUDIO_SAMPLE_RATE, CLOCK_HZ, Config, Emulator, Frame, Memory,
    StereoSample, WHITE, cart_ram_size,
};

const W: usize = AUDIO_SAMPLE_CLOCKS as usize;
const FS: f64 = AUDIO_SAMPLE_RATE as f64;

/// Samples dropped before measuring: the first waveform period after a
/// trigger differs (Pan Docs: a square starts at digital 0, the wave
/// channel replays its old sample buffer).
const SETTLE: usize = 1024;

/// Runs `LDH (reg),A` writes on a fresh CGB cartridge (after an APU power
/// cycle), then halts; returns `n` samples after `SETTLE`.
fn render(prog: &[(u8, u8)], n: usize) -> Vec<i32> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x143] = 0x80; // CGB
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // JP 0x0150
    let mut pc = 0x150;
    let power = [(0x26, 0x00), (0x26, 0x80), (0x24, 0x77)];
    for &(reg, value) in power.iter().chain(prog) {
        // LD A,value; LDH (reg),A
        rom[pc..pc + 4].copy_from_slice(&[0x3E, value, 0xE0, reg]);
        pc += 4;
    }
    // DI; HALT; JR to the HALT.
    rom[pc..pc + 4].copy_from_slice(&[0xF3, 0x76, 0x18, 0xFD]);
    let mut cart_ram = vec![0u8; cart_ram_size(&rom).unwrap()];
    let mut mem = Box::new(Memory::new());
    let mut emu = Emulator::new(&mut mem);
    assert!(
        emu.load(&rom, &mut cart_ram, Config::default())
            .unwrap()
            .is_none()
    );
    let mut frame: Box<Frame> = Box::new([WHITE; 160 * 144]);
    let mut out = Vec::with_capacity(SETTLE + n + 4096);
    let mut buf = vec![StereoSample::default(); 4096];
    while out.len() < SETTLE + n {
        let r = emu.run_cycles(4096 * u64::from(AUDIO_SAMPLE_CLOCKS), &mut frame, &mut buf);
        assert!(r.audio_samples > 0);
        for s in &buf[..r.audio_samples] {
            assert_eq!(s.left, s.right);
            out.push(i32::from(s.left));
        }
    }
    out.drain(..SETTLE);
    out.truncate(n);
    out
}

fn gcd(a: usize, b: usize) -> usize {
    if b == 0 { a } else { gcd(b, a % b) }
}

/// One measured signal: the program that plays it and its model, one
/// period as steps of a constant analog level (Pan Docs DAC: digital `d`
/// is analog `15 - 2d` on a -15..15 scale).
struct Case {
    name: String,
    prog: Vec<(u8, u8)>,
    /// One period's levels.
    steps: Vec<i8>,
    /// Clocks per step.
    step_clocks: usize,
}

impl Case {
    fn period(&self) -> usize {
        self.steps.len() * self.step_clocks
    }

    fn f0(&self) -> f64 {
        f64::from(CLOCK_HZ) / self.period() as f64
    }

    /// `(n, r)`: the output repeats every `n` samples, which hold `r`
    /// periods of the signal. Harmonic `m` lies at bin `m * r`. Harmonics
    /// above Nyquist fold onto multiples of `r` modulo `n`; with
    /// `gcd(r, n) = 1` and `r > 1` those coincide with a true harmonic
    /// only for harmonics a multiple of `r * 32768` Hz away, where the
    /// box filter has its nulls (and `r >= 2`). So "bin not a multiple of
    /// `r` below Nyquist" is "alias". A period that divides the sample
    /// grid (`r = 1`, e.g. a square with `2048 - f` a multiple of 4) folds
    /// every alias onto a harmonic instead: the same energy, heard as a
    /// timbre change rather than as stray tones. The cases avoid those.
    fn grid(&self) -> (usize, usize) {
        let p = self.period();
        let l = p / gcd(p, W) * W;
        let (n, r) = (l / W, l / p);
        assert!(r > 1 && gcd(r, n) == 1, "{}: aliases would hide", self.name);
        (n, r)
    }

    /// The model at clock rate over one output period (`n * 128` clocks).
    fn clocks(&self) -> Vec<i8> {
        let (n, _) = self.grid();
        (0..n * W)
            .map(|t| self.steps[t / self.step_clocks % self.steps.len()])
            .collect()
    }

    /// Amplitude of harmonic `m` of the ideal (continuous-time) signal:
    /// the Fourier series of a staircase.
    fn ideal(&self, m: usize) -> f64 {
        let s = self.steps.len();
        let (mut re, mut im) = (0.0, 0.0);
        for (k, &v) in self.steps.iter().enumerate() {
            let a = 2.0 * PI * ((m * k) % s) as f64 / s as f64;
            re += f64::from(v) * a.cos();
            im -= f64::from(v) * a.sin();
        }
        let x = m as f64 / s as f64;
        let sinc = if m.is_multiple_of(s) {
            0.0
        } else {
            (PI * x).sin() / (PI * x)
        };
        2.0 * re.hypot(im) / s as f64 * sinc.abs()
    }
}

/// A square channel (CH2: no sweep) at volume 15: `p = 2048 - f`.
/// Pan Docs: the duty step lasts `4p` clocks; 12.5% is one step in eight.
fn square(p: usize, duty: u8) -> Case {
    let f = 2048 - p as u16;
    let high = [1, 2, 4, 6][usize::from(duty)];
    Case {
        name: format!(
            "square {}% p={p}",
            ["12.5", "25", "50", "75"][usize::from(duty)]
        ),
        prog: vec![
            (0x25, 0x22),
            (0x16, duty << 6),
            (0x17, 0xF0),
            (0x18, f as u8),
            (0x19, 0x80 | (f >> 8) as u8),
        ],
        steps: (0..8).map(|i| if i < high { -15 } else { 15 }).collect(),
        step_clocks: 4 * p,
    }
}

/// The wave channel at 100% volume. Pan Docs: a sample lasts `2p` clocks,
/// the high nibble of each byte plays first.
fn wave(name: &str, ram: [u8; 16], p: usize) -> Case {
    let f = 2048 - p as u16;
    let mut prog: Vec<(u8, u8)> = vec![(0x25, 0x44)];
    prog.extend((0..16).map(|i| (0x30 + i as u8, ram[i])));
    prog.extend([
        (0x1A, 0x80),
        (0x1C, 0x20),
        (0x1D, f as u8),
        (0x1E, 0x80 | (f >> 8) as u8),
    ]);
    let mut steps: Vec<i8> = ram
        .iter()
        .flat_map(|b| [b >> 4, b & 15])
        .map(|d| 15 - 2 * d as i8)
        .collect();
    // A waveform written twice is a signal of half the period.
    if steps[..16] == steps[16..] {
        steps.truncate(16);
    }
    Case {
        name: format!("wave {name} p={p}"),
        prog,
        steps,
        step_clocks: 2 * p,
    }
}

/// The noise channel at volume 15, divider code 0 (8 clocks) shifted by
/// `shift`. Pan Docs LFSR: the XNOR of bits 0 and 1 goes to bit 15 (and
/// bit 7 in the short mode), then the register shifts right; bit 0 is the
/// output. The short mode repeats every 127 steps, the long one every
/// 32,767.
fn noise(short: bool, shift: u8) -> Case {
    let mut lfsr = 0u16;
    let mut step = || {
        let x = !(lfsr ^ (lfsr >> 1)) & 1;
        lfsr = (lfsr & 0x7FFF) | (x << 15);
        if short {
            lfsr = (lfsr & !0x80) | (x << 7);
        }
        lfsr >>= 1;
        if lfsr & 1 != 0 { -15i8 } else { 15 }
    };
    for _ in 0..64 {
        step();
    }
    let len = if short { 127 } else { 32767 };
    Case {
        name: format!(
            "noise {} {} kHz",
            if short { "7-bit" } else { "15-bit" },
            524 >> shift
        ),
        prog: vec![
            (0x25, 0x88),
            (0x21, 0xF0),
            (0x22, shift << 4 | u8::from(short) << 3),
            (0x23, 0x80),
        ],
        steps: (0..len).map(|_| step()).collect(),
        step_clocks: 8 << shift,
    }
}

/// A filter from the clock-rate signal to 32,768 Hz: the kernel's taps,
/// one per clock, applied at every 128th clock (sum 1).
type Kernel = Vec<f64>;

fn normalised(mut h: Kernel) -> Kernel {
    let s: f64 = h.iter().sum();
    h.iter_mut().for_each(|v| *v /= s);
    h
}

fn boxcar(n: usize) -> Kernel {
    vec![1.0 / n as f64; n]
}

/// `a` convolved with the taps `g` placed `spacing` clocks apart: `a` as
/// the first decimation stage, `g` as a FIR at the intermediate rate.
fn cascade(a: &[f64], g: &[f64], spacing: usize) -> Kernel {
    let mut h = vec![0.0; a.len() + (g.len() - 1) * spacing];
    for (i, &gi) in g.iter().enumerate() {
        for (k, &ak) in a.iter().enumerate() {
            h[i * spacing + k] += gi * ak;
        }
    }
    normalised(h)
}

/// The zeroth-order modified Bessel function, for the Kaiser window.
fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..40 {
        term *= (x / 2.0 / f64::from(k)).powi(2);
        sum += term;
    }
    sum
}

/// A Kaiser-windowed sinc low-pass of `taps` taps with its -6 dB point at
/// `cutoff` (cycles per tap). `bits > 0` rounds the taps to integers with
/// the largest at about `2^bits`, as an integer core would hold them.
fn lowpass(taps: usize, cutoff: f64, beta: f64, bits: u32) -> Kernel {
    let mid = (taps - 1) as f64 / 2.0;
    let mut h: Kernel = (0..taps)
        .map(|i| {
            let t = i as f64 - mid;
            let sinc = if t == 0.0 {
                1.0
            } else {
                (2.0 * PI * cutoff * t).sin() / (2.0 * PI * cutoff * t)
            };
            let r = t / (taps as f64 / 2.0);
            sinc * bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta)
        })
        .collect();
    if bits > 0 {
        let max = h.iter().fold(0.0f64, |a, &b| a.max(b));
        let scale = f64::from(1u32 << bits) / max;
        h.iter_mut().for_each(|v| *v = (*v * scale).round());
    }
    normalised(h)
}

/// The model through `h`, the first window starting `phase` clocks into the
/// period: `n` output samples in level units.
fn filter(case: &Case, h: &[f64], phase: usize) -> Vec<f64> {
    let x = case.clocks();
    let (n, _) = case.grid();
    (0..n)
        .map(|i| {
            h.iter()
                .enumerate()
                .map(|(k, &hk)| hk * f64::from(x[(phase + i * W + k) % x.len()]))
                .sum()
        })
        .collect()
}

/// What a measurement found, in dB.
#[derive(Debug)]
struct Alias {
    /// The strongest alias below 16,384 Hz, relative to the ideal
    /// fundamental (which may itself be ultrasonic), and its frequency.
    worst: f64,
    worst_hz: f64,
    /// The same within 1 to 8 kHz, where the ear is most sensitive.
    worst_mid: f64,
    /// All alias energy below Nyquist over the ideal signal's energy
    /// below Nyquist (infinite when no harmonic is audible).
    energy: f64,
    /// The highest true harmonic below Nyquist against the ideal one: the
    /// filter's passband droop.
    droop: f64,
}

fn db(ratio: f64) -> f64 {
    20.0 * ratio.max(1e-12).log10()
}

/// Measures one output period `y` (level units) of `case`.
fn measure(case: &Case, y: &[f64]) -> Alias {
    let (n, r) = case.grid();
    assert_eq!(y.len(), n);
    let (cos, sin): (Vec<f64>, Vec<f64>) = (0..n)
        .map(|i| (2.0 * PI * i as f64 / n as f64).sin_cos())
        .map(|(s, c)| (c, s))
        .unzip();
    let amp = |b: usize| {
        let (mut re, mut im) = (0.0, 0.0);
        for (i, &v) in y.iter().enumerate() {
            let k = b * i % n;
            re += v * cos[k];
            im -= v * sin[k];
        }
        2.0 * re.hypot(im) / n as f64
    };
    let fund = case.ideal(1);
    // True harmonics below Nyquist (n is odd: no bin at Nyquist itself).
    assert!(n % 2 == 1);
    let harmonics = (n / 2) / r;
    let (mut signal, mut kept, mut droop) = (0.0, 0.0, 0.0);
    for m in 1..=harmonics {
        let (ideal, got) = (case.ideal(m), amp(m * r));
        signal += ideal * ideal / 2.0;
        kept += got * got / 2.0;
        if ideal > 1e-9 {
            droop = db(got / ideal);
        }
    }
    let mean = y.iter().sum::<f64>() / n as f64;
    let total = y.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    let mut a = Alias {
        worst: f64::NEG_INFINITY,
        worst_hz: 0.0,
        worst_mid: f64::NEG_INFINITY,
        // Parseval: what is not a true harmonic is alias.
        energy: 10.0 * ((total - kept).max(1e-12) / signal).log10(),
        droop,
    };
    // The long noise period has 16,383 bins: the energy is enough there.
    if n <= 4096 {
        for b in (1..=n / 2).filter(|b| b % r != 0) {
            let (level, hz) = (db(amp(b) / fund), b as f64 * FS / n as f64);
            if level > a.worst {
                (a.worst, a.worst_hz) = (level, hz);
            }
            if (1000.0..=8000.0).contains(&hz) {
                a.worst_mid = a.worst_mid.max(level);
            }
        }
    }
    a
}

/// The core's output for `case`: one output period of samples.
fn core_samples(case: &Case) -> Vec<i32> {
    let (n, _) = case.grid();
    let mut got = render(&case.prog, 2 * n);
    assert_eq!(got[..n], got[n..], "{}: not periodic", case.name);
    got.truncate(n);
    got
}

/// Samples in level units: 512 per unit of analog level at master volume 7.
fn levels(samples: &[i32]) -> Vec<f64> {
    samples.iter().map(|&s| f64::from(s) / 512.0).collect()
}

/// Checks that the core's samples are the model through a 128-clock box
/// filter, exactly (with master volume 7 a sample is 4 times the window's
/// level sum), and returns the phase: where in the model's period the
/// first window starts. A few levels depend on it by a fraction of a dB: a
/// harmonic and its mirror image, `r * 32768` Hz minus it, share a bin.
fn phase(case: &Case, got: &[i32]) -> usize {
    let (n, _) = case.grid();
    let x = case.clocks();
    let mut prefix = vec![0i32; x.len() + W + 1];
    for t in 0..x.len() + W {
        prefix[t + 1] = prefix[t] + i32::from(x[t % x.len()]);
    }
    let window = |t: usize| {
        let t = t % x.len();
        4 * (prefix[t + W] - prefix[t])
    };
    let phase = (0..case.period()).find(|&o| (0..n).all(|i| window(o + i * W) == got[i]));
    let Some(phase) = phase else {
        panic!("{}: the output is not the box-filtered model", case.name)
    };
    phase
}

/// `2048 - f` for the squares: odd (see `Case::grid`), 502 Hz to 131 kHz.
/// 63 is the C7 of the review's hand computation (2,080 Hz).
const SQUARE_PERIODS: [usize; 14] = [261, 131, 97, 63, 47, 31, 23, 15, 11, 9, 7, 5, 3, 1];

/// A 32-step ramp and a 16-step one played twice (double the pitch).
const SAW32: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
];
const SAW16: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
];
/// A 50% square of 32 steps.
const WSQUARE: [u8; 16] = [
    0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0, 0, 0, 0, 0,
];

fn cases() -> Vec<Case> {
    let mut v = Vec::new();
    for duty in [2, 0] {
        v.extend(SQUARE_PERIODS.iter().map(|&p| square(p, duty)));
    }
    v.extend([
        wave("saw32", SAW32, 31),
        wave("saw32", SAW32, 15),
        wave("saw16", SAW16, 31),
        wave("saw16", SAW16, 7),
        wave("square", WSQUARE, 15),
        wave("saw32", SAW32, 1),
        noise(true, 0),
        noise(true, 1),
        noise(true, 2),
        noise(true, 3),
        noise(false, 0),
    ]);
    v
}

fn row(name: &str, f0: f64, a: &Alias) -> String {
    format!(
        "| {name} | {f0:.0} | {:.1} | {:.0} | {:.1} | {:.1} | {:.2} |",
        a.worst, a.worst_hz, a.worst_mid, a.energy, a.droop
    )
}

/// The current levels, as measured at this commit: (worst alias dBc,
/// alias-to-signal dB) per case of `cases()`, in order. A ratchet: a
/// change of the output filter must update these, and with them
/// `docs/reference/audio-aliasing.md`. `INF`: no value (no audible
/// harmonic, or the long noise period, whose bins are not searched).
const INF: f64 = f64::INFINITY;
const CURRENT: [(f64, f64); 39] = [
    // Square, 50% duty, `SQUARE_PERIODS` in order.
    (-34.43, -28.91),
    (-28.86, -25.73),
    (-26.90, -24.75),
    (-24.37, -23.30),
    (-22.73, -22.21),
    (-21.04, -20.91),
    (-13.84, -13.63),
    (-22.14, -20.83),
    (-31.19, -28.01),
    (-23.48, -23.20),
    (-5.29, INF),
    (-12.71, INF),
    (-13.61, INF),
    (INF, INF),
    // Square, 12.5% duty.
    (-27.68, -25.28),
    (-23.48, -22.53),
    (-19.48, -22.16),
    (-21.85, -21.67),
    (-14.53, -18.62),
    (-8.12, -10.84),
    (-6.43, -8.10),
    (-5.47, -4.90),
    (-10.32, -9.38),
    (-17.80, -15.45),
    (-5.64, INF),
    (-12.66, INF),
    (-11.82, INF),
    (INF, INF),
    // Wave.
    (-22.26, -20.49),
    (-16.57, -16.42),
    (-16.49, -16.25),
    (-5.61, INF),
    (-22.08, -22.23),
    (INF, INF),
    // Noise: 7-bit at 524, 262, 131 and 65 kHz, then 15-bit.
    (-4.02, -4.85),
    (-4.10, -6.62),
    (-4.46, -8.52),
    (-3.06, -6.92),
    (INF, -7.06),
];

const TOLERANCE_DB: f64 = 0.05;

fn close(got: f64, want: f64) -> bool {
    if want.is_finite() {
        (got - want).abs() <= TOLERANCE_DB
    } else {
        !got.is_finite()
    }
}

#[test]
fn box_filter_alias_levels() {
    let cases = cases();
    assert_eq!(cases.len(), CURRENT.len());
    println!(
        "| signal | f0 Hz | worst alias dBc | at Hz | worst 1-8 kHz dBc | alias/signal dB | droop dB |"
    );
    println!("|---|---|---|---|---|---|---|");
    let mut bad = Vec::new();
    for (case, want) in cases.iter().zip(CURRENT) {
        let a = measure(case, &levels(&core_samples(case)));
        println!("{}", row(&case.name, case.f0(), &a));
        if !close(a.worst, want.0) || !close(a.energy, want.1) {
            bad.push(format!(
                "{}: ({:.2}, {:.2}), recorded {want:?}",
                case.name, a.worst, a.energy
            ));
        }
    }
    assert!(bad.is_empty(), "alias levels moved:\n{}", bad.join("\n"));
}

/// The candidates of `docs/reference/audio-aliasing.md`, on the model.
fn kernels() -> Vec<(&'static str, Kernel)> {
    let b32 = boxcar(32);
    vec![
        ("box128 (current)", boxcar(128)),
        (
            "box64 + [1,2,1]",
            cascade(&boxcar(64), &[1.0, 2.0, 1.0], 64),
        ),
        ("triangle 256", cascade(&boxcar(128), &boxcar(128), 1)),
        (
            "box64 + 8-tap",
            cascade(&boxcar(64), &lowpass(8, 0.25, 3.0, 8), 64),
        ),
        (
            "box64 + 16-tap",
            cascade(&boxcar(64), &lowpass(16, 0.236, 5.0, 10), 64),
        ),
        (
            "box64 + 32-tap",
            cascade(&boxcar(64), &lowpass(32, 0.23, 7.0, 12), 64),
        ),
        (
            "box32 + 7-tap halfband + 15-tap",
            cascade(
                &cascade(&b32, &[-1.0, 0.0, 9.0, 16.0, 9.0, 0.0, -1.0], 32),
                &lowpass(15, 0.236, 5.0, 10),
                64,
            ),
        ),
        (
            "box32 + 16-tap",
            cascade(&b32, &lowpass(16, 0.125, 3.0, 8), 32),
        ),
        (
            "box32 + 32-tap",
            cascade(&b32, &lowpass(32, 0.118, 5.0, 10), 32),
        ),
        (
            "box32 + 64-tap",
            cascade(&b32, &lowpass(64, 0.115, 7.0, 12), 32),
        ),
        ("sinc 8 samples", lowpass(8 * 128, 0.46 / 128.0, 5.0, 0)),
        ("sinc 16 samples", lowpass(16 * 128, 0.46 / 128.0, 8.0, 0)),
    ]
}

/// Prints the candidate comparison (`cargo test -p gbc-core --release
/// --test apu_aliasing -- --ignored --nocapture`). Not a check: the
/// candidates are prototypes, nothing in the core implements them.
#[test]
#[ignore = "report only; slow without --release"]
fn candidates() {
    let cases: Vec<(Case, usize)> = cases()
        .into_iter()
        .filter(|c| c.steps.len() < 1000)
        .map(|c| {
            let phase = phase(&c, &core_samples(&c));
            (c, phase)
        })
        .collect();
    let mut summary = Vec::new();
    for (name, h) in kernels() {
        println!("\n### {name} ({} clocks)\n", h.len());
        println!(
            "| signal | f0 Hz | worst alias dBc | at Hz | worst 1-8 kHz dBc | alias/signal dB | droop dB |"
        );
        println!("|---|---|---|---|---|---|---|");
        // Worst alias, worst within 1-8 kHz and worst alias-to-signal over
        // the notes music uses (squares and wave up to 4.4 kHz), over the
        // tones above that, and over the 7-bit noise.
        let mut worst = [[f64::NEG_INFINITY; 3]; 3];
        let mut droop = [0.0; 2];
        for (case, phase) in &cases {
            let a = measure(case, &filter(case, &h, *phase));
            println!("{}", row(&case.name, case.f0(), &a));
            let group = match () {
                () if case.name.starts_with("noise") => 2,
                () if case.f0() > 4400.0 => 1,
                () => 0,
            };
            let w = &mut worst[group];
            let energy = if a.energy.is_finite() { a.energy } else { w[2] };
            *w = [w[0].max(a.worst), w[1].max(a.worst_mid), w[2].max(energy)];
            // Fundamental-only cases: the filter gain at 8.7 and 14.6 kHz.
            match case.name.as_str() {
                "square 50% p=15" => droop[0] = a.droop,
                "square 50% p=9" => droop[1] = a.droop,
                _ => {}
            }
        }
        let [t, u, n] = worst;
        summary.push(format!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2} | {:.2} |",
            h.len(),
            t[1],
            t[0],
            t[2],
            u[1],
            u[0],
            n[1],
            n[0],
            droop[0],
            droop[1]
        ));
    }
    println!("\n### Summary\n");
    println!(
        "| filter | clocks | notes to 4.4 kHz: worst 1-8 kHz dBc | worst dBc | worst alias/signal dB | higher tones: worst 1-8 kHz dBc | worst dBc | 7-bit noise: worst 1-8 kHz dBc | worst dBc | gain at 8.7 kHz dB | at 14.6 kHz dB |"
    );
    println!("|---|---|---|---|---|---|---|---|---|---|---|");
    println!("{}", summary.join("\n"));
}

/// The model reproduces the core: every case's samples are the model's
/// through the 128-clock box, exactly (`phase`), and `filter` with that
/// box measures the same. So the candidates' numbers, which come from the
/// model, are comparable with the measured ones.
#[test]
fn model_matches_the_core() {
    for case in cases() {
        let got = core_samples(&case);
        let phase = phase(&case, &got);
        // The long noise period: the exact match is the check.
        if case.steps.len() > 1000 {
            continue;
        }
        let core = measure(&case, &levels(&got));
        let model = measure(&case, &filter(&case, &boxcar(128), phase));
        for (a, b) in [
            (core.worst, model.worst),
            (core.worst_mid, model.worst_mid),
            (core.energy, model.energy),
            (core.droop, model.droop),
        ] {
            assert!((a - b).abs() < 1e-6 || a == b, "{}: {a} vs {b}", case.name);
        }
    }
}
