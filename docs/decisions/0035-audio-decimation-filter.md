# 0035: Audio decimation: 64-clock sums and an 8-tap FIR

- Status: Accepted (human, 2026-11-24)
- Date: 2026-11-24
- Amends: ADR 0021 section 8 ("Filter" and the sample formula under
  "Mixing") and the "no clamping" part of the `StereoSample` contract.
  The rate, the buffer contract and everything else in ADR 0021 stand.

## Context

ADR 0021 made each output sample the average of the mixed signal over its
128 clocks: a box filter straight from 4,194,304 Hz to 32,768 Hz. Its
gain is `|sinc(f / 32768)|`, so harmonics between 16 and 30 kHz fold back
into the audible band almost unattenuated. M10 review finding A3-1 asked
for the number; `docs/reference/audio-aliasing.md` has it
(`tests/apu_aliasing.rs` measures it on the core):

- notes up to 4.4 kHz (squares at both duties, wave tones): inharmonic
  tones up to **-22.1 dBc** within 1-8 kHz, up to -8.1 dBc below 16 kHz;
- higher and ultrasonic tones: up to -12.7 dBc within 1-8 kHz;
- the 7-bit noise: up to -8.5 dBc within 1-8 kHz;
- passband: -1.0 dB at 8.7 kHz, -3.1 dB at 14.6 kHz.

The same note costs twelve candidates on a clock-rate model that
reproduces the core's samples exactly. The human chose "box 64 + 8-tap
FIR" on 2026-11-24, with its cost (about +10% on `synth:worst-case`) and
a re-recorded bench baseline.

## Decision

Two stages, both integer and exact, in `apu/mix.rs`:

1. **Window sums.** The mixed signal is summed over 64-clock windows
   (`WINDOW_LOG2 = 6`), two per output sample: the M6/M10 span machinery
   unchanged, with spans cut every 64 clocks instead of 128. A window sum
   is the average level times 64, which already is the sample scale, at
   most 480 x 64 = 30,720 in magnitude: it is kept as `i16`.
