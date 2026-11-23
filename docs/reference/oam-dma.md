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
Not modelled: gambatte keeps the old transfer running through the new
start delay with the *new* source's conflict area, so the CPU access one
M-cycle after the FF46 write already conflicts on the new bus and reads
the byte the old transfer is writing (taken from the new source page).
gambatte cgb04c `oamdma_src8000_srcchange0000_busyinc` pins it: the
first `INC A` fetched after the restart (from ROM, source 00) is lost;
we lose only the two at bytes 0-1 and show 1, not 0.

## HALT pauses the transfer
While the CPU is halted the DMA makes no progress, and the halted M-cycles
are dropped, not caught up. Source: SameBoy master (MIT): `GB_dma_run`
(`Core/memory.c`) returns early while `halted || stopped`, and
`GB_advance_cycles` (`Core/timing.c`) overwrites `dma_cycles` on every call,
so the halted cycles are lost. Hardware evidence: gambatte
`oamdma/oamdmasrc80_halt_m2irq_read8000` expects 81 on dmg08 and cgb04c (it
reads source byte 31 after ~33 running and ~90 halted M-cycles); it needs
STAT mode-2 IRQs; it passes (throwaway harness, M3), as does
`oamdmasrc80_halt_lycirq_read8000` (81).
- **Where it starts:** HALT's own fetch M-cycle still steps (SameBoy
  `halt()` advances it with `halted` still false); the pause starts at the
  end of it, when the CPU halts. A HALT that doesn't halt (an interrupt
  pending, the HALT bug) pauses nothing.
- **The start delay is frozen too:** its step (SameBoy `dma_current_dest`
  0xFF -> 0) is a `GB_dma_run` step like the others.
- **OAM stays blocked:** the transfer is still active (`GB_is_dma_active`:
  `dest != 0xA1`). The CPU can't access anything while halted; for the PPU
  see "What the PPU sees" (a halted transfer with bytes left blocks the
  OAM scan on CGB C).
- **Wake-up:** on CGB SameBoy samples IE & IF, advances the +1 wake-up
  M-cycle still halted (dropped too, `docs/reference/halt.md`), then sets
  `dma_cycles = 4` and runs exactly one step (`GB_cpu_run`, both the IME = 0
  and the dispatch branch), before the next fetch or the dispatch. So by the
  next CPU access the transfer has done every step up to the end of HALT's
  fetch M-cycle plus one: as if it resumed at the start of the wake-up
  M-cycle, which is what we do (`Ctx::dma_resume`). Only the PPU could see
  the step's position inside that M-cycle.
- **STOP:** SameBoy pauses it as well (`stopped`; `leave_stop_mode` takes one
  step), but our STOP is an M5 stub, so it is not paused yet.
- **Speed-switch pause:** in SameBoy the CPU is `halted` through
  `speed_switch_halt_countdown` (no IRQ pending), so the DMA pauses; that
  halt ends inside `GB_advance_cycles` with no extra wake-up step. Ours
  rescales instead (`dma_rescale`); M5 replaces it.
- **Lockup:** SameBoy's `ill` sets `halted` with IE = 0, so a transfer
  freezes forever. That is SameBoy's way to model a hung CPU, not a
  hardware claim; we keep the DMA running. Unverified.

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
| E000-FFFF | open (FF / to OAM) | free | redirected |

  - *DMA byte:* a read returns the byte the DMA reads (`source + i`). A
    write never reaches its target; the OAM byte just transferred (`i`)
    gets the CPU's data for a ROM/SRAM source, 00 for a VRAM source, and
    keeps the DMA's byte for a WRAM source. *Open* (E000-FFFF source,
    external bus): reads FF, a write puts its data in OAM byte `i` too.
    Source: gambatte cgb04c hardware captures (`oamdma_src*_busypush*`,
    `busywrite*`; gambatte `Memory::nontrivial_write` matches them), ADR
    0028. **SameBoy disagrees:** it drops the write and clears OAM byte
    `i` for any source below A000 (`GB_write_memory`, a model check its
    author applies to CGB C); the human chose gambatte (session 10).
    A *read* that conflicts with a VRAM source returns the DMA byte and
    also clears OAM byte `i` (gambatte cgb04c
    `oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*`; gambatte
    `nontrivial_read` matches them; ADR 0028 amendment). The read only
    records it (`dma::ReadClears` in `Memory`, atomics: the read path keeps
    shared references); `Ctx::dma_sync` applies it after copying the bytes
    due. SameBoy has no read side effect.
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
  Also before every PPU sync, so the PPU finds the bytes written by its
  reads in `mem.oam` (and the others in `Memory::oam_dma_old`).
- `peek` and the DMA's own source reads use the plain `read` with the flag
  cleared: no conflicts, OAM as it is.
- HALT pauses the lazy model: at the halt (`Ctx::dma_pause`) the bytes due
  are copied, `OamDma::paused` caps the progress at that clock, and the
  `Dma` deadline is cleared, so the halted skip (ADR 0011) passes it. At
  wake-up (`Ctx::dma_resume`) `start`, `end` (and a pending `block_from`)
  move out by the paused time and the deadline is set again. The CPU calls
  `Bus::halt_begin`/`halt_end`; nothing is added per M-cycle.
