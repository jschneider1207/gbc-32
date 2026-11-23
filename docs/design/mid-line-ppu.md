# Design: mid-line PPU fidelity (fetch-accurate mode-3 writes)

- Status: Proposed (design only; no code yet). Plan bullets: stage-1 M8
  "Mid-line PPU fidelity (Mealybug, stretch)" and parts of "PPU edge cases
  from the M3 hardware review".
- Date: 2026-11-23. Written against 01bca59 without building anything
  (a gate run was in progress). Every Ir and byte figure below is an
  estimate from reading the code. Each increment must measure it.
- Model: CGB C, single speed first (the Mealybug manifest compares
  against `*_cgb_c.png`; the gambatte ROMs are `cgb04c`).

## 1. Hardware behavior to model

**The fetcher (SameBoy `Core/display.c`, MIT, `advance_fetcher_state_machine`;
Pan Docs "Pixel FIFO"; Mealybug "Comprehensive PPU documentation").**
A BG/window tile fetch takes 6 dots in 3 steps of 2 dots (T1 computes an
address from registers, T2 reads VRAM), then waits in PUSH until the BG
FIFO is empty:

| Step | Registers sampled at T1 | VRAM read at T2 |
|---|---|---|
| GET_TILE | LCDC.3 (BG map) or LCDC.6 (window map), LCDC.5 (clears `wx_triggered`), SCX (all 8 bits), SCY (`fetcher_y` = LY + SCY), window state | map index; CGB: attribute byte in the same dot |
| DATA_LOW | LCDC.4 (tile data select), SCY again (CGB <= C; D+ caches the GET_TILE y) | bitplane 0 |
| DATA_HIGH | LCDC.4 again, SCY again | bitplane 1 |
| PUSH | (when the FIFO is empty) | 8 pixels into the FIFO |

The tile column is `x = (SCX + position_in_line + 8 - is_cgb) / 8 & 31`.
It uses the **whole** SCX as of T1 and the current output position. The
fine scroll (SCX & 7 discard) happens once, at line start (Pan Docs:
Scrolling). So a mid-line change to the low bits still moves the *tile
column* through a carry or borrow. This is the `m3_scx_low_3_bits`
pixels 152-159 that are still open. Mealybug: "SCY is read during the
background tile fetch B, 0 and 1 stages [up to CGB C] ... allows for
mixing tile bitplane data from different rows". It also says TILE_SEL "is
read during the 0 and 1 stages ... allows for mixing tile bitplane data
from two different tile patterns". On CGB, *clearing* TILE_SEL on the
same dot as a bitplane read makes the tile index serve as the data (CGB
<= C). Setting it uses glitched data: SameBoy `data_for_tile_sel_glitch`,
`GB_CONFLICT_LCDC_CGB` holds `tile_sel_glitch` for 1 dot.

**Fetch vs output in steady state.** SameBoy pops a pixel, then advances
the fetcher, in the same dot. Tile k+1 is pushed in the dot that pops
tile k's last pixel. Tile k+2's GET_TILE T1 runs in the dot that outputs
tile k+1's first pixel. So when output is inside tile j (screen pixels
`[X_j, X_j + 8)`, `X_j = 8j - SCX%8`, clipped at 0), tile j is in the FIFO
and tile j+1 is being fetched. Its fetch progress is `p = t - out(X_j)`
dots (0-6+). A write that lands at dot `t` therefore affects:
- GET_TILE registers: from tile j+1 if `p == 0`, otherwise from tile j+2;
- the data registers: tile j+1's low plane if `p <= 2`, its high plane if
  `p <= 4`, otherwise from tile j+2;
- the output-side registers (below): from pixel `pixels_before(t)`, as
  today.

During an object stall SameBoy first finishes the BG fetch in progress
(`while (fetcher_state < GET_TILE_DATA_HIGH_T2 || fifo empty)`). Inside a
stall, tile j+1 counts as fully fetched (`p = 6`). The exact `p` origin
(0 or 1 dot) and the write landing dots are **calibration constants**,
fixed by the ROMs below. The current pixel model, `LineTiming::pixels_before`
(`ppu/mod.rs:265`), stays the reference for `out()`. The gambatte `scy`
ROMs that "fail by the same 2,112 pixels" (16 px x 132 lines) fit a
renderer that applies the write 8-16 pixels earlier than this fetch
frontier.

**Per register** (where each one is sampled; the write landing dot is
from SameBoy `cgb_conflict_map`, already used in `bus.rs:817-833`):

| Register | Sampled by | Landing (single speed) |
|---|---|---|
| LCDC.3 / LCDC.6 map select | GET_TILE T1 | at access; double speed: `GB_CONFLICT_LCDC_CGB_DOUBLE`, two phases 2 dots apart |
| LCDC.4 tile data | DATA_LOW T1 and DATA_HIGH T1 (mixed planes); CGB glitch on the read dot | clear: glitch dot, then +1 (`GB_CONFLICT_LCDC_CGB`) |
| LCDC.5 window enable | GET_TILE T1 (switch back to BG at the next fetch, Mealybug WIN_EN notes); also whether the window stall happens at all | at access |
| LCDC.0 (BG/master priority), LCDC.1 (obj enable) | pixel output (`render_pixel_if_possible`) | at access |
| LCDC.2 obj size | object fetch (`get_object_line_address`) and the OAM scan (mode 2, out of scope) | at access |
| SCX | GET_TILE T1 (column, 8 bits); low 3 bits also once at line start (discard) | at access (`READ_OLD`); double speed `SCX_DMG_AND_CGB_DOUBLE` |
| SCY | GET_TILE, DATA_LOW, DATA_HIGH T1 (CGB C) | Mealybug: "on CGB ... 2 T-cycles later" |
| BGP / OBP0 / OBP1 (compat. mode) | pixel output | 1 dot early (`GB_CONFLICT_PALETTE_CGB`), modelled |
| BCPD / OCPD (CGB) | pixel output; CPU-blocked in mode 3 | only the mode-3 edges matter: `cgbpal_m3` access timing, not this design |
| WX | window start: compared with the pixel counter at fetch time (`GB_update_wx_glitch` at every T1; WX=0 CGB glitch) | +1 dot (`WRITE_CPU`), modelled |
| WY | WY==LY condition (mode 2, `wy_check`), outside mode 3 | at access (`READ_OLD`) |

WX, WY and LCDC.5 also change the **mode-3 length**: the 6-dot window
stall happens only if the window really starts. Today it is latched at
`t = 89` (`ppu/mod.rs:1132`).

## 2. Which failing manifest ROMs each sub-behavior should fix

Estimated from the names in `acceptance/results/stage1.json`
(`"fail"` entries grouped by directory and stem; `_ds` = double speed),
not by running anything. "Upper" counts the whole group. "Likely" leaves
out variants that also depend on another sub-behavior (double-speed
landing, CGB glitches, objects).

