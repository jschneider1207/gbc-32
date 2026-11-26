# h7-bench: Cortex-M7 cycles per emulated frame

A bare-metal measurement spike for an STM32H7 Nucleo board. It runs the
bench workloads of `tools/bench` on the board's Cortex-M7, counts CPU cycles
per frame with the DWT cycle counter and prints the results on the ST-LINK
virtual COM port. The figures replace the x86 instruction-count proxy of
`docs/perf.md` (the bar: 50% of the CPU in the worst case).

**Board change, 2026-11-25.** The bench board is now a **NUCLEO-H753ZI**
(MB1364, STM32H753ZIT6, single core, LDO supply; silicon revision V), with
an X-NUCLEO-GFX02Z1 display shield plugged in (unused here). It replaced the
**NUCLEO-H755ZI-Q** (MB1363, STM32H755ZIT6, dual core, SMPS supply) on which
the first results were taken. Until the switch onto the firmware lib
(below) the board was a cargo feature, `h753` or `h755`; see "Boards" for
what differed.

**Status.** NUCLEO-H755ZI-Q, 2026-11-24: all three placements at both
clocks. NUCLEO-H753ZI, 2026-11-25: all three placements at 64 and 392 MHz
with the first five workloads (the 392 MHz runs after the human re-plugged
the board, which had dropped off USB once), all three placements at
"480 MHz" with all fifteen, and then all three at **472 MHz**, the current
`pll480` clock, with all fifteen ("NUCLEO-H753ZI at 472 MHz", the figures
to use). Every bench run reproduced the PC's hashes; see "Results". The
selftest image is still the one to flash first on a board that has not run
this before.

