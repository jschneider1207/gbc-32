# A3: APU emulation correctness and audio quality

Reviewer A3, tree at HEAD of `main` (49c0756). Read end to end:
`crates/gbc-core/src/apu/{mod,mix,units,square,wave,noise}.rs` (non-test
code in full, tests by name and by the reference they compare against), the
APU paths in `bus.rs` (`write_apu`, `write_nr52`, `apu_div_reset`,
`set_apu_deadline`, `set_div_apu_deadline`, `div_apu_event`,
`audio_buffer_event`, `start_audio`, `finish`, `read_io` FF10-FF3F/FF76/77,
`SOUND_READ_MASK`, `vdma_stall`, `idle_speed_switch`), the run loop and audio
API in `lib.rs`, ADR 0021 with all amendments, `docs/reference/apu.md`, and
the desktop consumer (`gbc-desktop/src/dsp.rs`, `audio.rs`) only as far as it
decides what the core's stream has to be. No cargo, no builds: everything
below is from reading and hand arithmetic.

Headline: I found **no high-severity defect**. The register-level model is
careful, the closed-form integrators agree with the stepping model by
construction, and there is no overflow or wraparound path in long runs. The
real weaknesses are (1) the output filter, which is a quality floor rather
than good audio, (2) the fact that nothing verifies the produced samples
against anything but the model itself, and (3) a layer of fitted timing
windows whose physical model is not stated. Findings are few on purpose.

---

### A3-1 Box-filter decimation leaves audible aliasing on high notes   [severity: medium] [confidence: medium]
- Where: `apu/mix.rs:96-174` (`Apu::sync`, one sample = exact mean over a
  128-clock window), `apu/mix.rs:86-94` (`sample`), ADR 0021 section 8
  ("a crude low-pass ... a quality floor, not hi-fi").
- What: the only anti-alias filter before decimating 4.19 MHz to 32,768 Hz
  is a 128-clock box, i.e. gain `|sinc(f / 32768)|`. That is excellent for
  aliases that fold to low frequencies (they come from harmonics next to a
  multiple of 32,768 Hz, where the box has its nulls) and for ultrasonic
  "DC" tones, but weak for harmonics between 16 and 30 kHz, which fold into
  5-16 kHz almost unattenuated. It is far better than point sampling and
  far worse than what SameBoy/blip_buf-class output does (< -60 dB).
- Scenario or measurement (hand-computed, not measured): 50% square at
  2,093 Hz (C7; f = 0x7C1 region is the boot chime, games use this octave
  for arpeggios and SFX). Harmonic k has amplitude 1/k; alias level
  relative to the fundamental:
  | harmonic | at | folds to | box gain | alias level |
  |---|---|---|---|---|
  | 9  | 18.8 kHz | 13.9 kHz | 0.54 | -24.5 dB |
  | 11 | 23.0 kHz | 9.7 kHz  | 0.36 | -29.6 dB |
  | 13 | 27.2 kHz | 5.6 kHz  | 0.195 | -36.5 dB |
  | 15 | 31.4 kHz | 1.4 kHz  | 0.044 | -51 dB |
  12.5% duty (even harmonics, flatter spectrum) is a few dB worse, and a
  frequency sweep (CH1 sweep SFX) makes the 5-10 kHz products move against
  the pitch, which is the easy-to-hear case. Passband droop is also real:
  -0.9 dB at 8 kHz, -3.9 dB at 16 kHz.
- Reference: plain sampling theory; SameBoy `Core/apu.c` renders through a
  band-limited path; ADR 0021 section 8 itself calls the box "crude".
