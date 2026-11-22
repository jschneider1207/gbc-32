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
