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
- `core_mode3_length_with_an_scx_write_matches_the_oracle`: about 54,000
  lines with one SCX write at every position 80..=124 and some later,
  every pair of fine scrolls, with objects (at X = 0 too: one, two, and
  one beside an object at X = 4, that also behind a window at WX 3 and
  7) and windows, the one at WX = 0 too (alone, with one or two objects
  at X = 0, with one at X = 4 or 12).
- `..._exhaustive` (`#[ignore]`): the same with 1,000,000 and 500,000
  pseudo-random lines (about 45 s unoptimized).

The core runs through `Ppu::sync`, `Ppu::scroll_write` and `Ppu::stat`
on consecutive lines of one PPU; one line in eight is synced and read at
every dot, the others once after the line.

Not covered: double speed, the first line after LCD enable, WX/WY/LCDC
writes in mode 3, OAM DMA lines, the HBlank interrupt's own end
(`LineTiming::edge`), pixels.

## Disagreements (2026-11-24)

Everything asserted agrees. One class disagrees and is left out by the
enumerated list in `excluded()`; the test prints its count and fails if
a listed class stops disagreeing.

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

   With an SCX write on the line it is the same dispute, listed apart
   (`excluded()`): the SCX-write sweep leaves out 4,900 lines (the window
   at WX = 0 starting before the discard, an object at X = 4 or 12),
   3,181 disagreeing, all by the same +1 or 6 against 11. SameBoy's
   extra pixel belongs to the early start, whatever the discard comes to
   after the write (8 + n cut for a discard of n pixels, the core
   7 + n % 8). Tried (2026-11-24): with the core's grid one pixel further
   left, 6,104 of 6,615 such lines (X = 4, 8, 12) agree; the rest are a
   discard of 0 or 8 pixels, which the trial grid did not cover (231),
   and X = 12 at a discard of 7 (280, SameBoy's 14). Without an on-screen
   object these lines are asserted (the third class below).

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

A second class was the core's and is fixed (2026-11-24): **an SCX write
during the discard on a line with an object at X = 0** (positions
89..=107, 6 more per further object at X = 0; 745 of the sweep's 1,160
such lines). Objects at X = 0 are fetched before the discard, so its
compares run 11 dots later and see a write made meanwhile; the core
compared from position 89 whatever the objects. Example: SCX 0, X = 0,
SCX = 1 written at 89: 268 (the core gave 267). With it, when the write
moves the discard past the 8 junk pixels, an object at X 1..=7 no
longer shares its tile with the one at X = 0 (the junk tile is gone):
SCX 1, X = 0 and 4, SCX = 0 written at 100: 282 (the core gave 281).
(`Ppu::unshare_junk_tile`, applied at the SCX write only: a later window
write on such a line rebuilds the stalls without it; not covered here.)
Evidence, none of it a capture of this case:

- SameBoy `render_pixel_if_possible`: "Rendering (including scrolling
  adjustment) does not occur as long as an object at x=0 is pending". No
  TODO on it, no model condition.
- The DMG netlist (GateBoy, `GateBoy.cpp`): `VYBO_CLKPIPE = nor3(FEPO_
  STORE_MATCH, WODU_HBLANK, MYVO)`; the fine counter's clock
  `PECU_FINE_CLK` and the match flip-flop `PUXA_SCX_FINE_MATCH` are
  clocked from CLKPIPE. A sprite match stops the discard's compares; the
  X counter is 0 until the discard is done, so X = 0 matches first. DMG,
  not CGB.
- Pan Docs, "OBJ penalty algorithm": X = 0 "always incurs a 11-dot
  penalty, regardless of SCX", which a fetch after the discard would not
  give.

No ROM here pins it. Gambatte `scx_during_m3_spx0` (and
`scx_attrib_during_m3_spx0`, not in the manifest) writes SCX at
positions 95 and 99 on its 8 lines with an object at X = 0, inside the
window, but 0x00 -> 0x60 -> 0xC0: `SCX & 7` stays 0 and both orders give
the same line. Mealybug `m3_scx_high_5_bits` changes `SCX & 7` on lines
with an object at X = 0, at position 115, after the discard in both.
No other ROM of the manifest writes SCX at 85..=125 on such a line
(probed over the full manifest). A capture that would pin it: an object
at X = 0, SCX 0 at the line start, SCX = 1 written at positions 89..=99,
and the dot at which STAT reads mode 0 (268 against 267) or the image
shifted by one pixel.

