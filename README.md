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

Design, fabricate, and test a first custom board based on what stage 2 taught us.

- Select the MCU, display, and supporting hardware
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


