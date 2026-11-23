# APU notes

Distilled hardware rules as implemented in `crates/gbc-core/src/apu/`, with
sources. The design (state, sync points, output pipeline) is ADR 0021;
`docs/architecture.md` has the module map. Sources: Pan Docs "Audio",
"Audio Registers", "Audio details" (gbdev.io/pandocs); SameBoy
`Core/apu.c` (MIT; function names below are its); blargg `cgb_sound`;
gambatte `sound/` ROMs (rules only, Gambatte is GPL). Target is CGB: DMG
and AGB differences are noted only where the code deliberately differs.

## Conventions

- All periods are in base clocks (4,194,304 Hz), the same in both CPU
  speeds; only the frame sequencer follows double speed.
- SameBoy counts in 2 MHz ticks (2 base clocks). Its countdown conventions
  differ per channel, so each conversion below is explicit:
  square and wave `sample_countdown` n fires after **n + 1** ticks; the
  noise divisor countdown n fires after **n** ticks.
- A frequency or NR43 write changes the period used at the next reload,
  not the running countdown (SameBoy keeps `sample_countdown`).
- State is a function of the clock and the register writes only; register
  reads are pure (ADR 0021 section 3).

## Frame sequencer and DIV

DIV-APU is the falling edge of DIV bit 4 (bit 5 in double speed): system
counter bit 12 or 13 (Pan Docs "Audio details", DIV-APU; SameBoy
`apu_bit`). `fs` counts steps since power on, mod 8 (SameBoy
`div_divider`). A step: during the power-on skip (below), see there; else
`fs = (fs + 1) & 7`, then length on `fs & 1 == 1` (256 Hz), sweep on `fs & 3 == 3`
(128 Hz), envelopes on `fs == 0` (64 Hz; SameBoy decrements at `fs == 7`
and applies at the next event, the same step). Length runs before the sweep.

- A DIV write or STOP that clears a set APU bit is a falling edge: the
  frame sequencer steps at once (SameBoy `GB_set_internal_div_counter`).
- A speed switch changes the bit 12 <-> 13 and reschedules.
- NR52 power on: `fs = 0`; if the APU bit is 1 at the end of the write's
  M-cycle (SameBoy `cycle_write` runs the pending cycles first), `fs = 1`
  and the first edge is skipped (it comes too soon); the second clocks the
  units at `fs = 1` again without stepping, so the third steps to 2
  (SameBoy `GB_apu_div_event`: `GB_SKIP_DIV_EVENT_SKIP`, then `_SKIPPED`;
  gambatte `sound/ch2_*nr52*`, CGB; ADR 0021 note 2026-11-22).
- "The next step doesn't clock length" (Pan Docs "Obscure behavior") is
  `fs & 1 == 1`.
- The rising-edge `GB_apu_div_secondary_event` is not modelled.

## Shared units

- **Length** (max 64; 256 for CH3): an NRx1 write loads `max - value`.
  Clocked when enabled; at 0 the channel turns off. Enabling length (NRx4
  bit 6 0 -> 1) while `fs & 1 == 1` with a non-zero counter clocks it once
  more; reaching 0 turns the channel off unless the write also triggers. A
  trigger with the counter at 0 loads `max`, or `max - 1` if length is
  enabled and `fs & 1 == 1`.
- **Envelope** (classic model): trigger loads volume = NRx2 >> 4 and the
  timer from the period (0 loads 8; it never counts while the period is
  0). On a step with a non-zero period the timer counts down, reloads at 0
  and moves the volume one step in the NRx2 direction until 0 or 15, where
  it stops. **Zombie mode** (an NRx2 write while on; Pan Docs CGB-02/04):
  if the old period was 0 and the envelope still runs, volume + 1; else if
  the old direction was decrease, volume + 2; then if the direction
  changed, volume = 16 - volume; keep the low 4 bits (wraps: 17 gives 15).
- **DAC**: on when `NRx2 & 0xF8 != 0` (CH3: NR30 bit 7). A DAC-off write
  turns the channel off; a trigger with the DAC off leaves it off (the
  length reload still happens). Analog level is `15 - 2 * digital` (Pan
  Docs: digital 0 -> +1, 15 -> -1; -15..15) while the DAC is on, else 0.
- **Sweep** (CH1; blargg 04-06): trigger copies the frequency to `shadow`,
  loads the timer (0 counts as 8), `enabled = period != 0 || shift != 0`,
  clears `negate_used`, and if `shift != 0` runs one calculation (overflow
  turns the channel off). On sweep steps the timer counts down; at 0 it
  reloads and, if enabled and the period is non-zero, `new = calc()`; if
  `new <= 2047` and `shift != 0` it is stored as shadow and frequency and
  `calc()` runs again for the overflow check only. `calc()` is `shadow +-
  (shadow >> shift)`; subtraction sets `negate_used`; over 2047 turns the
  channel off. **Negate quirk:** an NR10 write clearing the negate bit
  after `negate_used` turns the channel off (SameBoy: always, on CGB C and
  older).

