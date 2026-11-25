# h7-bench: Cortex-M7 cycles per emulated frame

A bare-metal measurement spike for the **NUCLEO-H755ZI-Q** (MB1363,
STM32H755ZIT6). It runs the bench workloads of `tools/bench` on the board's
Cortex-M7, counts CPU cycles per frame with the DWT cycle counter and prints
the results on the ST-LINK virtual COM port. The figures replace the x86
instruction-count proxy of `docs/perf.md` (the bar: 50% of the CPU in the
worst case).

**Status: measured on the board, 2026-11-24.** All three placements run at
both clocks and reproduce the PC's hashes; see "Results". The selftest image
is still the one to flash first on a board that has not run this before.

Not part of the root workspace: `cargo build` at the repo root does not see
it.

## Images

`./build.sh` writes these to `out/` (ELF and `.bin`; not committed):

| Image | Features | What it does |
|---|---|---|
| `selftest` | `selftest` | Supply, LEDs, UART, caches, cycle counter. No emulator. **Flash this first.** |
| `bench-flash-axi` | none | Code in flash behind the I-cache; stack, emulator state and buffers in AXI SRAM behind the D-cache. |
| `bench-flash-dtcm` | `dtcm` | Code in flash; stack, `Memory` and the core's lookup tables in DTCM; frame, audio and cart RAM buffers in AXI SRAM. |
| `bench-itcm-dtcm` | `itcm` | As `flash-dtcm`, plus the hot set of `perf/itcm-sections.txt` in ITCM (docs/perf.md "ITCM placement plan"). |
| `*-pll` | `+pll` | Phase B clock (392 MHz), only with `H7_BENCH_PLL=1`. Flash these last. |
| `m4-stub` | (own crate) | Optional Cortex-M4 image, see "Dual core". Not needed. |

