# ROM loader on the board (measured)

The `loader` image (`crates/gbc-nucleo-h753/src/bin/loader.rs`), its
session logic (`src/loader.rs`, host-tested by `tools/gbc-pc/tests/board.rs`)
and the design (ADR 0038). PC side: `scripts/gbc-pc.sh` (`tools/gbc-pc`).
All figures are from 2026-11-25, NUCLEO-H753ZI + X-NUCLEO-GFX02Z1
(MX25L6433F), ST-LINK VCP on COM5, CPU 472 MHz, QSPI 39 MHz for
erase/program and 59 MHz (0xEB) for window reads.

## Use

```
crates/gbc-nucleo-h753/flash.sh crates/gbc-nucleo-h753/out/loader.bin
scripts/gbc-pc.sh hello
scripts/gbc-pc.sh load <rom>          # gbc-pc defaults to --baud 2000000
H7_BAUD=2000000 tools/h7-bench/read-serial.sh 15 | tr -d '\000' | grep 'loader status'
```

Saves and live input (M5: `load --sav`, `dump`, `play`): `link-protocol.md`.

At reset the image prints the banner at 115200 and a `boot` line (the
stored image as `rom_image::validate` sees it), then serves at
`GBC_LOADER_BAUD` (build time; default 2,000,000). While idle it repeats the
banner and a `status` line every 10 s at the serving rate: the stored
header now, the boot validation, the load in progress, counters, UART
errors, `exit_fallbacks` and the slowest answer per request type.

## Baud rate

