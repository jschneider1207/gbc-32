# Stage 2 M2 acceptance: board evidence

What the M2 acceptance rests on, as commands anyone can re-run, and the
key lines of the last run of each (M2 review T-2: before this, the 4 MiB
image generator was not committed, no script did the round trip, and the
captures were gone). NUCLEO-H753ZI + X-NUCLEO-GFX02Z1 (MX25L6433F),
ST-LINK VCP on COM5, CPU 472 MHz.

**Run on commit `f0d6a3e`** (2026-11-25; rebased onto main as `dc36238`, which differs from it only in `docs/plan/stage-2.md`), clean tree, every image built
from it by `build.sh` (here and in `tools/h7-bench`). The loader served at
2,000,000 bit/s (exact; `loader.md` "Baud rate"). The full captures were
kept outside the repository; the lines below are copied from them.

## Setup

```
export GBC_BUILD_LOCK=<lock file>   # optional: build under flock
export GBC_TEST_ROMS=<repo>/test-roms
(cd crates/gbc-nucleo-h753 && ./build.sh)
(cd tools/h7-bench && H7_BENCH_CPU_INSTRS=$GBC_TEST_ROMS/blargg/cpu_instrs/cpu_instrs.gb ./build.sh)
```

## The test image

```
crates/gbc-nucleo-h753/gen-test-image.sh <out> [variant]
```

4 MiB of AES-128-CTR keystream (openssl, key `6762632d33322d6d322d6c6f61646572`,
IV 15 zero bytes then the variant), deterministic:

| Variant | FNV-1a 64 |
|---|---|
| 1 | `fd2f394f6aeae85c` |
| 2 | `4ee1deb90295c485` |

gbc-pc computed the same hash for the file (`ROM: 4194304 bytes, FNV-1a
64 0xfd2f394f6aeae85c`). The figures in `loader.md` "Baud rate" from
before this file came from images that were not kept; the re-measured rows
there use these two.

## Loader round trip

```
cd crates/gbc-nucleo-h753
./run-loader-roundtrip.sh --variant 1 --interrupt
```

Under one board lock: generate, flash the loader, load (gbc-pc's FNV of the
file, `verified hash`, link counters all 0), the loader's status line
(stored header, board receive counters), load again (`already loaded`),
flash `qspi-mmap` and check it, then flash the loader again, kill a load of
variant 2 after 10 s and check the status line. Exit 0 pass, 1 fail, 2 tool
error. Without `--interrupt` the NOR is left holding the image.

```
loaded 4194304 bytes in 50.08 s (83749 B/s effective), verified hash 0xfd2f394f6aeae85c
link: resends 0, naks 0, timeouts 0, stale 0, corrupt 0, other 0
gbc-nucleo-h753 loader status image=valid len=4194304 hash=0xfd2f394f6aeae85c ... overruns=0 framing=0 noise=0 ... baud=2000000
already loaded (length and hash match)
gbc-nucleo-h753 qspi-mmap compare segments=102 bytes=90766 rounds=12 mismatches=0 eb_mismatches=0 errors=0 fallback_rounds=0x400 first_bad=none PASS
gbc-nucleo-h753 qspi-mmap unaligned bases=8 offsets_per_base=48 kinds=8 cold=0/3072 warm=0/3072 uncached=0/3072 (mismatches/checked) errors=0 PASS
gbc-nucleo-h753 loader status image=none(Erased) boot_image=valid len=4194304 hash=0xfd2f394f6aeae85c boot_validate=142.2ms state=loading(845824/4194304) ... bad_frames=0 duplicates=0
run-loader-roundtrip: PASS variant=1 fnv1a64=fd2f394f6aeae85c baud=2000000; loaded 4194304 bytes in 50.08 s (83749 B/s effective); overruns=0 framing=0 noise=0 bad_frames=0 duplicates=0; reload already loaded; qspi-mmap 7 run(s) PASS first16=4742433332524f4d0100000000004000; interrupted at loading(845824/4194304): image=none(Erased)
```

`first16` is the image header's start: `GBC32ROM`, version 1, flags 0,
length 0x400000 (ADR 0037). The same script, without `--interrupt`, gave
the four clean loads of the baud re-measurement in `loader.md` (variants 1
and 2 at the corrected 4 M and at 2 M).

## Headless loop with the ROM from QSPI

```
cd crates/gbc-nucleo-h753
./run-headless-qspi.sh $GBC_TEST_ROMS/blargg/cpu_instrs/cpu_instrs.gb ../../tools/headless/expected/cpu_instrs.txt
./run-headless-qspi.sh $GBC_TEST_ROMS/cgb-acid2/cgb-acid2.gbc ../../tools/headless/expected/cgb-acid2.txt
```

Both exit 0. Same `headless-qspi.bin` for both (image_bytes=211896
image_cksum=3253251680).