## Square channel trigger countdown (SameBoy conversion)

SameBoy (`Core/apu.c`, MIT) counts the square channels in 2 MHz ticks
(2 base clocks). In `GB_apu_run`, a `sample_countdown` of n fires after
n + 1 ticks: the loop steps while `cycles_left > sample_countdown` and
subtracts `sample_countdown + 1`. The reload `(f ^ 0x7FF) * 2 + 1` therefore
gives `(2048 - f) * 2` ticks = `(2048 - f) * 4` base clocks per duty step,
which is the Pan Docs period.

The trigger (`GB_apu_write`, NRx4 bit 7) loads
`(f ^ 0x7FF) * 2 + delay`, `delay = 6 - lf_div` if the channel was off and
`4 - lf_div` if it was on. In base clocks, to the first duty step:

| channel before the trigger | base clocks |
|---|---|
| off | `(2048 - f) * 4 + 10 - 2 * lf_div` |
| on  | `(2048 - f) * 4 + 6 - 2 * lf_div` |

`lf_div` is SameBoy's 1 MHz phase: 1 at power on (`GB_apu_init`), toggling
every tick. The core derives it as `((now >> 1) ^ phase ^ 1) & 1`, with
`phase = (power-on clock >> 1) & 1` (`Apu::lf_div`). A trigger at an odd
base clock is not modelled (half a tick); the exact fit, and SameBoy's
extra delay when retriggering an active channel, are deferred (ADR 0021
section 12, gambatte `ch*_duty*_pos*`).

Until its first duty step a newly started channel outputs digital 0 (Pan
Docs: "On first startup..."; SameBoy sets bit 7 of `current_sample_index`).
A retrigger of an active channel keeps its output.

## Duty patterns

SameBoy's `duties` lists positions 0-7 left to right: 12.5% `00000001`,
25% `10000001`, 50% `10000111`, 75% `01111110`. As masks with bit i =
position i: `0x80`, `0x81`, `0xE1`, `0x7E` (`square.rs` `DUTY`).

## Wave channel (CH3) timing and CGB wave RAM access

SameBoy (`Core/apu.c`, MIT) counts CH3 in the same 2 MHz ticks as the
squares, with the same convention (a `sample_countdown` of n fires after
n + 1 ticks: `GB_apu_run` steps while `cycles_left > sample_countdown`).
The step reload `f ^ 0x7FF` gives `2048 - f` ticks = `(2048 - f) * 2` base
clocks per sample (Pan Docs). The trigger (`GB_apu_write` NR34) loads
`(f ^ 0x7FF) + 3`: the first step comes `(2051 - f) * 2` base clocks after
the trigger, with no `lf_div` term and no on/off difference.

- Trigger: index 0, the sample buffer (`current_sample_byte`) kept, the
  output the buffer's high nibble at once ("we don't change the sample
  just yet! This was verified on hardware"). Exception: a retrigger of an
  active channel with `sample_countdown == 0` (one tick, 2 clocks, before
  a step) loads wave RAM byte 0 into the buffer. The DMG wave RAM
  corruption on retrigger is `!GB_is_cgb` only.
- Each step: index + 1 (mod 32), buffer = wave RAM byte `index / 2`,
  output its high nibble for even indices, low for odd, `>> {4, 0, 1,
  2}[NR32 code]`.
- CGB wave RAM while CH3 is on: reads and writes reach byte `index / 2`
  at any time (`GB_apu_read`/`GB_apu_write`; DMG only within the read
  window, AGB reads FF). A write does not update the buffer.
- Pinned arbiters (all pass): blargg `cgb_sound` 09, 10, 12; gambatte
  `sound/ch3_*` (26 ROMs); same-suite `apu/channel_3/*` gained 13 passes.

## Noise channel (CH4) timing and the LFSR

SameBoy (`Core/apu.c`, MIT) runs a 14-bit counter (`noise_channel.counter`,
`& 0x3FFF`) clocked every `divisor` 2 MHz ticks, `divisor = r * 4` (NR43
bits 0-2; `r = 0` gives 2). The LFSR steps on each rising edge of counter
bit `s` (NR43 bits 4-7, mask `1 << s`), i.e. every `2^(s+1)` counter
clocks. Per LFSR step: `divisor * 2^(s+1)` ticks, times 2 base clocks =
`(r == 0 ? 8 : 16 * r) << s` base clocks, which is Pan Docs' 262,144 /
(r * 2^s) Hz with r = 0 as 0.5. Shortest: 8 clocks (r = 0, s = 0).

- **Countdown convention:** the divisor countdown fires when
  `cycles_left >= counter_countdown` (n fires after n ticks, unlike the
  square/wave `>` with n + 1). The core counts the LFSR step period
  directly in base clocks: `Noise::countdown` = clocks to the next step,
  reloaded with the period at each step; a trigger loads one full period.
  SameBoy's trigger loads `r * 4 + 6` ticks (r = 0: 6) plus
  alignment-dependent corrections into the divisor countdown, and the
  free-running counter's phase decides the first edge; that start, and
  NR43 writes landing on a counter reload, are deferred (ADR 0021
  section 12). An NR43 write takes effect at the next step.
