# 0017: CGB VRAM DMA runs between instructions, triggered by a slow event

- Status: Accepted
- Date: 2026-11-22

## Context
CGB VRAM DMA (FF51-FF55, `crates/gbc-core/src/hdma.rs`) stalls the CPU
for a GDMA, and for each 16-byte HDMA block at the start of an HBlank.
SameBoy (Core/sm83_cpu.c, `GB_cpu_run`) runs the transfer right after an
opcode fetch, when `hdma_on` is set: by the FF55 write, by its HBlank
trigger (Core/display.c, 3 dots after the single-speed mode-0 STAT change,
not while halted or stopped), by `GB_lcd_off` outside mode 0, or on a
wake-up from HALT in mode 0 when HALT began outside it. An interrupt
dispatch due at that point goes first. The core has no per-instruction
hook (ADR 0008), and the hot path must not gain one.

## Decision
- **Where it runs:** between instructions, in `Emulator::run`. A due
  transfer sets `VramDma::pending` and `Ctx::done`, so `Cpu::run` returns
  after the current instruction (`done` is already tested once per
  instruction). The run loop then runs it if `Cpu::fetches_next` (running,
  no dispatch due), else lets the CPU take one step (the dispatch, or a
  halted idle) and checks again. The stall is idle M-cycles through the
  event loop (`Ctx::idle_mcycles`, shared with the speed-switch pause):
  `1 + 8n` M-cycles at single speed, `1 + 16n` at double speed (SameBoy
  `GB_hdma_run`: 2 dots per byte). The opcode fetch then happens after the
  stall, where SameBoy fetches before it: only the fetch's own read moves,
  which matters only for code in VRAM/OAM.
