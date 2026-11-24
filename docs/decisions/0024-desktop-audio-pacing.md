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

**2026-11-23: frame statistics.** `GBC_FRAME_STATS=1` (`frame_stats.rs`,
pure, unit-tested; `None` and one branch per event when unset) prints
every 4 s: emulated and presented frames per second, completed frames
replaced before any present ("unpresented"), completed frames identical
to the one before ("unchanged": the game did not redraw, e.g. it lags or
runs at 30 fps, as on hardware), a histogram of frames run per wake-up
(0/1/2/3+), the p1/p99/max interval between completed frames and
between presents, and how late timed wake-ups ran (winit's
`ResumeTimeReached`: actual minus requested, mean and max). Reading it:
emulated ≈ 59.7/s, unpresented 0 and tight intervals mean the host keeps
pace, so remaining choppiness is the game's own (rising "unchanged");
uneven intervals or unpresented frames are ours. `scripts/run-windows.sh`
forwards it (and `GBC_AUDIO_STATS`) to the Windows process through
`WSLENV`.

**2026-11-23: pacing on a smoothed fill.** Measured on native Windows
(WASAPI, 48 kHz, Shantae intro): audio was clean but frames ran 9-24 ms
apart (p1/p99) instead of every 16.74 ms. winit already waits on a
high-resolution timer (wake-ups 0.3 ms late on average), and no wake-up
ran two frames. The cause was the fill itself: WASAPI takes 480 frames
(10 ms) per callback, so the ring's fill falls in 10 ms steps and "run
while below target" ran frames on callback boundaries, 10 or 20 ms
apart; on a 60 Hz display that is visible judder. Changes (`pacing.rs`,
pure and unit-tested, including a simulation of a 10 ms-chunk device
with up to 2 ms of callback jitter that requires every frame within
0.5 ms of 16.74 ms):
- Pacing and DRC read `smoothed_fill`: the ring plus the last chunk,
  less what the device has played of it since it started, as if the
  device drained continuously (bounded to one chunk either way).
- The callback stamps when its chunk starts on a smoothed device clock
  (`callback_clock`: previous start plus its length, nudged 1/8 toward
  the actual callback time; restarted if more than a chunk off), in one
  atomic word with the chunk length, read under a seqlock.
- The wake-up floor is 0.25 ms (was 1 ms): a frame can run up to the
  floor late, and 1 ms showed in the simulation.
- DRC centers on target plus one frame (was half a frame): frames now
  run as the smoothed fill reaches the target, so the post-push fill sits
  a frame above it. The factor went from x0.99936 (pinned off-center)
  to x1.00008.
After (same run, host under build load): frame interval p1/p99 about
14-15.5 / 18-20 ms, rate x1.0001, no underruns. The rest is wake-up
lateness and callback jitter; video is still not vsync-locked, so
59.73 Hz against a 60 Hz display still repeats a frame every ~3.7 s.
`GBC_AUDIO_STATS` now also prints the raw ring fill and the callback
rate, chunk size and min-max gap.

**2026-11-23: attributing stalls.** `GBC_FRAME_STATS` lines also give
`blank` (frames that completed no picture: LCD off), `busy max` (longest
event-loop iteration, wake-up to `about_to_wait` done, all handlers and
the stats print included), `step max` (running one wake-up's frames,
autosave hand-off included), `present max` (one blit + present), `pads
max` (one gilrs poll), and `saves N max X ms` (writes completed on the
save writer thread, ADR 0023 amendment). Wake lateness now also counts a
wake-up caused by an event after the deadline had passed. A gap of 50 ms
or more between completed frames prints its own `hitch:` line: frames
emulated in it and how many were blank, busy/step/wake-late maxima
within it, and the min-max audio pacing fill estimate seen at wake-ups
in it. Reading it: frames ≈ gap / 16.74 ms with all but one blank is
the game blanking the LCD (as on hardware); busy or step near the gap is
our code blocking; wake late near the gap is the OS; few frames with
small busy and late and a fill above target is the pacer holding back.
Measured, native Windows: Wario Land 3's boot shows
`hitch: 344.8 ms between pictures, 23 frames emulated (22 blank), busy
max 36.2 step max 0.4 wake late max 1.00 ms, audio fill 49.0-58.0 ms`.
The human's in-game hitches (exact multiples of a frame, wake late ~1
ms, emulated a little above 59.73/s, no multi-frame catch-up) fit the
same pattern: the extra "emulated" frames are frames cut short when the
LCD turns back on, not catch-up.
