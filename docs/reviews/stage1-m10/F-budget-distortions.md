# F: Inventory of budget-driven distortions in gbc-core

Tree: `main` at 596f80c (2026-11-24). Read-only inventory; nothing was built
or measured for it. Every byte and Ir figure below is quoted from the commit
body, ADR, design doc or review that recorded it, at the time it was made
(mostly under `codegen-units = 16`; release is `codegen-units = 1` since
4db1d02, so sizes and inlining decisions have moved: re-measure, do not
trust the old deltas to the byte). Where no number was ever recorded it says
"not measured".

Line numbers are for HEAD. Paths are under
`/home/jess/gbc-32/crates/gbc-core/src/` unless they start with `docs/`.

## 0. What the budgets were, and what remains

| Budget | History | Status |
|---|---|---|
| Total `.text` | 85,100 (0dec57e) -> 96,000 (8c27f73) -> 97,500 (8048a84) -> 99,000 (c6c4ea7) | report-only since be036b7; to become the one size gate, against the H735 ITCM (128 KiB) |
| Per-function caps, measured + 5% | 5bbde0c on; 54 gating lines after be036b7 | to be dropped (dev-board report only) |
| ITCM hot set 57,344 B | 670bd67, 0a424bc | to be dropped (whole core goes in ITCM) |
| `size_of::<Hw>()` | <1,024 -> <=864 (b799d82) | dropped be036b7; no state has moved yet |
| `size_of::<Apu>() <= 128` | ADR 0021; test `apu/mod.rs:489` | **still asserted** (see S12) |
| +3% Ir per bench | ADR 0010 | stays |

Headroom arithmetic for the undo work: total `.text` is about 95,950 B
(STATUS). 128 KiB = 131,072 B. ADR 0026 assumed 8 KiB of ITCM for the
front end, which nobody has re-derived for the H735: if it holds, the core
has about 27 KB of room. The items below that grow code add up to roughly
+25-30 KB if all are taken at their recorded sizes (L1 +5.2 KB, A1 up to
+10-12 KB, L2 +4.1 KB, T1 +1.1 KB, L6 +1.8 KB, L3/L4 +1.1 KB, rest small).
That does not obviously fit. A1 (inlining `write_mapped`) is the one that
decides it, so measure it first and treat the rest as a ranked list (Ir
gained per KB).

Classification: **S** state in the wrong owner or packed; **L** logic
shaped by a cap, or a rule left out; **A** attribute placed for size;
**T** table/loop made smaller at a speed cost. "BP" = behaviour-preserving
(fingerprint must be identical). Effort S/M/L.

Counts: S 14, L 21, A 14, T 6 (55), plus 4 ADR 0027 items listed apart and
22 "keep" entries.

---

## 1. S: state in the wrong owner, or bit-packed for struct bytes

### S1 Delayed LYC-write interrupt parked in IF bits 5-6
- Where: `bus.rs:35-39` (`LYC_IRQ_PENDING`, `LYC_IRQ_PHASE`), `:946-953`
  (`sync_ppu` test), `:970-981` (`lyc_irq_delay`), `:988-998`
  (`lyc_irq_due`), `:1138-1150` (`write_ppu` 0x45 arm, early `return`
  that skips `schedule_ppu`), `:1328-1346` (`write_if` preserves
  `!INT_MASK`); every `& INT_MASK` on `iflags` (`cpu/mod.rs` dispatch
  sites, `bus.rs:1367`, `:2253`).
- Why: ADR 0029:121 ("because `Hw` is at its size limit"); STATUS session
  13 ("`Hw` is exactly at its 864 B limit"); commit e5739b8/bd674ce.
  bd674ce then trimmed the mechanism for `.text` (1,030 -> 530 B):
  `lyc_irq_delay` only lowers the PPU deadline and `write_if` no longer
  reschedules.
