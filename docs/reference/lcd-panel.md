# The shield's LCD: an ST7789V on FMC bank 1

The X-NUCLEO-GFX02Z1's 2.2" 240x320 TFT, driven over an 8080-I 8-bit bus
by the H753's FMC. First contact on 2026-11-25 with the `lcd-probe` image
(`crates/gbc-nucleo-h753/src/bin/lcd-probe.rs`, driver `src/lcd.rs`).

## Which controller: ST7789V, not ILI9341V

UM2905 section 6.2 lists two variants of the shield: **XNGFX02Z1$AZ1**
(LCD DT022CTFT, ILI9341V) and **XNGFX02Z1$AZ2** (LCD TCXD022IBLON-5,
ST7789V). The plan assumed the first. This board has the second:

```
lcd id panel=st7789v rddid=858552 rdid1_3=858552 rddpm=0x9c rddmadctl=0x08 rddcolmod=0x05 madctl_ok=1
lcd te rising_edges_per_s=60
```

- **RDDID (0x04) and RDID1-3 (0xDA-0xDC) = 85 85 52.** These are the
  ST7789V's default IDs (ST7789V datasheet V1.3 sections 9.1.46-9.1.48:
  85h, 85h, 52h after power on, SW reset and HW reset). On an ILI9341V
  they are 00 00 00 unless the module maker programmed the MTP (ILI9341V
  section 8.2.3).
- **TE at 60 Hz.** The ST7789V's default frame rate is 60 Hz (FRCTRL2
  RTNA = 0Fh, section 9.2.16); the ILI9341V's is 70 Hz (FRMCTR1).
- **RDID4 (0xD3)** read 00 00 00 on the first probe. It is ILI9341V-only
  (and needs EXTC high there); the probe no longer sends it to an
  ST7789V.
- **The ILI9341V's RGBSET (0x2D) LUT had no effect**: an all-zero LUT
  left 32 written pixels reading back unchanged. The ST7789V has no
  RGBSET command.

**Unverified:** the board's sticker (`XNGFX02Z1$AZ2` expected). A human
can read it.

The ST7789V datasheet is not in `../gbc-32-hardware/datasheets/`. The copy
used here is Sitronix "ST7789V Datasheet Version 1.3, 2014/03", 316 pages,
fetched 2026-11-25 from
`https://www.newhavendisplay.com/appnotes/datasheets/LCDs/ST7789V.pdf`
(sha256 `8ecf0e438aa25554efc473a4cfe6e436f3fb33e68cd876b25041557235d04ca1`).
It should be added to the hardware repo's datasheet list (human; that repo
is read-only to agents).

The driver only sends what both controllers define the same way: the MIPI
DCS commands SWRESET 01, SLPOUT 11, DISPON 29, CASET 2A, PASET 2B, RAMWR
2C, RAMRD 2E, TEON 35, MADCTL 36 (same bit layout: MY MX MV ML RGB/BGR
MH), COLMOD 3A (same fields), and the reads 04, 0A, 0B, 0C, DA-DC. The
8080-I timing tables of the two datasheets have the same numbers
(ILI9341V section 18.3.1; ST7789V section 7.4.1 Table 4, which adds TAHT
and TCSH, 10 ns each). The reset timing is the same too (ILI9341V 15.4,
ST7789V 7.4.5). So the driver should run either variant. Only the
ST7789V was tested.

## Pins and alternate functions

UM2905 Rev 2 Table 2 (and the Zio tables 6-8, footnote 1 lists the
NUCLEO-H753ZI), checked against DS12117 Rev 10 Table 9, column by column:
AF12 is "FMC/SDMMC1/MDIOS/OTG_FS1/LCD" and carries the FMC signal on
every bus pin.

| LCD pin | Signal | GPIO | AF12 function (DS12117 Table 9) | Set up as |
|---|---|---|---|---|
| DB0 | DISP_D0 | PD14 | FMC_D0/FMC_DA0 | AF12 |
| DB1 | DISP_D1 | PD15 | FMC_D1/FMC_DA1 | AF12 |
| DB2 | DISP_D2 | PD0 | FMC_D2/FMC_DA2 | AF12 |
| DB3 | DISP_D3 | PD1 | FMC_D3/FMC_DA3 | AF12 |
| DB4 | DISP_D4 | PE7 | FMC_D4/FMC_DA4 | AF12 |
| DB5 | DISP_D5 | PE8 | FMC_D5/FMC_DA5 | AF12 |
| DB6 | DISP_D6 | PE9 | FMC_D6/FMC_DA6 | AF12 |
| DB7 | DISP_D7 | PE10 | FMC_D7/FMC_DA7 | AF12 |
| RD | DISP_NOE | PD4 | FMC_NOE | AF12 |
| WR | DISP_NWE | PD5 | FMC_NWE | AF12 |
| CS | DISP_NE1 | PD7 | FMC_NE1 | AF12, pull-up |
| RS (D/CX) | DISP_DCX | PE4 | **FMC_A20** (verified) | AF12 |
| RESET | DISP_NRESET | PE11 | (FMC_D8) | GPIO output, high |
| FMARK (TE) | DISP_TE | PE13 | (FMC_D10) | GPIO input |

