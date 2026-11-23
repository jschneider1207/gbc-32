# 0024: Desktop audio output and audio-driven frame pacing

- Status: Accepted
- Date: 2026-11-22
- Builds on: ADR 0021 sections 8-9 (audio output contract), ADR 0023

## Context
The core emits 32,768 Hz stereo `i16`, box-filtered, full scale ±30,720,
with DC kept (ADR 0021). DC blocking and resampling to the device rate are
the front end's job. The emulated frame rate (4,194,304 / 70,224 =
59.7275 Hz) and the audio device clock are independent, so pacing frames by
the wall clock alone drifts against the device and eventually underruns or
overflows. M7 asks for audio output with frame pacing driven by audio.

## Decision
**Output:** cpal (Apache-2.0) on the default device with its default
config; f32, i16, i32, u16 and f64 samples. Mono gets (L+R)/2; more than
two channels get L, R and silence. Between the main thread and the
callback is a lock-free SPSC ring (`ringbuf`, MIT/Apache) of device-rate
f32 stereo frames, 250 ms long. The callback only pops: on underrun it
writes silence and counts the callback; it never blocks. A full ring drops
the newest output and counts it. `GBC_AUDIO_STATS=1` prints underruns,
overflows, backend errors, average fill and the DRC factor every 4 s.

**Signal path (main thread, `dsp.rs`, pure and unit-tested):** one-pole
high-pass per channel at 20 Hz on the 32,768 Hz stream (the CGB output
capacitor; Pan Docs "Audio details", the idea behind SameBoy's highpass
mode), then a streaming Catmull-Rom cubic resampler whose fractional
phase carries across calls, then gain 0.5 / 30,720 (full scale ≈ -6 dBFS,
headroom for the high-pass overshoot on large steps) and a clamp to ±1.

**Pacing (`pacing.rs`):** `Pacing::{Audio, Clock}`. With audio, each
wake-up runs frames while the ring holds fewer than 50 ms of device frames,
at most 4 per wake-up so input and redraws keep flowing; it then sleeps
until the fill should drain back to the target, clamped to 1-8 ms
(immediately if the cap stopped it below target). Video shows the latest
completed frame. The emulator's speed is therefore slaved to the device
clock.

**Dynamic rate control:** the resampling ratio is the nominal one times a
factor `1 - 0.005 * clamp((fill - center) / center, -1, 1)`, from a
smoothed (1/32 per frame) post-push fill. `center` is the target plus half
a frame, since pacing tops the ring up to between the target and one frame
above it. The bound is ±0.5% (under 9 cents of pitch).

**Fallback:** if the device or stream can't be opened, or the stream is
lost (cpal `DeviceNotAvailable` / `StreamInvalidated`), or the data
callback stops for 500 ms, one warning is printed and pacing switches to
the wall-clock `Pacer` (silent). Other backend errors are counted, not
fatal: cpal's ALSA backend reports them and keeps streaming (under WSLg the
PulseAudio ALSA plugin reports a burst of `snd_pcm_avail_delay` EIO errors
at start, then plays normally). `GBC_NO_AUDIO=1` forces the silent clock
path. Headless runs open no device.

## Consequences
- Audio latency is the 50 ms ring plus the device's own buffer; lower is a
  constant change once measured on Windows.
- Emulation speed follows the device clock (within the DRC bound), not
  exactly 59.7275 Hz; video is not vsync-locked and can judder by a frame.
- The callback-side code has no unit tests (it needs a device); the stall
  watchdog and error classification were exercised only under WSLg.
- Under WSLg, the compositor (weston) segfaulted twice during early smoke
  runs with audio on; not reproduced since, and not seen with
  `GBC_NO_AUDIO=1` or with `paplay` alone. Cause unknown; worth watching.

## Amendments

**2026-11-22: audio recovers after a fallback.** Falling back to the wall
clock is no longer permanent. When the device never opened, or a stream
was lost or stalled, `Pacing::Clock` carries a `RetryPolicy` (`pacing.rs`,
pure, unit-tested): the first attempt is 2 s after the loss, each failed
attempt doubles the wait up to 16 s. `Pacing::begin` checks it with one
comparison per wake-up and, when due, calls `AudioOut::open()` on the
default device on the main thread (it can block for tens of ms on some
backends, once per 2-16 s). On success it replaces the pacing with a fresh
`AudioOut`: empty ring, new DC blocker, resampler phase and DRC state, so
no stale samples play and the first frames refill the 50 ms target as at
start-up. `GBC_NO_AUDIO=1` stays silent with no retries. Verified: the
policy by unit tests; the open/switch path only by code reading and a
native Windows smoke run (a device can't be unplugged by an agent).
