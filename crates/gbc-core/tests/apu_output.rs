//! Analytic checks of the audio the core produces (M10 review A3-2).
//!
//! The unit tests in `src/apu` compare the fast integration paths with a
//! slow model of the same rules, and the ROM suites judge registers. So an
//! error both paths share (polarity, a swapped side, a wrong duty or
//! volume table, a period off by one) passes all of them. These tests are
//! the independent net: each one drives the APU the way a game does, with
//! a small program of register writes run through the public API, reads
//! the `StereoSample` stream, and compares it with what Pan Docs says the
//! hardware produces. No expectation here comes from the implementation;
//! the only core facts used are the public contract of `StereoSample`
//! (rate 32,768 Hz = one sample per 128 clocks; a low-pass filter over
//! the 512 clocks that end with the sample, with a gain of 1 at DC; 64
//! per unit of mixed level; DC kept; saturation at the ends of `i16`).
//!
//! Pan Docs sources: "Audio Registers" (FF10-FF26, FF30-FF3F) and "Audio
//! details" (DACs, mixer, frame sequencer, LFSR).
//!
//! Exactness: a sample depends on the 512 clocks (`FILTER` = 4 samples)
//! that end with it and on nothing else. Where the signal holds one level
//! that long, the sample is that level times 64, exactly: the checks of
//! levels below are equalities on such samples (`held`). The up to 4
//! samples whose 512 clocks contain an edge ring around it: they
//! overshoot the levels on both sides (by up to 7.6% of the step) and
//! each is rounded down, by less than 1. So `extremes` of a stream with
//! edges are not its levels, a sum over whole periods is exact but for
//! what the rounding drops (the gain is 1 at DC), and where a check
//! counts samples, its tolerance is those edge samples.

use gbc_core::{
    AUDIO_SAMPLE_CLOCKS, AUDIO_SAMPLE_RATE, CLOCK_HZ, Config, Emulator, Frame, Memory,
    StereoSample, WHITE, cart_ram_size,
};

/// One unit of analog level at master volume 7 (factor 8): 64 * 8.
const UNIT: i32 = 512;
/// A lone channel's analog extremes at master volume 7. Pan Docs, DACs:
/// digital 0 is the positive extreme and digital 15 the negative one
/// ("digital 0 maps to analog 1, not -1": the slope is negative). On the
/// `StereoSample` scale the extremes are +-15 units.
const TOP: i32 = 15 * UNIT;

/// Samples dropped before a steady-state measurement: the first waveform
/// period after a trigger differs (Pan Docs: a square starts at digital 0,
/// the wave channel replays its old sample buffer).
const SETTLE: usize = 1024;

/// The output filter's length in samples: a sample depends on the 512
/// clocks that end with it (`StereoSample`).
const FILTER: usize = 4;

type Prog = Vec<(u8, u8)>;

/// Runs a program of `LDH (reg),A` writes on a fresh CGB cartridge and
/// returns the `n` samples from the first whole sample after the last
/// write. The program starts by power-cycling the APU (NR52 = 00, then
/// 80), so every register starts at 0 whatever the boot state is; then
/// the CPU halts with interrupts off, so nothing else touches the APU.
/// Each write takes 5 M-cycles (20 clocks), whatever its values: programs
/// with the same number of writes have the same timing.
fn render(prog: &[(u8, u8)], n: usize) -> Vec<StereoSample> {
    // JP (16 clocks) and the writes, rounded up to whole samples, plus one
    // and the filter's memory: the samples returned depend on nothing
    // before the last write.
    let skip = write_end(prog.len()).div_ceil(AUDIO_SAMPLE_CLOCKS as usize) + FILTER;
    render_from(prog, skip, n)
}

/// The clock at which the last of a program's `writes` writes is done:
/// JP (16 clocks), the two power writes and the program's, 20 clocks each.
fn write_end(writes: usize) -> usize {
    16 + 20 * (writes + 2)
}

/// As `render`, but the `n` samples from sample `skip` of the stream on
/// (sample 0 starts at clock 0, the cartridge's entry point).
fn render_from(prog: &[(u8, u8)], skip: usize, n: usize) -> Vec<StereoSample> {
    let mut rom = vec![0u8; 0x8000];
    rom[0x143] = 0x80; // CGB
    rom[0x100..0x103].copy_from_slice(&[0xC3, 0x50, 0x01]); // JP 0x0150
    let mut pc = 0x150;
    let power = [(0x26, 0x00), (0x26, 0x80)];
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
    let mut out = Vec::with_capacity(skip + n + 4096);
    let mut buf = vec![StereoSample::default(); 4096];
    while out.len() < skip + n {
        let r = emu.run_cycles(4096 * u64::from(AUDIO_SAMPLE_CLOCKS), &mut frame, &mut buf);
        assert!(r.audio_samples > 0);
        out.extend_from_slice(&buf[..r.audio_samples]);
    }
    out.drain(..skip);
    out.truncate(n);
    out
}