All at very high speed, push-pull. IM[3:0] are tied to GND on the shield
(8080-I, 8-bit). PE11 and PE13 must not be AF12 (they would become FMC
data lines of a 16-bit bus). Nothing else in the firmware uses these pins
(UART PD8/PD9, QSPI PD11-13/PE2/PB2/PG6, LEDs PB0/PB14/PE1).

## Addresses, mapping and the MPU

- FMC bank 1 NOR/PSRAM subbank 1 (NE1) is 0x6000_0000 to 0x63FF_FFFF in
  the default mapping, BCR1.BMAP = 00 (RM0433 Rev 9 Figure 98, Table 156,
  Table 157: ADDR[27:26] = 00). BMAP is left at 00: no remap is needed,
  because the MPU (not the default map) decides the window's attributes.
- For an 8-bit bank the address on FMC_A[25:0] is the byte address
  ADDR[25:0] itself (RM0433 Table 158; 16-bit banks shift by one). So
  FMC_A20 is address bit 20: **command at 0x6000_0000 (D/CX low), data at
  0x6010_0000 (D/CX high)**. Verified on the board: every command and
  readback works with these addresses.
- MPU region 7 (`mpu::REGION_FMC`): base 0x6000_0000, 2 MiB (covers both
  addresses), Device (TEX 000 C 0 B 1), read/write, execute-never. The
  rest of bank 1 and of the FMC space stays region 0's no-access,
  strongly-ordered, so nothing (speculative or not) reaches it. The
  default map makes the FMC banks Normal memory (RM0433 section 22.6),
  which allows speculative reads, merged and reordered writes: wrong for
  a command/data register. Device keeps order and size and lets the write
  buffer post stores (strongly-ordered would not). The region is opened
  only after bank 1 is configured and enabled (an access to a disabled
  bank is an AXI error, RM0433 22.7.6 MBKEN) and closed before the FMC is
  reconfigured.
- An AXI access wider than the 8-bit bus is split into byte accesses
  (RM0433 22.7.2, Table 171), lowest address first: a 32-bit store to the
  data address sends its low byte first. Verified by readback (the fill
  uses u32 stores).

## ES0392 FMC errata

Only 2.6.4 applies: unaligned FMC reads are unsupported. Every read here
is a byte read, and Device memory faults on unaligned accesses anyway.
2.6.1, 2.6.3 and 2.6.5 concern synchronous memories or the continuous
clock (CCLKEN = 0 here), 2.6.2, 2.6.6 and 2.6.7 NAND and SDRAM.

## FMC configuration

- RCC: AHB3ENR.FMCEN, a reset pulse on AHB3RSTR.FMCRST, D1CCIPR.FMCSEL
  (kernel clock) and CKPERSEL = 00 (per_ck = hsi_ker_ck), switched with
  the FMC's bus clock off.
- BCR1 = 0x8000_5081: FMCEN, EXTMOD (separate read and write timings),
  WREN, reserved bit 7 = 1 (RM0433 Table 174), MWID 00 (8 bits), MTYP 00
  (SRAM), MUXEN 0, MBKEN; write FIFO on, CCLKEN 0, no NWAIT.
- Writes in **mode A** (BWTR1): NWE low for DATAST cycles, then high for 1
  cycle before the transaction ends and ADDSET cycles of the next one, so a
  byte takes ADDSET + DATAST + 1 kernel cycles (RM0433 Figure 102). No gap
  between consecutive writes to one bank (22.7.6 BUSTURN).
- Reads in **mode D** (BTR1): NOE high for ADDSET + ADDHLD, low for
  DATAST, then BUSTURN (which mode D inserts after a read, 22.7.6). Mode D
  because the frame-memory read needs RD high for 90 ns (trdhfm), and mode
  A's only high phase is ADDSET, at most 15 cycles = 64 ns at 236 MHz.
  FMC_NADV toggles in mode D but is not routed (PB7 stays GPIO).

### Timings and their arithmetic

`lcd::Timing::new` computes the cycles from the panel minimums at the
kernel clock's **highest** frequency: both kernel clocks come from the
HSI, at most +1.5% (64.96 MHz; `clock.rs`), so 236 MHz is at most
239.5 MHz (T = 4.174 ns) and 64 MHz at most 64.96 MHz (T = 15.39 ns).

