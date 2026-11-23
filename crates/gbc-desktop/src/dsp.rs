//! Pure audio processing between the core's 32,768 Hz stream and the
//! device (ADR 0024): a DC blocker, a cubic resampler, and the dynamic
//! rate control that keeps the output buffer at its target fill. No I/O,
//! so all of it is unit-tested without a device.

/// One stereo frame, as `[left, right]`.
pub type Stereo = [f32; 2];

/// High-pass cutoff of the DC blocker. Real hardware AC-couples its
/// output through a capacitor, so a powered but silent DAC (a constant in
/// the core's stream, ADR 0021 section 9) is inaudible; Pan Docs "Audio
/// details" describes the capacitor and SameBoy's `GB_HIGHPASS_ACCURATE`
/// mode models it with a similar one-pole filter. 20 Hz keeps the audible
/// band intact.
pub const DC_CUTOFF_HZ: f32 = 20.0;

/// One-pole high-pass `y[n] = x[n] - x[n-1] + r * y[n-1]`, per channel.
pub struct DcBlocker {
    r: f32,
    prev_in: Stereo,
    prev_out: Stereo,
}

impl DcBlocker {
    pub fn new(sample_rate: f32, cutoff_hz: f32) -> Self {
        DcBlocker {
            r: 1.0 - 2.0 * core::f32::consts::PI * cutoff_hz / sample_rate,
            prev_in: [0.0; 2],
            prev_out: [0.0; 2],
        }
    }

    pub fn process(&mut self, x: Stereo) -> Stereo {
        let mut y = [0.0; 2];
        for c in 0..2 {
            let v = x[c] - self.prev_in[c] + self.r * self.prev_out[c];
            // Flush the decay tail before it turns denormal.
            y[c] = if v.abs() < 1e-12 { 0.0 } else { v };
        }
        self.prev_in = x;
        self.prev_out = y;
        y
    }
}

/// Streaming cubic (Catmull-Rom Hermite) resampler. Each input sample is
/// pushed on its own, and the fractional phase carries over between calls,
/// so the output does not depend on how the input is chunked.
pub struct Resampler {
    /// Input samples advanced per output sample (input rate / output rate).
    step: f64,
    /// The last four inputs, oldest first; output lies between `h[1]` and
    /// `h[2]`.
    h: [Stereo; 4],
    /// Output position past `h[1]`, in input samples.
    phase: f64,
}

impl Resampler {
    pub fn new(in_rate: f64, out_rate: f64) -> Self {
        Resampler {
            step: in_rate / out_rate,
            h: [[0.0; 2]; 4],
            phase: 0.0,
        }
    }

    /// Sets input samples per output sample; DRC nudges it.
    pub fn set_step(&mut self, step: f64) {
        self.step = step;
    }

    /// Feeds one input sample and appends the outputs it completes.
    pub fn push(&mut self, x: Stereo, out: &mut Vec<Stereo>) {
        self.h = [self.h[1], self.h[2], self.h[3], x];
        while self.phase < 1.0 {
            out.push(self.interpolate(self.phase as f32));
            self.phase += self.step;
        }
        self.phase -= 1.0;
    }

    fn interpolate(&self, t: f32) -> Stereo {
        let mut y = [0.0; 2];
        for (c, y) in y.iter_mut().enumerate() {
            let [x0, x1, x2, x3] = [self.h[0][c], self.h[1][c], self.h[2][c], self.h[3][c]];
            // Catmull-Rom in difference form: a constant input gives
            // exactly x1, since every bracketed term is then 0.
            *y = x1
                + 0.5
                    * t
                    * (x2 - x0
                        + t * (2.0 * x0 - 5.0 * x1 + 4.0 * x2 - x3
                            + t * (3.0 * (x1 - x2) + x3 - x0)));
        }
        y
    }
}

/// Largest change DRC makes to the output rate: ±0.5%, a pitch shift of
/// under 9 cents, far below what is audible on game music.
pub const DRC_MAX: f64 = 0.005;

/// Dynamic rate control (Near/byuu): the factor to multiply the nominal
/// output rate by, given the buffer fill and its target. Above target it
/// produces fewer samples (< 1), below it more (> 1), proportionally, and
/// never more than [`DRC_MAX`] away from 1.
pub fn drc_factor(fill: f64, target: f64) -> f64 {
    let err = ((fill - target) / target.max(1.0)).clamp(-1.0, 1.0);
    1.0 - DRC_MAX * err
}

#[cfg(test)]
mod tests {
    use super::*;