| Sub-behavior | Failing ROMs it targets | Upper | Likely |
|---|---|---|---|
| A. GET_TILE frontier (map select, SCX high bits) | gambatte `bgtilemap` 24 (8 ds); `scx_during_m3` 95 (52 ds; png group 93); Mealybug `m3_lcdc_bg_map_change{,2}`, `m3_lcdc_win_map_change{,2}`, `m3_scx_high_5_bits{,_change2}`; AGE `m3-bg-scx{,-ds}`, `m3-bg-lcdc{,-ds}` | ~131 | 40-60 (single speed) |
| B. SCX low-bit carry in the column | `m3_scx_low_3_bits`; the low-bit `scx_during_m3` variants (`scx1_scx0`, `scx2_scx1`, ...) | in A | +5-15 |
| C. Per-plane DATA steps (SCY, LCDC.4) | gambatte `scy` 43 (15 ds), `bgtiledata` 18 (6 ds); Mealybug `m3_scy_change{,2}`, `m3_lcdc_tile_sel_change`, `m3_lcdc_tile_sel_win_change` | 65 | 30-40 |
| D. Double-speed landing of SCX/SCY/LCDC | the `_ds` variants of A and C | 81 | 40-60 (after A-C) |
| E. Dynamic window stall and start (WX/WY/LCDC.5 in mode 3) | gambatte `window` 86 (84 are `out` timing ROMs: `late_disable*`, `late_wx*`, `late_wy*`, `late_reenable*`; 21 ds); AGE `stat-mode-window` 3; Mealybug `m3_lcdc_win_en_change_multiple`, `m3_window_timing_wx_0`; plan "mode-3 window stall latched at t=89" | ~91 | 40-65 |
| F. Object fetch on the dot (LCDC.2 at fetch, LCDC.1/OBP inside a stall) | Mealybug `m3_lcdc_obj_size_change{,_scx}`, `m3_lcdc_obj_en_change{,_variant}`, `m3_obp0_change`, `m3_bgp_change_sprites`, `m3_wx_4_change_sprites` | 7 | 3-5 |
| G. CGB TILE_SEL glitch | `m3_lcdc_tile_sel_change2`, `m3_lcdc_tile_sel_win_change2` | 2 | 1-2 |

Not covered: `cgbpal_m3` (28, palette-RAM access edges), `gambatte/sprites`
(22, object timing), `oamdma`/`dma`/`speedchange` and the STAT-timing
groups. Realistic total for A-G: **~160-250 of the 703 failing ROMs**,
all stretch tier.

## 3. Current architecture (01bca59)

- **One draw per line, at the mode-3 end.** `Pt::Latch` (t = 89) calls
  `Ppu::latch` (`ppu/mod.rs:1123`). Latch runs the OAM scan, fixes
  `LineTiming` (`first_pixel = mode3 + 12 + SCX%8`, `:1166`; window stall
  only if LCDC.5, WY condition and WX <= 166 hold *at latch*, `:1132`;
  `end`, `:1203`) and calls `Renderer::start_line` (latches `fine_scx`,
  `render.rs:294`). `Pt::End` draws whatever is left to pixel 160
  (`mod.rs:914`). VRAM and OAM are CPU-blocked in mode 3, so drawing late
  is exact for them.
- **Mid-line writes are synchronous catch-up, not queued.**
  `Ctx::write_ppu` (`bus.rs:776`; LCDC `:780-792`; SCY/SCX/palettes/WY/WX
  `:827-848`) runs the PPU to the write and calls `Ppu::flush(at)`
  (`mod.rs:1013`). That draws pixels `drawn..mode3_pixel(at)`
  (`mod.rs:1546`, `pixels_before`) with the *old* registers through
  `Ppu::draw` (`:982`, registers from `Ppu::regs`, `:968`). Then the write
  lands. So the write's dot timestamp is already exact. What is wrong is
  that every later pixel uses the new value, which is right for output-side
  registers (palettes, LCDC.0/1) and **wrong for fetch-side ones**.
- **The renderer is span-based.** `Renderer::render` (`render.rs:320`):
  window activation by pixel (`:355-372`), objects fetched when the span
  reaches them (`:377`, `fetch_objects` with the span's LCDC), then
  `bg_span` (`:407`). The BG column is
  `(SCX & 0xF8) + fine_scx + x0` (`:445`, so no carry) and the row is
  `ly + SCY` (`:453`), both from the span's registers. `fill_tiles`
  (`:747`) runs `Fetch::row` (`:688`, index, attr and both planes in one
  go) per tile. Partial tiles at span edges go through `partial_tile` →
  `fill_partial` (`:875`, `:884`); `fill_partial` already takes a decoded
  row `w` and an `attr`.

## 4. Proposed design

### Options

1. **FIFO snapshot at the write (recommended).** Keep the synchronous
   flush. When a fetch-side register is written during mode 3, `flush`
   also materializes the part of the fetcher the old registers already
   decided: the rest of tile j (in the FIFO) and tile j+1's completed
   steps. That goes into a tiny `Prefetch` buffer. The next `render` span
   drains it first through the existing `fill_partial`, then continues
   with the normal tile loop from tile j+2. With no write, the buffer is
   empty and the only cost is one test per span.
2. **Per-line write log, replayed at the mode-3 end.** Queue
   `(dot: u16, reg: u8, val: u8)` in a fixed `[_; 16]` during mode 3 and
   draw the whole line at `End` with a per-tile slow loop. For each tile it
   computes the dots of its fetch steps (the inverse of `pixels_before`,
   including object stalls) and looks up each register's value then.
   Most faithful for many writes per tile. But it needs a second tile loop
   and an inverse-timing walk (~1.5-2 KB, over the 740 B left), an
   overflow fallback, and a bypass of today's flush for the fetch
   registers.
3. **SameBoy-style per-dot mode 3 for "dirty" lines** (SameBoy's own
   `mode3_batching_length` fast path falls back to this on a write). It
   is exact for everything, including F and G, but it is a new pixel
   pipeline (~3-5 KB) and ~5k+ Ir per slow line. Rejected for the budgets.

**Recommendation: option 1.** The flush already gives the exact landing
dot for each write, one at a time, in order. The only state the hardware
carries past a write is what the fetcher holds, at most 2 tiles. So a
"queue" of length 1 (the write being handled) plus a 2-tile snapshot is
enough, and nothing needs replaying.

### Data structures (fixed size, no alloc, in `Renderer`)

```rust
struct Prefetch {            // ~12 B; Renderer stays under the Hw size assert
    n: u8,                   // tiles held: 0 = fast path
    tiles: [PTile; 2],       // [0] = rest of tile j (complete), [1] = tile j+1
}
struct PTile {
    px: u8,                  // pixels of this tile still to output (<= 8)
    done: u8,                // fetch steps done: 0..=3 (GET_TILE, LOW, HIGH)
    win: bool,               // fetched as window (row/column source)
    idx: u8, attr: u8,       // valid if done >= 1
    lo: u8, hi: u8,          // valid if done >= 2 / 3
}
```

`Renderer` lives in `Hw`, which is size-asserted at 864 B (ADR 0019). +12 B must be
checked. If it doesn't fit, put it in `Memory` beside `obj_buf` (ADR 0019
did the same).