Panel minimums used (ns): twc 66, twrl 15, twrh 15, tcs 15, tdst 10;
read (frame memory) trdlfm 355, trdhfm 90, trcfm 450, tratfm 340 (max),
trod 80 (max). FMC side (DS12117 Table 55/57): data valid at most T + 2.5
after NE low, data setup 11 before NOE high.

Write rules: DATAST ≥ ⌈twrl/T⌉; ADDSET ≥ ⌈tcs/T⌉ and ADDSET + 1 ≥
⌈twrh/T⌉; ADDSET + DATAST + 1 ≥ ⌈twc/T⌉; the spare cycles go half to each
phase; (ADDSET + DATAST)·T ≥ T + 2.5 + tdst (asserted).
Read rules: DATAST ≥ max(⌈trdlfm/T⌉, ⌈(tratfm + 11)/T⌉); ADDSET + ADDHLD
≥ ⌈trdhfm/T⌉ and ≥ ⌈trcfm/T⌉ − DATAST; BUSTURN = min(15, ⌈trod/T⌉).

| Timing | Kernel | Write ADDSET/DATAST | Cycles/byte | Bus (nominal) | Read ADDSET/ADDHLD/DATAST/BUSTURN | BTR1 / BWTR1 |
|---|---|---|---|---|---|---|
| `FAST` (`hclk3`) | rcc_hclk3 236 MHz | 8 / 7 | 16 = 67.8 ns | 14.75 MB/s | 11 / 11 / 86 / 15 | 0x3FFF56BB / 0x0FF00718 |
| `SPEC` (`perck`) | per_ck = HSI 64 MHz | 2 / 2 | 5 = 78.1 ns | 12.80 MB/s | 3 / 3 / 24 / 6 | 0x3FF61833 / 0x0FF00212 |
| `SAFE` (`perck-x2`) | per_ck = HSI 64 MHz | 4 / 4 | 9 = 140.6 ns | 7.11 MB/s | 6 / 6 / 47 / 6 | 0x3FF62F66 / 0x0FF00414 |

`SAFE` doubles every panel minimum (the conservative fallback).

**`FAST` is out of spec on the hold times**, which no FMC setting can
stretch: mode A ends one kernel cycle after NWE rises. The FMC
guarantees data hold T + 0.5 = 4.7 ns (DS12117 Table 57 th(Data_NWE))
against tdht 10 ns; address (D/CX) hold T − 0.5 = 3.7 ns (th(A_NWE))
against the ST7789V's TAHT 10 ns; NE hold T = 4.2 ns (th(NE_NWE)) against
TCSH/tcsf 10 ns. At 64 MHz all three are ≥ 14.9 ns, so `SPEC` meets
every minimum. `FAST` gave no error on this board at room temperature
(below), but that is one sample, not a margin. Also at 236 MHz, BUSTURN
(at most 15 cycles = 63 ns) cannot cover trod (80 ns), so the driver
waits 1 µs after every read burst before it writes again
(`Lcd::end_read`).

**Recommendation for M3: `SPEC`** (in spec everywhere; 12.8 MB/s). A
240x216 RGB565 frame (103,680 B) takes 8.10 ms at `SPEC` against 7.03 ms
at `FAST`; both leave more than half of a 16.74 ms frame. 160x144 takes
3.60 ms. A 1.5x row (480 B) takes 37.5 µs at `SPEC`; at the ST7789V's 60
Hz a scanned line lasts about 16.67 ms / 320 = 52 µs (porches not
counted, so a little less), so the write front stays ahead of the scan as
the M3 tearing plan needs.

## Init sequence (`Lcd::init_panel`)

1. RESX (PE11) low 1 ms (tRW ≥ 10 µs), high, wait 120 ms (tRT max, the
   NV load). ILI9341V 15.4, ST7789V 7.4.5.
2. SWRESET (0x01), wait 120 ms (5 ms, or 120 ms before SLPOUT if sent in
   sleep-out mode; ILI9341V 8.2.2).
3. COLMOD (0x3A) = 0x55: 16 bits per pixel on the MCU interface (and the
   unused RGB-interface field 101). Read back with RDDCOLMOD: 0x05 (the
   ST7789V reports the RGB-interface field as 0 here; the MCU field is
   101, 16 bpp).
4. MADCTL (0x36) = **0x08**; read back 0x08.
5. SLPOUT (0x11), wait 120 ms (5 ms before the next command, 120 ms before
   SLPIN).
6. DISPON (0x29). TEON (0x35) = 0x00 (V-blank TE only).

