# Stage 2: STM32H7 platform layer (NUCLEO-H753ZI + X-NUCLEO-GFX02Z1)

> **Status: DRAFT, revised 2026-11-25 with the human's decisions** (see
> "Decisions" below: D1-D7 and D9-D11 are accepted or decided; D8 is a
> note). Scope is the human's. Nothing here has started.

**Stage goal** (`README.md` §2): the unchanged `gbc-core` runs on the
NUCLEO-H753ZI with the X-NUCLEO-GFX02Z1 shield. Display goes to the shield
TFT over the FMC, the ROM is read memory-mapped from the shield's QSPI NOR,
and there is input, audio out and frame pacing. Memory layout and caches are
tuned until games run at full speed. The 50% performance target
(`README.md`) belongs to the final MCU (STM32H735 at 550 MHz). The H753 is
lower spec, so on this board `synth:worst-case` (the realistic worst case,
`docs/perf.md`) is **measured and reported against 50% of 472 MHz
(3,951,279 CPU cycles per frame = 472 MHz / 59.7275 Hz / 2), not gated**
(human, 2026-11-25). What is gated is "games run at full speed": every
sign-off workload finishes each frame inside real time (frame time
< 1/59.7275 s, i.e. under 7,902,557 cycles at 472 MHz, whole loop
included), plus the on-board cycle baseline's +3% regression ratchet.
The figures are 472,000,000 / 59.7275 = 7,902,557.4 (rounded down), and
the 50% line 3,951,278.7 (rounded to nearest, as the human approved it).
The budget uses the nominal clock. This board's HSI measured +0.36%, so the
nominal figure is slightly conservative here; a board whose HSI runs slow
(DS12117 lower bound, 63.7 MHz at 30 °C) would have up to that much less
real time, which the real-time check does not see.

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
| 480 MHz (VOS0 limit) | VOS0 needs the LDO (this board has it) and TJ ≤ 105 °C. Rev V only (ES0392 §2.2.21). VOS0 is entered from VOS1 by setting SYSCFG ODEN | DS12117 Table 112 and notes 4-5 |
| Flash at 236 MHz AXI | 4 wait states, WRHIGHFREQ = 10. A miss costs 5 AXI clocks = 10 CPU cycles, against 6 at 392 MHz (4 WS is valid at any lower AXI clock too) | RM0433 Table 17 (VOS0 band ]225; 240] MHz) |
| HSE | 8 MHz from the ST-LINK MCO by default (SB45 ON). **Not used**: it measured 8.0644 MHz (+8,048 ppm) against the LSE, so 8/2 x 240 / 2 ran the CPU at 483.86 MHz, above the 480 MHz limit, and the MCO has no stated tolerance (settled, D11) | UM2407 §7.5.1; h7-bench README "Clock check" |
| HSI | 64 MHz, **the Stage 2 clock source** (PLL1 /4 x59 /2 = 472 MHz nominal, AXI/AHB 236, APB 118). 63.7 to 64.3 MHz at VDD = 3.3 V, TJ = 30 °C, plus ±1% over TJ -20 to 105 °C (so at most +1.5%: CPU 479.1 MHz, AXI 239.6 MHz). Measured on this board: 64.228 MHz (+0.36%, CPU 473.68 MHz) against the LSE | DS12117 Rev 10 Table 133 (rev V), f_HSI and ΔTEMP rows; h7-bench README |
| TCM/SRAM | ITCM 64 KiB, DTCM 128 KiB (neither reachable by DMA1/2; MDMA reaches both), AXI SRAM 512 KiB, D2 SRAM1-3, D3 SRAM4 | RM0433 Table 7; perf.md |
| LCD | **ST7789V** on this board (shield variant XNGFX02Z1$AZ2; $AZ1 has an ILI9341V): RDDID = 85 85 52 and TE at 60 Hz, both the ST7789V's defaults (verified 2026-11-25, `lcd-probe`). 240x320 panel (native portrait), 8080-I 8-bit bus: D0-D7 = PD14 PD15 PD0 PD1 PE7 PE8 PE9 PE10, RD = PD4, WR = PD5, CS = PD7 (FMC_NE1), D/CX = PE4 (FMC_A20), TE = PE13, RESET = PE11; AF12 = FMC on every bus pin (verified). The driver uses only commands common to both controllers. The backlight anode is tied to 3V3 and the cathodes don't go to the MCU, so there is no backlight control | UM2905 Table 2 and section 6.2; DS12117 Table 9; ST7789V datasheet V1.3 9.1.46-48, 9.2.16; `docs/reference/lcd-panel.md` |
| LCD address map | Command register at 0x6000_0000, data at 0x6010_0000: PE4 AF12 is FMC_A20, and an 8-bit bank puts byte address bit 20 on A20; default mapping (BMAP = 00); MPU region 7, 2 MiB Device. Verified on the board | DS12117 Table 9; RM0433 Tables 156-158; `docs/reference/lcd-panel.md` |
| LCD bus speed | twc ≥ 66 ns per byte (twrl, twrh ≥ 15 ns; same numbers in both datasheets). Measured CPU fills: 14.75 MB/s at 16 cycles of HCLK3 236 MHz (out of spec on the write hold times, 4.7 ns vs 10 ns; no error seen), 12.80 MB/s at 5 cycles of per_ck = HSI 64 MHz (every minimum met; recommended). 240x216 RGB565 (103,680 B) takes 7.03 ms or 8.10 ms against a 16.74 ms frame. Full-GRAM readback: 0 mismatches at both and at a 2x-slack 7.11 MB/s timing (verified 2026-11-25) | ILI9341V §18.3.1; ST7789V 7.4.1 Table 4; DS12117 Table 57; `docs/reference/lcd-panel.md` |
| LCD refresh | ST7789V: TE measured at 60 Hz (default FRCTRL2 RTNA = 0Fh, 60 Hz; 10h is 58 Hz, finer steps need the porch settings). The FRMCTR1 figures (61-119 Hz, default 70 Hz) are the ILI9341V's and do not apply to this board. **Unverified**: the exact rate (a 1 s count is ±1) and fosc tolerance | ST7789V 9.2.16; ILI9341V §8.3.2 |
| QSPI NOR | 64 Mbit = 8 MiB. NCS = PG6, CLK = PB2, IO0-3 = PD11 PD12 PE2 PD13. PE2 is also routed to Zio CN9 (SAI_A_MCLK), "only one function at a time". **The part number is not in UM2905**, so its command set, DTR/QPI support and maximum clock are unverified (read JEDEC ID 0x9F in M2) | UM2905 Table 3; UM2407 CN9 note |
| QUADSPI | ≤ 133 MHz SDR at VDD ≥ 2.7 V. Erratum: the first nibble is lost in indirect write with dummy cycles (§2.7.1); a hang if CCR is cleared (§2.7.3) | DS12117 Table 73; ES0392 §2.7 |
| Joystick | 5-way, active low, common to GND: LEFT PE3, CENTER PA0, DOWN PF8, UP PF9, RIGHT PF7. Plus user button B1 on PC13. **That is 6 inputs for 8 Game Boy buttons, so the joystick is the D-pad and the rest comes from the PC over the VCP (D2)** | UM2905 Table 4; UM2407 §7.6.2 |
| Audio | No codec on either board. DAC1 OUT1/OUT2 on PA4/PA5 (free of the shield). Zio's SAI1 pins clash with the shield (PE2 QSPI, PE4 D/CX, PE3/PF7/PF8/PF9 joystick). I2S2 on PB12/PB13/PB15/PC6 is free (PB13 is Ethernet TXD1 only with JP6 ON) | UM2407 Zio tables; UM2905 |
| Free pins with the shield fitted | ST morpho CN11/CN12 carry every GPIO. **Unverified** that they stay reachable with the shield plugged in | UM2407 |
| Host link | VCP on USART3 PD8/PD9 through the STLINK-V3E. Reached from WSL only through PowerShell .NET `SerialPort` (`read-serial.sh`). Baud rates above 115200 are unverified | h7-bench README; UM2407 §7.6.5 |
| FMC erratum | Unaligned **reads** from the FMC are unsupported (ES0392 §2.6.4). This only matters for LCD readback, which uses byte reads. The other FMC errata (2.6.1-3, 2.6.5-7) are synchronous, NAND or SDRAM only (checked) | ES0392 §2.6 |

## Crate layout (human-approved 2026-11-25, ADR 0005 amendment; ADR in M1)

```
gbc-core ◄── gbc-firmware ◄── gbc-nucleo-h753 ──► gbc-link ◄── gbc-pc
                          ◄── gbc-handheld ···► (gbc-link, if USB serial)
```

- `crates/gbc-core/` (exists): `no_std`, no alloc, no unsafe; all emulation.
  No dependencies.
- `crates/gbc-firmware/` (new): `no_std`, `#![forbid(unsafe_code)]`, a
  root-workspace member, host-tested. The board-independent firmware: the
  main loop and audio-driven pacing, video conversion/scaling (RGB555 to
  panel format), audio conditioning (DC blocker, sample ring), button
  debounce and input mapping, per-frame stats, and the ROM image header and
  hash. Boards plug in their drivers through small traits. It exposes plain
  hooks (load a ROM/save, take input, report stats). It depends on
  `gbc-core` only and **not on `gbc-link`**. **All of this carries over to
  Stage 3.**
