# Mode-3 oracle (test only)

`crates/gbc-core/src/ppu/mode3_oracle.rs`, compiled only for tests. It
answers M10 review finding E1 (`docs/reviews/stage1-m10/E-holistic.md`):
the core computes the length of mode 3 in closed form (`LineTiming`,
`set_stalls`, the discard part of `hold_fetch`), and nothing independent
checked that form. The oracle steps one line's pixel pipeline dot by dot
and a differential test compares the two. It is not used by the core and
adds nothing to the release build.

## What it models, and from where

SameBoy `Core/display.c` (MIT; the file keeps its notice): the mode-3
loop of `GB_display_run`, `render_pixel_if_possible`,
`advance_fetcher_state_machine`, `GB_update_wx_glitch`, for a CGB up to
revision C, single speed. Timing only: no pixel data.

- BG fetcher: seven steps (tile, low plane, high plane at 2 dots each,
  then a push that waits for an empty FIFO). Pan Docs, Pixel FIFO.
- BG FIFO fill count, starting with 8 junk pixels.
- `position_in_line` from -16: junk tile, the `SCX & 7` compare at
  -16..-9, then one pixel per dot to 160. Pan Docs, Rendering, "Mode 3
  length".
- Window: the WY condition is an input; WX is compared with
  `position_in_line + 7` every dot; WX = 0 and WX = 166 follow SameBoy's
  special cases (the extra dot at WX = 0 with a fine scroll comes out of
  `cgb_wx_glitch` delaying the push); WX >= 167 never matches.
- Objects, in X order: each waits for the fetcher's last step with a
  non-empty FIFO, then 6 dots; X = 0 is fetched before the discard.
  Fetched with LCDC.1 clear too (CGB). Pan Docs, "OBJ penalty algorithm",
  describes the result.