fn left(s: &[StereoSample]) -> Vec<i32> {
    s.iter().map(|s| i32::from(s.left)).collect()
}

fn right(s: &[StereoSample]) -> Vec<i32> {
    s.iter().map(|s| i32::from(s.right)).collect()
}

/// Both sides, which must be identical (a channel panned to both sides at
/// equal master volumes).
fn mono(s: &[StereoSample]) -> Vec<i32> {
    assert!(s.iter().all(|s| s.left == s.right), "sides differ");
    left(s)
}

/// Master volume 7 on both sides, and the NR51 panning byte.
fn master(nr51: u8) -> Prog {
    vec![(0x24, 0x77), (0x25, nr51)]
}

/// A square channel (`ch` 1 or 2): duty code, NRx2 byte, frequency, and a
/// trigger with the length counter off.
fn square(ch: u8, duty: u8, nrx2: u8, f: u16) -> Prog {
    let base = if ch == 1 { 0x10 } else { 0x15 };
    vec![
        (base + 1, duty << 6),
        (base + 2, nrx2),
        (base + 3, f as u8),
        (base + 4, 0x80 | (f >> 8) as u8),
    ]
}

/// Wave RAM holding a rising ramp: samples 0, 1, .., 15, 0, 1, .., 15 in
/// playing order (Pan Docs: the high nibble of each byte plays first).
const RAMP: [u8; 16] = [
    0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
];

/// A rising ramp over the whole waveform is not possible with 4-bit
/// samples and 32 steps, so a single 32-step ramp uses each level twice.
const RAMP32: [u8; 16] = [
    0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF,
];

/// The wave channel: wave RAM, DAC on, NR32 volume code, frequency,
/// trigger.
fn wave(ram: &[u8; 16], vol_code: u8, f: u16) -> Prog {
    let mut p: Prog = (0..16).map(|i| (0x30 + i as u8, ram[i])).collect();
    p.extend([
        (0x1A, 0x80),
        (0x1C, vol_code << 5),
        (0x1D, f as u8),
        (0x1E, 0x80 | (f >> 8) as u8),
    ]);
    p
}

/// The noise channel: NR42, NR43, trigger.
fn noise(nr42: u8, nr43: u8) -> Prog {
    vec![(0x21, nr42), (0x22, nr43), (0x23, 0x80)]
}

fn cat(parts: &[Prog]) -> Prog {
    parts.concat()
}

/// The smallest shift `p >= 1` under which the whole stream repeats
/// exactly (`s[i] == s[i + p]` for every `i`), up to `max`.
fn min_period(s: &[i32], max: usize) -> Option<usize> {
    (1..=max).find(|&p| s[p..] == s[..s.len() - p])
}

/// Upward crossings of the midpoint between the extremes: one per cycle
/// of a two-level wave, however the filter blurs the edges.
fn cycles(s: &[i32]) -> usize {
    let (lo, hi) = (s.iter().min().unwrap(), s.iter().max().unwrap());
    let mid = (lo + hi) / 2;
    s.windows(2).filter(|w| w[0] <= mid && w[1] > mid).count()
}

fn extremes(s: &[i32]) -> (i32, i32) {
    (*s.iter().min().unwrap(), *s.iter().max().unwrap())
}

/// The levels the stream holds, in order: the value of every maximal run
/// of `FILTER` or more equal samples. A level held for 8 samples or more
/// always shows as such a run (at least 4 of them have their 512 clocks
/// inside it); the samples around an edge ring and do not repeat.
fn plateaus(s: &[i32]) -> Vec<i32> {
    s.chunk_by(|a, b| a == b)
        .filter(|run| run.len() >= FILTER)
        .map(|run| run[0])
        .collect()
}

/// The lowest and the highest level the stream holds (`plateaus`).
fn held(s: &[i32]) -> (i32, i32) {
    let p = plateaus(s);
    assert!(!p.is_empty(), "no level is held");
    extremes(&p)
}

#[test]
fn the_stream_is_32768_samples_per_second() {
    // The premise of every period below.
    assert_eq!(CLOCK_HZ, 4_194_304);
    assert_eq!(AUDIO_SAMPLE_RATE, 32_768);
    assert_eq!(AUDIO_SAMPLE_CLOCKS, 128);
}

/// Pan Docs NR13/NR14 (and NR23/NR24): the tone is `131072 / (2048 - f)`
/// Hz. At 32,768 samples per second one cycle is `(2048 - f) / 4` samples.
#[test]
fn square_tone_frequency_is_131072_over_2048_minus_f() {
    // (f, samples per cycle as a ratio): 128 Hz, 512 Hz, 1048.576 Hz (125
    // samples per 4 cycles), 2730.67 Hz.
    for ch in [1, 2] {
        for (f, num, den) in [(1024, 256, 1), (1792, 64, 1), (1923, 125, 4), (2000, 12, 1)] {
            assert_eq!((2048 - f) * den, 4 * num, "the table follows the formula");
            let pan = if ch == 1 { 0x11 } else { 0x22 };
            let prog = cat(&[master(pan), square(ch, 2, 0xF0, f as u16)]);
            let n = 16_384;
            let s = mono(&render(&prog, SETTLE + n))[SETTLE..].to_vec();
            // The stream repeats exactly every `num` samples and no sooner.
            assert_eq!(min_period(&s, 2 * num), Some(num), "CH{ch} f={f}");
            // And those `num` samples hold `den` cycles.
            let want = n * den / num;
            let got = cycles(&s);
            assert!(got.abs_diff(want) <= 1, "CH{ch} f={f}: {got} cycles");
        }
    }
}

