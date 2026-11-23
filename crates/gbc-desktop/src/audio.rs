//! Audio output through cpal (ADR 0024). The main thread filters and
//! resamples each frame's samples ([`crate::dsp`]) into a lock-free ring
//! of device-rate frames; the device callback only pops from it. The
//! ring's fill drives frame pacing ([`crate::pacing::Pacing`]).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{ErrorKind, FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use gbc_core::{AUDIO_SAMPLE_RATE, CLOCK_HZ, StereoSample};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};

use crate::dsp::{DC_CUTOFF_HZ, DcBlocker, Resampler, Stereo, drc_factor};

/// Audio buffered ahead of the device: frames run while the ring holds
/// less than this. Low enough to feel immediate, high enough to ride out
/// a late wake-up or a slow frame.
pub const TARGET_LATENCY: Duration = Duration::from_millis(50);

/// Ring capacity. Pacing keeps the fill within a few frames of the
/// target, so this only overflows if pacing is broken.
const RING_LATENCY: Duration = Duration::from_millis(250);

/// The core's full scale ±30,720 (ADR 0021 section 9) maps to 0.5, about
/// -6 dBFS: headroom for the DC blocker's overshoot on large steps.
const GAIN: f32 = 0.5 / 30_720.0;

/// DRC reads a smoothed fill: each reading's weight.
const FILL_SMOOTHING: f64 = 1.0 / 32.0;

/// How often `GBC_AUDIO_STATS=1` prints.
const STATS_PERIOD: Duration = Duration::from_secs(4);

#[derive(Default)]
struct Shared {
    underruns: AtomicU64,
    /// Underruns before the first push are start-up, not counted.
    started: AtomicBool,
    /// Data callbacks so far: the stall watchdog reads it.
    callbacks: AtomicU64,
    /// Errors the backend reported but kept the stream running after.
    errors: AtomicU64,
    /// The stream is gone (device lost or invalidated).
    failed: AtomicBool,
    /// The first error message.
    error: Mutex<Option<String>>,
}

/// No data callback for this long while frames wait on the buffer: the
/// stream is treated as dead, whatever the backend says.
const STALL_TIMEOUT: Duration = Duration::from_millis(500);

struct Stats {
    since: Instant,
    fill_sum: f64,
    readings: u64,
}

pub struct AudioOut {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    prod: HeapProd<Stereo>,
    rate: u32,
    target: usize,
    /// Where DRC centers the post-push fill: pacing tops the ring up to
    /// just past `target`, by up to a frame, so half a frame above it.
    drc_center: f64,
    dc: DcBlocker,
    rs: Resampler,
    avg_fill: f64,
    factor: f64,
    scratch: Vec<Stereo>,
    overflows: u64,
    stats: Option<Stats>,
    /// The callback count last seen, and when it last changed.
    seen_callbacks: u64,
    seen_at: Instant,
}

