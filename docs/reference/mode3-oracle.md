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

1. **WX = 0, `SCX & 7 != 0`, window starting, an object at X 1..=167.**
   Without objects both give 6 + 1 dots. With one, its wait depends on
   where the window's tile grid lies. The core's stalls use WX - 7 (7
   pixels cut off). The core's renderer cuts `7 + SCX & 7` (Pan Docs,
   Window: WX = 0 "stutters" with `SCX % 8`; Mealybug
   `m3_window_timing_wx_0` passes). SameBoy's loop cuts `8 + SCX & 7`
   (14 at `SCX & 7 = 7`) and carries a "TODO: Verify fetcher access
   timings in this case". Example: SCX 3, X = 5: core 273, oracle 277.
   Verdict: unclear which grid is right; the core's stalls disagree with
   its own renderer, so the core is suspected. No ROM in the manifest has
   an object on such a line.
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
