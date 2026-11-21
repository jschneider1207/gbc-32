# 0002: Catch-up timing model (accuracy without per-dot cost)

- Status: Accepted
- Date: 2026-11-21

## Context
The goal is high accuracy, but performance comes first: this is meant to be
a fast, modern GBC on a Cortex-M7, not a reference emulator. Advancing every
component every T-cycle (or rendering pixel by pixel through a FIFO) is the
most accurate approach but too expensive for that budget.

## Decision
- **CPU, timer, interrupts, OAM DMA, HDMA:** timed per M-cycle. Every memory
  access happens on its correct M-cycle. This is cheap, and most game
  compatibility depends on it.
- **PPU:** mode timing (OAM scan, the variable mode-3 length, HBlank, VBlank,
  STAT/LYC interrupts, VRAM/OAM access blocking) is exact. Pixels are drawn a
  scanline at a time, but any write to a PPU register during mode 3 first
  draws up to the current dot, so mid-line effects come out right.
- **APU:** catches up lazily on register access and at buffer boundaries,
  then generates samples in batches.

## Consequences
- Should pass Blargg, the acid2 tests and Mooneye acceptance. Mealybug-style
  mid-line fetcher quirks may not all be reproduced; they are tier `stretch`.
- The catch-up points must be complete. A missed register path shows up as a
  rendering bug; the acid2 and Mealybug tests are the safety net.
