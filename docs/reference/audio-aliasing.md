# Audio aliasing of the output filter

Measurement for M10 review finding A3-1 (`docs/reviews/stage1-m10/A3-apu.md`):
the number the finding asked for, the cost of the candidate fixes
(measured 2026-11-24 at 63ff596, with the 128-clock box filter the core
had then), and what the fix that landed measures.

**Result: the finding is confirmed.** For notes from about 1 kHz up, the
box filter leaves inharmonic tones at -39 to -22 dBc inside 1-8 kHz, and
at -29 to -8 dBc anywhere below 16 kHz. Only low notes (around 500 Hz,
50% duty) stay under -40 dBc in 1-8 kHz. Whether that is *heard* was not
tested: no listening was done, and "-40 dBc in 1-8 kHz" is a plausibility
line, not a measured threshold.

**What landed (ADR 0035, human decision of 2026-11-24):** the candidate
"box 64 + 8-tap FIR". Notes up to 4.4 kHz now have their worst alias
within 1-8 kHz at -40.9 dBc (from -22.1), the 7-bit noise at -17.9 (from
-8.5). Measured on the core: "The filter now" below.

## Method

`crates/gbc-core/tests/apu_aliasing.rs`:

- One channel plays alone at full volume, set up by register writes run
  through the public API (a synthesized ROM, as `tests/apu_output.rs`).
- Periods are chosen so the output repeats exactly every `N` samples and
  the aliases fall *between* the true harmonics (squares: `2048 - f` odd,
  so `N = 2048 - f` samples hold 4 periods). An `N`-point DFT then has no
  leakage: a bin is a true harmonic or an alias, never both, except for
  harmonics a multiple of 131 kHz away, which the box nulls. A period
  that divides the sample grid (`2048 - f` a multiple of 4) folds the same
  energy onto the harmonics: a timbre error instead of stray tones. Those
  are not in the table.
- Reference: the ideal band-limited signal, i.e. the Fourier series of the
  staircase waveform cut at 16,384 Hz.
- **Worst alias dBc**: the strongest non-harmonic bin below 16,384 Hz over
  the *ideal* fundamental. **1-8 kHz**: the same within that band.
  **Alias/signal**: all alias energy below Nyquist over the ideal signal's
  energy below Nyquist. **Droop**: the highest harmonic below Nyquist
  against the ideal one.
- Tests: `box64_fir8_alias_levels` (`box_filter_alias_levels` until ADR
  0035) asserts every row's worst alias and alias/signal within 0.05 dB
  (a ratchet: update it and this file together when the filter changes).
  `model_matches_the_core` checks that the core's samples equal, integer
  for integer, a clock-rate model of each signal through the core's
  filter (64-clock sums, the 8 taps, the shift), and that the same
  kernel in floats differs from them by the dropped fraction only; the
  candidates below run on that model. `candidates` (ignored; `cargo test
  -p gbc-core --release --test apu_aliasing -- --ignored --nocapture`)
  prints the candidate tables.
- Mutation checks. With the box filter (63ff596): a 64-clock window
  (`SAMPLE_PERIOD_LOG2 = 6`) failed both tests; so did a leaky window
  (`acc / 4` carried instead of 0), the ratchet on every tonal row. With
  the FIR (ADR 0035): taps 131 and 459 changed to 151 and 439, or to 132
  and 458, fail both tests (first row -38.70 and -37.69 against -37.64);
  a leaky window fails both (-37.94); a history that is not shifted
  fails both (-56.01).

## The filter now (measured on the core, ADR 0035)

64-clock window sums, then the taps [-22, -56, 131, 459, 459, 131, -56,
-22] / 1024 at 65,536 Hz, every other result kept. Columns as below.

