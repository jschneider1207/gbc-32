//! `play --audio` (ADR 0041, "Amendment: live playback"): the ring, the
//! feed's gap and `held` handling, the resampler, and the drift control
//! against a simulated board and sound card over 10 simulated minutes.

use gbc_link::audio::{encode_u12, sample};
use gbc_pc::audio::to_i16;
use gbc_pc::live::{
    Controller, Feed, IN_RATE, Live, MAX_GAP_FILL, Playback, Resampler, Ring, State, Tuning,
};

/// Pairs of 12-bit codes as the frames the PC decodes from a U12 `Audio`.
fn data(frames: &[(u16, u16)]) -> Vec<[i16; 2]> {
    let bytes: Vec<u8> = frames
        .iter()
        .flat_map(|&(l, r)| encode_u12(sample(l), sample(r)))
        .collect();
    let mut f = Vec::new();
    gbc_link::audio::decode(gbc_link::audio::U12, &bytes, |l, r| f.push([l, r])).unwrap();
    f
}

fn drain(r: &mut Ring) -> Vec<[i16; 2]> {
    std::iter::from_fn(|| r.pop()).collect()
}

#[test]
fn ring_is_fifo_across_the_wrap_and_refuses_when_full() {
    let mut r = Ring::new(4);
    for i in 0..3 {
        assert!(r.push([i, -i]));
    }
    assert_eq!(r.pop(), Some([0, 0]));
    assert_eq!(r.pop(), Some([1, -1]));
    for i in 3..6 {
        assert!(r.push([i, -i]));
    }
    assert!(!r.push([9, 9]));
    assert_eq!(r.len(), 4);
    r.drop_oldest(1);
    assert_eq!(drain(&mut r), vec![[3, -3], [4, -4], [5, -5]]);
    assert!(r.is_empty());
    assert_eq!(r.pop(), None);
    r.drop_oldest(10);
    assert!(r.is_empty());
}

#[test]
fn feed_inserts_held_and_gap_frames_and_counts_them() {
    let mut ring = Ring::new(10_000);
    let mut f = Feed::default();
    let a = [to_i16(100), to_i16(200)];
    let b = [to_i16(300), to_i16(400)];
    let c = [to_i16(500), to_i16(600)];
    // Two frames, then the DAC held the last one 3 times.
    f.on_audio(&mut ring, 10, 3, &data(&[(1, 2), (100, 200)]));
    // 12 follows: no gap.
    f.on_audio(&mut ring, 12, 0, &data(&[(300, 400)]));
    // 13 and 14 missing: the last frame twice.
    f.on_audio(&mut ring, 15, 0, &data(&[(500, 600)]));
    let got = drain(&mut ring);
    let mut want = vec![[to_i16(1), to_i16(2)], a, a, a, a, b, b, b, c];
    assert_eq!(got, want);
    let s = f.stats;
    assert_eq!(
        (s.messages, s.frames, s.gaps, s.missing, s.filled, s.held),
        (3, 4, 1, 2, 2, 3)
    );
    // A backward jump (board reset): counted, no fill, the count restarts.
    f.on_audio(&mut ring, 0, 0, &data(&[(7, 8)]));
    f.on_audio(&mut ring, 1, 0, &data(&[(9, 10)]));
    want = vec![[to_i16(7), to_i16(8)], [to_i16(9), to_i16(10)]];
    assert_eq!(drain(&mut ring), want);
    assert_eq!((f.stats.gaps, f.stats.missing), (2, 2));
    // A huge gap is filled only up to MAX_GAP_FILL.
    f.on_audio(&mut ring, 1_000_000, 0, &data(&[(1, 1)]));
    assert_eq!(ring.len(), MAX_GAP_FILL as usize + 1);
    assert_eq!(f.stats.filled, 2 + u64::from(MAX_GAP_FILL));
}

#[test]
fn feed_counts_overflow_when_the_ring_is_full() {
    let mut ring = Ring::new(3);
    let mut f = Feed::default();
    f.on_audio(&mut ring, 0, 2, &data(&[(1, 1), (2, 2)]));
    assert_eq!(ring.len(), 3);
    assert_eq!(f.stats.overflow, 1);
}

fn pull(res: &mut Resampler, step: f64, input: &[[i16; 2]], n: usize) -> Vec<[i16; 2]> {
    let mut it = input.iter().copied();
    let mut src = || it.next();
    (0..n).map_while(|_| res.next(step, &mut src)).collect()
}

#[test]
fn resampler_at_unity_is_a_passthrough() {
    let input: Vec<[i16; 2]> = (0..1000)
        .map(|i| [(i * 37 % 65536 - 32768) as i16, (i * 11) as i16])
        .collect();
    let out = pull(&mut Resampler::new(), 1.0, &input, 2000);
    // Three outputs of the initial (silent) history, then the input.
    assert_eq!(out.len(), input.len() + 1);
    assert_eq!(&out[..3], &[[0, 0]; 3]);
    assert_eq!(&out[3..], &input[..input.len() - 2]);
}

