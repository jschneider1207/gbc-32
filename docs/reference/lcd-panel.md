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

## MDMA and TE (`lcd_dma`, `lcd-dma-probe`)

The frame path: the CPU scales the core's 160x144 frame into a 240x216
RGB565 buffer in AXI SRAM (`gbc_firmware::video::scale_frame`, wire byte
order), then MDMA copies the buffer to the data address, started from the
TE interrupt. Code: `crates/gbc-nucleo-h753/src/lcd_dma.rs`; probe:
`src/bin/lcd-dma-probe.rs`; decision: ADR 0039. Measured 2026-11-25 at
472 MHz, FMC timing `SPEC` (per_ck 64 MHz, 5 cycles per byte), four runs.

### Scheme

1. The main loop takes the buffer (`LcdDma::buffer`: waits only for the
   previous transfer), converts into it, calls `LcdDma::present`: CASET/PASET
   for the window, the channel programmed and enabled (its registers only
   take writes with EN = 0), the transfer marked pending.
2. TE rises (start of V-blank). The EXTI15_10 handler (`lcd_dma::on_te`)
   clears EXTI13's pending bit, and if a transfer is pending writes RAMWR
   (2Ch) to the command address, DSB, then SWRQ.
3. MDMA streams 103,680 bytes; the channel disables itself and sets CTCIF.
   The main loop sees it at its next `busy`/`buffer`/`wait` (polled; the
   MDMA interrupt stays off in the NVIC).

### MDMA channel 0 (RM0433 Rev 9 chapter 14, registers 14.5)

| Register | Value | Fields |
|---|---|---|
| CxTCR | 0xE3FC_4222 | BWM 1, SWRM 1, TRGM 10 (one request = the whole repeated block), PAM 00, PKE 1, TLEN 127 (128-byte buffer), DBURST 000, SBURST 100 (16 beats), DINCOS 00, SINCOS 10, DSIZE 00 (byte), SSIZE 10 (word), DINC 00 (fixed), SINC 10 (increment) |
| CxBNDTR | 0x0010_CA80 | BRC 1 (2 blocks), BNDT 51,840 (BNDT is at most 65,536, so the frame takes two blocks) |
| CxBRUR, CxLAR, CxTBR | 0 | the source runs on across the block boundary, destination fixed; no linked list; SBUS = DBUS = 0 (AXI: AXI SRAM and the FMC) |
| CxSAR / CxDAR | buffer / 0x6010_0000 | the buffer (word aligned, 0x2400_0000 here) / the data address |
| CxCR | 0xC1, then 0x1_00C1 | PL very high, EN; SWRQ from the interrupt |

Read back after a transfer: TCR 0xE3FC4222, BNDTR 0 (BRC 0, BNDT 0), SAR
0x2401_9500 (= base + 103,680), DAR 0x6010_0000, CR 0xC0 (EN cleared by
hardware), ISR 0 (flags cleared), ESR 0.

