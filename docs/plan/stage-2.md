# Stage 2: STM32H7 platform layer (NUCLEO-H753ZI + X-NUCLEO-GFX02Z1)

> **Status: DRAFT, revised 2026-11-25 with the human's decisions** (see
> "Decisions" below: D1-D7, D9 and D10 are accepted or decided; D8 is a
> note). Scope is the human's. Nothing here has started.

**Stage goal** (`README.md` §2): the unchanged `gbc-core` runs on the
NUCLEO-H753ZI with the X-NUCLEO-GFX02Z1 shield. Display goes to the shield
TFT over the FMC, the ROM is read memory-mapped from the shield's QSPI NOR,
and there is input, audio out and frame pacing. Memory layout and caches are
tuned until games run at full speed. The 50% performance target
(`README.md`) belongs to the final MCU (STM32H735 at 550 MHz). The H753 is
lower spec, so on this board `synth:worst-case` (the realistic worst case,
`docs/perf.md`) is **measured and reported against 50% of 480 MHz
(4,018,249 CPU cycles per frame = 480 MHz / 59.7275 Hz / 2), not gated**
(human, 2026-11-25). What is gated is "games run at full speed": every
sign-off workload finishes each frame inside real time (frame time
< 1/59.7275 s, i.e. under 8,036,499 cycles at 480 MHz, whole loop
included), plus the on-board cycle baseline's +3% regression ratchet.

**Stage gate:** `./scripts/gate-stage2.sh` exits 0, with the board attached
for its on-board steps (proposed below; decision D1). M7 (the in-depth
review) must be closed, **and** the human signs off on the short hardware
checklist at the bottom of this file.

Milestone rules are as in Stage 1. A milestone closes when its acceptance
passes and its review (AGENTS.md "Independent review") has no confirmed
high or medium finding open. Then update the baselines, commit, and tag
`stage2-m<N>`. Tasks are a starting point. Acceptance criteria are fixed
unless the human changes them.

**Stepping stone.** Stage 3 is a different chip (STM32H735, SMPS, 128 KiB
ITCM, HyperRAM on OCTOSPI, new display, SAI audio, eMMC, USB). Every
milestone below says what carries over. Dev-board-only drivers get the
minimum that meets the stage goal, with no polish.

---

## Board facts this plan relies on

Sources are local (`../gbc-32-hardware/datasheets/`). "Unverified" means not
confirmed from a document or a measurement yet. Each such fact has a task
that checks it.

| Item | Fact | Source |
|---|---|---|
| MCU | STM32H753ZIT6 rev V (`idcode=0x20036450`), **single core**, so there is no Cortex-M4 to use | h7-bench README; DS12117 |
| 480 MHz | VOS0 needs the LDO (this board has it) and TJ ≤ 105 °C. Rev V only (ES0392 §2.2.21). VOS0 is entered from VOS1 by setting SYSCFG ODEN | DS12117 Table 112 and notes 4-5 |
| Flash at 240 MHz AXI | 4 wait states, WRHIGHFREQ = 10. A miss costs 5 AXI clocks = 10 CPU cycles, against 6 at 392 MHz | RM0433 Table 17 |
| HSE | 8 MHz from the ST-LINK MCO by default (SB45 ON). Enough for an exact 480 MHz (8/2 x 240 / 2). HSI tolerance at 480 MHz is unverified, so the plan uses HSE | UM2407 §7.5.1 |
| TCM/SRAM | ITCM 64 KiB, DTCM 128 KiB (neither reachable by DMA1/2; MDMA reaches both), AXI SRAM 512 KiB, D2 SRAM1-3, D3 SRAM4 | RM0433 Table 7; perf.md |
| LCD | ILI9341V, 240x320 panel (native portrait), 8080-I 8-bit bus: D0-D7 = PD14 PD15 PD0 PD1 PE7 PE8 PE9 PE10, RD = PD4, WR = PD5, CS = PD7 (FMC_NE1), D/CX = PE4, TE = PE13, RESET = PE11. The backlight anode is tied to 3V3 and the cathodes don't go to the MCU, so there is no backlight control | UM2905 Table 2 |
| LCD address map | Command register at 0x6000_0000. Data at 0x6010_0000 if PE4 is FMC_A20 (8-bit bank: HADDR = A). **Unverified**: the PE4 AF and the bank mapping | derived; check RM0433 FMC and DS12117 AF table |
| LCD bus speed | twc ≥ 66 ns per byte (twrl, twrh ≥ 15 ns), which is ≤ 15.15 MB/s. RGB565 is 2 B/px: 160x144 takes 3.04 ms, 240x216 (1.5x) 6.84 ms and 320x240 10.14 ms, against a 16.74 ms frame. Reaching 66 ns through the shield connectors is unverified | ILI9341V §18.3.1 |
| LCD refresh | FRMCTR1 sets 61-119 Hz at DIVA = 00 (default 70 Hz). DIVA = 01 halves it, so about 59.5 Hz should be possible. **Unverified** (fosc tolerance) | ILI9341V §8.3.2 |
| QSPI NOR | 64 Mbit = 8 MiB. NCS = PG6, CLK = PB2, IO0-3 = PD11 PD12 PE2 PD13. PE2 is also routed to Zio CN9 (SAI_A_MCLK), "only one function at a time". **The part number is not in UM2905**, so its command set, DTR/QPI support and maximum clock are unverified (read JEDEC ID 0x9F in M2) | UM2905 Table 3; UM2407 CN9 note |
| QUADSPI | ≤ 133 MHz SDR at VDD ≥ 2.7 V. Erratum: the first nibble is lost in indirect write with dummy cycles (§2.7.1); a hang if CCR is cleared (§2.7.3) | DS12117 Table 73; ES0392 §2.7 |
| Joystick | 5-way, active low, common to GND: LEFT PE3, CENTER PA0, DOWN PF8, UP PF9, RIGHT PF7. Plus user button B1 on PC13. **That is 6 inputs for 8 Game Boy buttons, so the joystick is the D-pad and the rest comes from the PC over the VCP (D2)** | UM2905 Table 4; UM2407 §7.6.2 |
| Audio | No codec on either board. DAC1 OUT1/OUT2 on PA4/PA5 (free of the shield). Zio's SAI1 pins clash with the shield (PE2 QSPI, PE4 D/CX, PE3/PF7/PF8/PF9 joystick). I2S2 on PB12/PB13/PB15/PC6 is free (PB13 is Ethernet TXD1 only with JP6 ON) | UM2407 Zio tables; UM2905 |
| Free pins with the shield fitted | ST morpho CN11/CN12 carry every GPIO. **Unverified** that they stay reachable with the shield plugged in | UM2407 |
| Host link | VCP on USART3 PD8/PD9 through the STLINK-V3E. Reached from WSL only through PowerShell .NET `SerialPort` (`read-serial.sh`). Baud rates above 115200 are unverified | h7-bench README; UM2407 §7.6.5 |
| FMC erratum | Unaligned **reads** from the FMC are unsupported (ES0392 §2.6.4). This only matters for LCD readback, which uses byte reads | ES0392 |