### Write path (slow, out of line, flash tier)

`Ppu::flush(at)` already computes `(ly, x)`. New: for writes to LCDC (only
if bits 3/4/5/6 change), SCX or SCY, `bus.rs` calls
`flush_fetch(at, old_regs)` instead:
1. `draw(ly, x)` as today (drains any existing `Prefetch` first).
2. `fetch_progress(at) -> (j, p)`. It reuses `LineTiming`: the tile index
   from `x` and the fine scroll, and `p = t - out(X_j)` from the same walk
   as `pixels_before`, with `p = 6` inside a stall. It is one function
   beside `pixels_before`, sharing its loop (~60 B).
3. Snapshot: if `n == 0`, push tile j (all steps, old registers) with
   `px = X_j + 8 - x` and tile j+1 with `done = step(p)`. If a snapshot
   already exists (a second write before it drains), only *advance* tile
   j+1 to `step(p)` with the current (pre-write) registers. That is how
   several writes in one tile compose.
4. The write lands; `render` will run any steps not yet done with the new
   registers.

The step reads reuse the address logic of `Fetch::row`. Split it into
`map_entry(col) -> (idx, attr)` and `plane(idx, attr, row, hi) -> u8`, both
`#[inline(always)]`, so `Fetch::row` (the hot loop) compiles to the same
code (check with `check-code-size.sh`: `fill_plain`/`fill_mixed` unchanged).

### Render path

At the top of `bg_span` (hot, ITCM): `if self.pf.n != 0 { self.drain(...) }`.
`drain` is out of line in flash. It finishes each held tile's missing steps
with the span's registers, then outputs `min(px, span)` pixels with
`fill_partial::<MIX>(out, obj, w << 2*skip, attr, master, colors)`, the
existing function (`w` from `decode(lo, hi)`), and advances `x0`. Then the
normal path runs from the first tile boundary after the held tiles. The
column of the first tile and of every later one comes from the fetch rule
`(SCX + X_t + K) >> 3` instead of `(SCX & 0xF8) + fine_scx + x`. `K` is
chosen so that with constant SCX the two are identical (sub-behavior B).
That is a span-setup change of a few instructions, not per pixel.

Window (sub-behavior E, also at the write): on a WX/WY/LCDC.5 write in mode
3 before the window has started (`win_stall` not yet reached), recompute
the window stall in `LineTiming` with the stall-building loop factored out
of `latch` (`mod.rs:1175-1202`, reused, not duplicated). Then reschedule
the `End` event (`next_event` already supports moving it; see the
latch-time `End` move in ADR 0014). Clearing LCDC.5 after the start goes
back to the BG at the next GET_TILE, which is a `Prefetch` boundary, not a
pixel boundary.

Objects (F): pass `p` (dots into the stall) from `fetch_progress` to the
renderer, so an object whose fetch at `x` is past its tile-address step
uses the old LCDC.2. Only on writes.

### Cost estimate (reasoned, to be measured per increment)

- **Fast path (no mid-line fetch-register write; this is `synth:worst-case`,
  which writes no PPU register in mode 3):** one byte test per `bg_span`
  call plus the K-term in the column setup. That is ~5 x86 Ir per span, at
  1-2 spans per line, so **~0.7-1.5k Ir/frame** (<= 1.5% of the ~100k
  headroom). The pixel loops are unchanged.
- **Slow path, per fetch-register write:** `fetch_progress` ~40 Ir,
  snapshot (2 map reads + up to 4 plane reads) ~80 Ir, drain (2 x
  `fill_partial`, <= 16 px) ~100-150 Ir, so **~250-350 Ir**.
  `synth:cgb-raster` (one SCX write per line) goes from ~2.66 M to
  ~2.70-2.71 M Ir/frame (67% of the line). A game with ~10 such writes per
  line would add ~0.4-0.5 M/frame. That is fine alone but would exceed the
  line if combined with everything in `worst-case` (see section 6).
- **Code (thumbv7em):** A+B scaffolding (`Prefetch`, `flush_fetch`,
  `fetch_progress`, `drain`) ~450-550 B, of which ~40-60 B goes in ITCM
  (`bg_span` test, column term); C +~80 B; E ~100-150 B (mostly a call to
  the factored stall loop plus the reschedule); F ~100-150 B; G ~150 B.
  **A-C fit in the 740 B left. E-G do not without a budget raise** (total
  ~0.9-1.2 KB). ITCM: +40-60 B against the 1,538 B margin, fine. The new
  out-of-line functions go in the flash tier (cold).

## 5. Step plan

Each step: one behavior, its own commit, `check-bench.sh` and
`check-code-size.sh` (run alone). Use `diff-fingerprint.sh` to prove that
ROMs without mid-line writes are bit-identical, and list and explain every
differing ROM. A unit test in the `render.rs` test harness (`begin`/`span`/
`line`, `render.rs:1000+`) is mutation-checked in a scratch worktree.

1. **A: GET_TILE frontier for LCDC.3/6 and SCX high bits** (scaffolding +
   snapshot of complete tiles and `done = 1` tiles). Flips: `bgtilemap`
   (single speed), `m3_lcdc_bg_map_change{,2}`, `m3_lcdc_win_map_change{,2}`,
   `m3_scx_high_5_bits{,_change2}`, `m3-bg-scx`/`m3-bg-lcdc`, much of
   `scx_during_m3` (single speed). Test: a map-select write at pixel x
   keeps tiles j and j+1 on the old map. Mutation: make `fetch_progress`
   return `p = 6` always (or drop the snapshot), and the test and ROMs
   fail. Calibrate the `p` origin here.
2. **B: SCX low-bit carry** (the `K` term in the column). Flips
   `m3_scx_low_3_bits`, the low-bit `scx_during_m3` variants. Test:
   constant SCX 0-255 equals the old column (equivalence), and a low-bit
   write mid-line moves the column at the carry. Mutation: revert to
   `(SCX & 0xF8) + fine`.
3. **C: per-plane DATA steps** (SCY, LCDC.4 at LOW/HIGH; `done = 2`).
   Flips `scy` and `bgtiledata` (single speed), `m3_scy_change{,2}`,
   `m3_lcdc_tile_sel_change`, `m3_lcdc_tile_sel_win_change`. Test: a write
   between LOW and HIGH gives a mixed-plane tile. Mutation: sample both
   planes at LOW.
4. **E: dynamic window stall and start.** Flips the gambatte `window`
   `out` ROMs, AGE `stat-mode-window`, `m3_lcdc_win_en_change_multiple`.
   Test: an LCDC.5 clear before the window pixel moves the mode-3 end
   6 dots earlier (STAT read). Mutation: skip the recompute.
   **Needs the .text raise** (section 6) unless steps 1-3 came in under
   the estimate.