- Proposed change: do not pick a fix blind. Step 1 (S, no risk): add a
  host-only measurement, a `gbc-core` unit test or romtest-free tool that
  drives `Apu::sync` with one square channel over a frequency list and
  reports the worst inharmonic component (Goertzel over a few seconds of
  samples, integer or `std` float in the test only). That turns this
  finding into a number and guards any later change. Step 2, options in
  order of value per cost:
  1. **Two 64-clock sub-windows per output sample combined [1,2,1]/4 across
     the sample boundary** (triangle approximation, decimate by 2). Gain
     becomes `sinc(f/65536) * cos^2(pi f/65536)`: the 27.2 kHz product
     drops from 0.195 to 0.052 (11.5 dB better), 23 kHz by 7 dB, 18.8 kHz
     by 4 dB, and everything above 38 kHz also improves. Needs one extra
     `[i32; 2]` of carried state (fits the 128 B cap as far as I can count:
     `Output` grows by 8 B) and stays integer and exact, so the sync
     invariant and determinism hold. Cost: the integrators run on twice as
     many spans, so the "channels and mixing" budget (224.1k of 230k
     Ir/frame) roughly doubles in the worst case; that is a budget decision
     for the human with the step-1 numbers in hand. Effort M.
  2. True linear-kernel (sinc^2) integration: each span also returns its
     first moment. Better still (-36 dB becomes -51 dB), but the closed
     forms for square, wave and the LFSR run all need a position-weighted
     sum; L, and code size is already at its ceiling.
  3. Leave the core as is and accept the floor; then say so in the
     real-game checklist rather than calling audio done.
  Whatever is chosen, compensate the treble droop in the front end, not
  the core.
- Test: the step-1 measurement with a threshold; existing
  `random_split_points_change_neither_state_nor_samples` must still pass.

### A3-2 Nothing checks the produced samples against an independent reference   [severity: medium] [confidence: high]
- Where: tests in `apu/square.rs:571` (`integrate_matches_a_clock_by_clock_reference`),
  `apu/wave.rs:442`, `apu/noise.rs:606/900`, `apu/mod.rs:956-1222`
  (`sync_tests`), `apu/mix.rs:198-221`; romtest's audio verdict (ADR 0021
  section 10).
- What: every sample-level test compares the fast path with a slow model
  of the *same rules* (or with itself under a different split). The ROM
  suites judge registers (NR52, PCM12/34, wave RAM) and, for the gambatte
  `_outaudio` ROMs, only "all samples equal or not". So nothing would
  catch an error that is consistent across both paths: polarity, a swapped
  left/right, a wrong duty mask order, a wrong wave nibble order, the
  noise output bit sense, the NR50 `+1`, the trigger-to-first-edge phase as
  heard. I checked those by hand against Pan Docs and SameBoy and they are
  right today (see "Looked at and found fine"), but that is a reviewer's
  reading, not a guard, and the M10 goal is to stop relying on "ROMs pass".
- Scenario: flip `DUTY[1]` to `0b0000_0011`. PCM12-based ROMs that use
  duty 1 would catch it; a change to the analog mapping (`15 - 2 * d` to
  `2 * d - 15`) or to `weights` (swap `nr51 >> 4` and `nr51`) passes every
  ROM in the manifest and every unit test except `mixer_pans_...`, which
  encodes the code's own convention.
- Reference: Pan Docs "Audio details" (DAC polarity, mixer), "Audio
  Registers" FF24/FF25.
- Proposed change (S-M, no core risk): a small set of analytic tests on
  `Apu::sync` output, each derived from the documentation and not from the
  implementation: (a) square at f: count sign changes of the AC part over
  N samples = `2 * 131072 / (2048 - f) * N / 32768` within 1; (b) duty d:
  fraction of samples below the mean matches 12.5/25/50/75%; (c) NR51 =
  0x10 gives signal on `left` only, 0x01 on `right` only; (d) wave RAM
  ramp 0..F plays rising digital, hence falling analog; (e) noise 15-bit:
  first low-going sample 15 steps after the trigger, period 32,767 steps.
  Optionally one golden PCM capture rendered by SameBoy for a fixed
  register script, compared by correlation (not bit-exact, the filters
  differ). Mutation-check each as AGENTS.md requires.

### A3-3 `Square::since_step` uses the frequency after an NRx3 write, not the period that is running   [severity: low] [confidence: high]
- Where: `apu/square.rs:66-69` (`since_step`), `apu/square.rs:213-226`
  (`write_nrx4`), `apu/square.rs:198` (NRx3 updates `freq` at once).
