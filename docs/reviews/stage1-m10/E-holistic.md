# Review E: holistic design of gbc-core (HEAD 49c0756)

Read-only review. No cargo, no builds. Byte figures come from `nm -S` on the
existing `target/codesize/thumbv7em-none-eabihf/release/libgbc_core.rlib`
(built 07:14 today, two minutes before the docs-only HEAD commit; total
98,198 B, matching STATUS's 98,124 within CGU noise). The per-symbol list is
in `/tmp/claude-1000/-home-jess-gbc-32/771053a9-0ac5-45ad-953b-96007463309d/scratchpad/review/E-sizes.txt`.
Ir figures are from `docs/perf.md` / `perf/bench-baseline.txt`; anything
marked "estimate" is mine and unmeasured.

Size map used below (thumbv7em, bytes):

| Group | Bytes | What |
|---|---:|---|
| `Cpu::run` + `execute_cb` + rest of cpu | 34,126 | interpreter |
| PPU, all | 27,282 | 79 functions |
| - mid-line patch machinery | >= 7,296 | `hold_fetch` 1,432, `mix_planes` 704, `obj_size_fetch` 624, `window_write` 524, `set_stalls_retriggered` 510, `render_held` 500, `tile_lead` 472, `retrigger` 258, `render_zero` 252, `window_stall_at` 240, ... 23 symbols; excludes what is inlined into `latch`, `render`, `write_ppu` |
| - STAT/LY walk, reads, write views | ~7,058 | `update_at` 982, `sync` 930, `write_stat` 716, `write_lyc` 682, `next_event` 586, `seek` 562, `stat` 492, `write_lcdc` 426, `point_irq` 422, ... |
| - OAM DMA as the PPU sees it | ~2,742 | `scan_to` 984, `fix_attrs` 874, `dma_event`, ... |
| APU | 7,000 | |
| cart | 4,004 | |

## Short answers to the six questions

1. **Timing.** The core is a good lazy/closed-form design for the timer,
   APU, serial and the event table. It is not one model for the PPU and
   the CPU/PPU boundary: there the same facts are encoded as (a) pure
   read functions of position, (b) a hand-unrolled update-point walk in
   four copies, (c) per-`View` write predicates, (d) per-call-site T-cycle
   offsets, and (e) pending bits and latches. See E3, E4.
2. **PPU.** Mode timing is SameBoy's `GB_display_run` state positions in
   closed form; the STAT line is SameBoy's `GB_STAT_update` model with
   gambatte's per-event "latched register" predicates grafted on; pixels
   are a span renderer plus a closed-form reconstruction of SameBoy's
   fetcher/FIFO state at the dot of each mid-mode-3 write. That last part
   is >= 7.3 KB and ~1,000 lines, built after a dot-stepped slow path was
   rejected for an estimated 3-5 KB. A hybrid (fast spans on clean lines,
   dot-stepped fetcher for the dirty remainder of a line) would be simpler
   and more exact, with a bounded worst case, but slower on the one
   benchmark that currently defines the wall. See E1.
3. **Copied flaws.** E5 (gambatte's `prefetched_`), E7 (SameBoy/gambatte
   behaviours taken without a pinning ROM, calibrated leads), E4
   (two references' origins disagreeing by one dot, patched at the
   observer).
4. **Budgets.** The `.text` cap and the `Hw` size cap are not well founded
   and have visibly bent the design. The absolute Ir line is a proxy
   with unknown error treated as a hard wall. The ITCM-set check and the
   relative +3% Ir guard are the two worth keeping. See E2.
5. **Test net.** Strong for register-visible timing, weak for: audio
   samples, anything past 20 emulated seconds (2^32 base clocks is 17
   minutes), sync-placement independence of the PPU, and real-game
   raster output. Four cheap nets close most of it. See E6.
6. Target architecture and roadmap: at the end.

---

## Findings

### E1 Mid-line PPU is a closed-form image of a FIFO state machine, larger than the machine it avoids, with unbounded per-write cost   [severity: high] [confidence: high on the diagnosis, medium on the size/speed estimates]

- Where: `ppu/mod.rs:1593-1790` (`hold_fetch`), `:1793-1822`
  (`tile_sel_latch`), `:1839-1869` (`tile_lead`), `:1881-1901`
  (`scroll_write`), `:2163-2440` (`set_stalls`, `set_stalls_retriggered`,
  `window_stall_at`, `window_write`, `window_zero`, `retrigger`,
  `window_cuts_holds`, `end_moved`), `:2004-2070` (WY paths), `:2676-2738`
  (`obj_size_fetch`); `ppu/render.rs:556-870` (`render_held`,
  `render_zero`, `mix_planes`, `tile_sel_glitch`, `tile_byte_8000`,
  `carry_sel`); state in `memory.rs:138-186` (`ObjRank::hold`, `sel_latch`,
  `sel_end`, `win_zero`, `Held`); constants `ppu/mod.rs:355-455`.
  Design history: `docs/design/mid-line-ppu.md` section 4 (options) and
  the 13 implementation-note sections that follow.
- What: `docs/design/mid-line-ppu.md:164-168` rejected "SameBoy-style
  per-dot mode 3 for dirty lines" as "a new pixel pipeline (~3-5 KB) and
  ~5k+ Ir per slow line. Rejected for the budgets", in favour of a
  "~450-550 B" snapshot. What was then built, one ROM group at a time, is
  that pipeline's state computed backwards from the write dot:
  - which tile is in the FIFO and how far the next fetch has got
    (`tile_lead` walks the stall list to recover it; `FETCH_SAMPLED`,
    `FETCH_LOW`, `FETCH_HIGH`);
  - up to three "held" line parts with per-plane bits and glitch bits
    packed into `Held::planes` (bits 0-1 planes, 2-3 glitched planes, 4-7
    object slot for the TILE_SEL latch);
  - a TILE_SEL latch carried across lines as `(byte, key)` with key
    values 0, 1, 8-175 and 0xFF meaning four different things;
  - the window as a start x plus a second start (`prev_wx`), one
    re-trigger per line, none with 10 objects (the stall array has 11
    slots);
  - an object fetch re-run with one VRAM byte temporarily swapped (E10).

  The measured result is >= 7,296 B and nine calibration constants whose
  doc comments say "calibrated over the full manifest" with sweep tables
  (`LCDC_TILES_LATE`: "0 -> 30 flips, 1 -> 45, 2 -> 39"; `DISCARD_LEAD`:
  "7 -> 17 flips / 4 regressions, 8 -> 21 / 0"; `WINDOW_*_LEAD`:
  "Approximations ... no reference pins the values").
  Each constant is the offset between this code's coordinate origin and
  SameBoy's internal `position_in_line` / fetcher-state origin. In a
  stepped model they are not parameters at all: they are the order of
  statements in the per-dot loop.
- Scenario or measurement:
  - Size: >= 7.3 KB of patches against the 3-5 KB estimate that was
    rejected (allow 2-3x on that estimate, as STATUS warns: still no worse).
  - Cost is per write, not per line: `docs/perf.md` "Outside the bound"
    measures LCDC writes in double speed at 10.15 M Ir/frame (252% of the
    line), SCX at 7.84 M (195%), up from 6.1-6.3 M at stage1-m7. The
    patch path made the pathological case 25-60% slower.
  - Known capacity approximations that are artifacts of the representation,
    not of hardware: three held parts (`memory.rs:147-150`), one re-trigger
    per line and none with 10 objects (`ppu/mod.rs:2364-2366`), "a window
    stall inside tile j finishes tile j + 1 (approximation kept from step
    1)" (`:1832-1833`), tile cut by the window start "its planes as of its
    map entry (approximation)" (`:1746-1749`).
- Reference: Pan Docs "Pixel FIFO"; Mealybug "Comprehensive PPU
  documentation"; SameBoy `Core/display.c` `advance_fetcher_state_machine`,
  `render_pixel_if_possible` and its `mode3_batching_length` fast path
  (SameBoy itself is exactly this hybrid).
- Proposed change: a `ppu/mode3.rs` dot-stepped fetcher + FIFO used only
  for the remainder of a line after the first fetch-side write in mode 3
  (LCDC bits 1-6, SCX, SCY, WX, WY when it can still matter) and for whole
  lines OAM DMA touches. Clean lines keep `LineTiming` + `fill_plain` /
  `fill_mixed` untouched. Output-side writes (BGP/OBPx, LCDC.0) keep the
  flush-and-split path. The stepper produces the line's mode-3 end itself,
  so `set_stalls_retriggered`, `retrigger`, `window_stall_at`, `end_moved`
  and the lead constants go. Estimates (unmeasured): +5-8 KB new, -7.3 KB
  patches and most of the 2.7 KB OAM-DMA attribute fixing (`fix_attrs`,
  `obj_attr`, `attr_final`: an object fetched at its dot sees the DMA state
  directly); net about size-neutral, -1.5k non-test lines. Speed: clean
  lines unchanged; a dirty line costs a bounded ~15-25 Ir per remaining
  dot, at most ~4k Ir per line, about 0.6 M Ir/frame if every line is
  dirty from dot 84. So `synth:worst-case` (one SCX write per line) gets
  slower by an estimated 0.2-0.3 M Ir/frame (5-8% of the line: over the
  4.02 M line as drawn today), while the "outside the bound" programs drop
  from 7.8-10.2 M to an estimated 2.5-3.5 M. It trades a few percent on
  the common case for a hard bound on the worst one.
  Effort L, risk high: a literal SameBoy port would regress gambatte
  CGB04C ROMs SameBoy itself fails, so each calibrated fact has to be
  re-expressed as stepper state. Therefore: do not start by replacing.
  First build the stepper as a test-only oracle (roadmap step 8), in the
  pattern the project already uses for the timer (`timer/reference.rs` +
  `timer/differential.rs`), prove it on the Mealybug + gambatte m3 groups
  through a romtest feature switch, differential-test it against the
  patch path, and promote it only when Stage 2 gives a real cycle budget.
  Until then: freeze the patch machinery. No new mid-line special cases.

### E2 The byte and struct budgets, not hardware or speed, now decide where state lives and which rules get implemented   [severity: high] [confidence: high]

- Where (each site says so in its own comment):
  - `bus.rs:35-39`, `:820-866`: a delayed LYC-write interrupt is kept in
    IF bits 5-6 (`LYC_IRQ_PENDING`, `LYC_IRQ_PHASE` = parity of the
    write's M-cycle) because "`Hw` has no room for a field".
  - `hdma.rs:99-107`: `VramDma::switch_end` is "Not VRAM DMA state ...
    Here because it fits this struct's padding (the `Hw` size budget)".
  - `memory.rs:59-63`, `:132-137`: renderer fetch state (`hold`, `sel_latch`,
    `sel_end`, `win_zero`) lives in `Memory::obj_rank` because "a second
    [pointer] would grow `Ppu::draw` past its code-size budget";
    `dma_read_clears` is in `Memory` "for `Hw`'s size limit". `Memory`'s
    own invariant ("every field is an integer array ... power-on value is
    zero") is now carrying PPU line state that `Memory::clear` only
    partly resets (`hold` and `win_zero` are not cleared in `clear`,
    `memory.rs:112-129`; `latch` resets them per line, so unobservable
    today).
  - `ppu/mod.rs:236-244`: `IrqLine` is two bytes with packed bits because
    "three `bool`s grow `Hw` past its limit".
  - `ppu/mod.rs:1388-1391`: line 153's LYC=0 point "would fit the same
    rule [as the line-0 pulse], but no ROM needs it and the test didn't
    fit this function's budget". A timing rule was left out for 4 bytes of
    `next_event` headroom (586 of 590).
  - `ppu/mod.rs:1404-1406`: an Ir saving skipped because it "takes this
    past its thumbv7em code-size budget".
  - `ppu/mod.rs:1316-1345` (`point_irq` / `point_irq_rare`), `bus.rs:877-897`
    (`run_ppu_to` / `run_ppu_to_call`), `ppu/mod.rs:972-990`
    (`palette_blocked` re-implements `cpu_blocked` as a fast path): logic
    split or duplicated to sit under per-function numbers.
  - `ppu/mod.rs:351-353`: `LineTiming::wy_late` is there "so LCD off and on
    clear it".
- What: three budgets are doing this, and none of the three protects what
  it claims to.
  1. **`Hw` <= 864 B** (`lib.rs:265-272`, test at `:836`). Justified as
     "the `Emulator` may live on the stack" on the MCU. Nothing requires
     that: on the MCU it is a `static` in DTCM (128 KB). The only real
     layout concern is the one `Sys`'s own comment gives (`bus.rs:45-48`):
     hot fields at small offsets for 16-bit Thumb-2 loads. Total size is
     irrelevant to that.
  2. **`.text` <= 99,000 B.** Flash is 2 MB (H753ZI) / 1 MB (H735IG). What
     matters is the per-frame hot set in ITCM, which has its own check
     (`scripts/check-itcm-set.sh`, ADR 0026), and the product board can
     configure 128-256 KB of ITCM (architecture.md Targets table). The
     total cap mostly charges for cold code (`#[cold]` mid-line and STAT
     write paths), which costs nothing at run time. It is the budget that
     rejected the cleaner PPU design (E1) and it needs the human for every
     500 B.
  3. **Per-function budgets at measured + 5%.** A proxy for (2)'s proxy.
     For ITCM members the set total already covers it; for cold functions
     it is noise that forces splits.
  4. **The 4.02 M Ir/frame line.** 480 MHz / 59.73 / 2 = 4.02 M *cycles*.
     Equating that with x86-64 Ir assumes Thumb-2 instruction count equals
     x86-64 instruction count and M7 IPC = 1. ADR 0010 itself says "a
     proxy ... not a measurement". Specific reasons the proxy is biased
     for this code base: clocks and deadlines are `u64` everywhere
     (`Schedule::at: [u64; 8]`, `Ppu::frame_start`, `LineTiming::start`,
     every `now: u64` argument), which is one instruction on x86-64 and
     2-4 on a 32-bit core, so the event path (`boundary`, `take_first_due`,
     `next_event`, `sync`) is under-counted; x86 memory operands fold loads
     that Thumb-2 does separately; and ROM reads from QSPI/SDRAM through a
     16 KiB cache are not modelled at all. "Worst case 99.6% of the line"
     is a number with perhaps +-30-50% error being managed to 0.4%.
- Scenario or measurement: STATUS "Budgets": `.text` 98,124/99,000,
  `next_event` 586/590, worst case 99.6%, `Hw` at 864. Every recent
  correctness change needed a human raise or a contortion from the list
  above.
- Proposed change (human decisions; the gate scripts loosen, so escalate):
  - Keep: the ITCM-set total (it is the real constraint on the H753), and
    the relative +3% Ir guard per workload (a sound regression detector).
  - Replace the `Hw` size assert with `offset_of!` asserts on the handful
    of hot `Sys` fields. Then move state to its owner: `Held`/`sel_*`/
    `win_zero` into `Renderer`; `switch_end` to a named field; the delayed
    LYC request out of IF (E4). Each is S, fingerprint-identical.
  - Demote total `.text` and per-function numbers for non-ITCM symbols to
    a printed trend, not a gate failure.
  - Stop treating 4.02 M Ir as a wall until one measurement exists:
    `tools/bench` `synth:worst-case` and `cgb-acid2` on the NUCLEO with
    the DWT cycle counter gives the Ir-to-cycles factor per workload. This
    is a one-day spike and it re-prices every trade-off in this report.
    It should precede any further Stage 1 optimisation or size work.
  - Test: `diff-fingerprint.sh` identical for every move.

### E3 The line timeline is encoded three to four times; edges.rs unified the constants but not the logic   [severity: medium] [confidence: high]

- Where: point sequence: `Ppu::step` (`ppu/mod.rs:1212-1275`),
  `step_vblank` (`:1284-1307`), `next_point` (`:1088-1143`),
  `next_work_point` (`:1144-1174`), with `debug_assert_eq!(self.cursor,
  self.seek_point(double))` (`:1481`) to keep two of them in step.
  Mode source by position: `mode` (`:813-843`, reads), `mode_source`
  (`:1016-1083`, write views), `point_irq` + `point_irq_rare`
  (`:1316-1366`, the walk). Comparison value: `cmp` (`:783-797`) versus
  the `Some(ly)` / `None` arguments in the walk. "Is this clock in mode 3
  of the latched line": `mode3_end` (`:804`), `cpu_blocked` (`:949`),
  the `palette_blocked` fast path (`:980-988`), `hold_fetch` (`:1612-1616`),
  `obj_size_fetch` (`:2683-2690`), `mode3_pixel`, `drawing`,
  `mode0_start`, `hdma_period`.
- What: a new edge (the next lcd-offset step, for example) must be edited
  consistently in up to four places, and the walk is evaluated twice per
  point on lines with a STAT source: once speculatively in `next_event`
  (`:1392-1420`, to find the first point that fires) and again in `sync`.
  ADR 0014 lists "commit the walk's simulated state at the event instead
  of re-walking" as a further cut; it was never done (it needs ~10 B in
  `Hw`).
- Scenario or measurement: ~7.0 KB for this group. cgb-acid2 is +1.33%
  against its baseline with a 3% guard, and STATUS reports that the
  lcd-offset step 1 was held for +3.24% Ir and +718 B: the structure makes
  each new edge expensive in both currencies.
- Reference: `docs/design/lcd-offset-phase.md` 3.1-3.2 already describes
  the right shape (one row per physical edge, observers as columns); only
  step 0 (constants) landed.
- Proposed change: `ppu/timeline.rs`: a const table per line class
  (visible, first-after-enable, 0, 144, 145-152, 153) and speed; each row
  is `(half-dot, action, ly, cmp, mode_read, mode_src[view])`. One walker
  (`next row`), one `state_at(t)` that the reads and write views use.
  `step`/`step_vblank`/`next_point`/`next_work_point`/`cmp`/`mode`/
  `mode_source`/`point_irq_rare` collapse into table rows. Estimate:
  -1.0 to -1.5 KB, +0 to +0.2% Ir on cgb-acid2 (a table load replacing
  folded immediates; measure). Effort L, risk medium; must be
  `diff-fingerprint` identical on all 4,020 runs, which is exactly what
  that tool is for. Then cache `next_event`'s walked state (dirty flag set
  by any PPU write) so `sync` at the deadline adopts it: roughly halves
  walk Ir on STAT-source lines (estimate).

### E4 Sub-M-cycle timing is a set of per-site patches; two calibrations that disagree by one dot are reconciled at the observers   [severity: medium] [confidence: high]

- Where: write landing offsets: `bus.rs:1021-1027` (`now - 1` SCX/SCY in
  double speed, `now - 1` palettes, `now + 1` WX), `:931` (LCDC
  `now - double`), `edges.rs:57-66` (`LYC_WRITE`, `STAT_PHASE2`
  observers), `bus.rs:1191` (IF write `+ 1`), `:2040` (dispatch
  acknowledge at `mcycle / 2`), `:843` (LYC interrupt `now + 5`), the
  parity logic at `:856-866`. Observer-side fixes for the walk's own
  origin: `Ppu::early_point` (`ppu/mod.rs:889-909`), the `late` bit in
  `next_event` (`:1413-1415`), the `pulse && high` undo in the STAT write
  (`bus.rs:981-991`), and the "Open" item in `docs/reference/stat-irq.md`
  (single-speed line-153/0 walk points a dot early versus gambatte).
- What: one hardware fact, "this signal changes at half-dot H and that
  observer samples at half-dot S", is expressed five different ways. The
  clearest case: `lyc_irq_delay`/`lyc_irq_due` store a request in unused
  IF bits plus an M-cycle parity bit, and move the PPU deadline to
  `now + 5`, in order to say "this interrupt becomes visible 5 dots after
  the access". That is a timestamp compare. Likewise the line-0 pulse:
  the walk keeps it at t = 4 because the write views were calibrated
  there, gambatte's captures need the CPU to see it at t = 5, so the
  deadline is posted one dot late (`late`), IF writes treat it as "still
  ahead" (`early_point`), and a STAT write un-requests it afterwards.
- Scenario or measurement: `ppu/mod.rs:1388-1391` records that the same
  correction for line 153 was not made for lack of bytes. So today line 0
  and line 153 follow different rules for the same physical situation;
  an IF read at the M-cycle boundary on 153:12 at CPU phase `t = 0 mod 4`
  sees the LYC=0 request one M-cycle earlier than the line-0 logic implies
  it should. No ROM in the manifest pins it (their own note); unverified
  on hardware.
- Reference: SameBoy `sm83_cpu.c` `cgb_conflict_map` /
  `cgb_double_conflict_map` (a table); gambatte `memevent_oneshot_statirq`
  (a timestamped event).
- Proposed change:
  1. A timestamped interrupt request: `EventKind::Irq` with `(at, bits)`
     in `Sys` (9-12 B, needs E2's `Hw` reform). PPU requests carry their
     exact dot; the event promotes them into IF at the first boundary at
     or after it. Deletes `LYC_IRQ_PENDING/PHASE`, `lyc_irq_delay`,
     `lyc_irq_due`, the `late` special case, and lets line 153 get the
     same rule for free. The interrupt check in `Cpu::run` is unchanged.
     Effort M, estimate -150 to -250 B, Ir neutral.
  2. One const table `LAND[reg][speed]` in `edges.rs` (half-dots, signed)
     for FF0F and FF40-FF4B; `write_ppu` reads it instead of matching.
     Effort S-M, fingerprint-identical.
  3. Then re-derive the line-0/153 walk points against the write views
     once (the table from E3 makes that a row edit) and delete
     `early_point`.
  Test: the existing `bus.rs` tests at `:2951-3147` and the gambatte
  `lcd_offset`, `m2enable`, `lycEnable` groups.

### E5 The CPU models opcode-fetch overlap with three extra modes and a port of gambatte's `prefetched_` flag   [severity: medium] [confidence: medium]

- Where: `cpu/mod.rs:244-275` (`Mode::HaltBug`, `Latched`, `Prefetched`),
  `:416-431` (`latch_fetch`), `:499-517`, `:555-563` (`hdma_refetch`,
  `fetch_peek`, `prefetched`), `:567-577`, `:620-624`; `Bus` hooks
  `fetch(addr, latched)`, `fetch_peek`, `hdma_refetch`, `wake_stalls`
  (`cpu/mod.rs:108-205`); `hdma.rs:80-93` (`refetch` = `REQUESTED` / `RAN`,
  "gambatte: `cc -= 4` after a DMA in the `hdma_requested` halt state").
- What: the SM83 fetches the next opcode in the last M-cycle of the
  current instruction (GBCTR, fetch/execute overlap). This core fetches at
  the start of a step. Every place where that difference is observable
  got its own mechanism: the HALT bug (a mode), VRAM DMA starting after
  the fetch (a mode plus a latched opcode plus "undo prefetch" on
  dispatch), HALT/STOP with a block requested in the same M-cycle (a third
  mode, named after gambatte's variable, with the stall "one M-cycle
  shorter, but triggers until its full length are still lost").
  The comments cite gambatte function and variable names as the
  justification, which is the signature of a ported implementation
  artifact rather than a stated hardware rule.
- Scenario: this is the copied behaviour with the most real-game
  exposure. HDMA active while the game HALTs for VBlank is the normal
  case in HDMA-using games; a HALT whose M-cycle coincides with the HBlank
  trigger is roughly a 4-in-456 event per HALT. On that path the byte
  after HALT executes twice (`cpu/mod.rs:555-563`). The CGB04C captures
  `dma/hdma_late_m3halt_m2unhalt_inc_*_2` back it, so I am not claiming
  it is wrong; I am saying a wrong corner here corrupts game state rather
  than a pixel, and that it is reachable in ordinary play. It deserves a
  targeted soak (E6 net 5) before any refactor of this area.
- Reference: Gekkio GBCTR "fetch/execute overlap"; Pan Docs "halt bug".
- Proposed change: an IR-based loop: each instruction ends with the fetch
  of the next opcode into `ir`; the interrupt check happens at that
  fetch; HALT bug = fetch without PC increment; a VRAM DMA stall is simply
  inserted after the fetch M-cycle with the opcode already latched.
  `HaltBug`, `Latched`, `Prefetched`, `latch_fetch`, `fetch_peek`,
  `hdma_refetch`, `Cpu::latched/prefetched` and `VramDma::refetch` go.
  The sm83 single-step harness needs a one-line PC adjustment.
  Effort M-L, risk medium (it touches `Cpu::run`, the ITCM-critical
  symbol: 24,584 of 26,800 B). Defer to Stage 2 unless the soak finds a
  problem; verify with SST, `halt_bug`, `ei_delay_halt`, the gambatte
  `dma/hdma_*halt*` and `*_pc_7ffe*` groups.

### E6 The test net does not cover audio output, long runs, sync-placement independence of the PPU, or real-game rasters   [severity: medium] [confidence: high]

- Where: `scripts/gate-stage1.sh` (steps: tests, hard rules, code size,
  ITCM set, ROM loads, bench, fuzz, romtest); `docs/perf.md` "Behavior
  fingerprint"; `tools/fuzz/src/lib.rs:1-8` ("The property: no panic and
  no hang").
- What is covered well: register-visible timing (3,563 ROMs), CPU
  semantics (SST), APU register state, call-split independence of the APU
  (`--audio-buffer`, unit tests), "no behaviour change" on demand
  (`diff-fingerprint.sh`).
- What can regress unnoticed:
  1. **Audio samples.** No ROM check looks at samples. `mix.rs` (panning,
     NR50 scaling, the box filter) is guarded by a handful of unit tests;
     `--hash-samples` exists but is an opt-in diff, not in the gate.
  2. **Long runs.** The longest run anywhere is 20 emulated seconds.
     2^32 base clocks is 1,024 s (17 min). The code has many `as u32`
     clock truncations with wrapping compares (`Ppu::at`,
     `VramDma::switch_end`, `speed_switch_over`, the APU's u32 advance
     paths with `rem_long`). I found no bug in them by reading, but
     nothing executes them past the wrap except the APU's
     `check_long_advance` unit test.
  3. **Sync-placement independence of the PPU.** ADR 0002: "the catch-up
     points must be complete". The APU has this as a tested invariant;
     the PPU does not. E8 is a small real instance.
  4. **Real-game raster effects.** Test ROMs write registers at chosen
     dots; games write them from interrupt handlers with jitter. No
     real-game frame is checked anywhere.
  5. **HALT + HDMA + interrupts in game-like mixes** (E5).
  6. Input timing: `#input` fingerprint runs cover the path, not
     correctness; low concern.
- Proposed change (all cheap, all before any large refactor):
  1. **Golden fingerprint in the gate**: commit the output of `bench
     --fingerprint --audio-buffer 64 --hash-samples` (about 11 s) as
     `perf/fingerprint-golden.txt`; a change must be committed with the
     list of ROMs that moved, like the bench baseline. Makes audio and
     every frame hash a gated net. S.
  2. **Eager-sync mode**: a test-only switch that makes `Ctx::tick` call
     `sync_ppu_to(clock)` and `apu_sync(clock)` on every M-cycle. Run the
     fingerprint corpus both ways; the outputs must be identical. This is
     lazy-versus-stepped equivalence without writing a second model. S-M.
  3. **Clock-translation invariance**: a test-only constructor that
     starts the clock at 2^32 - k and at 2^40; fingerprints must equal
     the clock-0 run (modulo the hashed clock). Covers every wrap. S.
  4. **Split and step properties in the fuzz harness**: for each corpus
     input, `run_until` in one call, in random chunks, and by `step`,
     must reach the same state, frames and samples. S.
  5. **Local real-game goldens**: ADR 0030 recordings replayed headless
     over the human's ROMs, per-frame hashes committed, ROMs not; the
     check skips when the ROM is absent. Pick scenes with HDMA + HALT,
     a status-bar split, a wavy SCX effect, a speed switch. M, needs the
     human's recordings. The queued SameBoy differential harness
     (STATUS) is the stronger form of this.

### E7 Behaviours taken from a reference without a pinning ROM, or fitted to one ROM, that real games can reach   [severity: medium] [confidence: medium; each item unverified against hardware]

- Where and what:
  - `ppu/mod.rs:861-872` `stat_during_dma`: STAT mode 2 reads as 0 while
    an OAM DMA runs. The comment: SameBoy does it as a side effect of
    `GB_STAT_update`; "Gambatte has no such rule; no test ROM we have
    checks it." This is SameBoy's implementation structure copied as
    behaviour. Reachable by any game that polls STAT from its HRAM DMA
    wait loop outside VBlank. What would confirm it: a hardware capture
    or a targeted test ROM; absent that, the two references disagree and
    the choice is a coin flip.
  - `ppu/mod.rs:409-429` `WY_LEAD` = 5 / `WY_LEAD_DS` = 1: stands for
    SameBoy's check on an 8-dot grid aligned to the LCD-on clock ("1-8
    dots after the write"). Exact only at the phase the test ROMs use;
    at other phases it is off by up to 4 dots. Affects a window that
    starts on the line where WY is written in mode 3.
  - `ppu/mod.rs:2325-2356` `window_zero`: from one ROM
    (`m3_wx_4_change_sprites`), against SameBoy (DMG only), with "counted
    from 0-5 pixels after `drawn` gives the same manifest".
  - `ppu/mod.rs:2358-2366` one window re-trigger per line, none with 10
    objects; `memory.rs:147-150` three held parts.
  - `apu/mod.rs:285-305` double-speed trigger delay from gambatte's
    CGB-C rule, single-speed from the 1 MHz grid, and STATUS: "bbad441's
    1 MHz phase shift under the ds APU delay is inferred from one ROM
    pair".
  - `hdma.rs:31-45` `HBLANK_DELAY` 2 (gambatte) where SameBoy has 3, and
    `HBLANK_UNTIL` 4: fine as calibrations, but they are positions in
    gambatte's xpos origin mapped by hand.
- Revision consistency: the target is CGB C (ADR 0003) and the places
  that say so are careful (line-0 mode 0 at dot 3, the speed-switch DIV
  hold, FEA0-FEFF, SCY per fetch step). The risk is the inverse: base
  structure ported from SameBoy where SameBoy's default path is CGB E and
  the C branch was not noticed. I did not audit this (dimension A);
  the candidates are the STAT update positions and the APU paths without
  a gambatte CGB04C capture behind them.
- Proposed change: mark each of these in code with one grep-able tag
  (`UNPINNED:`) and list them in `docs/reference/`; the real-game goldens
  (E6.5) and the SameBoy differential harness are how they get exercised.
  For `stat_during_dma`, write the 20-line test ROM and ask the human to
  run it on hardware, or drop the rule (30 B). S.

### E8 A speed switch re-seeks the PPU walk without syncing first, so pending points run with the new speed's rules   [severity: low] [confidence: medium]

- Where: `bus.rs:1573-1574` (`self.hot.mcycle = m; self.ppu_speed_changed()`),
  `:912-915`; `ppu/mod.rs:1186-1198` (`speed_changed` -> `seek` from
  `synced`), and the comment there: "positions already passed keep the
  lazy sync's rule: a point runs with the speed of the sync that reaches
  it". `Ppu::sync` takes `double` from the caller at sync time (`:1424-1430`).
- What: update points between `synced` and the switch clock that did not
  fire (so were not deadlines) are evaluated later with `double` of the
  new speed, and line 0 / line 153 points are re-sought at the new speed's
  positions. The PPU's result then depends on where the last sync
  happened, which the lazy model is supposed to rule out.
- Scenario: LCD on, a STAT source selected, STOP executes on line 153
  between t = 8 and t = 12 in single speed with no sync since t < 8.
  `L153Off` then runs as double speed (`Some(153)`, the LYC part holds)
  instead of single (`None`, it drops). With LYC = 153 the STAT line state
  differs until `L153Zero`; a STAT or LYC write in that window sees a
  different blocking state. Narrow, and games switch speed once at boot,
  usually with interrupts off. Not confirmed by running it.
- Proposed change: `self.run_ppu_to_call(now)` before `hot.mcycle`
  changes. S. The eager-sync net (E6.2) would have found this and will
  confirm the fix; a unit test in the `ppu` rig: sync at 153:4, switch at
  153:10, compare the line state with a rig synced at 153:10 first.

### E9 The stall list is walked by five hand-written loops, and the "which object is stall i" arithmetic is duplicated   [severity: low] [confidence: high]

- Where: `LineTiming::pixels_before` (`ppu/mod.rs:519-535`), `tile_lead`
  (`:1845-1866`), `obj_size_fetch` (`:2692-2700`, "As
  `LineTiming::pixels_before`"), `window_stall_at`, `edge_dots`
  (`:3164`); `i - usize::from(i > win) - usize::from(i > prev)` at
  `:1857` and `:2700`.
- What: each loop re-derives `(clock, x)` per stall with slightly
  different saturation and ordering; a change to the stall model
  (the re-trigger added `prev_stall`) has to be applied to all of them.
- Proposed change: one `StallWalk` iterator yielding `(index, kind, x,
  start_clock, dots)` with `kind` = window / earlier window / object
  slot. S, fingerprint-identical, estimate -200 to -300 B. Becomes moot
  for the write paths if E1's stepper lands; still wanted for
  `pixels_before`.

### E10 `obj_size_fetch` swaps a VRAM byte to steer a fetch routine   [severity: low] [confidence: high]

- Where: `ppu/mod.rs:2722-2735`.
- What: to give one object's high plane a different height than its low
  plane, the code overwrites `mem.vram[old_hi]` with `mem.vram[new_hi]`,
  calls `fetch_next_object`, and restores it. Safe today (no panic path
  between, VRAM is CPU-blocked in mode 3), already flagged in STATUS.
  It exists because adding a parameter to the fetch grew a budgeted
  function. If `old_hi == new_hi` it is a no-op, fine; the hazard is any
  future early return or debug assertion inside the fetch.
- Proposed change: pass the high-plane address (or an `Option<u8>` height
  override) into `fetch_object`. S, after E2 removes the reason.

### E11 The orientation docs describe an older core   [severity: low] [confidence: high]

- Where: `docs/architecture.md` "PPU timing": "the post-boot state line
  144 dot 163" (code and ADR 0014 amendment 1: 171); "Scanline renderer":
  "CGB-mode rendering ... is M5"; the status block stops at M6; nothing
  on the mid-line machinery, `Held`, views, or `edges.rs`.
  `ppu/render.rs:41-60` "Open hardware choices": "`m3_lcdc_*_change`,
  `m3_scy_change*` fail; M3 doesn't need it" and "`m3_scx_low_3_bits`
  still differs ... unresolved" (both pass now). `schedule.rs:25-28`
  ("declared now ... `DivApu` and `AudioBuffer` in M6"), `apu/mod.rs:14-18`
  ("Task 1(a) is the framework"), `memory.rs:52-54`.
- What: AGENTS.md makes these files the session memory. A fresh session
  that reads architecture.md before a structural change is told a
  simpler system than the one it will edit.
- Proposed change: rewrite the PPU half of architecture.md from the code
  as part of M10 close. S.

---

## Bigger-picture recommendations

Ranked by value over cost.

1. **Measure before optimising or contorting further (E2).** One M7
   cycle measurement of two bench workloads converts the Ir line from a
   belief into a number. Every other trade-off here depends on it.
2. **Add the four cheap nets (E6.1-4) now.** About a session of work,
   zero bytes, zero Ir; they make every later step verifiable and they
   test the lazy architecture's central invariant for the PPU for the
   first time.
3. **Reform the budgets (E2), then put state back where it belongs.**
   Keep ITCM-set + relative Ir guard. Drop the `Hw` size cap for offset
   asserts; demote `.text` and cold per-function caps to a report.
4. **Stop patching the PPU; build the stepped mode 3 as an oracle (E1).**
   The patch route has reached the point where the last three ROMs cost a
   human budget raise and a latch encoded as `(byte, key)`. The next
   real-game raster bug should be fixed in a model, not with a tenth lead
   constant.
5. **One timeline table and one access-offset table (E3, E4).** These are
   pure refactors under `diff-fingerprint`, they shrink code, and they
   make the remaining lcd-offset steps row edits.

### Target architecture

```
Emulator
  Cpu            IR-prefetch loop; modes: Running, Halted, SpeedSwitch,
                 Stopped, Locked, Off. No fetch hooks in Bus.          (E5)
  Sys
    Hot          i32 countdown (as today: this part is right)
    Schedule     fixed slot per kind (as today), typed kinds:
                 Timer, Serial, Ppu, Irq{at,bits}, Dma, VramDma, Input,
                 DivApu, AudioBuffer.                                   (E4)
                 Stage 2 option: u32 wrapping stamps instead of u64.
    access       const LAND[reg][speed]: half-dot landing offset of
                 every timed write (FF0F, FF40-FF4B) and of the
                 dispatch acknowledge. The only place T-cycle offsets
                 live.                                                  (E4)
    Timer, Serial, Apu   closed-form / lazy, unchanged.
    Ppu
      timeline   const rows per line class x speed:
                 (half-dot, action, ly, cmp, mode_read, mode_src[view]).
                 One walker, one state_at(t). Reads, the walk and the
                 write views all read these rows.                       (E3)
      LineTiming Pan Docs penalty algorithm, closed form, for clean
                 lines; one StallWalk iterator.                         (E9)
      render     fast span renderer (fill_plain / fill_mixed), as today.
      mode3      dot-stepped fetcher + FIFO + object fetch, entered at
                 the first fetch-side write of a line (or for a line OAM
                 DMA touches), runs to the line end and yields its
                 mode-3 end. Owns all fetch-side state.                 (E1)
      OamDma     as today; the stepper reads `ppu_at` per fetch, so
                 `fix_attrs` / `obj_attr` go.
    VramDma      a bus-level stall after the fetch M-cycle; no CPU modes.
  Memory         RAM arrays only (plus Apu). No PPU line state.         (E2)

test-only
  timer/reference.rs     (exists)
  ppu/reference.rs       stepped STAT/LY machine + the mode3 stepper run
                         for every line: the oracle.                    (E1)
  eager-sync switch, clock-offset constructor, golden fingerprint.      (E6)
```

Rules that replace the current budget set: (1) nothing per M-cycle but
the countdown; (2) every component's visible state is a function of the
clock and the writes, tested by eager-sync equality; (3) each hardware
edge and each access offset is stated once; (4) hot set fits ITCM,
measured on the linked ELF; (5) Ir may not regress more than 3% per
workload without a recorded reason.

What this deletes, by estimate: ~7.3 KB + ~1.5 KB of PPU patch and DMA
attribute code (replaced by a 5-8 KB stepper), ~1-1.5 KB of duplicated
walk code, ~0.3-0.5 KB of interrupt-timing patches, three CPU modes and
four `Bus` hooks; about 2,000 non-test lines net. Clean-line speed is
unchanged; dirty-line cost becomes bounded per line.

### Roadmap

Each step lands alone behind the gate. "FP-identical" means
`diff-fingerprint.sh` shows no differing run.

**Do now, Stage 1**

| # | Step | Correctness | Ir | Bytes | Lines | Effort |
|---|---|---|---|---|---|---|
| 1 | Golden fingerprint with samples in the gate | net | 0 | 0 | +30 script | S |
| 2 | Eager-sync switch; corpus equality | net; expect to find E8-class bugs | 0 | 0 (test cfg) | +60 | S-M |
| 3 | Clock-offset constructor; equality at 2^32-k, 2^40 | net | 0 | 0 | +40 | S |
| 4 | Split/step properties in `tools/fuzz` | net | 0 | 0 | +80 | S |
| 5 | Fix speed-switch sync (E8) | fixes a sync dependence | ~0 | +8 | +3 | S |
| 6 | Human: budget reform (E2); then move `Held`/`sel_*`/`win_zero` to `Renderer`, name `switch_end`, drop the VRAM swap (E10) | FP-identical | ~0 | +-100 | -30 | S each |
| 7 | `EventKind::Irq`; delete IF-bit reuse and `late`; give line 153 the line-0 rule (E4.1) | line 153 consistent; rest FP-identical | ~0 | -150..-250 | -60 | M |
| 8 | `LAND` access table (E4.2) | FP-identical | ~0 | -100..-200 | -40 | S-M |
| 9 | `StallWalk` (E9) | FP-identical | ~0 | -200..-300 | -50 | S |
| 10 | Timeline tables (E3), then cached `next_event` | FP-identical | +0..0.2%, then -0.5..-1% on STAT-heavy | -1,000..-1,500 | -300 | L |
| 11 | Test-only `ppu/reference.rs` mode-3 stepper; romtest switch; differential fuzz against the patch path (E1) | oracle; will surface patch-path approximations | 0 | 0 | +1,200 test | L |
| 12 | Tag unpinned behaviours; `stat_during_dma` test ROM for hardware (E7); architecture.md rewrite (E11) | documentation of risk | 0 | 0 | docs | S |
| 13 | Human: local real-game goldens / SameBoy differential harness (E6.5) | net for E5, E7 | 0 | 0 | tools | M |

**Defer to Stage 2 (needs M7 cycle counts)**

| # | Step | Depends on |
|---|---|---|
| A | Ir-to-cycles calibration on the NUCLEO; re-draw the worst-case line in cycles. Ideally a one-day spike pulled ahead of step 6. | hardware, human |
| B | Promote the mode-3 stepper into the core for dirty lines; delete the patch machinery (E1). Expected: about size-neutral, worst-case +5-8% Ir (estimate), dense-write programs 2-3x cheaper. | A, step 11 |
| C | u32 wrapping stamps in `Schedule` and the PPU if the M7 profile shows 64-bit clock math in `boundary`/`next_event` (invisible in x86 Ir). | A, step 3 |
| D | IR-prefetch CPU loop (E5). Touches `Cpu::run`; wants ITCM numbers. | A, step 13 |
| E | Per-module opt levels and the 5.8 KB of alignment padding (already listed in STATUS). | A |

## Looked at and found fine

- `schedule.rs`: a fixed slot per kind with an incremental minimum and a
  cached slow minimum is the right structure for eight kinds; a heap or
  a generic typed-event queue would be slower and larger. Declaration
  order as the same-clock dispatch order is explicit and tested.
- `Hot` (`bus.rs:315-361`): the i32 countdown with `clock = deadline -
  budget`, and the `addr | dma` trick for OAM DMA conflicts, are sound and
  cheap; `idle_halted` skipping to the deadline is correct by
  construction (only a deadline can change `IE & IF`).
- Timer (`timer/mod.rs`, ADR 0011): closed-form counter with an eager
  reference model and a differential test. This is the pattern the PPU
  should copy.
- APU structure (`apu/mod.rs`): lazy `sync`, pure reads on a channel
  copy, call-split invariance tested. Not audited for hardware
  correctness (dimension A).
- `LineTiming` + `object_stalls` as a closed form of the Pan Docs penalty
  algorithm for undisturbed lines, checked against all 104 Mooneye sprite
  cases: a hardware-level statement, keep it.
- The clean-line renderer (`fill_plain`, `fill_mixed`, pair tables,
  ADR 0013/0019): no design objection.
- `edges.rs` half-dot constants and `Observer`: the right direction; the
  finding is that only the constants moved there (E3, E4).
- ADR 0003's single fixed revision: the right call; no per-revision
  switches found in the core.
- `diff-fingerprint.sh` and the mutation-checked unit tests: a refactor
  net better than most emulators have. The gaps are in E6.
- Not reviewed: `cart/`, `serial.rs`, `joypad.rs`, `cpu/exec.rs`,
  per-channel APU files, `tools/`.