| signal | f0 | worst alias dBc | at | worst 1-8 kHz dBc | alias/signal | droop |
|---|---|---|---|---|---|---|
| square 50% p=261 | 502 | -37.6 | 16196 | -59.2 | -34.6 | -5.74 |
| square 50% p=131 | 1001 | -32.6 | 15759 | -54.3 | -31.5 | -4.95 |
| square 50% p=97 | 1351 | -31.3 | 15202 | -51.0 | -30.9 | -4.80 |
| square 50% p=63 | 2081 | -30.4 | 14043 | -50.2 | -30.3 | -4.40 |
| square 50% p=47 | 2789 | -30.2 | 13247 | -44.7 | -29.8 | -3.69 |
| square 50% p=31 | 4228 | -31.9 | 11627 | -45.3 | -30.5 | -2.48 |
| square 50% p=23 | 5699 | -17.7 | 15672 | -47.4 | -17.5 | 0.15 |
| square 50% p=15 | 8738 | -37.1 | 13107 | -39.5 | -33.6 | -0.30 |
| square 50% p=11 | 11916 | -33.6 | 5958 | -33.6 | -32.5 | -1.88 |
| square 50% p=9 | 14564 | -34.7 | 7282 | -34.7 | -32.4 | -4.43 |
| square 50% p=7 | 18725 | -11.4 | 14043 | -62.5 | | |
| square 50% p=5 | 26214 | -28.9 | 13107 | -49.2 | | |
| square 50% p=3 | 43691 | -26.3 | 10923 | none | | |
| square 50% p=1 | 131072 | none | | none | | |
| square 12.5% p=261 | 502 | -32.2 | 15191 | -50.7 | -31.3 | -5.66 |
| square 12.5% p=131 | 1001 | -29.7 | 14758 | -46.4 | -29.5 | -5.08 |
| square 12.5% p=97 | 1351 | -23.9 | 15202 | -49.2 | -27.9 | -6.89 |
| square 12.5% p=63 | 2081 | -30.8 | 14043 | -44.1 | -32.4 | -4.53 |
| square 12.5% p=47 | 2789 | -17.9 | 16035 | -42.1 | -22.5 | -3.84 |
| square 12.5% p=31 | 4228 | -11.7 | 15855 | -40.9 | -15.4 | -2.61 |
| square 12.5% p=23 | 5699 | -10.2 | 15672 | -45.9 | -12.8 | -1.63 |
| square 12.5% p=15 | 8738 | -9.8 | 15292 | -41.1 | -9.7 | -0.36 |
| square 12.5% p=11 | 11916 | -22.6 | 14895 | -28.0 | -20.9 | -1.88 |
| square 12.5% p=9 | 14564 | -29.6 | 7282 | -29.6 | -26.8 | -4.54 |
| square 12.5% p=7 | 18725 | -11.7 | 14043 | -43.8 | | |
| square 12.5% p=5 | 26214 | -14.9 | 13107 | -49.2 | | |
| square 12.5% p=3 | 43691 | -24.5 | 10923 | none | | |
| square 12.5% p=1 | 131072 | none | | none | | |
| wave, 32-step saw, p=31 | 2114 | -25.9 | 15855 | -60.9 | -26.3 | -4.69 |
| wave, 32-step saw, p=15 | 4369 | -20.9 | 15292 | -57.5 | -22.0 | -2.86 |
| wave, 16-step saw, p=31 | 4228 | -20.1 | 15855 | -50.5 | -21.1 | -2.62 |
| wave, 16-step saw, p=7 | 18725 | -11.7 | 14043 | -49.0 | | |
| wave, 32-step square, p=15 | 4369 | -34.8 | 10923 | -60.0 | -34.4 | -3.00 |
| wave, 32-step saw, p=1 | 65536 | none | | none | | |
| noise 7-bit, 524 kHz clock | 4128 | -7.2 | 16255 | -17.9 | -9.3 | -2.25 |
| noise 7-bit, 262 kHz | 2064 | -7.3 | 16255 | -19.1 | -11.8 | -4.31 |
| noise 7-bit, 131 kHz | 1032 | -7.6 | 16255 | -29.2 | -14.9 | -5.80 |
| noise 7-bit, 65 kHz | 516 | -6.2 | 16255 | -34.9 | -14.2 | -4.60 |
| noise 15-bit, 524 kHz clock | 16 | (bins not searched) | | | -12.3 | -6.92 |

Against the box filter's table below:

- Within 1-8 kHz every note up to 4.4 kHz is at -40.9 dBc or better (the
  worst is the 12.5% square at 4.2 kHz); at 50% duty they are at -44.7
  or better. The squares at 11.9 and 14.6 kHz, whose only audible
  harmonic is the fundamental, keep an alias at -28 to -35 dBc there.
- The strongest alias of a note up to 4.4 kHz is still above 10 kHz
  (most at 14-16 kHz, the filter's transition band), 3 to 13 dB lower.
  Only a longer kernel or less treble removes those.
- Ultrasonic tones used as DC: p=5 at 50% duty leaves -28.9 dBc at
  13.1 kHz (was -12.7 at 6.6 kHz), at 12.5% -14.9 dBc; p=7 still -11.4
  dBc at 14.0 kHz (was -5.3); periods that divide 128 clocks stay clean.
- The 7-bit noise's aliases are 3 dB lower overall and 7 to 26 dB lower
  within 1-8 kHz.
- Droop: the highest harmonic below Nyquist is now at -2.5 to -6.9 dB
  for the low notes (it sits near 16 kHz, where the filter is at -6 dB),
  but harmonics up to 9 kHz are within 0.3 dB (the box: -1.0 dB).

Cost as measured: ADR 0035 and docs/perf.md ("Output filter").

## The 128-clock box filter (measured on the core at 63ff596)

`p = 2048 - f`. Frequencies in Hz, levels in dB.

| signal | f0 | worst alias dBc | at | worst 1-8 kHz dBc | alias/signal | droop |
|---|---|---|---|---|---|---|
| square 50% p=261 | 502 | -34.4 | 16196 | -46.0 | -28.9 | -3.53 |
| square 50% p=131 | 1001 | -28.9 | 15759 | -38.9 | -25.7 | -3.23 |
| square 50% p=97 | 1351 | -26.9 | 15202 | -37.5 | -24.8 | -3.20 |
| square 50% p=63 | 2081 | -24.4 | 14043 | -36.2 | -23.3 | -3.02 |
| square 50% p=47 | 2789 | -22.7 | 13247 | -30.2 | -22.2 | -2.75 |
| square 50% p=31 | 4228 | -21.0 | 11627 | -36.4 | -20.9 | -2.25 |
| square 50% p=23 | 5699 | -13.8 | 15672 | -30.7 | -13.6 | -0.44 |
| square 50% p=15 | 8738 | -22.1 | 6554 | -22.1 | -20.8 | -1.04 |
| square 50% p=11 | 11916 | -31.2 | 2979 | -31.2 | -28.0 | -1.97 |
| square 50% p=9 | 14564 | -23.5 | 10923 | -35.5 | -23.2 | -3.06 |
| square 50% p=7 | 18725 | -5.3 | 14043 | -39.4 | (no audible harmonic) | |
| square 50% p=5 | 26214 | -12.7 | 6554 | -12.7 | | |
| square 50% p=3 | 43691 | -13.6 | 10923 | none | | |
| square 50% p=1 | 131072 | none | | none | | |
| square 12.5% p=261 | 502 | -27.7 | 14689 | -38.3 | -25.3 | -3.45 |
| square 12.5% p=131 | 1001 | -23.5 | 13758 | -35.0 | -22.5 | -3.36 |
| square 12.5% p=97 | 1351 | -19.5 | 15202 | -30.3 | -22.2 | -4.05 |
| square 12.5% p=63 | 2081 | -21.8 | 11963 | -24.7 | -21.7 | -3.16 |
| square 12.5% p=47 | 2789 | -14.5 | 16035 | -30.9 | -18.6 | -2.90 |
| square 12.5% p=31 | 4228 | -8.1 | 15855 | -22.1 | -10.8 | -2.38 |
| square 12.5% p=23 | 5699 | -6.4 | 15672 | -23.7 | -8.1 | -1.90 |
| square 12.5% p=15 | 8738 | -5.5 | 15292 | -15.1 | -4.9 | -1.10 |
| square 12.5% p=11 | 11916 | -10.3 | 8937 | -23.8 | -9.4 | -1.98 |
| square 12.5% p=9 | 14564 | -17.8 | 10923 | -19.6 | -15.5 | -3.17 |
| square 12.5% p=7 | 18725 | -5.6 | 14043 | -20.7 | | |
| square 12.5% p=5 | 26214 | -12.7 | 6554 | -12.7 | | |
| square 12.5% p=3 | 43691 | -11.8 | 10923 | none | | |
| square 12.5% p=1 | 131072 | none | | none | | |
| wave, 32-step saw, p=31 | 2114 | -22.3 | 15855 | -32.5 | -20.5 | -3.14 |
| wave, 32-step saw, p=15 | 4369 | -16.6 | 15292 | -27.5 | -16.4 | -2.42 |
| wave, 16-step saw, p=31 | 4228 | -16.5 | 15855 | -27.6 | -16.3 | -2.39 |
| wave, 16-step saw, p=7 | 18725 | -5.6 | 14043 | -26.1 | | |
| wave, 32-step square, p=15 | 4369 | -22.1 | 10923 | -37.1 | -22.2 | -2.56 |
| wave, 32-step saw, p=1 | 65536 | none | | none | | |
| noise 7-bit, 524 kHz clock | 4128 | -4.0 | 16255 | -10.8 | -4.8 | -2.15 |
| noise 7-bit, 262 kHz | 2064 | -4.1 | 16255 | -10.9 | -6.6 | -3.03 |
| noise 7-bit, 131 kHz | 1032 | -4.5 | 16255 | -11.8 | -8.5 | -3.67 |
| noise 7-bit, 65 kHz | 516 | -3.1 | 16255 | -8.5 | -6.9 | -1.98 |
| noise 15-bit, 524 kHz | 16 | (bins not searched) | | | -7.1 | -3.93 |

Reading it:

- The review's hand computation holds: C7 (p=63, 50%) has its 13th
  harmonic at -36.2 dBc near 5.6 kHz (predicted -36.5) and its 9th at
  -24.4 dBc at 14.0 kHz (predicted -24.5).
- The strongest alias is usually between 12 and 16 kHz (a harmonic just
  above Nyquist, where the box does nothing). The 1-8 kHz column is the
  one that matters most for hearing.
- 12.5% duty is 5 to 14 dB worse than 50%, at every note.
- Ultrasonic tones used as DC are clean where the period divides 128
  clocks (p=1) but not in general: p=5 leaves a 6.5 kHz tone at -12.7 dB
  of the tone's own level, p=7 one at 14 kHz at -5 dB.
- The 7-bit noise is a tone with a flat harmonic series; its aliases are
  about as strong as its harmonics. For 15-bit noise, aliased noise is
  still noise: a tilt of the spectrum, not a new sound.
- Droop of the top harmonic is 2 to 4 dB (the box is -0.9 dB at 8 kHz,
  -3.9 dB at 16 kHz).

## Candidates

Costed before the decision; "current" in the tables is the 128-clock box
of the time. The row that landed is "box 64, then 8-tap FIR", with taps
scaled to a sum of 1,024 (10 bits) instead of the prototype's 8 bits: the
`candidates` test prints both, and they measure the same to 0.1 dB.

All are linear, integer and exact, so the sync invariant (the stream does
not depend on where syncs fall) and determinism hold for each. Alias
numbers come from the prototypes in the `candidates` test, on the model.
"Notes" are the squares and wave tones up to 4.4 kHz (both duties);
"higher" the tones above, ultrasonic ones included.

| filter | kernel, clocks | notes: worst 1-8 kHz dBc | notes: worst dBc | notes: worst alias/signal | higher: worst 1-8 kHz dBc | 7-bit noise: worst 1-8 kHz dBc | gain at 8.7 kHz | at 14.6 kHz |
|---|---|---|---|---|---|---|---|---|
| box 128 (current) | 128 | -22.1 | -8.1 | -10.8 | -12.7 | -8.5 | -1.04 | -3.06 |
| box 64, then [1,2,1]/4 | 192 | -31.3 | -11.4 | -14.6 | -22.9 | -17.1 | -1.82 | -5.37 |
| triangle (box 128 twice) | 255 | -33.1 | -12.1 | -15.4 | -25.2 | -21.4 | -2.08 | -6.06 |
| box 64, then 8-tap FIR | 512 | -40.9 | -11.7 | -15.4 | -28.0 | -17.9 | -0.32 | -4.45 |
| box 64, then 16-tap FIR | 1024 | -41.2 | -15.0 | -18.8 | -28.2 | -18.0 | -0.27 | -4.92 |
| box 64, then 32-tap FIR | 2048 | -41.2 | -21.8 | -25.2 | -28.2 | -18.0 | -0.25 | -4.98 |
| box 32, 7-tap half-band, 15-tap FIR | 1120 | -78.1 | -14.5 | -18.5 | -63.7 | -26.6 | -0.16 | -4.81 |
| box 32, then 16-tap FIR | 512 | -52.0 | -11.0 | -14.7 | -42.3 | -26.4 | -0.11 | -3.87 |
| box 32, then 32-tap FIR | 1024 | -77.1 | -14.2 | -18.2 | -62.5 | -26.6 | -0.08 | -4.38 |
| box 32, then 64-tap FIR | 2048 | -91.2 | -21.1 | -25.0 | -75.6 | -26.6 | -0.06 | -4.44 |
| windowed sinc, 8 samples wide (blep-class) | 1024 | -68.7 | -15.2 | -19.1 | -61.3 | -59.5 | -0.05 | -4.92 |
| windowed sinc, 16 samples wide (blep-class) | 2048 | -98.8 | -19.6 | -23.6 | -86.7 | -84.8 | -0.00 | -4.33 |

FIRs are Kaiser-windowed sincs with integer taps (8 to 12 bits), -6 dB
near 15-16 kHz; the windowed sincs are float prototypes of what a
band-limited-step table would hold. Two things the table shows:

- The "worst dBc" column barely moves until the kernel is 16 samples
  long. Those aliases sit at 14-16 kHz, in the transition band; only a
  long kernel or a lower cutoff (less treble) removes them.
- A two-stage design is limited by its first box: box 64 cannot do better
  than about -41 dBc in 1-8 kHz whatever follows it, box 32 about -78.

### Cost

Measured (callgrind, as `scripts/check-bench.sh`, release build): the
core integrating 64- and 32-clock windows instead of 128, by a scratch
change of `SAMPLE_PERIOD_LOG2` with the bench's audio buffer scaled. This
is the cost of the first stage alone; the second stage is an estimate on
top.

| bench | 128 clocks (now) | 64 clocks | 32 clocks |
|---|---|---|---|
| `synth:worst-case` 2 s, Ir | 286,755,768 | 310,789,978 | 356,361,943 |
| per frame | 2.401 M | +201.2k (+8.4%) | +582.7k (+24.3%) |
| `blargg/cgb_sound` 20 s, Ir | 890,360,439 | 943,233,868 | 1,048,746,948 |
| per frame | 745.4k | +44.3k (+5.9%) | +132.6k (+17.8%) |
| `synth:apu-writes` 2 s, Ir | 77,367,946 | 100,208,274 | 145,525,386 |
| per frame | 647.7k | +191.2k (+29.5%) | +570.6k (+88.1%) |
| thumbv7em `.text`, B | 116,358 | 116,350 | 116,358 |

For scale: the APU's "channels and mixing" budget is 230k Ir/frame
(docs/perf.md), the worst-case line 4.02 M Ir/frame, and the measured
Cortex-M7 worst case 3.09 M cycles/frame in ITCM at 1.12-1.33 cycles per
x86 Ir (47.8% of a frame at 392 MHz).

| candidate | Ir/frame, worst-case | Ir/frame, cgb_sound | `.text` | RAM in `Apu` | effort |
|---|---|---|---|---|---|
| box 64 + [1,2,1] | +201k measured, +~5k est. | +44k measured, +~5k est. | +~100 B est. | +8 B | M |
| box 64 + 8-tap FIR | +201k measured, +~35k est. | +44k measured, +~35k est. | +~250 B est., 16 B table | +24 B est. | M |
| box 32 + 16-tap FIR | +583k measured, +~80k est. | +133k measured, +~80k est. | +~250 B est., 32 B table | +24 B est. | M |
| box 32 + 32-tap FIR | +583k measured, +~140k est. | +133k measured, +~140k est. | +~250 B est., 64 B table | +56 B est. | M |
| triangle (first moments) | +100-200k est. | +30-60k est. | +1-2 KB est. | +8 B est. | L |
| band-limited steps (sinc 8/16) | typical music ~+0-50k est.; worst case +2 M or more est. | not estimated | +1.5-3 KB est., 0.5-2 KB table | 72-136 B est. | L, rewrite |

How the estimates were made (none is measured):

- FIR stage: accumulate each sub-window sum into the pending output
  samples it contributes to (taps / decimation of them per side), instead
  of keeping a delay line. Per sub-window: (taps / decimation) x 2 sides
  multiply-adds at about 3-4 x86 Ir each plus ~10 Ir of bookkeeping; 1,097
  (box 64) or 2,194 (box 32) sub-windows per frame. State: the pending
  sums, `i32` x 2 sides, in place of today's `acc: [i32; 2]`.
- The `Apu` cap is 128 B (unit test); every row but the 16-sample sinc
  can fit only if the current size leaves that room (not checked here).
- Triangle: every channel's `integrate` would return a first moment as
  well as a sum; the closed forms (square periods, the wave's cached sum,
  LFSR runs) each need a position-weighted version. Its alias numbers are
  worse than box 64 + 8-tap, so it was not costed further.
- Band-limited steps: about 16 or 32 table adds per level change (8- or
  16-sample kernel, 2 sides) plus finding the edge, ~80-130 Ir. A 1 kHz
  square has 33 edges a frame, so typical music is cheap. But the
  channels' closed forms skip edges, and this cannot: the worst-case
  bench's periods give tens of thousands of level changes a frame (wave
  at 2 clocks a step: 35,112; noise at 524 kHz: up to 8,778), i.e.
  millions of Ir, unless ultrasonic periods fall back to the averaging
  path. It also ends the "sample = exact window average" contract that
  `tests/apu_output.rs` checks sample by sample.