5. **D: double-speed landing** for SCX/SCY/LCDC (SameBoy
   `cgb_double_conflict_map`, replacing the "at the access" approximation
   in `bus.rs:829`). Flips the `_ds` variants of 1-4. Mutation: restore
   `at = now`.
6. **F: object fetch on the dot** (LCDC.2 at the fetch step, LCDC.1/OBP
   inside a stall). Flips up to 7 Mealybug object ROMs. Diagnose each
   first (`m3_obp0_change` may be a landing-dot issue only).
7. **G: CGB TILE_SEL glitch** (index-as-data on the clear dot). Flips
   `*_tile_sel*_change2`. Lowest value; do it last or drop it.

Also, as a one-line fix with any of these steps (plan "M3 hardware
review" bullet): the palette flush when the line end equals `now` misses
pixel 159 (`bus.rs` palette path: run to `at`, flush, then sync).

## 6. Risks and what needs the human

- **Code size (needs the human).** A-C ~530-630 B against ~740 B left.
  E-G add ~350-450 B more. Ask for gbc-core `.text` budget **96,000 →
  97,000 B** (+1.0%), with the measured sizes, before step 4. The
  alternative is to stop after C or find equal savings. ITCM is not a
  problem (+40-60 B of 1,538 B).
- **Worst-case definition (decision for the human).** `synth:worst-case`
  has no mid-line PPU writes, so this design costs it ~1k Ir/frame. A
  raster-effect game on the busiest frame (10 objects per line, window, a
  fetch-register write every line) would cost ~+40-50k on top of the
  3.92 M (~99%). With 3+ writes per line it would exceed the 4.02 M
  line. Should `worst-case` gain one mid-line SCX write per line? That
  would show the real margin, measured as ~+45k by estimate.
- **Calibration uncertainty.** The `p` origin, the SCY "2 T-cycles later"
  on CGB, and the stall-time fetch state come from SameBoy and Mealybug
  notes, not first principles. Mitigation: fix them in step 1 with ROMs
  that pin them (`m3_lcdc_bg_map_change2`, `bgtilemap_spx08_*`), and
  record them in an ADR (renderer fetch model). Expect some single ROMs
  to need a second sub-behavior before they flip, which is why the
  "likely" counts are below the upper bounds.
- **Window interplay.** A snapshot tile that crosses the window start, or
  WX=0 with fine scroll (`m3_window_timing_wx_0`, the CGB WX glitch in
  `GB_update_wx_glitch`), can mis-assign BG/window. v1 records `win` per
  held tile and falls back to today's per-pixel start if a held tile
  would cross WX. Mutation tests must cover both.
- **OAM DMA lines** (`attr_on`, `fix_attrs` in `flush`) must keep working
  with `flush_fetch`. Run the `oamdma` group with `--filter` in every
  step (no regressions allowed).
- **Regression guard.** The fast path must stay bit-identical. The
  fingerprint diff on every step is the proof. Any differing ROM without
  a mid-line fetch-register write means the step is wrong.

## 7. Implementation notes (step 1, branch `ppu-midline-1`, WIP)

- **Held registers, not held tile data.** Instead of a `Prefetch` of
  fetched index/attr/planes, `Ppu::hold_fetch` (cold) records up to two
  `(end pixel, LCDC, SCX)` parts: the pixels before `end` are drawn with
  that LCDC's bits 3/6 and that SCX. VRAM is CPU-blocked in mode 3, so
  re-reading it at draw time gives what the fetcher read. `Renderer::render`
  splits a span at the part ends (`render_held`, cold); no per-pixel or
  per-tile change, one byte test per span on the fast path. Two parts
  compose consecutive writes (tile j+1 fetched between two writes keeps
  the values between them). Steps C/D can store the data registers per
  part the same way.
- **Fetch frontier.** Tile j (in the FIFO) always keeps the old values;
  tile j+1 too once `FETCH_SAMPLED` dots of tile j were output, or when an
  object/window stall at or after tile j's first pixel has run (BG fetch
  completed in the stall). Discarded SCX & 7 pixels count as output from
  mode-3 dot 12. A held part never crosses the window's start pixel.
- **Calibration:** `FETCH_SAMPLED = 1` (single speed). 0 loses 8 of the
  35 flips; 2 loses the 4 Mealybug flips but gains 7 `scx_during_m3_ds`
  ROMs: double speed wants one dot more, which is step D's landing.
- **Result:** 35 fail->pass, 0 regressions (`bgtilemap` 20 incl. 4 `_ds`,
  `scx_during_m3` 11, Mealybug `m3_lcdc_bg_map_change2`,
  `m3_lcdc_win_map_change2`, `m3_scx_high_5_bits{,_change2}`).
- **Code size.** The hold lives in `Memory::obj_rank` (`ObjRank { rank,
  hold }`): `Memory` is `repr(C)`, and every other spot either shifted hot
  arrays out of Thumb-2 immediates (`Ppu::draw` 146 -> 158 B with the hold
  after `bg_pairs`, 160 B with `bg_pairs` moved last), needed a second
  `Sources` pointer (+16 B in `draw`), or overflowed `Hw` (872 > 864 B in
  `Ppu`). With `ObjRank`, `draw` stays 146 B (budget 155) and total
  `.text` is 95,608 of 96,000 B (`hold_fetch` and `render_held` cold, in
  flash).
- **Slow-path cost.** `synth:cgb-raster` (one SCX write per line) goes
  from 315.93 M to 324.68 M x86 Ir over 2 s (+2.77% vs fe0e18f, ~500 Ir
  per write: one more `render` span and `fill_plain` call per line).
  `render_held` takes the hold while it draws the parts, so they don't
  re-enter it (the first version re-checked and cost ~700 Ir per write).
  `synth:worst-case` (no mode-3 writes): +0.04%.

### Step 2 (SCX low-bit carry, branch `ppu-midline-2`)

- **Column rule.** `render::bg_column(scx, fine, x) = ((scx - fine +
  FETCH_COL) & 0xF8) + fine + x`: the tile column from the whole SCX at
  the tile's GET_TILE (`(SCX + X + FETCH_COL) / 8`, `X` = the tile's
  first screen pixel `8k - fine`), the pixel within the tile from the
  line-start fine scroll. It equals `scx + x` when `scx & 7 == fine`
  (unit test over all 256 x 256). Held parts (step 1) carry their SCX, so
  the carry applies per part with no change to `hold_fetch`. Cost: one
  subtract per BG span, `.text` +8 B (95,616), `render` 1,094 B.
