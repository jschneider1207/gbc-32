# gbc-nucleo-h753

Stage 2 firmware for the NUCLEO-H753ZI plus shield. Images are built with
`./build.sh` (`./build.sh <image>` for one); `flash.sh` and the `run-*.sh`
scripts drive them. Gate-relevant images are `headless*`, `paced`, `player`,
`loader` and `link-probe`.

## Probe bins

Bring-up images from Stage 2 M2-M5. Each one prints a UART report; the
reference doc named below holds the results and the procedure. None is
deleted: all are built by `build.sh` (so they keep compiling) and are the
documented way to re-check a part of the board. "Stage 3" says whether the
custom PCB bring-up would reuse it.

| Bin | What it is for | Used by | Stage 3 |
| --- | --- | --- | --- |
| `qspi-probe` | Read-only first contact with the QSPI NOR: JEDEC ID, REMS, SFDP, status (`docs/reference/qspi-nor.md`) | bring-up procedure | Yes, first check of any new NOR part |
| `qspi-mmap` | Memory-mapped read mode, timing and clock sweep | `run-loader-roundtrip.sh`; `docs/reference/loader.md` | Yes, and its mapped-read numbers are the HyperRAM stand-in |
| `qspi-write` | NOR erase/program driver on the last 64 KiB block only | bring-up procedure | Only if the driver is reused; keep until then |
| `lcd-probe` | First contact with the panel controller over FMC: ID, init, TE (`docs/reference/lcd-panel.md`) | bring-up procedure | Yes, if the panel or controller changes |
| `lcd-dma-probe` | MDMA frame path to the panel and TE-period measurement (ADR 0039) | bring-up procedure | Maybe; the frame path moves with the bus |
| `dac-probe` | DAC + TIM6 + DMA audio output checked by registers and timing (`docs/reference/audio-dac.md`, ADR 0044) | bring-up procedure | Maybe; Stage 3 uses SAI, so the structure of the check carries over |
| `link-probe` | Serial link against a running emulator (M5) | `gate-stage2.sh`, `check-stage2-sets.sh`, `run-player-save.sh`, `run-save-roundtrip.sh`, `tools/gbc-pc` tests | Yes |
