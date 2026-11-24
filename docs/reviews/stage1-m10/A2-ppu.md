# A2: Emulation correctness, PPU

Reviewer A2, tree at HEAD of `main` (49c0756). Read-only; no cargo run, so
every finding is from reading the code path end to end. Paths are relative
to `/home/jess/gbc-32/crates/gbc-core/src/` unless absolute.

Read: `ppu/mod.rs` 1-3182 (all non-test code), `ppu/edges.rs`,
`ppu/objects.rs`, `ppu/render.rs` 1-1500 (all non-test code), `bitset.rs`,
`memory.rs`, `bus.rs` PPU paths (reset/`boot_logo`, `sync_ppu`,
`lyc_irq_*`, `write_ppu`, `write_vram`/`write_oam`, `write_palette_data`,
`write_if`, `push_acknowledge*`, `read_io`, `read_vram`/`read_oam`,
`boundary`/`run_events`, `dma_ppu_event`, `vram_dma` head), `dma.rs`
`ppu_at`; ADR 0014, 0019, 0029, `docs/reference/stat-irq.md`,
`cgb-palettes.md`, the architecture PPU sections. `docs/design/*.md` only by
section heading and the parts the code cites. The pixel loops
(`fill_tiles`, `fetch_object`) were read for bounds and priority rules, not
audited bit by bit. The mid-line "hold" machinery (`hold_fetch`,
`tile_lead`, `tile_sel_latch`, `mix_planes`) was read for reachable
panics/loops and internal consistency, not re-derived against SameBoy dot
by dot.

Summary: 1 high, 2 medium, 8 low.

---

## Findings

### A2-01 CGB palette writes are dropped for as long as the LCD is off, if it was turned off inside mode 3   [severity: high] [confidence: high]
- Where: `ppu/mod.rs:1977-1979` (`Ppu::drawing`), `bus.rs:1164-1171`
  (`write_palette_data`), `ppu/mod.rs:2998-3007` (`write_lcdc`, LCD-off
  branch), `ppu/mod.rs:1433-1436` (`sync` returns early with the LCD off).
- What: `write_palette_data` drops a BCPD/OCPD write when
  `s.ppu.drawing() || s.ppu.palette_blocked(..)`. `drawing()` is just
  `matches!(self.cursor.pt, Pt::End)` and does not test `lcd_enabled()`.
  Turning the LCD off leaves `cursor` untouched (`write_lcdc` only resets
  `line`, `blanked`, `skip_frame`, `lyc_flag_off`), and `sync` returns
  before touching it while the LCD is off. `Ppu::cursor`'s own doc says it
  is "meaningless while the LCD is off". So if the LCDC write that clears
  bit 7 lands between a visible line's `Latch` (t = 89) and its `End`
  (mode-3 end, t = 256..~380), `cursor.pt` stays `Pt::End` and `drawing()`
  stays true until the LCD is enabled again (`seek`). Every palette-RAM
  write in that period is discarded (the index still steps). Reads are not
  affected (`palette_blocked` checks `lcd_enabled`), so read-back shows the
  old bytes.
- Scenario: CGB-mode game, LY = 40, t = 150 (mode 3): `xor a / ldh
  [rLCDC],a`, then the usual "LCD off, load tiles, maps and all 16
  palettes, LCD on" sequence. All 128 palette bytes are dropped; the scene
  comes up with the previous palettes (after reset: all-white BG, black
  OBJ). Turning the LCD off outside VBlank is harmless on CGB hardware and
  some CGB-only games and much homebrew do it without waiting for LY >= 144;
  a game that does it at an arbitrary time hits mode 3 on roughly 35-55% of
  attempts. Games that wait for VBlank never hit it (in lines 144-153 the
  cursor is never `End`), which is why no test ROM or the unit test
  `drawing_whenever_a_mode3_pixel_is_due` (LCD on only) sees it.
- Reference: Pan Docs, "LCD Color Palettes (CGB only)": palette RAM is
  inaccessible only during mode 3; with LCDC.7 clear the PPU is off and
  nothing is blocked. SameBoy `GB_lcd_off` clears `cgb_palettes_blocked`.
- Proposed change: reset the cursor in the LCD-off branch of `write_lcdc`
  (e.g. `self.cursor.pt = Pt::V0;`), which costs nothing on the hot path,
  or make `drawing()` `self.lcd_enabled() && ...`. Effort S, risk minimal
  (fingerprint-neutral for every ROM that turns the LCD off in VBlank).
  Test (Emulator level, like `lib.rs` `write_lcdc` helper): run to a
  visible line's t = 150, write LCDC = 0x11, write BCPS = 0x80 and two BCPD
  bytes, `peek(0xFF69)` after resetting BCPS: must read the new byte; see
  it fail first. Also assert `!ppu.drawing()` after LCD off in the existing
  unit test.