- A variant with no filter in the core: output 65,536 Hz (box 64,
  the measured +8.4% / +5.9%) and let the front end decimate, or run the
  DAC at that rate. Its alias levels are the "box 64 + 32-tap" row at
  best. It changes the public audio contract (rate, buffer sizes).

## Recommendation

As written for the human before the decision (option 1 was chosen, ADR
0035; its estimate of +35k Ir/frame for the FIR stage and +250 B is
compared with the measured cost there):

1. **Box 64 + 8-tap FIR** is the best value: about -41 dBc in 1-8 kHz for
   notes (19 dB better than now, 10 dB better than the review's [1,2,1]
   for ~30k Ir more), a flat passband (-0.3 dB at 8.7 kHz against -1.0
   now), for about +10% Ir on the worst case and +11% on cgb_sound
   (estimates on measured first-stage costs). On the M7 that is roughly
   +0.26-0.31 M cycles/frame, about 4-5 points of a frame at 392 MHz
   (estimate from the Ir ratio).
2. **Box 32 + 16-tap FIR** if -41 is not enough: -52 dBc for about +28% Ir
   on the worst case.
3. Band-limited steps are the only candidate that also cleans the noise
   channel and the 14-16 kHz aliases, but the worst-case cost is not
   bounded without a hybrid and it rewrites every channel's integrator.
   Not recommended for the MCU target.
4. Doing nothing leaves -22 to -39 dBc inharmonic tones in 1-8 kHz on
   notes above 1 kHz. If that is accepted, record it as a known limitation
   in the real-game checklist.

Passband droop is a separate, cheap matter for the front end (a treble
shelf), whatever is chosen.

## Sources

- ADR 0021 section 8 (the output contract); review A3-1.
- Sampling theory: a `T`-clock box has gain `|sinc(f T / 4194304)|`;
  decimation folds `f` to `|f - k * 32768|`.
- Pan Docs, "Audio Registers" and "Audio details": square duty steps of
  `4 * (2048 - f)` clocks, wave steps of `2 * (2048 - f)`, the noise
  divider and LFSR, the DAC's `15 - 2d` mapping. https://gbdev.io/pandocs/
- Band-limited step synthesis is the technique of Blargg's blip_buf (LGPL:
  not read or copied here; the prototype is a plain windowed sinc).