- **Calibration:** `FETCH_COL = 0` (SameBoy's CGB `(SCX + position + 8 -
  1) / 8` with the pixel popped before the fetcher advances): low bits
  below the line-start fine scroll borrow a tile, higher ones never
  carry. Full-manifest sweep vs main: 0 -> 23 flips, 0 regressions; 1 ->
  18/1; 2 -> 17/2; 3-4 -> 3/2; 5-7 -> <= 3/6.
- **Result:** 23 fail->pass, 0 regressions: AGE `m3-bg-scx{,-ds}`,
  gambatte `scx{1_scx0,2_scx0,2_scx1}_during_m3_1` and 18
  `scx_during_m3/scx_0*c0/scx_during_m3_{1..6}` (single speed). The `_ds`
  variants change but still fail (step D's double-speed landing).
  Still failing (single speed): `scx_0360c0` `_2`, `_3` (9,879 -> 14,872
  px), `scx_0367c0` `_2`, `_3`, `scx_0761c0` `_2`-`_4`.
- **Not fixed:** Mealybug `m3_scx_low_3_bits` stays at 540 px for every
  `FETCH_COL` 0-7 (we draw nothing at 152-159), so its pixels 152-159 are
  not the column carry; needs a separate diagnosis.
- **Perf:** `synth:cgb-raster` +2.27% vs the committed baseline (same as
  step 1), `synth:worst-case` -0.32%.

### Step 3 (per-plane SCY and LCDC.4, branch `ppu-midline-3`)

- **Model.** `hold_fetch` also takes SCY writes and LCDC.4 changes; a
  held part (`memory::Held { end, lcdc, scx, scy, planes }`) carries SCY
  and LCDC.4 too. Tile j+1 caught between its map read and its DATA_HIGH
  step keeps its map entry (old map select, SCX, SCY row) and `planes`
  marks the bit planes still to read (`3` both, `2` the high one).
  `render_held` draws the part as before, then `Renderer::mix_planes`
  (cold) redraws that one tile: two `Fetch::row` reads (old and next
  LCDC.4/SCY row) merged by plane (`0x5555` low, `0xAAAA` high), then
  `fill_partial::<true>`. Window tiles mix LCDC.4 only (SCY not used).
  Only SCY/LCDC.4 writes set `planes`, so SCX/map writes never redraw.
  Approximations: a tile cut by the window start keeps its planes as of
  the map read; two writes within one tile fetch compose only by part
  ends (the later write's planes are not tracked).
- **Calibration** (full-manifest sweeps vs main, all 0 regressions):
  `FETCH_LOW`/`FETCH_HIGH` (dots after a tile's first output pixel at
  which the next tile's planes are read) `= FETCH_SAMPLED + 2 / + 4`
  (SameBoy's 2-dot steps): 30 flips; `+3/+5` gives 33 but loses 15 SCY
  ROMs and gains the `bgtiledata` ones, so LCDC.4 lands one dot later than
  SCY: `LCDC_TILES_LATE = 1` (SameBoy `GB_CONFLICT_LCDC_CGB` phases):
  0 -> 30, 1 -> 45, 2 -> 39 flips.
- **Result:** 45 fail->pass, 0 regressions: gambatte `scy_during_m3_{1..6}`
  (`scy/` and `scy/scx3/`), `scy_during_m3_spx0{8,9,A,B}_{1..4}`,
  `bgtiledata_spx*` 16 (incl. `spx09_ds_{1..4}`), Mealybug
  `m3_scy_change2`. Still failing: `scy_during_m3_*ds*` (step D landing),
  Mealybug `m3_scy_change` (10,703 -> ~560 px), `m3_lcdc_tile_sel_change`
  and `_win_change` (CGB TILE_SEL glitch / window, steps E/G).
  `diff-fingerprint.sh main`: only ROMs that write SCY or LCDC.4 in mode 3
  differ (AGE `m3-bg-lcdc*`, Mealybug `m3_scy_change*`, `m3_lcdc_tile_sel*`).
- **Cost.** `.text` 95,010 -> 95,636 B (+626; `mix_planes`, `render_held`,
  `hold_fetch`, all cold). `Ppu::draw` 146 B unchanged. `synth:cgb-raster`
  (one SCX write per line) +2.63% vs the committed baseline (main +2.27%,
  ~+65 Ir per write: the wider `Held` copies and `regs_of`);
  `synth:worst-case` -0.32% (same as main).

### Step D (double-speed write timing, branch `ppu-midline-d`)

- **Rule.** In double speed, LCDC, SCY and SCX writes reach the fetcher
  one dot before the access: `write_ppu` flushes and calls `hold_fetch`
  at `now - 1` (LCDC: `now - u64::from(double)`; the rest of the LCDC
  write, `write_lcdc`, stays at `now`). It is a per-speed constant, not a
  per-register one: the speed is applied where the write dot is chosen,
  so `hold_fetch` and its split points (`FETCH_SAMPLED`, `FETCH_LOW`,
  `FETCH_HIGH`, `LCDC_TILES_LATE`) are unchanged and single speed is
  untouched. References: SameBoy `cgb_double_conflict_map` puts SCX
  (`GB_CONFLICT_SCX_DMG_AND_CGB_DOUBLE`) and LCDC
  (`GB_CONFLICT_LCDC_CGB_DOUBLE`, then the TILE_SEL glitch for one dot)
  at `pending_cycles - 2` 8 MHz ticks = one dot early; it keeps SCY at the
  access (`GB_CONFLICT_READ_OLD`), but gambatte's hardware-verified
  `scy_during_m3_*ds*` need SCY one dot early too (0 -> 15 flips).
- **Calibration** (full manifest vs main, offsets LCDC/SCY/SCX in dots):
  0/0/1 -> 35, 0/1/0 -> 15, 1/0/0 -> 6, 0/0/2 -> 35 (same ROMs as 1),
  0/2/0 -> 11, 2/0/0 -> 2 flips with 6 regressions, 1/1/1 -> 56,
  1/1/2 -> 56, 1/2/1 -> 52; all others 0 regressions.
- **Result:** 56 fail->pass, 0 regressions: gambatte `scx_during_m3_ds_*`
  (38, all SCX groups), `scy_during_m3_ds_{1..7}`,
  `scy_during_m3_spx0{8,9}_ds_{1..4}`, `bgtilemap_spx0{8,9}_ds_{1,4}`,
  `bgtiledata_spx08_ds_{3,4}`. Still failing: AGE `m3-bg-lcdc-ds`,
  gambatte `scx_attrib_during_m3_spx{1,2}_ds`, `scx_during_m3_spx2_ds`.
- **Cost.** `.text` 95,636 -> 95,730 B. `synth:cgb-raster` +2.60% vs the
  committed baseline (main +2.63%), `synth:worst-case` -0.32% (as main).
  `Held` not repacked (not needed for this step).

### Live fine-scroll discard (m9-triage group 5)

- **Rule.** The SCX & 7 discard at mode-3 start compares its count with
  the live SCX & 7 each dot and wraps past 7 (SameBoy display.c,
  `render_pixel_if_possible`: `position_in_line & 7` vs SCX, -9 back to
  -16). `hold_fetch` on an SCX write seen at compare `w <= d` (the
  latched discard count) sets it to the next `n >= w` with `n & 7 ==
  SCX & 7` (up to 14): `first_pixel`, mode 3's end and the renderer's
  fine scroll (`set_fine`, `n & 7`: `bg_column` is the same for n and
  n + 8) move. Writes before the line latch (dot `mode3 + 5`) are latched.
- **Calibration.** The compare for discard pixel `k` runs `DISCARD_LEAD
  = 8` dots before that pixel's output slot (`mode3 + 12 + k`), i.e.
  SameBoy's -16..-9 compares precede the 8 positions -8..-1. Full
  manifest vs main: 0-6 regress 7-32 ROMs, 7 -> 17 flips / 4 regressions,
  8 -> 21 / 0, 9 -> 9 / 0, 10-12 -> 4-1 / 0. (Clamping earlier writes to
  `w = 0` is moot at 8: the latch is already past compare 0.)
- **Result.** 21 fail->pass, 0 regressions: gambatte `scx_during_m3`
  `scx_0360c0` `_2`, `_3`, `_ds_1`-`_3`; `scx_0761c0` `_2`-`_4`,
  `_ds_1`-`_5`; `scx_0063c0_ds_{1,2}`; `scx_0367c0_ds_1`;
  `scx_m3_extend_{1,ds_1}`; `m2int_m3stat/scx/late_scx4_ds_1`;
  `enable_display/ly0_late_scx7_m3stat_scx{1,3}_1`. Still failing:
  `scx_0367c0` `_2`/`_ds_2` (1 px), `_3`/`_ds_3` (143 px, from 12,012),
  `10spritesPrLine_late_scx4_ds` (not diagnosed; suspect: the object
  stalls still use the latched SCX). `diff-fingerprint.sh main`: 27 ROMs differ, all
  SCX-in-discard writers (the 21, those 4, `scx_m3_extend_{2,ds_2}`
  still passing).
- **Cost.** `.text` +64 B (95,836 of 96,000). `synth:cgb-raster`
  325,848,204 -> 325,984,385 Ir (+0.04%; +2.68% of the 3% guard: the one
  extra compare on each mid-line SCX write), `synth:worst-case` +5 Ir.

### Step E1 (window stall on WX/WY/LCDC.5 writes, branch `ppu-midline-e`)

- **Rule.** The 6-dot window stall was fixed at `latch` (t = 89). Now a
  WX, WY or LCDC.5 write in mode 3 calls `Ppu::window_write` (cold),
  which rebuilds the line's stalls (`set_stalls`, shared with `latch`,
  objects re-read from the renderer) from the live registers, unless the
  window's start is already decided: its stall begins within
  `WINDOW_LEAD` (0) dots of the write. A new start (window appears or
  moves) counts only if it begins more than `WINDOW_ON_LEAD` (1) dots
  later; for WY writes `WY_LEAD` (4) dots, measured without the
  fine-scroll discard (WY's edge doesn't move with SCX & 7, unlike the
  LCDC.5/WX edges). WX < 7 starts at position WX - 7, before the first
  pixel (SameBoy display.c: `position_in_line == WX - 7`, `wx_triggered`
  cleared at GET_TILE_T1 while LCDC.5 is clear). `LineTiming.wx` keeps
  the WX the stalls were built with (the old start for a WX write).
- **Calibration** (full manifest vs main): WINDOW_LEAD -2/-1 regress
  10/5, 0 -> best, 1/2/3 -> 32/22/18 flips with 0 regressions;
  WINDOW_ON_LEAD 0/1/2 -> 43-3 / 41-0 / 39-1; WY_LEAD 2 -> 1 regression,
  3-5 identical (41/0). A single lead for all three registers was at best
  42 flips / 12 regressions.
- **Result.** 41 fail->pass, 0 regressions: gambatte `window/`
  `late_disable*` (18), `late_reenable*` (7), `late_wx*` (9),
  `arg/late_wy*`/`late_*_late_wy*` (8, incl. `late_enable_ly0_ds_1`).
  `diff-fingerprint.sh main`: only those 41 and Mealybug
  `m3_lcdc_win_en_change_multiple_wx` (no reference image; writes LCDC.5 in
  mode 3, so its later writes land at other pixels).
- **Not done (E2+).** CGB `late_disable*_1` (out0: a disable after the
  stall began still shortens mode 3 on CGB C, not on DMG), the remaining
  `late_wy` variants (`1toFF`, `2toFF`: clearing the WY match mid-line),
  the WX = A6 / object X = A7 edge (10 ROMs), `wx17_weoff_wxA5_weon`,
  `weon_wx18_weoff_weon_wx80` and Mealybug `m3_lcdc_win_*` (pixels:
  re-trigger after a disable). 56 of group 4's 97 still fail.
- **Cost.** `.text` 95,836 -> 96,412 B (`latch` 1,398 B: the stall loop
  moved to `set_stalls`, called only for lines with objects or a window).
  `synth:cgb-raster` 325,984,385 -> 326,052,348 Ir (+0.02%; +2.70% of the
  3% guard), `synth:worst-case` -0.32% vs baseline (as main).

### Window trigger consistency (M8-close review fixes)

- **One decision for timing and pixels.** The renderer no longer starts
  the window from the live WY condition, LCDC.5 and WX when it reaches
  x = WX - 7. It follows `Renderer::win_wx` (WX + 1, 0 = none), set with
  the stalls: at the line start (`start_line`, from `latch`) and by
  `window_write` whenever it rebuilds them. So a write `window_write`
  rejects as too late draws no window and counts no window row (before,
  WY writes up to `WY_LEAD + fine` dots, LCDC.5/WX writes 1 dot, before
  x = WX - 7 drew the window with no stall and shifted later rows), and a
  WX or LCDC.5 write during the 6-dot stall can't undo it: the row is
  counted at the trigger (SameBoy display.c: `window_y++` and
  `wx_triggered` at the trigger dot), the window starts at the decided x,
  and LCDC.5 clear at the trigger draws the BG. Approximation: no
  re-trigger at a later WX match after LCDC.5 was cleared on the same
  line (SameBoy re-triggers; still group-4 work). Test:
  `window_pixels_follow_the_timing_decision`.
- **Leads.** `WINDOW_LEAD`, `WINDOW_ON_LEAD` and `WY_LEAD` stay calibrated
  approximations; no reference pins them. SameBoy compares the live WX
  each dot (a lead of 0 fits `WINDOW_LEAD`), with no appear/disappear
  asymmetry (the extra dot of `WINDOW_ON_LEAD` is calibration; a CGB WX
  write lands one dot after an LCDC one, `GB_CONFLICT_WRITE_CPU` vs
  `GB_CONFLICT_LCDC_CGB`, which would fit WX only). A WY write is checked
  1-8 dots later on an 8-dot grid aligned to LCD-on (`wy_check_scheduled`,
  `wy_check_modulo`, CGB single speed), then `wy_just_checked` skips one
  window compare: phase-dependent, which the constant `WY_LEAD` (3-5 give
  the same manifest) only approximates. Pan Docs and GBCTR give no
  write-to-effect timing. Details at the constants in `ppu/mod.rs`.
- **Result.** Full manifest: 0 regressions, no flips (86 / 39 / 3126).
  `diff-fingerprint.sh main`: 11 runs (6 ROMs), all writing WX or
  LCDC.5 in mode 3 and failing before and after: Mealybug
  `m3_wx_{4,5,6}_change`, `m3_lcdc_win_en_change_multiple(_wx)` (and
  their `#input` runs) and gambatte `weon_wx18_weoff_weon_wx80` (WX moved
  or the window re-enabled after its start no longer re-trigger it).
