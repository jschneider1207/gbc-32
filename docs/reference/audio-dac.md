# Audio out: DAC1 on PA4/PA5, TIM6, DMA1

Stage 2 M4, decision D4 (a): the H753's own 12-bit DAC, both channels,
line level. Brought up on 2026-11-26 with the `dac-probe` image
(`crates/gbc-nucleo-h753/src/bin/dac-probe.rs`, driver
`src/audio_dac.rs`). Nobody listened: everything below was checked by
registers and timing. Sources: RM0433 Rev 9, DS12117 Rev 10 (rev V
tables), ES0392 Rev 16, UM2407 Rev 6, UM2905 Rev 2, all in
`../gbc-32-hardware/datasheets/`.

```
dac config tim_hz=236000000 tim_div=7202 rate_mhz=32768675 frames=512 half=256 buf=0x24000000 bytes=2048 mpu_region=9 rbar=0x24000009 rasr=0x13080015
dac regs dac_cr=0x00170017 dac_mcr=0x00000000 dac_sr=0x00000000 dac_ccr=0x0014000f tim6_cr1=0x0080 tim6_cr2=0x0020 tim6_psc=0 tim6_arr=7201 dmamux_c0cr=0x00000043 ...
dac started dac_cr=0x00171017 tim6_cr1=0x0081 dma_s0cr=0x0002555f ndtr=512
dac ramp done=1 ms=257 (ramp 8192 frames = 250 ms nominal)
dac halves good=92408 bad=0 skipped=4557 calls=133
dac tone left_hz=440 right_hz=660 amp=600 window_ms=8007 frames=262400 rate_mhz_dwt=32768675 ppm_dwt=20 lse_cpu_hz=473584748 rate_mhz_lse=32878696 ppm_lse=3378 ... dor_in_range=1 dor_changing=1
dac tone refills=1257 late=0 spurious=0 te=0 dme=0 fe=0 udr1=0 udr2=0 isr_cycles_min=2300 isr_cycles_max=2345
dac silence mid=1 ms=2000
dac stop ok=1 ms=272 dor=0/0 dac_cr=0x00170017 tim6_cr1=0x0080 dma_s0cr=0x0002555e running=0
dac restart playing=1 dor=2048/2048 stop_ok=1 dor_after=0/0
dac summary config=1 ok=1
```

## Sample clock

- **TIM6 kernel clock = 236 MHz.** TIM6 is on APB1. RM0433 8.5.6
  Table 57: with RCC_CFGR.TIMPRE = 0 (reset value; nothing sets it) and
  D2PPRE1 = 100 (/2, `clock.rs`), the timer clock is rcc_hclk1 (twice
  pclk1): 472 / 2 = 236 MHz. (TIM16 on APB2 runs from the same clock;
  `clockcheck.rs` counts 7,202 of them per LSE period, which is the same
  ratio seen from the other side.)
- **PSC 0, ARR 7201**: an update every 7,202 clocks, 236,000,000 / 7,202
  = **32,768.675 Hz, +20.6 ppm** against 32,768 Hz (ADR 0021). The
  neighbours: /7,201 is +159.5 ppm, /7,203 -118.2 ppm. (The plan's
  "240 MHz / 7,324" figure predates the 472 MHz clock, D11.)
- TIM6_CR2 MMS = 010: the update event is TRGO (RM0433 42.4.2).
- **Measured against the cycle counter: 32,768.675 Hz exactly** (262,400
  frames over 8.007 s of refill interrupts). The DWT and TIM6 both come
  from the HSI, so this proves the divider, not the absolute rate.
- **Measured against the LSE crystal: 32,871.7 to 32,881.5 Hz, +3,165 to
  +3,462 ppm** over four runs (`lse_cpu_hz` 473.48 to 473.62 MHz: the
  HSI runs +0.32 to +0.35% fast on this board and drifts as it warms).
  DS12117 Table 133 allows 63.7 to 64.3 MHz at 30 °C plus -2% to +1%
  over temperature, so -2.6% to +1.5% is in spec.