/// Pan Docs NR11/NR21 bits 6-7: the waveform is high (the channel puts its
/// volume on the DAC) for 12.5%, 25%, 50%, 75% of the cycle. A high DAC
/// input is the negative analog extreme, so the fraction of a cycle spent
/// below the midpoint is the duty.
#[test]
fn square_duty_is_the_fraction_of_the_cycle_at_the_high_input() {
    // f = 1024: 256 samples per cycle, 32 per duty step.
    const CYCLE: usize = 256;
    for ch in [1, 2] {
        for (duty, eighths) in [(0, 1), (1, 2), (2, 4), (3, 6)] {
            let pan = if ch == 1 { 0x11 } else { 0x22 };
            let prog = cat(&[master(pan), square(ch, duty, 0xF0, 1024)]);
            let s = mono(&render(&prog, SETTLE + 16 * CYCLE))[SETTLE..].to_vec();
            assert_eq!(held(&s), (-TOP, TOP));
            // The filter passes through the midpoint once per edge, in
            // one sample; two edges per cycle, so the count is within 2
            // samples per cycle (0.8%, against 12.5% between neighbouring
            // duties).
            let low = s.iter().filter(|&&x| x < 0).count();
            let want = 16 * CYCLE * eighths / 8;
            assert!(low.abs_diff(want) <= 2 * 16, "CH{ch} duty {duty}: {low}");
            // The filter's gain is 1 at DC, so over whole cycles the mean
            // has no such error: `+15 - 30 * duty` units, less what the
            // rounding down drops, under 1 in each of the up to 4 samples
            // around each of the 32 edges.
            let sum: i32 = s.iter().sum();
            let want = (16 * CYCLE) as i32 * UNIT * (15 * 8 - 30 * eighths as i32) / 8;
            let dropped = want - sum;
            assert!(
                (0..(4 * 32)).contains(&dropped),
                "CH{ch} duty {duty}: {sum}"
            );
        }
    }
}

/// Pan Docs NR12/NR22 bits 4-7 and "DACs": the channel alternates between
/// digital 0 and its volume; digital `v` is `15 - 2 * v` units.
#[test]
fn square_amplitude_is_proportional_to_the_volume() {
    for ch in [1, 2] {
        for vol in [1, 4, 8, 11, 15] {
            let pan = if ch == 1 { 0x11 } else { 0x22 };
            let prog = cat(&[master(pan), square(ch, 2, vol << 4, 1792)]);
            let s = mono(&render(&prog, SETTLE + 512))[SETTLE..].to_vec();
            let want = (TOP - 2 * i32::from(vol) * UNIT, TOP);
            assert_eq!(held(&s), want, "CH{ch} volume {vol}");
        }
    }
}

/// The stretches over which the stream holds a level: the first sample
/// and the value of every maximal run of `FILTER` or more equal samples.
fn plateau_starts(s: &[i32]) -> Vec<(usize, i32)> {
    let mut at = 0;
    let mut out = Vec::new();
    for run in s.chunk_by(|a, b| a == b) {
        if run.len() >= FILTER {
            out.push((at, run[0]));
        }
        at += run.len();
    }
    out
}