- Natural form: a field in `Ppu` (`lyc_irq_at: u64`, or a small enum),
  folded into `Ppu::next_event`'s minimum and delivered in `Ppu::sync`
  (A2-04's proposal); or a `Sys` field appended after the hot fields
  (D1). The parity bit, the `min(deadline, now + 5)` special case and the
  early return all go. Triage already plans a typed `Irq` event (E4).
- Speed: neutral (cold path; `sync_ppu`'s test changes from a bit of an
  already-loaded byte to a byte load, D1 estimates +2-4 B). Bytes: about
  flat.
- BP: the field move alone is BP. A2-04 lists four ways the parked
  request is lost or stranded (a `write_ppu` in the next M-cycle
  overwrites the lowered deadline; LCD off while parked;
  `set_interrupt_flags` masks it); fixing those is a behaviour change and
  needs tests seen failing. Do the move first, fingerprint-identical.
- Risk: low for the move. Effort: S-M.

### S2 `VramDma::switch_end` has two meanings, one of them not VRAM DMA state
- Where: `hdma.rs:97-107` (field; its own comment: "The speed-switch use
  belongs in `Sys`"), `hdma.rs:84-91` (`refetch`: "Fits this struct's
  padding"), uses `bus.rs:1843-1846` (`speed_switch_over`),
  `bus.rs:1996-2003` (`vdma_hblank`, `RAN` compare), `speed_switch`
  (`bus.rs:1744` on).
- Why: A1-05/D1 quote the pre-reform comment "Here because it fits this
  struct's padding (the `Hw` size budget)". 0d64707 widened it to u64 and
  noted "The field fits the padding left by `VramDma::single`: the struct
  stays 32 bytes".
- Natural form: `Sys::speed_switch_end: u64` (or a field of a small
  speed-switch state struct) and `VramDma::stall_end: u64` for the `RAN`
  case; `refetch` as an enum `{None, Requested, Ran}` (D6).
- Speed: neutral. Bytes: +8 B of state, code flat.
- BP: yes. Risk: low. Effort: S.

### S3 `Timer::apu_ds_delay`: APU frame-sequencer state in the timer
- Where: `timer/mod.rs:136-148`; toggled in `Timer::speed_switch`
  (`timer/mod.rs:510`); read by `Sys::set_div_apu_deadline`
  (`bus.rs:142`) and `Apu::trigger_delay` via the packed `ds` argument
  (see S9).
- Why: 0a83ae5 ("The flag lives in Timer's padding (the Hw size budget)").
- Natural form: a field of `Apu` (cleared by NR52 power on/off where the
  rest of the APU state is) or of `Sys` next to the DIV-APU deadline.
- Speed: neutral (cold). BP: yes. Risk: low (the "toggled even with the
  APU off" rule must be kept). Effort: S.

### S4 `Memory::dma_read_clears`: OAM DMA bus state in the RAM arrays, as atomics
- Where: `memory.rs:66-70`; `dma.rs:70-125` (`ReadClears([AtomicU32; 5])`,
  `clear_bits`); `bus.rs:2490` (record), `:1550` (`dma_apply_read_clears`).
- Why: ADR 0028 amendment ("It lives in `Memory`, not `Sys`, because `Hw`
  would pass its 864 B limit"). The atomics are a second, separate
  decision: the read path is `&`-only because a `&mut read_high`
  tail-calling a shared body measured +2.1% Ir on cpu_instrs (+1.4%
  cgb-raster), and duplicating the body for `peek` measured +1.0%
  cpu_instrs and about +800 B (8f33025, ADR 0028).
- Natural form: owned by `OamDma` (in `Ppu`) or `Sys`. Whether it can be
  a plain `[u32; 5]` depends on getting a `&mut` CPU read path without
  the Ir cost; with cgu=1 and no `read_high` cap that is worth one
  re-measurement (D3 says the same), but the recorded result is that both
  `&mut` variants were slower, so do not assume.
- Speed: move alone neutral. `&mut` path: was slower when measured.
  Bytes: flat for the move.
- BP: yes. Risk: low (move), medium (`&mut` read path touches
  `Cpu::run` codegen). Effort: S for the move with `Cell`/atomics kept, M
  for the `&mut` experiment.
- Also fixes D3/A2-09: `Memory::clear` (`memory.rs:112-129`) does not
  reset this field.

### S5 Renderer fetch state inside `Memory::obj_rank` (`ObjRank`)
- Where: `memory.rs:131-170`: `hold: [Held; 3]`, `next`, `sel_latch`,
  `sel_end`, `win_zero`; the struct comment `memory.rs:131-136` ("a second
  [pointer] would grow `Ppu::draw` past its code-size budget").
- Why: af81360 (hold after the hot arrays: `Ppu::draw` 158 B vs budget
  155), 418421c (`ObjRank::next`: "in Memory: Hw has no room"),
  `docs/design/mid-line-ppu.md:363-369` (`Hw` 872 > 864), `:1213-1217`
  (TILE_SEL latch in `Renderer` "pushed `Hw` to 872 B"), `:1349-1353`
  (`win_zero`: "in `Renderer` it pushed `Hw` past 864 B").
- Natural form: fields of `Renderer` (or one `LineState`/`FetchState`
  struct owned by the PPU, A2-03). `ObjRank` goes back to being the rank
  array only, or disappears with S8.
- Speed: neutral to slightly better (the renderer already has `self`; the
  state stops travelling through a `&mut ObjRank` argument, which with S8
  removes one stack-passed argument from the seven-argument render calls,
  D6). Bytes: not measured; the old objection was a 3 B overrun of a
  146 B function.
- BP: yes. Also closes D3/A2-09 (`clear()` skips `hold` and `win_zero`).
- Risk: low-medium (touches every `render*` signature). Effort: M.

### S6 `IrqLine` packed as `bits: u8` + `lyc: bool`
- Where: `ppu/mod.rs:232-244` (comment), `:241-301`.
- Why: 8e30952 ("three bools grew Hw past its 864 B limit"), ADR 0029:45.
  35bc38d then un-packed `lyc` again for speed (one byte for all three
  cost about 3 Ir per update point) and kept "the same size and layout as
  before the mode part".
- Natural form: three `bool`s (`stat`, `mode`, `lyc`), or keep `bits` and
  make the mode source an enum (D6). The current comment also records
  that the two-byte layout is 62 B smaller on thumbv7em than "one byte
  unaligned".
- Speed: unknown; 35bc38d's `eval` is branch-free over `bits` and was
  tuned to it. Three bools may be slower. Measure before changing.
- BP: yes. Risk: medium for Ir (`point_irq` runs at every update point).
  Effort: S. **Low priority; may belong in the keep list after one
  measurement.**

### S7 `Held::planes` and `ObjRank::sel_end` bit-packing
- Where: `memory.rs:172-189` (`planes`: bits 0-1 late planes, 2-3
  glitched planes, 4-7 latch slot + 1 / 15 / 0), `memory.rs:157-167`
  (`sel_end: (u8, u8)` with a five-meaning key), built at
  `ppu/mod.rs` `hold_fetch` (`:1595` on).
- Why: D6; the hold had to fit `Memory` without moving offsets (af81360)
  and the latch could not go in `Renderer` (S5).
- Natural form: `Held { late: [bool; 2], glitched: [bool; 2], latch:
  LatchSource }` with an enum; `sel_end` as `(u8, SelKey)` enum. D6 offers
  the zero-cost variant (`repr(transparent)` wrappers with accessors).
- Speed: neutral (cold paths only). Bytes: a few per `Held`.
- BP: yes. Risk: low. Effort: M. Do it with S5.

### S8 Per-line object buffers and pair tables in `Memory`
- Where: `memory.rs:28-33` (`bg_pairs`, 512 B), `:51-58` (`obj_buf`
  176 B, `obj_rank.rank` 176 B); `Renderer::slow` `OBJ_DIRTY`
  (`ppu/render.rs:284-291`), the deferred per-line clear.
- Why: b799d82 ("Hw: 1,008 -> 832 B. Size assert lowered ... to <=864 B";
  cost recorded: Ir +0.02..+0.47%, cgb-acid2 worst; `Ppu::draw` 138 ->
  146 B for the extra `&mut` argument); ADR 0019 (rank buffer "in
  `Renderer` it would push `Hw` over its 864 B size assert"); 41a47b2
  (pair tables in `Memory`, "Hw unchanged").
- Natural form: `Renderer` owns `obj_buf`, `rank` and the pair tables;
  `Ppu::latch`/`draw`/`flush`/`sync` stop taking `&mut Memory` for them.
- Speed: recovers up to the +0.47% b799d82 cost (cgb-acid2), not
  re-measured since. Bytes: `Hw` +864 B of state, `Memory` -864 B: no net
  RAM change.
- BP: yes. Risk: medium. `Hw` grows to about 1.7 KB, and ADR 0006's
  argument (an `Emulator` may be built on a few-KiB MCU stack) then
  matters again: the human dropped the cap, not the stack concern. The
  hot `Sys` field offsets are unaffected (the PPU is behind them), but
  `Renderer` field offsets change. Effort: M. **Decide with the human
  note in section 6 before doing it**; S5 does not depend on it.

### S9 `Apu::write`'s `ds: u8` argument packs M-cycle length and the delay flag
- Where: `apu/mod.rs:326` (`trigger_delay(&self, now, ds: u8)`), `:341`
  (`write(.., ds: u8, edge: u64)`); built in `Ctx::write_apu`
  (`bus.rs:765` on).
- Why: bbad441 ("`double: bool` becomes `ds: u8`, the M-cycle length plus
  the delay flag in bit 0, so write_apu stays the same size (+8 B .text)").
- Natural form: `double: bool` plus the delay read from where S3 puts it
  (in `Apu` it is not an argument at all).
- Speed: neutral. BP: yes. Risk: low. Effort: S. Do it with S3.

### S10 FF55 read-back mirrored into `io[0x55]`
- Where: `bus.rs:195` (reset), `:1921` (`self.mem.io[0x55] =
  self.sys.vdma.read_hdma5()`), every transfer step and FF55 write.
- Why: 86a13ca ("The FF55 arm in read_io (inlined into read_high) cost
  40 B and put read_high at 928 B against its 890 B budget").
- Natural form: `0x55 if cgb => sys.vdma.read_hdma5()` in `read_io`; no
  stored copy to keep in step.
- Speed: neutral (one store less per HDMA block, one arm more in a
  match). Bytes: +40 B recorded.
- BP: yes (check DMG compatibility mode, where the mirror is FF).
  Risk: low. Effort: S.

### S11 Line-timing state derived or squeezed to keep `Hw` at 864 B
- Where: `ppu/mod.rs:319` (`LineTiming::mode3: u8`), `:480-484`
  (`LineTiming::fine()` re-derives the fine scroll from `first_pixel` and
  `mode3`), `:358-361` (`wy_late`, 65f00bd/ea5e484: "kept in a LineTiming
  padding byte"), `ppu/render.rs:258-266` (`bg_fine` replaced
  `fine_scx`), `ppu/render.rs:278`, `:348-357` (window start computed
  from `win_wx`; `LineTiming::wx` dropped).
- Why: 8176991 ("Hw stays 864 B: the renderer's fine_scx gives way to
  bg_fine (the timing derives its fine scroll from first_pixel) and
  LineTiming::mode3 is a u8"); `docs/design/mid-line-ppu.md:570-571`
  (c8e8626, +0.02% Ir for the per-line `win_wx` store) and `:733-737`.
- Natural form: store `fine: u8` and `wx` in `LineTiming`; `mode3: u16`
  like its neighbours; `wy_late` named as the late-WY latch where the WY
  condition lives.
- Speed: neutral to marginally better (cold readers). BP: yes.
  Risk: low. Effort: S. Low value on its own; fold into S5.

### S12 `Apu` lives in `Memory` and is capped at 128 B
- Where: `memory.rs:71-73`; test `apu/mod.rs:489-490`
  (`apu_fits_in_128_bytes`); `apu/mix.rs:36-46` and `apu/mod.rs` `sync`
  (level lanes are a stack array cleared per batch).
- Why: ADR 0021:26-27, :61 ("`Hw` is capped at 864 B ... `Apu` is a field
  of `Memory`"); `docs/perf.md:170-172` ("removing the clear altogether
  needs the lanes in `Apu` (store semantics; the `Apu` size test caps it
  at 128 B)"); ADR 0021:459-460 (pending samples "do not fit
  `Memory::apu`'s 128 B cap").
- Natural form: `Sys::apu` (or keep it in `Memory`, which is defensible:
  it is zero-initialised state), with no size test. The real item is the
  cap: see T3.
- Speed: move neutral. BP: yes. Risk: low. Effort: S (drop the test),
  M (move). The `Memory` "all zero at power-on" invariant already covers
  `Apu`, so only the cap is clearly wrong.

### S13 Index masks standing in for a bounded-length type
- Where: `ppu/mod.rs:490`, `:494`, `:510`, `:1661`, `:2220`, `:2224`,
  `:2232`, `:2240`, `:2252`, `:2305`, `:2561` (`% 11`, `% 12`, `% 10` on
  `stalls`/`n_stalls`/`win_stall`); `ppu/render.rs:450`, `:947`, `:988`,
  `:1006`, `:1032`, `:1057`, `ppu/objects.rs:133` (`% MAX_PER_LINE`);
  `ppu/render.rs:243` (`ly % SCREEN_HEIGHT`).
- Why: A2-03 and D6. They remove bounds-check panic paths (ADR 0009's
  no-panic spirit and bytes) on indices that are not ranges: `NO_STALL`
  is 0xFF and `0xFF % 11 == 2`, so `hold_fetch` (`:1661`) reads stall 2
  before testing `win_stall != NO_STALL`, and `set_stalls_retriggered`
  (`:2220-2224`) would copy stall 2 if `prev_stall()` missed.
- Natural form: a `Stalls` type (`len`, `push`, `insert`, `get ->
  Option`), `Option<u8>`-style accessors for `win_stall`/`prev_stall`,
  and `debug_assert!`s. Also a speed point on the MCU: `% 11`/`% 12`/
  `% 10` on a non-power-of-two is a multiply-shift or `udiv` on M7, where
  a compare is one instruction.
- Speed: neutral on x86 Ir, slightly better on M7 for the per-line sites
  (`:490-494` in `finish`, `:2252`). Bytes: a few panic-free compares.
- BP: yes as long as the invariants hold (A2-03 traced them and found no
  reachable violation). Risk: low for the cold sites, measure the
  `render.rs` ones. Effort: M.

### S14 `LineTiming`/`ObjRank` capacity limits that are artifacts of the representation
- Where: `memory.rs:147-150` (three held parts), `ppu/mod.rs:327`
  (`stalls: [(u8, u8); 11]`: one re-trigger per line and none with 10
  objects, `Ppu::retrigger` `:2378`).
- Why: E1 lists them; the hold size was fixed by what fitted after the
  hot arrays (af81360) and `Hw` (8176991).
- Natural form: sized by the hardware maximum (10 objects + window
  starts), not by struct bytes. Becomes moot if the dot-stepped mode-3
  path (L21) lands.
- Speed: neutral. **Not BP** (lifting a limit changes output on lines
  that hit it; no manifest ROM does today). Risk: medium. Effort: M.
  Park behind L21.

---

## 2. L: logic shaped by a byte cap, and rules left out for bytes

### L1 The 32 CB-prefixed (HL) forms share one body
- Where: `cpu/exec.rs:59-70` (`execute_cb`, `cb_hl`).
- Why: 10777f8 (`.text` 97,320 -> 92,134, `execute_cb` 12,078 -> 6,892;
  "Cost: ~5 x86 Ir per (HL) form (cgb-acid2 +7.6k Ir per 2 s)"). It was
  done to pay for mid-line PPU fixes under the 97,500 cap.
- Natural form: each CB opcode its own arm with its bus read and write
  inlined, as the register forms are.
- Speed: faster by about 5 Ir per CB (HL) instruction. Bytes: +5,186.
- BP: yes (SM83 single-step suite, 500,000 cases). Risk: low.
  Effort: S (revert 10777f8's exec.rs part). Poor Ir-per-KB compared with
  L2/L3; take it only if room is left.

### L2 `fill_plain`/`fill_mixed`/partial tiles kept out of `render`
- Where: `ppu/render.rs:1139-1175` (`fill`, `fill_plain`, `fill_mixed`),
  `:1401-1420` (`partial_tile`, `fill_partial`).
- Why: 68c5d32 and `docs/perf.md:354-367`: "whole-tile loop, inlined":
  cgb-acid2 508,396,952 / worst-case 471,299,421 / cpu_instrs
  1,590,190,248 at 4,218 + 1,820 B, against the chosen out-of-line
  514,287,199 / 472,244,651 / 1,598,254,766 at 1,680 + 1,000 + 1,104 +
  548 B ("Inlining both copies instead was -0.9 M Ir more on worst-case
  for +4.1 KB; not done"). e22721c moved `partial_tile` out "so
  fill_plain/fill_mixed stay inside their size budgets".
- Natural form: the tile loops inlined into the BG and window call sites
  of `render`.
- Speed: recorded -1.15% cgb-acid2, -0.50% cpu_instrs, -0.20% worst-case
  (at 68c5d32; the renderer has changed since). Bytes: about +4.1 KB.
- BP: yes. Risk: low. Effort: S (attributes) to M (if `render`'s shape
  needs help). Note 0c23c48's table: "mixed tile inlined twice" was
  slower than out of line on that revision (681.10 M vs 680.54 M
  cgb-acid2), so measure `fill_mixed` and `fill_plain` separately.

### L3 `point_irq` / `point_irq_rare` split
- Where: `ppu/mod.rs:1318-1340`, `:1344-1372`.
- Why: 35bc38d ("Inlining all of them is ~1 M Ir cheaper still but 678 B
  on thumbv7em, over the 520 B budget"); the human rejected raising that
  budget (4ceae03).
- Natural form: one `point_irq` with every point's `eval` specialised.
- Speed: about -1 M Ir on cgb-acid2's 20 s (-0.15%). Bytes: +256 B plus
  the `point_irq_rare` symbol going.
- BP: yes. Risk: low. Effort: S.

### L4 `next_event` calls `point_irq` at `Latch`; `point_irq` not forced inline
- Where: `ppu/mod.rs:1407-1411` (comment: "skipping the call there saves
  ~0.25% Ir on cgb-acid2 but takes this past its thumbv7em code-size
  budget"); `docs/perf.md:334-335` ("`#[inline(always)]` on `point_irq`:
  -0.1 M for +800 B of `next_event`; not done").
- Why: `next_event` cap 590 B, at 586 (b309904, 4b091ef).
- Natural form: skip `Latch` (and `End`) before the call; let
  `point_irq` inline where it pays.
- Speed: -0.25% cgb-acid2 recorded for the skip. Bytes: tens of bytes
  for the skip; +800 B for the forced inline (take the skip, measure the
  inline).
- BP: yes. Risk: low. Effort: S.

### L5 Line 153's LYC=0 point left out of the "CPU sees it a dot late" rule
- Where: `ppu/mod.rs:1396-1399` (comment), `:1412-1418` (the `late` test
  is by position, single-speed line-0 pulse only); `Ppu::early_point`
  (`:890`) covers its IF writes instead.
- Why: 4b091ef ("Moving line 153's LYC=0 point's deadline too gave the
  same results but took next_event 4 B over budget").
- Natural form: both early points get the late deadline, by one rule;
  `early_point`'s special case for 153 may then shrink.
- Speed: neutral. Bytes: +20 B.
- **Not BP in principle** (an IF read or interrupt check on 153:12 at
  the one CPU phase that has a boundary there changes). The commit says
  the manifest result was the same with it. Needs the full manifest and
  a unit test seen failing. Risk: low-medium. Effort: S.

### L6 BG attribute decode runs in DMG compatibility mode
- Where: `ppu/render.rs` `Mode { attr_mask, master }` (`:1128-1137`),
  the tile fetch in `fill_tiles`.
- Why: plan `docs/plan/stage-1.md:146` ("BG attribute decode runs in DMG
  compatibility mode with `attr_mask` 0 (cpu_instrs +3.13% at e22721c); a
  const-generic CGB variant costs ~1.8 KB, revisit with the M6 code-size
  budget").
- Natural form: `fill_tiles::<MIX, CGB>`, the DMG instance with no
  attribute read, flip or bank select.
- Speed: up to about -3% on DMG-compatibility games (the +3.13% was
  cpu_instrs, a DMG ROM). No effect on CGB-mode ROMs. Bytes: +1.8 KB
  recorded.
- BP: yes. Risk: low. Effort: M.

### L7 Timer stays lazy only while IE bit 2 is clear
- Where: `bus.rs:484-500` (`schedule_timer`: `iflags & !ie & INT_TIMER`),
  `:884-892` (`run_events` reloads and re-arms when IE.2 is set),
  `:2262` on (inlined `push_acknowledge` has no timer hook).
- Why: 69247dd and ADR 0011:405-412 ("A hook there instead (deadline off
  whenever IF bit 2 is set) cost +720 B in `Cpu::run` (measured), which is
  why the condition includes IE").
- Natural form: no Timer deadline whenever IF bit 2 is set; the
  acknowledge that clears it re-arms.
- Speed: saves the scheduler events between an overflow and its
  acknowledge when IE.2 is set but the CPU cannot take it yet (IME off,
  a higher-priority handler running). Small for normal games; the Ir of
  the alternative was not recorded, only its bytes. The hook also adds a
  test to every interrupt dispatch. Bytes: +720 in `Cpu::run`.
- BP: yes (the differential timer test covers it). Risk: medium
  (`Cpu::run` codegen; c18cfdc showed a small change in the inlined
  acknowledge costing +1.3-1.9% Ir). Effort: M. Measure; may not pay.

### L8 `write_ppu` runs the WY pre-check on every LCDC write
- Where: `bus.rs:1081-1085`.
- Why: 65f00bd ("testing the bit in `write_ppu` measured 1564 B
  (thumbv7em, cap 1565) against 1520 B without").
- Natural form: `if s.ppu.lcdc & 0x20 != 0 { wy_write(..) }` (the
  pre-check does nothing when the old LCDC.5 is clear).
- Speed: saves one cold call per LCDC write with the window off (LCDC
  raster effects). Bytes: +44.
- BP: yes. Risk: low. Effort: S.

### L9 RP (FF56) write shares the FF72-FF75 store arm
- Where: `bus.rs:1420-1428`.
- Why: b5d17c2 (a separate `0x56 if cgb` arm put `write_io` at 718 B over
  its 712 B cap).
- Natural form: either is defensible: SameBoy stores RP on any CGB, and
  the read is CGB-mode-only, so the shared arm is unobservable. The only
  smell is the comment's reason. **Recommend keep, fix the comment.**
- BP: n/a. Effort: S (comment).

### L10 `write_if` does not reschedule; `lyc_irq_delay` only moves the deadline
- Where: `bus.rs:1328-1346`, `:970-981`; `if_write_sees`/`early_point`
  compare frame positions (`ppu/mod.rs:890`).
- Why: bd674ce ("trim the IF-write and LYC-delay paths for code size":
  1,030 -> 530 B).
- Natural form: every path that syncs the PPU reschedules it; the
  delayed request is an ordinary scheduled item (S1). The
  position-compare-without-division part is also a speed win and stays.
- Speed: neutral. BP: the reschedule is BP today (the passed deadline
  runs `sync_ppu` at the next boundary), but it is the mechanism behind
  A2-04's stranded-request cases. Do with S1. Effort: S.

### L11 `write_stat`/`write_lyc` share an out-of-line `update_at`
- Where: `ppu/mod.rs:2746-2747` (`update_at`), `:2824`, `:2924`.
- Why: 65dc4a9 ("742 + 714 -> 290 + 294 + 444 B; both kept out of line so
  write_ppu (ITCM) does not grow"); 046958f notes "update_at +298,
  chaotic inlining".
- Natural form: keeping one `update_at` is fine (it is real shared
  logic). The size-only part is the `#[inline(never)]` trio. STAT writes
  are per-line in raster games. **Keep the sharing; drop the forced
  out-of-line and let cgu=1 decide.** Speed: probably neutral.
  BP: yes. Effort: S (with the A-class sweep).

### L12 Walk points of lines 144-153 in a separate `step_vblank`
- Where: `ppu/mod.rs:1281-1307`; `step` (`:1214`), `next_point`, `seek`.
- Why: b309904 ("next_point walks VBlank lines with an out-of-line
  step_vblank (seek 562/605)"), ADR 0029:102-105.
- Natural form: one step function for the walk and the re-seek (today
  two encodings of the VBlank points exist, held equal by a debug assert
  in `sync`). Triage plans a single line-timeline table (E3), which
  supersedes this.
- Speed: neutral (re-seek is cold). BP: yes. Effort: M. Fold into E3.

### L13 `palette_blocked` re-implements `cpu_blocked` as a fast path; `cpu_blocked` is one shared out-of-line copy
- Where: `ppu/mod.rs:944-958` (`cpu_blocked`: "Out of line: one copy for
  the three (code size)"), `:972-996` (`palette_blocked`), `:935`,
  `:1005`.
- Why: 6110717 ("Code size: VRAM, OAM and palette windows share one
  out-of-line `Ppu::cpu_blocked` ... A synced fast path in
  `palette_blocked` ... keep synth:cgb-raster at +2.61%").
- Natural form: `cpu_blocked` `#[inline]` with constant `from`/
  `end_extra` per caller, so each of `vram_blocked`, `oam_blocked` and
  `palette_blocked` is a specialised check; the hand-written fast path
  may then be unnecessary, or apply to all three. `cpu_blocked` runs on
  every CPU VRAM/OAM access (0a424bc put it in ITCM for that reason).
- Speed: faster for VRAM/OAM-heavy code (a call and a table index less
  per access); not measured. Bytes: +2 x ~186 B.
- BP: yes. Risk: low. Effort: S-M.

### L14 `run_ppu_to` / `run_ppu_to_call` twin; `vdma_arm` marked cold to keep the twin small
- Where: `bus.rs:1009-1031`; `vdma_arm` `bus.rs:1964-1974` ("must stay
  small enough to inline into the PPU register and palette write paths").
- Why: b315d4a ("every other caller goes through the out-of-line
  run_ppu_to_call, to keep sync_ppu (230 B) and total .text (85,100 B)
  within budget"); the inline copy exists because out of line it cost
  synth:cgb-raster +2.59%.
- Natural form: one `run_ppu_to`, `#[inline]`, inlined into `sync_ppu`
  (every PPU event, about 3.6 per line) and the palette path; the
  compiler decides for the register writes. `vdma_arm` is not cold while
  an HBlank transfer is active (plan: about 30 Ir per call, 3.4 calls per
  line, 1.76 M Ir per 2 s of worst-case): it wants a cheap inline
  "already armed for this line" test in front of an out-of-line body,
  not `#[cold]`.
- Speed: a call less per PPU event; the `vdma_arm` guard is the bigger
  part during HDMA. Not measured. Bytes: + a few hundred.
- BP: yes. Risk: low-medium (cgb-raster is sensitive here). Effort: S-M.

### L15 Envelope trigger delay handled in `Apu::write_rest`, not in the channels' trigger paths
- Where: `apu/mod.rs:360-380` (doc: "here rather than in the channels'
  trigger paths: code size").
- Why: 1f1332a ("Apu::write grew 128 B that way, 12 B this way");
  a86cc85/3bb48a4 (CH4 and sweep writes routed through the out-of-line
  tail "to keep Apu::write in budget").
- Natural form: each channel's trigger applies its own envelope delay;
  `Apu::write` dispatches per channel without a catch-all tail.
- Speed: neutral (APU writes cost about 880 Ir each, dominated by the
  sync). BP: yes. Risk: low-medium (three channels, timing windows).
  Effort: M. Low value.

### L16 Sweep trigger cancels the pending overflow check by bumping the timer
- Where: `apu/units.rs:102` on (`Sweep::trigger`: `let cancel =
  self.check != 0; self.check = 0; self.timer += .. + u8::from(cancel)`),
  `Sweep::clock` `:124`.
- Why: fc2fcc3 (`Sweep::clock` was 22 B over its cap with 0513cde's
  explicit `CHECK_CANCELLED` flag).
- Natural form: 0513cde's version: the pending check carries a cancelled
  flag and `clock` skips the calculation.
- Speed: neutral. BP: fc2fcc3 says the timer ends at the same value; the
  two forms differ in intermediate state (`due`, `check`), so compare
  with the fingerprint including `--hash-samples`. Risk: low. Effort: S.

### L17 `render_held` uses the mixed tile path even with no objects
- Where: `ppu/render.rs:729-735` ("One instance (mixed with no object
  pixels when none are shown): calling the plain one too grows it past
  its size budget").
- Natural form: plain path when no objects are shown.
- Speed: faster on held parts without objects (mid-mode-3 SCX/LCDC
  writes: the "outside the bound" case in `docs/perf.md`). Not measured.
- BP: yes. Risk: low. Effort: S.

### L18 `obj_size_fetch` patches a VRAM byte to steer the fetch
- Where: `ppu/mod.rs:2724-2739`.
- Why: A2-06/E10 ("It exists because adding a parameter to the fetch
  grew a budgeted function").
- Natural form: pass the high-plane LCDC (or address) to `fetch_object`.
- Speed: neutral. BP: yes. Risk: low. Effort: S.

### L19 `Cart::dma_block` serves ROM and RAM through one by-value function
- Where: `cart/mod.rs:799-818`.
- Why: a3dff9b ("Separate ROM and RAM block functions or writing into
  VRAM through an out pointer measured 85,116-85,542 B (over budget), so
  ROM and RAM share one function").
- Natural form: copy the 16 bytes straight into the VRAM slot (no
  `[u8; 16]` return through the stack), with ROM and RAM as separate
  small paths.
- Speed: a 16-byte copy less per HDMA block (128 blocks/frame at most).
  Small. BP: yes. Risk: low. Effort: S-M. Interacts with ADR 0027 (R2).

### L20 `write_ppu`'s `_` arm re-matches `reg` four times; `hold_fetch` takes `reg: usize`
- Where: `bus.rs:1160-1192`; `ppu/mod.rs:1595` on.
- Why: D6; the arm was kept as one block under `write_ppu`'s cap
  (1,290 then 1,565 B).
- Natural form: `write_scroll`, `write_dmg_palette`, `write_wy`,
  `write_wx` called from the outer match; an enum instead of `reg`.
- Speed: neutral to slightly better (no second dispatch). BP: yes.
  Effort: M. Belongs to the `bus.rs` split (D2).

### L21 Mid-line PPU as closed-form patches instead of a dot-stepped mode-3 path
- Where: `ppu/mod.rs` `hold_fetch`, `tile_lead`, `retrigger`,
  `set_stalls_retriggered`, `window_stall_at`, `end_moved`,
  `obj_size_fetch`; `ppu/render.rs` `render_held`, `mix_planes`,
  `tile_sel_glitch`, `render_zero`.
- Why: `docs/design/mid-line-ppu.md:156-162` (a stepped pipeline,
  "~3-5 KB ... Rejected for the budgets"); E1 measures the result at
  >= 7.3 KB with nine calibration constants and unbounded per-write cost.
- Natural form: E1's proposal. Already planned as a test-only oracle
  (wave 6), promotion into the core deferred to Stage 2.
- **Not BP.** Effort: L. Listed for completeness; not part of this
  clean-up.

---

## 3. A: attributes placed for size

Rule of thumb for the sweep: with the whole core in ITCM and cgu=1, an
`#[inline(never)]` or `#[cold]` is justified only by (a) a recorded Ir
measurement, (b) a path that is rare by a named condition, or (c) keeping
`Cpu::run`'s register allocation intact. "One copy (code size)" is no
longer a reason. Each removal is BP by construction; the check is Ir.

### A1 `Ctx::write_mapped` `#[inline(never)]`
- Where: `bus.rs:1204-1216`.
- Why: 7bdaee2/75b12ad and `docs/perf.md:494-508`. With `write_mapped`
  `#[inline]` at 7bdaee2: cpu_instrs -2.70% / cgb_sound +2.91% / acid2
  +1.70% against +2.94% / +4.31% / +3.05% out of line, at `Cpu::run`
  40,862 B vs 32,560 B. The out-of-line form was kept for the `run` cap,
  and the bench was then recovered another way (ROM windows by bit 14).
- Natural form: the RAM arms (WRAM, HRAM via `write_high`) inline in the
  CPU's write, as reads are; cart and VRAM writes stay calls.
- Speed: potentially the largest single item (several % on write-heavy
  code), but the recorded numbers are from M2, before the PPU, APU and
  cgu=1. Bytes: recorded +8-10 KB in `Cpu::run`. A middle form (inline
  only the `6 =>` WRAM arm, call for the rest) should get most of it for
  much less.
- BP: yes. Risk: medium (`Cpu::run` register pressure, B9). Effort: M.
  **Measure first; it decides the headroom for everything else.**
  Overlaps wave-4 item B8 ("plain memory access 14-27 Ir before the
  tick"): hand it to that agent.

### A2 `execute_cb` `#[inline(never)]`
- Where: `cpu/exec.rs:64`. ADR 0008; per-function cap on `run`.
- Speed: a call and its register save per CB instruction if inlined
  into `run`; may hurt `run`'s codegen. Measure with L1, or leave.
  Effort: S.

### A3 `Ctx::serial_event` `#[cold]`
- Where: `bus.rs:917-926`.
- Why: 323c4f0 ("boundary: the serial shift and the scheduled-input
  events, both rare, go to cold out-of-line helpers (2288 -> 1250)",
  `boundary` cap 2,220).
- Not rare while a transfer runs: review B2 estimates serial at about 11%
  of worst-case. `#[cold]` moves it to `.text.unlikely` and biases the
  branch against it.
- Natural form: plain (inline or not, as measured). Wave-4 B2 (one event
  per byte) rewrites this path anyway: fold in. Effort: S.

### A4 `Timer::reload`, `Timer::schedule_overflow`, `Timer::sync`, `Timer::catch_up` `#[inline(never)]`
- Where: `timer/mod.rs:281-283`, `:305-308`, `:313-315`, `:336-338`.
- Why: 7def3ab ("Code size for a pending PPU feature ... -360 B ... None
  of those is hot"). `reload` + `schedule_overflow` run on every serviced
  TIMA overflow from `boundary` (22266e7, 0a424bc put both in ITCM; B-perf
  counts `schedule_overflow` at 67 Thumb instructions).
- Natural form: `reload` and `schedule_overflow` inline into the Timer
  event; `sync`/`catch_up` as the compiler chooses.
- Speed: two calls less per timer event. Matters once worst-case has a
  serviced timer IRQ (human decision, session 16). Bytes: +~360.
  Effort: S.

### A5 `Timer::write_div`, `Timer::write_tac` `#[inline(never)]`
- Where: `timer/mod.rs:376-379`, `:411`.
- Why: 265a078 ("the new code repartitioned the codegen units and LLVM
  inlined them into div_reset (508 -> 994 B) and write_timer_io (892 ->
  1,486 B)"); the comment still says "keeps the code-size budgets of its
  callers independent of codegen-unit partitioning". Moot with cgu=1.
- Speed: neutral. Effort: S (delete attribute and comment).

### A6 `Schedule::set` twin and `schedule_serial` pinned out of line
- Where: `schedule.rs:107-111` (`set` = `#[inline(never)]` wrapper of
  `set_inline`), `bus.rs:473` (`schedule_serial`), `:491`
  (`schedule_timer`).
- Why: f74cceb ("Schedule::set (out of line, cold)"); 386acba
  ("schedule_serial is pinned out of line ... so write_timer_io,
  div_reset and stop stay at their old sizes").
- Natural form: one `set`, `#[inline]`; no pin. Speed: neutral to
  slightly better on register-write paths. Effort: S.

### A7 `Ctx::read_io_other` split from `read_io`
- Where: `bus.rs:2559-2562`.
- Why: 6110717 ("rare I/O reads (BCPD/OCPD, FF74) move to
  `read_io_other` to keep `read_high` in budget").
- Natural form: arms of `read_io`. BCPD/OCPD reads are not always rare
  (palette fades read-modify-write). Speed: a call less on those reads.
  Effort: S.

### A8 `Ctx::stop_div_reset`, `Sys::set_div_apu_deadline`, `vdma_turn`, `start_audio`, `finish`
- Where: `bus.rs:553-557`, `:136-143`, `:707-723`; `lib.rs:793-796`.
- Why: 323c4f0 (`speed_switch` 1,082 > 880; `Emulator::run` 880 > 481)
  and a later STOP-mode commit ("keeps `stop_begin` within its code-size
  budget").
- Cold or once per run call: out of line is harmless. **Keep the
  placement; delete the "(code-size budgets)" rationale and the forced
  attribute where the compiler would do the same.** `vdma_turn`: check
  that the per-instruction test in `Emulator::run` stays inline.
  Effort: S.

### A9 Square channel methods forced out of line, "one copy for both squares"
- Where: `apu/square.rs:108`, `:126`, `:159`, `:242`, `:282`
  (`digital_at`, `advance`, `integrate`, `write_nrx4`, `frame_step`).
- Why: 3bb48a4 ("keeping Apu::write (436/438), sync, step and read_apu
  within their budgets"); 91ff0de (`write_nrx4`: `Apu::write` 426 -> 308).
- Natural form: no attribute. `integrate` is large and called twice per
  batch: the compiler will likely keep it out of line anyway.
- Speed: neutral to marginal. Effort: S.

### A10 `mix::constant` shared out of line
- Where: `apu/mix.rs:49-58`. Why: 4dd75a2 ("one shared function").
- Natural form: `#[inline]`, per channel. See T1 for its `black_box`.
  Effort: S.

### A11 `LineObjects::consider_cold` twin
- Where: `ppu/objects.rs:94-100`; callers in `Ppu::scan_to`
  (`ppu/mod.rs:2467`).
- Why: 65dc4a9 ("its two consider loops were runtime-unrolled by 4; a
  call to an out-of-line consider_cold in the body stops that (2,052 ->
  968 B, +72 B helper)").
- Natural form: call `consider`; let the cold loop unroll or not.
  Speed: neutral (OAM DMA during mode 2 only). Bytes: +1 KB.
  Effort: S. Lowest value of the A class: the twin is a smell, the bytes
  buy nothing. Take it only in a comment/attribute sweep with room left.

### A12 `Ppu::cpu_blocked` `#[inline(never)]`
- See L13 (same change).

### A13 `Renderer::fetch_objects` `#[inline(never)]`, `Renderer::last_fetched` out of line
- Where: `ppu/render.rs:931-936` ("inlined it would grow `render` past
  its size budget"), `:445`.
- Why: 17db555 (ADR 0019: one out-of-line `fetch_objects` call per span
  replaced a per-object call that cost cgb-acid2 +4.21%), 4c16263.
- Natural form: no attribute; one call site, so the compiler will inline
  it into `render` if it pays. Speed: a call less per span with objects.
  Effort: S. Measure with L2.

### A14 Undocumented remainder: 49 `inline(never)` / 15 `cold` in `bus.rs`, 32 / 25 in `ppu/mod.rs`
- Where: full list with one-line rationale excerpts in
  `/tmp/claude-1000/-home-jess-gbc-32/771053a9-0ac5-45ad-953b-96007463309d/scratchpad/attrs.txt`
  (375 lines, generated from HEAD; `timer/mod.rs` is missing from it, see
  A4/A5).
- Why: D13: all tuned under 16 codegen units and per-function caps; 4db1d02
  measured that under cgu=1 the hand placement is 0.6-2.4% slower on
  eight of nine benches.
- Sweep rule: keep `#[cold] #[inline(never)]` on paths named rare
  (mid-mode-3 writes, OAM DMA conflicts, STOP, speed switch, LCD-off,
  re-seek, RTC, long-span APU helpers). Remove the bare
  `#[inline(never)]`s whose comment says "off X" or "out of line" with no
  measurement, one file at a time, and keep a removal only if
  `check-bench.sh` does not regress. Candidates with no recorded
  measurement: `bus.rs` `write_ie` (:507), `write_timer_io` (:519),
  `apu_div_reset` (:668), `set_apu_deadline` (:687), `div_apu_event`
  (:740), `audio_buffer_event` (:754), `write_apu` (:765), `sync_ppu`
  (:946), `write_ppu` (:1055), `write_high` (:1234), `write_oam` (:1265),
  `write_palette_data` (:1299), `write_if` (:1328), `oam_dma` (:1444),
  `dma_ppu_event` (:1513), `write_hdma5` (:1945), `read_during_dma`
  (:2453), `read_oam` (:2602); `ppu/mod.rs` `LineTiming::finish` (:488),
  `m0_latched` (:736), `palette_blocked` (:972), `draw` (:1533),
  `scroll_write` (:1882), `set_stalls` (:2169), `update_at` (:2746),
  `write_stat` (:2823), `write_lyc` (:2923); `apu/mod.rs` `step` (:250),
  `write` (:340), `write_rest` (:378), `read_apu`-side (:436, :459);
  `apu/noise.rs` :287-:516; `apu/wave.rs` :113-:208; `cart/mod.rs`
  `read_rtc` (:901), `write_rtc` (:921), `latch_rtc` (:1060).
- Effort: M (one agent per file family, bench after each batch).

---

## 4. T: tables and loops made smaller at a speed cost

### T1 `black_box` used to stop loop unrolling in the APU integrators and mixer
- Where: `apu/square.rs:177`, `apu/wave.rs:233`, `apu/noise.rs:417`
  (batch loops read the span length through `black_box`),
  `apu/mix.rs:56` (`constant`), `apu/mix.rs:151-154` (the mixer loop
  index, plus a `& (BATCH - 1)` mask added by 1c7b1df because the opaque
  index brought back a bounds-check panic path).
- Why: 4dd75a2 and `docs/perf.md:140-142` ("LLVM's Cortex-M runtime
  unrolling (x4) otherwise doubles them on thumbv7em (`Square::integrate`
  836 -> 410 B, `Wave::integrate` 1,304 -> 642 B)").
- Natural form: plain loops. `black_box` is a best-effort hint, hides
  the trip count from every other optimisation too, and on x86 forces
  the value through memory.
- Speed: faster in principle (the unrolled form is what LLVM's Cortex-M7
  cost model chose); Ir effect not recorded. APU channels + mixing are
  about 224k Ir/frame on worst-case. Bytes: about +1.1 KB (426 + 662,
  plus noise and the mixer).
- BP: yes (`--hash-samples` fingerprint). Risk: low. Effort: S.

### T2 `fmt_flags` loop index through `black_box`
- Where: `bitset.rs:166-193`.
- Why: 58c87a4 ("black_box on the index keeps LLVM from unrolling the
  loop (586 B unrolled)").
- Natural form: plain loop. Keep the shared `fmt_flags` (see keep list).
  Speed: neutral (Debug only). Bytes: +~340. Effort: S. Cosmetic.

### T3 APU level lanes are a stack array cleared on every sync
- Where: `apu/mix.rs:36-46` (`BATCH`, `i16` lanes), `apu/mod.rs` `sync`
  (`let mut lv = [[0; BATCH]; 4]`).
- Why: `docs/perf.md:166-172`: the clear is paid on every APU register
  write; halving it to `i16` was 894.2 -> 884.6 Ir per write. "Not done:
  removing the clear altogether needs the lanes in `Apu` (... the `Apu`
  size test caps it at 128 B) or uninitialized memory (`unsafe`)".
- Natural form: the lanes live in `Apu` (store semantics: each integrator
  writes rather than accumulates into its lane), no clear.
- Speed: up to about 10-20 Ir per APU write on x86 (x86's vector memset
  hides most of it; `__aeabi_memclr4` of 128 B on the M7 is more).
  Bytes: `Apu` +128 B of state.
- BP: yes (`--hash-samples`). Risk: low-medium (the integrators'
  `+=` contract changes). Effort: M. Needs S12's cap gone.

### T4 `Colors::set_cgb` as a loop of calls
- Where: `ppu/render.rs:194-200` ("a plain conversion loop gets unrolled
  and vectorized into ~650 bytes (code size)").
- Why: 17db555. Reset-time only. **Keep** (no speed to gain); reword the
  comment. Listed so the count is honest.

### T5 Boot logo tiles from a nibble table
- Where: `bus.rs:236-255`. Why: f53c49c (474 -> 264 B). Reset-time only
  and arguably clearer. **Keep.**

### T6 `rom_tail` "stays rolled" by a call per byte
- Where: `cart/mod.rs:836-848` ("A loop with a call per byte and a trip
  count LLVM can't see, so it stays rolled (code size; it is never
  hot)"). Cold. Tied to ADR 0027 (R2). **Keep until ADR 0027 is
  decided**, then it disappears with `rom_block`.

---

## 5. Forced by ADR 0027 (byte-wide ROM reads): separate pending decision

Not budget distortions in themselves, but each was then squeezed for
bytes, and all go away if ADR 0027 is retired (Stage 3 ROM is HyperRAM on
OCTOSPI per the human's session-16 note, not FMC SDRAM; "ADR 0027 stays
until retired").

- **R1** `rom.rs:1-66`: `Rom` newtype, `Rom::get` with `compiler_fence`
  after every ROM byte load. 5d220e6: worst-case +0.005% Ir, `.text`
  +576 B. The fence also blocks any load combining around opcode and
  immediate fetches.
- **R2** `cart/mod.rs:820-834` `rom_block`: 16 separate `ldrb` for a VRAM
  DMA block instead of four word loads; `:836-848` `rom_tail`; c78613b
  reshaped it to save 254 B ("16 checked loads ... cost ~500 B").
- **R3** `cart/mod.rs:258-263` `header()` out of line so the MBC1M logo
  compare is a rolled byte loop instead of `memcmp`; `:266-270`
  `rom_byte`.
- **R4** `scripts/check-rom-loads.sh` and the marker comment in
  `rom.rs:62`.

If retired: `Rom` becomes `&[u8]`, `dma_block` copies a `[u8; 16]` chunk
for ROM as it does for RAM (L19 collapses into it), the logo compare is a
slice compare. Speed: small (HDMA blocks, load time). Bytes: about -600.
BP: yes.

---

## 6. Keep: size-motivated changes that were also speed wins or plain simplifications

Do not revert these. Where the comment gives "code size" as the reason,
reword it to the reason that still holds.

| # | What | Commit | Why it stays |
|---|---|---|---|
| K1 | `set_stalls` writes stalls in place, `sorted_object_stalls` sums as it goes | 7995d19 | -0.11% worst-case, -0.52% cgb-acid2 |
| K2 | One loop for object stalls with/without a window (`i32::MAX` start, tile key one `i32`) | 7fbb6b2 | -0.22% worst-case, -0.47% cgb-acid2; the `Option` forms were slower (+0.45% cgb-acid2) |
| K3 | `window_stall_at`, `OamDma::ppu_read`, `LineTiming::finish` out of line | 40570d0, d937a28, 7995d19 | cold callers; Ir unchanged |
| K4 | Insertion sort instead of `core`'s sort for 10 objects; packed-key `priority_order`, inlined into `latch` | 3b556ae, fd52709, b5d58d7 | -4.4 KB and faster; b5d58d7 already undid the size-only part (+96 B, -0.3% cgb-acid2) |
| K5 | VRAM accesses out of the inlined bus path | 22eccab | needed for the PPU position check; cpu_instrs -1.24% |
| K6 | Echo RAM in `read_high`; region key `addr \| hot.dma`; ROM windows by bit 14 | 75b12ad | the measured-fastest layout |
| K7 | Timer/serial register logic off `write_high` (`write_timer_io`, `Timer::read`) | 4b81a49 | cold I/O; `run` -5.5 KB with -25..-33% Ir |
| K8 | `Ppu::latch` out of line | e15d85f | inlined, LLVM hoisted its register loads into every sync |
| K9 | Cursor walk; `Latch`/`End` skip STAT evaluation | e15d85f | -12% cgb-acid2 |
| K10 | Dropping the per-speed LYC compare point | 2c1df77 | dead machinery; -206 B and -0.16% cgb-acid2 |
| K11 | Lines 144-152 share lines 1-143's LY=LYC arms | 13be1bd | hardware-justified (+2 ROMs), -154 B |
| K12 | `push_acknowledge_reg` cold; dispatch's old-IF select out of line | c18cfdc | +1.3-1.9% Ir when inline |
| K13 | OAM DMA restart tail kept off `dma_conflict` | 30609c3 | dma-storm +3.0% otherwise |
| K14 | `tile_sel_latch` out of `hold_fetch` | d3403bb | Ir (worst-case), cost 20 B |
| K15 | u32 division on APU advance paths; `rem_long`/`long_steps` cold | 7a2edbc | avoids `__aeabi_uldivmod` on M7 |
| K16 | 32-bit SWAR words in `Wave`; 256-entry popcount table in `Noise` | 29cacd2, d236d92 | M7 has no cheap 64-bit shifts or popcount (x86 Ir would prefer the opposite: do not "fix" from Ir alone) |
| K17 | `mix` as an explicit sum instead of `w.map(..)` | 91b11ea | removes dead `try_map`/`Drain` code; simpler |
| K18 | `i16` level lanes | 117461b | halves the per-sync clear (until T3 removes it) |
| K19 | `speed_switch_halt` as one cold `Bus` call | 58c87a4 | keeps the HALT arm of `step_not_running` clean |
| K20 | `fmt_flags` shared `Debug` body; `Hw::new`, `Sys::new`, `into_ejected`, RTC advance, `header` out of line | 58c87a4, fe0e18f, cbc14de | cold, deduplicated; drop only the `black_box` (T2) |
| K21 | `Mapper::Mbc1M` / `Mbc3Rtc` unit variants | ae12fdf, ADR 0016 | a flat mapper enum is a fine model; fix the "to keep `Cart` at 128 bytes" comments (`cart/mod.rs:522-533`) |
| K22 | `Sys` and `Memory` `#[repr(C)]` with hot fields at small offsets | f74cceb, 323c4f0, af81360 | a real Thumb-2 addressing effect (+1 KB in `run` when wrong). C8 (small arrays first in `Memory`) is an open improvement, not a revert |

Also keep: `write_to`/`write_ff` (the DMA-conflict twins exist for speed,
not size; D13 suggests factoring the shared bodies), `Renderer::slow`
(one-byte test on the hot path), `if_write_sees` comparing positions
without a division (bd674ce's speed half), `VramDma::stall: u16`.

Stale comments that still cite a dropped cap (docs sweep, no code):
`memory.rs:22-24`, `:66-69`, `:131-136`; `bus.rs:141`, `:555`, `:968`,
`:1026`, `:1427`, `:2560`; `ppu/mod.rs:236-239`, `:948`, `:1325-1326`,
`:1408-1409`; `timer/mod.rs:146-148`, `:280`, `:305`, `:376`;
`hdma.rs:91`; `lib.rs:268-275`, `:290`, `:793`; `apu/square.rs:34-35`,
`:107`, `:125`, `:158`, `:241`, `:281`; `apu/mix.rs:51`, `:55`, `:74`,
`:151`; `apu/mod.rs:367`; `ppu/render.rs:196`, `:730`, `:935`, `:1141`,
`:1404`; `ppu/objects.rs:96`; `cart/mod.rs:259`, `:524`, `:530-532`,
`:839`; `cpu/exec.rs:59-63`; `cpu/mod.rs:662`.

---

## 7. Open points for the human (do not decide in a fix agent)

1. **How big may `Hw` get?** The cap is gone, but ADR 0006's reason (the
   `Emulator` may be built on a small MCU stack) was not withdrawn. S5,
   S7, S11, S1-S3 add tens of bytes. S8 adds 864 B and T3 128 B. If the
   MCU front end holds the `Emulator` in a `static`, none of it matters.
2. **ITCM share for the front end on the H735.** Without it the room for
   L1 + A1 + L2 + L6 cannot be judged.
3. **ADR 0027** (section 5).
4. **L5 and S14 change behaviour** on paths no manifest ROM pins.

---

## 8. Recommended order of work

One theme = one fix agent, about 30 tool calls, in a scratch worktree.
Verification shorthand: **FP** = `scripts/diff-fingerprint.sh <base>`
identical (run it also with `--audio-buffer 7 --hash-samples` for APU
themes); **IR** = `scripts/check-bench.sh` (run alone; report every
bench, keep a change only if none regresses and the target improves);
**SZ** = `scripts/check-code-size.sh` total `.text` (report); **T** =
`cargo test --workspace`. Heavy steps under the session's exclusive lock.

Behaviour-preserving, state moves (no Ir expected to move):

1. **State home: speed switch, APU delay, FF55** (S2, S3, S9, S10, S12's
   test). Small scalar moves on cold paths. Verify: FP, T, SZ; IR once at
   the end (expect +-0.1%).
2. **State home: delayed LYC interrupt** (S1 + L10, move only; no fix of
   A2-04's stranded cases). Verify: FP, T; add the D1 unit test (IE = FF,
   no wake-up before the due M-cycle). IR for `sync_ppu`.
3. **State home: renderer fetch state** (S5, S7, S11, L18; `ObjRank`
   becomes the rank array; `Memory::clear` test with exhaustive
   destructuring, seen failing first, D3). Verify: FP, T, IR (render
   path), SZ.
4. **State home: OAM DMA read clears** (S4 move; then the `&mut` read
   experiment as a separate measured commit, dropped if IR regresses).
   Verify: FP, T, IR (cpu_instrs, dma-storm).
5. **Stalls type** (S13; `Stalls` with `Option` accessors, debug
   asserts). Verify: FP, T, IR (cgb-acid2, worst-case).

Behaviour-preserving, speed (each commit carries its Ir and bytes):

6. **CPU write path** (A1, measured in two forms; then A2 + L1 only if
   room remains). Coordinate with wave-4 B8. Verify: FP, T + SM83 suite,
   IR, SZ. Do this first among the speed themes: it sets the headroom.
7. **Renderer inlining** (L2, A13, L17, then L6 as its own commit).
   Verify: FP, IR (cgb-acid2, cpu_instrs, worst-case, cgb-raster), SZ.
8. **PPU walk and access checks** (L3, L4, L13/A12, L14, L8). Verify: FP,
   IR (cgb-acid2, cgb-raster, worst-case), SZ.
9. **APU loops** (T1, A9, A10, then T3 as its own commit; L16 with the
   sample-hash fingerprint). Verify: FP with `--hash-samples`,
   `tests/apu_output.rs`, IR (apu-writes, worst-case, cgb_sound), SZ.
10. **Timer and scheduler attributes** (A3, A4, A5, A6, A7; L7 as a
    measured experiment, dropped if `Cpu::run` Ir regresses). Best run
    after wave-4 B2/B5 and the serviced-timer-IRQ bench change, or by the
    same agent. Verify: FP, T (timer differential), IR (event-storm,
    halt-irq, worst-case), SZ.
11. **Attribute sweep** (A14, A8, A11, T2), one file family per agent:
    (a) `bus.rs`, (b) `ppu/`, (c) `apu/` + `cart/` + `timer/`. Verify:
    FP, IR after each batch, SZ.
12. **Comment and doc sweep** (section 6's stale-comment list; K21
    wording; `docs/perf.md` "Code-size budgets" rewritten to the final
    rules; D13's inlining convention paragraph). No code. Verify: T,
    `cargo doc`.

Not behaviour-preserving or awaiting a decision (after the above):

13. **A2-04 fixes on top of theme 2** (stranded/lost delayed LYC
    request), each with a test seen failing; full manifest.
14. **L5** (line 153's LYC=0 point under the late-deadline rule): unit
    test seen failing, full manifest, human FYI.
15. **ADR 0027 retirement** (R1-R4, L19) once the human retires it.
16. **S8** (object buffers and pair tables into `Renderer`) once point 1
    of section 7 is answered. Verify: FP, IR (cgb-acid2 was the +0.47%).
17. **L21 / S14, L12, L15, L20**: belong to the planned structure work
    (E1 oracle, E3 timeline table, D2 `bus.rs` split), not to this
    clean-up.

## 9. Top ten by expected value

Ranked by recorded or likely Ir gain, then by hazard removed.

1. A1 `write_mapped` inline (recorded up to several % on write-heavy
   code at M2; +8-10 KB; re-measure).
2. L6 no attribute decode in DMG compatibility mode (up to -3% on DMG
   games; +1.8 KB).
3. L2 tile loops inlined into `render` (-1.15% cgb-acid2, -0.5%
   cpu_instrs, -0.2% worst-case recorded; +4.1 KB).
4. S1 + L10 delayed LYC interrupt out of IF (removes a hazard on every
   IF reader/writer; neutral Ir).
5. S5 + S7 renderer fetch state out of `Memory` (ownership, `clear()`
   correctness, one argument less on the render calls).
6. T1 + T3 APU loops without `black_box`, lanes without the per-sync
   clear (APU is 224k Ir/frame and about 880 Ir per write).
7. L3 + L4 `point_irq` whole and `Latch` skipped (-0.15% and -0.25%
   cgb-acid2 recorded; under +1 KB).
8. L13 + L14 `cpu_blocked` specialised, `run_ppu_to` single, `vdma_arm`
   guarded instead of `#[cold]` (per VRAM/OAM access; per PPU event).
9. A3 + A4 serial and timer event paths not cold / not forced out of
   line (matters for the serviced-timer worst case; fold into wave 4).
10. A14 attribute sweep under cgu=1 (4db1d02 measured the hand placement
    0.6-2.4% slower than it was under 16 units; unknown how much comes
    back).

L1 (CB (HL) un-merge) is deliberately outside the ten: about 5 Ir per CB
(HL) instruction for 5.2 KB is the worst Ir-per-byte trade in the list.