- **Cost.** `.text` 97,058 -> 96,990 B (`window_start` computed from
  `win_wx`, `LineTiming::wx` dropped; `Hw` stays 864 B). `synth:worst-case`
  464,590,933 -> 464,700,675 Ir (+0.02%: the per-line `win_wx` store);
  with the mid-line-SCX worst case (branch `bench-worst-case-midline-scx`)
  476,334,993 -> 476,467,536 Ir (99.19% -> 99.22% of the 4.02 M Ir/frame
  line).

### Step E2 (late window disable and WY change, branch `ppu-window-e2`)

- **Rule 1, LCDC.5 clear during the window stall (CGB).** E1 kept the
  full 6-dot stall once it began. On CGB, clearing LCDC.5 during the
  stall ends it: each remaining stall step outputs a pixel instead
  (gambatte ppu.cpp `StartWindowDraw::inc`, `!lcdcWinEn(p) && p.cgb`;
  read for behaviour only, GPL). SameBoy display.c doesn't model it (its
  DMG-only pixel insertion is a different glitch). The test ROMs show it:
  `late_disable_{0,1,2}` move the write 4 dots at a time against a fixed
  STAT read; dmg08 gives out0/out3/out3, cgb04c out0/out0/out3.
  `window_write` now trims the window stall to the dots before the write
  plus `WINDOW_OFF_LEAD` (0) and moves `end` with it. Pixels unchanged
  (the renderer still draws the window from the decided start).