/// Pan Docs NR12 bits 0-2 and 3: the envelope steps the volume by 1 every
/// `pace` ticks of 64 Hz (512 samples), down or up, and stops at 0 or 15.
///
/// The carrier is a square at f = 2047, 50% duty: its period is 32
/// clocks (Pan Docs: `4 * (2048 - f)` clocks per duty step), so every
/// 64 clocks hold two whole periods, half at digital 0 (+15 units) and
/// half at the volume `v` (`15 - 2v`): the output is the steady level
/// `15 - v` units between envelope steps, whatever the phase. A step is
/// then a step of one unit in the stream: the up to 4 samples whose 512
/// clocks contain it ring, and the next level is held from the sample
/// after them on. So the starts of the held levels are the envelope
/// steps, each delayed by the same 4 samples (to the one sample the
/// position of the step inside a sample can move it by).
#[test]
fn the_envelope_steps_once_per_pace_64ths_of_a_second() {
    const TICK: usize = AUDIO_SAMPLE_RATE as usize / 64;
    for (ch, nrx2, pace) in [(2, 0xF1, 1), (1, 0xF3, 3), (2, 0x0A, 2), (1, 0x79, 1)] {
        let (start, up) = (i32::from(nrx2 >> 4), nrx2 & 8 != 0);
        let steps = if up { 15 - start } else { start } as usize;
        let pan = if ch == 1 { 0x11 } else { 0x22 };
        let prog = cat(&[master(pan), square(ch, 2, nrx2, 2047)]);
        let s = mono(&render(&prog, (steps + 3) * pace * TICK));
        let levels = plateau_starts(&s);
        // The volumes, in order: one step at a time, and it stops at the
        // end (the last level lasts 3 paces, to the end of the stream).
        assert_eq!(levels.len(), steps + 1, "NRx2={nrx2:#04x}: {levels:?}");
        for (k, &(_, level)) in levels.iter().enumerate() {
            let step = if up { 1 } else { -1 };
            let v = start + step * k as i32;
            assert_eq!(level, TOP - v * UNIT, "NRx2={nrx2:#04x}, step {k}");
        }
        assert_eq!(levels[0].0, 0, "the first volume from the trigger on");
        // Where each volume starts: the first step is at most one pace
        // after the trigger (plus the filter's 4 samples and `render`'s
        // skipped ones, fewer than 4 more).
        let at: Vec<usize> = levels[1..].iter().map(|l| l.0).collect();
        assert!(at[0] <= pace * TICK + 2 * FILTER, "NRx2={nrx2:#04x}");
        // The rest are exactly one pace apart, to the sample: 32,768
        // clocks per tick is a whole number of samples.
        for w in at.windows(2) {
            let gap = w[1] - w[0];
            assert!(gap.abs_diff(pace * TICK) <= 1, "NRx2={nrx2:#04x}: {gap}");
        }
        // And they do not drift: from the first step to the last.
        let span = at[steps - 1] - at[0];
        let want = (steps - 1) * pace * TICK;
        assert!(span.abs_diff(want) <= 1, "NRx2={nrx2:#04x}: {span}");
    }
}

/// One program per channel that makes it play (CH1-CH4).
fn each_channel() -> [Prog; 4] {
    [
        square(1, 2, 0xF0, 1792),
        square(2, 1, 0xF0, 1923),
        wave(&RAMP, 1, 1536),
        noise(0xF0, 0x31),
    ]
}

/// Pan Docs NR51: bits 4-7 send CH1-CH4 to the left output, bits 0-3 to
/// the right. A side with no channel routed to it gets nothing: 0.
#[test]
fn nr51_routes_each_channel_to_its_side_only() {
    for (i, ch) in each_channel().iter().enumerate() {
        let s = render(&cat(&[master(0x10 << i), ch.clone()]), SETTLE + 512);
        let (l, r) = (left(&s[SETTLE..]), right(&s[SETTLE..]));
        assert_eq!(held(&l), (-TOP, TOP), "CH{} plays on the left", i + 1);
        assert_eq!(extremes(&r), (0, 0), "CH{} silent on the right", i + 1);
        let s = render(&cat(&[master(0x01 << i), ch.clone()]), SETTLE + 512);
        let (l, r) = (left(&s[SETTLE..]), right(&s[SETTLE..]));
        assert_eq!(extremes(&l), (0, 0), "CH{} silent on the left", i + 1);
        assert_eq!(held(&r), (-TOP, TOP), "CH{} plays on the right", i + 1);
    }
}

/// Pan Docs NR50: bits 4-6 are the left volume and bits 0-2 the right; a
/// value of `v` scales the side by `v + 1` (0 is not mute). Bits 3 and 7
/// (VIN) do not change the channels' level.
#[test]
fn nr50_scales_each_side_by_its_volume_plus_one() {
    for (l, r) in [(7, 0), (0, 7), (2, 5), (4, 4), (0, 0)] {
        for vin in [0x00, 0x88] {
            let nr50 = vin | l << 4 | r;
            let prog = cat(&[vec![(0x24, nr50), (0x25, 0x22)], square(2, 2, 0xF0, 1792)]);
            let s = render(&prog, SETTLE + 512);
            let (l, r) = (i32::from(l), i32::from(r));
            let want = |v: i32| (-15 * 64 * (v + 1), 15 * 64 * (v + 1));
            assert_eq!(held(&left(&s[SETTLE..])), want(l), "NR50={nr50:#04x}");
            assert_eq!(held(&right(&s[SETTLE..])), want(r), "NR50={nr50:#04x}");
        }
    }
}