2. **FIR, 2:1.** At the end of every second window, the sample is

   ```
   (-22 x0 - 56 x1 + 131 x2 + 459 x3 + 459 x4 + 131 x5 - 56 x6 - 22 x7) >> 10
   ```

   over the last 8 window sums, `x7` the newest, saturated to `i16`. The
   taps are a Kaiser-windowed sinc (beta 3, -6 dB at 16,384 Hz) scaled to
   a sum of 1,024 and rounded; the rounded taps add up to exactly 1,024.
   They are symmetric (linear phase; 4 multiplies per side), and the taps
   at even and at odd positions each add up to 512, so a tone at 32,768 Hz
   (the decimation's image of DC) is nulled.

**Scale and bounds.** The DC gain is exactly 1: a steady level comes out
as before (level x 64, full scale ±30,720), so the loudness is unchanged.
The filter's sum is at most 1,336 x 30,720 = 41,041,920 in magnitude
(1,336 = the sum of the taps' magnitudes): 2% of `i32`. The shift rounds
down, as the old average did. The result can exceed `i16`: around a step
the output overshoots by up to 78/1,024 = 7.6% of the step's height (and
dips by as much the other way just before it: the step response is
symmetric), and a signal alternating in step with the taps'
signs reaches 1,336/1,024 = 1.30 times its amplitude, 40,080 at full
scale. So the sample **saturates** at -32,768 and 32,767. That needs a
mix swinging over more than 81% of the full scale: all four channels at
master volume 7 stepping together (`tests/apu_output.rs`,
`the_mixer_adds_the_channels`, reaches it); three channels cannot
(23,040 x 1.30 = 30,060). Scaling the output down instead (by 0.82)
would have made everything quieter for a case that almost never occurs.
All of this is asserted at compile time or unit-tested in `mix.rs`.

**Delay.** A sample is the filter over the 512 clocks that end where its
128 clocks end, centred 256 clocks before that end; the old average was
centred 64 before. The stream is therefore **1.5 samples (192 clocks,
46 µs) later** than before, 2.0 samples behind the emulated clock in
all. Video is not delayed, so audio trails it by that much: far below
what is perceptible, and not compensated.

**State.** `Output` gains the window history (`[[i16; 2]; 8]`, 32 B) and
the half-sample flag; `acc` stays. The history is state like `acc`: the
samples do not depend on where syncs, span batches, run calls or the
caller's buffers end (ADR 0021 section 3; `check-sync-invariance.sh`).

**Restart.** After an advance-only stretch (empty buffer, ADR 0021
section 9) the window grid restarts and the first window's sum stands
for the seven before it. A steady level is therefore steady from the
first sample on, with no ramp from 0 or from stale history: romtest's
Gambatte audio verdict ("silent" = all samples of the last frame equal,
ADR 0021 section 10) reads exactly as before. That section's blind spot
is also unchanged: a tone whose period divides 128 clocks still comes
out constant (the window sums alternate between two values, and the even
and odd taps weigh them equally).

**Public API.** Unchanged: rate, `AUDIO_SAMPLE_CLOCKS`, samples per call,
buffer contract. The `StereoSample` rustdoc now describes the filter,
the ringing and the saturation; a DC blocker behind the core must expect
a range of up to ±65,535 at its output (was ±61,440).

## Measured

Alias levels, on the core (`tests/apu_aliasing.rs`,
`box64_fir8_alias_levels`; full table in the reference note):

| | 128-clock box | this filter |
|---|---|---|
| notes to 4.4 kHz: worst alias within 1-8 kHz | -22.1 dBc | **-40.9 dBc** |
| notes to 4.4 kHz: worst alias below 16 kHz | -8.1 dBc | -11.7 dBc |
| higher tones: worst within 1-8 kHz | -12.7 dBc | -28.0 dBc |
| 7-bit noise: worst within 1-8 kHz | -8.5 dBc | -17.9 dBc |
| gain at 8.7 kHz | -1.04 dB | -0.30 dB |
| gain at 14.6 kHz | -3.06 dB | -4.43 dB |

Cost (callgrind Ir as `scripts/check-bench.sh`, against main at 2821437;
thumbv7em sizes from `scripts/check-code-size.sh`):

| bench | main, Ir | with the filter, Ir | delta | per frame |
|---|---|---|---|---|
| `blargg/cpu_instrs` 20 s | 1,264,344,219 | 1,327,813,432 | +5.02% | +53.1k |
| `blargg/cgb_sound` 20 s | 890,523,560 | 973,851,265 | +9.36% | +69.8k |
| `cgb-acid2` 20 s | 649,967,201 | 726,390,252 | +11.76% | +64.0k |
| `synth:event-storm` 2 s | 175,300,349 | 182,973,533 | +4.38% | +64.2k |
| `synth:dma-storm` 3 s | 381,935,612 | 393,398,153 | +3.00% | +64.0k |
| `synth:halt-irq` 10 s | 263,308,262 | 301,522,527 | +14.51% | +64.0k |
| `synth:worst-case` 2 s | 286,911,384 | 314,276,521 | **+9.54%** | +229.1k |
| `synth:cgb-raster` 2 s | 267,388,360 | 275,061,275 | +2.87% | +64.2k |
| `synth:apu-writes` 2 s | 77,400,848 | 103,388,853 | +33.58% | +217.6k |
| `synth:dmg-raster` 2 s | 185,765,867 | 193,438,927 | +4.13% | +64.2k |
| thumbv7em `.text` | 115,692 B | 115,940 B | +248 B | |
| `size_of::<Memory>()`, thumbv7em | 50,984 B | 51,016 B | +32 B | |

Every bench line grows, the silent ones too (about 64k Ir a frame): the
benches pass audio buffers, and a window costs its listing, its four
integrator calls and its mix whatever plays. Most of the growth is the
first stage, twice the windows: measured alone before the decision at
+201k Ir/frame on `synth:worst-case`, +44k on `cgb_sound` and +191k on
`synth:apu-writes` (the reference note, at 63ff596). The FIR and the
history are the rest, 26-28k Ir/frame, about 50 Ir per output sample
(estimated at 35k). A first version kept the history as a ring indexed
through `& 7` and cost 23k Ir/frame and 134 B more; the history is now
moved down by two entries per sample (24 bytes) and read at fixed
offsets. `synth:worst-case` is 2.63 M Ir/frame with the filter (2.40 M
before), 65% of the 4.02 M line of docs/perf.md.

## Alternatives

Costed in `docs/reference/audio-aliasing.md` ("Candidates"):

- **Keep the 128-clock box**: -22 dBc inharmonic tones on notes above
  1 kHz. Rejected by the human.
- **Box 64 + [1, 2, 1]/4** (the review's suggestion): -31.3 dBc, 10 dB
  worse than this for about 30k Ir/frame less, and more treble loss
  (-1.8 dB at 8.7 kHz).
- **Triangle (box 128 twice)**: -33.1 dBc, and every channel's closed
  form needs a first-moment version (1-2 KB).
- **Box 64 + 16 or 32 taps**: no better within 1-8 kHz (-41.2 dBc): the
  first box limits it.
- **Box 32 + 16-tap FIR**: -52 dBc for about +28% on the worst case. The
  next step if -41 dBc turns out not to be enough: `WINDOW_LOG2` and the
  taps are the only things to change.
- **Band-limited steps**: the only candidate that also cleans the noise
  channel and the 14-16 kHz aliases; unbounded worst-case cost (tens of
  thousands of level changes a frame) and a rewrite of every integrator.
- **65,536 Hz out of the core**, filter in the front end: changes the
  public audio contract and moves the work to every platform.

## Consequences

- Notes above 1 kHz lose their inharmonic tones in the band the ear is
  most sensitive in. What remains is at 14-16 kHz (the transition band),
  and the 7-bit noise, whose aliases are still only 18 dB down.
- The top octave is flatter up to about 10 kHz and rolls off more above
  (-4.4 dB at 14.6 kHz). A front end that wants to correct treble can.
- A sample is no longer "the exact average of its window": it depends on
  512 clocks, rings around edges and is rounded. `tests/apu_output.rs`
  reads levels where they are held for 4 samples or more, and bounds the
  rounding where it sums.
- The APU "channels and mixing" budget of 230k Ir/frame (docs/perf.md)
  no longer describes the APU: the filter adds 229k Ir/frame on the
  worst case. The human approved that cost and the re-recording of
  `perf/bench-baseline.txt` (2026-11-24); a new figure for the budget
  line is theirs to set.
- The MCU pays the same share: estimated from the Ir ratio, not yet
  measured on the Cortex-M7.