**Clock correction, 2026-11-25.** That "480 MHz" was 483.86 MHz: measured
against the board's 32.768 kHz LSE crystal (`clockcheck`, see "Clock
check"), the ST-LINK's MCO, the HSE reference, is 8.064 MHz, not 8.000,
which put the CPU and AXI clocks 0.8% over their maxima. `pll480` now runs
PLL1 from the HSI at **472 MHz** nominal (473.62 MHz measured; at most
479.1 MHz at the HSI's datasheet tolerance), from a 12.8 MHz reference
in fractional mode so that the PLL input also stays in range ("Boards").
Measured at 472 MHz, the cycle counts are not identical to the 483.86 MHz
run's: 0.984x to 1.036x per workload and placement, 1.002x for the worst
case in `itcm-dtcm` (3,296,548 / 3,334,278 cycles, 41.7% / 42.2% of a
472 MHz frame). The images differ only in the bench crate's clock code,
so the spread is most likely code layout, not the clock (see "NUCLEO-H753ZI
at 472 MHz").

**Bring-up from the firmware lib, 2026-11-25 (ADR 0036).** The board
images link `crates/gbc-nucleo-h753` for everything chip-side: the supply
(`__pre_init`), the 472 MHz HSI clock (plan D11), the UART, the LEDs, the
MPU (new to the bench: ES0392 section 2.1.2's no-access background region,
write-back flash and AXI SRAM), the caches, the TCM copy, the cycle
counter, the LSE clock check and the panic, HardFault and DefaultHandler
(still `h7bench v1 FAULT ...`). `src/board.rs`, `src/tcm.rs` and
`src/stack.rs` are gone, and with them the `h755` feature and the 392 MHz
(`pll`) and 64 MHz HSI images: the H755 is retired (plan D8) and D11 fixes
the clock. Every image runs at 472 MHz and keeps its `-pll480` name. The
report lines are unchanged; the banner is the lib's ("Read the output").
After the switch `bench-itcm-dtcm-pll480` printed the same `calls`,
`frames`, `clocks` and `hash` as before on all fifteen workloads, with
cycles -1.9% to +1.2% off the 472 MHz table ("After the switch onto the
lib"). The text below keeps the removed images' names and figures as
history; `m4-stub/` is kept but no longer built.

**ROMs from the QSPI NOR, 2026-11-25 (plan Stage 2 M2).** The bench images
now also run every workload on a copy of its ROM in the shield's QSPI NOR
(a "bench pack" loaded by the gbc-nucleo-h753 loader), in the same boot as
the internal-flash pass: `itcm-dtcm-qspi`. The worst case costs 2.4% more
from QSPI (3,378,396 / 3,417,095 cycles, 85.5% / 86.5% of the 50% line);
see "QSPI ROM placement" and its result in "Results".

Not part of the root workspace: `cargo build` at the repo root does not see
it.

## Images

`./build.sh` writes these to `out/h753/` (ELF and `.bin`; not committed).
All run at the lib's 472 MHz HSI clock (VOS0); the `-pll480` suffix is the
name the figures below were taken under:

| Image | Features | What it does |
|---|---|---|
| `selftest-pll480` | `selftest` | The lib's bring-up and banner, the cycle counter on a known delay loop, a TCM probe. No emulator. **Flash this first.** |
| `clockcheck-pll480` | `clockcheck` | The selftest's bring-up, then the CPU clock measured against the LSE crystal ("Clock check"). No emulator. |
| `bench-flash-axi-pll480` | none | Code in flash behind the I-cache; stack, emulator state and buffers in AXI SRAM behind the D-cache. |
| `bench-flash-dtcm-pll480` | `dtcm` | Code in flash; stack, `Memory` and the core's lookup tables in DTCM; frame, audio and cart RAM buffers in AXI SRAM. |
| `bench-itcm-dtcm-pll480` | `itcm` | As `flash-dtcm`, plus the hot set of `perf/itcm-sections.txt` in ITCM (docs/perf.md "ITCM placement plan"). |
| `qemu`, `qemu-itcm` | `qemu` (+`itcm`) | Only with `H7_BENCH_QEMU=1`: the frame loop on QEMU (see "Build"). |
| `bench-pack.bin` | (host) | Not an image: the fifteen workload ROMs as one blob for the QSPI NOR ("QSPI ROM placement"). |

Every `bench-*` image also runs each workload a second time on its ROM
from the QSPI NOR when the NOR holds a valid bench pack ("QSPI ROM
placement"): with the pack loaded, `bench-itcm-dtcm-pll480` is the
`itcm-dtcm-qspi` placement too.

The board images have the default feature `board` (the lib); the QEMU
images are built with `--no-default-features` and link no lib. The
features `h753`, `h755`, `pll` and `pll480` and the images without the
suffix, `*-pll` and `m4-stub` existed before ADR 0036.

The ROMs are built in: fourteen synthetic ones from the bench's generator
(`tools/bench/src/synth.rs`, compiled into `build.rs`) and blargg's
`cpu_instrs.gb`, read at build time from `H7_BENCH_CPU_INSTRS` (default: the
repo's `test-roms/`). The build fails with a message if it is missing.

### Workloads

Fifteen, in report order (`src/workload.rs`, `expected-hashes.txt`). Each
makes `calls` frame-sized `run_until` calls as `run_emulation` in
`tools/bench` does, on the ROM `synth::build(name)` returns (`synth:<name>`
there), so the ROM bytes are the bench's. 120 calls are about 2 emulated
seconds, the duration `scripts/check-bench.sh` uses for all of them but
`dma-storm` (3 s) and `halt-irq` (10 s); here those two also get 120 calls.

| Workload | Calls | Kind |
|---|---:|---|
| `worst-case`, `cgb-raster`, `dmg-raster`, `event-storm` | 120 | the bound (the original four) |
| `cpu_instrs` | 600 | blargg, from `H7_BENCH_CPU_INSTRS` |
| `dma-storm`, `halt-irq`, `apu-writes`, `oam-lines` | 120 | `BENCHES` of `check-bench.sh`, added 2026-11-25 |
| `storm-control`, `storm-apu`, `storm-spsw`, `storm-div`, `storm-gdma`, `storm-ppu` | 120 | `synth::NAMES`' storms, **outside the bound**, for context (docs/perf.md "Outside the bound"); the default variant of each. `storm-io` has no default (it needs `@<hex reg>`), so it is not run |

The storms cost 2 to 22 M x86 instructions a frame (docs/perf.md), so at
64 MHz each should run for roughly a minute (an estimate from those
counts, not measured): the whole report will take several minutes, not one. On the host, `host/` prints all fifteen lines in
2 s, and QEMU runs the whole report in 18 s.

### Placements and sizes

Built 2026-11-25 for the H753 with all fifteen workloads (bytes; `build.sh`
prints the current figures; with the first five workloads the bench `.bin`s
were 381,176, 381,352 and 382,432 B and `.rodata` 202,072 and 199,768 B; the
H755 images are 80 B larger). The ITCM image holds 102 gbc-core functions,
60,632 B:

| Image | `.bin` | `.itcm` | `.text` (flash) | `.rodata` | DTCM (`.dtcm_rodata` + `.bss`) | AXI SRAM |
|---|---|---|---|---|---|---|
| `selftest` | 9,520 | - | 7,572 | 1,284 | - | 4 |
| `bench-flash-axi` | 709,208 | - | 127,476 | 530,048 | - | 51,020 + 58,368 |
| `bench-flash-dtcm` | 709,384 | - | 127,652 | 527,744 | 2,304 + 51,020 | 58,368 |
| `bench-itcm-dtcm` | 710,464 | 64,024 | 64,712 | 527,744 | 2,304 + 51,020 | 58,368 |

The 480 MHz images as built at 7d4b026 (`H7_BENCH_PLL480=1`; the clock
code adds about 2.3 KB of `.text` and 20 B of `.bss`; the images without
`pll480` then printed 708,736, 708,912 and 709,992 B):

| Image | `.bin` | `.itcm` | `.text` (flash) | `.rodata` | DTCM (`.dtcm_rodata` + `.data` + `.bss`) | AXI SRAM |
|---|---|---|---|---|---|---|
| `selftest-pll480` | 12,152 | - | 9,972 | 1,516 | - | 20 |
| `bench-flash-axi-pll480` | 711,264 | - | 129,772 | 529,808 | - | 51,016 + 24 + 58,368 |
| `bench-flash-dtcm-pll480` | 711,632 | - | 130,140 | 527,504 | 2,304 + 51,016 + 24 | 58,368 |
| `bench-itcm-dtcm-pll480` | 712,688 | 64,024 | 67,176 | 527,504 | 2,304 + 51,016 + 24 | 58,368 |

With the HSI-based 472 MHz clock (no HSE setup) the `.bin`s were
11,924 (`selftest-pll480`), 711,040, 711,344 and 712,416 B; `clockcheck`
13,188 B and `clockcheck-pll480` 15,644 B. With the fractional PLL1
setup and the banner's `pll` line (every image) they are 12,072, 711,208,
711,512 and 712,584 B; `clockcheck` 13,336 B, `clockcheck-pll480`
15,816 B. On the lib (ADR 0036: the MPU setup, the lib's banner and LSE
measurement, the fault handler on its own stack) they are 16,524
(`selftest-pll480`), 715,504, 715,504 and 716,456 B, `clockcheck-pll480`
19,220 B; `.itcm` is still 64,024 B, and a 2 KiB `.fault_stack` follows
`.axisram` in AXI SRAM.

`.rodata` is mostly the fifteen ROM images (fourteen of 32 KiB and cpu_instrs at 64 KiB: 512 KiB). The `.bin` is 34% of the H753's 2 MiB flash and 68% of the H755's 1 MiB bank 1, so both still fit. The stack starts at the
top of DTCM (0x2002_0000), or at 0x2404_0000 in `flash-axi`; the deepest
use measured is 2,952 B with the first five workloads and 3,440 B with
all fifteen (3,432 B in `flash-axi`; the `stack used=` line).

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
`halt_begin`, `halt_end`, `idle_halted`). `build.sh` checks each `itcm` image with
`scripts/check-itcm-elf.sh` (docs/perf.md "ITCM placement plan"): every
kept function the link kept in ITCM, the dropped tiers, `.itcm` size, thunks.

Flash the `.bin`, not the ELF: the ITCM ELF has a load segment at address
0x100 whose load address is in flash, which only the `.bin` flattens.

## Build

```
./build.sh                      # all five images, the host hash test, the bench pack
H7_BENCH_QEMU=1 ./build.sh      # also the QEMU smoke run (qemu-system-arm)
H7_BENCH_LOCK=<file> ./build.sh # every cargo command under flock (agents)
```

`H7_BENCH_PLL480=1` is still accepted and changes nothing;
`H7_BENCH_BOARD=h755` and `H7_BENCH_PLL=1` are refused (ADR 0036).

```
cargo fmt --check && cargo clippy --release -- -D warnings   # also with --features selftest, clockcheck, itcm
cargo clippy --release --no-default-features --features qemu -- -D warnings
```

`flash.sh` and `read-serial.sh` take the board lock (`scripts/lib/board-lock.sh`, `${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock}`, ADR 0036); it is re-entrant and separate from `H7_BENCH_LOCK`.

Needs only the pinned toolchain (`rust-objcopy` ships inside it) and
binutils' `readelf`. The script prints the section sizes of each ELF, the
first two vector words of each `.bin`, and checks: the initial SP is in RAM
and 8-byte aligned (the placement's RAM, which also shows the image linked
its own `memory.x` and not the lib's); the reset vector is a Thumb address
inside the image; the image fits both flash banks; the lib's `__pre_init`,
`HardFault` and `DefaultHandler` are linked, not cortex-m-rt's defaults;
the fault stack (`__fault_stack_top`) is in AXI SRAM, 8-byte aligned, 2 KiB
above `.axisram`; in the ITCM image every glob of
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
peripherals, and the QEMU image links no lib (`src/qemu.rs` has its own
TCM copy, stack depth and panic handler), so no board code is covered by
it.

## Flash

```
./flash.sh out/h753/selftest-pll480.bin
```

It copies the `.bin` to the ST-LINK mass-storage drive (`H7_DRIVE`,
default `E:`; label `NOD_H753ZI`) with a
native Windows `copy`; the drive programs it at 0x0800_0000 and resets the
target. It checks the image's vectors, that the image carries this board's
name, the drive's label and files first, and reports the `FAIL.TXT` the
drive writes on an error. All drive accesses go through `cmd.exe`, so no
WSL mount of the drive is needed (the drvfs mount at `/mnt/e` was stale
once, and remounting needs sudo). Close the serial port first.

To flash and read under one hold of the board lock:
`. ../../scripts/lib/board-lock.sh; with_board_lock bash -c './flash.sh out/h753/selftest-pll480.bin && ./read-serial.sh 25'`.

Finding the drive and the port: `powershell.exe -NoProfile -Command
"Get-Volume"` lists the label (the H753 board was `E:`), and
`Get-CimInstance Win32_PnPEntity | Where-Object Name -match 'COM[0-9]+'`
lists "STMicroelectronics STLink Virtual COM Port (COM5)".

## Read the output

```
./read-serial.sh 20      # COM5 (H7_COM; the H755 board was COM3), 115200 8N1, 20 s
```

Expected, `selftest-pll480` on the lib (2026-11-25, verbatim; one line a
second, the banner every 8 lines and the `tcmprobe` line after it):

```
h7bench v1 banner image=selftest placement=flash-axi board=NUCLEO-H753ZI clock=pll472 cpu_hz=472000000 lse_cpu_hz=473565548 baud=115200 d3cr=0x0000e000 syscfg_pwrcr=0x00000081 oden=1 flash_acr=0x24 icache=1 dcache=1 mpu=1 cyccnt=1
h7bench v1 power cr3=0x00000042 csr1=0x0000e000 d3cr=0x0000e000 actvosrdy=1 supply=ldo
h7bench v1 clock rcc_cr=0x0304c025 cfgr=0x0000001b d1cfgr=0x00000048 d2cfgr=0x00000440 d3cfgr=0x00000040 flash_acr=0x24 rsr=0x00fe0000
h7bench v1 pll pllckselr=0x02020050 pllcfgr=0x01f9000d pll1divr=0x01010248 pll1fracr=0x0000c000
h7bench v1 vos0 syscfg_pwrcr=0x00000081 oden=1 pwrcr_before=0x00000000 polls_to_drop=2 polls_low=5
h7bench v1 cpu cpuid=0x411fc271 idcode=0x20036450 optsr=0x1bc6aaf0 mpu_ctrl=0x1 mpu_regions=16
h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 itcm_bytes=0 dtcm_table_bytes=0 msp=0x2403fec0
h7bench v1 selftest n=40 delay=8000000 cycles=8000019 cyccnt=2878872037 stack_used=528
h7bench v1 tcmprobe want=0x5a17c0de itcm=0x5a17c0de dtcm=0x5a17c0de
h7bench v1 selftest n=41 delay=8000000 cycles=8000019 cyccnt=3353616618 stack_used=528
h7bench v1 selftest n=42 delay=8000000 cycles=8000008 cyccnt=3825616650 stack_used=528
h7bench v1 selftest n=43 delay=8000000 cycles=8000019 cyccnt=2649404 stack_used=528
```

- The banner is the lib's (`Board::banner`), with `banner image=<image>
  placement=<placement>` as its image name, so the first line keeps every
  key the bench's own banner had (`board`, `image`, `placement`, `clock`,
  `cpu_hz`, `baud`) and adds `lse_cpu_hz` (the clock measured against the
  LSE once at boot, about 2 s, before any workload; plan D11), the VOS0
  and flash fields and `icache`/`dcache`/`mpu`/`cyccnt`. The old `options
  optsr=` line is the `cpu` line's `optsr=`, and `itcm_bytes` and
  `dtcm_table_bytes` moved to the `tcm` line.
- `cycles=` alternates 8,000,019 (odd `n`, and the line after a banner)
  and 8,000,008: every value repeats exactly, the 11-cycle step noted
  below for the older images.
- `tcmprobe` writes the top of ITCM (only while nothing is placed there)
  and the top of DTCM (only while the stack and statics are not in DTCM;
  otherwise a `.bss` word, which is then in DTCM).

Before the lib (the bench's own banner), selftest at 64 MHz on the H753
(2026-11-25, verbatim):

```
h7bench v1 banner board=NUCLEO-H753ZI image=selftest placement=flash-axi clock=hsi64 cpu_hz=64000000 baud=115200
h7bench v1 power cr3=0x00000042 csr1=0x00006000 d3cr=0x00006000 actvosrdy=1 supply=ldo
h7bench v1 clock rcc_cr=0x0004c025 cfgr=0x00000000 d1cfgr=0x00000000 flash_acr=0x37 rsr=0x00fe0000
h7bench v1 cpu cpuid=0x411fc271 idcode=0x20036450 icache=1 dcache=1 cyccnt=1 itcm_bytes=0 dtcm_table_bytes=0
h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 msp=0x2403ff68
h7bench v1 options optsr=0x1bc6aaf0
h7bench v1 selftest n=0 delay=8000000 cycles=8000034 cyccnt=10904947 stack_used=360
h7bench v1 tcmprobe want=0x5a17c0de itcm=0x5a17c0de dtcm=0x5a17c0de
```

`idcode=0x20036450`: DEV_ID 0x450, REV_ID 0x2003 = revision V. `cr3=0x42`
is the rev V reset value 0x46 with SCUEN cleared: the LDO configuration,
locked.

`selftest-pll480` on the H753 with the first, HSE-based clock (2026-11-25,
verbatim; that clock was in fact 483.86 MHz, see "Clock check"):

```
h7bench v1 banner board=NUCLEO-H753ZI image=selftest placement=flash-axi clock=pll480 cpu_hz=480000000 baud=115200
h7bench v1 power cr3=0x00000042 csr1=0x0000e000 d3cr=0x0000e000 actvosrdy=1 supply=ldo
h7bench v1 clock rcc_cr=0x0307c025 cfgr=0x0000001b d1cfgr=0x00000048 flash_acr=0x24 rsr=0x00fe0000
h7bench v1 cpu cpuid=0x411fc271 idcode=0x20036450 icache=1 dcache=1 cyccnt=1 itcm_bytes=0 dtcm_table_bytes=0
h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 msp=0x2403ff58
h7bench v1 options optsr=0x1bc6aaf0
h7bench v1 vos0 syscfg_pwrcr=0x00000081 oden=1 pwrcr_before=0x00000000 polls_to_drop=2 polls_low=6
h7bench v1 selftest n=0 delay=8000000 cycles=8000022 cyccnt=34046928 stack_used=376
h7bench v1 tcmprobe want=0x5a17c0de itcm=0x5a17c0de dtcm=0x5a17c0de
h7bench v1 selftest n=1 delay=8000000 cycles=8000008 cyccnt=516839769 stack_used=376
h7bench v1 selftest n=2 delay=8000000 cycles=8000019 cyccnt=996839813 stack_used=376
h7bench v1 selftest n=3 delay=8000000 cycles=8000008 cyccnt=1476839836 stack_used=376
```

- `rcc_cr` 0x0307c025: HSEON, HSERDY, HSEBYP (bits 16-18), PLL1ON and
  PLL1RDY; `cfgr` 0x1b: SW = SWS = PLL1; `d1cfgr` 0x48: D1CPRE /1, HPRE
  /2, D1PPRE /2; `flash_acr` 0x24: 4 wait states, WRHIGHFREQ 0b10.
  With the HSI clock (`clock=pll472 cpu_hz=472000000`) the banner reads
  the same except `rcc_cr=0x0304c025`: HSEON and HSERDY clear (HSEBYP,
  bit 18, still reads 1 after the earlier image set it; the HSE is off),
  and the `cyccnt=` steps are 472,000,000. Since the fractional PLL1
  setup every image also prints a `pll` line after `clock`: `pllckselr`,
  `pllcfgr`, `pll1divr`, `pll1fracr` (reset values in the HSI images; see
  "Boards" for the `pll480` values).
- VOS0: `d3cr` and `csr1` read the same as at 392 MHz (VOS = ACTVOS =
  0b11, VOSRDY = ACTVOSRDY = 1): neither field has a VOS0 code (RM0433
  sections 6.8.2 and 6.8.6), so `oden=1` is what shows VOS0.
- `syscfg_pwrcr=0x81`: bit 7 is reserved in RM0433 Rev 9 section 12.3.10
  (reset value 0), and it reads 0 before the ODEN write
  (`pwrcr_before`), so the hardware sets it with VOS0; the firmware only
  writes bit 0. Not explained by any document here.
- `polls_to_drop` / `polls_low`: after ODEN, VOSRDY and ACTVOSRDY stay at
  their VOS1 value for a couple of polls, then read low for a few.
  `clock_up_480` waits for the drop, then for both flags, then at least
  100 us more (RM0433 Figure 31 shows a 20 us ramp; the flags were low for
  much less than that), so the clock only goes up after the ramp. The
  first version waited only for VOSRDY, as RM0433 section 6.6.2 step 4
  and ST's HAL do, and saw 0 polls low: it passed on the stale VOS1 flag.
- `cycles=` is 8,000,008, and about 11 more on a line after another
  UART line or the probe, as at 64 MHz (8,000,007 and 8,000,018).
  Clean text at 115200 with a BRR computed for a 120 MHz PCLK puts the
  8 MHz HSE (the ST-LINK's MCO) within a few percent of 8 MHz (the LSE
  measurement later put it at +0.8%).
- A failed wait panics with the step's name, at the right baud rate:
  checked by building with the HSE wait (since removed) and, separately,
  the final switch wait made to fail (`FAULT panic panicked at
  src/board.rs:...: clock setup failed: hse ready`, `... switch to pll1`).

### Clock check

`clockcheck` (and `clockcheck-pll`, `clockcheck-pll480`: the same with
that image's clock) measures the CPU clock against the board's 32.768 kHz
LSE crystal X2 (UM2407 section 7.5.2, default: SB40/SB41 off, R38/R39
on), a reference that depends neither on the HSI, the HSE nor the PC. It
enables the LSE (PWR_CR1.DBP, then RCC_BDCR LSEDRV medium-high and LSEON,
up to 5 s for LSERDY; RM0433 sections 6.8.1 and 8.7.26; the LSE stays on
across resets, in the backup domain), routes it to TIM16's TI1 (RM0433
section 41.6.18, `TIM16_TISEL.TI1SEL` = 0b0010 = LSE) and captures every
rising edge (input capture, section 41.4.6). The CPU polls CC1IF and reads
DWT_CYCCNT at the first and the last edge of 65,536 LSE periods (2 s);
the polling latency is the same at both ends, so the error is a few dozen
cycles in about a billion. TIM16's own latched captures count the same
window in timer kernel clocks (`tim_hz_measured`, = AHB clock), a check
with no polling at all. Each line: that window, the running average since
reset, its deviation from `cpu_hz` in ppm, and the system clock's
reference oscillator implied by it (the PLL multiplies exactly). If the
LSE does not start it prints `lse=FAIL` and no figure. The crystal's
tolerance is not in UM2407; a 32.768 kHz watch crystal is typically
within +-20 ppm, which is far below everything measured here.

Results on the NUCLEO-H753ZI, 2026-11-25 (LSE started on the first run;
the start time was not captured, later runs found it running):

| Image | Windows averaged | CPU clock, average | Window min / max (lines captured) | Implied reference |
|---|---|---|---|---|
| `clockcheck` (HSI 64 MHz) | 30 (60 s; n=29) | 64,229,574 Hz (+3,587 ppm) | 64,221,197 / 64,243,083 (15) | HSI 64.230 MHz |
| `clockcheck-pll480`, HSE (first version) | 50 (100 s; n=49) | 483,863,097 Hz (+8,048 ppm) | 483,646,110 / 484,116,400 (37) | HSE (MCO) 8.0644 MHz |
| `clockcheck-pll480`, HSI 472 MHz, DIVM1 4 (integer, second version) | 47 (94 s; n=46) | 473,682,766 Hz (+3,565 ppm) | 473,562,160 / 473,762,450 (34) | HSI 64.228 MHz |
| `clockcheck-pll480`, HSI 472 MHz, DIVM1 5 (fractional, now) | 51 (102 s; n=50) | 473,617,229 Hz (+3,426 ppm) | 473,525,540 / 473,678,360 (37) | HSI 64.219 MHz |
| `clockcheck-pll`, HSI 392 MHz, DIVM1 5 (fractional, now) | 33 (66 s; n=32) | 393,290,972 Hz (+3,293 ppm) | 393,236,230 / 393,374,010 (33) | HSI 64.211 MHz |

"Windows averaged" is how many 2 s windows the average covers: every
window since the image's reset (the last line's `n` + 1); min / max are
over the lines captured, which can start after the first windows. The
fractional runs: 2026-11-25, after the DIVM1 5 change.

- The HSI, at 64.21 to 64.23 MHz (+0.33 to +0.36%), is inside DS12117
  Table 133 (rev V: 63.7 to 64.3 MHz at 30 °C). The first two HSI-based
  images agree on it to 30 ppm; the later fractional runs read 160 and
  290 ppm lower (an HSI drift within the datasheet's, perhaps
  temperature; not separated). The fractional `pll480` runs 473.62 MHz,
  1.3% under the 480 MHz limit. The fraction is in effect: with FRACN1
  not latched (x 73) it would read 1% below the integer image, not 0.014%.
- The HSE-based "480 MHz" ran at 483.86 MHz, the MCO at 8.064 MHz,
  0.8% fast; the PC's 484.31 MHz was the right order. The MCO also moved:
  +/-0.05% from window to window and about +50 ppm in the average in 20 s,
  about three times the HSI's window spread. It behaves like an RC
  oscillator, not a crystal (UM2407 section 7.5.1 only says "fixed at
  8 MHz", changeable by an ST-LINK firmware upgrade; no tolerance).
- `tim_ppm` equals `cpu_ppm` in every line: TIM16's clock is the AHB
  clock, CPU / 2 (HSI: CPU / 1).

`clockcheck-pll480` now (fractional PLL1), verbatim:

```
h7bench v1 banner board=NUCLEO-H753ZI image=clockcheck placement=flash-axi clock=pll472 cpu_hz=472000000 baud=115200
h7bench v1 power cr3=0x00000042 csr1=0x0000e000 d3cr=0x0000e000 actvosrdy=1 supply=ldo
h7bench v1 clock rcc_cr=0x0304c025 cfgr=0x0000001b d1cfgr=0x00000048 flash_acr=0x24 rsr=0x00fe0000
h7bench v1 pll pllckselr=0x02020050 pllcfgr=0x01f9000d pll1divr=0x01010248 pll1fracr=0x0000c000
h7bench v1 vos0 syscfg_pwrcr=0x00000081 oden=1 pwrcr_before=0x00000000 polls_to_drop=2 polls_low=6
h7bench v1 clockcheck lse=ready waited_cycles=0 (0: already running) window_periods=65536 lse_hz=32768 ref=hsi ref_hz_nominal=64000000 tim_hz_nominal=236000000
h7bench v1 clockcheck n=50 cycles=947198460 cpu_hz_measured=473599230 avg_s=102 avg_cpu_hz_measured=473617229 cpu_ppm=+3426.3 hsi_hz_implied=64219285 tim_hz_measured=236808615 tim_ppm=+3426.3 overcapture=0 stack_used=624
```

(The banner's `cpu`, `tcm` and `options` lines and the windows n=48 and 49 are left out above.)

On the H755:

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

Expected, bench (the workloads run once, several minutes at 64 MHz (the storms dominate); then the
whole report repeats every 3 s forever):

```
h7bench v1 banner image=bench placement=itcm-dtcm board=NUCLEO-H753ZI clock=pll472 cpu_hz=472000000 lse_cpu_hz=... ...
h7bench v1 start worst-case calls=120
h7bench v1 worst-case calls=120 frames=... clocks=... min=... mean=... max=... hash=................
...
h7bench v1 storm-ppu calls=120 frames=... clocks=... min=... mean=... max=... hash=................
h7bench v1 stack used=3440 of=77592
h7bench v1 end pass=1
```

`min`/`mean`/`max` are CPU cycles per `run_until` call of one frame
(`FRAME_CLOCKS`); 50% of a 59.73 Hz frame is `cpu_hz / 59.73 / 2` cycles
(4.02 M at 480 MHz, docs/perf.md; 3.95 M at 472 MHz, the `pll480` clock now). `calls`, `frames`, `clocks` and `hash`
must equal the line of the same workload in `expected-hashes.txt`: the hash
folds every call's clocks, events and audio samples and every completed
frame, so a match means the board computed exactly what the PC does.

`end pass=N` is not a verdict: N counts the report's repeats (1 after the
run, 2 three seconds later, and so on), each printing the same stored
results, so `pass=2` or later is as good as `pass=1`. The verdict is the
comparison with `expected-hashes.txt`, done on the PC. The ST-LINK's VCP
drops or truncates lines sent while no program has the port open (the
first report after a flash is often cut), so read a later repeat whole.

**`expected-hashes.txt` regenerated 2026-11-25** (human approved) from
`host/`'s `cargo run --release` at f34bfd0: it had been made at bcd881e, and
core changes since (not bisected; the APU decimation rewrite 7d389ce is
the likely one) changed the `worst-case` and `cpu_instrs` hashes, so
`host/`'s `cargo test` failed. The new values equal what the H753 printed
in every placement at both clocks. If `host/`'s test fails again after a
core change, regenerate with the command in the test's message, and say
why in the commit.

LEDs: LD1 (green) on from the first instructions of `main`, toggling while
it works (every 8 frames; 1 Hz toggle in the selftest; 2 Hz after the
report); LD2 (yellow) on once bring-up is through; LD3 (red) blinking fast
in antiphase with LD1 is a fault (panic, HardFault, unexpected exception),
with a `h7bench v1 FAULT ...` line on the UART every second.

## QSPI ROM placement

Plan Stage 2 M2, placement `itcm-dtcm-qspi`: the bench workloads with
their ROMs read from the shield's QSPI NOR (MX25L6433F, memory-mapped,
0xEB quad I/O at 59 MHz, cached; `docs/reference/qspi-nor.md`) instead of
internal flash, as the conservative proxy for the Stage 3 HyperRAM.

**One binary, one boot, two ROM sources.** Every board bench image, after
its banner, maps the NOR, runs `rom_image::validate` over the window and
parses the image's data as a *bench pack* (below). It then runs all
fifteen workloads on their ROMs in internal flash (`.rodata`, the lines as
before) and, if the pack is valid, all fifteen again on the pack's copies
in the window:

```
h7bench v1 qspi pack ok len=524800 hash=b8aead06e9d0dc4e roms=15 same_as_flash=1 validate_cycles=8408320 qspi_hz=59000000 window=cached exit_fallbacks=0
h7bench v1 worst-case calls=120 frames=119 clocks=8420792 min=... mean=... max=... hash=de590b88792b5778
...
h7bench v1 qspi worst-case calls=120 frames=119 clocks=8420792 min=... mean=... max=... hash=de590b88792b5778
...
```

Both passes call one instance of `workload::run` (the same fn items for
`now` and `between`; `readelf` shows one `workload::run` symbol), with the
same buffers, clock and placement, so the ROM pointer is the only
difference: the QSPI figures are against the internal-flash ones of the
same boot, free of the +-2% rebuild noise. The window stays mapped for the
life of the image, so `exit_fallbacks` stays 0. `same_as_flash=1`: every
pack ROM is byte for byte the embedded one. Without a valid pack the line
is `h7bench v1 qspi pack none stage=<map|header|validate|pack>
error=<e>` and only the flash pass runs; the QEMU image has no QSPI code.
The pack line is repeated after every banner.

**The bench pack** (`src/pack.rs`; the NOR holds one ADR 0037 image, and
the pack is that image's data, so the loader, the header and `validate`
treat it as any ROM): magic `H7BPACK1`, the entry count, one 32-byte
entry per workload in report order (name, offset, length), then the
ROMs at 256-byte-aligned offsets, 0xFF between them. `host/`'s
`cargo run --release -- pack <file>` writes it (build.sh:
`out/h753/bench-pack.bin`, 524,800 B) after parsing it back with the
firmware's parser; `cargo test` covers the round trip and every refusal.

**Run:**

```
./build.sh && ../../crates/gbc-nucleo-h753/build.sh   # the bench images, the pack, the loader
./run-qspi.sh out/h753/bench-itcm-dtcm-pll480.bin     # loader, load the pack, flash, read 100 s, check
./check-report.sh <capture> <image.bin> --qspi        # the check alone, on a saved capture
```

`run-qspi.sh` holds the board lock across: flash the loader, `scripts/
gbc-pc.sh load out/h753/bench-pack.bin` (a no-op when the NOR holds it
already), flash the image, `read-serial.sh` into
`out/h753/runs/<image>-<time>.txt`, `check-report.sh`. The check takes the
last complete report of the capture (banner through `end pass=N`) and
requires: the banner's `image_bytes`/`image_cksum` equal `cksum` of the
`.bin`; the fifteen flash lines and, with `--qspi`, the fifteen `qspi`
lines (with `qspi ` removed), cycle fields zeroed, exactly
`expected-hashes.txt`; `qspi pack ok ... same_as_flash=1`. It then prints
the mean / max table with QSPI against flash in %. Exit 0 pass, 1 a check
failed, 2 no complete report. A later run needs only the last three steps
(the pack stays in the NOR).

The image takes about 65 s from reset to its first report (two passes of
about 30 s, the storms most of it); the stack peak is now 4,488 B (3,440
before: the second results array).

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

## Boards

### NUCLEO-H753ZI

UM2407 Rev 6 (MB1364 board), RM0433 Rev 9 (MCU), DS12117 Rev 10
(STM32H753xI datasheet); all in `../gbc-32-hardware/datasheets/`. The code
comments in `crates/gbc-nucleo-h753/src/` carry the same citations (in
`src/board.rs` before ADR 0036).

- **Supply.** The STM32H753 has no SMPS: VCORE comes from the LDO or, in
  bypass, from an external regulator (RM0433 section 6.4, Figure 20,
  Table 34). The MB1364 feeds it from the LDO (UM2407 has no SMPS or
  bypass option). Table 34 "LDO supply": LDOEN = 1, BYPASS = 0. In
  `PWR_CR3` (RM0433 section 6.8.4) bit 2 is **SCUEN**, not SDEN as on the
  H755, so the H755 value (0x04) would be an illegal combination here
  (ignored, register locked, `ACTVOSRDY` low until a power cycle). The
  firmware does `PWR_CR3 = (PWR_CR3 & !0x07) | 0x02`, ST's
  `HAL_PWREx_ConfigSupply(PWR_LDO_SUPPLY)`, in `__pre_init` as on the H755,
  then waits for `ACTVOSRDY` (section 6.4.1 step 5). Read back:
  `cr3=0x00000042 supply=ldo actvosrdy=1`.
- **Clock.** Phase A as on the H755 (64 MHz HSI, VOS3, 7 wait states;
  VOS3 allows 200 MHz, DS12117 Table 111). Phase B is the same 392 MHz
  tree at VOS1: DS12117 Table 111 (rev V) allows a 400 MHz CPU and a
  200 MHz AXI/AHB clock at VOS1 (480 MHz needs VOS0, rev V only, plus
  SYSCFG ODEN; not used), and RM0433 Table 17 gives 2 wait states,
  WRHIGHFREQ = 0b10, for a 196 MHz AXI clock at VOS1. VOS in `PWR_D3CR`:
  RM0433 section 6.8.6.
- **Phase C clock (`pll480`, this board only).** Since 2026-11-25: PLL1
  from the HSI, 64 MHz / DIVM1 5 = 12.8 MHz reference, x 73.75 = 944 MHz
  VCO, / 2 = **472 MHz** nominal; AXI/AHB 236 MHz, every APB 118 MHz.
  x 73.75 is fractional mode: DIVN1 field 72 (N 73; the field holds
  N - 1, RM0433 section 8.7.13), FRACN1 6144 (0.75 x 2^13, section
  8.7.14), loaded and latched by PLL1FRACEN 0 -> 1 before PLL1ON as
  RM0433 section 8.5.5 "PLL initialization phase" and Figure 48 give it.
  The banner's `pll` line shows the registers (`pllckselr=0x02020050
  pllcfgr=0x01f9000d pll1divr=0x01010248 pll1fracr=0x0000c000`). The
  first HSI version used DIVM1 4, x 59: a 16 MHz reference, the very top
  of the PLL input range (RM0433 section 8.7.12 PLL1RGE 0b11: 8 to
  16 MHz; DS12117 Table 136: fPLL_IN 2 to 16 MHz), and over it with this
  board's HSI (16.06 MHz). VOS1, then
  SYSCFG_PWRCR.ODEN, then VOS0 (RM0433 section 6.6.2), allowed by DS12117
  Table 111/112 (LDO only, TJ at most 105 °C) and ES0392 section 2.2.21
  (rev V). Flash 4 wait states, WRHIGHFREQ = 0b10 (RM0433 Table 17, AXI in
  ]225; 240] MHz at VOS0), set before the switch.
  Why 472 and why the HSI: the first version took the HSE in bypass, 8 MHz
  from the ST-LINK MCO (UM2407 section 7.5.1, SB45 on), / 2 x 240 / 2 =
  480 MHz, and measured 483.86 MHz ("Clock check"): the MCO is 0.8% fast
  and has no stated tolerance, so no margin on it can be shown, on this
  board or another NUCLEO's ST-LINK. The HSI has a datasheet bound
  (DS12117 Table 133, rev V): at most 64.3 MHz at 30 °C, +1% for TJ -20
  to 105 °C, +0.03% over VDD, so at most 64.96 MHz (+1.5%); at least
  63.7 MHz -2% (TJ down to -40 °C) -0.12% (VDD) = 62.35 MHz. Every
  limit at both bounds (RM0433 sections 8.7.12 and 8.7.13, DS12117
  Tables 111 and 136):

  | Clock | Limit | 62.35 MHz HSI | 64 MHz (nominal) | 64.23 MHz (this board) | 64.96 MHz HSI |
  |---|---|---|---|---|---|
  | fPLL_IN (HSI / 5) | 8 to 16 MHz | 12.47 | 12.80 | 12.85 | 12.99 |
  | VCO (x 73.75) | 192 to 960 MHz | 919.7 | 944.0 | 947.4 | 958.2 |
  | CPU (VCO / 2) | 480 MHz | 459.8 | 472.0 | 473.7 | 479.1 |
  | AXI/AHB (CPU / 2) | 240 MHz | 229.9 | 236.0 | 236.8 | 239.6 |
  | APB (AXI / 2) | 120 MHz | 115.0 | 118.0 | 118.4 | 119.8 |

  VCO, CPU, AXI and APB all end 0.19% under their limits at the upper
  bound (one ratio), fPLL_IN 19% under. The next integer step, x 60
  from 16 MHz, is 480 MHz nominal, no margin. Measured against the LSE:
  473.62 MHz (HSI 64.219 MHz, +0.34%; "Clock check"). The AXI clock
  stays in ]225; 240] MHz at the low bound, and 4 wait states are valid
  at any lower clock anyway. The table is about the mean frequency: the
  PLL's long-term jitter (DS12117 Table 136: +-0.8% in sigma-delta mode,
  +-0.7% in integer mode) is a period spread that the datasheet does not
  relate to the frequency limits, and integer mode has it too.
- **Memory** (RM0433 section 2.3.2 Table 7): ITCM 64 KB at 0, DTCM 128 KB
  at 0x2000_0000, AXI SRAM 512 KB at 0x2400_0000, the same as the H755;
  flash 2 x 1 MB from 0x0800_0000, and with one core the image may use
  both banks.
- **Pins**: the same as the H755. VCP on USART3 PD8/PD9 (UM2407 section
  7.6.5, Table 10, SB19 and SB12 on); LD1 green PB0 (SB39 on), LD2 yellow
  PE1, LD3 red PB14 (section 7.6.1).
- **Single core**: no BCM4/BCM7, no M4 stub; the banner's `dualcore` line
  is an `options optsr=` line instead (read only).

### NUCLEO-H755ZI-Q (retired)

History: the `h755` feature was removed with the switch onto the lib (ADR
0036). UM2408 Rev 6 (board), RM0399 Rev 5 (MCU), DS12919 Rev 3
(STM32H755xI datasheet); the code comments in `src/board.rs` carried the
same citations (git history before ADR 0036).

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
- Phase B: PLL1 from HSI, 64 MHz / 5 x 61.25 / 2 = 392 MHz (the HSI's
  tolerance must stay under 400: 397.9 MHz at the H753's HSI bound,
  DS12117 Table 133; DS12919 not checked for it),
  AXI/AHB 196 MHz (also the M4's clock, limit 200), APB 98 MHz, flash 2
  wait states with WRHIGHFREQ = 2. x 61.25 is fractional mode (DIVN1
  field 60, FRACN1 2048), with a 12.8 MHz reference and a 784 MHz VCO,
  as in `pll480` ("NUCLEO-H753ZI"); the 2026-11-24 runs used the integer
  64 / 4 x 49 / 2, the same nominal clock from a 16 MHz reference (the
  top of the 8 to 16 MHz range, RM0399 has the same PLL1).

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
- **`m4-stub`** (no longer built; 64-byte vector table plus `wfi` in a loop, no RAM, no
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

On the H753 the supply deadlock of step 2 should not occur: its only
legal configurations are the LDO (this firmware, and the reset default)
and bypass, and an illegal write leaves the LDO running (RM0433 section
6.8.4). Steps 1 and 3 apply as written; where BOOT0 is on the MB1364 was
not checked.

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

## Results

CPU cycles per frame-sized `run_until` call, mean / max over the calls of
each workload (see "Workloads"; the 2026-11-24 and 2026-11-25 results below are the first five).

### NUCLEO-H755ZI-Q, 2026-11-24

Every run printed the hashes of `expected-hashes.txt`. Core at `main`
bcd881e plus this crate.

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

### NUCLEO-H753ZI, 2026-11-25

Core at `main` 74e7dc3 plus this crate's board selection (the same
workloads, placements and clock code as on the H755). STM32H753ZIT6
revision V (`idcode=0x20036450`), the same Cortex-M7 r1p1 (`cpuid=0x411fc271`),
LDO supply (`cr3=0x42 supply=ldo actvosrdy=1`). Every run's `calls`,
`frames`, `clocks` and `hash` equal what `host/` prints at 74e7dc3, and the
`expected-hashes.txt` regenerated from it (see "Read the output").

64 MHz HSI (`hsi64`: VOS3, flash 7 wait states, AXI clock = CPU clock):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` |
|---|---|---|---|
| worst-case | 4,475,431 / 4,527,598 | 4,472,123 / 4,525,620 | 3,287,649 / 3,324,581 |
| cgb-raster | 2,806,076 / 2,861,491 | 2,801,192 / 2,847,811 | 2,635,358 / 2,674,888 |
| dmg-raster | 2,203,710 / 2,236,155 | 2,201,487 / 2,229,413 | 1,917,793 / 1,939,871 |
| event-storm | 2,109,015 / 2,135,121 | 2,097,801 / 2,113,684 | 2,025,842 / 2,039,744 |
| cpu_instrs | 1,469,945 / 2,042,018 | 1,462,485 / 2,025,882 | 1,293,504 / 1,766,193 |

(A table here used to scale these 64 MHz cycles to % of a 392 and a
480 MHz frame. Both clocks are now measured, below; the scaled 480 MHz
figures were 55.7% / 56.3% for the worst case from flash, against 63.4% /
64.2% measured, because flash costs more cycles at 480 MHz.)

392 MHz PLL (`pll392`: VOS1, flash 2 wait states, AXI clock = CPU clock / 2),
measured 2026-11-25. Cycles per frame, mean / max, then % of a 392 MHz
frame (6,563,140 cycles). Hashes equal the 64 MHz runs and `host/`:

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` | % `flash-axi` | % `itcm-dtcm` |
|---|---|---|---|---|---|
| worst-case | 4,625,922 / 4,680,200 | 4,593,882 / 4,645,866 | 3,285,344 / 3,322,872 | 70.5% / 71.3% | 50.1% / 50.6% |
| cgb-raster | 2,783,141 / 2,836,198 | 2,811,114 / 2,856,566 | 2,604,823 / 2,643,809 | 42.4% / 43.2% | 39.7% / 40.3% |
| dmg-raster | 2,258,589 / 2,290,448 | 2,232,378 / 2,261,918 | 1,921,983 / 1,944,282 | 34.4% / 34.9% | 29.3% / 29.6% |
| event-storm | 2,115,450 / 2,141,192 | 2,107,003 / 2,122,730 | 2,014,338 / 2,028,449 | 32.2% / 32.6% | 30.7% / 30.9% |
| cpu_instrs | 1,473,695 / 2,043,796 | 1,471,993 / 2,040,030 | 1,293,829 / 1,766,281 | 22.5% / 31.1% | 19.7% / 26.9% |

`selftest-pll` ran first (`clock=pll392`, `d3cr=0x0000e000`,
`cfgr=0x0000001b`, `d1cfgr=0x00000048`, `flash_acr=0x22`, `cycles=8000008`
every second, `tcmprobe` equal). Right after `bench-flash-axi-pll` was
first copied, the whole ST-LINK (drive, VCP, debug interface) dropped off
USB for several minutes; after the human re-plugged it, that image was
running and passing (`end pass=13`), so the image was not the cause. Cause
of the drop unknown; see "Still not verified".

- The worst case in `itcm-dtcm` is 3.29 M cycles, 6.3% more than the H755's
  3.09 M at bcd881e. The CPU core, the placements and the clock are the
  same and the hot set runs from ITCM, so the difference is most likely
  the core's code since bcd881e, not the board; it was not separated (the
  H755 is no longer here). Measured at 392 MHz: 50.1% of a frame on
  average, 50.6% at most: at the 50% bar, no longer under it. At 480 MHz
  it measures 41.0% / 41.4% (below).
- Unlike the 64 MHz runs, at 392 MHz the flash placements cost 3% more
  cycles on the worst case (AXI at half the CPU clock, flash wait states);
  ITCM is unchanged, 1.41x faster than flash there.
- ITCM against flash: 1.36x on the worst case, 1.04x to 1.15x on the
  others; DTCM against AXI SRAM makes under 1%.
- `stack used=2952` (itcm-dtcm), 2944 (the flash placements).

### ROMs from QSPI (`itcm-dtcm-qspi`), NUCLEO-H753ZI at 472 MHz, 2026-11-25

`bench-itcm-dtcm-pll480` as built at 7458dc5 (core unchanged), with the
bench pack (524,800 B, FNV-1a 64 `b8aead06e9d0dc4e`) loaded by the
gbc-nucleo-h753 loader at 4 Mbit/s in 5.47 s; "QSPI ROM placement" for
the method. Three boots of the same `.bin` (`run-qspi.sh` once, then
flash and read twice), each running the internal-flash pass and then the
QSPI pass. Every boot passed `check-report.sh --qspi`: banner
`image_bytes=728720 image_cksum=1055326146` = `cksum` of the `.bin`; all
fifteen flash lines and all fifteen `qspi` lines' `calls`, `frames`,
`clocks` and `hash` equal `expected-hashes.txt`; `qspi pack ok ...
same_as_flash=1 validate_cycles=8408320 qspi_hz=59000000 window=cached
exit_fallbacks=0` (the pack's validate: 17.8 ms, 29.5 MB/s through the
cold window). `stack used=4488 of=77564`.

Cycles per frame, mean / max, boot 1 (boot 2 printed exactly the same
sixty numbers; boot 3 differs by at most 0.054%, `halt-irq`'s QSPI max):

| Workload | internal flash | QSPI | QSPI vs internal |
|---|---|---|---|
| worst-case | 3,300,028 / 3,338,391 | 3,378,396 / 3,417,095 | +2.37% / +2.36% |
| cgb-raster | 2,608,239 / 2,647,364 | 2,611,818 / 2,653,032 | +0.14% / +0.21% |
| dmg-raster | 1,920,125 / 1,942,291 | 1,923,155 / 1,947,229 | +0.16% / +0.25% |
| event-storm | 2,028,773 / 2,042,705 | 2,032,042 / 2,048,093 | +0.16% / +0.26% |
| cpu_instrs | 1,296,003 / 1,768,804 | 1,296,953 / 1,768,804 | +0.07% / +0.00% |
| dma-storm | 3,071,740 / 3,077,003 | 3,075,984 / 3,081,493 | +0.14% / +0.15% |
| halt-irq | 656,766 / 658,741 | 659,184 / 662,248 | +0.37% / +0.53% |
| apu-writes | 895,837 / 918,106 | 897,533 / 920,678 | +0.19% / +0.28% |
| oam-lines | 1,446,752 / 1,462,811 | 1,448,369 / 1,464,718 | +0.11% / +0.13% |
| storm-control | 2,242,987 / 2,261,261 | 2,246,694 / 2,265,580 | +0.17% / +0.19% |
| storm-apu | 23,962,863 / 24,175,615 | 23,963,759 / 24,176,415 | +0.00% / +0.00% |
| storm-spsw | 14,468,245 / 14,632,669 | 14,466,317 / 14,618,911 | -0.01% / -0.09% |
| storm-div | 16,462,228 / 16,606,192 | 16,462,989 / 16,606,915 | +0.00% / +0.00% |
| storm-gdma | 2,905,185 / 2,930,139 | 3,317,581 / 3,756,955 | +14.20% / +28.22% |
| storm-ppu | 28,474,636 / 28,772,245 | 28,475,119 / 28,775,851 | +0.00% / +0.01% |

- The worst case from QSPI: 3,378,396 / 3,417,095 cycles, 42.8% / 43.2%
  of a 472 MHz frame, **85.5% / 86.5% of the 50% line** (3,951,279; report
  only), far from a real-time frame (7,902,557).
- Run to run (three boots), every figure moved by at most 0.054%, so the
  QSPI cost is resolved to well under 0.1%: about 0.1-0.4% on most
  workloads, +2.4% on the worst case and +14% / +28% on `storm-gdma`.
  Those two read the ROM in bulk every frame: the worst case's HBlank HDMA
  copies 2 KiB from ROM 0000 a frame, and `storm-gdma`'s GDMA source
  walks the whole address space at 1,665 blocks of 16 B a frame, so
  roughly half its 26.6 KB a frame comes from ROM, more than the 16 KiB
  D-cache holds. At the ~770 cycles per random 32-byte miss of
  `docs/reference/qspi-nor.md`, +78 k cycles a frame is about 100 misses
  and +412 k about 540 (estimates from that figure, not counted).
- The internal-flash pass against the previous `itcm-dtcm` run ("After
  the switch onto the lib"), a different build: -1.1% (`cgb-raster`) to
  +2.7% (`storm-div`), +0.12% on the worst case. That is the rebuild
  noise this method avoids: the QSPI column is compared with the flash
  column of the same boot, not with that table.
- The other two images (`flash-axi`, `flash-dtcm`) run the QSPI pass too
  when the pack is loaded; not run.
- Captures (`out/h753/runs/`) are not committed.

### After the switch onto the lib, 2026-11-25

`bench-itcm-dtcm-pll480` built on the gbc-nucleo-h753 lib (ADR 0036; core
unchanged), after `selftest-pll480` (the verbatim lines in "Read the
output"). All fifteen lines' `calls`, `frames`, `clocks` and `hash` equal
`expected-hashes.txt`, and every repeat printed the same line for each
workload. Banner: `lse_cpu_hz=473697508`, `itcm_bytes=64024
dtcm_table_bytes=2432`, `mpu=1`; `stack used=3440 of=77592`. Only this
placement was run; the flash placements and `clockcheck-pll480` are built,
not run.

Cycles per frame, mean / max, against the 472 MHz table below
(`itcm-dtcm`, built at b4d6c81):

| Workload | On the lib | Before (b4d6c81) | Mean | Max |
|---|---|---|---|---|
| worst-case | 3,296,081 / 3,333,519 | 3,296,548 / 3,334,278 | -0.01% | -0.02% |
| cgb-raster | 2,636,904 / 2,676,479 | 2,605,630 / 2,644,564 | +1.20% | +1.21% |
| dmg-raster | 1,920,520 / 1,942,643 | 1,924,667 / 1,947,063 | -0.22% | -0.23% |
| event-storm | 2,026,813 / 2,041,255 | 2,016,303 / 2,031,279 | +0.52% | +0.49% |
| cpu_instrs | 1,296,462 / 1,769,208 | 1,295,514 / 1,767,935 | +0.07% | +0.07% |
| dma-storm | 3,084,675 / 3,089,737 | 3,088,109 / 3,092,401 | -0.11% | -0.09% |
| halt-irq | 659,801 / 661,155 | 658,298 / 660,286 | +0.23% | +0.13% |
| apu-writes | 898,367 / 920,576 | 897,195 / 919,289 | +0.13% | +0.14% |
| oam-lines | 1,451,297 / 1,467,214 | 1,447,995 / 1,463,898 | +0.23% | +0.23% |
| storm-control | 2,248,256 / 2,266,586 | 2,248,086 / 2,266,601 | +0.01% | -0.00% |
| storm-apu | 23,961,057 / 24,173,855 | 23,961,284 / 24,174,010 | -0.00% | -0.00% |
| storm-spsw | 14,477,681 / 14,608,726 | 14,572,044 / 14,705,973 | -0.65% | -0.66% |
| storm-div | 16,036,439 / 16,178,658 | 16,339,897 / 16,483,324 | -1.86% | -1.85% |
| storm-gdma | 2,883,432 / 2,908,998 | 2,876,062 / 2,901,220 | +0.26% | +0.27% |
| storm-ppu | 28,427,641 / 28,727,641 | 28,561,042 / 28,862,232 | -0.47% | -0.47% |

- Within the about +-2% build-to-build band seen before (the 472 against
  483.86 MHz comparison below): the worst case is unchanged (3,296,081 /
  3,333,519, 41.7% / 42.2% of a 472 MHz frame, 83.4% / 84.4% of the 50%
  line), `storm-div` moves most (-1.9%), then `cgb-raster` (+1.2%). The
  bring-up runs before the first measured call, so only code layout and
  the MPU's attributes (write-back flash, which the default map has as
  write-through) can move these figures.

### NUCLEO-H753ZI at 472 MHz, 2026-11-25 (all three placements, before the lib)

`pll480` as it is now: PLL1 from the HSI, 12.8 MHz reference, fractional
x 73.75, **472 MHz** nominal (473.62 MHz measured against the LSE, "Clock
check"), VOS0, flash 4 wait states, AXI 236 MHz. Images built at b4d6c81
(core unchanged since the 483.86 MHz run's 7d4b026; only this crate's
clock code and the `clockcheck` image changed). `selftest-pll480` first
(`clock=pll472`, `flash_acr=0x24`, `oden=1`, `cycles=8000009`, `cyccnt=`
steps of 472,000,030, `tcmprobe` equal), then the three placements. In
every placement all fifteen lines' `calls`, `frames`, `clocks` and `hash`
equal `expected-hashes.txt` (compared on the lines without
`min`/`mean`/`max`, a later repeat than the first). Banners:
`clock=pll472 cpu_hz=472000000`, the `pll` line as in "Clock check",
`itcm_bytes=64024` in `itcm-dtcm`. A second flash of
`bench-itcm-dtcm-pll480` printed the same cycles to the last digit.

Cycles per frame, mean / max, and % of a 472 MHz frame (**7,902,557
cycles**, 472 MHz / 59.7275 Hz):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` | % `flash-axi` | % `flash-dtcm` | % `itcm-dtcm` |
|---|---|---|---|---|---|---|
| worst-case | 5,188,678 / 5,251,386 | 5,116,841 / 5,182,062 | 3,296,548 / 3,334,278 | 65.7% / 66.5% | 64.7% / 65.6% | 41.7% / 42.2% |
| cgb-raster | 2,974,477 / 3,027,148 | 2,880,291 / 2,925,168 | 2,605,630 / 2,644,564 | 37.6% / 38.3% | 36.4% / 37.0% | 33.0% / 33.5% |
| dmg-raster | 2,356,393 / 2,390,256 | 2,359,427 / 2,390,740 | 1,924,667 / 1,947,063 | 29.8% / 30.2% | 29.9% / 30.3% | 24.4% / 24.6% |
| event-storm | 2,190,008 / 2,216,442 | 2,123,478 / 2,140,134 | 2,016,303 / 2,031,279 | 27.7% / 28.0% | 26.9% / 27.1% | 25.5% / 25.7% |
| cpu_instrs | 1,543,172 / 2,141,960 | 1,540,351 / 2,146,528 | 1,295,514 / 1,767,935 | 19.5% / 27.1% | 19.5% / 27.2% | 16.4% / 22.4% |
| dma-storm | 3,249,074 / 3,256,794 | 3,227,889 / 3,235,118 | 3,088,109 / 3,092,401 | 41.1% / 41.2% | 40.8% / 40.9% | 39.1% / 39.1% |
| halt-irq | 747,913 / 751,156 | 753,776 / 756,908 | 658,298 / 660,286 | 9.5% / 9.5% | 9.5% / 9.6% | 8.3% / 8.4% |
| apu-writes | 1,134,930 / 1,159,974 | 1,163,854 / 1,187,010 | 897,195 / 919,289 | 14.4% / 14.7% | 14.7% / 15.0% | 11.4% / 11.6% |
| oam-lines | 1,676,920 / 1,697,822 | 1,679,072 / 1,698,584 | 1,447,995 / 1,463,898 | 21.2% / 21.5% | 21.2% / 21.5% | 18.3% / 18.5% |
| storm-control | 2,491,435 / 2,516,566 | 2,521,886 / 2,546,334 | 2,248,086 / 2,266,601 | 31.5% / 31.8% | 31.9% / 32.2% | 28.4% / 28.7% |
| storm-apu | 26,570,203 / 26,841,596 | 25,743,612 / 25,981,351 | 23,961,284 / 24,174,010 | 336.2% / 339.7% | 325.8% / 328.8% | 303.2% / 305.9% |
| storm-spsw | 15,822,280 / 15,996,594 | 15,310,679 / 15,464,886 | 14,572,044 / 14,705,973 | 200.2% / 202.4% | 193.7% / 195.7% | 184.4% / 186.1% |
| storm-div | 16,841,934 / 17,041,328 | 16,902,052 / 17,062,206 | 16,339,897 / 16,483,324 | 213.1% / 215.6% | 213.9% / 215.9% | 206.8% / 208.6% |
| storm-gdma | 3,248,548 / 3,281,206 | 3,171,715 / 3,203,608 | 2,876,062 / 2,901,220 | 41.1% / 41.5% | 40.1% / 40.5% | 36.4% / 36.7% |
| storm-ppu | 31,101,566 / 31,470,508 | 30,224,496 / 30,544,076 | 28,561,042 / 28,862,232 | 393.6% / 398.2% | 382.5% / 386.5% | 361.4% / 365.2% |

- The worst case in `itcm-dtcm`: 3,296,548 / 3,334,278 cycles, 41.7% /
  42.2% of a frame, **83.4% / 84.4% of the 50% line** (3,951,279). From
  flash 129.5% / 131.1% (`flash-dtcm`) and 131.3% / 132.9% (`flash-axi`)
  of the line.
- `stack used=3440` (`flash-dtcm`, `itcm-dtcm`), 3432 (`flash-axi`).

**472 against 483.86 MHz**, cycles per frame at 472 over the "480 MHz"
run below, mean (max):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` |
|---|---|---|---|
| worst-case | 1.018 (1.018) | 1.007 (1.008) | 1.002 (1.002) |
| cgb-raster | 1.005 (1.004) | 0.995 (0.994) | 0.999 (0.999) |
| dmg-raster | 1.008 (1.008) | 1.010 (1.010) | 0.998 (0.998) |
| event-storm | 1.034 (1.033) | 0.998 (0.998) | 1.001 (1.001) |
| cpu_instrs | 1.001 (0.989) | 1.006 (1.004) | 1.001 (1.000) |
| dma-storm | 0.992 (0.992) | 1.005 (1.005) | 1.018 (1.018) |
| halt-irq | 0.996 (0.997) | 1.019 (1.019) | 1.002 (1.002) |
| apu-writes | 0.995 (0.995) | 1.024 (1.020) | 0.999 (0.999) |
| oam-lines | 0.995 (0.995) | 1.004 (1.004) | 1.001 (1.000) |
| storm-control | 0.992 (0.991) | 1.007 (1.007) | 1.002 (1.002) |
| storm-apu | 1.036 (1.036) | 1.005 (1.005) | 1.000 (1.000) |
| storm-spsw | 1.023 (1.020) | 0.997 (0.998) | 0.985 (0.985) |
| storm-div | 0.984 (0.984) | 1.015 (1.014) | 1.017 (1.017) |
| storm-gdma | 1.000 (0.998) | 0.993 (0.991) | 0.998 (0.994) |
| storm-ppu | 1.002 (1.003) | 0.991 (0.990) | 1.004 (1.004) |

- Not "unchanged": 0.984x to 1.036x. The worst case in `itcm-dtcm` is
  1.002x; `itcm-dtcm` is within 0.4% on twelve workloads and 1.5% to
  1.8% off on `dma-storm`, `storm-spsw` and `storm-div`; the flash placements
  move by up to 3.6% (`flash-axi` `storm-apu` and `event-storm`).
- The clock is not the likely cause. The wait states are the same (4)
  and every clock is a fixed ratio of the CPU clock, so a cycle count
  should not depend on the absolute frequency; the two builds differ only
  in this crate's board code (clock setup, banner, clock-check support),
  which moves where the code after it lands. An
  integer-PLL 472 MHz `bench-itcm-dtcm-pll480` (DIVM1 4 code, 761049b
  to 9730bd3, exact build not recorded; no `pll` line) was found running on the board at the
  start of these runs, with matching hashes: against it, at the same
  nominal clock, today's build is 0.979x (`storm-div`) to 1.013x
  (`dma-storm`), 0.998x on the worst case. So rebuilding alone moves
  counts by about 2% on some workloads; a figure here carries that much
  build-to-build uncertainty. Not separated further.

### NUCLEO-H753ZI at 480 MHz, 2026-11-25

`pll480` (VOS0 via ODEN, HSE bypass 8 MHz, flash 4 wait states, AXI
240 MHz = CPU clock / 2, APB 120 MHz), images built at 7d4b026 with all
fifteen workloads. In every placement all fifteen lines' `calls`,
`frames`, `clocks` and `hash` equal `expected-hashes.txt` (checked by a
diff of the lines without `min`/`mean`/`max`), so the six storms and the
four `BENCHES` workloads added on 2026-11-25 ran on the board for the
first time and matched the PC and QEMU. Banners: `clock=pll480
cpu_hz=480000000`, `flash_acr=0x24`, `oden=1`, `syscfg_pwrcr=0x00000081`;
`itcm_bytes=64024` in `itcm-dtcm`.

**Clock correction**: the clock of these runs was HSE-based and measured
483.86 MHz, not 480 ("Clock check"), over the CPU and AXI maxima; the
images now run 472 MHz nominal from the HSI. These figures are kept as
history: the current ones are "NUCLEO-H753ZI at 472 MHz" above, measured
with the corrected clock, at 0.984x to 1.036x of the counts below (not
identical, see there); the % below are of a 480 MHz frame.

Cycles per frame, mean / max, and % of a 480 MHz frame (8,036,499 cycles
at 59.7275 Hz):

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` | % `flash-axi` | % `flash-dtcm` | % `itcm-dtcm` |
|---|---|---|---|---|---|---|
| worst-case | 5,096,080 / 5,159,618 | 5,079,014 / 5,142,534 | 3,291,153 / 3,328,005 | 63.4% / 64.2% | 63.2% / 64.0% | 41.0% / 41.4% |
| cgb-raster | 2,959,943 / 3,013,762 | 2,894,227 / 2,942,521 | 2,607,183 / 2,646,327 | 36.8% / 37.5% | 36.0% / 36.6% | 32.4% / 32.9% |
| dmg-raster | 2,338,765 / 2,372,186 | 2,335,260 / 2,367,324 | 1,927,981 / 1,950,733 | 29.1% / 29.5% | 29.1% / 29.5% | 24.0% / 24.3% |
| event-storm | 2,117,855 / 2,144,660 | 2,127,321 / 2,143,998 | 2,014,882 / 2,028,825 | 26.4% / 26.7% | 26.5% / 26.7% | 25.1% / 25.2% |
| cpu_instrs | 1,542,280 / 2,165,586 | 1,531,800 / 2,137,526 | 1,294,743 / 1,767,346 | 19.2% / 26.9% | 19.1% / 26.6% | 16.1% / 22.0% |
| dma-storm | 3,275,046 / 3,283,766 | 3,211,272 / 3,219,188 | 3,033,551 / 3,039,197 | 40.8% / 40.9% | 40.0% / 40.1% | 37.7% / 37.8% |
| halt-irq | 750,757 / 753,528 | 739,531 / 742,716 | 656,976 / 658,854 | 9.3% / 9.4% | 9.2% / 9.2% | 8.2% / 8.2% |
| apu-writes | 1,140,966 / 1,165,990 | 1,137,090 / 1,163,168 | 897,762 / 919,997 | 14.2% / 14.5% | 14.1% / 14.5% | 11.2% / 11.4% |
| oam-lines | 1,684,800 / 1,706,478 | 1,673,067 / 1,691,083 | 1,447,229 / 1,463,331 | 21.0% / 21.2% | 20.8% / 21.0% | 18.0% / 18.2% |
| storm-control | 2,512,771 / 2,539,448 | 2,504,796 / 2,528,921 | 2,243,418 / 2,261,753 | 31.3% / 31.6% | 31.2% / 31.5% | 27.9% / 28.1% |
| storm-apu | 25,641,531 / 25,919,538 | 25,614,716 / 25,848,430 | 23,957,661 / 24,170,419 | 319.1% / 322.5% | 318.7% / 321.6% | 298.1% / 300.8% |
| storm-spsw | 15,472,765 / 15,681,128 | 15,353,951 / 15,500,482 | 14,791,973 / 14,934,687 | 192.5% / 195.1% | 191.1% / 192.9% | 184.1% / 185.8% |
| storm-div | 17,117,331 / 17,322,872 | 16,649,536 / 16,832,276 | 16,062,214 / 16,212,035 | 213.0% / 215.6% | 207.2% / 209.4% | 199.9% / 201.7% |
| storm-gdma | 3,247,102 / 3,287,728 | 3,193,040 / 3,234,018 | 2,882,474 / 2,919,847 | 40.4% / 40.9% | 39.7% / 40.2% | 35.9% / 36.3% |
| storm-ppu | 31,032,565 / 31,384,376 | 30,503,428 / 30,852,328 | 28,457,135 / 28,756,285 | 386.1% / 390.5% | 379.6% / 383.9% | 354.1% / 357.8% |

The `itcm-dtcm` report, verbatim (the second repeat; the first was cut
by the VCP):

```
h7bench v1 banner board=NUCLEO-H753ZI image=bench placement=itcm-dtcm clock=pll480 cpu_hz=480000000 baud=115200
h7bench v1 power cr3=0x00000042 csr1=0x0000e000 d3cr=0x0000e000 actvosrdy=1 supply=ldo
h7bench v1 clock rcc_cr=0x0307c025 cfgr=0x0000001b d1cfgr=0x00000048 flash_acr=0x24 rsr=0x00fe0000
h7bench v1 cpu cpuid=0x411fc271 idcode=0x20036450 icache=1 dcache=1 cyccnt=1 itcm_bytes=64024 dtcm_table_bytes=2304
h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 msp=0x2001fbd0
h7bench v1 options optsr=0x1bc6aaf0
h7bench v1 vos0 syscfg_pwrcr=0x00000081 oden=1 pwrcr_before=0x00000000 polls_to_drop=2 polls_low=6
h7bench v1 worst-case calls=120 frames=119 clocks=8420792 min=785630 mean=3291153 max=3328005 hash=de590b88792b5778
h7bench v1 cgb-raster calls=120 frames=118 clocks=8370434 min=471712 mean=2607183 max=2646327 hash=28517598fd7f8e17
h7bench v1 dmg-raster calls=120 frames=118 clocks=8411052 min=785364 mean=1927981 max=1950733 hash=08ce100a696ecbd6
h7bench v1 event-storm calls=120 frames=120 clocks=8426718 min=510009 mean=2014882 max=2028825 hash=b03f0faee1117987
h7bench v1 cpu_instrs calls=600 frames=597 clocks=42131558 min=371132 mean=1294743 max=1767346 hash=d0e9954397cd56f4
h7bench v1 dma-storm calls=120 frames=120 clocks=8426720 min=3026614 mean=3033551 max=3039197 hash=502bf87511d799cb
h7bench v1 halt-irq calls=120 frames=120 clocks=8426716 min=645064 mean=656976 max=658854 hash=8a1f79e8e93d84ab
h7bench v1 apu-writes calls=120 frames=120 clocks=8426716 min=895725 mean=897762 max=919997 hash=ad45a7bd9328fccb
h7bench v1 oam-lines calls=120 frames=119 clocks=8396532 min=831633 mean=1447229 max=1463331 hash=39e08ed53ff29055
h7bench v1 storm-control calls=120 frames=119 clocks=8420234 min=553498 mean=2243418 max=2261753 hash=f8e810318f7827c6
h7bench v1 storm-apu calls=120 frames=119 clocks=8420234 min=1198471 mean=23957661 max=24170419 hash=9138ca07a7e124a2
h7bench v1 storm-spsw calls=120 frames=119 clocks=8357218 min=116364 mean=14791973 max=14934687 hash=bbdfa57379371fa3
h7bench v1 storm-div calls=120 frames=119 clocks=8420232 min=957871 mean=16062214 max=16212035 hash=7b2635c400f5d2fe
h7bench v1 storm-gdma calls=120 frames=119 clocks=8420232 min=581567 mean=2882474 max=2919847 hash=6292eca8fdd0188a
h7bench v1 storm-ppu calls=120 frames=119 clocks=8420234 min=1525407 mean=28457135 max=28756285 hash=31e4b880f2666ad4
h7bench v1 stack used=3440 of=77728
h7bench v1 end pass=2
```

The two flash placements printed the same fifteen `calls`/`frames`/
`clocks`/`hash` fields; their cycle figures are the table's.

**480 against 392 MHz**, cycles per frame (mean) at 480 over 392, for the
five workloads both clocks ran:

| Workload | `flash-axi` | `flash-dtcm` | `itcm-dtcm` |
|---|---|---|---|
| worst-case | 1.102 | 1.106 | 1.002 |
| cgb-raster | 1.064 | 1.030 | 1.001 |
| dmg-raster | 1.035 | 1.046 | 1.003 |
| event-storm | 1.001 | 1.010 | 1.000 |
| cpu_instrs | 1.047 | 1.041 | 1.001 |

- The worst case in `itcm-dtcm`: 3,291,153 / 3,328,005 cycles, 41.0% /
  41.4% of a real-time frame, 81.9% / 82.8% of the 50% bar (4,018,249).
  From flash it is 63.4% / 64.2%: over the bar by 27% / 28%.
- `itcm-dtcm` costs the same cycles as at 392 MHz (within 0.3%), as it
  did between 64 and 392 MHz. The flash placements cost up to 10.6% more
  cycles: a flash access is now 5 AXI clocks = 10 CPU cycles (4 wait
  states) against 6 at 392 MHz. So ITCM is now 1.55x faster than flash on
  the worst case (1.41x at 392 MHz) and 1.05x to 1.27x on the others.
- DTCM against AXI SRAM: under 3% on everything (`flash-dtcm` against
  `flash-axi`), as before.
- No workload of the bound or of `BENCHES` comes near a real-time frame
  (the highest is the worst case at 41.4% in `itcm-dtcm`). The storms,
  outside the bound by decision, run at 1.8 to 3.6 frame times per frame
  in `itcm-dtcm` (`storm-ppu` the dearest: 28.46 M / 28.76 M cycles,
  357.8% at most); `storm-control` and `storm-gdma` are under 40%.
- `stack used=3440` (`flash-dtcm`, `itcm-dtcm`), 3432 (`flash-axi`).
- The raw serial logs are not committed (session scratch).

**The clock, measured from the PC.** `selftest-pll480` prints a line each
480,000,000 DWT cycles (the `cyccnt=` field). A PowerShell reader
timestamped each line with .NET `Stopwatch` for 125 s; a least-squares fit
of the cumulative `cyccnt` over the 125 lines that were not backlog gives
**484.31 MHz**, largest residual 36 ms. The Windows clock itself was
checked against an NTP server (`w32tm /stripchart`): 1 ms of drift in
60 s. So the CPU runs about 0.9% fast, which puts the HSE input (the
ST-LINK's MCO) at about 8.07 MHz rather than 8.000. That is inside the
~1% the check allows, and cycle counts do not depend on it (the % of a
frame above are against the nominal 480 MHz, so real time has 0.9% more
margin than they show), but it means the core and the 240 MHz AXI/flash
clock run about 0.9% above their DS12117/RM0433 maxima. Confirmed on chip
against the LSE crystal (483.86 MHz, MCO 8.064 MHz) and fixed: see "Clock
check" and the clock correction above.

The board is left running `bench-itcm-dtcm-pll480` as built at b4d6c81
(fractional 472 MHz clock; see "NUCLEO-H753ZI at 472 MHz").

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
  whether the H755 board was power-cycled between runs. On the H753 the
  value read (0x42) is also what ST's HAL in the board's factory firmware
  would have written, so it does not prove this firmware's write either:
  the H753 was not power-cycled on 2026-11-25 (agent runs cannot).
- Why the H753's ST-LINK dropped off USB on 2026-11-25 after
  `bench-flash-axi-pll` was copied to it (the human unplugging it, the PC
  cutting the port, or the board's supply with the display shield fitted);
  and so whether the `*-pll` bench images run on the H753 at all.
- That the Cortex-M4 sitting in lockup has no effect (nothing suggests one:
  the cycle counts repeat exactly).
- `m4-stub`: built, never flashed; no longer built (ADR 0036).
- On the lib (ADR 0036): `bench-flash-axi-pll480`, `bench-flash-dtcm-pll480`
  and `clockcheck-pll480` are built, not run; the `tcmprobe` path for a
  selftest with its RAM in DTCM (the `.bss` word) is built, not run (the
  selftest image keeps its RAM in AXI SRAM).
- Why the ST-LINK's MCO is 8.064 MHz and wanders ("Clock check"): its
  source inside the ST-LINK is not documented here. No longer used.
- The LSE crystal's own accuracy (not measured with an instrument; the
  HSI reading inside its datasheet range is the cross-check).
- The 392 MHz bench and selftest images with the fractional PLL1
  (b4d6c81): rebuilt, not run. `clockcheck-pll` was: 393.29 MHz (HSI
  64.211 MHz, +0.33%), under the 400 MHz VOS1 limit (397.9 MHz at the
  HSI's +1.5% bound).

## Flashing order on a board that has not run this

1. `selftest-pll480.bin`: LD1 on then toggling at 1 Hz, LD2 on, banner
   with `clock=pll472`, `oden=1`, `flash_acr=0x24`, `supply=ldo`,
   `actvosrdy=1 icache=1 dcache=1 mpu=1 cyccnt=1` and a plausible
   `lse_cpu_hz=` (about 473.6 MHz), one `selftest` line a second with
   `cycles=` repeating exactly, and a `tcmprobe` line whose three values
   are equal. Then unplug and replug USB and check the same again (the
   real test of the supply write).
2. `bench-flash-axi-pll480.bin`: the report lines; compare `calls`,
   `frames`, `clocks`, `hash` with `expected-hashes.txt` (and with
   `host/`'s `cargo run --release` if the core changed since it was made).
3. `bench-flash-dtcm-pll480.bin`, then `bench-itcm-dtcm-pll480.bin`: same
   hashes, lower cycle counts; `itcm_bytes=` non-zero in the banner.
4. Any time: `clockcheck-pll480.bin` to measure the clock against the LSE
   (`avg_cpu_hz_measured=`; read for a minute).
