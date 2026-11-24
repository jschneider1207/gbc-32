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

**2026-11-23: presentation phase and precise waits.** The decision
stands: the game runs at 59.7275 Hz from the audio clock (DRC within
±0.5%), never at the display's rate. What is new is *when* a finished
picture is presented, and how precisely the loop wakes.

Measured first (native Windows 11, 59.973 Hz display, LADX route of
`scripts/inputs/ladx-new-file-slides.txt`, 6,880 frames, frame log):
pictures were presented 0.2 ms (mean) after completing, so the present
phase drifted with the 59.97 vs 59.73 Hz beat; for a second or so around
each beat crossing the presents ended within ~2.5 ms after a vblank, and
the jitter in when pictures were ready (wake-ups 0.38 ms late on average,
p99 1.0 ms) flipped them between refreshes: 50 pictures shown never and
78 twice where the beat explains 28 (54 irregularities/min beyond the
beat; 115/min on the slides). The present itself is cheap (blit plus
softbuffer's GDI present: median 0.43 ms, p99 1.9 ms) and the flips came
from present *timing*, not its duration, so the blit was left alone.

**Phase-aware presents** (`present.rs`, pure, unit-tested; Windows):
each completed picture (or blank-screen change) is handed to a
`Presenter` with the DWM refresh grid (`dwm::grid`: DWM's last vblank as
an `Instant` and the period, a query per picture and per present). The
present starts inside a window of the refresh interval chosen so that it
*returns* at least 3 ms after a vblank and 2 ms before the next (start
2.5-14.2 ms after the vblank for a 0.5 ms present; the present duration
is a smoothed measurement): at once if the picture is ready inside the
window, else at the window's next start. At most one present starts per
refresh interval (per `k` intervals at `k`x the game rate), counted from
the interval the previous present started in, on the current grid in
whole periods. That rule is the hysteresis that leaves exactly the beat:
a picture a little too late for its interval takes the next one and the
following pictures follow one interval at a time, instead of jitter
flipping them across the window edge. A picture whose turn would be more
than half a frame (8.4 ms) away (only after an early picture: the short
frame when the LCD turns back on, or a stalled loop catching up) takes
the earliest window instead, replacing the previous picture: one drop
rather than every picture for the next second or two waiting most of a
refresh. It applies when DWM timing is available and the refresh rate is
within -0.5%..+3% of 1x or 2x the game rate (59.4-61.5 Hz, 119-123 Hz);
otherwise (off Windows, other rates, no compositor timing) every picture
is presented at once via `request_redraw`, as before. `GBC_PRESENT_PHASE=0`
forces that. Latency added: normally 0-5 ms (refresh minus window), mean
about 0.8 ms, capped at 8.4 ms.

**Precise waits** (`wait.rs`, pure deadline logic, unit-tested): a
frame's due time (audio pacing: when the smoothed fill drops below the
target, if within the 8 ms wake cap plus the margin; the clock pacer's
deadline) and a timed present are deadlines. The loop asks winit to wake
2 ms before the earliest one and spins the rest (`spin_loop` plus
`yield_now` each turn, at most 2.5 ms); a wake-up later than the margin
is late as before. `GBC_PRECISE_WAIT=0` turns it off. The frame log's
`wake_late_us` and `GBC_FRAME_STATS`' wake lateness are now measured
against the deadline, after the spin; `busy max` includes the spin.

Measured after (same route and display; analysis script with the
emulated-rate estimate fixed to keep short stalls, which had overstated
the rate by up to 0.2% and so the beat):

| | before | after |
|---|---|---|
| display irregularities beyond the beat, whole route | 54.1/min | 1.1/min |
| ... slides only (frames 5750-6880) | 115/min | 0/min |
| pictures shown once / twice / never (whole route) | 6,575 / 78 / 50 + 2 unpresented | 6,678 / 24 / 0 + 2 unpresented |
| wake lateness mean / p99 / max | 0.38 / 1.02 / 17.5 ms | 0.03 / 0.99 / 24.6 ms |
| present delay after the picture mean / p99 / max | 0.22 / 0.90 / 5.9 ms | 1.03 / 5.7 / 7.8 ms |
| Wario Land 3, no input, 2,400 frames | 51.0/min | 1.6/min |

The remaining irregularities all coincide with host stalls (wake-ups
3-35 ms late), which a second run under heavier host load showed more
of (8.3/min). CPU: Wario Land 3 used 4.2% of one core with both features
off and 10.3% with them on (`GetProcessTimes`); the spins take 12-14% of
wall time but yield to any other ready thread.

Consequences: a VRR display still breaks the fixed-grid assumption (the
fallback does not detect it); 2x-rate displays are covered by unit tests only.