Each rate: the loader built with `GBC_LOADER_BAUD=<rate>`, flashed, then
`hello` and two full loads of different 4 MiB pseudo-random images
(AES-128-CTR keystream, fixed key; not ROMs), each erasing and programming
the whole range. Counters are gbc-pc's (`resends, naks, timeouts, stale,
corrupt`) and the board's (`overruns`, `framing`, `noise`,
`bad_frames`, `duplicates`).

| Rate | Clean | Load 1 | Load 2 | gbc-pc counters | Board counters |
|---|---|---|---|---|---|
| 115,200 | yes (1 load) | 403.11 s, 10,405 B/s | (AlreadyLoaded) | all 0 | all 0 |
| 921,600 | yes | 75.68 s, 55,423 B/s | 75.60 s, 55,477 B/s | all 0 | all 0 |
| 2,000,000 | yes | 50.61 s, 82,874 B/s | 50.54 s, 82,982 B/s | all 0 | all 0 |
| 4,000,000 (really 4,068,966, +1.72%) | yes | 39.70 s, 105,652 B/s | 39.67 s, 105,738 B/s | all 0 | all 0 |

**Correction (M2 review H-1, 2026-11-25).** The "4,000,000" row ran at
8x with BRR 0x35, which the hardware reads as USARTDIV 58, not 59:
4,068,966 bit/s, +1.72% (`qspi-nor.md` "UART"; `uart::baud_setting`
computed the rate from the odd divisor BRR cannot hold). The other rows
were 16x and are as stated. After the fix, 4 M is 16x with USARTDIV 30,
3,933,333 bit/s (-1.67%); no setting gives it exactly (29.5 kernel clocks
per bit). Re-measured with `run-loader-roundtrip.sh` (two variants each,
each load over a different image, so each is a full erase and
program; `docs/reference/m2-evidence.md`):

| Rate (actual) | Clean | Variant 1 | Variant 2 | gbc-pc counters | Board counters |
|---|---|---|---|---|---|
| 4,000,000 (3,933,333, -1.67%) | yes | 38.94 s, 107,719 B/s | 39.44 s, 106,352 B/s | all 0 | all 0 |
| 2,000,000 (exact) | yes | 50.04 s, 83,827 B/s | 49.98 s, 83,927 B/s | all 0 | all 0 |

**Chosen: 2,000,000** (16x, USARTDIV 59, exact at the 118 MHz kernel
clock). The rule: 4 M only if its loads are clean *and* the combined error
budget can be stated. The loads were clean, but the budget cannot be: the
receiver tolerance at 16x with BRR[3:0] != 0 is 3.33% for both ends
together (RM0433 Tables 406/407), the board takes 1.67% of it at 4 M, and
the ST-LINK/V3E's own rate error and receiver tolerance at 4,000,000 are
not documented anywhere local (UM2407 says nothing about VCP rates; ST's
UM2448/TN1235 could not be fetched). At 2 M the board's error is 0, so the
whole 3.33% is the ST-LINK's. The cost is about 11 s per 4 MiB load
(about 0.2 s for a 64 KiB ROM). Rates above 4 M: 6 M is 5,900,000 at 16x
(-1.67%, the same budget question as 4 M), 8 M and 12 M need 8x, where
-1.67% leaves 0.33% of a 2% budget.

Where the time goes at the old 4 M setting (4,068,966; 39.7 s for 4,096 chunks, 9.7 ms each): the
wire time of a 1,040-byte frame is 2.6 ms; four page programs ~1.4 ms
(`qspi-nor.md`); the 64 erases of 64 KiB (about 0.31 s each, ~20 s) are
about half the total; the rest is USB and turnaround. A faster link would
gain little unless the erase moved off the critical path.

## Reply times (board side, slowest seen)

From the `status` line, `slowest` (cycles from the frame's last byte to
the reply being ready), over all the loads above: Hello 0.0 ms;
RomBegin up to 456 ms (boot-time-like `validate` of a stored 4 MiB image,
~142 ms, plus the block-0 erase); RomChunk up to 365 ms (a 64 KiB erase
plus programming); RomEnd 284.5 ms (two 4 MiB hashes through the window:
the read-back header, then `validate`). All well inside the PC's 2 s.
`validate` of a stored 4 MiB image at boot: 142.2 ms (~29.5 MB/s through
the window, about its 59 MHz quad limit). `qspi::exit_fallbacks()` stayed
0 in the loader across every load (each load enters and leaves the window
three times, plus once per status line).

## Interrupted load

A valid 64 KiB image was loaded, then a 4 MiB load was started and its
gbc-pc process killed from WSL (`taskkill.exe /IM gbc-pc.exe /F`) after
~10 s, at 1,080,320 bytes. The status line then read `image=none(Erased)
state=loading(1080320/4194304)`, and after a reset (re-flash) `boot
image=none(Erased)`: the old image's header went with the block-0 erase at
`RomBegin`, and no header was written for the partial one (ADR 0037). A
fresh `load` then completed normally.

## Real data in the window (`qspi-mmap`)

With the 4 MiB image stored (header `GBC32ROM`, len 0x400000 in
`first16`), `qspi-mmap` passed every run: `compare segments=102
bytes=90766 mismatches=0 eb_mismatches=0 errors=0 PASS`, `unaligned
cold=0/3072 warm=0/3072 uncached=0/3072 PASS`, `data ff_bytes=39820
of=90766 distinct_values=256` (the 0xFF bytes are the erased upper 4 MiB
its segments also cover). Its own `exit_fallbacks` rose by one per run
(in `compare`), as recorded in `qspi-nor.md` ("ABORT alone doesn't always
idle the QUADSPI"): the same once-per-run case, independent of the data.

## Running the headless image from QSPI (plan Stage 2 M2)

`out/headless-qspi.bin` (`--features rom-qspi`, build.sh) is the M1
`headless` loop without an embedded ROM: at boot it maps the NOR (0xEB,
59 MHz), runs `rom_image::validate` over the window and runs 600 frames on
the image's data at 0x9000_0000 (cached). With no valid image it prints
`headless qspi error stage=<map|header|validate> error=<e>` once a second
and runs no frame. One command loads a ROM and compares:

```
cd crates/gbc-nucleo-h753 && ./build.sh
./run-headless-qspi.sh ../../test-roms/blargg/cpu_instrs/cpu_instrs.gb ../../tools/headless/expected/cpu_instrs.txt
./run-headless-qspi.sh ../../test-roms/cgb-acid2/cgb-acid2.gbc ../../tools/headless/expected/cgb-acid2.txt
```

It holds the board lock across three steps: flash `out/loader.bin`,
`scripts/gbc-pc.sh load <rom>` (a no-op when the NOR already holds that
ROM), then `RUN_HEADLESS_ROM=<rom> run-headless.sh out/headless-qspi.bin
<expected>`. Besides the M1 checks (a complete report, `image_bytes`/
`image_cksum` equal to the `.bin`), `RUN_HEADLESS_ROM` requires the
report's `rom_len`/`rom_hash` (the NOR header, which the image checked
against the data) to be the file's length and FNV-1a 64 computed by the
script, so a stale NOR image exits 2 (checked on a saved capture: another
ROM, one changed hash digit and a report without the fields all exit 2).
`GBC_BUILD_LOCK=<file>` builds gbc-pc under that lock first.

2026-11-25, both ROMs: 602 of 602 `headless` lines equal, image CRC and
ROM hash matched, `exit_fallbacks=0` (the image never leaves
memory-mapped mode). Boot `validate`: 1,051,582 cycles for cpu_instrs
(64 KiB, 2.23 ms, ~29.4 MB/s), 527,294 for cgb-acid2 (32 KiB). Loads at
the old 4 M setting (4,068,966 bit/s): 64 KiB in 0.82 s, 32 KiB in 0.46 s. Cycles per frame:
`docs/perf.md`, "`headless` image on the board".

## Several ROMs in one image: the bench pack (plan Stage 2 M2)

The NOR holds one ADR 0037 image, and the `itcm-dtcm-qspi` bench needs
fifteen workload ROMs. They travel as one **bench pack**: a file whose
bytes are, to the loader and to `rom_image`, just the image's data (its
length and FNV-1a 64 are the header's, so `validate` covers all of it; the
header's title field is the pack's bytes 0x134..0x144, meaningless). The
pack's own layout (`tools/h7-bench/src/pack.rs`, written by
`tools/h7-bench/host`'s `pack <file>`, `out/h753/bench-pack.bin`): an
8-byte magic `H7BPACK1`, the entry count (15), then one 32-byte entry per
workload in report order (name NUL-padded to 24 bytes, offset, length),
then the ROMs, each at a multiple of 256 bytes after the table, 0xFF
between them; 524,800 bytes for the current fifteen. The bench image parses
it at boot and refuses anything else (`qspi pack none stage=pack`).
`tools/h7-bench/run-qspi.sh` does loader, load and bench run under one
board lock; tools/h7-bench/README.md "QSPI ROM placement".