/// Pan Docs NR33/NR34 and wave RAM: the 32 samples play high nibble
/// first, the whole waveform at `65536 / (2048 - f)` Hz, i.e. one wave
/// sample every `(2048 - f) / 64` output samples. A rising digital ramp is
/// a falling analog one (DAC slope).
#[test]
fn a_wave_ram_ramp_plays_as_a_falling_ramp_at_65536_over_2048_minus_f() {
    // f = 1536: 8 output samples per wave sample, 256 per waveform; the
    // ramp repeats twice in wave RAM, so the stream repeats every 128.
    let s = mono(&render(
        &cat(&[master(0x44), wave(&RAMP, 1, 1536)]),
        SETTLE + 4096,
    ));
    let s = &s[SETTLE..];
    assert_eq!(min_period(s, 512), Some(128));
    // Every level of the ramp is held (8 samples, at least 4 of them
    // clear of the steps), and it falls: each level is one digital step
    // (2 units) below the one before, down to digital 15, from where it
    // jumps back up to digital 0.
    let levels = plateaus(s);
    assert_eq!(extremes(&levels), (-TOP, TOP));
    for w in levels.windows(2) {
        let wraps = w == [-TOP, TOP];
        assert!(w[1] == w[0] - 2 * UNIT || wraps, "{} then {}", w[0], w[1]);
    }
    let ramps = s.len() / 128;
    assert!((16 * (ramps - 1)..=16 * ramps).contains(&levels.len()));
    // The frequency, with a 32-sample ramp so the waveform is one cycle:
    // f = 1792 is 128 samples, f = 1798 (262.144 Hz) is 125, f = 1984 is 32.
    for (f, period) in [(1792, 128), (1798, 125), (1984, 32)] {
        assert_eq!(
            (2048 - f) * 64,
            period * 128,
            "the table follows the formula"
        );
        let prog = cat(&[master(0x44), wave(&RAMP32, 1, f as u16)]);
        let n = 8192;
        let s = mono(&render(&prog, SETTLE + n))[SETTLE..].to_vec();
        assert_eq!(min_period(&s, 2 * period), Some(period), "f={f}");
        assert!(cycles(&s).abs_diff(n / period) <= 1, "f={f}");
    }
}

/// Pan Docs NR32 bits 5-6: 00 mutes, 01 is 100%, 10 is 50% (the sample
/// shifted right once), 11 is 25% (twice). A muted channel still drives
/// its DAC with digital 0.
#[test]
fn nr32_shifts_the_wave_samples() {
    for (code, top_digital) in [(0, 0), (1, 15), (2, 7), (3, 3)] {
        // f = 1536: 8 output samples per wave sample.
        let prog = cat(&[master(0x44), wave(&RAMP, code, 1536)]);
        let s = mono(&render(&prog, SETTLE + 512))[SETTLE..].to_vec();
        let want = (TOP - 2 * top_digital * UNIT, TOP);
        assert_eq!(held(&s), want, "NR32 code {code}");
        // The shifted ramp has `top + 1` levels, each of them held.
        let levels = plateaus(&s);
        for d in 0..=top_digital {
            assert!(levels.contains(&(TOP - 2 * d * UNIT)), "code {code}: {d}");
        }
    }
}

/// Pan Docs NR43 and "Noise channel (CH4)": the LFSR is clocked at
/// `262144 / (r * 2^s)` Hz, i.e. every `16 * r * 2^s` clocks; with the
/// short-mode bit (NR43 bit 3) it is a 7-bit register, else 15 bits. With
/// XNOR feedback from an all-zero start it runs the maximal sequence: all
/// states but all-ones, 127 or 32,767 steps.
#[test]
fn noise_repeats_every_127_or_32767_lfsr_steps() {
    // r = 1, s = 3: 128 clocks per step, one output sample.
    let s = mono(&render(&cat(&[master(0x88), noise(0xF0, 0x39)]), 4 * 127));
    assert_eq!(min_period(&s, 2 * 127), Some(127), "7-bit");
    // The amplitude. A step lasts one sample here, so the extremes of
    // the stream are the filter's ringing, not the channel's levels. But
    // the 127-step sequence has one run of 7 zeros (896 clocks at +15
    // units) and one of 6 ones (768 clocks at -15): each is longer than
    // the filter's 512 clocks, so 3 or 4 samples of a period are exactly
    // the top level and 2 or 3 exactly the bottom one. No other run is
    // that long (the next are 5 steps, 640 clocks: at most one such
    // sample each). And nothing exceeds the levels by more than the
    // filter can add: 1,336 / 1,024 of the amplitude, the sum of its
    // taps' magnitudes.
    let period = &s[127..254];
    let at = |level: i32| period.iter().filter(|&&x| x == level).count();
    assert!((3..=6).contains(&at(TOP)), "{} at the top", at(TOP));
    assert!((2..=5).contains(&at(-TOP)), "{} at the bottom", at(-TOP));
    let (lo, hi) = extremes(&s);
    assert!(lo < -TOP && hi > TOP, "rings past the levels: {lo}, {hi}");
    assert!(lo >= -TOP * 1336 / 1024 && hi <= TOP * 1336 / 1024);
    // r = 1, s = 4 and r = 2, s = 3: 256 clocks per step. Both levels are
    // held: the sequence has runs of 6 and 7 equal bits (12, 14 samples).
    for nr43 in [0x49, 0x3A] {
        let s = mono(&render(&cat(&[master(0x88), noise(0xF0, nr43)]), 4 * 254));
        assert_eq!(
            min_period(&s, 2 * 254),
            Some(254),
            "7-bit, NR43={nr43:#04x}"
        );
        assert_eq!(held(&s), (-TOP, TOP), "7-bit, NR43={nr43:#04x}");
    }
    // 15-bit: 32,767 = 7 * 31 * 151 samples and no divisor of it.
    let s = mono(&render(
        &cat(&[master(0x88), noise(0xF0, 0x31)]),
        2 * 32_767 + 64,
    ));
    assert!(s[32_767..] == s[..s.len() - 32_767], "15-bit");
    for p in [32_767 / 7, 32_767 / 31, 32_767 / 151] {
        assert!(s[p..] != s[..s.len() - p], "15-bit: repeats every {p}");
    }
    assert_eq!(held(&s), (-TOP, TOP));
}