Power, VCOM, porch and gamma registers keep their reset values (loaded
from the module's NV memory). RDDPM reads 0x9C: booster on, sleep out,
normal mode, display on. Image quality with these defaults is a human
check.

### MADCTL = 0x08 and orientation

MY = MX = MV = 0: the native portrait scan, 240-pixel rows written in the
order the panel refreshes them (ML = 0 top to bottom, MH = 0), which the
M3 tearing plan relies on (the write front follows the scan). BGR = 1, as
the plan asks: the 5-bit field in RGB565 bits 15-11 is sent to the
panel's blue subpixels if the panel's filter is RGB-ordered, so the core's
R-low RGB555 (`xBBBBBGGGGGRRRRR`) goes out with a shift and no channel
swap. The datasheets cannot say which way the glass is mounted or which
filter order the TCXD022IBLON-5 has, so **two things need a human look**
at the panel while `lcd-probe` runs:

- **Colour order.** The probe fills solid 0xF800, 0x07E0, 0x001F, white,
  black (0.25 s each) and then leaves 8 vertical bars, left to right in
  memory order: field 15-11 alone, 10-5 alone, 4-0 alone, 15-11 + 10-5,
  10-5 + 4-0, 15-11 + 4-0, white, then a grey ramp running the other way;
  each bar fades from full at the top (row 0) to black at the bottom.
  The plan wants the first bar **blue** and the third **red**. If it is
  the other way round, MADCTL becomes 0x00 (one bit).
- **Orientation.** A white 16x16 square marks memory (0, 0). It should be
  in the corner the human considers top-left when the board is held as
  the player will hold it; otherwise MX/MY change (and the TE/scan
  argument must be rechecked if MV changes).

## Readback (RAMRD, 0x2E)

RAMRD returns a dummy byte, then 3 bytes per pixel, R, G, B, each a 6-bit
value in D7..D2 (ILI9341V 8.2.24 and 7.6.3; ST7789V 9.1.23); D1..D0 are
masked. Frame memory holds the data as written: MADCTL's BGR bit only
changes the panel output (ILI9341V 8.2.29), and readback confirms it.

The 16 → 18 bit expansion of the 5-bit red and blue fields is not stated
in text in either datasheet (the ST7789V shows a "look-up table" box in a
figure; the ILI9341V's RGBSET LUT has no effect on this controller).
**Measured:** red and blue get **green's LSB** as their new LSB:
R6 = R5·2 + G0, B6 = B5·2 + G0. `lcd::expected_readback` uses that rule.
The "MSB repeated" rule gives 57,728 mismatches on the noise pattern, so
the comparison does detect wrong values.

## Measurements (2026-11-25, CPU 472 MHz, DWT cycles)

Each timing in turn, full panel re-init per timing, two rounds each read:

| Timing | Fill 153,600 B, u32 stores | u8 stores | Readback noise / bars (76,800 px) | Full GRAM read |
|---|---|---|---|---|
| `FAST` | 10,410 µs, **14.75 MB/s** | 14.75 MB/s | 0 / 0 mismatches | 62.6 M cycles, 133 ms |
| `SPEC` | 11,996 µs, **12.80 MB/s** | 12.80 MB/s | 0 / 0 mismatches | 68.3 M cycles, 145 ms |
| `SAFE` | 21,593 µs, **7.11 MB/s** | 7.11 MB/s | 0 / 0 mismatches | 117.6 M cycles, 249 ms |

- The measured fill rates equal the computed bus rates to 0.03%: the CPU
  stores never starve the bus (the FMC write FIFO absorbs them), and u8
  and u32 stores are the same speed. The CPU is busy for the whole fill,
  though: a CPU fill costs 32 CPU cycles per byte at `FAST` and 37 at
  `SPEC` (M3's MDMA transfer removes that).
- Readback costs about 272 CPU cycles per byte at `FAST` (a 123-kernel-
  cycle FMC read, 521 ns, plus AXI latency).
- High-bit mismatches (R/B top 5 bits, G 6 bits) were 0 from the first
  run, before the LSB rule was known: no bus error at any timing.
- TE: 60 rising edges in 1 s at every timing (±1 from the gate).

## Sources

- RM0433 Rev 9: 22.6 (Figure 98, Tables 156-158), 22.7.2 (Table 171),
  22.7.4 (Figures 102, 108; Tables 174-176), 22.7.6 (BCR, BTR, BWTR), 8.7.19
  (D1CCIPR FMCSEL, CKPERSEL).
- DS12117 Rev 10: Table 9 (AF), Tables 55 and 57 (FMC asynchronous
  timings).
- ES0392 Rev 16: section 2.6.
- UM2905 Rev 2: Table 2 (LCD pins), Tables 6-8 (Zio), section 6.2
  (variants).
- ILI9341V datasheet V1.00: 7.4, 7.6.3, 8.2.2, 8.2.3, 8.2.24, 8.2.29,
  8.2.33, 8.3.23, 15.4, 18.3.1.
- ST7789V datasheet V1.3 (Sitronix, see above): 7.4.1 Table 4, 7.4.5, 8.8.3,
  9.1.3, 9.1.7, 9.1.23, 9.1.28, 9.1.32, 9.1.46-48, 9.2.16.
