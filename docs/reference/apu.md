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