```
loaded 65536 bytes in 0.85 s (76954 B/s effective), verified hash 0x9bc9250d7f185f9d
gbc-nucleo-h753 headless qspi image rom_len=65536 rom_hash=9bc9250d7f185f9d title=CPU_INSTRS validate_cycles=1051528 qspi_hz=59000000 exit_fallbacks=0
stats run_frame min=377172 mean=1297601 max=1769327 max_frame=548 hz=472000000
run-headless.sh: image matched: image_bytes=211896 image_cksum=3253251680
run-headless.sh: ROM matched: rom_len=65536 rom_hash=9bc9250d7f185f9d (exit_fallbacks=0)
run-headless.sh: EQUAL: 602 headless lines match .../tools/headless/expected/cpu_instrs.txt

loaded 32768 bytes in 0.55 s (59507 B/s effective), verified hash 0xe00f807ed1acf443
gbc-nucleo-h753 headless qspi image rom_len=32768 rom_hash=e00f807ed1acf443 title=CGB-ACID2 validate_cycles=527240 qspi_hz=59000000 exit_fallbacks=0
stats run_frame min=500715 mean=693354 max=759909 max_frame=0 hz=472000000
run-headless.sh: image matched: image_bytes=211896 image_cksum=3253251680
run-headless.sh: ROM matched: rom_len=32768 rom_hash=e00f807ed1acf443 (exit_fallbacks=0)
run-headless.sh: EQUAL: 602 headless lines match .../tools/headless/expected/cgb-acid2.txt
```

## Bench with the workload ROMs from QSPI (`itcm-dtcm-qspi`)

```
cd tools/h7-bench
./run-qspi.sh out/h753/bench-itcm-dtcm-pll480.bin
```

Loads the bench pack, runs every workload from internal flash and then
from the NOR, and checks both against `expected-hashes.txt`. Exit 0.

```
loaded 524800 bytes in 6.78 s (77389 B/s effective), verified hash 0xb8aead06e9d0dc4e
check-report.sh: banner image_bytes=728688 image_cksum=1739811902 = cksum of .../out/h753/bench-itcm-dtcm-pll480.bin
check-report.sh: flash: 15 lines equal expected-hashes.txt
check-report.sh: h7bench v1 qspi pack ok len=524800 hash=b8aead06e9d0dc4e roms=15 same_as_flash=1 validate_cycles=8408324 qspi_hz=59000000 window=cached exit_fallbacks=0
check-report.sh: qspi: 15 lines equal expected-hashes.txt
check-report.sh: OK
```

Cycles per frame (mean / max):

| Workload | internal flash | QSPI | QSPI vs internal |
|---|---|---|---|
| worst-case | 3,300,648 / 3,338,357 | 3,378,959 / 3,419,683 | +2.37% / +2.44% |
| cgb-raster | 2,605,945 / 2,644,955 | 2,609,400 / 2,650,056 | +0.13% / +0.19% |
| dmg-raster | 1,918,090 / 1,940,291 | 1,921,199 / 1,945,073 | +0.16% / +0.25% |
| event-storm | 2,025,911 / 2,040,021 | 2,029,090 / 2,045,544 | +0.16% / +0.27% |
| cpu_instrs | 1,295,151 / 1,768,195 | 1,296,049 / 1,768,243 | +0.07% / +0.00% |
| dma-storm | 3,097,364 / 3,102,665 | 3,101,523 / 3,106,896 | +0.13% / +0.14% |
| halt-irq | 657,811 / 659,709 | 660,238 / 663,584 | +0.37% / +0.59% |
| apu-writes | 897,664 / 920,005 | 899,455 / 922,529 | +0.20% / +0.27% |
| oam-lines | 1,448,372 / 1,464,438 | 1,449,882 / 1,466,356 | +0.10% / +0.13% |
| storm-control | 2,242,961 / 2,261,293 | 2,246,567 / 2,267,023 | +0.16% / +0.25% |
| storm-apu | 23,959,134 / 24,171,920 | 23,960,042 / 24,172,688 | +0.00% / +0.00% |
| storm-spsw | 14,708,607 / 14,842,123 | 14,711,957 / 14,847,200 | +0.02% / +0.03% |
| storm-div | 16,036,214 / 16,178,844 | 16,037,036 / 16,179,896 | +0.01% / +0.01% |
| storm-gdma | 2,925,429 / 2,952,203 | 3,337,803 / 3,780,305 | +14.10% / +28.05% |
| storm-ppu | 28,429,800 / 28,730,950 | 28,430,478 / 28,731,813 | +0.00% / +0.00% |

The QSPI overheads match the earlier run in `tools/h7-bench/README.md`
(+2.4% worst case, +14% / +28% storm-gdma). Absolute internal-flash means
moved against that run by under 0.9% for the bound workloads (dma-storm
+0.83%, worst case +0.02%) and up to 2.6% for the storms (storm-div
-2.59%, storm-spsw +1.66%): a rebuilt image (the lib's `uart` changed),
within the +-2% code-layout spread noted in STATUS ("Risk for M6") for
all but storm-div, which is outside the bound.

After this run the board holds `bench-itcm-dtcm-pll480` in internal flash
and the bench pack in the NOR.
