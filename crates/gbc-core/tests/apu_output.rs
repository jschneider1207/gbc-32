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
//! (rate 32,768 Hz = one sample per 128 clocks, box-filtered; 64 per unit
//! of mixed level; DC kept).
//!
//! Pan Docs sources: "Audio Registers" (FF10-FF26, FF30-FF3F) and "Audio
//! details" (DACs, mixer, frame sequencer, LFSR).
//!
//! Exactness: a sample is the exact average over its window times 64. With
//! NR50 = 0x77 the per-side factor is 8, so every window sum is even and
//! the samples are exact integers: most checks below are equalities. The
//! box filter only blurs the one or two samples that contain an edge;
//! where a check counts samples, its tolerance is those edge samples.

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

type Prog = Vec<(u8, u8)>;

/// Runs a program of `LDH (reg),A` writes on a fresh CGB cartridge and
/// returns the `n` samples from the first whole sample after the last
/// write. The program starts by power-cycling the APU (NR52 = 00, then
/// 80), so every register starts at 0 whatever the boot state is; then
/// the CPU halts with interrupts off, so nothing else touches the APU.
/// Each write takes 5 M-cycles (20 clocks), whatever its values: programs
/// with the same number of writes have the same timing.
fn render(prog: &[(u8, u8)], n: usize) -> Vec<StereoSample> {
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
    // JP (16 clocks) and the writes, rounded up to whole samples, plus one.
    let skip = (16 + 20 * (prog.len() + 2)).div_ceil(AUDIO_SAMPLE_CLOCKS as usize) + 1;
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
/// of a two-level wave, however the box filter blurs the edges.
fn cycles(s: &[i32]) -> usize {
    let (lo, hi) = (s.iter().min().unwrap(), s.iter().max().unwrap());
    let mid = (lo + hi) / 2;
    s.windows(2).filter(|w| w[0] <= mid && w[1] > mid).count()
}

fn extremes(s: &[i32]) -> (i32, i32) {
    (*s.iter().min().unwrap(), *s.iter().max().unwrap())
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
            assert_eq!(extremes(&s), (-TOP, TOP));
            // An edge inside a sample window blurs that one sample; two
            // edges per cycle, so the count is within 2 samples per cycle
            // (0.8%, against 12.5% between neighbouring duties).
            let low = s.iter().filter(|&&x| x < 0).count();
            let want = 16 * CYCLE * eighths / 8;
            assert!(low.abs_diff(want) <= 2 * 16, "CH{ch} duty {duty}: {low}");
            // The box filter is an exact average, so over whole cycles the
            // mean has no such error: `+15 - 30 * duty` units.
            let sum: i32 = s.iter().sum();
            let want = (16 * CYCLE) as i32 * UNIT * (15 * 8 - 30 * eighths as i32) / 8;
            assert_eq!(sum, want, "CH{ch} duty {duty}");
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
            assert_eq!(extremes(&s), want, "CH{ch} volume {vol}");
        }
    }
}

/// The volume a square at f = 2016 (8 samples per cycle) shows in each
/// cycle: the low extreme is `15 - 2 * v` units.
fn volumes(s: &[i32]) -> Vec<i32> {
    s.as_chunks::<8>()
        .0
        .iter()
        .map(|c| {
            let (lo, hi) = extremes(c);
            assert!(hi == TOP, "digital 0 in every cycle");
            assert_eq!((TOP - lo) % (2 * UNIT), 0);
            (TOP - lo) / (2 * UNIT)
        })
        .collect()
}

/// Pan Docs NR12 bits 0-2 and 3: the envelope steps the volume by 1 every
/// `pace` ticks of 64 Hz (512 samples), down or up, and stops at 0 or 15.
#[test]
fn the_envelope_steps_once_per_pace_64ths_of_a_second() {
    const TICK: usize = AUDIO_SAMPLE_RATE as usize / 64;
    for (ch, nrx2, pace) in [(2, 0xF1, 1), (1, 0xF3, 3), (2, 0x0A, 2), (1, 0x79, 1)] {
        let (start, up) = (i32::from(nrx2 >> 4), nrx2 & 8 != 0);
        let steps = if up { 15 - start } else { start } as usize;
        let pan = if ch == 1 { 0x11 } else { 0x22 };
        let prog = cat(&[master(pan), square(ch, 2, nrx2, 2016)]);
        let s = mono(&render(&prog, (steps + 3) * pace * TICK));
        let v = volumes(&s);
        // Where the volume changes, in cycles (8 samples).
        let changes: Vec<usize> = (1..v.len()).filter(|&i| v[i] != v[i - 1]).collect();
        assert_eq!(changes.len(), steps, "NRx2={nrx2:#04x}: stops at the end");
        for (k, &i) in changes.iter().enumerate() {
            let step = if up { 1 } else { -1 };
            assert_eq!(v[i], start + step * (k as i32 + 1), "NRx2={nrx2:#04x}");
        }
        // The first step is at most one pace after the trigger; the rest
        // are exactly one pace apart, to the 8-sample cycle.
        assert!(changes[0] * 8 <= pace * TICK + 8, "NRx2={nrx2:#04x}");
        for w in changes.windows(2) {
            let gap = (w[1] - w[0]) * 8;
            assert!(gap.abs_diff(pace * TICK) <= 8, "NRx2={nrx2:#04x}: {gap}");
        }
    }
}