- What: "clocks since the last duty step" is computed as
  `period(self.freq) - countdown`. `freq` is the value for the *next*
  reload; after an NRx3 write it no longer describes the period the
  countdown was loaded from. Games update pitch as NRx3 then NRx4, so this
  is the common order. Result: the "NRx4 within 4 clocks of a step
  re-times the running period" rule fires on the wrong clocks.
- Scenario: CH2 at f = 0x700 (1,024 clocks/step). NR23 = 0x83 gives
  `freq = 0x783`, i.e. 500 clocks. An NR24 = 0x06 write arrives when
  `countdown` is 496..500, that is 524-528 clocks after the real step:
  `since_step` returns 0..4, so `countdown` becomes `period(0x683) - since`
  = 1,520-1,524: the current step is stretched by ~1,024 clocks (one phase
  slip). Conversely a write that really is 0-4 clocks after a step is not
  recognised when NRx3 was written first. Probability about `5 / period`
  per pitch update; audibly nothing (one phase jump), but it is a
  wrong-by-construction path next to a rule that was fitted to a ROM that
  writes only NR14 (SameSuite `channel_1_freq_change_timing`).
- Reference: `docs/reference/apu.md` "Conventions" (the rule as intended).
- Proposed change: keep the running period (or simply a "reloaded at"
  marker): store `reload: u16` set in `step`/`integrate`/`advance`/`trigger`
  where `countdown` is loaded, and compute `since = reload - countdown`.
  `Square` has padding for it (12 B today by ADR count; check
  `apu_fits_in_128_bytes`). Effort S, risk low (state only; the SameSuite
  ROM still passes because it never writes NR13 in between). Unit test:
  the scenario above, expecting `countdown` unchanged.

### A3-4 The fitted trigger-timing windows have no stated physical model; the two square trigger rules are one rule seen twice   [severity: low] [confidence: medium, unverified]
- Where: `apu/mod.rs:271-314` (`env_late`, `lf_div`, `trigger_delay`),
  `apu/mod.rs:360-386` (`write_rest`: sweep skip window 8 clocks, envelope
  window 4), `apu/square.rs:296` (`SWEEP_LAG` = 4), `apu/square.rs:213-226`
  (the <= 4 clock NRx4 window), `timer/mod.rs:397` (`apu_ds_delay`),
  `apu/noise.rs:444-463` (`trigger_delay` table).
- What: five independent constants (4, 8, 4, <= 4, 5/6 + grid) were each
  fitted to a pair of ROMs. They are honest (each cites its ROMs) but they
  are bracketing fits, and every one of them is a statement of the form
  "an effect of a CPU write, or of the DIV edge, reaches the unit N clocks
  later". That is what a model with explicit propagation delays would
  produce from one or two numbers. Concretely, for the square trigger:
  the double-speed "linear rule" `6 + 2 * lf_div` puts the first step at a
  clock `t` with `(t >> 1)` parity equal to the power-on clock's, i.e. it
  is also a snap to a 1 MHz grid (the power-on-relative one), exactly as
  the single-speed branch snaps to the absolute grid `1 mod 4`. So the
  code describes one mechanism (the APU's 1 MHz divider gates the first
  step) with two formulas, one keyed to the absolute clock and one to
  `Fs::phase`, and `apu_ds_delay` is that grid moving by one 2 MHz tick.
- Scenario: power the APU on in single speed after a switch back from
  double speed (CPU on an odd clock grid, ADR 0020), then trigger. The
  single-speed branch ignores `Fs::phase` and uses `now mod 4`; the
  double-speed branch would have used the power-on phase. At most one of
  them is the hardware; no ROM here covers it (apu.md says so).
- Reference: `docs/reference/apu.md` "Square channel trigger countdown";
  SameBoy `GB_apu_write` (`lf_div`), gambatte `DutyUnit::nr4Change`
  (behaviour only).
