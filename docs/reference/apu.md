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
- A frequency write changes the period used at the next reload, not the
  running countdown (SameBoy keeps `sample_countdown`). NR43 writes
  follow the noise divisor counter instead (noise section). Exception:
  the square reload takes the **high frequency bits up to 4 clocks after
  the duty step**, so an NRx4 write 0-4 clocks after a step sets the
  period running from that step (the position has already moved);
  `Square::write_nrx4`. A trigger's countdown counts as reloaded one period
  before its first step (no ROM tests that case). The window is measured
  in the period the countdown was loaded with (`Square::reload`), not in
  `freq`: an NRx3 write or a sweep write-back since the step has already
  changed `freq`, and games write NRx3 then NRx4 (M10 review A3-3). Source: SameSuite
  `channel_1_freq_change_timing-*` (f 0x7FC -> 0x0FC written 22-29 M-cycles
  after a trigger, read back 16 M-cycles later): the final position
  matches CGB C in both speeds and D/E/AGB in single speed only with this
  window (writes 3 clocks after the step in single speed and 4 in double
  speed count, 6 and 7 don't). SameBoy's NRx4 write likewise re-times a
  "just reloaded" countdown and steps the index back for 7 -> <7 writes
  (its TODO calls it a hack for its write timing).
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

- **Sweep lag:** a sweep step that ends the sweep timer lands
  `SWEEP_LAG` = 4 clocks after its edge (both speeds): `Apu::step` leaves
  it pending (`Sweep::due`) and the `DivApu` event runs it then
  (`Ctx::set_apu_deadline`). An access at the edge's clock is before it:
  an NR52 read still sees CH1 on, an NR10 write still changes the period
  it uses (gambatte cgb04c `ch1_init_reset_sweep_counter_timing_nr52_3`
  vs `_4`, `_7` vs `_9`; gambatte `SweepUnit` has its events 2 of its
  2 MHz units after the 128 Hz boundary, length on it). Steps that only
  count down stay at the edge: the same either way (a trigger inside the
  lag reloads the timer), and one sync per sweep period instead of two
  per 32,768 clocks. A DIV write or speed switch inside the lag re-arms
  the step 4 clocks after itself.
- **Sweep trigger window:** a CH1 trigger up to 8 clocks before the edge
  into step 3 or 7 (or inside the lag) doesn't count that step: one more
  step to the first calculation (gambatte cgb04c `_timing_7`, 12 clocks
  before: counts; `_10`, 8 before: doesn't; gambatte `SweepUnit::nr4Init`
  `(cc + 2 + 2 * cgb) >> 14`; 4 clocks on DMG). SameBoy has no window.
- **Envelope trigger window:** a CH1, CH2 or CH4 trigger from 4 clocks
  before the edge into step 7 to 4 clocks before the one into step 0
  adds one envelope step (`Envelope::delay`): step 0 doesn't count
  (gambatte cgb04c `ch2_init_reset_env_counter_timing_13`/`_14`, 8 clocks
  before: counts; `_15`/`_16`, 4 before: doesn't; gambatte
  `EnvelopeUnit::nr4Init`, `(cc + 2) & 0x7000 == 0`; SameBoy's countdowns
  tick at step 7, the same step). blargg's `cgb_sound`/`dmg_sound` and
  `mem_timing` trigger in step 7 (traced) and still pass.

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
  `calc()` runs again for the overflow check only, `shift` 1 MHz ticks
  (4 base clocks each) after the step: until then CH1 plays on at the new
  frequency (`Sweep::check`, one `DivApu` event per tick; SameBoy
  `trigger_sweep_calculation` counts `NR10 & 7` down before
  `sweep_calculation_done`, on every revision; SameSuite `channel_1_sweep`,
  shift 7: on 7 M-cycles after the step, off 8 after). A check whose shift
  was written to 0 meanwhile is dropped (SameBoy pauses it). A CH1 trigger
  inside the window cancels the pending check (`Sweep::trigger`; the tick
  already scheduled runs as a step that only counts down): SameBoy's trigger reloads the countdown and the
  addend for its own check, so the old one never judges the new note with
  a later NR10 (M10 review A3-6). `calc()` is `shadow +-
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
`phase = (power-on clock >> 1) & 1` (`Apu::lf_div`). The core uses neither
row as is: single speed uses the 1 MHz grid rule and double speed
gambatte's linear rule (both below). SameBoy's extra delay when
retriggering an active channel is deferred (ADR 0021 section 12, gambatte
`ch*_duty*_pos*`).

**Single speed: the first step lands on the APU's 1 MHz grid.** The core
puts the first duty step after a single-speed trigger at the first clock
`1 mod 4` that is at least one period plus 5 clocks after the write (off
channel), or plus 1 (on channel: 4 fewer, as in both references):
`(2048 - f) * 4 + 5 + (-now mod 4)` and `+ 1 + (-now mod 4)`
(`Apu::trigger_delay`). Before any speed switch the CPU writes on clocks
`0 mod 4`, so this is `+ 5` (off) and `+ 1` (on): 3 clocks earlier than the
table with `lf_div` = 1, which single speed always had. Source: gambatte's
cgb04c `speedchange{,2,3,4,5}{,_nop}_ch1_duty0_pos6_to_pos7_timing{,_nop}_{1,2}`
(trigger CH1 at f = 0x7E0 in single speed, switch speed one to five times,
then retrigger to freeze the position: `_1` must still read position 6,
`_2` position 7). Traced in this core, each `_1`/`_2` pair brackets the
step into position 7: one switch (and four, five) allow it 2 or 3 clocks
before the table's step, two and three switches (whose CPU then writes on
odd clocks, ADR 0020) 3 or 4. Only 3 fits all five, a step on a clock `1
mod 4`; with all periods multiples of 4 the channel stays on that grid
through any number of switches. The single-speed `sound/ch1_duty0_pos6_
to_pos7_timing_{1,2}` pair allows 0-3 clocks, so it agrees, and so do the
`ch1_init_pos_*` window (post-boot, below) and SameSuite (its channel
1/2 tests pass unchanged). The `+ (-now mod 4)` term only matters for a
single-speed trigger after a switch to single speed (odd CPU grid), which
no ROM here tests: it keeps the step on the same grid (gambatte's
`DutyUnit::nr4Change` likewise snaps the first step to its counter's
parity). This is gambatte's linear rule in shape (`+ 6 + 2 * lf_div`,
below) with the grid phase fitted to the ROMs.

**Double speed: gambatte's CGB-C linear rule.** In double speed `lf_div`
alternates per M-cycle, and the sign of the `lf_div` term differs by
revision (SameBoy `GB_apu_write`: `6 + lf_div` for an off channel on CGB C
and older in double speed, `6 - lf_div` otherwise). The core follows
gambatte's linear rule (`DutyUnit::nr4Change`, behaviour only: first step
`(2048 - f) * 2 + 3 + lf_div` ticks after an off-channel trigger, 2 fewer
for an on channel): in base clocks one period plus `6 + 2 * lf_div` (off)
or `2 + 2 * lf_div` (on) (`Apu::trigger_delay`). Source: gambatte's cgb04c
`ch1_duty0_pos6_to_pos7_timing_ds_6` and the five
`speedchange*_ch1_duty0_pos6_to_pos7_timing*ds_2`, which it passes.
SameSuite's `channel_{1,2}_align{,_cpu}` expect SameBoy's CGB-E sign and
fail; its APU README says real CPU-CGB-C fails most of its channel 1/2
tests. The human signed those 4 off as CGB-E results (2026-11-23, ADR
0003: CPU CGB C); the manifest runs them in the stretch group
`same-suite-apu-cgbE`. Applying the single-speed grid rule in double speed
instead was measured and rejected: it also costs SameSuite
`channel_{1,2}_duty` and four gambatte `speedchange*_ds_1`.

**A switch to double speed with the APU on flips `lf_div`.** While the
frame sequencer runs 2 clocks late after such a switch
(`Timer::apu_ds_delay`, below: AGE's `m_current_ds_delay`, hardware
verified by AGE `spsw-ch2-lc-delay` on CGB B/C/E; it toggles at each
switch to double speed and NR52 power on/off clears it), a double-speed
trigger takes the other delay: one period plus `6 + 2 * (lf_div ^ 1)`
(`Apu::trigger_delay`; `Apu::write` gets the flag in bit 0 of `ds`). The
whole APU clock is one 2 MHz tick behind, so its 1 MHz phase is too.
Source: gambatte cgb04c `speedchange_ch1_nr4init_duty0_pos6_to_pos7_
timing_1`/`_2` (NR52 power on in single speed, switch, CH1 trigger at
f = 0x7E0 at clock 0 mod 4, retrigger 900 or 902 clocks later to freeze
the position: `_2` must see position 7, so the first step is at most 6
clocks past the period, not the 8 the power-on phase gives). That the
delay covers the 1 MHz phase as well as the frame sequencer is inferred
from this one ROM pair (no other ROM here triggers in double speed with
the flag set); AGE delays only its frame sequencer there.
Alternative rejected (measured): AGE's own CGB-C trigger rule ties the
phase to DIV instead (`gb_sound::should_align_frequency_timer`: one
sample more when `(clock + div_offset) & 2`, double speed only; gambatte
likewise keys `DutyUnit::nr4Change` to its DIV-aligned counter). As bit 2
of the system counter it also passes this ROM and the gambatte `sound`/
`speedchange` ROMs, but fails SameSuite `channel_{1,2}_duty` (CGB-E
verified; power cycles in double speed at varying DIV phases), which the
power-on phase plus the delay keeps.

**The output bit is latched at each duty step.** `Square::high` holds the
duty bit sampled when the position last moved; an NRx1 duty change shows
only from the next step, and a retrigger or NRx2 write keeps it (gambatte
`DutyUnit::nr1Change` leaves `high_` alone; its cgb04c
`ch1_duty0_to_duty3_pos3*` ROMs, whose duty change lands either side of a
step; SameBoy latches it the same way, `square_duty_output`, but only for
pre-CGB models). Until its first duty step a newly started channel
outputs digital 0 (Pan Docs: "On first startup..."; SameBoy
`sample_surpressed`): a trigger of a channel that was off clears the bit.

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
  reloaded with the period at each step.
- **Trigger phase** (`noise.rs` `trigger_delay`; SameBoy
  `prepare_noise_start`): a trigger keeps the counter and loads the
  divisor countdown with `r * 4 + 6` ticks (r = 0: 6) plus terms by the
  alignment `a` (2 MHz ticks since power on, mod 4; in single speed
  always even, bit 1 = M-cycle parity): r = 0: +1 if `a` odd, +2 in
  double speed if even (CGB C; 0 on D/E); r > 0: `a` = 1, 2, 3: -1, -2,
  -3, `a` = 0: -4 for r > 1 except in double speed on CGB C; r = 1
  exceptions (on and `a` = 1: -5; off and `a` = 3: +1; double speed and
  `a` = 2: +2 on CGB C; at `a` = 0: -4 only if on with shift 0). The first
  LFSR step is then the first increment that raises bit `s`: from power
  on (counter 0) `2^s - 1` increments after the first. The CGB-C
  branches of SameBoy are the ones used; SameSuite `channel_4_delay` and
  `channel_4_equivalent_frequencies` (single speed, `a` = 0, where C and
  E agree) fit it exactly. `channel_4_align` (double speed, r = 0)
  expects E's even-alignment term (0, not 2): CGB-E result. The power-off
  state reads as counter 0 (SameBoy's power-on counter). Not modelled:
  the terms for a counter left counting in the background while the
  channel is off (the core's counter stops with the channel), a restart
  within 2 ticks of an increment, and the r = 0 restart LFSR 0x0055.
- **The counter and NR43 writes** (`noise.rs` `rebase`; SameBoy
  `GB_apu_run`, `GB_apu_write` NR43): the counter is never reset by a
  trigger or an NR43 write (only by APU power off), and NR43 is read at
  each increment. A write keeps the counter and the divisor countdown;
  only if the countdown has just reloaded (`countdown_reloaded`: the
  write lands on an increment) does the new divisor load at once (SameBoy
  adds `{2, 1, 4, 3}[alignment & 3]` ticks there on CGB-C, `{2, 1, 0, 3}`
  on CGB-D+, for r > 0; not modelled). The next step is the first
  increment after which counter bit `s` (new shift) is set and was clear:
  counter `== 2^s` modulo `2^(s+1)`. So a new shift or divisor applies
  within the rest of the old divisor countdown (at most 56 clocks) plus
  one new period; changing `s` alone never steps the LFSR. The core keeps
  the counter implied between steps: `counter` is its value after the
  increment at `countdown`, with at most `pre` earlier increments every
  divisor clocks back from there (`pre` bounds the first, longer gap
  after a write that shortened the divisor). A trigger keeps the counter
  bits as they are (Trigger phase above).
- **NR43 write glitches (not modelled):** SameBoy's `nr43_write` steps
  or corrupts the LFSR when the shift selector passes through an
  intermediate value whose bit differs from the old and new bits
  (revision-specific and partly non-deterministic on hardware; CGB-C
  also goes through an intermediate 0xFF write).
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

**CGB C read glitch (not modelled).** SameSuite `apu/README.md`: on CPU
CGB C and older, PCM12/PCM34 report a glitched amplitude for CH1, CH2
and CH4 when read in the M-cycle the output changes, which is why real
CGB C fails most of its channel 1/2/4 tests (their expected values are
CGB E's). `channel_1_freq_change_timing-cgb0BC` sees it: a read 3 clocks
after a 0 -> 15 duty step (single speed) and 0, 2 or 4 clocks after one
(double speed) returns 0, one 7 clocks after returns 15 (SameBoy's CGB-C
`pcm_mask` masks a nibble to 0 when a step from sample 0 lands at the end
of a run). Modelled as "a square read within 4 clocks of a step returns
the AND of the old and new bit", it passes that ROM but fails 14 SameSuite
ROMs that pass now (measured, 2026-11-23): the single-speed part alone
`channel_{1,2}_{delay,duty_delay,freq_change,restart,stop_div,stop_restart}`
(12), the double-speed part `channel_{1,2}_duty` (2). Those expect CGB E
reads; choosing between them needs the human (STATUS "Needs human").

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
  envelope stopped (the boot chime has decayed by hand-over). CH1 keeps
  running at the chime's last note, f = 0x7C1 (252 clocks per step), duty
  2, position 6 with 145 clocks to its next step at clock 0. The phase is
  fitted to gambatte's cgb04c `ch1_init_pos_1..8`, which retrigger CH1 at
  clocks spread over 4 steps to freeze its position: the step into
  position 1 must fall within clocks 144-147 (+ 504) for all 8; 145 puts
  the steps on the 1 MHz grid (`1 mod 4`) where the boot ROM's own
  single-speed trigger leaves them (above). The length counter stays 0
  (provisional).

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

- The CGB-C PCM12/PCM34 read glitch (PCM12 / PCM34 section): the one
  missing piece of SameSuite `channel_1_freq_change_timing-cgb0BC`.
- SameBoy's envelope lock/clock model, the secondary DIV event and the
  CGB-C `_nrx2_glitch` (the classic model plus zombie mode is used).
- The square trigger delay fit: the extra delay when retriggering an
  active channel (gambatte `ch*_duty*_pos*`).
  The single-speed grid phase across switches is fitted, not derived: the
  APU is not ticked differently over the switch (the CPU's position
  against the APU and PPU is ADR 0020's).
- The noise counter's background counting while the channel is off (and
  the trigger terms that depend on it), the alignment term when an NR43
  write lands on a reload, and the NR43 write glitch steps.
- SameBoy's sweep delay at the trigger (its trigger overflow check also
  waits `shift` ticks, plus a reload of 2 or, on CGB C in one 1 MHz
  phase, 3), its NR10 writes inside the step's check window and its CGB-C
  `nr10_write_glitch`; the step's delayed overflow check (Sweep above) is
  modelled. The DAC fade.
- DMG and AGB behaviours (wave RAM corruption, length counters surviving
  power off, the DMG read window).