**PKE behaves as designed:** each 32-bit read is unpacked into four byte
writes, lowest address first (RM0433 14.5.6 PKE: "right aligned, in
little-endian mode"), so the buffer's memory order is the wire order.
Verified by readback: 0 mismatching pixels in 60 + 1 frames x 51,840
pixels in every run. Not tried: DSIZE = word without packing (not needed).

**The FMC write FIFO keeps the bus busy:** the transfer takes **3,822,720
to 3,822,742 CPU cycles = 8.099 ms** (from SWRQ to CTCIF; 810 buffer transfers of 128 B),
exactly 103,680 B at the `SPEC` bus rate of 12.80 MB/s (8.100 ms). Byte
writes from MDMA with BWM = 1 do not starve the FMC. (BWM = 1 because the
MDMA AXI master has one outstanding non-bufferable write; not measured
with BWM = 0.) No transfer error (TEIF) in any run.

### The buffer and its MPU region

`lcd_dma::DmaBuffer`: `#[repr(align(131072))]`, 128 KiB, in `.axisram`
(NOLOAD) at 0x2400_0000. MPU region 8 (`mpu::REGION_DMA_BUF`, set by
`LcdDma::new`): 128 KiB over exactly it, Normal non-cacheable (TEX 001 C 0
B 0), RW, XN; inside region 4 (AXI SRAM), so it grants no new access. The
D-cache is cleaned and invalidated over the range before and after the
region is enabled. **Checked by mutation:** with the region left out (the
buffer write-back cached, no maintenance) the readback gave 474,387
mismatching pixels over 60 frames and the control 7,887 instead of 2;
with it, 0. MDMA does not go through the MPU.

The buffer is zeroed once, at the end of that setup (`mpu::dma_buffer_window`,
an explicit `strd` loop; the audio buffer too): AXI SRAM is 64-bit ECC
(RM0433 3.2) and NOLOAD, so a first 32-bit store to a never-written word would
be a read-modify-write of a word with a bad ECC (M3 review P-7).

Timeouts: `LcdDma::wait` giving up, or an accessor that waited 250 ms
(`SETTLE_CYCLES`), calls `abort`: with interrupts off the state goes to IDLE
(so the TE interrupt's compare-exchange from PENDING fails), EN is cleared and
waited for, the flags are cleared, and `LcdDma::aborts` counts it (M3 review
P-1). `lcd-dma-probe` runs a self-test of this before its frames
(`timeout-selftest`: `wait(1)` with a transfer pending; checked on the board:
`timed_out=1 aborts=1 idle=1 en_after=0 ok=1`, then 60 frames with 0
mismatches). With the state reset removed from `abort` the same self-test
printed `idle=0 ok=0`.

### TE: EXTI13 on PE13

PE13 input, no pull. SYSCFG_EXTICR4 EXTI13 = 0100 (port E, RM0433
12.3.5); EXTI_RTSR1 bit 13 = 1, FTSR1 bit 13 = 0; EXTI_CPUIMR1 bit 13 =
1; pending cleared by writing bit 13 of EXTI_CPUPR1 (then DSB); NVIC IRQ
40 (EXTI15_10). TEON (35h) parameter 00h: mode 1, V-blank only (ST7789V
9.1.27, 8.15.1).

- **TE high time = the porch lines.** 1,154 µs with the reset porches
  (24 lines x 48.9 µs = 1,175 µs), 1,588 µs with 34 (34 x 47.3 =
  1,608 µs): TE is high for FPA + BPA lines (and so, presumably, falls
  when the scan of row 0 begins).
- **TE to transfer start:** 57 to 85 CPU cycles (0.12 to 0.18 µs) from the
  handler's entry to the SWRQ write; the exception entry before it (12
  cycles plus the vector fetch) is not counted.

### Frame rate: FRCTRL2 and PORCTRL

ST7789V 9.2.16 note 1: frame = (320 + FPA + BPA) lines x (250 + 16 RTNA)
clocks of the internal oscillator (nominally 10 MHz). FRCTRL2 (C6h)
parameter: NLA[2:0] (000, dot inversion, as in the reset value 0Fh) and
RTNA[4:0]; PORCTRL (B2h): BPA, FPA, PSEN 0, then the idle and partial
porches at their reset value 33h, 33h (9.2.3). `Lcd::set_frame_rate`.

| Setting | Lines x clocks | TE period (mean of 120, 4 runs) | Rate |
|---|---|---|---|
| reset: RTNA 0Fh, FPA = BPA = 0Ch | 344 x 490 | 16,811 to 16,823 µs (single periods within ±10 µs of the mean) | 59.44-59.48 Hz |
| **`FrameRate::GAME_BOY`: RTNA 0Eh, FPA = BPA = 11h** | 354 x 474 | **16,735 to 16,749 µs** | 59.71-59.76 Hz |
| target (70,224 / 4,194,304 Hz) | | 16,742.7 µs | 59.7275 Hz |

The oscillator these imply is 10.018 to 10.021 MHz. The tuned setting is
what `FrameRate::closest` picks for it (the probe searches RTNA 0 to 31
and porch sums 2 to 64 for the period closest to the target, with a line
of at least 45 µs; both rounds agree with the constant). Its error,
0.04% or less, is under the run-to-run drift of the oscillator itself
(0.08%), and one porch line is a 0.28% step, so no setting does
better. The panel's frame and the Game Boy's will still drift apart
slowly (about one frame in 2,500 at 0.04%); pacing is M4/M5's
decision (ADR 0039).

### Write-ahead of the scan (tuned setting)

W = one 240-pixel row (480 B) at 12.8 MB/s = 37.5 µs; L = a scanned line
= 16,748.6 µs / 354 = 47.3 µs; d = TE to start, under 2 µs (budgeted 2 µs).
The window is panel rows 52..267 (`Y0` = (320 - 216) / 2). Taking the
scan of row 0 to start when TE falls (1,588 µs after the rising edge, the
measured high time), panel row 52 + k is scanned at 1,588 + (52 + k) L µs
and window row k is complete at d + (k + 1) W µs after the edge. The
margin, 1,588 + 52 L - d - W + k (L - W), is smallest at k = 0:
**4,049 - 39.5 = 4,009 µs**, and grows by L - W = 9.8 µs per row. The last
row is written at 8,102 µs, scanned at 14,221 µs. Even if the scan started
at TE's rising edge, the margin would be 52 L - d - W = 2,420 µs. The
condition holds with the reset rate too (L = 48.9 µs). It needs W < L,
that is a bus faster than 480 B / 47.3 µs = 10.1 MB/s, or the margin
shrinks by W - L per row (`SAFE`'s 7.1 MB/s: W = 67.5 µs > L, the scan would
overtake the write near the window's bottom unless the transfer starts
earlier).

What the probe cannot show is tearing itself (that is a human look at
moving content in M3's acceptance). It shows that the writes are where
the arithmetic puts them.

### Conversion and the main loop

- `scale_frame` (in flash, I-cache on; the frame in DTCM, the output in
  the non-cacheable AXI buffer): **178,128 to 179,124 cycles (377 to 380
  µs)** in the committed build (181,772 to 183,362 in an earlier build of
  the same code, a layout effect), about 45% of M3's 402k-cycle budget for conversion plus DMA
  setup. The headless image's ITCM placement may change this.
- `present` (window, 8 channel registers): included in the TE-to-start
  path only as SWRQ; its own cost was not timed separately (about 20
  register writes and 10 FMC bytes, about 1 µs).
- Back to back (120 frames, no readback): 120 transfers in 120 TE periods,
  none missed; the main loop's wait for the previous transfer averaged
  16.0 ms (it has nothing else to do in the probe) and was at most 22.3
  ms (presenting just after an edge waits a frame plus the transfer).

### In the headless loop (M3, `headless-display`)

The headless loop on `cgb-acid2` with the panel (src/bin/headless.rs
`display`; figures and method: docs/perf.md "Display in the loop (M3)"),
600 calls, 587 frames, every one read back after its transfer:

- **0 mismatching pixels** in 587 readbacks, in each of the `te-wait`,
  `te` and `now` start modes; the control at the end (one buffer word
  inverted) gave exactly 2.
- The transfer takes **3,822,714 to 3,822,792 cycles (8,099 µs)** with
  the core emulating beside it, the same as the idle probe: no slowdown
  from the core's bus traffic. TE to SWRQ 59 to 88 cycles; `present` 298
  to 386 cycles.
- `scale_frame` 195,927 to 197,247 cycles with the frame in cached AXI
  SRAM (the headless image's `.axisram.frame`), about 10% more than the
  probe's DTCM frame.
- The window readback (155,520 bytes) takes 42.63 M cycles, 90.3 ms (274
  cycles per byte, as the full-GRAM figure above).
- `dmg-acid2` the same way (`te-wait`): 592 frames, 0 mismatches,
  control 2, the same transfer and `scale_frame` figures.
- TE over the about 3,500 periods of each of those four runs: single
  periods 16,706 to 16,764 µs (7,885,240 to 7,912,445 cycles; these
  include the interrupt's entry
  latency, which an FMC read in progress can delay by about a byte read,
  0.6 µs), wider than the ±10 µs seen over 120 periods.
- Since M3 review T-2 the window is overwritten with the inverse of
  each frame by the CPU before its transfer, so a missed or short
  transfer shows as mismatches even when the frame repeats (docs/perf.md
  "Stricter self-test"); the panel can show the inverse briefly each
  frame while the test runs.

### Test image

After the readback phase the probe leaves a frame up (sent by MDMA like
the others): rows 0-51 and 268-319 black; in the window 8 vertical bars of
30 panel pixels (20 core pixels), in `lcd-probe`'s order of panel fields
(field 15-11 alone, 10-5, 4-0, 15-11 + 10-5, 10-5 + 4-0, 15-11 + 4-0,
white, a grey ramp the other way), fading to black towards the bottom,
and a white Γ at the frame's top-left: an 18x18-pixel square with an arm
along the top edge to 72 panel pixels (the core's 12x12 square and 48x4
arm, scaled). The plan wants the first bar blue and the third red, and
the Γ in the top-left corner reading as a Γ (not mirrored); a human check,
like `lcd-probe`'s.

## Sources

- RM0433 Rev 9: 22.6 (Figure 98, Tables 156-158), 22.7.2 (Table 171),
  22.7.4 (Figures 102, 108; Tables 174-176), 22.7.6 (BCR, BTR, BWTR), 8.7.19
  (D1CCIPR FMCSEL, CKPERSEL); chapter 14 (MDMA, registers 14.5), 12.3.5
  (SYSCFG_EXTICR4), 20.6 (EXTI registers), 8.7.40 (AHB3ENR MDMAEN).
- DS12117 Rev 10: Table 9 (AF), Tables 55 and 57 (FMC asynchronous
  timings).
- ES0392 Rev 16: section 2.6.
- UM2905 Rev 2: Table 2 (LCD pins), Tables 6-8 (Zio), section 6.2
  (variants).
- ILI9341V datasheet V1.00: 7.4, 7.6.3, 8.2.2, 8.2.3, 8.2.24, 8.2.29,
  8.2.33, 8.3.23, 15.4, 18.3.1.
- ST7789V datasheet V1.3 (Sitronix, see above): 7.4.1 Table 4, 7.4.5, 8.8.3,
  8.15 (TE modes and timing), 9.1.3, 9.1.7, 9.1.23, 9.1.27 (TEON), 9.1.28,
  9.1.32, 9.1.46-48, 9.2.3 (PORCTRL), 9.2.16 (FRCTRL2).
