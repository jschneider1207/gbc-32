# 0014: Lazy PPU mode timing with update points and one event per line

- Status: Accepted
- Date: 2026-11-21

## Context
M3 needs exact PPU mode timing (ADR 0002): STAT mode and LY as the CPU sees
them, the STAT interrupt with its blocking ("the line is an OR, only the
rising edge interrupts"), LYC including line 153, the mode-2 interrupt
quirk at line 144, the variable mode-3 length, and a place where the
scanline renderer draws each line. The PPU is on the hot path: a halted
game wakes only for events, and every event costs the dispatch in
`Ctx::boundary` plus the handler.

The obvious model is a state machine with an event at every mode change
and LY change (SameBoy's `GB_display_run` advances through ~40 states per
line). That is 5-8 events per line, about 1,000 per frame, most of them
observed by nothing.

## Decision
**Reads are computed, not stepped.** LY, the STAT mode bits and the
coincidence bit are pure functions of the frame position
`p = now - frame_start` (base clocks; a line is 456) and, for the mode-3
end, of the current line's latched timing. `frame_start` is set when the
LCD is enabled (the write's clock is position 7) and advances a frame at a
time. Reads are side-effect free, as `peek` requires.

**Positions, not SameBoy's nominal times.** SameBoy runs an action timed
`n` dots after LCD enable once the CPU has advanced *more than* `n` dots,
so a CPU access at `n + 1` is the first to see it. The tables in
`ppu/mod.rs` are positions where the state at `p` includes every action at
`p`; LCD enable at position 7 makes line 1 start at position 456. Every
M-cycle boundary in single speed then sits at position 3 mod 4. The
post-boot position (`POST_BOOT_POS`, line 144) keeps that phase: Mooneye
`intr_2_*` fail by one M-cycle otherwise.

**The STAT line is walked over update points.** The line is re-evaluated
only at SameBoy's `GB_STAT_update` call sites (3-5 per line: `t = 3, 4`,
the mode-3 end + 1, the VBlank-line comparison points, line 153's
`t = 6, 8, 12`) and at STAT/LYC writes, with SameBoy's
`mode_for_interrupt`, `ly_for_comparison` and held `lyc_interrupt_line`
semantics. `Ppu::sync(now)` walks the points since the last sync and
requests IF on rising edges. `Ppu::next_event` walks forward without
committing and stops at the first point that requests an interrupt,
completes the frame (VBlank) or ends a visible line's mode 3.

**One event per visible line, at the end of mode 3.** That point is where
the renderer draws the line: VRAM, OAM and the palettes are CPU-blocked
during mode 3, so drawing there sees what the hardware saw, and register
writes during mode 3 flush up to `Ppu::mode3_pixel` first. The line's
mode-3 length is latched when its pixel pipeline starts (`t = 89`, the
`Latch` point, walked as part of the next sync), from SCX, the window and
the selected objects: `167 + SCX % 8 + penalties` dots from there (Pan
Docs "Mode 3 length" and "OBJ penalty algorithm"; checked against all 104
cases of Mooneye `intr_2_mode0_timing_sprites`). The event is first set
at the earliest possible end (no penalty); the sync there latches the line
and, if the line is longer, moves the deadline to its real end. STAT reads
before the earliest end need no latch (the mode is 3 regardless).

**Quiet mode.** With no STAT source selected the line stays low, so the
walk visits only the points that do work (`Latch`, `End`, `VBlank`), and
the LY=LYC part of the line is recomputed when a source is selected.

**OAM summary.** A 144-bit map of lines with objects is rebuilt after OAM
changes (`Ppu::oam_written`, called from CPU OAM writes, DMA copies and
the DMA conflict clear), so lines without objects skip the OAM scan.

## Consequences
- Instruction counts (20 emulated s, callgrind, see `docs/perf.md`):
  cpu_instrs +6.8% (+79k Ir/frame), cgb_sound +16.8%, cgb-acid2
  12.9 M -> 341.7 M (+274k Ir/frame: it selects STAT sources and has
  objects on most lines; it spends most of its time halted, so the
  baseline was almost nothing), halt-irq +45.7%, dma-storm +9.0%,
  event-storm +2.6%. Per line event about 470 Ir quiet without objects,
  940 with STAT sources and objects, including dispatch. The renderer
  will add its own per-line work at the same event.
- The per-line event exists before the renderer does; its cost is
  measured now rather than appearing with the renderer.
- Double speed follows SameBoy where it differs cheaply (mode 0 one dot
  later, line 153 LY = 0 two dots later, the coincidence bit holding while
  the comparison is off); the double-speed STAT bit during a hold window
  ignores a LYC write made in it (approximation, documented in code).
- Not modelled: OAM DMA as the PPU's OAM scan sees it (M3 task), the
  DMG-only quirks, mid-line SCX changes to the fine-scroll penalty, WX
  edge cases, SameBoy's mode-3 abort.
- Further cuts if the per-line cost matters on the MCU: commit the walk's
  simulated state at the event instead of re-walking, and a cursor
  instead of the division-based `next_point`.

## Amendment 1 (2026-11-23, session 10): post-boot position 171

The post-boot PPU position moved from 163 to 171 (740154c). "Mooneye
`intr_2_*` fail by one M-cycle otherwise" holds only for positions off the
4-dot M-cycle grid: `intr_2_*`, `boot_*` and `stat_irq_blocking` pass at
every on-grid position from 155 to 171. 163 was the grid point nearest
Gambatte's measured 164, which is in Gambatte's own line origin. What pins
171 is Gambatte `display_startstate/stat{,_scx2,_scx3,_scx5}_{1,2}` (CGB04C
results: two STAT reads one M-cycle apart must see 87 then 84); of 155-179
only 171 satisfies both halves. With it, the CGB B/C line-0 dot-3 mode-0
read (AGE `stat-mode.inc` `M1E`) no longer breaks `stat_irq_blocking`. One
position serves both cartridge modes; no test we have measures the DMG-mode
handover separately (compare ADR 0022 for DIV).