### A2-02 The STAT interrupt model is two reference models spliced per observer; the single-speed walk is a dot early at two points and five separate mechanisms compensate   [severity: medium] [confidence: high on the structure, medium on game impact]
- Where: `ppu/mod.rs:139-170` (`MODE_0_2`, `MODE_HOLD`, `MODE_1_STICKY`,
  `View`), `1016-1080` (`mode_source`), `736-740` (`m0_latched`),
  `889-909` (`early_point`), `1392-1419` (`next_event`, `late`),
  `2740-2809` (`update_at`, `early_cmp`), `2817-2902` (`write_stat`),
  `2917-2984` (`write_lyc`); `bus.rs:36-39, 813-863` (delayed LYC request),
  `957-991` (STAT two-phase write plus the `pulse` undo), `1190-1201`
  (`write_if`); `docs/reference/stat-irq.md` "Open".
- What: the walk's update points are SameBoy's `GB_STAT_update` call sites
  (positions), but the write behaviour is gambatte's, whose model is
  different in kind: its interrupt *events* evaluate copies of the STAT
  selects and LYC that were latched a fixed number of cycles before the
  event (`mstat_irq.h` `statRegChange`/`lycRegChange`, `lyc_irq.cpp`
  `regChange`). The code reproduces the latching per call site instead of
  as a mechanism:
  1. `MODE_HOLD` (STAT writes at 144:3-4 and 0:3 neither raise nor lower),
  2. `MODE_1_STICKY` (writes 1-2 dots before a VBlank comparison point),
  3. `m0_latched` + the `late`/`blocked`/`hold` fix-ups in `write_stat` and
     `write_lyc`,
  4. `early_point` (IF writes and STAT phase 2 treat 153:12 and 0:4 as
     "still ahead") and the `pulse && high` IF undo in `write_ppu`,
  5. `next_event` returning the line-0 pulse deadline one dot after the
     point, so that only the CPU sees it late,
  plus the delayed LYC-write request in IF bits 5-6 (A2-04). Mechanisms 4
  and 5 exist only because the single-speed walk keeps line 0's mode-2
  pulse at t = 4 and line 153's LYC=0 at t = 12 while gambatte's events
  (and this code's own double-speed walk) have them at 5 and 13.
- Scenario (acknowledged deviations with no ROM, all from the docs/code
  themselves): (a) `stat-irq.md`: "differs from gambatte for a write at 0:4
  with modes 1 and 2 selected: its view ends the mode-1 source, and the
  pulse then rises (no ROM)"; (b) line 153's LYC=0 request is visible to an
  IF read/dispatch at 153:12 for CPU phases with a boundary on that dot,
  one dot before gambatte's event, and only IF *writes* are corrected
  (`ppu/mod.rs:1389-1391`: "the test didn't fit this function's budget");
  (c) `write_stat` double-speed line-0 trigger "kept: no ROM tells"
  (`2887-2888`); (d) `was_quiet` re-seed of the LY=LYC line uses the
  current LYC (`2845-2853`). Each is a one-M-cycle interrupt-timing error
  in a narrow window, so game impact is low, but every new ROM in this area
  has been fixed by adding another view-specific exception (ADR 0029 has
  five amendments in one day, +2.7 KB `.text`).
- Reference: gambatte `mstat_irq.h`, `lyc_irq.cpp`, `video.cpp` (behaviour
  only); SameBoy `display.c` `GB_STAT_update`; the project's own
  `stat-irq.md` "Open" section, which states the principled fix.
- Proposed change: make the latch the mechanism. Keep the lazy walk, but
  give `Ppu` a second copy of `(stat_select, lyc)` as the interrupt events
  see them ("event view"), plus the clock from which a write's value
  reaches that view (STAT: access + 2 T; LYC: access + 5 T + 1 - ds on CGB;
  gambatte's constants). `point_irq` evaluates with the event view; a write
  inside the latch window updates the CPU-visible registers immediately and
  the event view when the walk passes its landing dot (one more `Pt`, or a
  check in `sync`'s rare path). Then move the single-speed walk points to 5
  and 13 as in double speed. This should delete `MODE_HOLD`,
  `MODE_1_STICKY`, `m0_latched` and its fix-ups, `early_point`, the
  `late` deadline, the `pulse` undo and, with the LYC landing delay, the
  IF-bit pending request. Effort L, risk medium: the whole
  `gambatte/{m0enable,m1,m2enable,lycEnable,miscmstatirq,lcd_offset,ly0,
  lyc153int_m2irq}` families are the regression suite, and the previous
  attempts in `stat-irq.md` list exactly which rows move. Worth doing only
  as one step, not per ROM. If it is not done, at least record in ADR 0029
  that the single-speed/double-speed asymmetry is a known modelling gap.

### A2-03 The mid-line state lives in three structs and its invariants are held by `%` masks rather than types   [severity: medium] [confidence: high]
- Where: `ppu/mod.rs:313-353` (`LineTiming`: `stalls`, `n_stalls`,
  `win_stall`, `prev_wx`, `edge`, `wy_late`), `memory.rs:137-195`
  (`ObjRank`: `hold`, `next`, `sel_latch`, `sel_end`, `win_zero`),
  `ppu/render.rs:248-297` (`Renderer`: `win_wx`, `window_skip`, `bg_fine`,
  `slow`); masks at `ppu/mod.rs:490, 494, 510, 1659, 2213, 2217, 2225,
  2233, 2245, 2298, 2550, 2554`; unmasked slices at `522`, `1845`, `2690`.
- What: (1) One hardware object, the fetcher's per-line state, is split
  across `Ppu`, `Renderer` and the caller's `Memory` because `Hw` has a
  self-imposed 864-byte size assert (`lib.rs:836`; ADR 0019 "would push
  `Hw` over its 864 B size assert"; `bus.rs:833` "`Hw` has no room for a
  field"). The split is the reason `Ppu::draw` and friends take
  `&mut Memory`, why `obj_size_fetch` patches VRAM (A2-06), and why the
  delayed interrupt lives in IF (A2-04). (2) Indices that would be a bug if
  out of range are wrapped instead of checked: `NO_STALL` is 0xFF and
  `0xFF % 11 == 2`, so `set_stalls_retriggered` (`2213-2217`) would copy
  stall 2 over the window's stall if `prev_stall()` ever returned
  `NO_STALL`, and `hold_fetch` (`1659`) reads stall 2's dots as
  `win_dots` before checking `win_stall != NO_STALL`. Three other sites
  slice `stalls[..n_stalls]` unmasked and would panic instead. I traced the
  callers and could not construct a state where `n_stalls > 11` or
  `prev_stall()` misses (the stall count per object is fixed for the line
  and `retrigger` bails at 11), so this is not a live bug; it is that the
  safety argument is non-local and nothing asserts it.
- Scenario: none reachable found. The risk is the next mid-line fix: e.g. a
  second re-trigger per line (the code documents "one re-trigger per line"
  as an approximation) would make `n_stalls` 12 and panic in
  `pixels_before` on the next `flush`, on the MCU a hard fault.
- Proposed change: (a) move the per-line fetcher state into one
  `LineState` struct owned by one place (in `Memory` if `Hw` must stay
  small, or lift the 864 B assert: `Hw` is a `static` on the MCU, not a
  stack object); (b) replace the `%` masks by a small `Stalls` type with
  `len: u8` and `push`/`insert`/`get(i) -> Option` (bounds-checked once),
  and `Option<u8>`-like accessors for `win_stall`/`prev_stall`; (c) add
  `debug_assert!(n_stalls as usize <= stalls.len())` in `finish`. Effort M,
  behaviour-preserving (fingerprint identical), size impact to be measured.

### A2-04 The delayed LYC-write interrupt in IF bits 5-6: works for its ROM, three ways to lose or leak it   [severity: low] [confidence: high]
- Where: `bus.rs:36-39` (`LYC_IRQ_PENDING`, `LYC_IRQ_PHASE`), `813-863`
  (`sync_ppu`, `lyc_irq_delay`, `lyc_irq_due`), `996-1006` (`write_ppu`
  0x45), `1053-1056` (`schedule_ppu` at the end of every other
  `write_ppu`), `2012-2014` (`interrupt_flags` returns the raw byte),
  `2054-2056` (`set_interrupt_flags` masks with `INT_MASK`);
  `cpu/mod.rs:169-173`.
- What: the request is parked in the stored IF byte and delivered by the
  next `sync_ppu` whose M-cycle parity matches; the only thing that
  guarantees a `sync_ppu` at access + 8 is the PPU deadline being lowered
  to `now + 5` in `lyc_irq_delay`, which then returns *without*
  `schedule_ppu`. (1) Any `write_ppu` in the next M-cycle ends with
  `schedule_ppu()`, which overwrites that deadline with the walk's next
  event; the request then waits for an unrelated PPU event (up to a line
  later). Reachable only by back-to-back write M-cycles: `PUSH`/`CALL`/
  interrupt dispatch with SP = FF46 (FF45 then FF44, whose arm is `{}` but
  still falls through to `schedule_ppu`). (2) `Ctx::set_interrupt_flags`
  clears bits 5-6; it is dead today because `Ctx` overrides
  `push_acknowledge`, but it is the trait's documented way to write IF.
  (3) `Bus::interrupt_flags` exposes the raw bits; every caller masks with
  `INT_MASK` today. (4) If the LCD is turned off while a request is
  parked, no PPU event comes until it is turned on. All four are contrived
  for a game; the point is that correctness depends on every present and
  future IF reader/writer knowing about two hidden bits.
- Reference: gambatte `LCD::lycRegChange` (`memevent_oneshot_statirq` at
  `cc + 5`, CGB single speed; behaviour only); CGB04C
  `lycEnable/lyc_ff45_trigger_delay_2`.
- Proposed change: store it where it belongs: a `lyc_irq_at: u64` (or a
  1-byte countdown in M-cycles) in `Ppu`, folded into `next_event`'s
  result (`min`) and delivered in `Ppu::sync`. That removes the parity
  bit, the `min(deadline, now + 5)` special case and the early return.
  `Ppu` has six separate `bool`s (`first_frame`, `lyc_flag_off`, `blanked`,
  `skip_frame`, `bus_live`, `attr_on`) that pack into one byte if `Hw`'s
  size is the obstacle. With A2-02 done it disappears entirely. Effort S-M,
  risk low; test: the existing ROM plus a unit test "PUSH with SP = FF46
  still delivers at access + 8" (seen failing first).

### A2-05 An LCDC.5 clear early on the WY line loses the WY condition for the frame   [severity: low] [confidence: medium-high]
- Where: `bus.rs:939-951` (LCDC write: `write_lcdc`, then `check_wy_at`
  with the *new* LCDC), `ppu/mod.rs:2004-2011` (`check_wy_at`),
  `2019-2027` (`wy_write`, the pre-check that exists for WY writes only),
  `2071-2075` (`latch`: the line's first WY check, at t = 89),
  `ppu/render.rs:332-336`.
- What: the sticky WY == LY match needs LCDC.5 set at the check. The only
  unconditional check for line L is in `latch` (t = 89); before that, a
  check happens only on writes. For WY writes the bus calls `wy_write`
  first, so a match that the hardware already sampled at the line start
  (`edges::WY_CHECK_AT`: dot 3, line 0: dot 7) is kept. There is no such
  pre-check when LCDC.5 is cleared: with WY == LY and LCDC.5 set at the
  line start, clearing LCDC.5 at t in [3, 89) means no check ever sees
  both, and `wy_triggered` stays false for the frame.
- Scenario: WY = 40, LCDC.5 set. On line 40 at t = 20 (mode 2, e.g. in an
  LYC = 40 handler) the game clears LCDC.5; on line 48 it sets it again.
  Hardware: the window draws from line 48 (the WY latch was taken at line
  40's start). Here: no window for the rest of the frame.
- Reference: gambatte `ppu.cpp` `M2_LyNon0` (`weMaster |= lcdcWinEn &&
  wy == ly + 1` at line cycle 454 of the previous line = our t = 3;
  behaviour only); SameBoy `display.c` `wy_check` at each line start. The
  code's own `WY_CHECK_AT` comment states when the line's check "has seen
  WY".
- Proposed change: in `write_ppu` 0x40, when bit 5 goes 1 -> 0, call
  `s.ppu.wy_write(now, self.mem, double)` before `write_lcdc` (it checks
  with the registers as they still are, only past `WY_CHECK_AT`). Effort S,
  risk low (stricter in one direction only; run `gambatte/window/**` and
  Mealybug `m2_win_en_toggle`). Test: Emulator-level, the scenario above,
  assert the mode-3 length of line 49 includes the window's 6 dots; seen
  failing first.

### A2-06 `obj_size_fetch` passes a parameter by patching VRAM   [severity: low] [confidence: high]
- Where: `ppu/mod.rs:2676-2734`, in particular `2717-2732`;
  `ppu/render.rs:967-983` (`fetch_next_object`), `1019-1099`
  (`fetch_object`).
- What: to make one object's fetch read its low plane with the old height
  and its high plane with the new one, the code overwrites
  `mem.vram[old_hi]` with `mem.vram[new_hi]`, runs the normal fetch with
  the old LCDC, and restores the byte. I checked the aliasing: `old_hi` and
  `new_hi` are both odd addresses (`object_row | bank) + 1`), the low
  plane is even, bank is included, `old_hi == new_hi` is a no-op, and
  nothing between patch and restore can observe VRAM. So it is correct
  today. It is still the wrong shape: emulated memory is mutated (the
  function needs `&mut Memory` only for this), any future early return
  between the two stores corrupts VRAM permanently, and the reason it
  exists is to avoid adding a parameter to `fetch_object`.
- Proposed change: give `fetch_object` the high-plane LCDC (or the high
  byte's address) as an argument: `object_row(.., lcdc_lo)` for `lo`,
  `object_row(.., lcdc_hi) + 1` for `hi`; the hot caller passes the same
  value twice (the compiler folds it, `fetch_object` is `inline(always)`).
  Effort S, fingerprint identical; measure `.text` (the reason given for
  the current form is the size budget).

### A2-07 A palette/SCX/SCY/LCDC write that lands one dot *before* its access is flushed after the PPU has already run the access dot   [severity: low] [confidence: high]
- Where: `bus.rs:925-934` (LCDC, double speed: `run_ppu_to_call(now)`,
  then `flush(at = now - 1)`), `1023-1036` (`at = now - 1` for BGP/OBP0/
  OBP1 in single speed and SCX/SCY in double speed; `run_ppu_to_call(
  now.max(at))` syncs to `now` first).
- What: the PPU is synced to the access clock, then the line is flushed up
  to the earlier landing dot. If the walk's `End` point is exactly at
  `now`, the sync has already drawn pixels up to 160 with the old value, so
  the pixel(s) output at `now - 1` keep the old value although the write
  landed before them.
- Scenario: DMG-compat game, BGP written in mode 3 with the access on the
  line's mode-3 end dot: pixel 159 gets the old shade (hardware: new). One
  pixel at the right edge, one CPU phase in four; cosmetic.
- Proposed change: `run_ppu_to_call(at.min(now))`, flush/hold at `at`,
  then `run_ppu_to_call(now)` when `at < now`. Effort S; expected
  fingerprint change limited to ROMs that write on that dot.

### A2-08 The window zero pixel is applied on CGB against SameBoy, from one ROM's picture   [severity: low] [confidence: medium]
- Where: `ppu/mod.rs:2329-2355` (`window_zero`), `ppu/render.rs:595-657`
  (`window_tile_edge`, `set_zero`, `render_zero`).
- What: SameBoy gates `insert_bg_pixel` with `!GB_is_cgb(gb)` (DMG only).
  The code applies it in both modes because Mealybug
  `m3_wx_4_change_sprites` expects it in its CGB C capture. Its timing
  ("counted from 0-5 pixels after `drawn` gives the same manifest") and the
  take-back rule are fitted to that one ROM. Internally it is consistent: I
  checked that it cannot fire with a static WX (z == window start), that a
  not-due call clears a pending one, that `ZERO_DUE` cannot recurse through
  `render_held`, and that a stale `ZERO_DUE` with `win_zero == 0` is a
  no-op. Any game that writes WX in mode 3 while its window is on screen
  gets a one-pixel color-0 notch when WX - 7 lands on a later window tile
  boundary.
- Reference: SameBoy `display.c` (`insert_bg_pixel`, DMG only); Mealybug
  tearoom `m3_wx_4_change_sprites` CGB C expected image.
- Proposed change: none to behaviour (ADR 0003 targets CGB C and the
  capture is hardware). Record in `docs/reference/` that this contradicts
  SameBoy and rests on one capture, so a second source (e.g. a CGB C run of
  `m3_wx_5_change`/`m3_wx_6_change`) can confirm or refute it.

### A2-09 `Memory::clear` leaves part of the PPU's line state behind   [severity: low] [confidence: high]
- Where: `memory.rs:117-134` (no reset of `obj_rank.hold`,
  `obj_rank.win_zero`, `dma_read_clears`).
- What: `reset`/`load` rely on `clear` giving the same value as
  `Memory::new`. `hold` and `win_zero` are not cleared. I checked that it
  is unobservable today: `latch` zeroes `hold[2].end` before the first
  draw, `hold_fetch` then rebuilds `hold[0..2]`, and `win_zero` is read
  only under `ZERO_DUE`, which `Renderer::new` clears. But the determinism
  rule ("same ROM and inputs, same output") holds here by that chain of
  reasoning, not by construction, and the `memory_new_is_all_zero_bytes`
  test does not cover `clear`.
- Proposed change: clear them (three stores) and add a test that
  `clear()` on a dirtied `Memory` equals `Memory::new()` (it derives
  `PartialEq` under test). Effort S.

### A2-10 TILE_SEL glitch latch: the "unknown" states are answered by the previous calibration   [severity: low] [confidence: medium]
- Where: `ppu/mod.rs:1778-1811` (`tile_sel_latch`), `1720-1741`
  (`hold_fetch`, `planes` bits 2-7), `ppu/render.rs:739-788`
  (`tile_sel_glitch`), `823-858` (`carry_sel`), `memory.rs:160-177`.
- What: the latch ("last byte read with 8000 addressing") is tracked only
  partly: `sel_end.1` is 0 (unknown) after reset and 0xFF (unknown: BG
  reads since) whenever LCDC.4 is set at a line start. In those states a
  set-glitch returns `new` or `old` depending on whether an object stall is
  near (`planes >> 4 == 0`), which is the earlier `LCDC_TILES_LATE` fit
  relabelled, not hardware. The known states are fitted to three ROM
  families (Mealybug `m3_lcdc_tile_sel_change`, `_win_change`, gambatte
  `bgtiledata_spx*`). For a game it means: an LCDC.4 set on one of two
  specific dots of an 8-dot tile fetch shows one tile's bit plane from an
  unrelated byte. Real CGB A-C hardware does that too (CGB D/E and AGB do
  not), so the *existence* is right; the *content* is a guess in the
  common case (LCDC.4 normally set, so the latch is "unknown").
- Proposed change: either track the latch honestly (the last BG high-plane
  byte fetched is available at every `Fetch::row`; one `u8` store per tile
  only while a mode-3 LCDC.4 write is pending is not possible lazily, but
  `tile_byte_8000` can recompute it at the write, as `tile_sel_latch`
  already does for the clear), or drop the set-glitch in the unknown state
  and draw the tile with the new select (what SameBoy does in single
  speed). State which in the design doc. Effort M; affects only the three
  ROM families.

### A2-11 `stat_during_dma`, the object-fetch OAM read dot and the DMG-mode boot position are unpinned guesses on paths games can reach   [severity: low] [confidence: medium]
- Where: `ppu/mod.rs:861-872` (`stat_during_dma`: "Gambatte has no such
  rule; no test ROM we have checks it"), `2536-2575` (`fix_attrs`: "`d - 5`
  dots into the stall ... untested"), `106-123` (`POST_BOOT_POS` for
  DMG-compat carts: "no test we have measures it"), `bus.rs:194-201` (OBJ
  palette RAM zero after boot), `209-217` (BCPS/OCPS post-boot in CGB
  mode).
- What: three behaviours come from one emulator with no hardware
  cross-check. `stat_during_dma` changes what a STAT poll returns in mode 2
  whenever an OAM DMA runs outside VBlank (mode 2 reads as mode 0), which a
  "wait for mode 0/1 then write VRAM" loop would take as permission while
  VRAM is about to be blocked. SameBoy has it in `GB_STAT_update`; gambatte
  does not.
- Proposed change: no code change without evidence. Add them to a "single
  source, unverified" list in `docs/reference/` and to the real-game
  checklist (any game that runs OAM DMA from an HBlank/LYC handler).

---

## Answers to the brief's direct questions

**Does it panic, loop or index out of range under arbitrary writes?** I
found no reachable case. Checked: every `stalls`/`order`/`objects` index
(A2-03), `scan_to` progress (`edge >= c` for each `PpuView`, so
`scan.next` always advances), `next_event`/`sync` termination (every path
reaches `End` or `VBlank`), `at()` with a clock before `frame_start`
(`now - 1` landing dots: wraps to a line >= 144, every user then returns
"not in mode 3"), repeated SCX writes in the discard (an extension needs
writes < 8 dots apart; single speed cannot chain them, double speed at most
~7 register-to-memory writes, so `end` stays below 456), `render_held` /
`render_zero` mutual recursion (bounded: `hold` is taken, `ZERO_DUE` is
cleared before the inner call), `u8` truncations (`wy_late`, `end.min(176)`).
Arithmetic that could underflow in `usize` (`win_from - window_start()`)
is guarded by the activation order. In a debug build an invariant break
would panic on overflow; in release it wraps.

**The six flagged items.**
- `obj_size_fetch` VRAM swap: correct, wrong shape (A2-06).
- `boot_logo`: matches SameBoy's `cgb_boot.asm` layout (24 tiles from 8010,
  each nibble doubled into 4 bytes, plane 0 only, (R) at tile 0x19), reads
  the cartridge header, detects nothing. Two caveats: SameBoy's boot ROM is
  a reimplementation, so "tile map rows 5-14 cleared, bank 1 tiles 0-64
  cleared" is its behaviour, not a dump of the original; and OBJ palette
  RAM is left zero. Fine as is; listed under A2-11.
- `LYC_IRQ_PENDING`: A2-04.
- `next_event` pulse deadline: sound for what it claims (I checked the
  write-side sync posts it at the point, `write_if` agrees through
  `early_point`, HALT wake-up moves only for the CPU phase that has a
  boundary on the dot). It is a symptom of A2-02.
- `window_zero`: A2-08.
- TILE_SEL latch in `Memory`: placement is A2-03, content is A2-10.

**Constants calibrated to ROMs rather than derived** (value: what it
should be derived from):
- `POST_BOOT_POS` = 144:171: the boot ROM's cycle count from its LCD
  enable to the jump to 0100 (per cartridge mode; DMG-compat path differs).
- `DISCARD_LEAD` = 8, `FETCH_SAMPLED`/`FETCH_LOW`/`FETCH_HIGH` = 1/3/5,
  `LCDC_TILES_LATE` = 1, `FETCH_COL` = 0: the fetcher state machine's
  register-sample dots (SameBoy `advance_fetcher_state_machine`,
  `render_pixel_if_possible`) plus each register's CPU write landing dot
  (`cgb_conflict_map`); today each is a manifest sweep ("0 -> 30 flips,
  1 -> 45, 2 -> 39").
- `WINDOW_LEAD` = 0, `WINDOW_ON_LEAD` = 1, `WX_ON_LEAD` = 0,
  `WINDOW_OFF_LEAD` = 0, `WY_LEAD` = 5, `WY_LEAD_DS` = 1: the window
  comparator's per-dot test (WX == position + 7 with LCDC.5 and the WY
  latch) and SameBoy's `wy_check_scheduled` 8-dot grid; the code says "no
  reference pins the values". `WY_LEAD` stands in for a phase-dependent
  1-8 dot delay with a constant.
- `OBJ_LOW_READ`/`OBJ_HIGH_READ` = 4/2: SameBoy has 3/1; "+1 fits CGB C"
  from two Mealybug ROMs.
- `edges::WY_CHECK_AT` = [7, 3], `WY_LAST_AT` = 455, `WY_DS_LATE`: gambatte's
  `weMasterCheck*LineCycle` mapped by sweep ("7 -> 6 flips / 0
  regressions").
- `edges::M0_STAT_LATCH` = [2, 1], `M0_LYC_LATCH` = [6, 2], `MODE_HOLD_SS`,
  the `near()` window of `MODE_1_STICKY`, `LYC153_SS_UNTIL` = 8,
  `LYC_DEFER_UNTIL_*`: all instances of gambatte's "register copy latched
  N cycles before the event" (A2-02); derive from one delay per register.
- `edges::CMP_WALK` = 5, `L153_CMP` = 5, `l153_zero_walk`, `ly0_pt`/
  `ly0_pulse` (+1 in double speed only), `next_event`'s `late`: event
  times; single and double speed should not differ in whole dots for a PPU
  that does not change speed (the code's own header says so).
- `edges::PALETTE_EXTRA_SS/DS` = 4/2, `VRAM_FROM_*`, `OAM_*_FROM_*`: pinned
  by captures at several CPU phases and cross-checked against gambatte's
  formulas; these are the best-founded of the set.
- `LyCIRQ + 5` (`lyc_irq_delay`), `fix_attrs`'s `d - 5`, `LY_GLITCH` = 2.
- `LineTiming::edge` rule (stalls at pixel 159 do not delay the HBlank
  interrupt): two references agree in part; fitted to four ROM families.

**Common game-visible behaviour with no test** (unit or ROM):
- LCD turned off outside VBlank, then palette/VRAM/OAM loads (A2-01).
- LCD off/on with the window: WY latch and window line counter restart,
  STAT line carried high across the off period, first-frame line 0 without
  objects (only timing is tested, not pixels).
- LCDC.5 toggled on the WY line before mode 3 (A2-05); LCDC.2 changed in
  HBlank (only mode-2 and mode-3 changes are tested).
- STAT polling during an OAM DMA outside VBlank (`stat_during_dma`).
- Speed switch in the middle of a frame followed by raster effects (the
  AGE `lcd-align-ly` rows test LY only).
- `reset()`/`load()` in the middle of mode 3 with held fetches pending
  (A2-09).
- Hi-color style per-line palette writes in CGB mode are exercised only by
  the synthetic bench ROM, which checks cost, not pixels.
- Mid-line BGP in DMG-compat mode at the mode-3 end dot (A2-07).

**Would anything matter for commercial games?** A2-01 is the one I would
expect to see in the wild. Everything else found is either a one-M-cycle
interrupt shift in a window a few dots wide (A2-02, A2-04), a rare register
sequence (A2-05), or a cosmetic pixel (A2-07, A2-08, A2-10).

---

## Bigger-picture recommendations

1. **Fix A2-01 now and audit "fast-path predicates" for stated
   preconditions.** `drawing()` is a shortcut whose doc states its
   precondition ("the PPU, synced to some clock") but not the one that
   breaks it (LCD on). The same pattern exists in `palette_blocked`'s fast
   path (correct: it tests `lcd_enabled` and `mode3 != 0`) and
   `scroll_write`'s deferred flush (correct as far as I could trace). Give
   each such predicate a unit test that walks LCD off, LCD on first frame
   and reset. Value high, cost S.
2. **Replace the per-view STAT exceptions with latched event views**
   (A2-02, removes A2-04). This is the one place where "replicate two
   emulators until each ROM passes" has produced a model nobody can predict
   outside the tested CPU phases. Value medium (correctness in untested
   phases, about 2-3 KB `.text` and several hot-path branches back), cost L.
3. **One owner and real types for per-line fetcher state** (A2-03, A2-06,
   A2-09). Revisit the 864-byte `Hw` assert: it is the stated cause of
   three workarounds in this subsystem alone, and `Hw` is not on the stack
   on the target. Value medium, cost M, behaviour-preserving.
4. **Separate "derived" from "fitted" constants in `edges.rs` and
   `ppu/mod.rs`**, with one line each saying what would derive them. A
   sweep that flips N ROMs with no regression is evidence for a value, not
   for the model; several of these (window leads, `WY_LEAD`) are constants
   standing in for a phase-dependent quantity and will be wrong for CPU
   phases the suite does not reach. Value medium for Stage 2+ maintenance,
   cost S (documentation), and it feeds item 2.
5. **Pixel-level regression tests for LCD off/on and window sequences**
   that real games use (list above). The ROM suite is dense in STAT timing
   and sparse in plain state-machine coverage of LCDC.7/LCDC.5 sequences.
   Value medium, cost S-M.

## Looked at and found fine

- Line geometry: 456 dots, mode 2 at t = 4..84, minimum mode 3 172 dots,
  line 153 (LY 153 for 4 dots, comparison against 0 from t = 12), LY
  mid-carry reads, first line after LCD enable (no OAM scan, mode 0 then
  mode 3 at SameBoy's nominal 78 + 8).
- The update-point walk: `step`/`step_vblank`/`next_point`/
  `next_work_point` agree point by point for quiet and non-quiet, both
  speeds, frame wrap, speed switch re-seek; `end_moved` keeps the cursor
  consistent after every mode-3 write that moves the end.
- STAT blocking semantics in `IrqLine::eval` (OR of sources, rising edge,
  held LY=LYC line while the comparison is off), the line-144 mode-2
  quirk, LCD off keeping the coincidence bit and the line, LCD on comparing
  LY = 0.
- Event ordering: a CPU access at clock N runs after every PPU point at or
  before N (`tick`/`boundary`), so drawing at `End` sees VRAM/OAM/palettes
  exactly as blocked; `latch` only reads state whose writers sync first.
- Access windows: VRAM/OAM/palette blocking edges, FEA0-FEFF, palette index
  stepping on a blocked write, BCPS/OCPS bit 6, DMG-compat lockout.
- Mode-3 length: SCX & 7, window 6 dots (+1 at WX = 0 with fine scroll),
  the Pan Docs object penalty algorithm in `sorted_object_stalls`, objects
  stalling with LCDC.1 clear (CGB), X = 0 and X >= 168 handling.
- OAM scan: Y-only selection, 10-object limit counting off-screen objects,
  8x16, the `ObjectLines` cache invalidation on every OAM writer, the
  incremental scan under OAM DMA (terminates, bus tracking consistent).
- Object rendering: X-then-OAM priority in DMG-compat (rank 0), OAM-index
  priority in CGB mode via the rank buffer (the SWAR compare is correct for
  ranks 0-9), the winner's BG-priority bit hiding lower objects, flips,
  bank bit, palette bits, clipping at both edges.
- BG/window rendering: signed/unsigned tile data, map select, CGB
  attributes (palette, bank, X/Y flip, priority) via `ATTR_FETCH`, LCDC.0
  as BG enable (DMG-compat: BG and window color 0, objects on top) and as
  master priority (CGB), `mix` truth table, window start for WX 0-7 and
  166, window line counter advancing only on lines where the window
  starts, WY latch requiring LCDC.5 (SameBoy and gambatte agree).
- Palettes: BGP/OBP resolution at write time, BCPD/OCPD color and pair
  table updates, bit 15 masking, boot-time white BG palettes.
- `boot_logo` (see above), `bitset.rs` (generic flag-set macro; nothing
  PPU-specific, no issues).
