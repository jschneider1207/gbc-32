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