## Crate layout (agent decision, ADR in M1)

- `crates/gbc-platform/`: `no_std`, `#![forbid(unsafe_code)]`, no
  dependencies, a root-workspace member, host-tested. It holds
  chip-agnostic front-end logic: the RGB555 to panel-format
  converter/scaler, the audio DC blocker and sample ring, audio-driven
  frame pacing, button debounce and input mapping, the ROM image header and
  hash, the serial protocol (ROM load, save load/dump, PC input), the player
  loop, and the per-frame statistics. **All of this carries over to
  Stage 3.**
- `crates/gbc-nucleo-h753/`: firmware, a bin crate and a separate workspace
  like `tools/h7-bench` (thumbv7em only, its own profile and rustflags).
  Everything chip- or board-specific lives here: PAC (`stm32h7`,
  `cortex-m`, `cortex-m-rt`, as h7-bench uses; no HAL, no async runtime),
  `memory.x`, clocks, power, MPU, FMC/ILI9341, QUADSPI, DAC, GPIO and UART.
  Dependency rule: see decision D9.
- The Stage 3 handheld gets its own crate later (`crates/gbc-handheld/`,
  provisional name). Shared H7-specific code is extracted into a common
  crate only once Stage 3 actually duplicates it.
- All Cortex-M7 parts use the same Rust target (`thumbv7em-none-eabihf`).
  What differs per chip is the PAC, the memory map and the peripherals
  (e.g. QSPI on the H753 vs OCTOSPI on the H735), which is why the split is
  per board and there is no generic "stm32" crate.
- `tools/h7-bench` stays the cycle-measurement tool. In M1 its board
  bring-up code (power, clock, UART, fault handler, TCM copy) moves into
  `gbc-nucleo-h753`, so the two can't drift. h7-bench then depends on it.

---

## M0: 480 MHz measurement (h7-bench only)

Answers the question everything else depends on before any new driver
exists: where does the worst case land against 50% at 480 MHz (reported, not
gated)?

- [ ] `pll480` clock in h7-bench: HSE bypass 8 MHz (ST-LINK MCO) → PLL1 480 MHz; VOS1 → ODEN → wait VOSRDY → VOS0 (RM0433 PWR/SYSCFG sequence: cite section in code); flash 4 WS / WRHIGHFREQ=10 before the switch; AXI/AHB 240, APB 120. Banner prints `d3cr`, `SYSCFG_PWRCR`, `flash_acr`, `cpu_hz=480000000`
- [ ] `selftest-480` on the board: constant `cycles=`, UART at 115200 (it bounds the clock error), `tcmprobe` equal
- [ ] All three placements at 480 MHz: hashes equal `expected-hashes.txt`; record mean/max and % of an 8,036,499-cycle frame in the h7-bench README and `docs/perf.md` (replacing the scaled figures)
- [ ] Add the bench workloads the board does not run yet: `dma-storm`, `halt-irq`, `apu-writes`, `oam-lines`, and the five `synth:storm-*` (the Stage 1 M8 task "measure the dense mid-mode-3 programs on the M7"; outside the bound, recorded for context). Regenerate `expected-hashes.txt` for the new lines only (old lines unchanged, say so in the commit)
- [ ] Re-baseline the event-density question (Stage 1 M8 "worst-case event density on the M7") with the 480 MHz `event-storm` figure; close it or open B4 below
- [ ] Update `docs/perf.md` "Measured on a Cortex-M7" (still titled H755) and `docs/architecture.md` workspace line (h7-bench "on a NUCLEO-H755ZI-Q")

