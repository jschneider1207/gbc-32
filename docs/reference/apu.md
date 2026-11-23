# APU notes (M6)

Distilled rules with sources; the full design is ADR 0021. Task 10 extends
this file.

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
