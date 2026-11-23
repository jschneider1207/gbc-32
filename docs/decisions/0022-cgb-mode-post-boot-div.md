# 0022: Mode-dependent post-boot DIV counter (0x1EA0 in CGB mode)

- Status: Accepted
- Date: 2026-11-22

## Context
No boot ROM runs; `Timer::post_boot` sets the system counter as the CGB
boot ROM leaves it at PC = 0100. The value, 0x2678 (DIV 26, 34 M-cycles
before 27), was fitted to Mooneye `misc/boot_div-cgbABCDE`, which is a
DMG-mode cartridge (header 0143 = 00). Its comment said CGB mode was
unverified. The CGB boot ROM runs a different length for the two modes
(Pan Docs: Power Up Sequence), and the APU's DIV-APU phase at an NR52
power on depends on this counter (ADR 0021, DIV-APU).

## Decision
`Timer::post_boot(cgb_mode)`: DMG compatibility mode keeps 0x2678; CGB
mode (header 0143 bit 7) uses 0x1EA0 (`POST_BOOT_COUNTER_CGB`, DIV 1E,
24 M-cycles before 1F). Evidence, all from CGB-mode cartridges:

- Gambatte `div/start_inc_1_cgb04c_out1E` / `start_inc_2_cgb04c_out1F`
  (header C0, results from a CGB04C): LDH A,(04) after JP 0150, JP 1000
  and 13 or 14 NOPs reads 1E at M-cycle offset 23 and 1F at offset 24
  (offsets counted as for the Mooneye value), so the counter is
  0x1F00 - 24 * 4. Both failed with 0x2678 and pass with 0x1EA0.
- Gambatte's CGB initial state uses the same value (read for the fact,
  not copied; Gambatte is GPL).
- Further CGB-mode results that start passing (and no regressions): the
  gambatte `sound/ch2_*nr52*` length/late-reset tests and other
  `sound/*_counter_timing_*` ones, `tima/tc00_start_2_cgb04c_outF1`, and
  SameBoy's own same-suite `apu/div_trigger_volume_10`,
  `div_write_trigger_10`, `div_write_trigger_volume_10` (header 80).

Nothing in the manifest checks the DMG-mode value against anything but
`boot_div-cgbABCDE`, which still passes, so it is unchanged.

## Consequences
Unit tests that pinned numbers on 0x2678 with a CGB-mode cartridge are
re-derived from 0x1EA0 (the STOP/TIMA speed-switch test, the idle-skip
DMA test, the timer helpers in `lib.rs`). Other CGB-mode post-boot
phases (PPU, BCPS/OCPS) are still fitted to DMG-mode ROMs: plan task.