/// Pan Docs: the channel's DAC input is the volume while LFSR bit 0 is
/// set, else 0. The 127 states of the 7-bit sequence are all but all-ones,
/// so bit 0 is set in 63 of them and clear in 64: over one period the
/// output averages `(64 * 15 + 63 * (15 - 30)) / 127` units, slightly
/// positive. (The inverted bit sense would give the same magnitude,
/// negative.) The filter's gain is 1 at DC, so the samples of a period
/// add up to that, less what rounding each of them down drops (under 1).
#[test]
fn noise_output_is_high_while_lfsr_bit_0_is_set() {
    let s = mono(&render(&cat(&[master(0x88), noise(0xF0, 0x39)]), 4 * 127));
    for period in s.as_chunks::<127>().0 {
        let dropped = 15 * UNIT - period.iter().sum::<i32>();
        assert!((0..127).contains(&dropped), "{dropped}");
    }
}

/// Pan Docs "DACs" and "Mixer", and the `StereoSample` contract ("DC is
/// kept"): a channel whose DAC is on but which is not playing feeds
/// digital 0, the positive extreme (+15 units), as a constant. A playing
/// channel swings down from there: a high input is the negative extreme.
/// A DAC that is off adds 0, and so does everything while NR52 is off.
#[test]
fn polarity_a_high_dac_input_is_negative_and_an_idle_dac_is_positive_dc() {
    // CH2's DAC on (NR22 bits 3-7 non-zero), never triggered.
    let s = mono(&render(&cat(&[master(0x22), vec![(0x17, 0xF0)]]), 256));
    assert_eq!(extremes(&s), (TOP, TOP), "idle DAC: constant +15 units");
    // Triggered at duty 12.5%: at the top 7/8 of the time, at the bottom
    // (digital 15) 1/8 of it.
    let prog = cat(&[master(0x22), square(2, 0, 0xF0, 1024)]);
    let s = mono(&render(&prog, SETTLE + 4096))[SETTLE..].to_vec();
    // 16 cycles: 32 edges, each in the 512 clocks of at most 4 samples.
    let at = |level: i32| s.iter().filter(|&&x| x == level).count();
    assert!(at(TOP) >= 4096 * 7 / 8 - 4 * 32, "{} at the top", at(TOP));
    assert!(
        (4096 / 8 - 4 * 32..=4096 / 8).contains(&at(-TOP)),
        "{}",
        at(-TOP)
    );
    assert!(at(TOP) + at(-TOP) >= 4096 - 4 * 32);
    // The wave channel's DAC (NR30 bit 7) likewise.
    let s = mono(&render(&cat(&[master(0x44), vec![(0x1A, 0x80)]]), 256));
    assert_eq!(extremes(&s), (TOP, TOP), "idle wave DAC");
    // All DACs off: nothing, though every channel is routed.
    let s = mono(&render(&master(0xFF), 256));
    assert_eq!(extremes(&s), (0, 0), "DACs off");
    // DAC turned off again (NR22 = 0): back to nothing.
    let prog = cat(&[master(0x22), square(2, 2, 0xF0, 1792), vec![(0x17, 0x00)]]);
    assert_eq!(
        extremes(&mono(&render(&prog, 256))),
        (0, 0),
        "DAC off again"
    );
    // APU off (NR52 = 0) under a playing channel: nothing.
    let prog = cat(&[master(0x22), square(2, 2, 0xF0, 1792), vec![(0x26, 0x00)]]);
    assert_eq!(extremes(&mono(&render(&prog, 256))), (0, 0), "APU off");
}

