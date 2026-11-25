# 0034: At WX = 0 with a fine scroll the window loses 7 + SCX % 8 pixels, for pixels and object stalls alike

- Status: Accepted
- Date: 2026-11-24

## Context
A window that starts at WX = 0 begins left of the screen, and with a
fine scroll (f = `SCX & 7` != 0) the references do not agree on how many
of its pixels are cut off at the left, that is, on where its tile grid
lies on the line. Two things in the core depend on that grid: the pixels
(`Renderer::trigger_skip`) and the mode-3 stall of each object in the
window, which waits for the fetch of the tile its leftmost pixel is in
(`sorted_object_stalls`; Pan Docs, Rendering, "OBJ penalty algorithm").

The mode-3 oracle (`docs/reference/mode3-oracle.md`, disagreement 1)
showed that the core held two different grids:

| cut | who | 11-dot object X (mod 8) |
|---|---|---|
| 7 | the core's stalls (`win_x = WX - 7`); Gambatte's timing model, which has no special case for WX = 0 with a fine scroll (WX = 0 matches at its xpos 0, after the discard) | 1 |
| 7 + f | the core's renderer; Pan Docs, Window: at WX = 0 the window "is switched to before the initial fine scroll adjustment, causing it to be shifted left by SCX % 8 pixels" | 1 - f |
| 8 + f, 14 at f = 7 | SameBoy `Core/display.c` (activation at position -16, a pixel inserted at f = 7), marked "TODO: Verify fetcher access timings in this case"; the oracle follows it | 8 - f; at f = 7: X = 1, then 2 from X = 10 |

Measured before this decision (one object on the line, the dots it adds
to mode 3, core/oracle, X = 1..=8, repeating every 8 except at f = 7):

| f | core stalls (cut 7) / oracle (cut 8 + f) |
|---|---|
| 1 | 11/9 10/8 9/7 8/6 7/6 6/6 6/11 6/10 |
| 3 | 11/7 10/6 9/6 8/6 7/11 6/10 6/9 6/8 |
| 7 | equal at X = 1..=8; from X = 9: 11/6 10/11 9/10 8/9 7/8 6/7 6/6 6/6 |

Example: SCX 3, one object at X = 5: core 273, oracle 277. Of the 1,348
lines of this class in the oracle's static sweep, 1,197 disagreed.

No evidence here decides between the three:
- No ROM in the manifest has WX = 0, a fine scroll, a window start and
  an object on the line (probed with a counter in `set_stalls` over the
  full manifest).
- Mealybug `m3_window_timing_wx_0` (SCX = LY, objects off) has the case
  without objects, but its window is one solid tile: the manifest has no
  regression with the renderer cutting 7, 7 + f or 8 + f.
- No hardware capture of the case is available.

The human's instruction (2026-11-24): decide on judgement and document
it.

## Decision
The object stalls use the grid the renderer draws: **7 + f pixels cut at
WX = 0**, so the window tiles start at screen x = 8k - 7 - f and one
object adds the full 11 dots at X = 1 - f (mod 8). The renderer is
unchanged.

In `sorted_object_stalls` the window's tile grid starts at
`WX - 7 - (SCX & 7)` when WX = 0 and at `WX - 7` otherwise. What counts
as "in the window" is unchanged (The Pixel at or right of `WX - 7`), so
an object at X = 0 is still outside it, as it is at f = 0. One
subtraction per line with a window; nothing per pixel.

Reasons:
1. One fetcher produces both the pixels and the timing. Under any of the
   three models an object waits for the tile its pixel is shown in, so a
   core whose stalls and pixels use different grids is wrong under all
   of them. Agreement is the part that is certain.
2. A fixed cut of 7 cannot give the SCX-dependent shift Pan Docs
   documents for the window at WX = 0, so the stalls move to the
   renderer's grid and not the other way round.
3. SameBoy's 8 + f is one pixel from 7 + f and is marked unverified in
   its own source, with no test named; that is not enough to move the
   renderer (and the stalls with it) off the documented behaviour.

This is a best-evidence choice resting on one source (Pan Docs), not a
verified hardware fact.

## Consequences
- The core is consistent at WX = 0: pixels and stalls share one grid.
  Unit test `ppu::render::tests::object_stalls_use_the_window_grid_the_renderer_draws`
  reads the grid off a rendered line (window tiles of different colors)
  for WX 0, 1, 3, 7, 8, 50 and every fine scroll, and requires
  `object_penalty` to follow it for one object at every X; seen failing
  with the stall change reverted ("WX 0 SCX 1 X 1": 11, expected 10).
- Behaviour changes only on lines with WX = 0, f != 0, a window start
  and an object at X 1..=167: mode 3 ends up to 5 dots earlier or later
  than before. f = 0 and WX != 0 are unchanged (same test).
- The oracle still disagrees in this class, now by SameBoy's one extra
  pixel, and the class stays on its exclusion list: 892 of the 1,348
  lines disagree (1,197 before). The core's wait is one dot longer than
  the oracle's, or 6 against 11 where the object's pixel is the last of
  a core tile and the first of an oracle tile; at f = 7 only X = 1..=8
  differ. Per f: `docs/reference/mode3-oracle.md`. Example: SCX 3,
  X = 5: core 272, oracle 277.
- No acceptance result changes (full manifest, no regressions) and the
  fingerprint of all 4,031 corpus ROMs is identical to `main`'s with
  `--audio-buffer 7 --hash-samples`: the case is outside the corpus, so
  nothing but the unit test guards it.
- Cost: total `.text` 113,952 -> 114,046 B (thumbv7em); one subtraction
  per line with a window.

## What would change it
One hardware observation on a CGB: WX = 0, WY = 0, each f in 1..=7, a
window tile map whose first tiles have distinguishable columns (and
differ from each other).
- The window pixel shown at screen x = 0 is the window's pixel 7 (the
  fixed cut, Gambatte's grid), 7 + f (this ADR) or 8 + f, 14 at f = 7
  (SameBoy).
- Independently, for the stalls alone: with one object on the line at
  each X in 9..=16, mode 3 is longest (11 dots more) at X = 9, 17 - f,
  or 16 - f (10 at f = 7), in the same order.

If it shows anything but 7 + f, renderer and stalls move together
(`Renderer::trigger_skip` and the grid in `sorted_object_stalls`), and
the oracle's exclusion 1 is removed or rewritten to match.
