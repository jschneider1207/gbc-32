# Game Boy Color 32

A Game Boy Color emulator written in Rust, running on an STM32H7 microcontroller, and ultimately shipping on a custom handheld PCB.

## Goal

Build a self-contained, playable Game Boy Color emulator on bare-metal STM32H7 hardware. The emulator core is written once, in portable `no_std` Rust, and runs unchanged on both a desktop development harness and the microcontroller. The project ends with a purpose-built board rather than a dev-kit prototype.

**Target hardware (development):**

- NUCLEO-H753ZI (STM32H753ZI, Cortex-M7)
- X-NUCLEO-GFX02Z1 display shield (2.2" QVGA TFT over 8-bit parallel/FMC, 64 Mbit QSPI NOR flash, joystick)

## Approach

The project proceeds in four stages. Each stage should be demonstrably working before the next begins.

### 1. Portable emulator core + desktop layer

Build the emulator as a platform-independent Rust library and prove it on a PC.

- Emulator core: CPU, PPU, APU, timers, DMA/HDMA, interrupts, and cartridge mappers
- `no_std`, no heap, so the same code can move to the microcontroller untouched
- A basic desktop front end for video, audio, and input that builds and runs on Windows and/or WSL2, used to verify correct behavior (prototyping only; not maintained past stage 1)
- Validate against community test ROMs before moving on

**Accuracy target:** high accuracy, but performance comes first: the goal is a fast, modern GBC, not a reference emulator. The core uses lazy "catch-up" timing, so each subsystem only runs when something reads or writes it:

- **CPU, timers, interrupts:** timed per memory access (M-cycle accurate). This is cheap and fixes most compatibility problems, so it is not compromised.
- **PPU:** draws a scanline at a time, but catches up to the current dot before any mid-line register write is applied. Mode timing (STAT interrupts, VRAM/OAM access windows) is exact.
- **APU:** generates samples in batches at register writes and audio buffer boundaries, not per cycle.
- **OAM DMA / HDMA:** timed exactly in CPU cycles.

**Test ROMs:** the [game-boy-test-roms](https://github.com/c-sp/game-boy-test-roms) collection (pinned release), fetched into `test-roms/` with `scripts/fetch-test-roms.sh`. Where a suite has tests for several hardware models, only the CGB-compatible ones count. The exact list, run times and pass/fail checks are defined in [`acceptance/stage1.toml`](acceptance/stage1.toml), which encodes each suite's published pass/fail rules; the table below is a summary.

| Suite | Covers | Requirement |
|---|---|---|
| Blargg `cpu_instrs`, `instr_timing`, `mem_timing`, `halt_bug` | CPU correctness and timing | Must pass |
| cgb-acid2, dmg-acid2 | PPU rendering | Must pass |
| Mooneye Test Suite (acceptance, MBC) | Timers, interrupts, DMA, MBC1/2/5 | Must pass (PPU timing subset: should pass) |
| Blargg `cgb_sound` | APU | Must pass |
| rtc3test, mbc3-tester | MBC3 real-time clock and banking | Must pass |
| SameSuite, AGE | CGB HDMA, double speed, APU edge cases | Should pass (APU subset: stretch) |
| Mealybug Tearoom, Gambatte | Mid-scanline PPU effects, broad regressions | Stretch / regression tracking |

**Real games:** Pokémon Crystal (MBC3 + RTC), Link's Awakening DX (general compatibility), Shantae and Alone in the Dark: The New Nightmare (heavy HDMA and CGB graphics), Wario Land 3 (general CGB).

**Done when:** `scripts/gate-stage1.sh` passes (all "must" ROMs, no regressions, core still builds for the MCU) and the listed games play correctly on the desktop (human sign-off).

### 2. STM32H7 platform layer

Port the core to the NUCLEO-H753ZI with the X-NUCLEO-GFX02Z1 shield.

- Display output to the shield's TFT via the FMC
- ROM storage in the shield's QSPI NOR flash
- Input, audio output, and frame pacing on the microcontroller
- Memory layout and cache tuning to hit full speed

**Done when:** games run at full speed on the dev boards with video, audio, and input working.

### 3. Prototype PCB

Hardware design lives in a separate repo, `../gbc-32-hardware`.

Design, fabricate, and test a first custom board based on what stage 2 taught us.

**MCU: STM32H735IGK6** (Cortex-M7, up to 550 MHz, 1 MB flash, 564 KB RAM, UFBGA176+25). This is a different chip from the NUCLEO-H753ZI used in stage 2. The emulator core doesn't change, but the custom board needs its own platform layer rather than a copy of the dev-board one. The board-side contract is `../gbc-32-hardware/docs/firmware-interface.md`, and the hardware rationale is in that repo's `docs/decisions/` (ADR 0001 covers the external SDRAM; the MCU choice itself has no hardware ADR yet). Differences from the dev board:

- **Reference manual:** RM0468 (STM32H723/733/725/735/730) instead of RM0433. Peripherals are largely shared, but register-level details differ, as do the device crate and HAL selection.
- **Power and clocks:** the core is supplied from the on-chip SMPS, where the Nucleo uses the LDO, so the supply configuration at boot is different. 520 MHz runs at VOS0; 550 MHz also needs `CPU_FREQ_BOOST` (SYSCFG_UR18), which turns off ECC on ITCM/DTCM. The AXI/AHB maximum is 275 MHz (DS13312 Table 13).
- **Memory map:** 564 KB of internal RAM split differently: 128 KB DTCM, 64 KB ITCM, 192 KB that can be shared between ITCM and AXI, and further AXI/AHB SRAM. The L1 caches are larger (32 KB I + 32 KB D). Linker scripts and the memory placement tuned in stage 2 need redoing, so treat the stage 2 cache tuning as a starting point.
- **ROM location:** the active game is copied from eMMC into 32 MiB of external FMC SDRAM at load time, and the core gets a `&'static [u8]` over it, where stage 2 used memory-mapped QSPI NOR. This needs:
  - FMC SDRAM initialisation;
  - an MPU region marking the SDRAM as normal, cacheable memory;
  - unaligned access disabled for code that reads the FMC region, because unaligned reads there are unsupported (erratum ES0491 §2.7.3). The compiler can merge byte reads into unaligned halfword reads, so the build must guarantee that no such read is ever emitted. Rust has no stable compiler flag for this (`-C target-feature=+strict-align` is unstable and slated to become an error, rust-lang/rust#162235), so the firmware enforces it in code: all ROM reads go through byte reads the compiler can't merge, and a disassembly check backs that up (see `docs/plan/stage-1.md`).

  The core's ROM interface is unchanged.
- **New drivers that stage 2 doesn't have:** SDMMC for eMMC (ROM library and saves) and USB upload of ROMs. The display and audio parts are also new: neither the panel, its interface (FMC parallel vs LTDC) nor the audio DAC/amplifier is the same as on the dev setup. The dev-board display is only a stand-in.

Stage 2 on the NUCLEO-H753ZI still applies: it proves the core runs at full speed on a Cortex-M7, and its QSPI NOR ROM reads are a conservative proxy for SDRAM.

- Select the display and supporting hardware
- eMMC storage for loading ROMs and storing save files
- Dedicated audio DAC/amplifier driving onboard speakers
- Schematic, layout, and first fabrication run
- Bring-up and testing of each subsystem

**Done when:** the prototype board runs the emulator on its own hardware.

### 4. Refined PCB

Iterate on the prototype to fix what testing uncovered and polish the design.

- Correct issues found during prototype bring-up
- Refine the layout, power design, and form factor
- Produce a board that is ready for regular use

**Done when:** the refined board is a reliable, finished handheld.


