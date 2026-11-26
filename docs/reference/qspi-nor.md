# The shield's QSPI NOR: Macronix MX25L6433F

The X-NUCLEO-GFX02Z1's 64-Mbit QSPI NOR (U1). UM2905 names no part number.
Identified on the board on 2026-11-25 with the `qspi-probe` image
(`crates/gbc-nucleo-h753/src/bin/qspi-probe.rs`, driver `src/qspi.rs`).
It used read commands only: nothing was written to the NOR.

## How it was identified

The probe used 1-line SPI, CKMODE 0, SSHIFT 1, kernel clock HCLK3 236 MHz.
It read at PRESCALER 23 (9.83 MHz) and PRESCALER 5 (39.3 MHz). Six reports
were read back to back, all identical, and the two clocks agreed:

```
qspi jedec slow=c22017 fast=c22017 match=1 clk_hz=39333333
qspi rems id_90=c216
qspi sfdp sig_ok=1 nph=2 hdr=53464450000101ff00000109300000ffc2000104600000ff
qspi ptab id=0xff00 rev=1.0 dwords=9 ptr=0x000030 d=fff120e5,03ffffff,6b08eb44,bb043b08,ffffffee,ff00ffff,ff00ffff,520f200c,ff00d810 density_bits=67108864
qspi ptab id=0xffc2 rev=1.0 dwords=4 ptr=0x000060 d=26503600,6477f99e,ffffcffe,ffffffff
qspi status sr1_05=40 rdcr_15=0000 rdscur_2b=00
```

- **JEDEC ID (0x9F):** `C2 20 17`. C2 is Macronix (JEP106 bank 1). 0x20
  is the MX25L family. 0x17 is 64 Mbit.
- **REMS (0x90):** `C2 16`.
- **SFDP (0x5A):** JESD216 rev 1.0, with two parameter tables: the 9-dword
  basic table (BFPT) and the Macronix table (ID 0xFFC2). Every byte matches
  the MX25L6433F datasheet's SFDP tables (Tables 11 to 13, "for
  MX25L6433F...-08G"), including the Macronix table's VCC range of
  2.650 to 3.600 V and its software-reset word `F99E`.

The JEDEC and REMS IDs alone fit several Macronix 64-Mbit parts
(MX25L6406E, 6433F, 6436F, 6473F, 6445E). The SFDP tables and the 2.65 V
minimum single out the 6433F. Community reports and ST's X-CUBE-DISPLAY
flash loader name the same part for this shield. **Unverified:** the
package and grade suffix (e.g. `ZNI-08G`). Reading it needs the chip's
marking.

## Facts that matter here

Source: MX25L6433F datasheet, P/N PM2130 Rev. 1.4 (2016-03-10), unless
noted. VCC on the shield is 3.3 V (UM2905 section 5.3.2).

| Item | Value |
|---|---|
| Size | 8 MiB, 3-byte addresses only (BFPT dword 1 bits 18:17 = 00) |
| Page | 256 B (PP 0x02 1-1-1, 4PP 0x38 1-4-4) |
| Erase | 4 KB sector 0x20, 32 KB block 0x52, 64 KB block 0xD8, chip 0x60/0xC7 (BFPT dwords 8 and 9 agree) |
| Read 0x03 (READ) | 50 MHz max (fR), no dummy cycles |
| Read 0x0B (FAST_READ) | 133 MHz (fC), 8 dummy cycles |
| Read 0x6B (1-1-4, QREAD) | 133 MHz, 8 dummy cycles |
| Read 0xEB (1-4-4, 4READ) | CR.DC = 0 (the default and the current value): 6 dummy cycles (2 "performance enhance" mode cycles + 4), 104 MHz at VCC ≥ 3 V (80 MHz at 2.65 to 3 V). CR.DC = 1: 10 dummy cycles, 133 MHz (Table 7). SFDP BFPT dword 3 says 0xEB: 4 wait states + 2 mode clocks |
| Read 0xBB (1-2-2) | 4 dummy cycles at 104 MHz (DC = 0); 8 at 133 MHz (DC = 1) |
| Clock duty cycle | tCH, tCL ≥ 45% of the period: an even QUADSPI divider is needed (ES0392 2.7.6, RM0433 23.5.1) |
| CS# deselect | tSHSL ≥ 15 ns after a read, ≥ 50 ns after a write, erase or program |
| Program time | page typ 0.33 ms, max 1.2 ms; byte typ 10 µs, max 50 µs |
| Erase time | 4 KB typ 25 ms, max 200 ms; 32 KB 0.14 s / 0.6 s; 64 KB 0.25 s / 1 s; chip 20 s / 60 s |
| WRSR cycle | tW max 40 ms |
| Endurance | 100,000 erase/program cycles |
| DTR | **No.** BFPT dword 1 bit 19 = 0, and there is no DTR command in Table 4 |
| QPI (4-4-4) | **No.** BFPT dword 5 bit 4 = 0, and there is no enter-QPI command (Table 4) |
| Reset | RSTEN 0x66, then RST 0x99. No RESET# pin; pin 7 is HOLD#/SIO3 |