/// Pan Docs "Mixer": each side is the sum of the DAC outputs routed to
/// it. Four programs that each play one channel (the other three DACs
/// off) and one that plays all four have the same timing, so the last
/// stream is the sample-by-sample sum of the others: the output filter is
/// linear, but for the rounding down of each sample (the sum of `n`
/// rounded samples is up to `n - 1` below the rounded sum) and the
/// saturation at the ends of `i16`, which the ringing of four channels
/// at full volume can reach.
#[test]
fn the_mixer_adds_the_channels() {
    let all = each_channel();
    // The same writes with the DAC bit cleared and no trigger effect: a
    // trigger with the DAC off does not start the channel.
    let muted = |i: usize| -> Prog {
        let dac_reg = [0x12, 0x17, 0x1A, 0x21][i];
        let mut p = all[i].clone();
        for w in &mut p {
            if w.0 == dac_reg {
                w.1 = 0;
            }
        }
        p
    };
    let n = 4096;
    let run = |on: [bool; 4]| {
        let parts: Vec<Prog> = (0..4)
            .map(|i| if on[i] { all[i].clone() } else { muted(i) })
            .collect();
        mono(&render(&cat(&[master(0xFF), cat(&parts)]), n))
    };
    let solo: Vec<Vec<i32>> = (0..4)
        .map(|i| run(core::array::from_fn(|j| j == i)))
        .collect();
    for (i, s) in solo.iter().enumerate() {
        assert_eq!(held(&s[SETTLE..]), (-TOP, TOP), "CH{} alone", i + 1);
    }
    let pair = run([true, true, false, false]);
    let mix = run([true; 4]);
    let (min, max) = (i32::from(i16::MIN), i32::from(i16::MAX));
    let sat = |x: i32| x.clamp(min, max);
    let (mut exact, mut over, mut under) = (0, 0, 0);
    for k in 0..n {
        let sum = solo[0][k] + solo[1][k];
        assert!((sum..=sum + 1).contains(&pair[k]), "sample {k}");
        let sum: i32 = solo.iter().map(|s| s[k]).sum();
        assert!((sat(sum)..=sat(sum + 3)).contains(&mix[k]), "sample {k}");
        exact += usize::from(mix[k] == sum);
        // Where the channels' ringing adds up to more than `i16` holds
        // (the sum of the four rounded samples is the unrounded sample
        // or up to 3 less), the sample is the end of the range itself:
        // saturated, not wrapped.
        if sum > max {
            assert_eq!(mix[k], max, "sample {k}: {sum} saturates");
            over += 1;
        }
        if sum + 3 < min {
            assert_eq!(mix[k], min, "sample {k}: {sum} saturates");
            under += 1;
        }
    }
    // Most samples are clear of every channel's edges: exact sums.
    assert!(exact > n / 4, "{exact} exact");
    // And this mix does leave the range (both ends, exactly:
    // `a_full_scale_step_saturates_at_the_ends_of_i16`).
    assert!(over + under > 0, "{over} over, {under} under");
    // Four idle DACs at full master volume are the documented full scale.
    let idle = [(0x12, 0xF0), (0x17, 0xF0), (0x1A, 0x80), (0x21, 0xF0)];
    let s = mono(&render(&cat(&[master(0xFF), idle.to_vec()]), 64));
    assert_eq!(extremes(&s), (4 * TOP, 4 * TOP));
    assert_eq!(4 * TOP, 30_720);
}

/// The `StereoSample` contract: "the samples saturate at -32,768 and
/// 32,767". The filter overshoots a step by up to 7.6% of its height (78
/// / 1,024: the two oldest taps, both negative, still on the old level),
/// so a step from 0 to the full scale of 30,720 would come out at up to
/// 30,720 x 1,102 / 1,024 = 33,060, past `i16`. It is the end of the
/// range instead; a wrapping conversion would give -32,476.
///
/// The step is one write: NR51 from 00 (nothing routed: 0) to FF, with
/// the four channels steady at one extreme. How far the overshoot goes
/// depends on where in a sample the write lands (33,060 if the step is
/// on the boundary between two samples, 30,720 x 1,046 / 1,024 = 31,380
/// half a sample from there), so it is tried at 32 positions 20 clocks
/// apart: every fourth clock of a sample (writes land on M-cycles), the
/// boundary among them.
fn full_scale_steps(setup: &Prog, wait: usize) -> Vec<(i32, i32, i32)> {
    (0..32)
        .map(|pad| {
            let mut prog = vec![(0x24, 0x77), (0x25, 0x00)];
            prog.extend(setup);
            // Writes to HRAM (FF80): nothing happens, for `wait` samples.
            prog.extend(vec![(0x80, 0x00); wait * 128 / 20 + pad]);
            prog.push((0x25, 0xFF));
            // The sample in which the NR51 write lands.
            let step = write_end(prog.len()) / AUDIO_SAMPLE_CLOCKS as usize;
            let s = mono(&render_from(&prog, 0, step + 16));
            assert!(
                s[step - 12..step - 1].iter().all(|&x| x == 0),
                "nothing routed"
            );
            let level = s[step + 5];
            assert!(s[step + 5..].iter().all(|&x| x == level), "steady");
            let (lo, hi) = extremes(&s[step - 1..step + 5]);
            (level, lo, hi)
        })
        .collect()
}