    const IN: f64 = 32_768.0;

    fn sine(freq: f64, rate: f64, n: usize, amp: f32) -> impl Iterator<Item = f32> {
        (0..n)
            .map(move |i| amp * (2.0 * std::f64::consts::PI * freq * i as f64 / rate).sin() as f32)
    }

    #[test]
    fn dc_blocker_removes_a_constant() {
        let mut f = DcBlocker::new(IN as f32, DC_CUTOFF_HZ);
        let mut y = [0.0; 2];
        for _ in 0..IN as usize {
            y = f.process([20_000.0, -7_000.0]);
        }
        assert!(y[0].abs() < 1.0 && y[1].abs() < 1.0, "{y:?}");
    }

    #[test]
    fn dc_blocker_passes_1khz_at_unity_gain() {
        let mut f = DcBlocker::new(IN as f32, DC_CUTOFF_HZ);
        let mut peak = 0f32;
        for (i, x) in sine(1000.0, IN, IN as usize, 10_000.0).enumerate() {
            // An offset on top, as a silent DAC adds; it must not leak.
            let y = f.process([x + 5_000.0, x + 5_000.0]);
            if i > IN as usize / 2 {
                peak = peak.max(y[0].abs());
            }
        }
        assert!(
            (peak / 10_000.0 - 1.0).abs() < 0.01,
            "gain {}",
            peak / 10_000.0
        );
    }

    fn run_chunked(chunk: usize, n: usize, out_rate: f64) -> Vec<Stereo> {
        let mut r = Resampler::new(IN, out_rate);
        let mut out = Vec::new();
        let input: Vec<f32> = (0..n).map(|i| (i % 97) as f32).collect();
        for c in input.chunks(chunk) {
            for &x in c {
                r.push([x, -x], &mut out);
            }
        }
        out
    }

    #[test]
    fn resampler_output_count_is_the_rate_ratio_whatever_the_chunking() {
        let n = 546 * 300;
        let ideal = n as f64 * 48_000.0 / IN;
        let whole = run_chunked(n, n, 48_000.0);
        assert!(
            (whole.len() as f64 - ideal).abs() <= 1.0,
            "{} vs {ideal}",
            whole.len()
        );
        for chunk in [1, 7, 546, 550] {
            assert_eq!(run_chunked(chunk, n, 48_000.0), whole, "chunk {chunk}");
        }
        let at44 = run_chunked(13, n, 44_100.0).len() as f64;
        assert!((at44 - n as f64 * 44_100.0 / IN).abs() <= 1.0, "{at44}");
    }

    #[test]
    fn resampler_reproduces_a_constant_exactly() {
        let mut r = Resampler::new(IN, 48_000.0);
        let mut out = Vec::new();
        for _ in 0..1000 {
            r.push([1234.5, -30_720.0], &mut out);
        }
        // The first outputs interpolate from the zeroed history.
        assert!(out[8..].iter().all(|&s| s == [1234.5, -30_720.0]));
    }

    #[test]
    fn resampler_follows_a_low_sine_closely() {
        let out_rate = 48_000.0;
        let mut r = Resampler::new(IN, out_rate);
        let mut out = Vec::new();
        for x in sine(440.0, IN, IN as usize, 10_000.0) {
            r.push([x, x], &mut out);
        }
        // Output k sits at input position k * step - 2: it lies between
        // h[1] and h[2], two behind the newest input.
        let step = IN / out_rate;
        let mut worst = 0f64;
        for (k, s) in out.iter().enumerate().skip(16) {
            let t = (k as f64 * step - 2.0) / IN;
            let ideal = 10_000.0 * (2.0 * std::f64::consts::PI * 440.0 * t).sin();
            worst = worst.max((f64::from(s[0]) - ideal).abs());
        }
        assert!(worst < 5.0, "worst error {worst} of 10,000");
    }

    #[test]
    fn drc_moves_the_right_way_and_clamps() {
        assert_eq!(drc_factor(100.0, 100.0), 1.0);
        assert!(drc_factor(110.0, 100.0) < 1.0, "over target: slow output");
        assert!(drc_factor(90.0, 100.0) > 1.0, "under target: speed up");
        assert!((drc_factor(110.0, 100.0) - (1.0 - DRC_MAX * 0.1)).abs() < 1e-12);
        assert_eq!(drc_factor(10_000.0, 100.0), 1.0 - DRC_MAX);
        assert_eq!(drc_factor(0.0, 100.0), 1.0 + DRC_MAX);
    }
}