- `crates/gbc-link/` (new): `no_std`, no dependencies, no unsafe,
  host-tested. The serial protocol: framing and checksums for ROM load, save
  load/dump, PC input and stats. Transport-agnostic. A dev tool that might
  be promoted: Stage 3 may reuse it over USB serial, or use USB mass
  storage instead and skip it (a Stage 3 decision).
- `crates/gbc-nucleo-h753/`: the Stage 2 firmware, a lib plus a bin (so
  `tools/h7-bench` can reuse its bring-up code), and a separate workspace
  like `tools/h7-bench` (thumbv7em only, its own profile and rustflags).
  Everything chip- or board-specific lives here: PAC (`stm32h7`,
  `cortex-m`, `cortex-m-rt`, as h7-bench uses; no HAL, no async runtime),
  `memory.x`, clocks, power, MPU, FMC/LCD (ST7789V on this board), QUADSPI, DAC, GPIO and UART.
  It depends on `gbc-firmware`, `gbc-core`, `gbc-link`, `cortex-m`,
  `cortex-m-rt` and `stm32h7`, and it wires `gbc-link` to the UART.
  Dependency rule: see decision D9.
- `crates/gbc-handheld/` (Stage 3, later; provisional name): the same role
  for the H735 board. It uses `gbc-link` only if Stage 3 chooses USB
  serial. Shared H7-specific code is extracted into a common crate only
  once Stage 3 actually duplicates it.
- `tools/gbc-pc/` (new): the PC companion, a console program built as a
  native Windows `.exe` (`x86_64-pc-windows-gnu`, cross-built like
  `gbc-desktop`, run from WSL through the `scripts/run-windows.sh` interop
  pattern) because WSL has poor COM-port access. It sends the ROM and
  `.sav`, pulls saves back, forwards keyboard input and prints the board's
  stats. It replaces the old PowerShell-script host side. Dependencies:
  `gbc-link` and `windows-sys` (direct Win32 COM; see D9).
  It has no window, so the WSLg ban doesn't apply; keep runs from WSL
  short, as with the other Windows runs.
- All Cortex-M7 parts use the same Rust target (`thumbv7em-none-eabihf`).
  What differs per chip is the PAC, the memory map and the peripherals
  (e.g. QSPI on the H753 vs OCTOSPI on the H735), which is why the split is
  per board and there is no generic "stm32" crate.
- `tools/h7-bench` stays the cycle-measurement tool (`h7-bench/host` and
  `h7-bench/m4-stub`, H755 only, are unchanged). In M1 its board bring-up
  code (power, clock, UART, fault handler, TCM copy) moves into
  `gbc-nucleo-h753`, so the two can't drift. h7-bench then depends on it.
  `romtest`, `bench`, `fuzz`, `sm83-tests` and `gbc-desktop` stay on
  `gbc-core`.

---

## M0: 480 MHz measurement (h7-bench only; superseded: now 472 MHz HSI, D11)

Answers the question everything else depends on before any new driver
exists: where does the worst case land against 50% at 480 MHz (superseded by D11: 472 MHz) (reported, not
gated)?

- [x] (6dfcfda; banner `d3cr=0x0000e000 syscfg_pwrcr=0x00000081 oden=1 flash_acr=0x24 cpu_hz=480000000`) `pll480` clock in h7-bench: HSE bypass 8 MHz (ST-LINK MCO) → PLL1 480 MHz; VOS1 → ODEN → wait VOSRDY → VOS0 (RM0433 PWR/SYSCFG sequence: cite section in code); flash 4 WS / WRHIGHFREQ=10 before the switch; AXI/AHB 240, APB 120. Banner prints `d3cr`, `SYSCFG_PWRCR`, `flash_acr`, `cpu_hz=480000000` (superseded: 472 MHz HSI, 761049b; b4d6c81: DIVM1 5, 12.8 MHz reference, fractional x 73.75, so fPLL_IN stays in 8-16 MHz over the HSI tolerance; the image name `pll480` and the ticked text above are history)
- [x] (7d4b026; `cycles=8000008` each line, `tcmprobe` equal, clean UART; bf6f7a5: the PC timed it at 484.3 MHz, 0.9% fast, HSE (ST-LINK MCO) the suspect) `selftest-480` on the board: constant `cycles=`, UART at 115200 (it bounds the clock error), `tcmprobe` equal (superseded: the HSE/MCO clock measured 483.86 MHz; 472 MHz HSI, 761049b; b4d6c81 on the board: `cycles=8000009`, LSE-measured 473.62 MHz)
- [x] (bf6f7a5; all 15 hashes equal in all three placements; worst case `itcm-dtcm` 3,291,153 / 3,328,005 cycles = 41.0% / 41.4% of an 8,036,499-cycle (480 MHz) frame, 82.8% of that 50% bar at most; against the 472 MHz figures 7,902,557 / 3,951,279: 41.6% / 42.1% of a frame, 84.2% of the 50% bar at most; flash 64.0%; nothing in the bound near a whole frame) All three placements at 480 MHz: hashes equal `expected-hashes.txt`; record mean/max and % of an 8,036,499-cycle frame in the h7-bench README and `docs/perf.md` (replacing the scaled figures) (superseded: 472 MHz HSI, 761049b; measured at 472 MHz with b4d6c81: all 15 hashes equal in all three placements, worst case `itcm-dtcm` 3,296,548 / 3,334,278 = 41.7% / 42.2% of 7,902,557, 84.4% of 3,951,279 at most, flash 65.6%; 0.984x-1.036x the 483.86 MHz counts, about +-2% build-to-build)
- [x] (60ef23d; first board run bf6f7a5, hashes equal; storms 1.8-3.6 frame times per frame in ITCM, docs/perf.md "Outside the bound") Add the bench workloads the board does not run yet: `dma-storm`, `halt-irq`, `apu-writes`, `oam-lines`, and the five `synth:storm-*` (the Stage 1 M8 task "measure the dense mid-mode-3 programs on the M7"; outside the bound, recorded for context). Regenerate `expected-hashes.txt` for the new lines only (old lines unchanged, say so in the commit)
- [x] (bf6f7a5; closed: `event-storm` at 480 MHz is 2,014,882 / 2,028,825 cycles in ITCM, 2,143,998 max from flash, at most 50.5% (ITCM) and 53.4% (flash) of the 4,018,249 bar (480 MHz; 51.3% and 54.3% of the 472 MHz bar, 3,951,279); B4 not opened for it) Re-baseline the event-density question (Stage 1 M8 "worst-case event density on the M7") with the 480 MHz `event-storm` figure; close it or open B4 below (superseded: 472 MHz HSI, 761049b; measured at 472 MHz, b4d6c81: 2,016,303 / 2,031,279 in ITCM, 2,140,134 max from flash, 51.4% and 54.2% of 3,951,279 at most; still closed)
- [x] (bf6f7a5) Update `docs/perf.md` "Measured on a Cortex-M7" (still titled H755) and `docs/architecture.md` workspace line (h7-bench "on a NUCLEO-H755ZI-Q")

**Acceptance:** `bench-itcm-dtcm-pll480` on the board prints `end pass=1`,
hashes equal to `expected-hashes.txt` for every workload, and its cycle
figures are committed. The 50% bar is **measured and reported** here, not
gated (the H753 is lower spec than the final H735; human, 2026-11-25). If
`synth:worst-case` max > 3,951,279, say so in STATUS and the log. It does
not block anything. If it exceeds a full real-time frame (7,902,557),
the human hears about it at once (STATUS "Needs human") and M6 moves ahead
of M3.

## M1: Firmware skeleton, link-time placement, host-side gate