- The STOP and lockup idle skips stop at the `Dma` deadline like any other.

## Unverified
- A speed switch during a transfer: the remaining bytes keep their M-cycle
  count at the new speed from the switch (`Ctx::dma_rescale`). No test ROM.
- The PPU during a restart: it sees only the new transfer's window, so
  the old one stops for it at the FF46 write and the new one starts 2 dots
  before the new byte 0 (SameBoy keeps it active throughout). No test ROM.
- What the mode-3 object fetch reads, and when (below). No test ROM.

## What the PPU sees (CGB C)
Implemented in `dma.rs` (`OamDma::ppu_at`, `ppu_read`) and `ppu/mod.rs`
(`Ppu::scan_to`, `fix_attrs`, `dma_event`).

**Provisional (M3 review).** Both deviations from SameBoy below (the
2-dot PPU lead and the last step not held through HALT) were fitted with
a throwaway hex harness that was not committed; romtest's `gambatte`
check still reports `unsupported`. Re-check them against the gambatte
ROMs named below once that check exists (plan task, M5).

**When.** The PPU sees each step of the transfer 2 dots before the CPU
access it is due by, in both speeds: byte 0's write at `start - 2`, byte
`i`'s at `start + i*m - 2`, the last step (the end) at `end - 2`; a PPU
read at clock `c` sees the steps before `c`. Evidence: gambatte
`oamdma/late_sp{00,01,02,39}{x,y}_*` (30 ROMs, cgb04c; DMA starting or
ending around the OAM scan's read of entry 0, 1, 2 or 39, read back
through the mode-3 length): single speed allows 0-2 dots, double speed
2-3, so 2 dots fits both; all 30 pass. SameBoy differs: its display sees
the transfer active from the FF46 write (`dma_current_dest = 0xFF`), which
`late_sp00x_2` (cgb04c out3) contradicts, and its byte writes are half an
M-cycle (1 dot in double speed) early.

**The OAM scan (mode 2).** SameBoy `add_object_from_index` (MIT): one
entry every 2 dots from the mode-2 STAT change, `4 + 2i` dots into the
line. Per entry, by what the transfer looks like then:
- *off* (before byte 0's write, in the start delay, after the end): Y and
  X from OAM as it is then;
- *running*: the PPU can't read OAM; the scan compares the Y and X it last
  read (`mode2_y_bus`, `mode2_x_bus`: the last scanned entry's, then each
  object fetch's tile index replaces Y), so unless that Y covers the line
  nothing is found, and if it does, each such entry becomes an object at
  that X. Pan Docs ("OAM DMA bus conflicts"): "most PPU revisions read
  each object as being off-screen". The two agree unless the last Y
  covers the line; we follow SameBoy. When tracking starts (an FF46 write
  after lines it didn't touch) the bus is taken from OAM as it is then:
  exact unless the CPU wrote those bytes since the PPU read them;
- *halted* (HALT with bytes left): CGB revisions 0-D find nothing (SameBoy;
  CGB E would read the word being written). The bus gets the word being
  written. The CPU's wake-up M-cycle still counts as halted (SameBoy clears
  `halted` after it).

**HALT and the PPU.** The window freezes with the transfer: halted in the
start delay, the transfer hasn't started for the PPU until after the
wake-up; with bytes left, it is active (and halted) until the wake-up and
ends as late as the transfer does. With every byte written, the last step
is *not* held: the PPU sees the transfer end on time. Evidence: gambatte
`oamdma_late_halt_stat_1` (out0) and `_2` (out3), cgb04c: HALT one M-cycle
before (1 byte left) or at (0 left) the transfer's last M-cycle, halted
through line 4's read of OAM entry 0 (woken by the timer at t = 23);
the object (entry 0) on line 4 is missed in `_1` and found in `_2`. SameBoy holds `dma_current_dest = 0xA0` through
the halt, which would miss it in `_2` too.

**Object fetches (mode 3).** The fetch reads the object's tile index and
attributes from OAM, or, while bytes 1-159 are being written, the 16-bit
word being written (SameBoy `oam_read`: `oam[(dest & ~1) | (addr & 1)]`
with `dest` the next byte; Pan Docs: "the PPU reads whatever 16-bit word
the DMA unit is writing ... an incorrect tile number and attributes"). We
place the read `d - 5` dots into the object's `d`-dot stall (SameBoy: after
the wait for the BG fetch and one more dot). Untested by any ROM.

**STAT.** While a transfer runs (as the CPU sees it: from the FF46 write
to the end), STAT reads mode 0 instead of mode 2. SameBoy `GB_STAT_update`
clears the mode bits whenever it runs in mode 2 with DMA active (the mode-2
start, STAT/LYC/FF46 writes), and `GB_dma_run` sets mode 2 again when the
transfer ends during the scan. Gambatte has no such rule, and none of the
674 cgb04c gambatte hex-output results changes with it (throwaway harness);
no ROM we have is known to test it. The STAT interrupt line is not affected.
