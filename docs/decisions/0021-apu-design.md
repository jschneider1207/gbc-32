# 0021: APU: lazy catch-up, state in `Memory`, 32,768 Hz box-filtered output

- Status: Accepted
- Date: 2026-11-22

## Context
M6 adds the APU (plan `docs/plan/stage-1.md` M6). The only M6 acceptance
is must-tier `blargg/cgb_sound`, whose 12 subtests check behavior through
registers, not samples: 01 registers (read masks; writes ignored while
off), 02 length counters, 03 trigger (length reload and extra clocking),
04 sweep, 05 sweep details (negate quirk, second calculation), 06 overflow
on trigger, 07 length/sweep period sync with the frame sequencer, 08
length counters during power (on CGB they don't survive power off), 09
wave RAM read while on (CGB: the byte being played), 10 wave trigger
while on (CGB: no corruption), 11 registers after power, 12 wave RAM
write while on. The 131 gambatte `_outaudio0/1` ROMs (stretch) are
`unsupported` until the core produces audio (ADR 0018).

Forces:
- **Perf.** `synth:worst-case` is at 3.54 M x86 Ir/frame, 88% of the
  4.02 M line (50% of an H753). Nothing may run per M-cycle (ADR 0011);
  the APU catches up lazily (ADR 0002).
- **Code size.** `.text` is 85,068 of 85,100 B. The human raises the total
  once for M6, to a figure measured after the first channel lands
  (STATUS, session 07), so the first step must let us extrapolate.
- **State size.** `Hw` is capped at 864 B (832 used, a test checks); the
  APU needs roughly 80 B.
- **Pure reads.** `bus::read` takes `&Memory, &Sys` and is shared with
  `peek` (ADR 0008): register reads have no side effects, so NR52, wave
  RAM while playing and PCM12/PCM34 must be computable from the stored
  state and the clock.
- **Determinism.** The same ROM and inputs give the same output; the
  audio buffer the caller passes must not change emulated behavior.

References (read for behavior; SameBoy is MIT, Gambatte GPL and read for
rules only): Pan Docs "Audio", "Audio Registers", "Audio details"
(gbdev.io/pandocs); SameBoy `Core/apu.c` (`GB_apu_div_event`,
`GB_apu_write`/`GB_apu_read`, `GB_apu_init`, `_nrx2_glitch`,
`trigger_sweep_calculation`), `Core/timing.c` (`GB_set_internal_div_counter`);
Gambatte `test/testrunner.cpp` (the audio verdict, ADR 0018).

## Decision

### 1. Module layout
```
apu/mod.rs     Apu (state, power, frame sequencer, sync), register write
               entry point, pure read helpers (NR52, wave RAM, PCM), StereoSample,
               output constants
apu/units.rs   Length and Envelope, shared by the channels that have them
apu/square.rs  Square (CH1 and CH2: one non-generic impl, `[Square; 2]`),
               Sweep (CH1 only)
apu/wave.rs    Wave (CH3)
apu/noise.rs   Noise (CH4)
apu/mix.rs     the window integrator and mixer (output samples)
```
No generics or traits over channels (one copy of each function, for code
size); channels are plain `Copy` structs, so a pure read can advance a
copy.

### 2. Where the state lives, and its size
`Apu` is a field of `Memory` (`Memory::apu`), not of `Hw`. `Hw` has 32 B
left under its cap; the APU is only touched on cold paths (APU register
access, two event kinds, the end of a run call), so reaching it through
`ctx.mem` costs the hot path nothing. All-zero is a valid `Apu` (powered
off), so `Memory::new()` stays `const` all-zero and `.bss`; `reset`
writes the post-boot state, as for the other non-zero `Memory` fields
(architecture.md, "Core ↔ platform boundary"). `Hw` does not grow: the
`DivApu` and `AudioBuffer` deadlines already exist in `Schedule`.

The register bytes stay in `mem.io` (FF10-FF25 raw, wave RAM FF30-FF3F),
so the plain register reads (`mem.io[reg] | SOUND_READ_MASK`) are
unchanged. `Apu` holds only the internal state, about 80 B (`repr(C)`):

| Part | Fields (approx.) | Bytes |
|---|---|---|
| `Output` | `synced: u64` (clock the state is exact at), window position `u8`, `acc: [i32; 2]`, `live: bool` | ~20 |
| `Fs` | power `bool`, step `u8`, skip `bool`, NR52 active bits `u8`, 1 MHz phase `u8` | ~5 |
| `Square` x2 | countdown `u16`, freq `u16`, duty pos, volume, envelope timer/flags, length, flags (`u8`s) | 2 x 12 |
| `Sweep` | shadow `u16`, timer, flags | 4 |
| `Wave` | countdown `u16`, freq `u16`, length `u16`, index, sample buffer, flags, cached waveform sum `u16` | ~12 |
| `Noise` | countdown `u32`, LFSR `u16`, volume, envelope timer/flags, length, flags | ~12 |

A unit test asserts `size_of::<Apu>() <= 128`. Power off replaces
everything but `Output` with the zero state (section 7), so the output
stream continues across power cycles.

### 3. Timing: lazy catch-up and its invariant
`Apu::sync(now, io, out)` brings the state from `synced` to `now`: each
channel's period counter, duty/sample position and LFSR advance exactly
in base clocks, and, if the caller's buffer has room, the elapsed time is
integrated into output samples (section 8). Nothing runs per M-cycle.

**Invariant:** the register-visible state (NR52 bits, lengths, envelope
volume, sweep, LFSR, positions, PCM12/PCM34, wave RAM accesses) is a
function of the clock and the register writes only, never of where syncs
happened. `advance(a→b); advance(b→c)` must equal `advance(a→c)`
bit-for-bit. A unit test syncs at random split points and compares; a
whole-machine check runs `bench --fingerprint` with and without audio
buffers and requires identical fingerprints (plan task).

The state only changes discontinuously at two kinds of points, so those
are where syncs are required:
- **APU register writes** (FF10-FF26, wave RAM FF30-FF3F): `Ctx::write_apu`,
  `#[inline(never)]` (off `write_high`, ADR 0011's rule for cold I/O):
  sync to the access clock, then apply the write.
- **Frame-sequencer steps** (`EventKind::DivApu`, section 4): sync, then
  step length/sweep/envelope.

Two more sync points serve the output only:
- **`EventKind::AudioBuffer`**: set when a run call starts with buffer room,
  at the clock where the last free slot's window ends. Handler: sync,
  raise `AUDIO_BUFFER_FULL` (always a stop event), deadline `NEVER`.
- **End of each run call**: sync to the final clock (in `Emulator::run`,
  before building `RunResult`), so `audio_samples` counts every sample
  completed in the call and the partial window carries over in `acc`.

Reads stay pure (`read_io(&Memory, &Sys, &Hot)`):
- NR52: `0x70 | power << 7 | active bits`, all stored state (active bits
  only change at writes and frame-sequencer steps).
- FF10-FF25: `mem.io | SOUND_READ_MASK` as today (power off zeroes `mem.io`).
- Wave RAM, PCM12, PCM34: one `#[inline(never)] read_apu(mem, now, reg)`
  that copies the channel, advances the copy by `now - synced`, and reads
  it. While powered on, `DivApu` fires every 8192 clocks, so the copy's
  advance is bounded (the noise LFSR: at most 1024 steps, only on an
  FF77 read).

### 4. Frame sequencer and DIV
DIV-APU is the falling edge of DIV bit 4, bit 5 in double speed (Pan Docs
"Audio details", DIV-APU), i.e. system-counter bit 12 or 13 (ADR 0011,
SameBoy `apu_bit`). It is the `DivApu` deadline, computed from the timer
like the serial clock: `timer.clock_at(next_fall(bit, timer.sc(now)))`,
`next_fall(k, a) = ((a >> (k+1)) + 1) << (k+1)`, `bit = 12 + shift`. It is
`NEVER` while the APU is powered off. One helper,
`Ctx::set_div_apu_deadline`, is called wherever the counter is rebased or
the APU powers on:
- **DIV write and STOP** (`Ctx::div_reset`): `old = timer.write_div(now)`
  is already returned; if `old & (1 << bit)` the reset is a falling edge
  and the frame sequencer steps now (sync first, then the step; SameBoy
  `GB_set_internal_div_counter`, `triggers & apu_bit`), then the deadline
  moves with the rebased counter.
- **Speed switch**: after `timer.speed_switch`, the bit changes 12 ↔ 13 and
  the counter may be frozen for the pause; recompute the deadline (the
  frozen counter is already handled by `clock_at`).
- **Reset/load**: post-boot deadline. **NR52 power on**: below.
- When STOP freezes DIV (M5 "STOP accuracy" task), the same helper gives
  no edge until DIV runs again; nothing APU-specific is needed.

The step counter follows SameBoy's `div_divider` (Pan Docs gives the same
rates): `fs` is the number of steps since power on, mod 8. A step: if
`skip`, clear it and do nothing else; otherwise `fs = (fs + 1) & 7`, then
- `fs & 1 == 1`: clock the length counters (256 Hz);
- `fs & 3 == 3`: clock the sweep (128 Hz);
- `fs == 0`: clock the envelopes (64 Hz). (SameBoy decrements the volume
  countdown at `fs == 7` and applies the tick at the next event; the
  classic model below ticks at `fs == 0`, the same step.)

"The next step doesn't clock length" (the extra-length-clocking
condition, Pan Docs "Obscure behavior") is `fs & 1 == 1`.
At NR52 power on: `fs = 0`, `skip = false`; but if the counter's APU bit
is 1 at that moment, `skip = true, fs = 1` (SameBoy NR52 power-on: the
first falling edge comes too soon and is skipped).

The rising-edge `GB_apu_div_secondary_event` is not modelled in M6 (the
classic envelope below doesn't need it; ADR 0011 left it optional).

### 5. Channels
All periods are in base clocks (4,194,304 Hz), the same in both CPU speeds
(only the frame sequencer follows double speed). Each channel keeps
`countdown` (clocks to its next step, >= 1) and reloads it from its period
at each step; a frequency or NR43 write changes the period used at the next
reload, not the running countdown (SameBoy keeps `sample_countdown`).
Divisions by periods use `NonZeroU32` so there is no divide-by-zero panic
path (thumbv7em code size; ADR 0009's no-panic spirit).

**Shared units** (`units.rs`), Pan Docs "Audio Registers" / "Obscure behavior":
- *Length* (max 64; 256 for CH3): an NRx1 write loads `max - value`.
  Clocked when enabled: decrement; at 0 the channel turns off. An NRx4
  write that enables length (bit 6 from 0 to 1) while `fs & 1 == 1` and
  the counter is non-zero clocks it once more; reaching 0 turns the channel
  off unless the write also triggers. A trigger with the counter at 0 loads
  `max`, or `max - 1` if length is enabled and `fs & 1 == 1`.
- *Envelope* (classic model): trigger loads volume = NRx2 >> 4 and the
  timer from the period; on envelope steps, if the period is non-zero the
  timer counts down and at 0 reloads, moving the volume one step in the
  NRx2 direction until 0 or 15, where the envelope stops (flag).
  **Zombie mode:** an NRx2 write while the channel is on applies the Pan
  Docs CGB-02/04 rule ("Audio details", Zombie mode): if the old period was
  0 and the envelope is still running, volume + 1; else, if the old
  direction was decrease, volume + 2; then, if the direction changed,
  volume = 16 - volume; keep the low 4 bits. (SameBoy `_nrx2_glitch` is the
  same idea on its lock/clock model; exactness is deferred, section 12.)
- *DAC*: on when `NRx2 & 0xF8 != 0` (CH3: NR30 bit 7). A DAC-off write
  turns the channel off; a trigger with the DAC off leaves it off (length
  reload still happens).

**Square** (CH1, CH2): duty position 0-7 steps every `(2048 - f) * 4`
clocks. Duty patterns as one 8-bit mask each, bit i = position i (SameBoy
`duties`, Pan Docs NRx1 waveforms), positions 0-7: 12.5% `0000_0001`, 25%
`1000_0001`, 50% `1000_0111`, 75% `0111_1110`. The position is reset
only by APU power off (Pan Docs: "cannot be reset, except by turning the
APU off"); a trigger reloads the countdown only. Trigger countdown: SameBoy
`sample_countdown = (f ^ 0x7FF) * 2 + delay` in 2 MHz ticks (2 base
clocks), `delay = 6 - lf_div` for an inactive channel and `4 - lf_div` for
an active one, where `lf_div` is SameBoy's 1 MHz phase (we derive it from
the clock and the power-on clock parity, stored as one bit). The
implementer converts that to base clocks from SameBoy's run loop (whether
a countdown fires at 0 or at -1) and records the conversion in
`docs/reference/apu.md`. A newly started channel outputs digital 0 until
its first duty step (Pan Docs: "On first startup, the channel outputs a
digital zero"; SameBoy `did_tick`). Output: `volume` if the duty bit at the
position is 1, else 0.

**Sweep** (CH1; classic model, Pan Docs "Audio Registers" NR10, blargg
04-06): trigger copies the frequency to `shadow`, loads the timer (period 0
counts as 8), sets `enabled = period != 0 || shift != 0`, clears
`negate_used`, and if `shift != 0` runs one calculation (overflow → off).
On sweep steps: the timer counts down; at 0 it reloads (0 → 8) and, if
enabled and the period is non-zero, `new = calc()`; if `new <= 2047` and
`shift != 0`, store it as the shadow and the channel frequency, then run
`calc()` again for the overflow check only. `calc()`: `shadow ± (shadow >>
shift)`; a subtraction sets `negate_used`; a result over 2047 turns the
channel off. **Negate quirk:** an NR10 write that clears the negate bit
after `negate_used` turns the channel off (SameBoy checks this always on
CGB C and older).

**Wave** (CH3): sample index 0-31 steps every `(2048 - f) * 2` clocks; at
each step the index increments and the sample buffer loads the nibble
(high nibble first). Trigger: index 0, sample buffer kept (Pan Docs:
retrigger does not clear or refresh it), first step after SameBoy's
`sample_countdown = (f ^ 0x7FF) + 3` ticks, i.e. `(2051 - f) * 2` base
clocks if a countdown of n fires after n + 1 ticks (that convention
matches the per-step period); blargg 09 and the gambatte `ch3_*read_ff30*`
hex ROMs are the arbiters. Output: `buffer >> shift`, shift 4/0/1/2 for
NR32 codes 0-3. Power on clears the buffer. **CGB wave RAM while CH3 is
on:** reads and writes go to byte `index / 2` at the access clock, at any
time (SameBoy `GB_apu_read`/`GB_apu_write` on CGB; blargg 09, 12); a
retrigger never corrupts wave RAM on CGB (blargg 10). The cached waveform
sum (section 8) is invalidated on wave RAM and NR32 writes.

**Noise** (CH4): 15-bit LFSR, cleared to 0 on trigger; a step writes
`!(b0 ^ b1)` into bit 14, and also bit 6 in 7-bit mode (NR43 bit 3), then
shifts right (Pan Docs' 16-bit description, SameBoy's 15-bit masks
0x4000/0x4040 are the same machine). Output: `volume` if bit 0 is 1. Step
period `(r == 0 ? 8 : 16 * r) << s` clocks (Pan Docs: 262144 / (r * 2^s)
Hz with r = 0 as 0.5); shift 14 and 15 never clock. Trigger: countdown =
period (SameBoy's alignment-dependent start is deferred, section 12).

### 6. PCM12 / PCM34 (FF76/FF77)
`PCM12 = ch1 | ch2 << 4`, `PCM34 = ch3 | ch4 << 4`, each the channel's
current digital output (0-15) if it is on, else 0 (SameBoy `GB_IO_PCM12`/
`PCM34`: `is_active ? sample : 0`). Read through `read_apu` (section 3) at
the access clock. On a CGB in either mode, as today.

### 7. NR52 power on CGB
- **Off** (bit 7 1 → 0): sync to now; `mem.io[0x10..=0x25] = 0`; all `Apu`
  state but `Output` becomes zero, which also clears the length counters
  (CGB: lengths don't survive power off, blargg 08; Pan Docs "Audio
  Registers": DMG keeps them, CGB doesn't); the `DivApu` deadline becomes
  `NEVER`. Wave RAM is untouched (SameBoy: "Wave RAM is not cleared").
- **While off**: writes to FF10-FF25 are ignored on CGB, the NRx1 length
  writes included (SameBoy: only DMG accepts them); NR52 bit 7 and wave
  RAM stay writable. Reads: zeroed registers with their masks; NR52 `0x70`.
- **On** (0 → 1): the zero state already has duty positions 0 and the wave
  buffer 0 (Pan Docs); set the frame-sequencer start (section 4) and the
  `DivApu` deadline; record the 1 MHz phase.
- **Post-boot** (`reset`/`load`): powered on, NR52 reads F1: CH1 on with
  its DAC on, volume 0 and the envelope stopped (the boot chime has decayed
  by hand-over; provisional, fit to Mooneye `boot_hwio-C` and SameBoy's
  post-boot state if a test says otherwise), `fs = 0`, deadline from the
  post-boot counter. The `(0xFF26, 0xF1)` entry in `POST_BOOT_IO` becomes
  `Apu` state.

### 8. Output: rate, integration, mixing
- **Rate:** one stereo sample per 128 base clocks, **32,768 Hz** exactly
  (`AUDIO_SAMPLE_RATE`), the same in both CPU speeds. 548.625 samples per
  frame; 64 per frame-sequencer step. 128 is a power of two, so the window
  normalisation is a shift. The constant is `SAMPLE_PERIOD_LOG2 = 7`
  (crate-internal), the one knob for a later rate change (8 → 16,384 Hz if
  the perf budget demands it; a human-facing quality trade).
- **Filter:** each output sample is the exact average of the mixed signal
  over its 128-clock window (a box filter). The channel outputs are
  piecewise constant, so the average is a sum of `level * duration`
  segments, integer only. That is a crude low-pass (first null at 32 kHz),
  much better than point sampling for the ultrasonic tones some games use
  as DC, and free of floats.
- **Per channel, per segment:** `integrate(dt) -> i32` advances the channel
  and returns the sum of its analog level times clocks. The analog level
  is `15 - 2 * digital` (Pan Docs DAC: digital 0 → analog +1, 15 → -1, so
  -15..15) while the DAC is on, 0 while it is off. Cost is bounded: a
  square skips whole waveform periods in closed form (8 steps of a full
  period always contain the whole duty pattern, so this is exact: one
  division), leaving at most 8 step iterations; the wave channel does the
  same with its cached 32-sample sum (at most 31 iterations, worst at f >=
  2041); noise steps its LFSR (at most 16 steps per window, at r = 0, s = 0).
- **Mixing** (NR51 panning, NR50 volume; VIN ignored, no cartridge audio):
  per side `acc += (sum of the panned channels' integrals) * (volume + 1)`,
  NR50/NR51 read from `mem.io` (a write syncs first, so a mid-window change
  splits the window correctly). At a window end: `sample = acc << 6 >>
  SAMPLE_PERIOD_LOG2` (the extreme 4 * 15 * 8 = 480 maps to 30,720, within
  i16 without clamping), `acc = 0`.
- **No high-pass filter in the core.** The DC-blocking capacitor (Pan Docs
  "Mixer": charge factor 0.998943 per clock on CGB) is the front end's job,
  with resampling: desktop resamples 32,768 → its device rate; the MCU can
  run the SAI at exactly 32,768 Hz (fractional PLL) and needs only the
  HPF. Keeping DC in the core also keeps "silence = constant samples"
  (section 10).
- **Buffer contract** (ADR 0006): samples are written from `audio[0]`, one
  per completed window. When the last free slot completes, the
  `AudioBuffer` event ends the call (`AUDIO_BUFFER_FULL`). Overshoot past
  that (rest of the instruction; a VRAM DMA stall stops at once, see the
  GDMA stall amendment) is less than one window, so it lands in `acc` and
  the next call continues the same window grid: for any non-empty
  buffers, the stream is independent of how the caller chunks it. A call
  of `n` clocks completes at most `(n >> 7) + 2` samples (the carried
  partial window and the overshoot past the target can each complete
  one); size buffers that, 550 for `run_frame`.

### 9. Empty buffer (romtest, benches)
With no room (`audio.len() == audio_len`), `sync` takes the advance-only
path: square and wave positions advance in closed form (one division
each), the noise LFSR steps (bounded per section 3), and nothing is
integrated or mixed. Syncs then happen only at APU writes, 512 Hz
`DivApu` steps and the end of each run call, so the cost is a few hundred
Ir per frame plus LFSR steps when noise plays; register-visible state is
exactly as with a buffer (section 3 invariant). `Output::live` records
whether the last sync integrated; the first sync with room after an
advance-only stretch restarts the window grid at `synced` with `acc = 0`.
That makes the output a deterministic function of the call sequence, not
of time alone; callers that want a continuous stream always pass room.

### 10. The romtest audio boundary
No new core API beyond `AUDIO_SAMPLE_RATE` (section 11). Gambatte's
testrunner runs a fixed number of chunks and passes `audio0` if all
35,112 samples of the last 70,224-clock chunk equal the first one, any
constant value, and `audio1` if any differs (testrunner.cpp; ADR 0018).
romtest does the same with what the core returns: it passes an empty
buffer until the last frame of the run, then a 550-sample buffer, and
judges "silent" as all returned samples equal. With no HPF in the core, a
steady DC level stays constant.

Known blind spot: a 128-clock average of a waveform whose period divides
128 clocks (square at f = 2044, 2046, 2047; wave at f = 2046, 2047) is
constant, so such an ultrasonic tone reads as silent where Gambatte's
2-clock samples would not. The romtest task checks the pinned set once
in a scratch worktree with `SAMPLE_PERIOD_LOG2 = 1` (2-clock windows,
Gambatte's resolution): if every one of the 131 verdicts matches the
32 kHz run, the boundary is sound for the pinned ROMs; any mismatch is
reported, and then a test-only rate option (public API, its own ADR and
review) is the fallback, not a manifest change.

### 11. Public API, budgets, measurement
- `pub const AUDIO_SAMPLE_RATE: u32 = 32_768;` (and the clocks per sample,
  128) is the only public addition: front ends need the rate. ADR 0006 is
  amended in the same commit; AGENTS.md "Independent review" rule 3 (one
  reviewer, that diff). `RunResult::audio_samples`, `AUDIO_BUFFER_FULL` and
  the run methods' `&mut [StereoSample]` are unchanged.
- **Perf budget:** the APU may add at most 200 k x86 Ir/frame to
  `synth:worst-case` once the bench passes audio buffers and drives all
  four channels at their worst periods (estimate: about 160 k, ~290 Ir per
  window); a typical game mix should be under 70 k. Over budget → measured
  options in order: noise multi-step (14 LFSR steps per iteration in 15-bit
  mode), mean level for ultrasonic wave periods, then 16,384 Hz (human).
- **Code size:** the first task builds the framework and CH2 as two
  commits and measures `.text` after each (framework F, square S). Full APU
  estimate: `F + S` (CH2) `+ ~0.3 S` (sweep; CH1 reuses `Square`) `+ ~1.0 S`
  (wave) `+ ~1.0 S` (noise) `+` the bus integration already in F. That
  number, plus a margin, goes to the human as the new total budget before
  anything merges to `main`.

### 12. Deferred precision (stretch tier, tracked in the plan)
Exactness that no M6 acceptance ROM needs, each a later task driven by
gambatte `sound/` and SameSuite APU ROMs: SameBoy's envelope lock/clock
model with the secondary DIV event and its CGB-C `_nrx2_glitch`; the
square trigger delay fit (`lf_div`, gambatte `ch1_duty*_pos*`); SameBoy's
noise counter phase at a trigger (alignment-dependent start; the counter
itself and NR43 writes are modelled, amendment "CH4 NR43 writes"); the
NR43 write glitch steps; the sweep calculation delay; the DAC fade.

## Consequences
- The APU costs nothing per M-cycle: 512 Hz `DivApu` events plus output
  work proportional to samples written, and almost nothing with an empty
  buffer, so romtest results and the existing bench numbers move only by
  the frame-sequencer events (a rebaseline in the first task).
- Register reads stay pure; the price is that wave RAM/PCM reads advance a
  copy of a channel (cold, bounded). Correctness depends on every APU
  state change going through `write_apu` or a `DivApu` step; the split-sync
  unit test and the audio/no-audio fingerprint check are the safety net.
- `Memory` gains ~80 B; `Hw` stays at 832 B under its 864 B cap.
- 32,768 Hz and a box filter are a quality floor, not hi-fi; band-limited
  synthesis would cost more code and cycles than the M7 budget allows now.
- Front ends own resampling and the DC-blocking HPF (M7 desktop, Stage 2
  MCU). The hardware repo's `docs/firmware-interface.md` says the sample
  rate is "decided in firmware M6": this ADR is that decision from the
  core side.
- Classic (Pan Docs/blargg) envelope, sweep and noise timing pass the
  register-level tests; the gambatte audio ROMs may need the SameBoy-exact
  models of section 12.

## Amendments

### 2026-11-22: framework implementation notes (M6 task 1(a))
Clarifications made while implementing; the decisions stand.
- **"Room" means the call's buffer is non-empty.** Section 9's advance-only
  test `audio.len() == audio_len` would also fire in the overshoot after
  `AUDIO_BUFFER_FULL` (buffer full, rest of the instruction), which
  section 8 wants integrated into `acc` so the next call continues the
  same window grid. So `Apu::sync` integrates whenever the run call was
  given a non-empty buffer and takes the advance-only path only for an
  empty one; a window that completes with no slot left (not expected:
  the overshoot is under one window) is dropped.
- **Call start.** `Ctx::new` brings the APU to the clock if a caller left
  it behind (advance only; normally a no-op after the end-of-call sync),
  then with room restarts the grid if `!live` and sets the
  `AudioBuffer` deadline, or `NEVER` without room, so a stale deadline
  never survives a call.
- `mem.io[0x26]` is no longer written: NR52 reads come from `Apu` only.
- `Fs::phase` stores `(clock >> 1) & 1` at power on (SameBoy's 1 MHz
  `lf_div` phase); task 1(b) fits its use.
- Unit tests whose precondition is "nothing scheduled" (the idle and
  halted-skip countdown tests) power the APU off first: post-boot it is
  on, so `DivApu` fires every 8,192 clocks.

### 2026-11-22: CH2 implementation notes (M6 task 1(b))
- **Duty masks.** Section 5's literals (`0000_0001` ...) list positions 0-7
  left to right, as SameBoy's `duties` does; as Rust masks with bit i =
  position i they are `0x80, 0x81, 0xE1, 0x7E` (unit-tested against the
  SameBoy sequences).
- The trigger countdown conversion is in `docs/reference/apu.md`;
  `lf_div` = `((now >> 1) ^ phase ^ 1) & 1` (SameBoy initialises it to 1).
- NR52 bit 1 is `sq[1].on`; `Fs::active` keeps the bits of the channels
  not yet implemented (CH1 post-boot) until their tasks.
- The envelope timer loads 8 for a period of 0 (it never counts while the
  period is 0; an NRx2 write giving a period then counts from 8).
- Zombie mode's `16 - volume` wraps (volume 17 gives 15): Pan Docs keeps
  the low 4 bits after all the operations.

### 2026-11-22: GDMA stall across the buffer deadline (M6 review)
A review found that a GDMA's CPU stall (`1 + 32 * blocks` dots, up to
1,025 M-cycles at single speed or 2,049 at double speed: about 4,100
clocks for 128 blocks) ran in one `idle_mcycles` inside `vram_dma`. If the
`AudioBuffer` deadline fell inside it, `AUDIO_BUFFER_FULL` was raised but
the call could not end until the stall did, and every window completed
meanwhile found no slot and was dropped: up to 32 samples (a click), and
output that depended on the caller's buffer size.

**Decision: the stall is resumable.** `VramDma::stall` holds the stall's
M-cycles left (a `u16` in the struct's padding: `Hw` doesn't grow).
`vram_dma` copies the blocks and sets it; `Ctx::vdma_stall` runs it a
deadline at a time and stops at the first deadline that raises a stop
event or reaches the call's target (a single `step`: stop events only).
The run loop resumes a non-zero stall before anything else, and ends the
call if it stopped early. So FULL ends the call within an M-cycle of the
last slot's window, nothing is dropped, and the stream and state are the
same however the run is split (tests
`a_gdma_stall_across_the_buffer_deadline_drops_no_sample` and
`samples_and_state_are_independent_of_call_and_buffer_splits`, both seen
failing under mutation).

Why not hold pending samples instead: up to 32 samples (128 B) do not fit
`Memory::apu`'s 128 B cap beside the APU state, and the call would still
overshoot its target and FRAME_READY by up to 4,100 clocks (the PPU drawing
the next frame's first lines into the buffer the caller is about to show).
The resumable stall fixes all three, costs one branch per `cpu.run` return
in the run loop (not per instruction), and is invisible to the emulated
program, since the CPU is between instructions. The cost is the API
contract: a call may now end inside a VRAM DMA stall, on an M-cycle
boundary, instead of only on instruction boundaries (ADR 0006 amendment,
`run_until`/`step` docs). `set_buttons_at`'s "between calls the clock is
on an M-cycle boundary" still holds.

### 2026-11-22: CH3 implementation notes (M6 task 4)
- **Countdown convention** verified in SameBoy: the trigger's
  `(f ^ 0x7FF) + 3` ticks are `(2051 - f) * 2` base clocks, as section 5
  assumed; no `lf_div` term (docs/reference/apu.md). SameBoy's retrigger
  one tick before a step loads wave RAM byte 0 into the buffer; modelled.
- **The waveform sum is eager, not invalidated.** `Wave::sum` is
  recomputed at every NR30-NR34 or wave RAM write (`apu::write_wave`,
  cold), instead of a lazy cache filled by `integrate`: a lazy fill would
  make the state depend on whether a buffer was passed, against the
  section 3 invariant. The zero state stays exact because `Wave` stores
  the NR32 code (0 = mute, sum 0), not the shift.
- CH3 register and wave RAM writes go from `Ctx::write_apu` to
  `apu::write_wave` (out of line; it needs `Memory::io` for wave RAM),
  not through `Apu::write`. The sync methods take `io` for the same reason.

### 2026-11-22: CH4 implementation notes (M6 task 5)
- **Units verified** in SameBoy: the period `(r ? 16r : 8) << s` is in
  base clocks (docs/reference/apu.md). The trigger loads one full period;
  SameBoy's counter-phase start stays deferred (section 12).
- **The LFSR advances k steps per word operation** (k <= 14, or 6 in
  7-bit mode), and the high time over a span is the popcount of the
  original bits 1..k, so `integrate` and `advance` share one stepping
  function and the LFSR state does not depend on whether a buffer was
  passed (section 3). Worst case (r = 0, s = 0): 1024 steps per
  frame-sequencer span, 74 word operations (171 in 7-bit mode). Spans
  past one LFSR cycle reduce modulo 32,767 (127 in 7-bit mode after 8
  steps), exactly.
- `Fs::active` is gone: NR52 bit 3 is `Noise::on`. NR41-NR44 go through
  `Apu::write` to its out-of-line tail `write_rest` (formerly
  `write_sweep`), which keeps `Apu::write` within its code-size budget.

## Amendment: APU perf budget 230k (2026-11-22, human)

The 200 k Ir/frame budget above was this ADR's estimate. With all four
channels at their worst periods, the first measurement was 682.6 k. Exact
(sample-identical) optimizations brought it to 224.1 k: closed-form square
and wave windows, batched mixing, and doubling-identity noise stepping
(docs/perf.md "APU cost"). `synth:worst-case` is 3.78 M Ir/frame, 94.0% of
the 4.02 M line. The human chose to accept this result and set the APU
budget to **230 k Ir/frame**. The output-changing fallbacks (mean level for
ultrasonic wave periods, 16,384 Hz) are not used.

The APU is still **over its original budget** (224.1 k vs 200 k): it is a
recorded optimization target for later if needed (plan, M8 task "APU cost
is over its original estimate").

### 2026-11-22: NR52 power-on skip, two edges (gate fix)

The DIV-APU section's power-on skip was one edge. SameBoy
(`GB_apu_div_event`) skips the first edge (`GB_SKIP_DIV_EVENT_SKIP`), and
on the second (`GB_SKIP_DIV_EVENT_SKIPPED`) clocks the units at step 1
again without advancing; `Fs::skip` now counts the two edges. The APU-bit
test samples the counter at the end of the NR52 write's M-cycle
(`sc(now) + 4`; SameBoy `cycle_write` advances the pending cycles before
the write). Together with the CGB-mode post-boot counter (ADR 0022) this
fixes the 5 gambatte `ch2_*nr52*` regressions against the M5 baseline.

### 2026-11-22: CH4 NR43 writes (M6 hardware review, medium)

An NR43 write used to keep the running step countdown, so a new shift or
divisor applied only when the old countdown ran out: up to `112 << 15`
clocks (~0.9 s) after shift 14/15, e.g. NR43 = F7 then 0x11 with no
retrigger. `Noise` now carries SameBoy's free-running 14-bit divisor
counter (`GB_apu_run` noise path, `GB_apu_write` NR43), implied between
steps so the hot path is unchanged: `counter` is its value right after
the increment at `countdown`, the earlier increments are every divisor
clocks back from there (at most `pre` of them), and `integrate`/`advance`
add `2^(s+1)` per step. An NR43 write (`Noise::rebase`) keeps the counter
and the divisor countdown (the new divisor loads only if the write lands
on an increment, SameBoy `countdown_reloaded`) and puts the next step at
the first increment that raises the new bit `s`: within the old divisor's
rest (at most 56 clocks) plus one new period. This supersedes the NR43
half of section 5's "next reload" rule. A trigger keeps the counter's
bits above `s` and loads one full period, as before. Remaining
differences from SameBoy (docs/reference/apu.md): the trigger phase and
its alignment terms, the alignment term on an NR43 reload, the counter
not running while the channel is off, and the revision-specific NR43
glitch steps (`nr43_write`, including CGB-C's intermediate 0xFF write).

## Amendment: split APU perf budget (2026-11-22, human)

The M6 perf review found that the per-sync fixed cost scales with the APU
register write rate, and the 230 k budget did not cover it (`synth:worst-case`
made no APU writes). `synth:apu-writes` (b78ed12) measures 884.6 x86 Ir per
write after 117461b. `synth:worst-case` now toggles NR50 about 146 times a
frame. That brings it to 3.908 M Ir/frame (97.2% of the 4.02 M line), with
an APU share of about 353 k. The human split the budget into two parts:
**230 k Ir/frame for channels and mixing** (measured without register
writes) and **<= 950 Ir per APU register write**. The 4.02 M worst-case
line stays the overall guard. Both parts remain optimization targets (plan
M8).

### 2026-11-23: noise trigger phase and the sweep's delayed overflow check (M9 group 15)

Two section 12 items are now modelled (docs/reference/apu.md, Sweep and
noise "Trigger phase"). **Sweep:** the overflow check after a write-back
runs `shift` 1 MHz ticks after the step, one `DivApu` event per tick
(`Sweep::check`), so the bus is unchanged (SameBoy's countdown, on every
revision; SameSuite `channel_1_sweep`). **Noise:** a trigger keeps the
free-running counter and loads SameBoy's `prepare_noise_start` countdown
with its alignment terms (`Noise::trigger_delay`); the alignment comes
from `Fs::phase`, now 2 bits. Where SameBoy's terms differ by revision,
the CGB-C branches are used (ADR 0003): this keeps SameSuite
`channel_4_align` failing (it expects CGB E's double-speed term) and is
the one choice here that a CGB E fit would reverse. Not modelled: the
background counting while CH4 is off and the terms that depend on it.

### 2026-11-24: output headroom is part of the contract (M10 review A3-5)

Section 8 says the stream is ±30,720 with DC kept and that the front end
high-passes it. It did not say what that costs downstream. From the mixer
(`apu/mix.rs`): a channel's level is -15..=15 while its DAC is on, a
side's weight is `NR50 volume + 1` (1-8), and a sample is the window's
average times 64, so a side lies in `-M..=M` with
`M = 64 * 15 * n * (v + 1) = 960 * n * (v + 1)` for `n` routed channels
with DACs on; `M <= 960 * 4 * 8 = 30,720`. Both ends are reached: four
idle DACs give a constant +30,720 (digital 0 is the positive extreme) and
four channels at digital 15 give -30,720.

A DC blocker `y[n] = x[n] - x[n-1] + a * y[n-1]` (0 <= a < 1) is
`x[n] - (1 - a) * sum(a^k * x[n-1-k])`: the input less a weighted average
of earlier inputs (weights summing to at most 1; a zero start state is
inside the range). So `|y| <= max(x) - min(x) <= 2 * M`: **±61,440, twice
the core's nominal full scale**, and the bound is tight (a mix resting at
+30,720 that drops to -30,720 within a sample gives -61,440 less the
filter's residue). 61,440 needs 17 bits signed.

Consequences, now stated next to `StereoSample`:
- the high-pass must run in a wider type than `i16` (`i32` or floats);
  an `i16` filter wraps;
- after it, a fixed halving (-6.02 dB) maps the bound back to ±30,720, or
  the front end saturates; desktop already does `0.5 / 30,720` in `f32`
  with a clamp (`gbc-desktop/src/audio.rs`);
- a resampler with negative coefficients adds its own overshoot on top.

The decision itself (no high-pass in the core, DC kept) is unchanged.
`tests/apu_output.rs` (`after_a_dc_blocker_the_signal_reaches_twice_full_scale`)
shows a stream that spans both ends and, through Pan Docs' CGB capacitor
(0.998943 per clock), goes below -50,000. A shared, tested integer DC
blocker for Stage 2 (the review's other proposal) is not part of this
amendment.

### 2026-11-24: NR50/NR51 writes list spans instead of syncing the channels (M10 B12)

Section 3 has every APU register write sync to the access clock, and
section 8 has the mixer read NR50/NR51 from `mem.io` because "a write
syncs first". The invariant behind both is narrower than a full sync: a
channel must be exact at the clock where *its* state changes, and the
output needs every stretch mixed with the weights that were in force
over it. An NR50/NR51 write changes no channel state. So `Apu` now keeps
a list of the spans not mixed yet (`mix::Spans`: cut at window ends and
at mixer writes, at most 16, each with the weights in force), and
`Apu::sync_write` only extends that list for a write to FF24/FF25,
noting the new weights for the spans that follow. The channels integrate
the whole list in one call each when it is full or when anything else
syncs (`Apu::sync`: channel and wave RAM writes, NR52, `DivApu` events,
the audio-buffer event, the end of the run call), and the mixer applies
the weights per run of spans. `Output::synced` is still the clock the
output is listed to; the channels are at the list's start, and the list
is empty between run calls and whenever no buffer is passed. `read_apu`
advances its channel copies by `now - synced` plus the clocks listed.

Exact: a window's sum is linear in its spans and the channels' closed
forms do not depend on where the spans are cut (the section 3
invariant); `bench --fingerprint --audio-buffer {7,64} --hash-samples`
is identical to before. Numbers in `docs/perf.md`, "APU cost".
