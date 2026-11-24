# Game Boy Color 32

A Game Boy Color emulator written in Rust, running on an STM32H7 microcontroller, and ultimately shipping on a custom handheld PCB.

## Goal

Build a self-contained, playable Game Boy Color emulator on bare-metal STM32H7 hardware. The emulator core is written once, in portable `no_std` Rust, and runs unchanged on both a desktop development harness and the microcontroller. The project ends with a purpose-built board rather than a dev-kit prototype.

**Target hardware (development):**

- NUCLEO-H753ZI (STM32H753ZI, Cortex-M7)
- X-NUCLEO-GFX02Z1 display shield (2.2" QVGA TFT over 8-bit parallel/FMC, 64 Mbit QSPI NOR flash, joystick)

## Quality and performance bar

This is meant to be a fast, carefully engineered emulator built with current (2026) Rust practice, not just one that works.

- **Performance target:** in the worst-case games (CGB double speed, heavy HDMA), the core alone uses **at most 50%** of an STM32H753 at 480 MHz, measured as CPU cycles per emulated frame in Stage 2. "Worst case" means realistic worst cases (everything a demanding game does at once, including one mid-line register write per line); programs that hammer PPU registers many times per line are outside the bound (human, 2026-11-23; `docs/perf.md` "Outside the bound"). The rest is headroom for display, audio, input, and running at a lower clock to save battery on the handheld.
- **Tracked continuously, not just at the end:** a deterministic benchmark (instruction counts under valgrind) runs in the gate from Stage 1 M2 on, so a slowdown of more than a few percent fails like any other regression. Code size is checked against the MCU build: the whole core must fit the final MCU's ITCM, and per-function sizes are reported (human, 2026-11-24; `docs/perf.md` "Code-size budgets"). Every performance claim cites a measurement.
- **Engineering standard:** zero-cost abstractions, compile-time tables, no hidden panics or bounds checks in hot paths (verified in the generated assembly), no heap, no floats in the core, and a deterministic core.
- **Robustness:** every untrusted input (ROM headers, save files, RTC state) is fuzzed, and the gate runs a short fuzz pass. Any `unsafe` code outside the core is confined, commented with its safety argument, and tested under Miri where it can run on a host.

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

**Done when:** `scripts/gate-stage1.sh` passes (all "must" ROMs, no regressions, core still builds for the MCU), the listed games play correctly on the desktop (human sign-off), and **every test ROM in the manifest passes, whatever its tier**. The tiers set the order of work and what gates each milestone, not what the finished core may fail. The only exceptions are failures the human signs off explicitly, per group, with a recorded reason: the references contradict each other (both outcomes can't pass), or the test doesn't apply to the CGB revision targeted (ADR 0003). (Human decision, 2026-11-22.)

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

**MCU: STM32H735IGK6** (Cortex-M7, up to 550 MHz, 1 MB flash, 564 KB RAM, UFBGA176+25). This is a different chip from the NUCLEO-H753ZI used in stage 2. The emulator core doesn't change, but the custom board needs its own platform layer rather than a copy of the dev-board one. The board-side contract is `../gbc-32-hardware/docs/firmware-interface.md`, and the hardware rationale is in that repo's `docs/decisions/` (ADR 0001 covers the external ROM memory and still describes FMC SDRAM, see "ROM location" below; ADR 0002 covers the MCU choice). Differences from the dev board:

- **Reference manual:** RM0468 (STM32H723/733/725/735/730) instead of RM0433. Peripherals are largely shared, but register-level details differ, as do the device crate and HAL selection.
- **Power and clocks:** the core is supplied from the on-chip SMPS, where the Nucleo uses the LDO, so the supply configuration at boot is different. 520 MHz runs at VOS0; 550 MHz also needs `CPU_FREQ_BOOST` (SYSCFG_UR18), which turns off ECC on ITCM/DTCM. The AXI/AHB maximum is 275 MHz (DS13312 Table 13).
- **Memory map:** 564 KB of internal RAM split differently: 128 KB DTCM, 64 KB ITCM, 192 KB that can be shared between ITCM and AXI in 64 KB steps, and further AXI/AHB SRAM (RM0468 §2.4). The L1 caches are larger (32 KB I + 32 KB D). The plan is 128 KB of ITCM (option byte `TCM_AXI_SHARED` = 01, leaving 256 KB of AXI SRAM; RM0468 Table 8), which holds all of the core, so there is no hot set to choose; the code-size gate is sized for it (`docs/perf.md` "ITCM placement plan"). Linker scripts and the memory placement tuned in stage 2 need redoing, so treat the stage 2 cache tuning as a starting point.
- **ROM location:** the active game is copied from eMMC into external HyperRAM on OCTOSPI (human decision 2026-11-24; hardware ADR 0001 pending update, it still says FMC SDRAM) at load time, and the core gets a `&'static [u8]` over the memory-mapped window, where stage 2 used memory-mapped QSPI NOR. This needs:
  - OCTOSPI HyperBus initialisation in memory-mapped mode (the errata and settings are in `docs/reference/stage3-rom-memory-options.md`);
  - an MPU region marking the HyperRAM window as normal, cacheable memory, sized exactly to the device;
  - memory that supports unaligned reads: the core reads the ROM with loads of any width at any address. HyperRAM on OCTOSPI qualifies (ES0491 §2.8.4 Table 5 allows memory-mapped reads at any address and byte count in HyperBus mode). FMC SDRAM would not (erratum ES0491 §2.7.3), and Rust has no stable flag to forbid unaligned loads (`-C target-feature=+strict-align` is unstable and slated to become an error, rust-lang/rust#162235); the byte-wide ROM reads that covered that case were retired with ADR 0027 on 2026-11-24.

  The core's ROM interface is unchanged.
- **New drivers that stage 2 doesn't have:** SDMMC for eMMC (ROM library and saves) and USB upload of ROMs. The display and audio parts are also new: neither the panel, its interface (FMC parallel vs LTDC) nor the audio DAC/amplifier is the same as on the dev setup. The dev-board display is only a stand-in.
- **Launcher firmware:** the device needs top-level firmware around the core. It boots into a native menu (not a Game Boy ROM) that lists the games on eMMC, loads the chosen ROM into HyperRAM and its save into cart RAM, runs the emulator, and flushes saves when you return to the menu. This is platform code, and the core doesn't change: the caller already owns the ROM, RAM and memory buffers. Stage 2 can make do with a single ROM in QSPI flash.

Stage 2 on the NUCLEO-H753ZI still applies: it proves the core runs at full speed on a Cortex-M7, and its QSPI NOR ROM reads are a conservative proxy for HyperRAM.

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

## License

MIT. See [LICENSE](LICENSE). Test ROMs are downloaded separately and remain under their authors' licenses. No Nintendo code or boot ROMs are included.

"Game Boy" and "Game Boy Color" are trademarks of Nintendo. This project is not affiliated with or endorsed by Nintendo.