- **Shift 14 and 15:** counter bits 14 and 15 never rise (14-bit counter),
  so the LFSR is frozen; the output holds bit 0.
- **LFSR:** 15 bits, 0 on trigger. A step computes `fb = !(b0 ^ b1)`,
  shifts right, then sets bit 14 to `fb` (and bit 6 in 7-bit mode, NR43
  bit 3; masks 0x4000/0x4040). Output: the envelope volume while bit 0 is
  1, else 0 (`current_lfsr_sample`). From 0, bit 0 first becomes 1 after
  15 steps (7 in 7-bit mode).
- **Word-parallel steps** (`noise.rs` `lfsr_steps`): the first k steps
  read only original bits while `k <= 14` (`k <= 6` in 7-bit mode), so k
  steps are one word operation and the outputs after steps 1..k are the
  original bits 1..k. Long spans reduce exactly: the 15-bit step is a
  bijection with cycles of 32,767 or 1 (0x7FFF); in 7-bit mode bit 7 is
  overwritten, so after 8 steps the state is a function of bits 0-6
  (cycles of 127 or 1).
- **CGB power off** zeroes NR41 and the length counter with everything
  else (blargg `cgb_sound` 11: "Powering off should clear NR41"); NR41
  writes while off are ignored on CGB, as for the other channels.

## PCM12 / PCM34 (FF76/FF77)

`PCM12 = ch1 | ch2 << 4`, `PCM34 = ch3 | ch4 << 4`: each channel's current
digital output (0-15) if it is on, else 0 (SameBoy `GB_IO_PCM12`/`PCM34`:
`is_active ? sample : 0`). Read at the access clock by advancing a copy of
the channel (`read_apu`). A new square channel reads 0 until its first
duty step; a new wave channel reads the kept buffer's output at once.

## NR52 and CGB power off

- NR52 reads `0x70 | power << 7 | channel-on bits` (Pan Docs FF26). Only
  bit 7 is writable. Channel bits change only at writes and frame-sequencer
  steps.
- **Off** (bit 7 1 -> 0): FF10-FF25 are zeroed and all channel state
  becomes zero, which on CGB includes the length counters (Pan Docs: DMG
  keeps them; blargg `cgb_sound` 08, 11). Wave RAM is kept (SameBoy "Wave
  RAM is not cleared"). The DIV-APU deadline is `NEVER`. The output
  stream (window position, accumulators) continues.
- **While off**: writes to FF10-FF25 are ignored on CGB, the NRx1 length
  writes included (SameBoy `GB_apu_write`: only DMG accepts them); NR52
  bit 7 and wave RAM stay writable. Reads give the zeroed registers with
  their read masks (`SOUND_READ_MASK`; SameBoy `read_mask`).
- **On** (0 -> 1): duty positions 0, wave buffer 0 (Pan Docs); frame
  sequencer start as above; the 1 MHz phase is recorded from the clock.
- **Post-boot**: powered on, NR52 reads F1: CH1 on, DAC on, volume 0,
  envelope stopped (the boot chime has decayed by hand-over; provisional,
  frequency/length/duty position not observable and left 0).

## Wave RAM access while CH3 is on (CGB)

Reads and writes reach byte `index / 2` at the access clock at any time
(SameBoy `GB_apu_read`/`GB_apu_write`; blargg `cgb_sound` 09, 12); DMG
allows only a read window and AGB reads FF. A write does not update the
sample buffer. A retrigger never corrupts wave RAM on CGB (blargg 10; the
DMG corruption is `!GB_is_cgb`). Wave RAM and NR30-NR34 writes recompute
the cached waveform sum eagerly (not lazily, so state does not depend on
whether a buffer was passed).

## Output (not hardware)

Mixing: per side, the panned channels' analog levels (NR51) times NR50
volume + 1; VIN is ignored. One sample per 128 clocks, the exact average
over the window, scaled so full scale is +-30,720. No DC blocking: Pan
Docs "Mixer" describes a high-pass capacitor (charge factor 0.998943 per
clock on CGB) that the front end applies. See ADR 0021 section 8.

## Not modelled / deferred (ADR 0021 section 12)

Exactness no M6 acceptance ROM needs, tracked in the plan for the
gambatte `sound/` and SameSuite APU ROMs:

- SameBoy's envelope lock/clock model, the secondary DIV event and the
  CGB-C `_nrx2_glitch` (the classic model plus zombie mode is used).
- The square trigger delay fit: a trigger at an odd base clock (half a
  tick), the extra delay when retriggering an active channel (gambatte
  `ch*_duty*_pos*`).
- The noise counter's phase at trigger (SameBoy loads `r * 4 + 6` plus
  alignment corrections; the core loads one full period) and NR43 writes
  that land on a counter reload.
- The sweep calculation delay and the DAC fade.
- DMG and AGB behaviours (wave RAM corruption, length counters surviving
  power off, the DMG read window).