**Acceptance:** `bench-itcm-dtcm-pll480` on the board prints `end pass=1`,
hashes equal to `expected-hashes.txt` for every workload, and its cycle
figures are committed. The 50% bar is **measured and reported** here, not
gated (the H753 is lower spec than the final H735; human, 2026-11-25). If
`synth:worst-case` max > 4,018,249, say so in STATUS and the log. It does
not block anything. If it exceeds a full real-time frame (8,036,499),
the human hears about it at once (STATUS "Needs human") and M6 moves ahead
of M3.

## M1: Firmware skeleton, link-time placement, host-side gate

- [ ] ADR: crate layout as above (per-board firmware crates plus `gbc-platform`); PAC-only; one `memory.x` generated by `build.rs` from `perf/itcm-sections.txt` (ADR 0026 mechanism); board module boundary
- [ ] `gbc-nucleo-h753` boots at 480 MHz (shared bring-up), MPU set up (see M2/M3 for the regions), I/D-cache on (write-back; ES0392 §2.1.1 rules out write-through), fault handler on its own stack with UART report (h7-bench pattern)
- [ ] `headless` image: core in the M6-target placement (ITCM set + DTCM data), a ROM from internal flash, a scripted input list, prints one hash per frame plus an audio-stream hash over UART; host counterpart in `gbc-platform`/`tools/` prints the same lines for the same ROM + inputs
- [ ] `scripts/check-itcm-elf.sh` (the Stage 1 M8 "post-link check", left for Stage 2): every demangled name in `perf/itcm-sections.txt` that the link kept is in `[0, 0x1_0000)`; dropped tiers listed; `.itcm` size vs 64 KiB; veneer count. Mutation-check: a glob moved to flash fails, a renamed function fails
- [ ] Settle the provisional size gate on the firmware link (Stage 1 C-P5): measure gbc-core's share of the linked ELF (compiler_builtins, panic code, padding) and report against the H735 total; **changing the 122,880 B figure needs the human**
- [ ] Name or glob the ~1.7 KB anonymous `.rodata` the `.dtcm_rodata` rules miss (Stage 1 C-P7)
- [ ] QEMU (`mps2-an500`) smoke of the `headless` frame loop: same hash lines as the host (h7-bench's `qemu` mode, extended)
- [ ] `scripts/gate-stage2.sh` host part (below), and fmt/clippy/build for `tools/h7-bench`, `tools/h7-bench/host` and `gbc-nucleo-h753` (Stage 1 C-T6, stricter)
- [ ] Board lock: one `flock` file (e.g. `target/board.lock`) taken by every flash/serial script, so two agents never drive the board at once

**Acceptance:** `gate-stage2.sh --host-only` passes. On the board, the
`headless` image's frame-hash and audio-hash lines for `cpu_instrs` and
`cgb-acid2` (internal-flash ROMs, 600 frames each) equal the host's byte
for byte.

## M2: ROM in QSPI NOR (memory-mapped)

- [ ] QUADSPI driver: JEDEC ID read (record the part, its max clock, quad-enable bit, DTR support in `docs/reference/`), sector erase, page program (no dummy cycles in indirect write: ES0392 §2.7.1), status polling; memory-mapped quad read (`0xEB` or the part's equivalent), FSIZE for 8 MiB, prefetch/timeout settings
- [ ] MPU: QSPI window 0x9000_0000 as Normal, cacheable, sized exactly to the device; the rest of the 256 MB QSPI region no-access, so speculative M7 fetches cannot reach unmapped space (standard ST guidance for the M7; cite the app note once checked: unverified)
- [ ] On-board test: unaligned u16/u32 loads at odd addresses from the QSPI window return the right bytes (the core relies on this, `docs/architecture.md` "ROM")
- [ ] ROM image format in `gbc-platform`: header (magic, length, SHA-256 or 64-bit hash, title) at a fixed QSPI offset, ROM data 256-byte aligned; the firmware refuses a bad hash
- [ ] Loader (decision D5): firmware `loader` mode receives a ROM over the VCP, programs it, verifies the hash, reports; host side is a script around PowerShell `SerialPort` writes (as `read-serial.sh`) plus an Elixir/bash framing helper; skip when the stored hash already matches. The frame format in `gbc-platform` has room for an optional save section sent with the ROM (used in M5) and for later PC-to-board and board-to-PC frames (input, save dump). Measure the VCP's highest reliable baud rate (unverified) and record it
- [ ] Bench placement `itcm-dtcm-qspi`: the bench workloads with their ROMs in QSPI instead of internal flash; record cycles vs `itcm-dtcm` at 480 MHz (the "QSPI as a conservative proxy for HyperRAM" figure Stage 3 needs)

**Acceptance:** a load → verify round trip passes for a 4 MiB test image
(hash equal, read back through the memory-mapped window). The `headless`
image running `cpu_instrs` and `cgb-acid2` **from QSPI** prints the same
hash lines as the host. The `itcm-dtcm-qspi` figures are committed. The
`synth:worst-case` figure from QSPI is reported against the 50% bar (not
gated). If it exceeds a full real-time frame, the M6 ROM-placement task
becomes required.

Carries over: the MPU/memory-mapped lessons and the measured miss cost
(OCTOSPI HyperRAM is memory-mapped the same way), the image header, and
the loader protocol framing. The NOR program/erase code does not carry
over (Stage 3 loads from eMMC).

## M3: Display (FMC → ILI9341V)

- [ ] FMC bank 1 as SRAM/8-bit/write-enabled, asynchronous mode A; ADDSET/DATAST from twc 66 ns, twrl/twrh 15 ns at a 240 MHz HCLK (and a slower fallback); MPU region for the LCD as Device (no speculative reads, no write merging) or FMC BMAP remap; GPIO AF12 on the pins above (check each AF against DS12117)
- [ ] ILI9341V init (reset timing §15.4, sleep out, COLMOD 16-bit, MADCTL for the chosen orientation; the BGR bit lets the core's R-low RGB555 order go out without a channel swap), window set (CASET/PASET), RAMWR
- [ ] Converter/scaler in `gbc-platform` (decision D3: 1.5x to 240x216 recommended): RGB555 → byte-swapped RGB565, nearest-neighbour 2→3; host unit tests against a naive reference over random frames; cycles per frame measured on the board
- [ ] `Frame` in DTCM (Stage 1 B10/C5): after `FRAME_READY` the CPU converts it into an AXI SRAM output buffer (MPU non-cacheable, so the 100 KB of writes do not evict the ROM's D-cache lines), then MDMA streams the buffer to the FMC data address; no double-buffered `Frame` is needed (the core is not running while the conversion runs)
- [ ] Tearing: TE (PE13) via EXTI; start the transfer on TE with rows written in the panel's native scan order (portrait 240-wide rows, so the write front stays ahead of the scan: 31.7 µs per 1.5x row vs ~44.6 µs per scanned row at 70 Hz); try FRMCTR1 near 59.7 Hz to remove 70-vs-59.73 Hz judder. ADR with what was measured
- [ ] Readback self-test: after a frame, read a GRAM window back (RAMRD, byte reads, 18-bit format per ILI9341V) and compare with what was sent; mismatch count over UART
- [ ] Measure: conversion cycles, DMA time per frame, and the core's cycles per frame with the display running (AXI/FMC contention), against M2's figures

**Acceptance:** over 600 frames of `cgb-acid2` with the display running,
the readback self-test reports 0 mismatches. The core's frame hashes still
equal the host's. Conversion plus DMA setup is ≤ 5% of a 480 MHz frame
(about 402k cycles; the target is a proposal). Human check: one look at
cgb-acid2 and dmg-acid2 on the panel (sign-off item 1).

Carries over: the converter/scaler structure, the DTCM-frame → DMA-buffer
pattern and the TE/pacing approach. The FMC 8080 driver carries over only
if Stage 3 picks an FMC-parallel panel (still undecided in the hardware
repo).

## M4: Audio out

- [ ] Output path per decision D4 (recommended: DAC1 stereo on PA4/PA5, 12-bit, buffer on, line level): TIM6/TIM7 trigger at 32,768 Hz from the 240 MHz timer clock (÷7324 = 32,768.9 Hz, +27 ppm; agent check of the timer clock tree), DMA1 circular double buffer in AXI SRAM (DMA1 cannot reach DTCM), half/full-transfer ISR
- [ ] `gbc-platform`: DC blocker (integer one-pole high-pass, ADR 0035 leaves it to the platform), i16 → 12-bit, sample ring with underrun/overrun counters; host unit tests (step response, no overflow at ±full scale)
- [ ] Audio-driven pacing (as desktop ADR 0024): the main loop runs the core while the ring has room for a frame's samples (≤ 550), else `WFI`. Statistics: underruns, ring-fill min/max, emulated clocks vs elapsed DWT cycles
- [ ] The sample stream the DAC is fed (after DC blocking) hashes equal to the host's for the same ROM and inputs

**Acceptance:** a 10-minute run of `cgb_sound` plus `synth:worst-case`
with the display on: 0 underruns. Emulated speed is within ±0.1% of real
time (emulated clocks / 4,194,304 against DWT cycles / 480 M). The
DAC-feed hash equals the host's. Human check: listen through powered
speakers or headphones via an amp (sign-off item 2).

Carries over: the ring, the DC blocker, pacing and statistics. The DAC
driver does not carry over (Stage 3 is SAI → external DAC; with D4 option
b, an SAI/I2S driver partly would).

## M5: Input and the player loop

- [ ] Buttons per decision D2 (no buttons wired, nothing bought): the shield joystick is the D-pad (LEFT/RIGHT/UP/DOWN; CENTER is spare), B1 (PC13) may map to one button if convenient (e.g. Start), and **PC input over the serial link carries all 8 buttons**, so PC input alone can play. Debounce, mapping and the PC input frame (button-state bitmap, sent on change) live in `gbc-platform` (host-tested); the host side is a small script or tool on the Windows side that turns key state into those frames. Joystick and PC state are OR-ed, sampled once per frame and applied with `set_buttons_at` at the frame's start clock
- [ ] Scripted input mode: the firmware takes an input script (the `acceptance/*.toml` `input` schema, converted to clocks exactly as romtest does) instead of GPIO or serial, so replays are deterministic
- [ ] Saves over the serial link (decision D6; replaces the old QSPI save milestone). The PC loader sends the `.sav` (cart RAM plus the RTC footer, the same format as the desktop front end, ADR 0023) with the ROM at load time. The firmware keeps cart RAM and RTC state in RAM (AXI SRAM) and loads it into the core. The board dumps the current save back to the PC on request (a serial command) or on a button combo (joystick CENTER held with B1, or an equivalent PC input chord). The save frame format and size/format checks live in `gbc-platform` (host-tested; a malformed or wrong-size save is refused, never half-applied). No flash save area and no flush policy
- [ ] `player` image: ROM from QSPI, display, audio and input all running, per-frame statistics over UART every N s (core cycles min/mean/max/p99 per frame, front-end cycles, slack, frames over budget, underruns, display skips)
- [ ] An "input test" screen or UART echo of button state for the human's button check
- [ ] DMG-mode games: one default compatibility palette (romtest's common palette); per-title palettes are not needed on the dev board

**Acceptance:** the `player` image in scripted mode with display and
audio running gives frame-hash and DAC-feed-hash lists equal to the
host's for the Stage 2 replay set (proposed below). That shows the front
end doesn't disturb the core. A 10-minute `synth:worst-case` run has 0
frames over budget and 0 underruns. A save round-trips byte-identically,
PC → board → PC: load a `.sav` (with RTC footer) with a ROM, dump it
before and after a scripted run that doesn't touch cart RAM, and the file
equals the original byte for byte. Human check: every button, via the joystick and via PC input
(sign-off item 3).

Carries over: debounce, mapping, the serial protocol (input and save
frames), scripted replay, the statistics format and the main loop.

## M6: Memory layout and performance to full speed

Everything perf-driven lives here. Each optional task starts only when an
M0-M5 measurement calls for it, and each ends with a cycle figure on the
board. The H753 is lower spec than the final H735, so the 50% target is
**measured and reported here, not gated** (human, 2026-11-25). The gated
requirements are real-time frames and the cycle-baseline ratchet.

- [ ] ADR 0026 (Proposed): accept or replace it with the measured data placement (DTCM: stack, `Emulator`, `Memory`, tables, `Frame`; AXI: DMA buffers, cart RAM) (Stage 1 B10/C5)
- [ ] ITCM set with the front end present: the set is 68,832 B in the ELF vs 65,280 B usable (plain tile path already dropped, 8,078 B); decide what else leaves (tier order in `docs/perf.md`), measure the cost of each drop; front-end ISRs and the converter stay in flash unless a measurement says otherwise
- [ ] Measure LTO and `opt-level` variants of the firmware link (inlining changes the globs: `check-itcm-elf.sh` catches it); keep what is faster on the board
- [ ] QSPI tuning: clock (up to 133 MHz SDR if the part allows), DTR/QPI if supported, send-instruction-only-once, prefetch/timeout; measured on `itcm-dtcm-qspi`
- [ ] Optional, if the QSPI ROM costs > ~3 points on any workload: ROM placement (Stage 1 M8 "ROM placement hook": bank 0 or hot banks in internal SRAM). A core public API change needs an ADR and one reviewer; first try platform-only options (ROM ≤ 1 MiB copied to internal flash bank 2 or SRAM at load)
- [ ] Optional, if the reported worst case is close to or over the 50% figure and the human wants the gap narrowed (it is not a gate here): B4 u32 epoch-relative clocks (Stage 1 M10 B4); S4/L7 re-measure (`&mut` read path, always-lazy timer); APU cost leads (Stage 1 M8 APU task)
- [ ] Optional: E1 dot-stepped mode 3 in the core, only if the `storm-*` numbers from M0 show the closed-form per-write cost matters for real games (they are outside the bound by human decision)
- [ ] Optional: B9 dispatch-mispredict measurement. **Note:** the Cortex-M7 DWT has CYCCNT/CPICNT/LSUCNT/FOLDCNT/EXCCNT/SLEEPCNT but, as far as I know, no branch-mispredict counter (unverified), so this needs an indirect method or is dropped
- [ ] Deferred to Stage 3 unless a measurement here needs it: E5/D11 CPU fetch rework (touches `Cpu::run`; wants the real-game frame-golden net, Stage 1 M10 E6, which needs the human's recordings); C10 recursion split (only for a provable static stack bound; measured stack is 2,952 B)
- [ ] On-board cycle baseline: `tools/h7-bench/expected-cycles.txt` (per workload, per placement, 480 MHz); the gate fails at > +3% over it (cycle counts repeat exactly, so this is tight but fair). It is a regression ratchet, not an absolute bar. Absorbing a +3% failure needs the human, as for `check-bench.sh`
- [ ] Report, in `docs/perf.md` and the log: `synth:worst-case` max cycles per frame at 480 MHz with ROMs in QSPI and the shipped placement, as a percentage of the 4,018,249 figure (and the projected margin on the H735 at 550 MHz, labelled as a projection)

**Acceptance:** on the board at 480 MHz with ROMs in QSPI and the shipped
placement, `synth:worst-case` max cycles per frame is **measured and
reported** against 4,018,249 (not gated). Full speed is gated: in the
`player` image with display, audio and input running, the whole loop
fits real time (frame time < 1/59.7275 s, under 8,036,499 cycles), with 0
frames over budget in 10 minutes of `synth:worst-case`. The cycle baseline
is committed. The performance review (AGENTS.md) is required at this
milestone's close.

Carries over: the placement method and checks (on the H735 the rule
covers all of gbc-core), measured miss costs, and the LTO findings. The
ITCM tiering does not carry over (the H735 puts the whole core in ITCM).

## M7: Stage 2 review and sign-off

- [ ] Full `gate-stage2.sh` (host and board) passes
- [ ] Independent reviews in parallel (AGENTS.md): **platform correctness** (register sequences against RM0433, DS12117, ES0392, UM2407, UM2905, ILI9341V: clock/power, MPU, DMA/cache coherence, QSPI, FMC timings, ISR races), **test integrity** (the on-board comparison and gate logic, any way a missing board or a garbled serial line can pass, determinism, nothing new in the core), **performance and portability** (front-end cost per frame, ITCM/DTCM use, what will and won't move to the H735)
- [ ] Turn lows into one-line Stage 3 plan tasks; write the carry-over list into `docs/architecture.md` "Targets"
- [ ] Human sign-off checklist (bottom)

**Acceptance:** `gate-stage2.sh` exits 0 with the board attached; no
confirmed high or medium finding is open; `docs/reviews/stage2-m7.md`
records every finding's outcome; the human signs the checklist.

---

## Proposed `scripts/gate-stage2.sh`

Gate scripts are prompt-protected, and the human approves the first
version. Exit codes are 0 pass, 1 a check failed, 2 a tool or runner error
(serial unreadable, flash refused, malformed line). A missing board is
exit 1, never a pass. `--host-only` runs steps H1-H8 and always ends with
`NOT A STAGE 2 GATE PASS` and exit 3, for per-commit use.

Host steps (heavy, run alone per AGENTS.md machine rules):
- **H1.** The Stage 1 preconditions (refused RUSTFLAGS variants, unset
  overrides), plus `H7_*` overrides unset except the port and drive.
- **H2.** `scripts/gate-stage1.sh` unchanged and in full: fmt, clippy,
  rustdoc, `cargo test` (now including `gbc-platform`), no-alloc and hard
  rules, the thumbv7em build, code size (H735 total), the ITCM set before
  linking, the Ir benchmark (±3%), fuzz, invariance `--long`, the desktop
  headless run, and romtest must plus regressions. **This is how Stage 1
  keeps guarding the core.**
- **H3.** `cargo fmt --check` and `clippy -D warnings` for `gbc-nucleo-h753`,
  `tools/h7-bench` and `tools/h7-bench/host` (Stage 1 C-T6).
- **H4.** Release builds of every firmware image. The `build.rs` checks
  (vectors, RAM ordering, flash fit) and `check-itcm-elf.sh` (the
  post-link ITCM check).
- **H5.** h7-bench `host/` hashes equal `expected-hashes.txt`. The replay
  set's host hash lists are regenerated and equal the committed lists.
- **H6.** The QEMU smoke run of the `headless` loop, plus the ITCM
  stand-in run, prints exactly the expected lines.
- **H7.** Host unit tests for the converter, DC blocker, debounce,
  serial framing (ROM, save, input) and image header (inside H2's `cargo test`; named
  explicitly so deleting them fails the gate).
- **H8.** Firmware size report: `.itcm`, DTCM and AXI use, and stack
  paint depth from the last board run. Report only.

Board steps (serialized under the board lock; each flash is followed by
reading the serial port and **ignoring the lines before the new banner**):
- **B1.** `selftest-480`: `cpu_hz=480000000`, VOS0/ODEN read back,
  `actvosrdy=1`, the expected `flash_acr`, constant `cycles=`, and an
  equal `tcmprobe`.
- **B2.** `bench-itcm-dtcm-qspi-480` (and the internal-flash variant):
  every workload's `calls/frames/clocks/hash` equals
  `expected-hashes.txt`. Every workload ≤ `expected-cycles.txt` + 3%.
  Every sign-off workload (`synth:worst-case` and the replay ROMs) has
  max cycles per frame < 8,036,499 (real time). `synth:worst-case` max vs
  4,018,249 is printed as a **report line, not a check** (human,
  2026-11-25).
- **B3.** Load the replay set into QSPI (skipped if the hashes match).
  Then the `player` image in scripted mode: each ROM's frame-hash list
  and DAC-feed hash equal the host's, the display readback has 0
  mismatches, and the run ends with 0 underruns and 0 frames over budget.
- **B4.** A 10-minute `synth:worst-case` soak in the `player` image: 0
  frames over budget (budget = one real-time frame, 8,036,499 cycles), 0
  underruns, and emulated speed within ±0.1% of real time.
- **B5.** Save round trip: load a ROM with a test `.sav` (cart RAM plus RTC
  footer) over the serial link, dump it back, and the dump equals the
  original byte for byte (PC → board → PC).

What the gate can't see (the human's checklist): whether the panel looks
right, whether the audio sounds clean, and whether the joystick and PC
input feel right.

### Proposed `acceptance/stage2.toml` (human-owned; not created)

```toml
# On-board equality set: each ROM runs on the host and on the board with the
# same inputs; frame-hash and audio-hash lists must be identical.
roms_release = "<same as stage1.toml>"
cpu_mhz = 480
frame_budget_cycles = 8036499          # real time at 480 MHz; gated
worst_case_report_cycles = 4018249     # 50% of a 480 MHz frame; REPORTED, not gated
soak_minutes = 10                      # synth:worst-case, player image

replay = [
  { rom = "blargg/cpu_instrs/cpu_instrs.gb", run_frames = 3600 },
  { rom = "cgb-acid2/cgb-acid2.gbc",         run_frames = 600 },
  { rom = "dmg-acid2/dmg-acid2.gb",          run_frames = 600 },
  { rom = "blargg/cgb_sound/cgb_sound.gb",   run_frames = 2400 },
  { rom = "<a SameSuite HDMA ROM>",          run_frames = 600 },
  { rom = "<an rtc3test ROM>",               run_frames = 0, input = "<its stage1.toml script>" },  # frames as stage1
]
# synth:* workloads are built into the bench image (expected-hashes.txt).
# Save round-trip fixture (B5): a test .sav with RTC footer, committed hash.
# Optional, once the human records them (Stage 1 M10 E6): real-game replays
# of the checklist games, ROMs copied from X:\gbc-roms, hashes committed.
```

(Paths are indicative. The schema is reused from `stage1.toml` where it
fits. `romtest` would gain a `--board` mode, or a small separate tool
would read this manifest. Either one is pass/fail logic and gets a
review.)

## How Stage 1's gates keep running

- `gate-stage1.sh` is unchanged and is step H2 of the Stage 2 gate.
  Removing it or any of its steps would loosen the gate and needs the
  human.
- Per commit, as AGENTS.md requires: `cargo fmt`, `clippy` and `test`,
  plus `gate-stage2.sh --host-only` for firmware commits.
  `check-bench.sh`, `check-code-size.sh` and `check-itcm-set.sh` run for
  any commit that touches `gbc-core`.
- Core changes made for performance in M6 are ordinary core changes.
  They need the Ir benchmark, the invariance checks and romtest, and they
  must not regress the H735 size gate. A change that helps the M7 but
  costs Ir over 3% needs the human (memory: rebaselines with numbers).
- The desktop front end is not maintained past Stage 1, but its headless
  build and run stay in H2. That keeps one std consumer of the public API
  honest. Dropping it would be a scope call for the human.

---

## Decisions

D1, D3, D4, D5, D7, D9 and D10 are **accepted (human, 2026-11-25)** with
the recommendation shown. D2 and D6 are **decided** (human, 2026-11-25) as
shown. D8 is a note.

| # | Decision | Options | Outcome |
|---|---|---|---|
| D1 | On-board steps in the stage gate | (a) the gate requires the attached board (B1-B5); (b) the host-only gate, with the board results as a separate sign-off | **Accepted: (a).** The stage goal is a hardware result. Without the board the gate exits 1 |
| D2 | Input hardware (6 inputs on board, 8 needed) | (a) joystick + B1 + extra tactile buttons; (b) joystick and B1 only; (c) live input from the PC over the VCP; (d) a USB HID gamepad | **Decided (human, 2026-11-25):** no buttons are wired and nothing is bought. The shield joystick is the D-pad; **all 8 buttons come from the PC over the serial link**, so PC input alone can play; B1 may map to one button if convenient. Implemented in M5 |
| D3 | Display scaling and orientation | (a) 1x, centred, 3.0 ms per frame on the bus; (b) **1.5x, 240x216**, 6.8 ms; (c) 1.667x, 266x240, uneven pixels, 8.4 ms; plus portrait (tear-free with TE, image fills the width) or landscape (likely tearing, since writes run across the panel's scan) | **Accepted: (b) in portrait** if the physical orientation is acceptable. Otherwise (b) in landscape, accepting some tearing. The look is the human's call |
| D4 | Audio output path | (a) **internal DAC1, PA4/PA5**, 12-bit, line level (no purchase; needs powered speakers or headphones through an amp); (b) an I2S DAC breakout (e.g. PCM5102A) on I2S2 (PB12/13/15, PC6) or SAI on non-Zio pins (pin plan unverified): costs money, closer to Stage 3's SAI → DAC; (c) PWM | **Accepted: (a).** It meets the stage goal with no purchase. The ring, DC blocker and pacing carry over whichever is chosen |
| D5 | Getting ROMs into QSPI | (a) **our own UART loader** over the VCP, scriptable from WSL; (b) STM32CubeProgrammer with an external loader for the shield's flash (the human's tool; availability unverified); (c) a ROM embedded in the firmware image, copied to QSPI at boot (≤ ~1.5 MiB) | **Accepted: (a)**, with (b) as the human's fallback. Agents need (a) for the on-board gate |
| D6 | Saves on the dev board | (a) none persistent; (b) QSPI save area; (c) internal flash bank 2; (d) over the PC serial link | **Decided (human, 2026-11-25): (d), not QSPI.** The PC loader sends the `.sav` (cart RAM plus the RTC footer, the desktop format, ADR 0023) with the ROM, and the board dumps it back on request or a button combo. Folded into M5; the old M7 is gone. Acceptance: a save round-trips byte-identically, PC → board → PC (B5) |
| D7 | Full-speed sign-off workloads | `synth:worst-case` (bench, ROM in QSPI, 480 MHz) plus real games for the human: Shantae and Alone in the Dark (heavy HDMA), Wario Land 3, LADX, and Pokémon Crystal (RTC) | **Accepted.** Gated: each finishes its frames inside real time (< 1/59.7275 s). `synth:worst-case` vs 50% and the real-game core cycles from the `player` statistics are reported, not gated. They become gated replays if the human records inputs (Stage 1 M10 E6) |
| D8 | Cortex-M4 | — | **Note: not applicable**: the H753ZI is single-core (the dual-core H755 is retired), and the Stage 3 H735 is single-core too |
| D9 | Dependencies for the new crates | AGENTS.md lets agents decide dependencies for `gbc-desktop` and `tools/*` only | **Accepted:** extend that to `gbc-nucleo-h753` (MIT/Apache crates: `cortex-m`, `cortex-m-rt`, `stm32h7`, as h7-bench already uses) and keep `gbc-platform` dependency-free; `gbc-handheld`: decide when it exists |
| D10 | `acceptance/stage2.toml` | the proposal above | **Accepted:** the human creates it (or edits the proposal). Agents won't create it |

## Risks

1. **ITCM doesn't fit.** The hot set is 68,832 B in the ELF (69,078 B on
   the rlib, 11.7 KB over the 56 KiB budget). h7-bench fits 64,664 of
   65,280 B only by leaving the plain tile path in flash, and nothing is
   left for the front end. With the display, audio and pacing code added,
   more of the core leaves ITCM, at a cost (ITCM was 1.36-1.41x faster
   than flash on the worst case). Mitigation: the front end runs from
   flash/I-cache, tiers are dropped in the measured order, and the
   post-link check catches what LTO moves. The H753 is allowed to be
   slower, and the 50% figure is reported for it, not gated (human, 2026-11-25); it must still run in real time.
2. **480 MHz is scaled, not measured.** The 40.9% / 41.3% figure assumes
   the cycle count doesn't change with frequency. That held within 0.3%
   for `itcm-dtcm` between 64 and 392 MHz, but flash goes to 4 wait
   states (10 CPU cycles per miss, up from 6), and everything left in
   flash, including dropped tiers, slows down. Mitigation: M0 measures it
   before anything else.
3. **QSPI ROM read latency.** The bench ROMs sit in internal flash today.
   A D-cache miss to memory-mapped quad SPI is probably a few hundred CPU
   cycles per 32-byte line (≈ 20 command/address/dummy clocks + 64 data
   clocks at ~120 MHz; estimate, unverified). Writing the 100 KB display
   buffer would evict the 16 KiB D-cache every frame. Mitigation: a
   non-cacheable MPU region for the DMA buffers, QSPI tuning, and the
   optional ROM-placement work in M6. M2 measures it.
4. **FMC display bandwidth and contention.** At best 66 ns per byte, a
   1.5x frame is 6.8 ms of bus time. If the CPU wrote it, that would be
   41% of a frame, so MDMA is mandatory, and its AXI traffic competes with
   the core's QSPI and flash misses. Signal integrity through the shield
   may force slower timings, and landscape orientation may tear. M3
   measures the core's cycles with the display running.
5. **DMA, cache and MPU mistakes** (stale lines, speculative reads of the
   LCD or QSPI regions, DMA1 pointed at DTCM) cause silent corruption.
   Mitigation: non-cacheable DMA regions, Device type for the LCD, and the
   readback and hash-equality checks, which catch corruption that a
   person looking at the screen would miss.
6. **Board automation is flaky.** The ST-LINK has dropped off USB once,
   the serial port goes through PowerShell, and a held COM port blocks
   flashing. Mitigation: the board lock, bounded retries, and exit 2 on a
   tool error. A garbled or missing line never counts as a pass.
7. **Audio quality with the internal DAC** (12-bit, line level only).
   Acceptable for a stand-in, but it's the human's ears. Option (b) of D4
   is the fallback.

## Facts not verified (each has a task above)

The shield's QSPI part number, maximum clock, DTR/QPI support and
quad-enable bit. Whether the 66 ns write cycle is reachable through the
shield. PE4 as FMC_A20 and the 0x6010_0000 data address. A panel rate
near 59.7 Hz with DIVA = 01. The panel's native scan direction relative to
how the shield is mounted. VCP baud rates above 115200. HSI accuracy at 480 MHz
(avoided by using HSE). An ST app-note citation for the M7 MPU setup of
the QSPI and FMC regions. Unaligned loads from the QSPI window (tested in
M2). Whether the Cortex-M7 DWT has a branch-mispredict counter (believed
not). A non-Zio pin plan for SAI. Whether the `*-pll` bench images run
reliably on the H753 after the 2026-11-25 USB drop.

---

## Hardware sign-off checklist (human)

| # | Check | How | Result |
|---|---|---|---|
| 1 | Display | cgb-acid2 and dmg-acid2 look like their references; a game in motion looks acceptable (tearing, judder) | |
| 2 | Audio | `cgb_sound`'s tones plus a game's music: no crackle or drift, right pitch | |
| 3 | Input | every Game Boy button works on the input-test screen and in a game, with the joystick for the D-pad and PC input for the rest (and for all 8 on their own) | |
| 4 | Real games at full speed | about 10 minutes each: Shantae, Alone in the Dark, Wario Land 3, LADX, Pokémon Crystal (save load and dump over the serial link, D6) | |
| 5 | Power cycle | unplug and replug USB, then the banner shows `supply=ldo actvosrdy=1` and VOS0 (the supply write is only really exercised by a power-on reset) | |