The ROMs are built in: the four synthetic ones from the bench's generator
(`tools/bench/src/synth.rs`, compiled into `build.rs`) and blargg's
`cpu_instrs.gb`, read at build time from `H7_BENCH_CPU_INSTRS` (default: the
repo's `test-roms/`). The build fails with a message if it is missing.

### Placements and sizes

Built 2026-11-24 (bytes; `build.sh` prints the current figures):

| Image | `.bin` | `.itcm` | `.text` (flash) | `.rodata` | DTCM (`.dtcm_rodata` + `.bss`) | AXI SRAM |
|---|---|---|---|---|---|---|
| `selftest` | 9,560 | - | 7,588 | 1,308 | - | 4 |
| `bench-flash-axi` | 329,328 | - | 126,716 | 201,944 | - | 50,996 + 58,368 |
| `bench-flash-dtcm` | 329,504 | - | 126,892 | 199,640 | 2,304 + 50,996 | 58,368 |
| `bench-itcm-dtcm` | 330,552 | 64,664 | 63,280 | 199,640 | 2,304 + 50,996 | 58,368 |

`.rodata` is mostly the five ROM images (192 KiB). The stack starts at the
top of DTCM (0x2002_0000), or at 0x2404_0000 in `flash-axi`; the deepest
use measured is 2,952 B (the `stack used=` line).

**The ITCM set of `perf/itcm-sections.txt` does not fit 64 KiB**: its
functions are 68,832 B in this build. The build therefore leaves the tier
the file names "first to drop" in flash, the plain tile path
(`Renderer::render`, `fill_tiles`, 8,078 B); `H7_BENCH_ITCM_DROP` chooses
other tiers by a substring of their `# Tier` header. In ITCM now: 101
gbc-core functions (61,284 B), the memory routines they call
(`__aeabi_memcpy*`, `__aeabi_memclr*`, `__aeabi_memset*`), and the linker's
long-branch thunks to flash; 64,664 of 65,280 B (ITCM is used from 0x100,
so nothing sits at address 0). Five globs match nothing in the ELF because
the linker removed the functions as unused (`run_frame`, `run_cycles`,
`halt_begin`, `halt_end`, `idle_halted`).

Flash the `.bin`, not the ELF: the ITCM ELF has a load segment at address
0x100 whose load address is in flash, which only the `.bin` flattens.

## Build

```
./build.sh                      # all images, the M4 stub, the host hash test
H7_BENCH_PLL=1 ./build.sh       # also the 392 MHz variants
H7_BENCH_QEMU=1 ./build.sh      # also the QEMU smoke run (qemu-system-arm)
H7_BENCH_LOCK=<file> ./build.sh # every cargo command under flock (agents)
```

Needs only the pinned toolchain (`rust-objcopy` ships inside it) and
binutils' `readelf`. The script prints the section sizes of each ELF, the
first two vector words of each `.bin`, and checks: the initial SP is in RAM
and 8-byte aligned; the reset vector is a Thumb address inside the image; the
image fits flash bank 1; in the ITCM image every glob of
`perf/itcm-sections.txt` that is still linked has its function below
0x0001_0000 (the linker itself fails if ITCM or DTCM overflows); the host
command's lines equal the committed `expected-hashes.txt`
(`host/`, `cargo test`).

With `H7_BENCH_QEMU=1` the same frame loop, built for thumbv7em, runs on
QEMU's Cortex-M7 board (`mps2-an500`, semihosting output, no display) and
must print exactly `expected-hashes.txt`; once plain, and once with the hot
set copied to a stand-in ITCM out of branch range, which exercises the
linker rules, the copy loop and the thunks (without the copy the run hangs:
checked 2026-11-24). QEMU has no cycle counter, caches or STM32
peripherals, so nothing in `src/board.rs` is covered by it.

## Flash

```
./flash.sh out/selftest.bin
```

It copies the `.bin` to the ST-LINK mass-storage drive (`E:`, mounted at
`/mnt/e`, label `NOD_H755ZIQ`) with a native Windows `copy`; the drive
programs it at 0x0800_0000 and resets the target. It checks the image's
vectors, the drive's label and files first, and reports the `FAIL.TXT` the
drive writes on an error. Close the serial port first. The M4 stub cannot
be flashed this way.

## Read the output

```
./read-serial.sh 20      # COM3, 115200 8N1, for 20 s
```

Expected, selftest (one line a second, the banner every 8 lines):

```
h7bench v1 banner board=NUCLEO-H755ZI-Q image=selftest placement=flash-axi clock=hsi64 cpu_hz=64000000 baud=115200
h7bench v1 power cr3=0x........ csr1=0x........ d3cr=0x........ actvosrdy=1 supply=direct-smps
h7bench v1 clock rcc_cr=0x........ cfgr=0x........ d1cfgr=0x........ flash_acr=0x.. rsr=0x........
h7bench v1 cpu cpuid=0x411fc27. idcode=0x........ icache=1 dcache=1 cyccnt=1 itcm_bytes=0 dtcm_table_bytes=0
h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 msp=0x2403ff60
h7bench v1 dualcore optsr=0x........ bcm7=1 bcm4=1 boot_c2=0
h7bench v1 selftest n=0 delay=8000000 cycles=8000020 cyccnt=... stack_used=368
h7bench v1 tcmprobe want=0x5a17c0de itcm=0x5a17c0de dtcm=0x5a17c0de
```

`cycles=` is 8,000,008 to 8,000,020 on this board, at both clocks.

Expected, bench (the workloads run once, about a minute at 64 MHz; then the
whole report repeats every 3 s forever):

```
h7bench v1 banner ... image=bench placement=itcm-dtcm ...
h7bench v1 start worst-case calls=120
h7bench v1 worst-case calls=120 frames=... clocks=... min=... mean=... max=... hash=................
...
h7bench v1 cpu_instrs calls=600 frames=... clocks=... min=... mean=... max=... hash=................
h7bench v1 stack used=2952 of=77772
h7bench v1 end pass=1
```

`min`/`mean`/`max` are CPU cycles per `run_until` call of one frame
(`FRAME_CLOCKS`); 50% of a 59.73 Hz frame is `cpu_hz / 59.73 / 2` cycles
(4.02 M at 480 MHz, docs/perf.md). `calls`, `frames`, `clocks` and `hash`
must equal the line of the same workload in `expected-hashes.txt`: the hash
folds every call's clocks, events and audio samples and every completed
frame, so a match means the board computed exactly what the PC does.

LEDs: LD1 (green) on from the first instructions of `main`, toggling while
it works (every 8 frames; 1 Hz toggle in the selftest; 2 Hz after the
report); LD2 (yellow) on once bring-up is through; LD3 (red) blinking fast
in antiphase with LD1 is a fault (panic, HardFault, unexpected exception),
with a `h7bench v1 FAULT ...` line on the UART every second.

## Measurement

- DWT `CYCCNT`, read right before and right after each call. No interrupt is
  enabled, so nothing else runs in the window. Hashing (which walks the
  46 KB frame buffer and so evicts most of the 16 KB D-cache) and the LED
  are outside it; a real front end's blit would evict the cache likewise.
- The calls are the bench's (`run_emulation` in `tools/bench/src/main.rs`):
  `run_until(clock + FRAME_CLOCKS)` with a 1024-sample audio buffer.
- Phase A runs from the 64 MHz HSI with nothing clock-related written.
  Cycle counts for code and data in ITCM/DTCM do not depend on the clock
  frequency. Everything that crosses the AXI bus does:
  - Flash. Phase A leaves `FLASH_ACR` at its reset value (the banner prints
    it; RM0399 sections 4.3.8 and 4.9.1: 0x37, 7 wait states), so a flash
    read costs 8 flash clocks = 8 CPU cycles, since CPU and AXI clocks are
    equal. At 392 MHz the AXI clock is 196 MHz, which needs 2 wait states at
    VOS1 (RM0399 Table 16): 3 AXI clocks = 6 CPU cycles. So phase A slightly
    overstates the cost of an I-cache or D-cache miss to flash (code outside
    ITCM, and the ROM image) compared with phase B. One wait state would be
    legal at 64 MHz/VOS3 and would understate it (2 cycles), which is why
    the reset value stays.
  - AXI SRAM: in phase B the AXI clock is half the CPU clock, so a D-cache
    miss costs more CPU cycles than in phase A. The `flash-axi` image is the
    most exposed, `itcm-dtcm` the least.
  Phase B exists to measure exactly these differences.

## Board facts and their sources

UM2408 Rev 6 (board), RM0399 Rev 5 (MCU), DS12919 Rev 3 (STM32H755xI
datasheet). The code comments in `src/board.rs` carry the same citations.

### Supply (the item that can lock the board)

- UM2408 section 7.4.8, Table 10: the board's default is "Supply config 2:
  Internal SMPS only" (SMPS on, LDO off). Its warning: firmware must use
  `HAL_PWREx_ConfigSupply(PWR_DIRECT_SMPS_SUPPLY)`; with the wrong setting
  "after the reset, STLINK cannot connect to the target anymore".
- RM0399 section 7.4, Table 33, row 2 "Direct SMPS step-down converter
  supply": SDEXTHP = 0, SDEN = 1, LDOEN = 0, BYPASS = 0 in `PWR_CR3`
  (section 7.8.4: BYPASS bit 0, LDOEN bit 1, SDEN bit 2, SDEXTHP bit 3,
  SDLEVEL bits 5:4).
- The firmware does `PWR_CR3 = (PWR_CR3 & !0x3F) | 0x04`, the same
  read-modify-write as ST's HAL, as the first instructions after reset
  (`__pre_init`, in assembly, no RAM access: RM0399 section 7.4.1 forbids
  RAM writes until `ACTVOSRDY`), then waits for `PWR_CSR1.ACTVOSRDY` with a
  bound.
- `PWR_CR3`'s low byte is write-once after a power-on reset. After a flash
  through the ST-LINK (a system reset, not a power-on) the value from the
  first write since power-up stays, so **the supply write is only really
  exercised by a power cycle**: unplug and replug USB after the first
  selftest and check the banner again (`supply=direct-smps actvosrdy=1`).
- Not verified: that this particular board still has the default solder
  bridges (Table 10: SB92, SB79, SB75, SB76, R35 fitted for config 2).

### Clock and voltage scale

- DS12919 Table 13 and Table 14 (note 1: "VOS0 (power scale 0) is not
  available when the SMPS step-down converter directly supplies VCORE"):
  the highest scale on this board is VOS1, limit 400 MHz. 480 MHz is not
  reachable without rework to LDO supply.
- Phase B: PLL1 from HSI, 64 MHz / 4 x 49 / 2 = 392 MHz (the HSI's
  tolerance must stay under 400), AXI/AHB 196 MHz (also the M4's clock,
  limit 200), APB 98 MHz, flash 2 wait states with WRHIGHFREQ = 2.

### Dual core

Both cores boot at reset (option bytes BCM7 = BCM4 = 1, read by the human;
RM0399 section 2.6 Table 8), the M4 from 0x0810_0000 (Table 9). Option bytes
are not changed, and the M7 cannot hold the M4: RM0399 section 9.4.7 "Hold
boot function" (Table 58) only applies when a BCMx option bit is 0.

- **M4 bank erased (the board's state after the full chip erase).** The M4
  loads SP = PC = 0xFFFF_FFFF from the erased vector table, faults on its
  first fetch, finds an erased HardFault vector and enters lockup. Lockup
  is inert here: its only connection is the optional timer break input
  (`SYSCFG_CFGR.CM4L`, RM0399 section 13.3.6, off at reset); it causes no reset. The M4 touches no
  peripheral, clock or power setting. This firmware never waits for the M4
  and never uses the hardware semaphore.
- **A stale M4 image** (not the case now) could do anything to the shared
  clock tree, GPIO and power registers. ST's dual-core templates stop the M4
  in D2 Stop until the M7 signals HSEM, which this firmware never does, but
  an unknown image deserves no trust: erase the chip, as was done.
- **`m4-stub`** (64-byte vector table plus `wfi` in a loop, no RAM, no
  peripheral access) parks the M4 in Sleep instead of lockup. It makes the
  M4's state tidier for a debugger, nothing more; it needs
  STM32CubeProgrammer at 0x0810_0000 and is one more thing that can be
  wrong. Recommendation: leave the M4 bank erased.
- USART3 is in the D2 domain (the M4's). The M7 enabling its clock
  allocates it to CPU1, which keeps D2 clocked (RM0399 section 9.5.10); the
  firmware waits for `RCC_CR.D2CKRDY` before it touches the UART.

### Pins

- Virtual COM port: USART3, PD8 = TX, PD9 = RX, AF7 (UM2408 section 7.10 and
  Table 12, default SB16/SB17 on; DS12919 alternate functions). Only TX is
  used. 115200 8N1.
- LEDs, active high (UM2408 section 7.6): LD1 green PB0 (default SB54 on,
  SB65 off), LD2 yellow PE1, LD3 red PB14.

## Recovery

If the board stops responding after a flash (no LED activity, the ST-LINK
cannot connect, `FAIL.TXT` on the drive):

1. In STM32CubeProgrammer choose mode "Under reset" (connect under reset)
   with "Hardware reset", connect, and do a full chip erase.
2. If that cannot connect (the supply deadlock of UM2408 section 7.4.8):
   power the board off; wire CN11 pin BT0 (BOOT0) to VDD; power on (the MCU
   starts the system bootloader instead of the user flash); connect with
   STM32CubeProgrammer; erase the user flash; power off; remove the wire.
3. Power-cycle (unplug USB) after any recovery: `PWR_CR3` only resets on a
   power-on reset.

## Results (2026-11-24, this board)

CPU cycles per frame-sized `run_until` call, mean / max over the calls of
each workload (120; cpu_instrs 600). Every run printed the hashes of
`expected-hashes.txt`. Core at `main` bcd881e plus this crate.

64 MHz HSI (`hsi64`: VOS3, flash 7 wait states, AXI clock = CPU clock):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` |
|---|---|---|---|
| worst-case | 4,268,540 / 4,318,514 | 4,269,141 / 4,316,296 | 3,093,762 / 3,130,103 |
| cgb-raster | 2,733,452 / 2,785,631 | 2,812,219 / 2,861,092 | 2,523,885 / 2,562,492 |
| dmg-raster | 2,128,850 / 2,160,853 | 2,115,847 / 2,144,013 | 1,814,678 / 1,836,588 |
| event-storm | 2,137,949 / 2,165,333 | 2,182,405 / 2,200,172 | 1,951,620 / 1,966,095 |
| cpu_instrs | 1,407,257 / 1,945,378 | 1,388,040 / 1,930,482 | 1,210,401 / 1,683,198 |

392 MHz PLL (`pll392`: VOS1, flash 2 wait states, AXI clock = CPU clock / 2):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` |
|---|---|---|---|
| worst-case | 4,444,323 / 4,496,832 | 4,383,420 / 4,435,230 | 3,100,670 / 3,137,337 |
| cgb-raster | 2,754,115 / 2,806,027 | 2,804,392 / 2,850,104 | 2,530,564 / 2,569,295 |
| dmg-raster | 2,183,739 / 2,216,313 | 2,150,011 / 2,179,232 | 1,815,665 / 1,837,309 |
| event-storm | 2,153,336 / 2,178,869 | 2,195,649 / 2,213,436 | 1,952,834 / 1,967,362 |
| cpu_instrs | 1,418,142 / 1,965,786 | 1,398,842 / 1,951,152 | 1,210,549 / 1,683,911 |

- A frame at 392 MHz is 6,563,140 cycles (59.7275 Hz). The worst case in
  `itcm-dtcm` takes 47.2% of it on average and 47.8% at most, under the 50%
  bar; from flash it takes 67 to 69%.
- The hot set in ITCM is 1.38x (hsi64) to 1.41x (pll392) faster than flash
  on the worst case and 1.11x to 1.18x on the other workloads, with the
  plain tile path still in flash. DTCM against AXI SRAM for the data makes
  under 3% either way.
- `itcm-dtcm` costs the same number of cycles at both clocks (within
  0.3%): what it leaves in flash and AXI SRAM hardly matters. The flash
  placements cost up to 4% more cycles at 392 MHz.
- Runs are exactly repeatable: a second flash of `bench-itcm-dtcm-pll`
  printed the same cycle counts to the last digit.

docs/perf.md "Measured on a Cortex-M7" relates these to the x86 instruction
counts and to the final chip.

## What the first hardware runs found

- **The DTCM images died in the startup code.** `.axisram` was inserted
  `AFTER .bss` (build.rs). cortex-m-rt takes `__ebss` from the location
  counter after that point, so with `.bss` in DTCM and `.axisram` in AXI
  SRAM the zeroing loop ran from DTCM towards 0x2400_0000 and took a bus
  fault at the end of DTCM (HardFault, IMPRECISERR, PC in `Reset`). The
  AXI-only image merely zeroed more than it needed; QEMU's RAM is
  contiguous. Fixed by inserting the section after `.got`; `build.sh` now
  checks that `__sdata`..`_stack_start` are ordered and inside RAM (seen
  failing on the old script).
- The fault was silent because the handler used the stack it was given and
  a UART that `init` had not set up yet. The HardFault entry now switches
  to its own stack (top of AXI SRAM) and the fault path sets up LEDs and
  UART itself, then prints `h7bench v1 FAULT hardfault sp= exc_return=
  frame= pc= lr= cfsr= hfsr= bfar= mmfar=` once a second.
- ITCM and DTCM are enabled at reset on this part (`ITCMCR` = 0x3b,
  `DTCMCR` = 0x43: EN and RMW set; the banner's `tcm` line), and the
  selftest's `tcmprobe` line writes and reads back a word in each.
- The supply write, the UART, the cycle counter, the caches, the ITCM copy
  and thunks and the 392 MHz clock tree all worked as written (`cr3` reads
  0x00010044, `actvosrdy=1`; at 392 MHz `d3cr=0x0000e000`,
  `cfgr=0x0000001b`, `flash_acr=0x22`).
- Flashing: a `cp` through the WSL mount is unreliable for the bench
  images, hence the native copy in `flash.sh`. The ST-LINK accepted one
  copy and never programmed it while a program held the COM port open.
  After a flash the first lines read are often the previous image's last
  ones (buffered in the ST-LINK), so read for a few seconds and trust the
  lines after the new banner.

## Still not verified

- The supply write after a power cycle with this firmware in flash:
  `PWR_CR3` is write-once per power-on, and these notes do not record
  whether the board was power-cycled between runs.
- That the Cortex-M4 sitting in lockup has no effect (nothing suggests one:
  the cycle counts repeat exactly).
- `m4-stub`: built, never flashed.
- The 400 MHz limit itself: the board ran at 392 MHz from the HSI, whose
  exact frequency was not measured (the UART at 115200 baud works at both
  clocks, which bounds the error to a few percent).

## Flashing order on a board that has not run this

1. `selftest.bin`: LD1 on then toggling at 1 Hz, LD2 on, banner with
   `supply=direct-smps actvosrdy=1 icache=1 dcache=1 cyccnt=1`, one
   `selftest` line a second with a constant `cycles=`, and a `tcmprobe`
   line whose three values are equal. Then unplug and replug USB and check
   the same again (the real test of the supply write).
2. `bench-flash-axi.bin`: the report lines; compare `calls`, `frames`,
   `clocks`, `hash` with `expected-hashes.txt`.
3. `bench-flash-dtcm.bin`, then `bench-itcm-dtcm.bin`: same hashes, lower
   cycle counts; `itcm_bytes=` non-zero in the banner.
4. Only if 1 to 3 are right: `selftest-pll.bin`, then the `*-pll` bench
   images (`clock=pll392`; the hashes must not change).