**Consequence for M4.** With audio-driven pacing the emulator runs at the
DAC's rate, so on this board it runs about 0.34% fast in real time (pitch
+5.9 cents). The M4 criterion "within ±0.1% (emulated clocks / 4,194,304
against DWT cycles / 472 M)" compares with the same HSI and cannot see
it. Options, not decided here: accept it (it is a stand-in board);
choose ARR at boot from the LSE measurement (`clockcheck`: 473.58 MHz
gives a 236.79 MHz timer clock, /7,226 = 32,769.5 Hz, +46 ppm; rounding
leaves at most ±70 ppm); HSITRIM is too coarse (DS12117 Table 133: user
trimming step typically 0.24%, max 0.32%, about this board's whole
error).

## DAC1

- **Dual 12-bit right-aligned**: one word to DAC_DHR12RD (offset 0x20,
  0x4000_7420): left (channel 1, PA4) in bits 11:0, right (channel 2,
  PA5) in 27:16 (RM0433 26.4.4, 26.7.9).
- **Simultaneous trigger**: TEN1 = TEN2 = 1, TSEL1 = TSEL2 = 5;
  dac_chx_trg5 is tim6_trgo (RM0433 26.4.14 "Simultaneous trigger without
  wave generation", Table 229). DOR updates 3 dac_pclk cycles after the
  trigger (26.4.7). TSEL can only change while ENx = 0.
- **DAC_CR = 0x0017_0017** (EN, TEN, TSEL 5 for both), **0x0017_1017**
  while playing (DMAEN1). Only DMAEN1: "the application can manage both
  DAC channels in dual mode by using one DMA request and a unique DMA
  channel" (26.4.8).
- **DAC_MCR = 0**: MODE1 = MODE2 = 000, normal mode, buffer on, external
  pin only (26.7.16). ES0392 2.10.1: a data register is written before
  DAC_MCR, or the output is invalid; the driver writes DHR12RD = 0 first.
  ES0392 2.2.19 concerns MODE 001/011 only.
- DAC_CCR read 0x0014_000F: the factory offset trims (OTRIM2 0x14, OTRIM1
  0x0F), loaded at reset and kept (26.4.12).
- **Output range with the buffer on: 0.2 V to VREF+ - 0.2 V**, load
  RL ≥ 5 kΩ to VSSA (25 kΩ to VDDA), CL ≤ 50 pF; settling 1.7 to 2.1 µs
  full scale; tWAKEUP 5 µs typ, 7.5 µs max (DS12117 Table 176). VREF+ is
  VDDA, 3.3 V, on the NUCLEO (UM2407 Table 18, CN7 pin 6 "VDDA/VREFP";
  the solder bridge R37 is not documented in the text, **unverified**
  on the schematic). So codes below about 250 and above about 3,850 are
  clipped by the buffer; mid-scale 2048 is 1.65 V.

## DMA1 stream 0 and DMAMUX1

- **DMAMUX1 channel 0 → DMA1 stream 0** (RM0433 Table 103:
  dmamux1_req_out0 = dma1_str0). **DMAMUX1_C0CR = 0x43**: DMAREQ_ID 67 =
  dac_ch1_dma (Tables 103, 124), no synchronisation, no events (SE = 0,
  SPOL = 00, so ES0392 2.5.4 does not apply). DMAMUX1 is clocked by
  DMA1EN (RCC_AHB1ENR, 8.7.41).
- **DMA1_S0CR = 0x0002_555E** (+ EN): DIR memory to peripheral, CIRC,
  MINC, PSIZE = MSIZE = word, PL high, single transfers, HTIE, TCIE, TEIE,
  DMEIE (15.5.5). **FCR = 0x80**: direct mode (DMDIS = 0), FEIE.
  NDTR = 512, PAR = DHR12RD, M0AR = the buffer. TRBUFF = 0 (only UARTs
  need it).
- In direct mode the stream preloads one word on enable (15.3.8), and the
  DAC moves DHR to DOR before it requests the next word (26.4.8): DOR
  trails the DMA by one to two frames. So DHR must hold a sample before
  the first trigger (code 0 here, where the start ramp begins).
- **NVIC interrupt 11, DMA1_STR0.** The image defines the vector and
  calls `audio_dac::on_dma(source)`.

## The buffer

- 512 frames of `u32`, **2 KiB, aligned to 2 KiB, in `.axisram`** (AXI
  SRAM; 0x2400_0000 in `dac-probe`, after the LCD buffer in images that
  have one). DMA1's memory port reaches AXI SRAM but not ITCM/DTCM
  (RM0433 Table 3).
- **MPU region 9** (`mpu::REGION_AUDIO_BUF`), RASR 0x1308_0015: Normal
  non-cacheable (TEX 001 C 0 B 0), read/write, execute-never, 2 KiB,
  inside region 4 (AXI SRAM, write-back), which it only re-attributes.
  Set once by `AudioDac::new` with the same clean/invalidate sequence as
  the LCD buffer (`mpu::dma_buffer_window`, now taking the region).
- Halves of 256 frames: **7.81 ms** for each refill after its interrupt,
  128 interrupts per second, at most 15.6 ms of output latency from this
  buffer. The emulator's frame (about 550 samples) is absorbed by the
  sample ring upstream, so the DMA buffer only has to cover interrupt
  latency; 256 is plenty and keeps the interrupt rate low.
- The ISR picks the half to fill from NDTR (1..256: the DMA reads half
  1, so half 0 is free; otherwise half 1), not from the flag, so a late
  interrupt still fills the half the DMA has left (counted as `late` if
  both HTIF and TCIF were set).

**Buffer-half check** (`dac halves`): the source fills each half with a
tag of the half (right channel) and its call number (left); the main
loop samples NDTR, DOR1 and DOR2 together. Away from the half boundaries
DOR must show the tag of the half NDTR says is being read and the call
number of the refill before last. 92,408 samples good, 0 bad. With the
half selection inverted in `on_dma` (mutation, not committed): 0 good,
92,578 bad, `ok=0`.

## Start and stop

`start` ramps both channels from code 0 to mid-scale over 8,192 frames
(250 ms) with a smoothstep (3s² - 2s³, integer: no step and no slope step
at either end), then calls the source, which should begin at mid-scale.
`stop` ramps from the last frame the source wrote to 0 the same way,
plays a whole half of zeros (DOR trails the DMA, so stopping one half
earlier froze the ramp's last code, 1, in DOR: seen on the first run),
then stops TIM6, then the stream (waits for EN = 0), then clears DMAEN1
(RM0433 15.3.19 warning: the stream before the peripheral). Measured:
ramp 257 ms, stop 272 ms, DOR 0/0 afterwards, restart and second stop
fine. The DAC stays enabled at code 0 (about 0.2 V, the buffer's floor):
disabling it would float the pin. The step at power-up is from the idle
pin to that 0.2 V.

## Errors and cost

- Counted in the ISR: TEIF (bus error; the stream disables itself), DMEIF,
  FEIF (direct mode: the memory port not granted before the request,
  15.3.20), DAC DMAUDR1/2 (a trigger before the previous request was
  served; the DAC then stops requesting until re-initialised, 26.4.8).
  All 0 over the run (1,847 refills). Recovery from an underrun is
  `stop` + `start` (`stop` times out on a stalled stream and shuts down
  anyway). ES0392 2.10.2 (DMAUDR missed when a trigger meets a DMA
  acknowledge) needs mixed software and hardware triggers; not used.
- **ISR cost, entry to exit:** tone generator (two phase accumulators, a
  256-entry table) 2,300 to 2,345 cycles per 256 frames (4.9 µs, about 9
  cycles a frame, 0.064% of the CPU at 128 interrupts/s); the halves
  test's `fill` 556 to 602; the ramps (a 64-bit smoothstep per frame) up
  to 3,946 to 4,079. A ring's `pop_into` (a copy) should cost about as
  much as the fill.

## In the paced loop (M4, `paced-<rom>`)

Measured 2026-11-26 with `src/bin/paced.rs` (figures: docs/perf.md
"Paced loop (M4)").

- **The cycle counter stops in `WFI`** unless DBGMCU_CR DBGSLEEP_D1
  (bit 0) is set (RM0433 Rev 9 DBGMCU_CR: "automatic clock stop" in
  Sleep): sleeping in `WFI` between TE interrupts, the mean TE period read
  as 163 DWT counts with the bit clear and 7,895,954 with it set (the
  panel's 59.8 Hz at 472 MHz). DWT_CYCCNT counts CPU clock cycles and
  CSleep gates the CPU clock. So a loop that idles in `WFI` and times
  itself with the DWT must set the bit (the paced image does), or its
  elapsed time is the busy time only. Stage 3 should time real time from
  a timer instead, so Sleep can gate the clock.
- **DBGMCU_CR survives a system reset** (`dbgmcu_cr_boot=0x00000001` in
  every boot after the first image set it; only a power-on reset clears
  it), so an image must write the bit it depends on, not assume the reset
  value.
- **The start ramp and the speed figures.** `AudioDac::start` plays its
  8,192-frame ramp (30 refills after the 512 frames it writes itself)
  before the source is called. A loop started right after `start` fills
  the ring and then idles 250 ms that the speed figures count as real
  time with nothing consumed: a 1,200-frame run measured -8,630 ppm. The
  paced image waits until 26 refills have passed (96 M cycles, 203 ms),
  then runs.
- **ISR cost with the ring:** `TAP.pop_into(&RING, out)` (a 256-frame
  copy plus the tap's FNV-1a fold of 1,024 bytes) is 8,110 to 8,290
  cycles at most (17.5 µs, 0.22% of the CPU at 128 interrupts/s); 552 to
  556 at least (the stop ramp's zeros).
- Over two 10-minute runs (76,872 refills each) with the panel's MDMA
  transfers and the core running: 0 late refills, 0 TEIF/DMEIF/FEIF, 0
  DAC DMA underruns, stop ramp finished.

## Wiring (for the human)

| Signal | MCU pin | ST morpho (outer rows, free with the shield on) | Zio (under the shield) |
|---|---|---|---|
| Left, DAC1_OUT1 | PA4 | **CN11 pin 32** | CN7 pin 17 (D24) |
| Right, DAC1_OUT2 | PA5 | **CN12 pin 11** | CN7 pin 10 (D13) |
| Ground | | **CN12 pin 32 (AGND)**, or CN11 pin 8, 19 or 20 (GND) | CN7 pin 8 |

UM2407 Tables 18 and 22. The shield leaves CN7 pins 10 and 17 unconnected
(UM2905 Table 5). PA5 is also the alternative LD1 pin: LD1 is on PB0 by
default (SB39 ON, SB47 OFF, UM2407 7.6.1); with SB47 ON the LED would
load the right channel.

- **Line level, not a headphone output.** The buffer needs RL ≥ 5 kΩ:
  use powered speakers, a line input, or an amplifier; not headphones
  directly.
- **The outputs sit at about 1.65 V DC** (mid-scale) while playing and
  0.2 V when stopped. AC-couple anything not built for that: a series
  capacitor per channel, e.g. 10 µF (electrolytic, + towards the board, or
  film), into a 10 kΩ or higher input gives a corner around 1.6 Hz; a
  100 kΩ resistor to ground after each capacitor keeps it from floating.
  Most PC speakers and amplifiers already have input capacitors; a DC-
  coupled input (some mixers, scopes on DC) does not.
- Levels: the probe plays ±600 codes (±0.48 V, 0.34 V rms, near consumer
  line level, -10 dBV); the maximum is about ±1.45 V around 1.65 V.
- The probe plays 440 Hz left and 660 Hz right for 10 s after the ramp,
  2 s of silence, then stops; reflash to hear it again.