### The quad-enable bit

QE is **status register bit 6**. It is non-volatile, and the factory
default is 0. It is set with WREN (0x06) followed by WRSR (0x01), which
takes the status byte and optionally the configuration byte (two bytes
change both), then waits on WIP (status bit 0, tW ≤ 40 ms). With QE = 1,
WP#/SIO2 and HOLD#/SIO3 are data pins, and the WP# and HOLD functions are
off.

**On this board, QE is already 1: status register = 0x40.** BP3..BP0 = 0
(nothing is write-protected), SRWD = 0, WEL = 0, WIP = 0. No write is
needed to use 0xEB or 0x6B. Whoever set QE (ST, or an earlier user) is
unknown. A later loader should check QE and set it only if it is clear.

The configuration register (RDCR 0x15) reads 0x00:
- DC = 0 (6 dummy cycles for 4READ);
- TB = 0 (top-area protection; TB is OTP, never write it);
- ODS = 0 (the datasheet contradicts itself on which ODS value is the
  default, so its meaning is **unverified**).

DC is **volatile**. To use 10 dummy cycles at 133 MHz, every boot must
issue a WRSR that writes the status byte as well, which is a non-volatile
status-register write. That is a trade-off for the M6 QSPI-tuning task.

The security register (RDSCUR 0x2B) reads 0x00: OTP not locked, no
program or erase suspended, no program or erase failure flagged.

### 0xEB mode bits

The two cycles after the address carry the "performance enhance" bits
P7..P0. If P7..P4 toggle against P3..P0 (e.g. 0xA5, 0x5A, 0xF0, 0x0F),
the part stays in enhance mode and expects the next command with no
instruction byte (sections 10-12 and 10-13). The safe option for
memory-mapped mode is one alternate byte of 0xFF on 4 lines (2 cycles)
plus 4 dummy cycles. That avoids leaving the IOs floating during the mode
cycles, which a plain 6 dummy cycles would do (RM0433 23.3.4: IOs are
high-Z in a quad dummy phase). Alternatively, use enhance mode on purpose
together with QUADSPI's SIOO (send instruction only once, RM0433
23.3.13): that is a tuning option for M6. Enhance mode blocks
program/erase Resume until it is exited (datasheet section 10-29).

## Memory-mapped mode (as set up on 2026-11-25)

The driver is `Qspi::memory_mapped` / `Mapped` in
`crates/gbc-nucleo-h753/src/qspi.rs`, the MPU region is in `src/mpu.rs`,
and the on-board checks are in the `qspi-mmap` image
(`src/bin/qspi-mmap.rs`). Read commands only: nothing was written to the
NOR.

