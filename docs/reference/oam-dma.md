# OAM DMA (FF46): timing and CGB C bus conflicts

Implemented in `crates/gbc-core/src/dma.rs` (state, conflict table) and
`crates/gbc-core/src/bus.rs` (`Ctx::oam_dma`, `dma_sync`,
`read_during_dma`, `write_during_dma`). The only model is CGB C (ADR 0003),
in both CGB and DMG-compatibility mode: the conflicts follow the hardware,
not the cartridge mode. HDMA/GDMA (FF51-FF55) is separate and not covered.

## Timing
M-cycle numbers count from the FF46 write's M-cycle (0). `m` is the current
M-cycle length (4 base clocks, 2 in double speed: the transfer is 160
M-cycles in either speed, Pan Docs "OAM DMA Transfer").
- **M-cycles 0-1: start delay.** No byte moves, no conflicts, and CPU OAM
  reads still work (unless restarting, below).
- **M-cycle `2 + i`, i = 0..159:** the CPU access in that M-cycle sees byte
  `i` as "the byte being transferred" (OAM blocked, conflicts against
  `source + i`). Byte `i` itself is copied half an M-cycle earlier, so a
  write in that M-cycle can't change it.
- **M-cycle 162:** the transfer is over; the `Dma` deadline is the clock of
  that M-cycle (`write + 162m`).

Source: SameBoy `Core/memory.c` (`GB_IO_DMA` write sets `dma_current_dest =
0xFF`, `dma_cycles_modulo = 2`; `GB_dma_run` spends one step on 0xFF -> 0,
copies 0..159, then 0xA0 -> 0xA1 = inactive; `is_addr_in_dma_use` treats
dest 0xFF/0 as warm-up). Mooneye `acceptance/oam_dma_start`,
`oam_dma_timing` and the `*_timing` instruction tests (which probe M-cycle
timing with OAM blocking) pass with it.

## Restart
A FF46 write while a transfer runs abandons the old transfer, but if the old
one was not yet in its last M-cycle (SameBoy: `dma_restarting = dest !=
0xA1 && dest != 0xA0`), CPU OAM reads keep returning FF through the new
start delay. A write in the old transfer's last M-cycle is a fresh start.
Mooneye `acceptance/oam_dma_restart`.

## CPU accesses during a transfer (CGB C)
- **OAM (FE00-FEFF):** reads FF from M-cycle 2 (or from the restart) to
  the end; writes are dropped for the whole transfer, start delay included
  (SameBoy `read_high_memory`/`write_high_memory`).
- **HRAM, I/O, IE (FF00-FFFF):** unaffected (Pan Docs "OAM DMA bus
  conflicts").
- **Everything else**, by bus. CGB buses: external (0000-7FFF, A000-BFFF),
  VRAM (8000-9FFF), WRAM (C000-FDFF); a source at E000-FFFF counts as WRAM
  (SameBoy `bus_for_addr`). Pan Docs only says the cartridge and WRAM are on
  separate buses on CGB; SameBoy's model (verified on hardware by its
  author) is finer, and is what `dma::conflict` follows:

| Source \ CPU access | external | VRAM | WRAM |
|---|---|---|---|
| ROM / SRAM | DMA byte | free | redirected |
| VRAM | free | DMA byte | free |
| WRAM | free | free | DMA byte |
| E000-FFFF | open (FF / dropped) | free | redirected |

  - *DMA byte:* a read returns the byte the DMA reads (`source + i`); a
    write is dropped and, if the source is below A000, clears the OAM byte
    just transferred (CGB C; SameBoy `GB_write_memory`, model checks).
  - *Redirected:* the access goes to WRAM at the CPU's low 12 bits with bit
    12 (the C000/D000 half) taken from the DMA's address; reads and writes
    both go there.
  - The address the DMA reads next is never a conflict (SameBoy "Shortcut
    for DMA access flow").
- **DMA from E000-FFFF** writes FF to OAM on CGB (SameBoy `GB_dma_run`; the
  DMG reads the echo instead). Pan Docs lists sources only up to DF.

## Implementation notes
- Nothing runs per M-cycle (ADR 0011). `Hot::dma` (FE00 while a transfer
  runs, else 0) is ORed into the read dispatch, so during a transfer every
  read reaches `read_high` and the conflict path; writes test it in the
  out-of-line `write_mapped`. Measured cost: `docs/perf.md`.
- Bytes are copied lazily (`OamDma::due`): before any CPU write during a
  transfer (it may change a source byte or its bank), on a restart, when a
  run call ends (so OAM is exact between calls), and at the `Dma` deadline.
  The PPU does not read OAM yet.
- `peek` and the DMA's own source reads use the plain `read` with the flag
  cleared: no conflicts, OAM as it is.
- HALT/STOP idle skips stop at the `Dma` deadline like any other.
- DMA continues through HALT (SameBoy defers the copies while halted but
  catches up with the same timing; nothing can observe the difference
  until the PPU reads OAM).

## Unverified
- A speed switch during a transfer: the remaining bytes keep their M-cycle
  count at the new speed from the switch (`Ctx::dma_rescale`). No test ROM.
- What the PPU sees during a transfer (Pan Docs: objects read as
  off-screen in mode 2, the word being written in mode 3): M3.