- [x] (ADR 0036, 20300b4; generator in `tools/memory-x`, 7d10abd) ADR: crate layout as above (per-board firmware crates plus `gbc-firmware` and `gbc-link`); PAC-only; one `memory.x` generated by `build.rs` from `perf/itcm-sections.txt` (ADR 0026 mechanism); board module boundary
- [x] (34d7fab; board: `icache=1 dcache=1 mpu=1`, `cycles=8000018` constant; a read of 0x9000_0000 faults with the UART report `cfsr=0x00000082 mmfar=0x90000000`) `gbc-nucleo-h753` boots at 472 MHz (shared bring-up), MPU set up (see M2/M3 for the regions), I/D-cache on (write-back; ES0392 §2.1.1 rules out write-through), fault handler on its own stack with UART report (h7-bench pattern)
- [x] (34d7fab; `lse_cpu_hz=473532048`) The `gbc-nucleo-h753` banner prints the LSE-measured CPU clock (h7-bench's `clockcheck` method), so a run records its actual clock (D11)
- [x] (71b22ec, a1f5bdb; on the board all 602 lines equal for cpu_instrs and cgb-acid2, re-run from main's tree; cpu_instrs mean/max 1,292,317 / 1,764,893 cycles (16.4% / 22.3% of a frame), cgb-acid2 681,737 / 754,643; `.itcm` 64,080 B, 1,200 B free) `headless` image: core in the M6-target placement (ITCM set + DTCM data), a ROM from internal flash, a scripted input list, prints one hash per frame plus an audio-stream hash over UART; host counterpart in `tools/` prints the same lines for the same ROM + inputs
- [x] (e9b440c; `--drop <tiers>`; on `bench-itcm-dtcm-pll480`: 48 kept functions in ITCM, `.itcm` 64,024 B, 92 veneers; mutation-checked) `scripts/check-itcm-elf.sh` (the Stage 1 M8 "post-link check", left for Stage 2): every demangled name in `perf/itcm-sections.txt` that the link kept is in `[0, 0x1_0000)`; dropped tiers listed; `.itcm` size vs 64 KiB; veneer count. Mutation-check: a glob moved to flash fails, a renamed function fails
- [x] (bb3fc2d; docs/perf.md "Firmware-link size": rlib gate 116,150 B, gbc-core in the link 111,022 B; with builtins in ITCM and fmt/panic in flash 120,488 B of 122,880; gate kept on the rlib plus a link report, `scripts/report-firmware-size.exs`) Settle the provisional size gate on the firmware link (Stage 1 C-P5): measure gbc-core's share of the linked ELF (compiler_builtins, panic code, padding) and report against the H735 total; **changing the 122,880 B figure needs the human**
- [x] (fb33d98: 1,207 B measured, not 1.7 KB; gbc-core `.rodata.cst*` (128 B, hot) now in `.dtcm_rodata`; panic paths and `.L*` records (1,079 B) are cold and stay in flash) Name or glob the ~1.7 KB anonymous `.rodata` the `.dtcm_rodata` rules miss (Stage 1 C-P7)
- [x] (dd67178; `tools/headless-qemu/run.sh`: both ROMs EQUAL at 600 frames, 1.35 s / 0.70 s; h7-bench `qemu`/`qemu-itcm` still equal) QEMU (`mps2-an500`) smoke of the `headless` frame loop: same hash lines as the host (h7-bench's `qemu` mode, extended)
- [x] (5904be4; first version approved by the human 2026-11-25, "I approve"; `--host-only` all OK, exit 3, 595 s; board steps B1-B5 not implemented: exit 1 without `--host-only`) `scripts/gate-stage2.sh` host part (below), and fmt/clippy/build for `tools/h7-bench`, `tools/h7-bench/host` and `gbc-nucleo-h753` (Stage 1 C-T6, stricter)
- [x] (1bbd37e; `scripts/lib/board-lock.sh`, `${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock}`, ADR 0036; not yet exercised by a real flash) Board lock: one `flock` file (e.g. `target/board.lock`) taken by every flash/serial script, so two agents never drive the board at once
- [x] (afe079e: `--remap-path-prefix` passed by both build.sh files; same sha256 from two checkout paths) Panic-location strings embed the absolute checkout path, so firmware `.bin`s differ between worktrees (fb33d98 finding): add `--remap-path-prefix` to the firmware and h7-bench rustflags so images are path-independent (check it does not change root-workspace Ir numbers)
- [ ] M1 review T-4 (low): `tools/headless/expected/*` and `tools/h7-bench/expected-hashes.txt` are generated by the code they check (board/QEMU-equals-host, not correctness) and agent-regenerated after core changes; record that ownership in an ADR note or `acceptance/README.md` (contract text: stricter-only wording, else ask the human)
- [x] (c69399c; every duplicate copy must be in ITCM, an absent kept tier fails; mutation-checked) M1 review T-7 (low): `check-itcm-elf.sh` takes the first ELF symbol with a matching name (a duplicate local copy in flash would pass), and a kept tier wholly absent from the link is report-only
- [ ] M1 review T-8 (low): gate H8 picks the newest gbc-core rlib, not the one matching the ELF's crate hash (report-only figures)
- [x] (7569334: a 128 B guard, MPU region 11, at `__stack_guard` below `_stack_end`; overflow test on the board faulted with MMFAR in the guard, docs/reference/player.md "Stack guard") M1 review F-2 (low): no stack guard: the DTCM stack grows into `.bss`/`.dtcm_rodata`/`.data` (`MEM`, `PREFIX`) silently; add a 32 B no-access MPU region at `_stack_end` (Armv7-M B3.5); measured use 2,952 B of ~77 KB
- [ ] M1 review F-3 (low): ITCM-placed mem builtins (`__aeabi_memcpy`...) are only valid after `tcm::place()` in `init`; a fault/panic before it locks up silently: enforce the order or keep a flash path for the fault handler
- [ ] M1 review F-5 (low): `.axisram` statics (NOLOAD, never zeroed) are declared `[0; N]`; use `MaybeUninit` (headless, h7-bench, headless-qemu)
- [ ] M1 review F-6 (low): `.dtcm_rodata` has no ASSERT (a rename or LTO leaves the tables in flash silently; perf only); no alignment ASSERT on `__siitcm`/`__sidtcm_ro`; the lib's copy duplicates cortex-m-rt's (`__edata` covers it)
- [ ] M1 review F-7 (low): `clockcheck.rs` leaves PWR_CR1.DBP set (backup domain unprotected, RM0433 6.8.1); clear it after `start()`; also the stale `asm::delay` comment in `src/bin/boot.rs`

**Acceptance:** `gate-stage2.sh --host-only` passes. On the board, the
`headless` image's frame-hash and audio-hash lines for `cpu_instrs` and
`cgb-acid2` (internal-flash ROMs, 600 frames each) equal the host's byte
for byte.

**Met (session 22, 2026-11-25; tag `stage2-m1`):** `--host-only` H1-H8 all OK on f1e1406 plus docs; on the board both ROMs EQUAL (602 lines) with the image CRC matched. Review: 3 mediums fixed (c2e8e2b, f1e1406, 953e829), 1 rejected, lows as tasks above.

## M2: ROM in QSPI NOR (memory-mapped)

- [x] (b325187 indirect + ID: MX25L6433F, `docs/reference/qspi-nor.md`; ac757d6 memory-mapped 0xEB at 59 MHz, FSIZE 16 MiB per ES0392 2.7.5, ABORT falls back to EN=0; edcd218 write side, measured 64K erase 310-340 ms, page 0.28-0.41 ms) QUADSPI driver: JEDEC ID read (record the part, its max clock, quad-enable bit, DTR support in `docs/reference/`), sector erase, page program (no dummy cycles in indirect write: ES0392 §2.7.1), status polling; memory-mapped quad read (`0xEB` or the part's equivalent), FSIZE for 8 MiB, prefetch/timeout settings
- [x] (ac757d6; region 6 = 8 MiB RO XN Normal WB, enabled only while memory-mapped; rest under the no-access background region; AN4861 citation unverified) MPU: QSPI window 0x9000_0000 as Normal, cacheable, sized exactly to the device; the rest of the 256 MB QSPI region no-access, so speculative M7 fetches cannot reach unmapped space (standard ST guidance for the M7; cite the app note once checked: unverified)
- [x] (ac757d6; HFNMIENA=1, fault path only uses mapped regions; a 0x9080_0000 read still faults and reports) M1 review F-4 (low, matters here and in M3): MPU_CTRL.HFNMIENA = 0, so inside HardFault the default map makes FMC (0x6000_0000) and QSPI (0x9000_0000) Normal cacheable: speculative reads (ES0392 2.1.2) can strobe the LCD or stall on QSPI; set HFNMIENA or make the fault path safe
- [~] (105c9dd `qspi-mmap`: 0 mismatches cold/warm/uncached, but over a blank NOR; 27c0039 `qspi-write` on a real pattern in the last 64 KiB: 3,456 checks, 0 mismatches; re-run `qspi-mmap` on the loaded 4 MiB image in the loader task) On-board test: unaligned u16/u32 loads at odd addresses from the QSPI window return the right bytes (the core relies on this, `docs/architecture.md` "ROM")
- [x] (23bd089, ADR 0037; FNV-1a 64, header written last, erased header = no image) ROM image format in `gbc-firmware`: header (magic, length, SHA-256 or 64-bit hash, title) at a fixed QSPI offset, ROM data 256-byte aligned; the firmware refuses a bad hash
- [x] (cb1e8f1..ec0f6ad, ADR 0038, `docs/reference/loader.md`; 4 MiB round trip verified through the window, AlreadyLoaded on repeat, interrupted load reads Erased; baud: see M2 review H-1 fix) Loader (decision D5): firmware `loader` mode receives a ROM over the VCP, programs it, verifies the hash, reports; host side is `tools/gbc-pc` (native Windows `.exe`, see Crate layout; it replaces the PowerShell-script idea); skip when the stored hash already matches. The frame format lives in `gbc-link` (host-tested, started here together with `tools/gbc-pc`) and has room for an optional save section sent with the ROM (used in M5) and for later PC-to-board and board-to-PC frames (input, save dump). Measure the VCP's highest reliable baud rate (unverified) and record it
- [x] (7458dc5..6dc6260, docs/perf.md "QSPI ROM placement (M2)": same binary, same boot; worst-case +2.4% from QSPI, 3,417,095 max = 86.5% of the 50% line; storm-gdma +14%/+28%) Bench placement `itcm-dtcm-qspi`: the bench workloads with their ROMs in QSPI instead of internal flash; record cycles vs `itcm-dtcm` at 472 MHz (the "QSPI as a conservative proxy for HyperRAM" figure Stage 3 needs)
- [~] (0701bed: the build.sh half, shared `scripts/lib/image-range.sh`; the gate half waits for B1/B2 to exist) Gate steps B1/B2 (h7-bench images): require the banner's `image_bytes=<n> image_cksum=<crc>` (every lib image prints it, `crates/gbc-nucleo-h753/src/image.rs`) to equal `cksum` of the flashed `.bin`, as `run-headless.sh` does (M1 test-integrity review: a copy the ST-LINK accepted but did not program left the old image reporting); h7-bench's build.sh to check the image range equals the `.bin` as gbc-nucleo-h753's build.sh does
- [x] (0765d62, f3020ab `headless --features rom-qspi`, `run-headless-qspi.sh`: cpu_instrs and cgb-acid2 from QSPI EQUAL, 602 lines, ROM hash checked against the file) `headless` image running its ROM from QSPI (M2 acceptance)
- [ ] M2 review H-2 (low): `qspi::FORBIDDEN` blocks only 0x60/0xC7/0x01 but `Qspi::read` sends any opcode: EQIO 0x35 (QPI until reset), WRSCUR 0x2F (permanent LDSO), DP 0xB9 get through; allow-list read opcodes or extend the deny list
- [ ] M2 review H-3 (low): `Qspi::init` never resets the NOR (RSTEN 0x66 + RST 0x99) or forces it out of 0xEB enhance/QPI mode left by other firmware; add the reset at init (QPI needs it on 4 lines)
- [ ] M2 review H-4 (low): after the EN=0 exit fallback the FIFO may still hold prefetched bytes (RM0433 23.3.15: only ABORT flushes); `drain` would return them as the next indirect read's data; assert FLEVEL == 0 after restoring CR, or ABORT once more with EN=1
- [ ] M2 review H-5 (low): `Qspi::init` has no once-guard and `Mapped` no Drop; a second init with region 6 open pulses QSPIRST under an open window (bus error); add a DONE flag or close region 6 in `init`
- [x] (M6 review T2 fix 10a8d65: check-cycles.sh requires two agreeing reports and min<=mean<=max) M2 review T-3 (low): the h7-bench report has no integrity check (cycle fields are zeroed before comparing), so a VCP-garbled digit can reach docs/perf.md; require two byte-identical reports or add bytes/cksum to `end pass=`
- [x] (25156c0) M2 review T-4 (low): the new script checks (`RUN_HEADLESS_ROM`, `check-report.sh` failure modes) were checked only on uncommitted captures; add regression tests like `scripts/test-board-lock.sh`
- [ ] M2 review T-5 (low): `docs/reference/loader.md` documents `flash.sh loader` and `gbc-pc.sh load` as two lock takes (fails safe); `GBC_BOARD_LOCK_HELD=1` makes the wrappers skip the pre-build so `gbc-pc.sh`'s cargo build runs outside `GBC_BUILD_LOCK`
- [x] M2 review H-6 (low, no change; human 2026-11-25: "that doesnt matter then for the nucleo prototype"): the ADR 0037 header shares the 8 MiB NOR, so the NUCLEO takes ROMs up to 8 MiB − 256 B and an 8 MiB MBC5 cart can't be loaded. Stage 3 is unaffected (eMMC library, ROM copied into ≥ 8 MiB HyperRAM, hardware ADR 0003)

**Acceptance:** a load → verify round trip passes for a 4 MiB test image
(hash equal, read back through the memory-mapped window). The `headless`
image running `cpu_instrs` and `cgb-acid2` **from QSPI** prints the same
hash lines as the host. The `itcm-dtcm-qspi` figures are committed. The
`synth:worst-case` figure from QSPI is reported against the 50% bar (not
gated). If it exceeds a full real-time frame, the M6 ROM-placement task
becomes required.

**Met (session 23, 2026-11-25; tag `stage2-m2`):** 4 MiB round trip, headless from QSPI (both ROMs EQUAL) and the h7-bench QSPI pass re-run on dc36238 (`docs/reference/m2-evidence.md`); `synth:worst-case` from QSPI 3,417,095 max = 86.5% of the 50% line, 43.2% of a frame, so the M6 ROM-placement task stays optional; `--host-only` H1-H8 OK. Review: 3 mediums fixed (H-1 36dbe9c/1c53efe, T-1 79706b7, T-2 dc36238/c617829), H-6 closed by the human, lows as tasks above.

Carries over: the MPU/memory-mapped lessons and the measured miss cost
(OCTOSPI HyperRAM is memory-mapped the same way), the image header, and
the loader protocol framing. The NOR program/erase code does not carry
over (Stage 3 loads from eMMC).

## M3: Display (FMC → ST7789V; plan first written for the ILI9341V variant)

- [x] (`lcd.rs`, d9d5655; writes mode A, reads mode D; shipped timing SPEC = per_ck HSI 64 MHz, 5 cycles/byte, in spec; docs/reference/lcd-panel.md) FMC bank 1 as SRAM/8-bit/write-enabled, asynchronous mode A; ADDSET/DATAST from twc 66 ns, twrl/twrh 15 ns at a 240 MHz HCLK (and a slower fallback); MPU region for the LCD as Device (no speculative reads, no write merging) or FMC BMAP remap; GPIO AF12 on the pins above (check each AF against DS12117)
- [x] Panel init (done in `lcd.rs`, session 24; ST7789V, commands common to both chips; reset timing §15.4, sleep out, COLMOD 16-bit, MADCTL for the chosen orientation; the BGR bit lets the core's R-low RGB555 order go out without a channel swap), window set (CASET/PASET), RAMWR
- [x] (host part done: `gbc-firmware::video`, b943740; board cycles: docs/perf.md "Display in the loop (M3)") Converter/scaler in `gbc-firmware` (decision D3: 1.5x to 240x216 recommended): RGB555 → byte-swapped RGB565, nearest-neighbour 2→3; host unit tests against a naive reference over random frames; cycles per frame measured on the board
- [ ] (conversion into the non-cacheable AXI buffer done, ADR 0039; the headless image keeps `Frame` in cached AXI SRAM, so `Frame` in DTCM moves to the `player` image, M5/M6) `Frame` in DTCM (Stage 1 B10/C5): after `FRAME_READY` the CPU converts it into an AXI SRAM output buffer (MPU non-cacheable, so the 100 KB of writes do not evict the ROM's D-cache lines), then MDMA streams the buffer to the FMC data address; no double-buffered `Frame` is needed (the core is not running while the conversion runs)
- [x] (`lcd_dma.rs`, ADR 0039: MDMA started from the TE ISR, FRCTRL2 0Eh + PORCTRL 11h/11h, TE 16.735-16.749 ms vs 16.743; tearing itself is the human look) Tearing: TE (PE13) via EXTI; start the transfer on TE with rows written in the panel's native scan order (portrait 240-wide rows, so the write front stays ahead of the scan: 31.7 µs per 1.5x row vs ~44.6 µs per scanned row at 70 Hz); try the ST7789V FRCTRL2 (C6h) and porch settings near 59.7 Hz (default 60 Hz) to remove 70-vs-59.73 Hz judder. ADR with what was measured
- [x] (in the headless loop, `headless-display`, 6d696e9: `stats display` line) Readback self-test: after a frame, read a GRAM window back (RAMRD, byte reads, 18-bit format; ST7789V readback expansion measured, docs/reference/lcd-panel.md) and compare with what was sent; mismatch count over UART
- [x] (docs/perf.md "Display in the loop (M3)") Measure: conversion cycles, DMA time per frame, and the core's cycles per frame with the display running (AXI/FMC contention), against M2's figures
- [ ] M3 review T-3 (low): the readback compares GRAM with the DMA buffer, not with the core frame (`lcd_dma.rs` readback); an on-target `scale_frame` miscompile would pass: add a sampled on-board check against the `video` reference math
- [ ] M3 review T-4 (low): docs/perf.md M3 says the display "costs the core nothing measurable"; te-wait is +0.07% vs 0.054% boot-to-boot noise: reword; and say the 5% figure excludes the TE ISR's RAMWR+SWRQ (tens of cycles)
- [x] (9c81d69) M3 review T-5 (low): stale ILI9341V names/sections in `gbc-firmware/src/video.rs` docs, the `gbc-nucleo-h753/Cargo.toml` lcd-probe comment, and `lcd.rs` 5->6-bit expansion docs mixing ILI9341V and ST7789V section numbers
- [x] (9c81d69) M3 review P-1 (low): after a `LcdDma::wait()` timeout the channel stays enabled and STATE PENDING/RUNNING; `lcd()`/`buffer()`/`parts()`/`present()` then spin forever and a later TE still fires RAMWR+SWRQ (headless.rs works around it with `dead`); disable the channel and reset STATE on timeout; `new()` should wait for CTCIF per RM0433 14.5.5; `busy()` silently consumes TEIF
- [x] (9c81d69) M3 review P-2 (low): the EXTI13 pending clear relies on a DSB through bufferable Device region 5; read CPUPR1 back after the clear so a late clear cannot re-enter `on_te` and skew TE_MIN
- [ ] M3 review P-3 (low): `lcd::Timing::new` doesn't model trcsfm 355 ns / tcsf (CS setup for frame-memory reads); record which reading of the figure is assumed (readback passes at all three timings)
- [x] (9c81d69) M3 review P-4 (low): `lcd.rs` cites DS12117 Tables 55/57 (rev Y section); this rev V part's are Tables 152/154 (values identical)
- [ ] M3 review P-5 (low): the panel init sends no VCOM/gamma/power registers and no INVON; readback can't see the glass; the human look should check for inverted colours and poor contrast; compare with ST's BSP init for the ST7789V shield if it can be found (MIT/BSD-licensed only for copying)
- [ ] M3 review P-6 (low): `Lcd::set_timing` resets the FMC while PD4/PD5/PD7 stay in AF12 (pin levels during reset undefined: a stray byte could reach the panel); lcd-probe only; switch the pins to GPIO high around the reset
- [x] (9c81d69) M3 review P-7 (low): the NOLOAD DMA buffer is written with 32-bit non-cacheable stores into 64-bit ECC AXI SRAM (read-modify-write of uninitialised words after power-on, RM0433 3.2); zero the buffer once with 64-bit writes at init (ties to M1 F-5)
- [x] (9c81d69) M3 review P-8 (low): `lcd_dma.rs` readback and headless.rs docs say "about 70 ms" per readback; measured 90.3 ms
- [x] (9c81d69) M3 review P-9 (low): say in `lcd::SAFE`'s doc that it is a bus fallback only (7.1 MB/s, below the 10.1 MB/s write-ahead needs, so it tears)

**Acceptance:** over 600 frames of `cgb-acid2` with the display running,
the readback self-test reports 0 mismatches. The core's frame hashes still
equal the host's. Conversion plus DMA setup is ≤ 5% of a 472 MHz frame
(about 402k cycles; the target is a proposal). Human check: one look at
cgb-acid2 and dmg-acid2 on the panel (sign-off item 1).

Carries over: the converter/scaler structure, the DTCM-frame → DMA-buffer
pattern and the TE/pacing approach. The FMC 8080 driver carries over only
if Stage 3 picks an FMC-parallel panel (still undecided in the hardware
repo).

## M4: Audio out

- [x] (`audio_dac.rs`, `dac-probe`, 477c997; docs/reference/audio-dac.md: the TIM6 clock is hclk1 236 MHz, ÷7202 = 32,768.675 Hz (+20.6 ppm), not the 240 MHz ÷7324 below; DMA1 stream 0 via DMAMUX request 67, 2 x 256 frames, MPU region 9) Output path per decision D4 (recommended: DAC1 stereo on PA4/PA5, 12-bit, buffer on, line level): TIM6/TIM7 trigger at 32,768 Hz from the 240 MHz timer clock (÷7324 = 32,768.9 Hz, +27 ppm; agent check of the timer clock tree), DMA1 circular double buffer in AXI SRAM (DMA1 cannot reach DTCM), half/full-transfer ISR
- [x] (`gbc_firmware::audio`, 54bdc05: k = 9, ~10 Hz; ring 2048 frames; `dac_feed_hash`) `gbc-firmware`: DC blocker (integer one-pole high-pass, ADR 0035 leaves it to the platform), i16 → 12-bit, sample ring with underrun/overrun counters; host unit tests (step response, no overflow at ±full scale)
- [x] (`gbc_firmware::paced` 4ddf563, host `headless --paced` 4ee61aa, board `paced-<rom>` 6485873 + `run-paced.sh` fab1e5a; docs/perf.md "Paced loop (M4)", docs/reference/m4-evidence.md) Audio-driven pacing (as desktop ADR 0024): the main loop runs the core while the ring has room for a frame's samples (≤ 550), else `WFI`. Statistics: underruns, ring-fill min/max, emulated clocks vs elapsed DWT cycles
- [x] (`paced drain` = host, cgb_sound and synth:worst-case, 35,837 frames, run-paced.sh; docs/perf.md "Paced loop (M4)") The sample stream the DAC ISR writes into the DMA half buffer (after DC blocking; the consumer side, not a read of the DAC output) hashes equal to the host's for the same ROM and inputs
- [x] (20fde08) M4 review T-1 (medium): `scripts/test-run-paced.sh` tests run-paced.sh on captures from the real M4 report (pass and every exit-1/exit-2 path; mutation-checked)
- [x] (20fde08) M4 review T-2 (medium): M4 evidence committed in docs/reference/m4-evidence.md (+ m4-evidence/); docs cite main's 6485873 / fab1e5a, not the pre-rebase commits
- [x] (20fde08) M4 review T-4 (low): docs/perf.md first-pop wait is about 30 ms (-50 ppm), recomputed from paced.rs `RAMP_LEFT`
- [x] (20fde08) M4 review T-5 (low): run-paced.sh header notes the host run passes no `--input` (a `GBC_HEADLESS_INPUT` image false-fails on input_fnv)
- [x] (20fde08) M4 review T-6 (low): the `paced drain` hash is what the ISR wrote into the DMA half buffer (consumer side), not a read of the DAC output
- [x] (3a0643b) M4 review P-1 (medium): `LcdDma::abort` waits (bounded, interrupts on) for CTCIF after clearing EN before the flags are cleared and the channel reprogrammed (RM0433 14.5.5: the ongoing 128-byte buffer completes); lcd-dma-probe's abort-while-running self-test: CTCIF after 8,756 cycles, next frame 0 mismatches (51,699 without the wait)
- [x] (6cd36fc, 80a0ce7) M4 review P-2 (medium): `AudioDac::watchdog` (paced image's Idle hook, after WFI) restarts the DAC path after a stall (no refill for 50 ms: DMAUDR1 or a stream error), counts the missed flags and `restarts=`; run-paced.sh requires restarts=0 (test-run-paced.sh covers it). Forced underrun on the board: udr1=1 restarts=1, run completes, exit 1; without the watchdog it hangs (no report)
- [x] (eb2e432) M4 review P-3 (low): DAC ISR reads/clears LISR then reads NDTR; entered ~half a buffer late with the next HT/TC in between, it refills the same half twice (256 frames lost, not counted LATE): remember the last half refilled
- [x] (eb2e432) M4 review P-4 (low): lost wakeup between the `free()` check and WFI (≤ 7.8 ms); for M5 use cpsid i / check / wfi / cpsie i (Armv7-M B1.5.19)
- [ ] M4 review P-5 (low): DBGMCU_CR survives system reset (RM0433 DBGMCU: power-on reset only), so DBGSLEEP_D1 stays set into later images; Stage 3 shouldn't rely on DWT in sleep: use a TIM or the refill count
- [x] (eb2e432) M4 review P-7 (low): RING/TAP statics can't be reset (a second `paced::run` fails NotFresh); the skip policy drops the newer frame even when the previous transfer is only PENDING (27% skips, ~44 fps): replace the pending frame or double-buffer in M5
- [ ] M4 review P-7 remainder (low): the skip policy is fixed (eb2e432); the RING/TAP statics still can't be reset, so a second `paced::run` in one boot fails NotFresh (the M5 player must reset or re-create them)

**Acceptance:** a 10-minute run of `cgb_sound` plus `synth:worst-case`
with the display on: 0 underruns. Emulated speed is within ±0.1% of real
time (emulated clocks / 4,194,304 against DWT cycles / 472 M). The
DAC-feed hash equals the host's. Human check: listen through powered
speakers or headphones via an amp (sign-off item 2).

Carries over: the ring, the DC blocker, pacing and statistics. The DAC
driver does not carry over (Stage 3 is SAI → external DAC; with D4 option
b, an SAI/I2S driver partly would).

## M5: Input and the player loop

- [x] (ad690b8) Buttons per decision D2 (no buttons wired, nothing bought): the shield joystick is the D-pad (LEFT/RIGHT/UP/DOWN; CENTER is spare), B1 (PC13) may map to one button if convenient (e.g. Start), and **PC input over the serial link carries all 8 buttons**, so PC input alone can play. Debounce and mapping live in `gbc-firmware` and the PC input frame (button-state bitmap, sent on change) in `gbc-link` (both host-tested); the host side is `tools/gbc-pc`, which turns key state into those frames (extending the M2 protocol and tool). Joystick and PC state are OR-ed, sampled once per frame and applied with `set_buttons_at` at the frame's start clock
- [x] (64d7374..569161e, docs/reference/m5-evidence.md: run-replay.sh, the 6-ROM proposed set EQUAL on the board) Scripted input mode: the firmware takes an input script (the `acceptance/*.toml` `input` schema, converted to clocks exactly as romtest does) instead of GPIO or serial, so replays are deterministic
- [x] (ad690b8) Saves over the serial link (decision D6; replaces the old QSPI save milestone). `tools/gbc-pc` sends the `.sav` (cart RAM plus the RTC footer, the same format as the desktop front end, ADR 0023) with the ROM at load time. The firmware keeps cart RAM and RTC state in RAM (AXI SRAM) and loads it into the core. The board dumps the current save back to the PC on request (a serial command) or on a button combo (joystick CENTER held with B1, or an equivalent PC input chord). The save frame format and size/format checks live in `gbc-link` (host-tested; `gbc-firmware` exposes the load-save/take-save hooks; a malformed or wrong-size save is refused, never half-applied). No flash save area and no flush policy
- [x] (f3187ad) `player` image: ROM from QSPI, display, audio and input all running, per-frame statistics over UART every N s (core cycles min/mean/max/p99 per frame, front-end cycles, slack, frames over budget, underruns, display skips)
- [x] (ad690b8) An "input test" screen or UART echo of button state for the human's button check
- [x] (d1aaae6) DMG-mode games: one default compatibility palette (romtest's common palette); per-title palettes are not needed on the dev board
- [x] (ce38bea) M5 review T-1: run-replay.sh treats any run-paced.sh exit other than 0/1 as a tool error
- [x] (ce38bea) M5 review T-3: the soak's +-0.1% bound is the entry's `max_ppm=1000` in soak-set.txt; env can only tighten
- [x] (ce38bea) M5 review T-7: a group entry's explicit frame count must equal the table's end
- [x] (1c1f3aa) M5 review T-6: m5-evidence.md cites commits on main
- [x] (1c1f3aa) M5 review P-3: link-protocol.md: the player's save buffer is 64 KiB + 64 (128 KiB + 64 is link-probe's)
- [ ] M5 review T-4 (low): player over-budget is counted at the next service call, so the last frame is never counted (frames=35836 of 35,837)
- [ ] M5 review T-5 (low): run-player-save.sh and run-save-roundtrip.sh don't check the flashed image cksum, only image=player
- [ ] M5 review T-8 (low): .gitignore `!tools/gbc-pc/tests/fixtures/*.sav` exception; mitigated by the generator test in crates/gbc-firmware/tests/save.rs
- [ ] M5 review P-2 (low): the 64 KiB player save limit comes from section order; putting the 128 KiB-aligned panel BUF first frees room for a 128 KiB + 64 save buffer; also a too-big save's dump answers Nak UnexpectedType, OutOfRange would be clearer
- [ ] M5 review P-4 (low): link_uart read() can't detect a ring lap > 4 KiB between polls or a dead RX stream (RX TEIF never read)
- [ ] M5 review P-5 (low): player.rs ~443-446 drops a Stats report silently on encode error; count it or assert the worst case vs MAX_STATS_EXTRA 1016
- [ ] M5 review P-6 (low): link_session.rs ~292 keeps the stored reply after a chord dump drops a load; clear last_req_len in start_dump
- [ ] M5 review P-7 (low): B1 active-high polarity assumed; open until the human's button check
- [x] (e278521, 83b82b5: sampled readback in player-script, `stats display readback`; run-replay.sh requires readbacks >= 1, mismatches=0, control 2; docs/reference/player.md "Panel readback") M5 review gate note (low): proposed B3 wants display readback 0 mismatches; player-script does no readback and run-paced.sh does not check it
- [x] (8b3a2e6) M5 review T-2 (medium): gbc-pc syncs the RTC footer to real time on `load --sav` and restamps dumps (desktop rule shared as `gbc_firmware::save::Clock::sync`, fb94fda); `--raw-save` for the byte-identity round trips (ADR 0040)
- [x] (7569334) M5 review P-1 (medium): the player's live stack depth was unmeasured and unguarded. `stack_used` is now in the live Stats/text fields; measured on the board 12,968 of 22,912 B after boot, a save load (reload), two dumps and Stats (run-player-save.sh PASS), so 9.9 KB of headroom and no reduction needed; the stack guard (M1 F-2) catches a creeping overflow

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

- [x] (ADR 0026 Accepted with a "measured placement" amendment, 2026-11-26: player.elf sections, `nm`, `tools/memory-x`; code 64,080 B of 65,280 B, data split table; open items listed there) ADR 0026 (Proposed): accept or replace it with the measured data placement (DTCM: stack, `Emulator`, `Memory`, tables, `Frame`; AXI: DMA buffers, cart RAM) (Stage 1 B10/C5)
- [x] (fits with the plain tile path dropped: `player` `.itcm` 64,088 B of 65,280; the A/B swaps that keep the plain tile path in ITCM are slower: drop per-event PPU +1.42% worst case, drop APU +2.35%; default unchanged; docs/perf.md "ITCM tier A/B (M6)") ITCM set with the front end present: the set is 68,832 B in the ELF vs 65,280 B usable (plain tile path already dropped, 8,078 B); decide what else leaves (tier order in `docs/perf.md`), measure the cost of each drop; front-end ISRs and the converter stay in flash unless a measurement says otherwise
  - [x] (measured: neither swap is faster, cgb-raster within 0.2%; docs/perf.md "ITCM tier A/B (M6)") Sub-task: the set already fits (`player.elf` `.itcm` 64,080 B of 65,280 B, front-end ISRs and `scale_frame` in flash, whole loop at most 29.7% of the frame budget, 0 over budget; docs/perf.md "Player (M5)"), so nothing else leaves; still unmeasured on the board: the cost of the plain tile path drop itself (A/B of `Renderer::render` in ITCM vs flash on `synth:worst-case` and `cgb-acid2`, needs a build with another tier dropped to make room, e.g. `GBC_ITCM_DROP="per line with objects"`; h7-bench `itcm-dtcm-qspi` or the headless image)
- [x] (none adopted: fat LTO +2.25% worst case, thin +7% on two storms, gbc-core opt 2 slower on cgb-raster/event-storm, "s" +10-29%; LTO also needs symbol-based `.text_core` rules; docs/perf.md "LTO and opt-level (M6)") Measure LTO and `opt-level` variants of the firmware link (inlining changes the globs: `check-itcm-elf.sh` catches it); keep what is faster on the board
- [x] (100 MHz from PLL2 + SIOO/0xA5 enhance mode; no DTR/QPI on the part, DC = 1 rejected; worst case +2.38% to +1.23% QSPI vs internal, storm-gdma +14.4% to +7.6%; docs/perf.md "QSPI tuning (M6)") QSPI tuning: clock (up to 133 MHz SDR if the part allows), DTR/QPI if supported, send-instruction-only-once, prefetch/timeout; measured on `itcm-dtcm-qspi`
- [x] (not triggered, session 27: after QSPI tuning the bound workloads cost at most +1.24% (worst case, 0.5 points of a frame); only `storm-gdma`, outside the bound by human decision, is above 3 points (+7.6% / +15% mean / max); docs/perf.md "QSPI tuning (M6)") Optional, if the QSPI ROM costs > ~3 points on any workload: ROM placement (Stage 1 M8 "ROM placement hook": bank 0 or hot banks in internal SRAM). A core public API change needs an ADR and one reviewer; first try platform-only options (ROM ≤ 1 MiB copied to internal flash bank 2 or SRAM at load)
- [x] (not triggered: worst case 85.6% of the 50% line, report only; docs/perf.md "M6 worst-case report") Optional, if the reported worst case is close to or over the 50% figure and the human wants the gap narrowed (it is not a gate here): B4 u32 epoch-relative clocks (Stage 1 M10 B4); S4/L7 re-measure (`&mut` read path, always-lazy timer); APU cost leads (Stage 1 M8 APU task)
- [x] (not triggered: the storms stay outside the bound; no real game measured near budget, Wario Land 3 busy max 29.7%) Optional: E1 dot-stepped mode 3 in the core, only if the `storm-*` numbers from M0 show the closed-form per-write cost matters for real games (they are outside the bound by human decision)
- [x] (dropped: no mispredict counter in the DWT; ADR 0042's predictor-off diagnostic (ACTLR) bounds the predictor's effect instead) Optional: B9 dispatch-mispredict measurement. **Note:** the Cortex-M7 DWT has CYCCNT/CPICNT/LSUCNT/FOLDCNT/EXCCNT/SLEEPCNT but, as far as I know, no branch-mispredict counter (unverified), so this needs an indirect method or is dropped
- [ ] Deferred to Stage 3 unless a measurement here needs it: E5/D11 CPU fetch rework (touches `Cpu::run`; wants the real-game frame-golden net, Stage 1 M10 E6, which needs the human's recordings); C10 recursion split (only for a provable static stack bound; measured stack is 2,952 B)
- [x] (recorded at main a6a2995 in the main checkout, `bench-itcm-dtcm-pll480`, both passes; a second boot passes `check-cycles.sh`, largest boot-to-boot |delta| 0.047%; docs/perf.md "M6 worst-case report") On-board cycle baseline: `tools/h7-bench/expected-cycles.txt` (per workload, per placement, 472 MHz); the gate (step B2, M7 work; no gate step runs `check-cycles.sh` yet) fails at > +3% over it. Counts do not repeat exactly: the margin over layout noise for a core change on the bound set is about 1.3-1.8 points, and storms can exceed 3% from layout alone (ADR 0042 "Amendments"; review finding P3 is open for the human). It is a regression ratchet, not an absolute bar. Absorbing a +3% failure needs the human, as for `check-bench.sh`
- [x] (QSPI max 3,382,280 = 85.6% of 3,951,279, 42.8% of a frame; H735 projection 73.5% of the 550 MHz 50% line; docs/perf.md "M6 worst-case report (shipped placement)") Report, in `docs/perf.md` and the log: `synth:worst-case` max cycles per frame at 472 MHz with ROMs in QSPI and the shipped placement, as a percentage of the 3,951,279 figure (and the projected margin on the H735 at 550 MHz, labelled as a projection)

- [ ] M6 review P6 (low): the cycle ratchet guards the h7-bench image, not `player` (their `.itcm` differs by 56 B); gate B2 (M7) still has to run `check-cycles.sh` (B2 runs check-cycles since session 28; the bench-vs-player gap stays open)
- [ ] M6 review P7 (low): ITCM has 1,192 B free in `player`; 57 ITCM->flash thunks (570 B) move with layout; the next drop in the tier order (per-line object path) is unmeasured on the board
- [ ] M6 review P9 (low): add `.text.__aeabi_memmove*` to the ITCM builtins rule (tools/memory-x/src/lib.rs:253); today ITCM callers go ITCM -> flash -> ITCM through two thunks; changes the ITCM layout, so do it with the next baseline refresh
- [ ] M6 review T4 (low): the h7-bench banner doesn't carry the ITCM drop (`H7_BENCH_ITCM_DROP`), and the capture behind `expected-cycles.txt` isn't archived in the repo (can't re-audit)
- [ ] M6 fix re-review L1 (low): `check-cycles.sh --update`'s dirty check misses `tools/bench/src/synth.rs` (h7-bench build.rs pulls it in) and the `H7_BENCH_ITCM_DROP` / `H7_BENCH_CPU_INSTRS` env vars
- [ ] M6 fix re-review L2 (low): the dirty check also sees `expected-cycles.txt` and captures under `tools/h7-bench/`, so a second refresh before a commit records `+dirty` falsely
- [ ] M6 fix re-review L3/L4 (low): the regression guard runs only when the out file exists (`rm` then `--update` absorbs a regression unflagged), and `--absorb-regression` leaves no mark in the written file; consider a `absorbed=` header field and comparing against `git show HEAD:` when the file is missing

**Acceptance:** on the board at 472 MHz with ROMs in QSPI and the shipped
placement, `synth:worst-case` max cycles per frame is **measured and
reported** against 3,951,279 (not gated). Full speed is gated: in the
`player` image with display, audio and input running, the whole loop
fits real time (frame time < 1/59.7275 s, under 7,902,557 cycles), with 0
frames over budget in 10 minutes of `synth:worst-case`. The cycle baseline
is committed. The performance review (AGENTS.md) is required at this
milestone's close.

**Met (session 27, 2026-11-26; tag `stage2-m6`):** `player-script`
10-minute `synth:worst-case` from the NOR: PASS, 0 over budget, 0
underruns, core max 3,390,856 (42.9% of a frame; 85.8% of the 3,951,279
report line); live `player` 640 s: 0 over budget, 0 underruns
(`docs/reference/m6-evidence.md`). Baseline `tools/h7-bench/expected-cycles.txt`
committed (a9ea350); bench worst case from QSPI 85.6% of the report line
(H735 projection 73.5%, labelled). Review: `docs/reviews/stage2-m6.md`
(no high; mediums fixed, P3 to the human); `--host-only` exit 3.

Carries over: the placement method and checks (on the H735 the rule
covers all of gbc-core), measured miss costs, and the LTO findings. The
ITCM tiering does not carry over (the H735 puts the whole core in ITCM).

## M7: Stage 2 review and sign-off

- [x] Full `gate-stage2.sh` (host and board) passes (session 28, exit 0, 2676 s; first version awaits the human's approval)
- [x] Independent reviews in parallel (AGENTS.md): **platform correctness** (register sequences against RM0433, DS12117, ES0392, UM2407, UM2905, ST7789V: clock/power, MPU, DMA/cache coherence, QSPI, FMC timings, ISR races), **test integrity** (the on-board comparison and gate logic, any way a missing board or a garbled serial line can pass, determinism, nothing new in the core), **performance and portability** (front-end cost per frame, ITCM/DTCM use, what will and won't move to the H735)
- [x] Turn lows into one-line Stage 3 plan tasks; write the carry-over list into `docs/architecture.md` "Targets"
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
  rustdoc, `cargo test` (now including `gbc-firmware` and `gbc-link`), no-alloc and hard
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
- **H7.** Host unit tests (`gbc-firmware`, `gbc-link`, `tools/gbc-pc` host logic) for the converter, DC blocker, debounce,
  serial framing (ROM, save, input) and image header (inside H2's `cargo test`; named
  explicitly so deleting them fails the gate).
- **H8.** Firmware size report: `.itcm`, DTCM and AXI use, and stack
  paint depth from the last board run. Report only.

Board steps (serialized under the board lock; each flash is followed by
reading the serial port and **ignoring the lines before the new banner**):
- **B1.** `selftest-pll480` (image name kept; banner `clock=pll472`): `cpu_hz=472000000`, VOS0/ODEN read back,
  `actvosrdy=1`, the expected `flash_acr`, constant `cycles=`, and an
  equal `tcmprobe`.
- **B2.** `bench-itcm-dtcm-qspi-pll480` (and the internal-flash variant):
  every workload's `calls/frames/clocks/hash` equals
  `expected-hashes.txt`. Every workload ≤ `expected-cycles.txt` + 3%.
  Every sign-off workload (`synth:worst-case` and the replay ROMs) has
  max cycles per frame < 7,902,557 (real time). `synth:worst-case` max vs
  3,951,279 is printed as a **report line, not a check** (human,
  2026-11-25).
- **B3.** Load the replay set into QSPI (skipped if the hashes match).
  Then the `player` image in scripted mode: each ROM's frame-hash list
  and DAC-feed hash equal the host's, the display readback has 0
  mismatches, and the run ends with 0 underruns and 0 frames over budget.
  (The readback is the image's own: a full readback takes 90 ms, so it
  samples by a fixed rule, the first frame shown at or after frame 120
  and then one at least every 300 frames, each read back in 1.7 ms steps
  between frames after a sentinel; every entry needs >= 1 readback, 0
  mismatches and a control of exactly 2; src/display.rs "Readback
  samples".)
- **B4.** A 10-minute `synth:worst-case` soak in the `player` image: 0
  frames over budget (budget = one real-time frame, 7,902,557 cycles), 0
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
cpu_mhz = 472
frame_budget_cycles = 7902557          # real time at 472 MHz; gated
worst_case_report_cycles = 3951279     # 50% of a 472 MHz frame; REPORTED, not gated
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
| D6 | Saves on the dev board | (a) none persistent; (b) QSPI save area; (c) internal flash bank 2; (d) over the PC serial link | **Decided (human, 2026-11-25): (d), not QSPI.** `tools/gbc-pc` sends the `.sav` (cart RAM plus the RTC footer, the desktop format, ADR 0023) with the ROM, and the board dumps it back on request or a button combo. Folded into M5; the old M7 is gone. Acceptance: a save round-trips byte-identically, PC → board → PC (B5) |
| D7 | Full-speed sign-off workloads | `synth:worst-case` (bench, ROM in QSPI, 472 MHz) plus real games for the human: Shantae and Alone in the Dark (heavy HDMA), Wario Land 3, LADX, and Pokémon Crystal (RTC) | **Accepted.** Gated: each finishes its frames inside real time (< 1/59.7275 s). `synth:worst-case` vs 50% and the real-game core cycles from the `player` statistics are reported, not gated. They become gated replays if the human records inputs (Stage 1 M10 E6) |
| D8 | Cortex-M4 | — | **Note: not applicable**: the H753ZI is single-core (the dual-core H755 is retired), and the Stage 3 H735 is single-core too |
| D9 | Dependencies for the new crates | AGENTS.md lets agents decide dependencies for `gbc-desktop` and `tools/*` only | **Accepted:** extend that to `gbc-nucleo-h753` (MIT/Apache crates: `cortex-m`, `cortex-m-rt`, `stm32h7`, as h7-bench already uses) and keep `gbc-firmware` (only `gbc-core`) and `gbc-link` (none) dependency-free; `tools/gbc-pc`: `gbc-link` plus a serial-port crate, chosen by the agent under the `tools/*` rule. **Decided (human, 2026-11-25): no `serialport` (MPL-2.0); `gbc-pc` talks to the COM port directly through `windows-sys` (MIT/Apache-2.0).** `gbc-handheld`: decide when it exists |
| D10 | `acceptance/stage2.toml` | the proposal above | **Accepted:** the human creates it (or edits the proposal). Agents won't create it |
| D11 | Dev-board clock | (a) 480 MHz from the ST-LINK MCO (measured 483.9 MHz, out of spec); (b) **472 MHz from the HSI**; (c) the HSI trimmed against the LSE at boot to about 478-480 MHz; (d) fit an HSE crystal | **Decided (human, 2026-11-25): (b).** PLL1 = HSI 64 MHz / 4 x 59 / 2 = 472 MHz nominal (measured 473.68 MHz), AXI/AHB 236, APB 118, flash 4 WS, VOS0 via ODEN. At the HSI's worst case (+1.5%) the CPU is at most 479.1 MHz. Figures: real-time frame **7,902,557 cycles**, 50% line **3,951,279** (commits 0a1d52b, 761049b, a5502f4; h7-bench README) |

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
2. **480 MHz is scaled, not measured.** (M0 has since measured it, at the 480 MHz MCO clock, then at 472 MHz HSI: D11.) The 40.9% / 41.3% figure assumes
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
quad-enable bit. VCP baud rates above 115200.
The HSE/MCO finding is settled (D11). An ST app-note citation for the M7 MPU setup of
the QSPI and FMC regions. Unaligned loads from the QSPI window (tested in
M2). Whether the Cortex-M7 DWT has a branch-mispredict counter (believed
not). A non-Zio pin plan for SAI. Whether the `*-pll` bench images run
reliably on the H753 after the 2026-11-25 USB drop.

---

## Hardware sign-off checklist (human)

| # | Check | How | Result |
|---|---|---|---|
| 1 | Display | cgb-acid2 and dmg-acid2 look like their references; a game in motion looks acceptable (tearing, judder) | Partial (human, 2026-11-26): Wario Land 3 looked okay; colour test photo correct (order, orientation, not inverted). cgb-acid2/dmg-acid2 look not yet done. |
| 2 | Audio | `cgb_sound`'s tones plus a game's music: no crackle or drift, right pitch | |
| 3 | Input | every Game Boy button works on the input-test screen and in a game, with the joystick for the D-pad and PC input for the rest (and for all 8 on their own) | Done (human, 2026-11-26): buttons worked, PC input too. |
| 4 | Real games at full speed | about 10 minutes each: Shantae, Alone in the Dark, Wario Land 3, LADX, Pokémon Crystal (save load and dump over the serial link, D6) | |
| 5 | Power cycle | unplug and replug USB, then the banner shows `supply=ldo actvosrdy=1` and VOS0 (the supply write is only really exercised by a power-on reset) | |

## Stage 3 carry-over tasks (from M7 and open M1-M6 lows)

From the M7 review (`docs/reviews/stage2-m7.md`):

- [ ] (M7 C3) `fault::die()`: quiesce IRQs and DMA and clear `CR3.DMAT` before the FAULT line
- [ ] (M7 C4) DAC/SAI underrun: re-init per RM0433 26.4.8 (RM0468 for the H735)
- [ ] (M7 C5) `link_uart` RX overflow (ring lap) detected and counted
- [ ] (M7 C6) check NDTR reads against posted writes (ordering)
- [ ] (M7 C7) fix the SAFETY comments that assume "no interrupt" (qspi.rs regs/`Mapped::exit`, mpu `qspi_window`/`fmc_window`)
- [ ] (M7 C8) one DS revision for datasheet citations (qspi.rs rev Y vs clock.rs rev V)
- [ ] (M7 C9) ES0392 2.2.30: PC13 vs LSE during clockcheck
- [ ] (M7 platform) set NVIC priorities; decide IWDG policy; test the stack guard vs the largest frame and MLSPERR; fault-inject DMAUDR/MDMA TEIF/USART errors; NCS/CLK float before init; temperature/VDD corners
- [ ] (M7 T8) boot nonce so a gate can tell a fresh board run from a stale capture
- [ ] (M7 T9) board loss after the lock: gate exit 2 vs 1 (never a pass; make it consistent)
- [ ] (M7 T11) tests for the gate's own board-step parsing
- [ ] (M7 P3, medium, accepted) bench with a realistic footprint: >= 1 MiB streaming synth ROM and a cart-RAM variant, QSPI A/B, LSUCNT; the H735 HyperRAM miss path is what matters
- [ ] (M7 P4, medium, accepted) make the `gbc-firmware` audio sample format generic when SAI lands (ADR 0036: extract when duplicated)
- [ ] (M7 P5) move the ST7789V pixel format (video.rs:54-60) out of the shared crate
- [ ] (M7 P6) scaler is 196k cycles: try an STRD-based store path
- [ ] (M7 P7) set NVIC priorities (see platform item above; one task)
- [ ] (M7 P8) player stack: 19,800 of 22,912 B scripted; move the 4 KiB clock table (player.rs:118) off the stack
- [ ] (M7 P9) fe regression bound (B4 only checks < 100%); ratchet sits on +-1.7% noise; guard `player`, not just the bench
- [ ] (M7 P10) p99 histogram range below a 550 MHz frame (player.rs:60-63); `Elapsed` wraps after 7.8 s at 550 MHz (paced.rs:517)
- [ ] (M7 carry-over) H735 items: AXI 394,240 vs 262,144 B, memory-x chip and `CORE_TEXT_OFFSET` assert, ITCM reserve 3,854 B, HyperRAM rebaseline, 550 MHz projection, crystal SAI pacing clock (see architecture.md "Carry-over to Stage 3")

Still open from M1-M6 (copied, also kept in their milestones):

- [ ] (M1 low) T-4: generated expected files (`tools/headless/expected/*`, `expected-hashes.txt`) are checked against the code that made them; record ownership in an ADR note or `acceptance/README.md`
- [ ] (M1 low) T-8: gate H8 picks the newest gbc-core rlib, not the one matching the ELF's crate hash
- [ ] (M1 low) F-3: ITCM mem builtins only valid after `tcm::place()`; a fault before it locks up silently
- [ ] (M1 low) F-5: `.axisram` statics declared `[0; N]`, use `MaybeUninit`
- [ ] (M1 low) F-6: `.dtcm_rodata` has no ASSERT; no alignment ASSERT on `__siitcm`/`__sidtcm_ro`
- [ ] (M1 low) F-7: `clockcheck.rs` leaves PWR_CR1.DBP set; stale `asm::delay` comment in boot.rs
- [ ] (M2 low) H-2: `qspi::FORBIDDEN` blocks only 0x60/0xC7/0x01; allow-list read opcodes
- [ ] (M2 low) H-3: `Qspi::init` never resets the NOR or leaves 0xEB enhance/QPI mode
- [ ] (M2 low) H-4: FIFO may hold prefetched bytes after the EN=0 exit fallback; assert FLEVEL == 0 or ABORT again
- [ ] (M2 low) H-5: `Qspi::init` has no once-guard, `Mapped` no Drop
- [ ] (M2 low) T-5: loader.md two lock takes; `GBC_BOARD_LOCK_HELD=1` runs gbc-pc's cargo build outside `GBC_BUILD_LOCK`
- [ ] (M3 low) T-3: LCD readback compares GRAM with the DMA buffer, not the core frame; add a sampled `scale_frame` check
- [ ] (M3 low) T-4: perf.md says the display "costs the core nothing measurable"; reword (te-wait +0.07% vs 0.054% noise) and note the TE ISR's RAMWR+SWRQ
- [ ] (M3 low) P-3: `lcd::Timing::new` doesn't model trcsfm 355 ns / tcsf; record the assumed reading
- [ ] (M3 low) P-5: panel init sends no VCOM/gamma/power registers and no INVON; compare with ST's BSP init
- [ ] (M3 low) P-6: `Lcd::set_timing` resets FMC while PD4/PD5/PD7 stay AF12; drive them as GPIO high around the reset (lcd-probe only)
- [ ] (M4 low) P-5: DBGMCU_CR survives system reset, DBGSLEEP_D1 stays set; don't rely on DWT in sleep on Stage 3
- [ ] (M4 low) P-7 remainder: RING/TAP statics can't be reset; a second `paced::run` in one boot fails NotFresh
- [ ] (M5 low) T-4: last frame's over-budget never counted (frames=35836 of 35,837)
- [ ] (M5 low) T-5: run-player-save.sh and run-save-roundtrip.sh don't check the flashed image cksum
- [ ] (M5 low) T-8: `.gitignore` `!tools/gbc-pc/tests/fixtures/*.sav` exception
- [ ] (M5 low) P-2: 64 KiB player save limit comes from section order; too-big dump answers Nak UnexpectedType
- [ ] (M5 low) P-4: link_uart `read()` can't detect a ring lap > 4 KiB or a dead RX stream (RX TEIF never read)
- [ ] (M5 low) P-5: player.rs ~443-446 drops a Stats report silently on encode error
- [ ] (M5 low) P-6: link_session.rs ~292 keeps the stored reply after a chord dump drops a load; clear `last_req_len` in `start_dump`
- [ ] (M5 low) P-7: B1 active-high polarity assumed until the human's button check
- [ ] (M5 low) gate note: B3 display readback of 0 mismatches is not checked by player-script or run-paced.sh
- [ ] (M6 low) P6: ratchet guards the h7-bench image, not `player`; wire gate B2
- [ ] (M6 low) P7: ITCM has 1,192 B free in `player`; 57 thunks (570 B) move with layout; next ITCM drop unmeasured
- [ ] (M6 low) P9: add `.text.__aeabi_memmove*` to the ITCM builtins rule (tools/memory-x/src/lib.rs:253), with the next baseline refresh
- [ ] (M6 low) T4: h7-bench banner lacks `H7_BENCH_ITCM_DROP`; capture behind `expected-cycles.txt` not archived
- [ ] (M6 low) re-review L1: `check-cycles.sh --update` dirty check misses `tools/bench/src/synth.rs` and the `H7_BENCH_*` env vars
- [ ] (M6 low) re-review L2: dirty check also sees `expected-cycles.txt` and captures, so a second refresh records `+dirty` falsely
- [ ] (M6 low) re-review L3/L4: regression guard only runs when the out file exists; `--absorb-regression` leaves no mark in the file