- **The HBlank trigger:** a new slow event kind `VramDma`, after `Dma` in
  dispatch order (not folded per event, ADR 0011's hot/slow split). It is
  armed, once per line (`VramDma::line`), from every PPU catch-up
  (`Ctx::run_ppu_to`) while an HBlank transfer is active, at
  `Ppu::mode0_start() + 3`. Every visible line's mode-3 end is already a
  PPU deadline, so the line is always latched by then; a trigger found
  late (by less than an M-cycle) fires at once.
- **Copy:** all bytes of the step at its start, after drawing the pixels
  before it. Sources in VRAM and E000-FFFF read FF (SameBoy: the CPU data
  bus, not modeled).

## Consequences
- No new work per M-cycle or per instruction; one branch per PPU sync
  while HDMA is active; a loop around `cpu.run` per run call.
- A run call may end past its target by one transfer step (up to 1025
  M-cycles for a 2 KiB GDMA), like an instruction.
- Deferred: byte-by-byte timing against a PPU reading VRAM in mode 3,
  OAM DMA running during VRAM DMA, the VRAM-bank and CGB-E write
  corruptions SameBoy models, STOP, HALT with IME off (the woken
  instruction runs before the block here), and the CPU data bus as the
  source for VRAM/E000+ addresses.

## Amendment (2026-11-23, M9 group 8): HALT wake-up and the HDMA block

The deferral "HALT with IME off (the woken instruction runs before the
block)" no longer holds:
- **Block before the woken instruction.** SameBoy `GB_cpu_run` checks
  STAT for the wake-up's block after the wake-up M-cycle (`halted` is
  cleared after its `GB_advance_cycles(4)`), then fetches the next opcode
  and runs `GB_hdma_run` before executing it. A new `Bus::wake_stalls`
  hook, called after the wake-up idle, does the mode-0 check there and
  ends the CPU step when a block is due, so the run loop runs the block
  (stall included) before the instruction; with IME on the dispatch still
  goes first. gambatte `dma/hdma_late_m0unhalt_{1,ds_1}` (a wake-up that
  ends in the next line's mode 2 starts none), `hdma_transition_halt_
  hdmadst_unhalt` and `hdma_transition_oamdma_2` (the instruction after
  the HALT sees the block's effects).
- **The HALT's sample** (`allow_hdma_on_wake`) treats the HBlank as
  starting at the HDMA trigger (`HBLANK_DELAY` after the mode-0 STAT
  change), not at the STAT change: a HALT between the two lets the wake-up
  start a block. Fitted to gambatte's CGB C captures
  (`hdma_late_m3halt_m0unhalt_scx{1_3,2_2}` need it, `hdma_late_m0halt_*`
  bound the other side); SameBoy samples raw STAT at the HALT's fetch,
  which differs from our grid by a few dots.
- **Kept as before:** the speed-switch halt's interrupt wake-up still
  checks at the wake-up M-cycle's start and runs the block after the next
  instruction (moving it, in a trial that also suppressed HBlanks in the
  wake-up M-cycle, regressed 5 `hdma_*speedchange*` ROMs), and
  HBlanks start blocks again from the wake-up M-cycle on, where SameBoy
  still suppresses them. Only the `hdma_late_m0halt_ds_lcdoffset1_{1,2}`
  pair tells the latter apart, and which half passes depends on the
  `lcd_offset` CPU/PPU phase (M9 triage), so it stays until that is fixed.

## Amendment (2026-11-23, M9 group 8): trigger, IRQ order, HBlank period

Fitted to gambatte's CGB C captures (gambatte, GPL, read for behaviour
only), replacing parts of the SameBoy-based rules above:
- **Trigger:** `HBLANK_DELAY` is 2 dots after the mode-0 STAT change,
  not 3 (gambatte: xpos 167, one dot after its mode-0 interrupt at 166).
  `dma/hdma_start_scx{,2,3,5}*` and `hdma_late_disable_scx*` (both
  speeds) allow only 2.
- **Block before dispatch:** a pending step runs before an interrupt
  dispatch due at the same boundary (`Cpu::running`; gambatte's DMA event
  pre-empts the dispatch, also at a HALT wake-up with IME on):
  `irq_precedence/{,late_}hdma_vs_*` (11).
- **HBlank period** (`Ppu::hdma_period`): from the trigger to 4 dots into
  the next line (where STAT still reads mode 0). The HALT sample and the
  wake-up check use it; an FF55 enable sees it one M-cycle later
  (gambatte `isHdmaPeriod(cc + 4)`), so an enable at the HBlank's end
  waits for the next line (`hdma_late_enable_{,ds_}2`) and one between
  the STAT change and the trigger takes this line's trigger.
- **Lost triggers:** a trigger during a block's stall is lost (gambatte
  acknowledges the request at a transfer's end;
  `hdma_transition_{,ei_}halt_late_unhalt_scx1_*`), and one in a
  speed-switch STOP's last M-cycle falls into the switch's halt
  (gambatte `stop(cc - 4)`; `hdma_late_m3speedchange_*_scx1_*`).
- **Known swap:** `hdma_late_m0halt_ds_lcdoffset1_{1,2}` and the
  `hdma_late_enable_*lcdoffset*` ROMs bracket the period end ~3 clocks
  off from the same tests without the lcd_offset phase, so they depend
  on the lcd_offset CPU/PPU phase fix (M9 triage); `_1` now passes and
  `_2` fails.

## Amendment (2026-11-23, M9 design step 10): the swap was the sync

The "known swap" above was not an HDMA-period edge. Traced, the FF55
enable and HALT samples of `hdma_late_{enable,m0halt}_*` fit the period
as stated (in up to `t = 3` of the next line, out from 4) at every CPU
phase and in both speeds, once the PPU edges around them are right:
the double-speed LYC comparison at `t = 5` (it syncs the
`ds_lcdoffset1` rows; design step 3) and the single-speed VRAM block
from dot 86 (`hdma_late_enable_lcdoffset3_1` reads VRAM after its
block's stall; design step 7). `HBLANK_DELAY` and `HBLANK_UNTIL` are
unchanged; all 8 `hdma_late_{enable,m0halt}_*lcdoffset*` ROMs pass.

## Amendment (2026-11-23, M9 group 8): HDMA across a speed switch

Fitted to gambatte's CGB C captures (gambatte `Memory::stop`/`dma`/
`intevent_unhalt`, GPL, read for behaviour only). A block triggered in a
speed-switch STOP's first M-cycle (gambatte samples at `stop(cc - 4)`)
makes STOP's second byte run as the next opcode (`VramDma::refetch`, PC
rewound; gambatte `prefetched`). To double speed the block runs in the
halt and ends the transfer (FF55 bit 7 set, length kept: `Memory::dma`
while `halted()`); to single speed it runs at the wake-up before that
opcode, its stall one M-cycle shorter (`cc -= 4` in the `hdma_requested`
state). A trigger in the last M-cycle is still lost.

The "Kept as before" rule of the HALT amendment is replaced: the
speed-switch wake-up checks for its block like HALT's (`Bus::wake_stalls`,
end of the wake-up M-cycle, block before the next instruction), also when
the countdown ends it (gambatte `intevent_unhalt`;
`hdma_m3speedchange_late_m0wakeup_1`), and an HBlank inside its wake-up
M-cycle starts no block (SameBoy clears `halted` after it;
`hdma_m0speedchange_late_m3wakeup_scx2_2`).

Known gap: the refetched byte is read again from memory, where hardware
runs the latched one; they differ only if the in-halt block overwrote it
(`hdma_transition_speedchange_7fffstop_inc`). Fixing it needs a
latched-opcode path in the CPU fetch (a branch per fetch or a second
dispatch copy); not worth it for one ROM. +20 `dma` ROMs, no regressions.

## Amendment (2026-11-23, M9 group 8): the opcode around a block, and HALT's request

Fitted to gambatte's CGB C captures (gambatte `Memory::halt`/`stop`/`dma`,
`Interrupter::prefetch`, GPL, read for behaviour only). SameBoy
`GB_cpu_run` also fetches before `GB_hdma_run`.

**The opcode is latched before the block.** This replaces "the opcode
fetch then happens after the stall". The fetch's M-cycle still comes
after the stall, but its value is latched before the block
(`Cpu::latch_fetch`, `Mode::Latched`). A block that overwrites the
opcode, or mode 3 starting during the stall, therefore doesn't change it
(`hdma_pc_7ffe`, `late_gdma_pc_7ffe_1`). If an interrupt dispatch is due
then, the fetch is undone.

**A block requested in HALT's M-cycle.** gambatte calls this
`prefetched`. The byte after HALT is latched at the HALT. It runs after
the wake-up and the block, with PC unchanged, so it runs twice (as in the
HALT bug), and before any dispatch (`Mode::Prefetched`). ROMs:
`hdma_late_m3halt_m2unhalt_inc_*_2`, the `*ldaaimm*` ROMs,
`7fffhalt_inc`.

**Speed-switch STOP.** STOP's second byte uses the same latch instead of
the PC rewind. This closes the known gap in the speed-switch amendment
(`7fffstop_inc`).

**Stall and lost HBlank triggers.** That block's stall is one M-cycle
shorter (gambatte `cc -= 4`). HBlank triggers up to its full length are
still lost, because gambatte acknowledges the request at the transfer's
end (`VramDma::refetch` RAN; the end is kept in `switch_end`).

**HALT wake-up M-cycle.** An HBlank inside it no longer starts a block,
as after the speed switch (SameBoy clears `halted` after it;
`hdma_m0halt_late_m3unhalt_scx1_2`). This supersedes the "kept as
before" note on the `lcdoffset1` pair, which still passes.

Still deferred: OAM DMA during a VRAM DMA (`hdma_transition_oamdma_1`).
+11 ROMs, no regressions.