/// The amplitude of `f` Hz in `x` (rate `fs`) by correlation, and the
/// frequency where it peaks among `cands`.
fn tone(x: &[f64], fs: f64, f: f64) -> f64 {
    let (mut s, mut c) = (0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let w = 2.0 * std::f64::consts::PI * f * i as f64 / fs;
        s += v * w.sin();
        c += v * w.cos();
    }
    2.0 * (s * s + c * c).sqrt() / x.len() as f64
}

#[test]
fn resampler_keeps_a_sine_at_its_frequency() {
    for (fs_out, f_in) in [(48_000.0, 1000.0), (44_100.0, 2500.0), (32_768.0, 440.0)] {
        let fs_in = IN_RATE;
        let amp = 20_000.0;
        let input: Vec<[i16; 2]> = (0..40_000)
            .map(|i| {
                let v = amp * (2.0 * std::f64::consts::PI * f_in * i as f64 / fs_in).sin();
                [v.round() as i16, (-v).round() as i16]
            })
            .collect();
        let step = fs_in / fs_out;
        let out = pull(&mut Resampler::new(), step, &input, 50_000);
        // Skip the start-up; compare with the ideal sine on the output grid
        // (the resampler is 3 input frames late, as at unity).
        let skip = 100;
        let left: Vec<f64> = out[skip..].iter().map(|f| f64::from(f[0])).collect();
        let a = tone(&left, fs_out, f_in);
        assert!((a - amp).abs() < amp * 0.01, "{fs_out}: amplitude {a}");
        // 3% off in frequency loses the correlation.
        let off = tone(&left, fs_out, f_in * 1.03);
        assert!(off < amp * 0.05, "{fs_out}: off-frequency {off}");
        // Sample by sample against the ideal.
        let mut worst: f64 = 0.0;
        for (k, f) in out.iter().enumerate().skip(skip) {
            let t_in = k as f64 * step - 3.0;
            let ideal = amp * (2.0 * std::f64::consts::PI * f_in * t_in / fs_in).sin();
            worst = worst.max((f64::from(f[0]) - ideal).abs());
            assert_eq!(f[1], -f[0], "channels kept apart");
        }
        assert!(worst < amp * 0.01, "{fs_out}: worst error {worst}");
    }
}

#[test]
fn controller_pushes_toward_the_target_and_clamps() {
    let t = Tuning::default();
    let mut c = Controller::new(1000.0, &t);
    assert_eq!(c.update(1000, 0.01), 0.0);
    for _ in 0..1000 {
        c.update(1200, 0.01);
    }
    // 20% over: 0.05 * 0.2 = +1% (the clamp), the I term clamped too.
    assert!((c.correction() - 0.01).abs() < 1e-6, "{}", c.correction());
    // 5% under: P alone is -0.25%; the I term unwinds from +1% and then
    // keeps pushing down while the error stays.
    let mut prev = f64::MAX;
    for i in 0..10_000 {
        let c = c.update(950, 0.01);
        if i > 600 {
            assert!(c <= prev, "{i}: {c}");
        }
        if i == 6000 {
            assert!(c < -0.0025, "{i}: {c}");
        }
        prev = c;
    }
    assert!((prev + 0.01).abs() < 1e-9, "{prev}");
}

#[test]
fn underrun_holds_the_last_frame_then_resumes_at_the_target() {
    let t = Tuning::default();
    let mut ring = Ring::new(20_000);
    let mut p = Playback::new(32_768, &t);
    let target = p.target();
    let start = p.prefill();
    assert!(start > target);
    let mut buf = vec![0i16; 2 * 328];
    assert!(!p.ready(&ring));
    for i in 0..target {
        ring.push([i as i16, 1]);
    }
    assert!(!p.ready(&ring));
    for i in target..start + 500 {
        ring.push([i as i16, 1]);
    }
    assert!(p.ready(&ring));
    p.fill(&mut ring, &mut buf);
    // The start trimmed the ring to the target.
    assert_eq!(p.stats().start_trimmed, 500);
    assert_eq!(p.stats().state, State::Playing);
    while p.stats().state == State::Playing {
        p.fill(&mut ring, &mut buf);
    }
    let s = p.stats();
    assert_eq!(s.underruns, 1);
    assert!(ring.is_empty());
    let last = [buf[buf.len() - 2], buf[buf.len() - 1]];
    assert_eq!(last[1], 1);
    // Rebuffering: the last frame held, no new underrun counted.
    ring.push([5, 5]);
    p.fill(&mut ring, &mut buf);
    assert!(buf.chunks(2).all(|f| f == last));
    assert_eq!(p.stats().state, State::Rebuffer);
    assert_eq!(p.stats().underruns, 1);
    for _ in 0..target {
        ring.push([7, 7]);
    }
    p.fill(&mut ring, &mut buf);
    assert_eq!(p.stats().state, State::Playing);
    assert_eq!(p.stats().underruns, 1);
    assert_eq!(&buf[buf.len() - 2..], &[7, 7]);
}