Output: the line position at which STAT first reads mode 0. The loop
starts at position 89 (SameBoy `cycles_for_line` at `mode_3_start`; the
core's `Latch`). One SCX write per line can be given; a CPU access at
position `p` comes after the loop's step at `p` (the ordering that also
makes a STAT read at the loop's last position see mode 0).

## Coverage

`ppu::mode3_oracle::tests`, in the normal `cargo test` (under a second):

- `oracle_gives_the_documented_lengths`: the oracle alone against Pan
  Docs' figures and Mooneye `intr_2_mode0_timing_sprites`' worst case.
- `core_mode3_length_matches_the_dot_stepped_oracle`: about 30,000 lines
  with static registers. Every `SCX & 7` with every WX 0..=167 and 0xFF,
  LCDC.5 on and off; one object at every X 0..=168 with and without a
  window; two objects at every distance 0..=16; 1..=10 objects at one X
  and evenly spaced; 6,000 fixed-seed pseudo-random lines (0..=10
  objects spread, clustered or at the left edge, any OAM slots, LCDC.1
  on or off); a sample again with the WY condition not met.
- `core_mode3_length_with_an_scx_write_matches_the_oracle`: about 29,000
  lines with one SCX write at every position 80..=124 and some later,
  every pair of fine scrolls, with objects and windows.
- `..._exhaustive` (`#[ignore]`): the same with 1,000,000 and 500,000
  pseudo-random lines (about 45 s unoptimized).

The core runs through `Ppu::sync`, `Ppu::scroll_write` and `Ppu::stat`
on consecutive lines of one PPU; one line in eight is synced and read at
every dot, the others once after the line.

Not covered: double speed, the first line after LCD enable, WX/WY/LCDC
writes in mode 3, OAM DMA lines, the HBlank interrupt's own end
(`LineTiming::edge`), pixels.

## Disagreements (2026-11-24)

Everything asserted agrees. Two classes disagree and are left out by the
enumerated list in `excluded()`; the test prints their counts and fails
if a listed class stops disagreeing.

1. **WX = 0, `SCX & 7 != 0` (= f), window starting, an object at X
   1..=167.** Without objects both give 6 + 1 dots. With one, its wait
   depends on where the window's tile grid lies, that is, on how many
   window pixels are cut off at the left. The references give three
   answers:

   | cut | who | 11-dot object X (mod 8) |
   |---|---|---|
   | 7 | Gambatte's model (and the core's stalls until ADR 0034) | 1 |
   | 7 + f | the core since ADR 0034: renderer (`Renderer::trigger_skip`) and stalls (`sorted_object_stalls`); Pan Docs, Window: "switched to before the initial fine scroll adjustment, causing it to be shifted left by SCX % 8 pixels" | 1 - f |
   | 8 + f, 14 at f = 7 | SameBoy's loop (the oracle) | 8 - f; at f = 7: X = 1, then 2 from X = 10 |

   Until ADR 0034 (2026-11-24) the core held two of them: its renderer
   cut 7 + f and its stalls assumed 7, although every reference makes
   the object wait for the tile its pixel is shown in. The stalls now use
   the renderer's grid (decision and reasons: ADR 0034). What is left is
   the one pixel between 7 + f and 8 + f.

   Measured (one object, dots it adds, core/oracle, X = 1..=8, the
   pattern repeating every 8 except at f = 7):

   | f | now (7 + f) | before ADR 0034 (7) |
   |---|---|---|
   | 1 | 10/9 9/8 8/7 7/6 6/6 6/6 6/11 11/10 | 11/9 10/8 9/7 8/6 7/6 6/6 6/11 6/10 |
   | 2 | 9/8 8/7 7/6 6/6 6/6 6/11 11/10 10/9 | 11/8 10/7 9/6 8/6 7/6 6/11 6/10 6/9 |
   | 3 | 8/7 7/6 6/6 6/6 6/11 11/10 10/9 9/8 | 11/7 10/6 9/6 8/6 7/11 6/10 6/9 6/8 |
   | 4 | 7/6 6/6 6/6 6/11 11/10 10/9 9/8 8/7 | 11/6 10/6 9/6 8/11 7/10 6/9 6/8 6/7 |
   | 5 | 6/6 6/6 6/11 11/10 10/9 9/8 8/7 7/6 | 11/6 10/6 9/11 8/10 7/9 6/8 6/7 6/6 |
   | 6 | 6/6 6/11 11/10 10/9 9/8 8/7 7/6 6/6 | 11/6 10/11 9/10 8/9 7/8 6/7 6/6 6/6 |
   | 7, X = 1..=8 | 6/11 11/10 10/9 9/8 8/7 7/6 6/6 6/6 | equal (11 10 9 8 7 6 6 6) |
   | 7, X >= 9 | equal (6 11 10 9 8 7 6 6) | 11/6 10/11 9/10 8/9 7/8 6/7 6/6 6/6 |

   Now the core's wait is one dot longer than the oracle's, or 6 against
   11 where the object's pixel is the last of a core tile and the first
   of an oracle tile; at f = 7 only X = 1..=8 differ (SameBoy's 14 is
   7 + f). Example: SCX 3, X = 5: core 272 (273 before), oracle 277. The
   static sweep leaves out 1,348 lines for this class; 892 disagree
   (1,197 before).

   Nothing available here says which grid is right:
   - No ROM in the manifest reaches the class (probed 2026-11-24: a
     counter in `set_stalls` over the full manifest; the fingerprint of
     the whole corpus is unchanged by ADR 0034). WX = 0 with a fine
     scroll and no object: AGE `stat-mode-window` (3 ROMs), Mealybug
     `m3_window_timing_wx_0`, Gambatte
     `window/arg/late_scx_late_wy_FFto4_ly4_wx00_*`. WX = 0 with objects
     and no fine scroll: Gambatte `sprites/space/10spritesPrLine_wx0_*`.
   - Mealybug `m3_window_timing_wx_0` (SCX = LY, objects off) shows the
     window as one solid tile: the full manifest has no regression with
     the renderer cutting 7, 7 + f or 8 + f (tried).
   - Gambatte's model has no special case for WX = 0 with a fine scroll
     (WX = 0 matches at its xpos 0, after the discard), so nothing shows
     it was checked there. SameBoy has special cases for exactly this
     (activation at position -16, a pixel inserted at f = 7) but names no
     test, and marks the place "TODO: Verify fetcher access timings in
     this case".

   Verdict: the core is consistent (7 + f for pixels and stalls, a
   best-evidence choice from one source, ADR 0034); 7 + f against 8 + f
   is open. What settles it on a CGB: WX = 0, WY = 0, each f in 1..=7, a
   window tile map whose first tiles and their columns can be told
   apart. The window pixel at screen x = 0 is 7 (Gambatte's grid), 7 + f
   (the core is right) or 8 + f with 14 at f = 7 (SameBoy is right). The
   stalls alone: mode 3 with one object at each X in 9..=16 is longest
   at X = 9, 17 - f, or 16 - f (10 at f = 7).
2. **An SCX write during the discard on a line with an object at X = 0**
   (positions 89..=107, 6 more per further object at X = 0). SameBoy
   fetches X = 0 objects before the discard, so the compares run 11 dots
   later and see the write; the core stalls at pixel 0, after the
   discard. Static lengths agree. Example: SCX 0, X = 0, SCX = 1 written
   at 89: core 267, oracle 268. Verdict: core suspected (SameBoy states
   the order explicitly); no ROM in the manifest covers it.

One disagreement was the oracle's and is fixed: SCX writes were first
applied to the loop's step at the write's own position; they come after
it (see above). With that, the core's `DISCARD_LEAD` agrees everywhere
outside class 2.