- **Rule 2, WY write after the line's WY check.** The WY == LY condition
  was checked only at mode-3 start (`latch`) and on writes, with the new
  value, so WY changed from LY to FF in mode 2 lost the match. Hardware
  samples it near the line start and the match is sticky for the frame
  (SameBoy `wy_check` at each line start; gambatte `weMaster` at the
  previous line's cycle 454 for lines 1-143, at cycle 2 of line 0).
  `Ppu::wy_write` (cold, bus FF4A before the store) compares the old WY
  once past the line's check point `WY_CHECK_AT` = [7 (line 0), 3
  (others)] dots as `at` counts them; the 4-dot gap is gambatte's.
- **Calibration** (full manifest vs main + rule 1 where noted):
  `WINDOW_OFF_LEAD` -3/-2/-1 -> 15 flips with 15/12/5 regressions, 0 ->
  15/0, 1/2/3 -> 5/3/2 flips, 0 regressions. `WY_CHECK_AT` with one value
  for all lines (lead form): 7 -> 6 flips / 0, 8 -> 2/0, 3-6 -> 2-4
  regressions (`late_wy_1`, `late_wy_lcdoffset1_1`, line 0), 0-2 and
  -2..-12 -> 2-8 regressions; split [line 0, others]: [7, 3] -> 10 / 0,
  [7, 4] 8/0, [7, 5] 7/0, [8, 3] 8/0, [7, 1], [7, 2], [6, 3] -> 2
  regressions.
- **Result.** 25 fail->pass, 0 regressions: rule 1, 15 CGB
  `window/late_disable*_1` / `scx2_0` / `late_scx_late_disable_1` (out0);
  rule 2, 10: `arg/late_wy_{1,2}toFF_{2,3}`,
  `arg/late_wy_1toFF_{,ds_}lcdoffset1_2`, `{arg/,}late_wy_2`,
  `late_wy_{,ds_}lcdoffset1_2`. `diff-fingerprint.sh main`: exactly those
  25. Tests: `window_writes_in_mode_3_move_its_stall` (LCDC.5 clear at
  dots 160-166 of a stall at 160), `wy_write_after_the_line_check_keeps_the_old_match`;
  both mutation-checked (rule off, lead 1; [999, 999], [3, 3], [7, 7]).
  `window_pixels_follow_the_timing_decision` expected the old full stall
  for a clear 2 dots in; now 2 stall dots.
- **Not done.** Double-speed `late_wy_1toFF_ds_2`, `late_wy_ds_2`, the
  `late_wy_FFto*`/`10to0` new-value timing (WY_LEAD, phase-dependent in
  SameBoy), `late_enable_afterVblank`, `late_wx_ff_*`, the WX = A6 /
  object X = A7 edge, re-trigger (`on_screen/*`, Mealybug `m3_lcdc_win_*`).
- **Cost.** `.text` 96,990 -> 97,198 B (+88 rule 1 in `window_write`,
  +120 `wy_write`; folding it into `check_wy_at` as a flag measured +54 B
  more; 94,450 B after rebasing on the cold-code cut 65dc4a9). Bench: all `check-bench.sh` rows +0.00% before the rebase (`synth:worst-case`
  476,467,536 -> 476,467,546 Ir; `cgb-acid2` +4,748).

### HBlank interrupt vs stalls at pixel 159 (WX = A6 / object X = A7, branch `m9-win-a6`)

- **Rule.** Stalls at the last pixel (x = 159: the window at WX = 166,
  objects at X = 167) delay the STAT mode-0 change but not the HBlank
  (mode-0) interrupt. gambatte (behaviour only) requests it at xpos 166
  (`predictedNextXposTime(lcd_hres + 6)`) and changes STAT at xpos 168
  (`xpos168`), so a stall at xpos 167 falls between them; SameBoy
  display.c has the window half (`wx_166_interrupt_glitch`: interrupt one
  dot after a WX = 166 trigger). The `out` pairs pin it: every
  `m3stat` variant already passed and only the `m0irq_2` / `m0enable`
  ones failed (interrupt late by the stall); `10xposA6_m0irq_*` (stalls
  at x = 158) pass with and without the rule, so they still delay it.
- **Model.** `LineTiming::edge` = stall dots at x = 159 (`edge_dots`,
  cold, called only when the last stall is at 159). `end` is now the
  interrupt's end (the walk's `End`/`HBlank`, `mode_source` for STAT
  writes, `mode3_pixel`); `stat_end()` = `end + edge` for the STAT mode
  bits, VRAM/OAM/palette blocking, HDMA's `mode0_start` and the mid-line
  write windows. The walk is unchanged. A CGB LCDC.5 clear that cuts a
  window stall at x = 159 shortens `edge`, not `end`. The line is drawn
  at the walk's `End`, so a register write in those last stall dots
  misses pixel 159.