/// One program per channel that makes it play (CH1-CH4).
fn each_channel() -> [Prog; 4] {
    [
        square(1, 2, 0xF0, 1792),
        square(2, 1, 0xF0, 1923),
        wave(&RAMP, 1, 1792),
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
        assert_eq!(extremes(&l), (-TOP, TOP), "CH{} plays on the left", i + 1);
        assert_eq!(extremes(&r), (0, 0), "CH{} silent on the right", i + 1);
        let s = render(&cat(&[master(0x01 << i), ch.clone()]), SETTLE + 512);
        let (l, r) = (left(&s[SETTLE..]), right(&s[SETTLE..]));
        assert_eq!(extremes(&l), (0, 0), "CH{} silent on the left", i + 1);
        assert_eq!(extremes(&r), (-TOP, TOP), "CH{} plays on the right", i + 1);
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
            assert_eq!(extremes(&left(&s[SETTLE..])), want(l), "NR50={nr50:#04x}");
            assert_eq!(extremes(&right(&s[SETTLE..])), want(r), "NR50={nr50:#04x}");
        }
    }
}

/// Pan Docs NR33/NR34 and wave RAM: the 32 samples play high nibble
/// first, the whole waveform at `65536 / (2048 - f)` Hz, i.e. one wave
/// sample every `(2048 - f) / 64` output samples. A rising digital ramp is
/// a falling analog one (DAC slope).
#[test]
fn a_wave_ram_ramp_plays_as_a_falling_ramp_at_65536_over_2048_minus_f() {
    // f = 1792: 4 output samples per wave sample, 128 per waveform; the
    // ramp repeats twice in wave RAM, so the stream repeats every 64.
    let s = mono(&render(
        &cat(&[master(0x44), wave(&RAMP, 1, 1792)]),
        SETTLE + 4096,
    ));
    let s = &s[SETTLE..];
    assert_eq!(min_period(s, 256), Some(64));
    assert_eq!(extremes(s), (-TOP, TOP));
    // Every level of the ramp is held for a whole sample.
    for d in 0..16 {
        assert!(s.contains(&(TOP - 2 * d * UNIT)), "digital {d}");
    }
    // It falls: 15 steps down per 64 samples and one jump back up. An edge
    // inside a window splits a step over two samples.
    let up = s.windows(2).filter(|w| w[1] > w[0]).count();
    let down = s.windows(2).filter(|w| w[1] < w[0]).count();
    let ramps = s.len() / 64;
    assert!((ramps - 1..=2 * ramps).contains(&up), "{up} rises");
    assert!(
        (15 * (ramps - 1)..=30 * ramps).contains(&down),
        "{down} falls"
    );
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
        let prog = cat(&[master(0x44), wave(&RAMP, code, 1792)]);
        let s = mono(&render(&prog, SETTLE + 512))[SETTLE..].to_vec();
        let want = (TOP - 2 * top_digital * UNIT, TOP);
        assert_eq!(extremes(&s), want, "NR32 code {code}");
        // The shifted ramp has `top + 1` levels, each held a whole sample.
        for d in 0..=top_digital {
            assert!(s.contains(&(TOP - 2 * d * UNIT)), "code {code}: {d}");
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
    assert_eq!(extremes(&s), (-TOP, TOP));
    // r = 1, s = 4 and r = 2, s = 3: 256 clocks per step.
    for nr43 in [0x49, 0x3A] {
        let s = mono(&render(&cat(&[master(0x88), noise(0xF0, nr43)]), 4 * 254));
        assert_eq!(
            min_period(&s, 2 * 254),
            Some(254),
            "7-bit, NR43={nr43:#04x}"
        );
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
    assert_eq!(extremes(&s), (-TOP, TOP));
}

/// Pan Docs: the channel's DAC input is the volume while LFSR bit 0 is
/// set, else 0. The 127 states of the 7-bit sequence are all but all-ones,
/// so bit 0 is set in 63 of them and clear in 64: over one period the
/// output averages `(64 * 15 + 63 * (15 - 30)) / 127` units, slightly
/// positive. (The inverted bit sense would give the same magnitude,
/// negative.)
#[test]
fn noise_output_is_high_while_lfsr_bit_0_is_set() {
    let s = mono(&render(&cat(&[master(0x88), noise(0xF0, 0x39)]), 4 * 127));
    for period in s.as_chunks::<127>().0 {
        assert_eq!(period.iter().sum::<i32>(), 15 * UNIT);
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
    let at = |level: i32| s.iter().filter(|&&x| x == level).count();
    assert!(at(TOP) >= 4096 * 7 / 8 - 32, "{} at the top", at(TOP));
    assert!(
        (4096 / 8 - 32..=4096 / 8).contains(&at(-TOP)),
        "{}",
        at(-TOP)
    );
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
/// stream is the sample-by-sample sum of the others.
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
        assert_eq!(extremes(&s[SETTLE..]), (-TOP, TOP), "CH{} alone", i + 1);
    }
    let pair = run([true, true, false, false]);
    let mix = run([true; 4]);
    for k in 0..n {
        assert_eq!(pair[k], solo[0][k] + solo[1][k], "sample {k}");
        let sum: i32 = solo.iter().map(|s| s[k]).sum();
        assert_eq!(mix[k], sum, "sample {k}");
    }
    // Four idle DACs at full master volume are the documented full scale.
    let idle = [(0x12, 0xF0), (0x17, 0xF0), (0x1A, 0x80), (0x21, 0xF0)];
    let s = mono(&render(&cat(&[master(0xFF), idle.to_vec()]), 64));
    assert_eq!(extremes(&s), (4 * TOP, 4 * TOP));
    assert_eq!(4 * TOP, 30_720);
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
    assert_eq!(extremes(&s), (-full, full), "both ends of the scale");
    assert!(s[..56].iter().all(|&x| x == full), "+full scale first");
    assert!(s[64..].iter().all(|&x| x == -full), "then -full scale");
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
    assert!(low >= -2.0 * f64::from(full), "{low}");
    assert!(low < f64::from(i16::MIN), "does not fit in i16");
}