- Proposed change: not a rewrite now. (1) S: state the grid reading in
  apu.md and express both branches as `c + ((g - now - c) & 3)` with
  `(c, g)` per speed, so the remaining difference between the speeds is
  two visible numbers instead of two formulas. Confirm with the gambatte
  `sound/`, `speedchange*` and SameSuite channel 1/2 groups; note that for
  odd `now` in double speed this differs from today's `(now >> 1)` parity
  rule by one clock, so it must be measured, not assumed. (2) Record as a
  plan task: try a single "write lands N clocks into the APU" delay for
  the sweep (8 before/4 after) and envelope (4) windows; drop it if the
  ROM groups disagree. Risk: these are the most ROM-sensitive lines in the
  APU; do it in a scratch worktree with the full `gambatte` + `same-suite`
  filters.

### A3-5 The stream's headroom requirement is not in the API contract   [severity: low] [confidence: high]
- Where: `apu/mod.rs:34-51` (`StereoSample` doc), `lib.rs:220-229`
  (`AUDIO_SAMPLE_RATE` doc), ADR 0021 section 8 ("No high-pass filter in
  the core").
- What: the core's samples are unipolar-ish: a powered, silent mix sits at
  up to +30,720 (4 DACs on, volume 7), and sound swings down from there.
  The doc says "±30,720, no clamping" and that the front end must
  high-pass. It does not say that the high-passed signal can reach twice
  the core's full scale. An MCU front end that runs the DC blocker in
  `i16` will wrap.
- Scenario: all four DACs on and silent at NR50 = 0x77: constant +30,720,
  the blocker settles to 0. All channels then start in phase at volume 15:
  the first low half-cycle is 61,440 below the settled level, so the
  blocker's output is about -61,440. The desktop survives only because it
  works in `f32` with `GAIN = 0.5 / 30720` and a clamp (`audio.rs:30`).
- Reference: Pan Docs "Audio details", Mixer / DACs (digital 0 is analog
  +1, the capacitor removes the offset).
- Proposed change (S): document the 2x bound next to `StereoSample`, and
  give Stage 2 a tested integer DC blocker instead of a sentence: a
  `no_std`, float-free helper (`i32` state, one multiply per sample,
  saturating to `i16` after a fixed -6 dB) either in a tiny shared crate or
  as an optional `gbc_core::audio` function that takes and returns
  samples (it must not live in the emulation path, so determinism and
  "DC kept" stay as they are). Then desktop and firmware cannot drift.
  Unit test: the scenario above stays within `i16`.

### A3-6 A stale sweep overflow check survives a retrigger   [severity: low] [confidence: medium]
- Where: `apu/square.rs:359-369` (`Sweep::trigger` leaves `check`/`due`),
  `apu/square.rs:397-406` (`Sweep::clock` check branch),
  `apu/mod.rs:375` (`skip` looks at `due && check == 0`).
- What: after a sweep write-back the delayed overflow check runs `shift`
  ticks later (up to 28 clocks plus the 4-clock lag). A CH1 trigger inside
  that window reloads the shadow and runs its own check, but the pending
  one is not cancelled and then runs against the *new* shadow and the
  current NR10.
- Scenario: NR10 = 0x17 (shift 7), sweep step at edge E writes back at
  E+4, check due at E+32. The game writes NR13/NR14 (trigger, f = 0x7F0)
  at E+12 and NR10 = 0x11 at E+20 for the next effect. At E+32 the stale
  check computes `0x7F0 + (0x7F0 >> 1)` > 2047 and turns CH1 off 28 clocks
  into a note that the game started with a non-overflowing NR10 at the
  trigger. (With NR10 unchanged the stale check only repeats the trigger's
  own result, harmless.) Needs writes inside a 32-clock window after a
  128 Hz edge, so rare.
- Reference: SameBoy `GB_apu_write` NR14: the trigger reloads
  `square_sweep_calculate_countdown` (one check, restarted), it does not
  leave the old one running.
- Proposed change: in `Sweep::trigger` clear `check` (and `due` when it
  was only the check); the bus's next `set_apu_deadline` then goes back to
  the DIV edge, so `write_apu` must re-arm the deadline after a CH1
  trigger that cleared it (it already reads the deadline there). Effort S,
  risk low-medium (touches the lag scheduling). Unit test: the scenario,
  CH1 still on at E+40. Note the trigger's own check being instant is a
  documented deferral (apu.md "Not modelled"), not part of this.

### A3-7 Desktop DC blocker starts from zero: a thump at start-up and after every device restart   [severity: low] [confidence: high] (front end; noted because the core's "DC kept" contract causes it)
- Where: `gbc-desktop/src/dsp.rs:24-31` (`prev_in: [0.0; 2]`),
  `gbc-desktop/src/pacing.rs:296` (a fresh `AudioOut` on device switch).
- What: post-boot the core's stream is a constant (CH1 DAC on: +15 x
  (NR50 + 1) x 64 on both sides, 7,680 at NR50 = 0x77). The first sample
  through a blocker whose history is 0 is a full step, decaying with the
  20 Hz time constant (8 ms): a click every time the stream starts.