impl AudioOut {
    /// Opens the default output device with its default config.
    pub fn open() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no default output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("no output config: {e}"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let rate = config.sample_rate;
        let frames = |d: Duration| (u128::from(rate) * d.as_millis() / 1000) as usize;
        let (prod, cons) = HeapRb::<Stereo>::new(frames(RING_LATENCY)).split();
        let shared = Arc::new(Shared::default());
        let channels = config.channels;
        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, config, cons, shared.clone()),
            SampleFormat::I16 => build::<i16>(&device, config, cons, shared.clone()),
            SampleFormat::I32 => build::<i32>(&device, config, cons, shared.clone()),
            SampleFormat::U16 => build::<u16>(&device, config, cons, shared.clone()),
            SampleFormat::F64 => build::<f64>(&device, config, cons, shared.clone()),
            other => Err(format!("unsupported sample format {other}")),
        }?;
        stream
            .play()
            .map_err(|e| format!("can't start the stream: {e}"))?;
        let stats = std::env::var_os("GBC_AUDIO_STATS")
            .is_some_and(|v| v == "1")
            .then(|| {
                eprintln!("audio: {rate} Hz, {channels} ch, {format}");
                Stats {
                    since: Instant::now(),
                    fill_sum: 0.0,
                    readings: 0,
                }
            });
        let target = frames(TARGET_LATENCY);
        let frame_out = f64::from(rate) * 70_224.0 / f64::from(CLOCK_HZ);
        Ok(AudioOut {
            _stream: stream,
            shared,
            prod,
            rate,
            target,
            drc_center: target as f64 + frame_out / 2.0,
            dc: DcBlocker::new(AUDIO_SAMPLE_RATE as f32, DC_CUTOFF_HZ),
            rs: Resampler::new(f64::from(AUDIO_SAMPLE_RATE), f64::from(rate)),
            avg_fill: target as f64,
            factor: 1.0,
            scratch: Vec::with_capacity(2048),
            overflows: 0,
            stats,
            seen_callbacks: 0,
            seen_at: Instant::now(),
        })
    }

    /// Device frames buffered in the ring.
    pub fn fill(&self) -> usize {
        self.prod.occupied_len()
    }

    /// [`TARGET_LATENCY`] in device frames.
    pub fn target(&self) -> usize {
        self.target
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Why the stream is unusable, if it is: the backend lost it, or its
    /// callback stopped for [`STALL_TIMEOUT`] (some backends report an
    /// error and carry on, others just go quiet).
    pub fn failure(&mut self, now: Instant) -> Option<String> {
        let calls = self.shared.callbacks.load(Relaxed);
        if calls != self.seen_callbacks {
            self.seen_callbacks = calls;
            self.seen_at = now;
        } else if now.duration_since(self.seen_at) > STALL_TIMEOUT {
            return Some("the device stopped asking for samples".into());
        }
        if !self.shared.failed.load(Relaxed) {
            return None;
        }
        let msg = self.shared.error.lock().ok().and_then(|m| m.clone());
        Some(msg.unwrap_or_else(|| "stream error".into()))
    }

    /// Filters, resamples and queues one frame's samples (32,768 Hz).
    /// When the ring is full the newest output is dropped and counted.
    pub fn push(&mut self, samples: &[StereoSample]) {
        self.shared.started.store(true, Relaxed);
        self.rs
            .set_step(f64::from(AUDIO_SAMPLE_RATE) / (f64::from(self.rate) * self.factor));
        self.scratch.clear();
        for s in samples {
            let x = self.dc.process([f32::from(s.left), f32::from(s.right)]);
            self.rs.push(x, &mut self.scratch);
        }
        for f in &mut self.scratch {
            *f = f.map(|v| (v * GAIN).clamp(-1.0, 1.0));
        }
        let pushed = self.prod.push_slice(&self.scratch);
        self.overflows += (self.scratch.len() - pushed) as u64;

        let fill = self.fill() as f64;
        self.avg_fill += FILL_SMOOTHING * (fill - self.avg_fill);
        self.factor = drc_factor(self.avg_fill, self.drc_center);
        self.report(fill);
    }

    fn report(&mut self, fill: f64) {
        let Some(st) = &mut self.stats else { return };
        st.fill_sum += fill;
        st.readings += 1;
        if st.since.elapsed() < STATS_PERIOD {
            return;
        }
        let ms = |frames: f64| frames * 1000.0 / f64::from(self.rate);
        eprintln!(
            "audio: underruns {} overflows {} errors {} avg fill {:.1} ms (target {:.0} ms) rate x{:.5}",
            self.shared.underruns.load(Relaxed),
            self.overflows,
            self.shared.errors.load(Relaxed),
            ms(st.fill_sum / st.readings.max(1) as f64),
            ms(self.target as f64),
            self.factor,
        );
        *st = Stats {
            since: Instant::now(),
            fill_sum: 0.0,
            readings: 0,
        };
    }
}

fn build<T: SizedSample + FromSample<f32> + Send + 'static>(
    device: &cpal::Device,
    config: StreamConfig,
    mut cons: HeapCons<Stereo>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, String> {
    let channels = usize::from(config.channels).max(1);
    let on_error = shared.clone();
    device
        .build_output_stream(
            config,
            // Never blocks: pops what is there, silence for the rest.
            move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
                shared.callbacks.fetch_add(1, Relaxed);
                let mut short = false;
                for frame in data.chunks_mut(channels) {
                    let [l, r] = cons.try_pop().unwrap_or_else(|| {
                        short = true;
                        [0.0; 2]
                    });
                    write_frame(frame, l, r);
                }
                if short && shared.started.load(Relaxed) {
                    shared.underruns.fetch_add(1, Relaxed);
                }
            },
            move |e: cpal::Error| match e.kind() {
                ErrorKind::Xrun => {
                    on_error.underruns.fetch_add(1, Relaxed);
                }
                ErrorKind::RealtimeDenied => {}
                kind => {
                    if let Ok(mut m) = on_error.error.lock() {
                        m.get_or_insert_with(|| e.to_string());
                    }
                    // cpal keeps running after other errors (an ALSA I/O
                    // error from the PulseAudio plugin, say): count them
                    // and let the stall watchdog judge.
                    if matches!(
                        kind,
                        ErrorKind::DeviceNotAvailable | ErrorKind::StreamInvalidated
                    ) {
                        on_error.failed.store(true, Relaxed);
                    } else {
                        on_error.errors.fetch_add(1, Relaxed);
                    }
                }
            },
            None,
        )
        .map_err(|e| format!("can't open the stream: {e}"))
}

/// Stereo onto the device's channels: mono gets the average, more than two
/// get left and right in the first two and silence in the rest.
fn write_frame<T: Sample + FromSample<f32>>(out: &mut [T], l: f32, r: f32) {
    match out {
        [] => {}
        [m] => *m = T::from_sample((l + r) * 0.5),
        [a, b, rest @ ..] => {
            *a = T::from_sample(l);
            *b = T::from_sample(r);
            rest.fill(T::EQUILIBRIUM);
        }
    }
}