| Setting | Value | Why |
|---|---|---|
| Command | 0xEB 1-4-4: instruction on 1 line, 24-bit address on 4, one alternate byte 0xFF on 4 (2 cycles), DCYC 4, data on 4 | 6 dummy cycles in all at CR.DC = 0 (Table 7). 0xFF doesn't enter enhance mode (see above) |
| CCR | `0x0F10EDEB` (FMODE 11, DMODE 11, DCYC 4, ABSIZE 00, ABMODE 11, ADSIZE 10, ADMODE 11, IMODE 01, 0xEB); SIOO, DDRM, DHHC, FRCM all 0 | RM0433 23.5.6 |
| ABR | `0xFF` | |
| Clock | HCLK3 236 MHz / 4 = 59 MHz (PRESCALER 3; CR = `0x03000011`: SSHIFT, EN) | an even divider (ES0392 2.7.6). Faster clocks are M6 tuning |
| DCR | `0x00170200`: FSIZE 23 (**16 MiB**), CSHT 2 (3 cycles, 51 ns) | ES0392 2.7.5: a memory-mapped read of the last byte of the FSIZE region reads 0x00, and the next read of it stalls the AXI bus. The margin puts that byte at 0x90FF_FFFF, outside the MPU window. Seen on the board: with FSIZE 22 (exactly 8 MiB), the image hung in its first compare round |
| TCEN | **0** (no timeout, LPTR unused) | after an access the QUADSPI fills its FIFO and then holds NCS low with the clock stopped, so the next sequential read skips the 20-clock command (instruction 8 + address 6 + mode 2 + dummy 4 = 0.34 us at 59 MHz). TCEN = 1 would save the NOR's selected-idle current and pay that cost again after every idle period. The NUCLEO is USB-powered, so latency wins. Stage 3 can revisit |
| Prefetch | always on in memory-mapped mode (RM0433 23.3.7; there is no switch) | |
| MPU | region 6: base 0x9000_0000, 8 MiB, Normal write-back no write-allocate (TEX 000 C 1 B 1), not shareable, read-only at both privilege levels (AP 110), XN. RASR = `0x1603002D` | Normal memory is needed for unaligned loads. The other 248 MB of the bank stay region 0's (no-access, XN, strongly-ordered) |

Order, in `qspi.rs`:
- **Enter:** BUSY = 0, TCEN cleared, ABR, CCR with FMODE = 11, DSB, then
  MPU region 6 on (DSB, ISB).
- **Exit:** MPU region 6 off (DSB, ISB), then ABORT. If BUSY is still
  high, clear EN (see below). Then the ES0392 2.7.4 sequence (required
  when leaving memory-mapped mode), restore DCR and CR, then clean and
  invalidate the D-cache, so the loader never reads stale lines after a
  program or erase.

