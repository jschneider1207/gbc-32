# 0039: The display transfer: MDMA from TE, one buffer, SPEC timing

- Status: Accepted (agent decision)
- Date: 2026-11-25
- Builds on: plan `docs/plan/stage-2.md` M3 (tasks "Frame in DTCM",
  "Tearing") and Risks 3-5; `docs/reference/lcd-panel.md` (facts and
  measurements, section "MDMA and TE")

## Context
Each emulated frame has to reach the shield's ST7789V over the 8-bit FMC
bus: 240x216 RGB565 (1.5x), 103,680 bytes. A CPU copy costs the CPU the
whole bus time (37 CPU cycles per byte at `SPEC`, about 3.8M cycles, half
a frame). The write must also stay ahead of the panel's scan (no tearing)
and the panel's refresh should be as close as it can get to the Game
Boy's 59.7275 Hz. To decide: who moves the bytes, what starts the
transfer, how many buffers, which FMC timing, which frame-rate setting.

## Decision
1. **MDMA channel 0, software-requested, word reads packed into byte
   writes** (TCR 0xE3FC_4222, BNDTR 0x0010_CA80: two 51,840-byte blocks;
   values and sources in `lcd_dma.rs` and the reference note). MDMA
   because it is the D1-domain DMA, an AXI master next to both the AXI
   SRAM buffer and the FMC (DMA1/DMA2 sit in D2 and would reach both
   through the D2-to-D1 bridge), and its 128-byte buffers with 16-beat
   reads keep the FMC write FIFO fed: the measured 8.099 ms is the bus
   rate itself.
2. **Started from the TE interrupt.** TE (PE13) on EXTI13, rising edge
   (start of V-blank, TEON mode 1). The EXTI15_10 handler starts a
   transfer the main loop has queued: RAMWR, DSB, SWRQ (57 to 85 cycles
   from its entry). The main loop never waits for TE; it waits only for
   the previous transfer before it writes the buffer again. Completion
   (CTCIF) and errors (TEIF, CxESR) are polled by the main loop, the
   MDMA interrupt stays off: nothing has to happen at that moment.
3. **One buffer** (128 KiB aligned in AXI SRAM, MPU region 8 Normal
   non-cacheable over exactly it), **no double buffering**. The transfer
   takes 8.1 ms of a 16.7 ms frame and starts in V-blank, so the buffer
   is free again 8.1 ms after TE; the conversion (0.38 ms) of the next
   frame fits in the other 8.6 ms. A second buffer would cost 104 KB of
   AXI SRAM to remove a wait that, with the core running between frames,
   should rarely happen. Revisit if M5 shows the main loop waiting.
4. **FMC timing `SPEC`** (per_ck = HSI 64 MHz, 5 kernel cycles per byte,
   every panel minimum met including the hold times): 8.099 ms per frame
   measured, against 7.03 ms computed for the out-of-spec `FAST`. The
   difference costs nothing (the CPU is not involved) and `SPEC` keeps the
   write ahead of the scan with 9.8 µs per row to spare. `SAFE` would
   not (its 67.5 µs row is longer than a 47.3 µs scanned line).
5. **Frame rate: `FrameRate::GAME_BOY`** = FRCTRL2 RTNA 0Eh, PORCTRL FPA =
   BPA = 11h, a constant, set once at init. On this board it gives a TE
   period of 16,735 to 16,749 µs (four runs, 120 periods each) against the
   target 16,742.7 µs (the reset setting: 16,811 to 16,823 µs, 59.45 Hz).
   A constant rather than a calibration at every boot: the error left
   (under 0.04%) is smaller than the panel oscillator's own run-to-run
   drift (0.08%), a calibration costs about a second of TE measurement at
   boot, and one porch line is a 0.28% step anyway.
   `FrameRate::closest` (the search the probe runs) stays in the lib for
   another panel: if a board's measured TE period is off by more than
   half a porch step (0.14%), recompute the constant.

## Consequences
- The CPU's share of the display is the conversion (178k to 183k cycles,
  about 45% of M3's 402k budget, measured with the code in flash) plus
  about 1 µs to queue the transfer. Nothing else per frame.
- The panel and the emulation still run on two clocks that differ by up
  to about 0.1% (the residual error plus drift): roughly one dropped or
  repeated frame every 15 to 40 s if the core is paced by anything other
  than TE. Which clock paces the emulation (TE, audio or the core's own
  timer) is decided with audio in M4/M5, not here.
- Every image that uses `LcdDma` must define the EXTI15_10 vector and call
  `lcd_dma::on_te` (the lib's DefaultHandler would otherwise report it
  and stop); images that do not use it are unchanged.
- While a transfer is queued or running nothing else may use the FMC;
  `LcdDma` owns the `Lcd` and hands it out (`lcd`, `parts`) only when
  idle. A readback check (RAMRD) therefore has to run between transfers,
  as M3's acceptance self-test does.
- The write-ahead argument assumes the panel scans memory row 0 first
  (MADCTL ML = 0) and that the window stays in the native portrait
  orientation (MV = 0). A rotation through MADCTL would invalidate it.
- Measured with `lcd-dma-probe` (60 frames of a changing pattern, each
  read back: 0 mismatching pixels in every run; the control with one
  changed word: exactly 2; with the MPU region removed: 474,387).

## Amendment (2026-11-26, M4 review P-7): the newest frame at each TE

The front end's frame policy, not the transfer scheme, is what this adds;
the one-buffer TE-started transfer above is unchanged.

**Problem.** The `paced` image's sink dropped a completed frame whenever
the previous transfer had not finished, also when it was only queued
(waiting for TE, MDMA not yet reading the buffer). Frames come from the
audio-paced loop at the DMA-half grid (every 15.6 or 23.4 ms), so about
27% were dropped and the panel showed about 44 frames a second, although
a new frame existed for nearly every TE.

**Decision.** Show the newest completed frame at each TE:
- *Pending* (queued, TE not come): `LcdDma::retract` takes it back with a
  compare-exchange PENDING→IDLE that races only the TE interrupt's
  PENDING→RUNNING (one wins; no SWRQ was given, so disabling the channel
  stops nothing in flight), and the new frame is converted into the same
  buffer and queued. A TE during that conversion (0.41 ms) finds nothing
  queued and the frame goes out one TE later.
- *Running* (MDMA reading the buffer): the core's frame is copied aside
  (46,080 B in AXI SRAM, about 43k cycles), because the core overwrites
  its own frame from the next frame on; the idle hook converts and queues
  the copy as soon as the transfer finishes (8.1 ms after TE; the loop
  wakes at least every DMA half, 7.8 ms, so it is queued before the next
  TE), unless a newer frame comes first.
- A second 128 KiB output buffer (convert while the other is read) was
  not chosen: it needs a 256 KiB MPU region and a channel reprogram at
  the swap for the same result; the copy costs 0.5% of a frame and only
  on the frames that wait.

Frames are then dropped only when two complete within one TE period.
Measured figures: docs/perf.md "Paced loop (M4)", after P-3/P-4/P-7.