A third class was the core's too and is fixed (2026-11-24): **an SCX
write during the discard on a line whose window at WX = 0 started
before it** (`SCX & 7` not 0 as the pipeline starts, position 89; the
write at positions 89..=102). 427 of the 2,520 lines without an object
(every pair of fine scrolls, positions 80..=124) disagreed by -7..=+8
dots, and 1,050 more of 5,040 with one or two objects at X = 0. None of
it is disagreement 1: without an on-screen object the static totals
agree. Two things, with f the fine scroll at the line's start, f' the
written one, and n the discard an SCX write seen at compare w gives (the
next n >= w with n % 8 = f'; f if compare f had matched):

| | core before | oracle, core now |
|---|---|---|
| compare k of the discard runs at | 89 + k | 96 + k: after the window's first tile, 7 dots |
| a write at position t is seen from compare | t - 88 | t - 95 (0 up to t = 95) |
| the window's stall | 7 dots if n % 8 != 0, else 6 | 7 dots: the early start's, whatever is written |
| mode 0 at | 262 + n + (n % 8 != 0) | 263 + n |

Measured, no objects (core before / oracle; equal at the other
positions, and at every position when f = 0: the discard is over at 89
and the window starts after it, 262):

| f -> f' | 89 | 90, 91 | 92..=95 | 96 | 97, 98 | from 99 |
|---|---|---|---|---|---|---|
| 1 -> 0 | 270/263 | 264/263 | 264/263 | 264/271 | 264 | 264 |
| 3 -> 0 | 270/263 | 270/263 | 266/263 | 266/271 | 266/271 | 266 |
| 3 -> 5 | 268 | 268 | 266/268 | 266/268 | 266/268 | 266 |

The core compared from position 89 whatever the window (`hold_fetch`)
and rebuilt the window's stall from the new discard (`set_stalls`), so
it held two orders at once: its renderer and its object stalls cut the
window by 7 + f pixels (ADR 0034), which is the window starting before
the discard, and its discard ran before the window's fetch. Now the
compares wait for that fetch and the stall keeps its 7 dots
(`Ppu::hold_fetch`; a later window write on
such a line rebuilds the stalls without it). Evidence, none of it a
capture of this case:

- SameBoy, mode-3 loop: at WX = 0 the window activates at
  `position_in_line` -16 when `SCX & 7` is not 0, the FIFO is cleared,
  and `render_pixel_if_possible`, which holds the fine-scroll compare,
  returns while the FIFO is empty. The extra dot is `cgb_wx_glitch`
  holding the push of a window tile fetched at positions -16..-8; SCX
  is not in it. SameBoy marks the activation "TODO: Verify fetcher
  access timings in this case" and names no test.
- The DMG netlist (GateBoy, `GateBoy.cpp`): `TYFA_CLKPIPE = and3(SOCY_
  WIN_HITn, POKY_PRELOAD_LATCHp, VYBO_CLKPIPE)`: a window hit stops
  CLKPIPE until its fetch is done, and the discard's match flip-flop
  `PUXA_SCX_FINE_MATCH` is clocked from CLKPIPE (`ROXO`), comparing the
  live SCX bits (`SUHA`/`SYBY`/`SOZU`). The WX compare needs no finished
  discard: the pixel counter is 0 until then, which is WX = 0's match.
  DMG, not CGB.
- Pan Docs, Window: at WX = 0 the window "is switched to before the
  initial fine scroll adjustment".
- AGE `stat-mode-window` (verified on CGB B/C/E) has the 7th dot of the
  static line, which the early start explains: Gambatte's model, the one
  reference with the window after the discard (no special case for
  WX = 0), has no such dot.

No ROM here reaches it (probed 2026-11-24 with a counter in
`hold_fetch` over the full manifest: 311 ROMs change SCX in mode 3,
none with WX = 0 and LCDC.5 set; Gambatte
`late_scx_late_wy_FFto4_ly4_wx00_*` changes SCX in no line's mode 3; the
fingerprint of the whole corpus is unchanged by the fix). A capture
that would pin it, on a CGB: WX = 0, WY = 0, SCX 1 at the line start,
SCX = 0 written at positions 89..=97, and the dot at which STAT reads
mode 0: 263 up to 95, then 271, then 264 (the core before: 270 at 89,
then 264). Not modelled: the pixels of such a line when the discard
wraps (n >= 8: the renderer still cuts 7 + n % 8 window pixels, not
7 + n).

One disagreement was the oracle's and is fixed: SCX writes were first
applied to the loop's step at the write's own position; they come after
it (see above). With that, the core's `DISCARD_LEAD` agrees everywhere
(with the X = 0 fix above).