- Proposed change (S): seed `prev_in` with the first sample pushed (output
  0). Test: constant input gives exactly 0 from the first sample.

---

## Answers to the brief's direct questions

- **Lazy catch-up vs stepping:** identical by construction. `Square`,
  `Wave` integrate/advance are closed forms whose state updates I checked
  term by term (positions visited, the partial last period, countdown
  reload); noise shares one stepping function between both paths and the
  frozen-LFSR case defers to `advance`. NR50/NR51, wave RAM, NR32 and every
  channel register write sync first, so parameters are constant per span.
  The remaining state that depends on the caller is exactly what ADR 0021
  section 9 says: the window grid restarts after an empty-buffer call.
- **Overflow/wraparound in long runs:** none found. `synced` is `u64`;
  `acc` is reset per window (|acc| <= 61,440); lanes are `i16` with a
  compile-time bound; spans >= 2^32 take the cold `rem_long`/`long_steps`
  paths with exact reductions; `Noise::bump` wraps mod 2^14 by design;
  `buffer_deadline` saturates. `Sweep::timer` wraps 0 to 255 from the
  power-off state, which only delays a calculation that is disabled anyway.
- **Buffer full:** `AudioBuffer` is a deadline at the last slot's window
  end and always stops the call; overshoot is the rest of one instruction
  (well under a window), and the two long CPU pauses are both resumable
  (`vdma_stall` by design; the speed-switch halt idles a deadline at a
  time). I found no path that completes a window with no slot.
- **Pops:** DAC on/off, NR51 and NR50 changes and power off step the level
  at once. That matches hardware closely enough (SameBoy's DAC fade is
  about 50 microseconds); the NR50/NR51 steps are what PCM-through-volume
  tricks rely on. Not a defect.
- **Emulator quirks replicated where something simpler may exist:** A3-4.
  The noise `trigger_delay` table and `Noise::rebase` are SameBoy's CGB-C
  branches transcribed; they are documented as such and I have no simpler
  model to offer for them.

## What is untested (beyond A3-2)

- Any spectral or analytic property of the output (frequency, duty,
  polarity, panning side as heard).
- A single-speed trigger after a switch back from double speed (the
  `-now mod 4` term), and a trigger in double speed on an odd clock.
- NRx3-then-NRx4 inside the 4-clock window (A3-3).
- A retrigger inside the sweep check window (A3-6).
- `Sweep::due` deferral through the real scheduler together with a DIV
  write or speed switch inside the lag, beyond the one bus test at
  `bus.rs:3299-3330`; the random sync script drives `Apu::write` with
  `edge = u64::MAX`, so `env_late` and the sweep skip never fire there.
- Output continuity across NR52 off/on mid-stream (state is tested, the
  samples are not), and across an empty-buffer call (grid restart).