/// xorshift64*: a deterministic jitter source.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

struct SimResult {
    underruns: u32,
    skips: u32,
    fill_min: usize,
    ema_min: f64,
    ema_max: f64,
    target: usize,
}

/// The board: 256-frame halves at `p_rate` (real time), sent at its
/// 59.73 Hz service hook, arriving after 0.5..8 ms of USB jitter with a
/// 25 ms stall every ~2 s (33 ms worst: about the board's own slack), the
/// first 100 ms as one late burst. The PC: 4
/// device buffers of 10 ms at `out_rate * (1 + dev_err)`, refilled as each
/// finishes. `secs` of simulated time.
fn simulate(p_rate: f64, out_rate: u32, dev_err: f64, secs: f64, tuning: &Tuning) -> SimResult {
    let mut live = Live::new(out_rate, tuning);
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ out_rate as u64 ^ (dev_err * 1e6) as u64);
    let frame_data = data(&[(2048, 2048); 256]);
    let half = 256.0 / p_rate;
    let hook = 1.0 / 59.73;
    // Arrival times of every half, in order.
    let mut arrivals = Vec::new();
    let mut t_arr: f64 = 0.0;
    let burst = (0.1 / half) as usize;
    let n_halves = ((secs + 1.0) / half) as usize;
    for k in 0..n_halves {
        // The start-up backlog: the first 100 ms arrive together, late.
        let t = if k < burst {
            0.1
        } else {
            let done = (k + 1) as f64 * half;
            let sent = (done / hook).ceil() * hook;
            let mut j = 0.0005 + 0.0075 * rng.next();
            if rng.next() < hook / 2.0 {
                j += 0.025;
            }
            sent + j
        };
        t_arr = t_arr.max(t);
        arrivals.push(t_arr);
    }
    let dev_rate = f64::from(out_rate) * (1.0 + dev_err);
    let frames = (out_rate / 100) as usize;
    let buf_t = frames as f64 / dev_rate;
    let mut buf = vec![0i16; frames * 2];
    let mut next_arr = 0;
    let mut first = 0u32;
    let mut push_until = |live: &mut Live, t: f64| {
        while next_arr < arrivals.len() && arrivals[next_arr] <= t {
            live.on_audio(first, 0, &frame_data);
            first = first.wrapping_add(256);
            next_arr += 1;
        }
    };
    // Pre-fill, polled every 2 ms as the playback thread does.
    let mut t = 0.0;
    loop {
        push_until(&mut live, t);
        if live.ready() {
            break;
        }
        t += 0.002;
    }
    let t0 = t;
    for _ in 0..4 {
        live.fill(&mut buf);
    }
    let mut r = SimResult {
        underruns: 0,
        skips: 0,
        fill_min: usize::MAX,
        ema_min: f64::MAX,
        ema_max: 0.0,
        target: live.play.target(),
    };
    let mut k = 0u64;
    while t < t0 + secs {
        k += 1;
        // Buffer k finished; the thread wakes up to 1 ms later.
        t = t0 + k as f64 * buf_t + 0.001 * rng.next();
        push_until(&mut live, t);
        live.fill(&mut buf);
        let s = live.play.stats();
        if t > t0 + 10.0 {
            r.fill_min = r.fill_min.min(s.fill);
            r.ema_min = r.ema_min.min(s.fill_ema);
            r.ema_max = r.ema_max.max(s.fill_ema);
        }
    }
    let s = live.play.stats();
    r.underruns = s.underruns;
    r.skips = s.skips;
    r
}

#[test]
fn drift_control_holds_the_fill_for_ten_minutes() {
    let t = Tuning::default();
    // The board at its rate and 0.33% fast (its clock vs real time), the
    // sound card at 48 kHz or the board's rate, 0.5% slow or fast.
    for (p_rate, out_rate, dev_err) in [
        (IN_RATE, 48_000, 0.005),
        (IN_RATE * 1.0033, 48_000, -0.005),
        (IN_RATE, 32_768, -0.005),
        (IN_RATE * 1.0033, 32_768, 0.005),
    ] {
        {
            {
                let r = simulate(p_rate, out_rate, dev_err, 600.0, &t);
                let tgt = r.target as f64;
                let what = format!("producer {p_rate:.1} Hz, device {out_rate} Hz {dev_err:+}");
                assert_eq!(r.underruns, 0, "{what}: underruns");
                assert_eq!(r.skips, 0, "{what}: skips");
                assert!(r.fill_min > 0, "{what}: ring ran dry");
                assert!(
                    r.ema_min > 0.85 * tgt && r.ema_max < 1.15 * tgt,
                    "{what}: filtered fill {:.0}..{:.0} of {tgt}",
                    r.ema_min,
                    r.ema_max
                );
            }
        }
    }
}