**Observed: ABORT alone doesn't always idle the QUADSPI.** Sometimes the
abort completes (ABORT clears, TCF sets) but BUSY stays high and FLEVEL
climbs to 30: SR went from `0x620` before the abort to `0x1E22` after it.
That is a new prefetch, even though the MPU window was already closed and
a DSB had completed every CPU access. In a given build it happens in the
same place every time: once per run, after compare round 10 of 12. It
moved with the code layout: the first time it showed up was in a
mutation build, as a panic. Clearing EN always brought BUSY down (RM0433
23.3.7: BUSY falls on "a timeout, ... an abort, or the peripheral is
disabled"), and every later indirect and memory-mapped read was correct.
`qspi::exit_fallbacks()` counts these cases. **Unverified:** the cause.
One candidate is a speculative M7 line fill that was in flight and
reached the QUADSPI after the abort. Neither ES0392 nor RM0433 describes
this behaviour.

MPU guidance (**unverified** against the app notes themselves, which aren't
local): an ST moderator on community.st.com, citing AN4861 section 4.6,
says the unused part of the 256 MB QUADSPI space "must be set to strongly
ordered XN". ST's AN4760 example makes the mapped part strongly-ordered
too. That can't work here: the core needs unaligned reads, and those fault
on Device and strongly-ordered memory.

### On-board checks (`qspi-mmap`)

All runs below were with the D-cache on, at 472 MHz.

- **compare:** 102 segments, 90,766 B per run. The segments cover the
  start, the end of the device (including its last byte, 0x7FFFFF), the
  64 KiB and 4 MiB boundaries, 32 page boundaries (±128 B, with odd
  starts) and 64 random 1 KiB pieces from a fixed-seed LCG, read in 12
  rounds of up to 8 KiB. Each round compares indirect 0x03 (at 39 MHz)
  against indirect 0xEB (at 59 MHz) and against the window (a slice
  compare). Each round leaves and re-enters memory-mapped mode.
- **unaligned:** 8 line-aligned bases (64, 0x1000, 0x20020, 0x7FFFC0 up
  to the last byte, and 4 random ones). At every offset from base − 8 to
  base + 40 it checks eight access kinds against the 0x03 reference:
  - a byte read;
  - u16 and u32 `from_le_bytes` of a slice (the core's pattern);
  - `read_unaligned` u16 and u32;
  - raw LDRH and LDR (inline asm; CCR.UNALIGN_TRP = 0);
  - a 16-byte `copy_from_slice` (the core's VRAM DMA block).

  Each kind is checked three ways: cold (lines invalidated before each
  access), warm, and with the window non-cacheable. That is 3,072 checks
  each way.
- **Result:** every captured run passed, with 0 mismatches: 24 runs of one
  build and 10 of the final one. A deliberate read of 0x9080_0000 (past
  the window) faults with `cfsr=0x00000082 mmfar=0x90800000`, with the MPU
  still on in HardFault (HFNMIENA = 1).
- **The tests are weak: the NOR is blank.** All 90,766 compared bytes
  were 0xFF, the only byte value seen. A wrong data shift would mostly go
  unnoticed. A mutation that flipped one reference byte failed as
  expected: 12 compare mismatches (one per round), and 29 unaligned
  mismatches in each cache state (the number of checks that cover that
  byte). The loader task must re-run these checks on a known image.

### Read latency (59 MHz, 0xEB, CPU 472 MHz; DWT cycles, median of 64)

| Access | Cycles | Time | Note |
|---|---|---|---|
| One u32, cold, cacheable (line invalidated: a line fill, critical word first) | 362 to 385 (max 587) | 0.77 to 0.82 us | a new command each time (random address) |
| One 32-byte line, cold, read word by word | 770 | 1.63 us | about (20 + 64) QUADSPI clocks x 8, plus about 100 cycles of bus overhead |
| One u32, non-cacheable, random | 376 to 388 | 0.80 us | (20 + 8) x 8 = 224, plus overhead |
| The next u32 after it, non-cacheable (already prefetched) | 120 | 0.25 us | the AXI-to-QUADSPI round trip, with no flash access |
| 64 KiB copy, cold (into DTCM, 4 KiB at a time) | 1,048,950 | 2.22 ms = **28.1 MB/s** | the wire limit at 59 MHz x 4 bits is 29.5 MB/s |

The cost of a random miss is about **770 cycles per 32-byte line**. That
is the figure the plan wants as a stand-in for Stage 3 HyperRAM. At
59 MHz it is roughly 85% wire time, so a faster clock (M6) scales it
almost linearly.

## Clock options on the H753

- The QUADSPI is good for ≤ 133 MHz SDR at VDD ≥ 2.7 V (DS12117
  Table 73).
- With the kernel clock on HCLK3 = 236 MHz, the even dividers give
  118 MHz (/2) and 59 MHz (/4). 118 MHz needs CR.DC = 1, a WRSR at every
  boot. 59 MHz works with DC = 0.
- For 104 MHz with DC = 0, use another kernel clock: QSPISEL =
  pll1_q_ck or pll2_r_ck (RM0433 section 8.7.19; ES0392 2.7.6 suggests
  these). PLL2 is unused today.
- The driver now refuses odd division factors (`Qspi::init` asserts).

## Sources

- Macronix, MX25L6433F datasheet, PM2130 Rev. 1.4, 2016-03-10. Read from
  the distributor copy at https://static.rapidonline.com/pdf/73-3773.pdf.
  Macronix publishes later revisions at macronix.com; those were not
  compared (**unverified** whether any timings changed).
- ST community: the GFX02Z1 flash is the MX25L6433F
  (https://community.st.com/t5/stm32-mcus-products/stm32h5-support-for-mx25l6433f/td-p/659928);
  also the Zephyr shield docs
  (https://docs.zephyrproject.org/latest/boards/shields/x_nucleo_gfx02z1/doc/index.html).
- JESD216 (SFDP), for the BFPT field layout. It was not read here: the
  decoding follows the datasheet's annotated tables, which use the same
  layout.
- RM0433 Rev 9 chapter 23; DS12117 Rev 10 Table 9 (AF numbers) and Table
  73; ES0392 Rev 16 section 2.7; UM2905 Rev 2 Table 3 and section 5.3.
- Memory-mapped mode: RM0433 Rev 9 sections 23.3.7, 23.3.12, 23.3.15,
  23.5.1 (TCEN) and 23.5.13 (LPTR). ES0392 Rev 16 sections 2.7.4 and
  2.7.5. Armv7-M ARM B3.5 (MPU, HFNMIENA) and A3.2.1 (unaligned
  accesses). The MPU guidance for the unused QUADSPI space comes from an
  ST community thread
  (https://community.st.com/stm32-mcus-products-25/speculative-access-and-stm32h7-memory-protection-unit-16674),
  which cites AN4861 section 4.6. AN4861 and AN4760 themselves were not
  read.