#[test]
fn a_full_scale_step_saturates_at_the_ends_of_i16() {
    let full = 4 * TOP;
    // What the filter may add to a step of `full`, rounded up: 78 / 1,024
    // past it, and as much the other way just before it.
    let over = full * 78 / 1024 + 1;
    let before = over;
    assert!(full + over > i32::from(i16::MAX));
    // Up: four idle DACs, digital 0, the positive extreme.
    let idle: Prog = vec![(0x12, 0xF0), (0x17, 0xF0), (0x1A, 0x80), (0x21, 0xF0)];
    let up = full_scale_steps(&idle, 16);
    for &(level, lo, hi) in &up {
        assert_eq!(level, full);
        assert!(lo >= -before, "{lo}: no wrap");
        assert!(hi <= i32::from(i16::MAX));
    }
    let peak = up.iter().map(|u| u.2).max().unwrap();
    assert_eq!(peak, i32::from(i16::MAX), "{up:?}");
    assert!(up.iter().any(|u| u.2 < i32::from(i16::MAX)), "{up:?}");
    // Down: all four channels at digital 15, the negative extreme, as in
    // `after_a_dc_blocker_the_signal_reaches_twice_full_scale` (from 64
    // to 107 samples after their triggers).
    let mut ram = [0xFF; 16];
    ram[..7].fill(0x00);
    ram[7] = 0x0F;
    let low = cat(&[
        square(1, 3, 0xF0, 128),
        square(2, 3, 0xF0, 128),
        wave(&ram, 1, 1792),
        noise(0xF0, 0x51),
    ]);
    let down = full_scale_steps(&low, 85);
    for &(level, lo, hi) in &down {
        assert_eq!(level, -full);
        assert!(hi <= before, "{hi}: no wrap");
        assert!(lo >= i32::from(i16::MIN));
    }
    let peak = down.iter().map(|d| d.1).min().unwrap();
    assert_eq!(peak, i32::from(i16::MIN), "{down:?}");
    assert!(down.iter().any(|d| d.1 > i32::from(i16::MIN)), "{down:?}");
}

/// The headroom a front end needs (the `StereoSample` contract; M10
/// review A3-5). The stream keeps DC, so it can sit at +full scale (four
/// idle DACs) and then drop to -full scale (four channels at digital 15):
/// a swing of 61,440. A DC-blocking high-pass has by then settled on the
/// +30,720, so its output goes towards -61,440: twice the core's full
/// scale and outside `i16`. That is also the bound: the filter's output
/// is the input less a weighted average of earlier inputs, so it cannot
/// exceed the input's whole range. Here with the filter Pan Docs gives
/// for the CGB ("Mixer": the capacitor keeps 0.998943 of its charge per
/// clock), which is fast (about 7 samples): each sample the drop takes
/// costs it some of the swing. A slower blocker, or a drop within one
/// sample, comes as close to -61,440 as one likes.
#[test]
fn after_a_dc_blocker_the_signal_reaches_twice_full_scale() {
    // All four channels output digital 0 for 60 samples (7,680 clocks)
    // after their triggers, then digital 15 for 50 or more: squares at
    // f = 128 (60 samples per duty step; digital 0 until the first step,
    // then duty 75% is high for 6 steps); the wave at 4 samples per step,
    // 15 samples of 0 then 17 of F; the 15-bit LFSR at 4 samples per
    // step, whose first 15 steps from the all-zero start output 0 and the
    // next 14 output 1. The four triggers are the last four writes (80
    // clocks), so the drop takes a few samples at most (the LFSR's first
    // step need not come a whole period after the trigger).
    let mut ram = [0xFF; 16];
    ram[..7].fill(0x00);
    ram[7] = 0x0F;
    let parts = [
        square(1, 3, 0xF0, 128),
        square(2, 3, 0xF0, 128),
        wave(&ram, 1, 1792),
        noise(0xF0, 0x51),
    ];
    let mut prog = master(0xFF);
    for part in &parts {
        prog.extend(&part[..part.len() - 1]);
    }
    prog.extend(parts.iter().map(|part| part[part.len() - 1]));
    let s = mono(&render(&prog, 110));
    let full = 4 * TOP;
    assert_eq!(held(&s), (-full, full), "both ends of the scale");
    // `render` drops 3 more samples than a filter without memory needs,
    // so everything is 3 samples earlier in `s`; the filter makes the
    // drop 3 samples longer.
    assert!(s[..53].iter().all(|&x| x == full), "+full scale first");
    assert!(s[64..107].iter().all(|&x| x == -full), "then -full scale");
    // The core's filter rings around the drop, past the steady full
    // scale on both sides.
    let (lo, hi) = extremes(&s);
    assert!(lo < -full && hi > full, "{lo}, {hi}");
    let charge = 0.998943_f64.powi(AUDIO_SAMPLE_CLOCKS as i32);
    let mut cap = 0.0;
    let low = s
        .iter()
        .map(|&x| {
            let out = f64::from(x) - cap;
            cap = f64::from(x) - out * charge;
            out
        })
        .fold(0.0, f64::min);
    assert!(low < -50_000.0, "{low}");
    assert!(low >= f64::from(lo - hi), "{low}");
    assert!(low < f64::from(i16::MIN), "does not fit in i16");
}