- **Result.** 10 fail->pass, 0 regressions: `m0enable/enable_wxA6_2x_spxA7_*`
  (6), `window/m2int_wxA6_{,spxA7_}m0irq_2`, `m0irq2_2`,
  `sprites/10spritesPrLine_10xposA7_m0irq_2`. Test
  `hblank_interrupt_skips_the_stalls_at_the_last_pixel`, mutation-checked
  (edge 0, threshold 158, no trim adjust, `mode_source` on the STAT end).
- **Cost.** `.text` 95,720 -> 96,004 B (`set_stalls` +40, `edge_dots`
  118, `window_write` +48). Bench vs main: `cgb-acid2` +0.08% (+2.39% of
  the 3% guard), `synth:cgb-raster` +0.18% (the `stat_end` add in the
  palette fast path), `synth:worst-case` -0.00%. Counting the x = 159
  dots per push instead measured `cgb-acid2` +0.50%.
- **Found, then fixed (branch `m9-end-reseek`).** A mode-3 write that
  moved `end` (`window_write`, the SCX discard move in `hold_fetch`)
  didn't move the walk's `End` already placed at `Latch`: the HBlank
  interrupt (and the draw) stayed at the old end, and a sync between the
  two ends tripped the `debug_assert` in `sync` (probe: WX 0x47 -> 0xFF
  at dot 100 of line 5). Now `Ppu::end_moved` (cold, called only from
  those write paths) moves a pending `End` cursor to the new end; a write
  only moves stalls or discard pixels still to come, so the new end is
  after it. Test `mode3_writes_move_the_hblank_interrupt` (window goes /
  appears, CGB LCDC.5 cut of the stall, discard stopped earlier /
  wrapped later), seen failing per call site (old-end interrupt in
  release, the `sync` assert in debug). Full manifest: no flips, 0
  regressions; `diff-fingerprint.sh main` identical (no ROM in the
  corpus hits it observably). `.text` +94 B (96,098).

### Step F (object stalls vs mid-line writes, branch `ppu-step-f`)

- **Rule 1, objects left of tile 0.** SameBoy (display.c,
  `x_for_object_match`) fetches an object when the line position reaches
  X - 8, the fine-scroll discard counted (tile 0 starts at -(SCX & 7)).
  An object with X - 8 < -(SCX & 7) (X = 0 included: it matches before
  the discard) is fetched while the 8 junk pixels before tile 0 are
  popped, after tile 0's fetch and before tile 1's. Its stall is clamped
  to pixel 0 in `LineTiming`, where `hold_fetch` counted it as finishing
  tile 1 (`stalled`), so an SCX/SCY/LCDC write in it held tile 1 with the
  old value. `hold_fetch` now skips those stalls (the object's X from the
  renderer's fetch order; cold, looked up only where it decides `end`).
  Objects at -(SCX & 7) <= X - 8 < 0 stall inside tile 0 and still finish
  tile 1 there. Not covered by a test: the window stall's index offset
  when the window starts before such an object (WX <= 7).
- **Rule 2, stall alignment.** The stalls' tile phase is the discard
  count, not the live SCX: a discard move in `hold_fetch` rebuilds the
  stalls with `n & 7` (`10spritesPrLine_late_scx4_ds_1`), and
  `window_write` rebuilds them with `Renderer::fine`, so a WX/WY/LCDC.5
  write after a mid-line SCX write no longer re-times objects already
  drawn (M8 review low).
- **Rule 3, a second write in one tile.** A write landing after a held
  tile's planes were read, with the same `end`, was dropped, so
  `mix_planes` took the planes read between the writes from the live
  registers. It now pushes an empty part carrying the values they were
  read with; `ObjRank::hold` grows to three parts so the earlier part
  survives the next tile's write (Mealybug `m3_scy_change` lines with
  X = 0..7 objects, AGE `m3-bg-lcdc-ds`).
- **Calibration.** None: no new constant.
- **Result.** Full manifest vs main: 5 fail->pass, 0 regressions:
  gambatte `scx_during_m3_spx{0,1}`, `scx_attrib_during_m3_spx1_ds`,
  `sprites/space/10spritesPrLine_late_scx4_ds_1`, AGE `m3-bg-lcdc-ds`.
  `diff-fingerprint.sh main`: 15 runs (11 ROMs): those 5, the three
  `*spx2*` below, and Mealybug `m3_lcdc_bg_map_change` (466 -> 402 px),
  `m3_lcdc_tile_sel_change` (846 -> 782), `m3_scy_change` (560 -> 189),
  failing before and after. In `m3_lcdc_tile_sel_change` the lines with
  an object at X = 5-7 got worse (192 px; X = 0-4 better, 256 px): an
  LCDC.4 write 5 dots after pixel 0 hits the high plane under
  `LCDC_TILES_LATE` = 1 (calibrated on lines without such objects), but
  hardware shows the old plane there. `LCDC_TILES_LATE` = 0 fixes those
  lines and regresses 14 gambatte `bgtiledata_spx*`: unresolved
  (SameBoy has the same fetch-to-pixel phase in both cases).
- **Still failing.** `scx_during_m3_spx2`, `spx2_ds`,
  `scx_attrib_during_m3_spx2_ds`: 8 px each, screen x 0 of the object
  lines, the object's color 2 from OBJ palette 0, which no ROM writes.
  The cgb04c captures show 0x21926C there (that console's power-on OBJ
  palette RAM; we start at 0, docs/reference/cgb-palettes.md). The timing
  part of these ROMs passes. Not fixable by timing; matching it means a
  power-on OBJ palette RAM pattern (gambatte ships a dump from
  `cgb_objp_dumper`; GPL, behaviour reference only), a human decision.
- **Tests** (mutation-checked): `object_left_of_tile_0_stalls_before_tile_1_is_fetched`
  (rule off; junk test without the fine scroll), `object_stalls_follow_the_discard_not_the_live_scx`
  (no rebuild on a discard move; live SCX in `window_write`),
  `second_scy_write_in_a_tile_keeps_the_planes_read_between` (same-end
  rule off; two parts).
- **Cost.** `.text` 94,980 -> 95,242 B (+262: `hold_fetch` +~200, the
  third part). Bench vs main (x86 Ir): `synth:worst-case` 475,843,415 ->
  476,391,802 (+0.12%; ~99.2% of the 4.02 M line), `synth:cgb-raster`
  322,710,305 -> 323,084,793 (+0.12%; +0.35% vs the committed baseline),
  `cgb-acid2` unchanged (673,925,093). A first version that kept a
  per-stall junk mask in `set_stalls` cost cgb-acid2 +0.46%; the lookup
  moved to the cold `hold_fetch`.