- Zombie mode against hardware captures (only the Pan Docs rule as unit
  test; SameBoy's CGB-C `_nrx2_glitch` differs and is a known deferral).

## Bigger-picture recommendations

1. **Give audio a measurable quality bar** (A3-1 step 1 + A3-2). Two small
   test additions turn "sounds right on two games" into numbers: worst
   alias level for a square sweep, and doc-derived analytic checks. Cheap,
   no core risk, and required before deciding anything about the filter.
2. **Decide the filter with those numbers.** If the budget can be found,
   the 64-clock/[1,2,1] scheme is the best value: integer, exact, keeps
   the sync invariant, about 5-12 dB less aliasing where it is audible. If
   not, record the box filter as an accepted limitation in the stage
   checklist.
3. **Ship the DC blocker as shared, integer, tested code** (A3-5, A3-7)
   so the MCU does not reinvent it with `i16` overflow and a start-up
   thump.
4. **Reduce the fitted windows to a stated mechanism where it is free**
   (A3-4 step 1), and fix the two small state slips (A3-3, A3-6) while
   there. Leave the SameBoy-exact envelope/sweep-at-trigger/noise
   background counter deferred: they move ROM counts for other
   revisions, not sound.

## Looked at and found fine

- Frame sequencer: step numbering (`fs` is post-increment; length on odd,
  sweep on 3/7, envelope on 0, equal to Pan Docs steps 0/2/4/6, 2/6, 7);
  DIV write and STOP falling edge (`apu_div_reset`, old bit by speed);
  bit 12/13 by speed; NR52 power-on skip, both edges, against SameBoy
  `GB_apu_div_event`; deadline `NEVER` while off.
- Length: NRx1 load, 64/256, extra clock on enable with odd `fs`, off
  unless the write triggers, reload to max or max-1, clocked while the
  channel is off, cleared by CGB power off, NRx1 ignored while off.
- Envelope: trigger reload (period 0 loads 8, never counts), stop at 0/15,
  Pan Docs zombie rule including the `16 - v` wrap, DAC-off turns the
  channel off, trigger with DAC off stays off.
- Sweep: trigger copy/reload/enable/negate clear/immediate check with
  shift != 0, period 0 as 8, write-back then second check, negate-then-
  clear quirk, frequency kept internally (NR13/14 are write-only, so the
  stale `io` bytes are unobservable).
- Square: duty masks vs SameBoy order, position kept across triggers,
  digital 0 until the first step for a channel that was off, latched
  output bit, `integrate` arithmetic (rotate, whole patterns by popcount,
  last partial step).
- Wave: nibble order, buffer kept on trigger, the `countdown <= 2` reload
  of byte 0, `(2051 - f) * 2`, CGB RAM access redirect for reads and
  writes at the access clock, no CGB corruption, NR32 shifts, eager `sum`
  (exact in the zero state), prefix-sum integral bounds (`k` up to 95).
- Noise: XNOR feedback into bit 14 (and 6), output = bit 0, LFSR 0 on
  trigger, shift 14/15 frozen, period formula, word-parallel stepping
  bounds (`n <= 16` per window), the 0x7F lock-up state falls out
  naturally.
- NR52: read `0x70 | power | on bits`, only bit 7 writable, power off
  zeroes FF10-FF25 including NR50/NR51, wave RAM kept and writable.
- Read masks (`SOUND_READ_MASK`) against Pan Docs; FF15/FF1F/FF27-FF2F.
- PCM12/PCM34: nibble order, 0 when off, pure reads via advanced copies.
- Mixer: NR51 bits 4-7 left, NR50 bits 4-6 left, `volume + 1`, VIN
  ignored; analog `15 - 2 * digital`, 0 with the DAC off; scale 64, peak
  30,720, no clipping possible; `acc >> 1` floors (half an LSB of DC,
  irrelevant).
- `Apu::sync` span bookkeeping (`segs`, `k`, `done`, `pos`) for all of
  first-partial, whole, last-partial and single-span cases.
- 32,768 Hz in both speeds (channels run on base clocks; only DIV-APU
  follows the CPU speed).
