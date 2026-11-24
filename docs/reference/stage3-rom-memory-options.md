# Stage 3: where the active ROM lives (design exploration)

Status: exploration for the hardware owner, 2026-11-23. Not a decision. Hardware ADR 0001
(FMC SDRAM) is Accepted and human-owned; this note only argues whether to revisit it.

Source tags: **[DS]** DS13312 Rev 4, **[RM]** RM0468 Rev 3, **[ES]** ES0491 Rev 9 (all in
`../gbc-32-hardware/datasheets/`); **[ext]** public document fetched for this note (URL given);
**[est]** my estimate or arithmetic; **[mem]** general STM32H7/Arm knowledge from memory, not
checked against a document for this part.

## Summary

**Recommendation: move the ROM from 16-bit FMC SDRAM to a 3.0 V HyperRAM on OCTOSPI**
(S70KL1281 / S27KL0642 class, copying the STM32H735G-DK's proven hookup), with a 3 V
Xccela octal PSRAM (AP Memory APS12808L-3OBM) as the alternative. Keep FMC free for an
8080 display. Make it conditional on one measurement on an STM32H735G-DK (exact MCU and
package, carries this HyperRAM) before the prototype layout. **Confidence: medium.**

Why:
1. On this MCU the two buses have about the same raw speed: FMC SDCLK ≤ 95 MHz × 16 bit
   ≈ 190 MB/s [DS §6.3 SDRAM timings]; OCTOSPI DTR with DQS ≤ 100 MHz × 8 bit × 2 = 200 MB/s
   [DS Table 82]. A random 32-byte D-cache line fill costs about 130–165 CPU cycles from SDRAM
   and 155–200 from HyperRAM/PSRAM [est, below]. With realistic miss rates (hundreds to a few
   thousand per frame) the difference is under 1% of a 9.2 Mcycle frame. ADR 0001's "slower"
   was a judgement about QSPI on the H753, not about OCTOSPI on the H735. And OCTOSPI
   memory-mapped *writes* are supported here [RM §25.2, Table 209], so the loader is simple.
2. 11–12 pins instead of about 39, so FMC isn't shared with the display (ADR 0001's stub
   problem goes away). That makes the BGA fan-out easier and gives lower idle power (µA standby
   instead of an SDRAM drawing mA with its clock always running).
3. It can be tested on the exact silicon before the PCB. The STM32H735G-DK has an
   STM32H735IGK6 in UFBGA176+25 with S70KL1281 HyperRAM on OCTOSPI2 and octal NOR on
   OCTOSPI1. No H735 board with FMC SDRAM is known.

Main risks: OCTOSPI has 16 errata on this part (most avoided by configuration, one has no
workaround and needs wrap kept off); HyperRAM bring-up on the H735 has a visible trail of
forum problems; real-game miss rates are not measured yet.

## STM32H735IGK6 capabilities and errata (this part only)

**Capabilities**

| Item | Value | Source |
|---|---|---|
| FMC SDRAM clock | SDCLK = fmc_ker_ck/2 or /3; max 95 MHz at 2.7–3.6 V, 20 pF | [DS §3.18, "SDRAM waveforms and timings"], [RM §24.9.1] |
| FMC SDRAM features | 8/16/32-bit, CL 1–3, self-refresh and power-down, read FIFO 6×32-bit (RBURST), tracks open row per bank | [RM §24.9.1, "SDRAM controller read cycle"] |
| PC2_C/PC3_C penalty | SDNE on PC2_C: +4.5 ns chip-select valid time. OCTOSPI data on PC2/PC3: DTR max drops to 47 MHz | [DS Table 76 note 2; Tables 81/82 note 4] |
| OCTOSPI instances | 2, each single/dual/quad/octal, SDR and DTR, DQS, HyperBus; Xccela and JEDEC xSPI compliant | [DS §3.19], [RM Table 209] |
| OCTOSPI max clock (VOS0) | SDR 140 MHz (2.7–3.6 V, 20 pF); DTR without DQS 110 MHz; **DTR with DQS / HyperBus 100 MHz** (DHQC required, RWDS-DQ skew ±1 ns) | [DS Tables 80, 81, 82 and notes] |
| Memory-mapped mode | Read **and write**; 256 MB window; byte/half/word access; XIP prefetch that "never stops" (NCS held low) unless TIMEOUT (TCEN) is set; AXI accepts one transaction at a time | [RM §25.2, §25.4.10] |
| Refresh / CS limits | REFRESH[31:0] (DCR4) caps transaction length in cycles; CSBOUND; MAXTRAN | [RM §25.4.6] |
| Wrap | Hybrid wrap, read only, 8-byte data size; WRAPSIZE=0 if the memory lacks it | [RM §25.4.6, DCR2] |
| I/O manager | Any OCTOSPI to port P1 or P2; multiplexed mode allows clock mode 0 only | [RM §26.4.3] |
| Memory map | FMC SDRAM bank1 at 0xC000_0000 (BMAP can swap); OCTOSPI windows at 0x7000_0000 / 0x9000_0000 | [RM §24.4 FMC bank mapping table] |
| Default MPU attributes | 0xC000_0000 is Device (uncacheable) without an MPU region; 0x7… WBWA, 0x9… WT | [mem] ARMv7-M default map |
| RAM | ITCM/AXI split by TCM_AXI_SHARED: 64/320, **128/256** (perf.md plan), 192/192, 256/128 KB | [RM Table 8] |
| Pins on UFBGA176+25 | Every OCTOSPIM_P1/P2 and FMC SDRAM signal named here is bonded; only PJ8–11 and PK0–2 are missing from the ball column | [DS Table 8], extracted by script; cross-check in CubeMX |

**Errata** [ES Table 3]. Every FMC, OCTOSPI, MDMA, DMA and core erratum below has the
**same status on rev A and rev Z**, so the comparison doesn't depend on which revision the
IGK6 ships as. I couldn't confirm the shipping revision from these documents (DS §7.1 points
to a marking note). Read `DBGMCU_IDC.REV_ID` (0x1000 = A, 0x1001 = Z) on delivered parts.

| Erratum | Affects | Workaround / impact |
|---|---|---|
| 2.1.1 PLD linefill to MPU-faulting address | all | MPU region 0 = 4 GB no-access background. Matters for OCTOSPI: an access past DEVSIZE in the 256 MB window is an AXI error/hard fault [RM §25.4.10], so size the ROM region exactly |
| 2.1.3 D-cache ECC with both DEBRs locked | all | don't lock both DEBRs (we don't) |
| 2.1.4 store after cache invalidate, no barrier | loader | DMB after invalidating the ROM region (do it anyway) |
| 2.3.1 MDMA write to reserved ITCM area not flagged | loader | only with TCM_AXI_SHARED=10; not our layout |
| 2.5.1, 2.6.x DMA/DMAMUX | — | UART/DMAMUX-specific; SDMMC IDMA and MDMA unaffected |
| 2.9.1 SDMMC end bits not checked | loader | data CRC still checked; add a whole-file CRC check after load (also catches RAM faults) |
| 2.7.1 FMC two dummy reads after a burst | SDRAM | +2 SDCLK per line fill |
| 2.7.3 FMC unaligned reads unsupported | SDRAM | not handled: the core's ROM reads may be unaligned since gbc-32 ADR 0027 (byte reads) was retired, 2026-11-24 |
| 2.8.2 / 2.8.3 hybrid wrap broken by REFRESH etc.; WP* registers dead | OSPI | **keep WRAPSIZE=0** (no memory-side wrap) |
| 2.8.15 wrap then linear read to same MSB address: corruption, **no workaround** | OSPI | only with wrap transactions → WRAPSIZE=0. My reading; confirm on the DK |
| 2.8.4 odd address/length combinations | OSPI | memory-mapped reads at any address are fine in DTR-with-DQS and HyperBus; mm writes fine in HyperBus or with DM |
| 2.8.5 <2 cycles before data, no DQS | OSPI | N/A with DQS; command/address phases are longer anyway |
| 2.8.6 mm write error with DQSE=0 in WCCR | OSPI | set WCCR.DQSE for writes |
| 2.8.7 / 2.8.10 clock mode 3 problems | OSPI | use clock mode 0 |
| 2.8.8 DQS only works in octal | OSPI | octal anyway |
| 2.8.9 extra bytes, indirect read with DQS | OSPI | loader uses mm or indirect *writes*; avoid short indirect reads |
| 2.8.11 deadlock in multiplexed mode | OSPI | don't multiplex; one memory per port |
| 2.8.12 misaligned AR then switch to mm: deadlock/corrupt, no workaround | OSPI | never leave a misaligned address in AR before entering mm mode |
| 2.8.13 HyperBus with DQS needs ≥ 6 latency cycles | HyperRAM | costs 1–2 cycles per access versus the part's minimum |
| 2.8.16 OctaRAM (Macronix MTYP) limited to 8 MB | — | doesn't apply to PSRAM/HyperRAM (stated in the erratum) |

No erratum touches CPU_FREQ_BOOST. Boost disables ITCM/DTCM ECC [RM §12.4.26 per ADR 0002],
which doesn't interact with either memory bus.

## Comparison

Per-miss figures are for a random 32-byte line at 550 MHz (1.818 ns/cycle). "Overhead" is
AXI plus clock-domain crossing, assumed 20–40 ns [est].

| | 1. FMC SDRAM 16-bit | 2. Octal PSRAM (Xccela) | 3. HyperRAM | 4. Octal NOR, XIP, rewritten per game | 5. Internal bank cache (ADR 0001 B) |
|---|---|---|---|---|---|
| Example 3 V part | W9825G6KH / IS42S16160J (32 MB) | APS12808L-3OBM (16 MB, 2.7–3.6 V, 133 MHz) [ext] | S70KL1281 (16 MB) / S27KL0642 (8 MB) [ext] | MX25LM51245G (64 MB, on the DK) | — |
| Bus clock used | 95 MHz SDR | 100 MHz DTR (MCU cap) | 100 MHz DTR (MCU cap) | 100 MHz DTR | — |
| Random line fill | ~21–25 clk + overhead ≈ **240–300 ns, 130–165 cyc** | ~26–30 clk + ovh ≈ **280–340 ns, 155–185 cyc** | ~26–32 clk + ovh ≈ **280–360 ns, 155–200 cyc** | ~29–33 clk + ovh ≈ 310–370 ns [est, dummy count unverified] | 0 on hit; ~1 ms per bank miss (ADR 0001) |
| Sequential next line | row hit, RBURST FIFO ≈ 18–21 clk | prefetch stream: 16 clk ≈ 160 ns | same | same | — |
| Peak bandwidth (load) | ~190 MB/s | 200 MB/s | 200 MB/s | read 200 MB/s; **program ~MB/s** [mem] | — |
| MCU pins | ~39 (A0–12, BA0–1, D0–15, NBL0–1, 6 ctl) | 11 (+RESET# GPIO) | 11 (+RESET#) | 11 | 0 |
| Shares FMC with an 8080 display? | yes (stub on ~95 MHz bus) | no | no | no | no |
| Errata exposure | 3 FMC, benign/handled | OSPI list, all avoidable by configuration | OSPI list + 2.8.13 | OSPI list | none |
| Standby (CS high) | SDRAM keeps clock + auto-refresh: mA-level [mem]; self-refresh ~1.5 mA max (-6L grade, distributor figure) [ext] | 280 µA typ / 500 µA max @25/85 °C [ext DS §8.5] | 90 µA typ / 540 µA max (full array) [ext] | ~0 when powered down | — |
| Active read | tens of mA [mem] | ≤ 14 mA @133 MHz [ext] | 15 typ / 34 max mA @166 MHz [ext] | ~tens of mA [mem] | — |
| Layout | 39 nets from 0.65 mm BGA, TSOP-54 (probeable) | 11 nets, 24-ball BGA, ±1 ns skew | same; reference layout exists (DK) | same; on the DK | trivial |
| Testable on exact MCU pre-PCB | no | no (HyperRAM on the DK is a close proxy) | **yes, STM32H735G-DK** | **yes, DK** | yes |
| Core API change | none | none | none | none | **yes** (rejected in ADR 0001) |
| Game-switch time, 8 MiB | eMMC-bound, ~0.1–0.3 s [est] | same | same | **tens of seconds** program+erase [est] | — |

Other rows considered:
- **Two OCTOSPI channels.** The H735 can't gang them into 16-bit (dual-quad only, [RM §25.2]),
  so a second channel only adds a separate device: an octal NOR "recent games" XIP store
  (option 4) or a framebuffer. Optional, and costs 11 more pins.
- **Small ROMs in internal AXI SRAM.** With 128 KB ITCM there's 256 KB AXI SRAM [RM Table 8],
  minus framebuffers and up to 128 KB cart RAM. Only ≤ ~64–128 KB ROMs would fit. This is a
  platform-only choice (still one `&[u8]`), and it's useful as a *measurement baseline*.

## Option notes

**1. FMC SDRAM (current ADR 0001).** A mature and well-understood path, and the TSOP-54 can
be probed. Findings:
- Pins: about 39, and the FMC address pins PF0–PF5/PG0–PG5 collide with OCTOSPIM_P2 [DS
  Table 8], so SDRAM and a P2 OCTOSPI memory exclude each other.
- **Timing problem in the ADR 0002 draft pin map:** SDNE0 on PC2_C adds 4.5 ns [DS Table 76
  note 2]. FMC outputs change on the SDCLK falling edge and the SDRAM samples on the rising
  edge, half a period later: 5.26 ns at 95 MHz. 1.5 + 4.5 = 6.0 ns of valid time plus the
  SDRAM's input setup time (~1.5 ns [mem]) doesn't fit. Either move SDNE0/SDCKE0 to
  PH3/PH2 or PC4/PC5, or use bank 2 (SDNE1/SDCKE1 on PH7/PH6 or PB6/PB5) [DS Table 8], or
  run SDCLK at about 65 MHz [est].
- Power: the FMC doesn't automatically drop CKE between accesses [mem]. SDRAM idles in
  auto-refresh with the clock running (mA-level) unless firmware uses power-down mode.
- MPU: the region must be remapped to Normal/cacheable (ADR 0001 already notes this).

**2. Octal DDR PSRAM (Xccela).** APS12808L-3OBM [ext:
https://forums.parallax.com/uploads/editor/5w/gv1xkzq8x6ts.pdf, Rev 1.1b]:
- 128 Mb, VDD 2.7–3.6 V, 133 MHz.
- Read latency LC 3/4/5 for 66/109/133 MHz, variable (up to 2×LC on refresh collision) or
  fixed 2×LC (Table 5).
- **tCEM ≤ 4 µs (standard temp) / 1 µs (extended)** (Table 28). Bursts start on even addresses.
- DM is supported, so byte writes work.
- Linear-burst command 20h exists, which suits WRAPSIZE=0.

At 100 MHz: LC=4, OCTOSPI REFRESH ≤ ~350 cycles (4 µs at 100 MHz = 400, minus margin and the
command phase), TCEN/TIMEOUT on. The 1.8 V APS12808L-OBM [ext, NXP-hosted Rev 3.0a] does
**not** fit a 3.3 V MCU (the H735 has no separate OCTOSPI I/O rail [mem]). Not on any ST
board I found, so it's less proven on this MCU than HyperRAM.

**3. HyperRAM.** S27KL0642 [ext:
https://www.infineon.com/assets/row/public/documents/10/49/infineon-s27kl0642-s27ks0642-hyperram-self-refresh-dram-3-0-v-1-8-v-64-mbit-automotive-e-grade-1-preliminary-datasheet-en.pdf,
002-31332 Rev **]:
- 3.0 V up to 200 MHz; CR0 initial latency 3–7 clocks; fixed 2× (default) or variable.
- Wrapped 16/32/64/128-byte and hybrid bursts.
- tCSM 4 µs at 85 °C / 1 µs at 125 °C.
- Standby 90 µA typ; hybrid sleep and deep power-down modes.

On this MCU it is capped at 100 MHz [DS Table 82] with latency ≥ 6 [ES 2.8.13]: variable
6/12, or fixed 12. ST's own H735G-DK driver uses **fixed latency, 100 MHz, REFRESH=400 (4 µs),
CSBOUND=23 (die boundary)** [ext: https://github.com/STMicroelectronics/stm32h735g-dk-bsp,
`stm32h735g_discovery_ospi.c` lines 1018, 1148–1166]. REFRESH=400 leaves no margin to tCSM;
I'd use about 350.
- DK pinout (copy it): OCTOSPIM_P2, NCS PG12, CLK PF4, DQS PF12, D0–D3 PF0–PF3,
  D4–D5 PG0–PG1, D6–D7 PG10–PG11 [ext: BSP `stm32h735g_discovery_ospi.h` lines 251–282].
- The DK fits this, an octal NOR on P1 (PF10, PG6, PD11–13, PE2, PD4/5/7, PG9, PB2 [ext:
  Zephyr `boards/st/stm32h735g_disco/stm32h735g_disco.dts`]), LTDC, 4-bit SDMMC1 and USB on
  the same package. That is a strong existence proof for the pin budget. For an FMC 8080
  display, take RS from A16–A18 (PD11–13), not A0 (PF0, used by P2).
- Bring-up trail: several ST community threads about H735 HyperRAM failures ("1st read pass
  next read fail", "HyperRAM access not possible", "multiplexed memory-mapped hardfault", and
  S70KL1282 vs 1281 latency differences). For example:
  https://community.st.com/t5/stm32-mcus-products/stm32h735g-dk-octospi2-hyperram-1st-read-pass-next-read-fail/m-p/622493
  and https://community.st.com/stm32-mcus-boards-and-hardware-tools-26/stm32h735-dk-hyperram-access-not-possible-150703

**4. Octal NOR XIP, rewritten per game.** It's on the DK (MX25LM51245G, 64 MB). It's
non-volatile, so resume is instant and nothing needs refresh. Rewriting 8 MiB takes tens of
seconds [est; get program/erase times from the datasheet], with wear on every switch. 64 MB
holds about 8 worst-case or ~30+ typical games as a "recent games" cache. It changes the
storage/UX model, so it's the human's call.

Prior art: retro-go on the Game & Watch (STM32H7B0, 280 MHz) runs GB/GBC with uncompressed
ROM banks read *directly* from memory-mapped external flash. It uses an internal-RAM bank
cache only for LZ4-compressed banks
[ext: https://github.com/kbeckmann/retro-go-stm32/blob/HEAD/gnuboy-go/components/gnuboy/loader.c,
lines 216–330, `rom_loadbank_cache`]. That is evidence that GB emulation tolerates
external-memory ROM latency on an H7, even from a slower quad flash.

**5. Software bank cache in internal SRAM.** The core reads one `&[u8]`, so a bank cache needs
the core API change that ADR 0001 rejected. The D-cache already acts as a 32 KB, 32-byte-line
ROM cache, because emulator state is in DTCM and code is in ITCM (perf.md ITCM plan), and
neither goes through the D-cache. Copying a 16 KiB bank costs ~80–100 µs at external-RAM
bandwidth [est] and would be paid on every switch. Games like Pokémon switch banks many times
per frame [mem]. **It doesn't change the comparison.** Revisit only if the measured miss cost
exceeds ~10% of a frame.

## Arithmetic

- Frame budget at 550 MHz: 550e6 / 59.73 = **9.21 Mcycles**. 1 cycle = 1.818 ns.
- SDRAM, random line with a row conflict: PRE 2 + ACT→READ (tRCD) 2 + CL 3 + 16 beats
  (32 B / 2 B) + 2 dummy [ES 2.7.1] = 25 SDCLK × 10.53 ns = 263 ns. Row hit: 21 SDCLK = 221 ns.
  Plus 20–40 ns overhead gives 240–300 ns, **130–165 cycles**. tRP/tRCD/CL are typical
  -6 grade values [mem].
- Octal PSRAM at 100 MHz: CMD 1 + ADDR 2 + LC 4 (8 on refresh collision) + preamble ~1 + data
  16 + CS high 2 (tCPH 18 ns) = 26–30 clk = 260–300 ns. With overhead, **155–185 cycles**.
- HyperRAM at 100 MHz: CA 3 + latency 6 (12 if fixed or refresh) + data 16 + CS high ~1 =
  26–32 clk = 260–320 ns. With overhead, **155–200 cycles**.
- OCTOSPI sequential continuation (prefetch still streaming, next AXI address contiguous):
  16 clk = 160 ns ≈ 90 cycles. A random access first aborts the prefetch (CS high, CSHT).
- Stage 2 proxy (H753 QUADSPI NOR, 4-line): ~2 + 6 + 6–10 + 64 clk ≈ 80 clk at ~100 MHz
  ≈ 800 ns ≈ 380 cycles at 480 MHz [est]. That's 2–3× worse per miss, so it's a conservative
  proxy, as ADR 0001 says.
- Cost per frame: misses M × ~170 cycles. M = 300 → 51k (0.6%); 1,000 → 170k (1.8%);
  3,000 → 510k (5.5%); 10,000 → 1.7M (18%). **The gap between options** (~±30 cycles/miss)
  is 90k cycles (1%) even at M = 3,000.
- Bounds on M: a CGB double-speed frame has 35,112 M-cycles, so at most ~35k ROM byte reads.
  Straight-line code with no reuse touches at most 35k/32 ≈ 1,100 new lines. HDMA/GDMA from
  ROM rewriting all 16 KB of VRAM is 512 lines. Only random access across banks (every read a
  miss) reaches ~35k misses (~65% of a frame), which isn't a realistic pattern. I expect
  realistic M in the hundreds to low thousands; **unmeasured**.
- Keep the D-cache for the ROM. Framebuffer and audio buffers in AXI SRAM must be
  non-cacheable, or write-through without write-allocate. Otherwise ~46 KB+ of framebuffer
  writes per frame evict the ROM from the 32 KB D-cache every frame. This is the biggest lever
  on M, whichever memory is chosen.
- Load: eMMC 8-bit HS at 3.3 V ≈ 52 MB/s raw [mem]. 8 MiB takes ~0.16 s + overhead, and it
  doesn't depend on the RAM type.

## Firmware complexity

- SDRAM: FMC init (timing registers, mode-register sequence, refresh count), MPU Normal WB,
  load with SDMMC IDMA → AXI bounce buffer → MDMA/CPU. Self-refresh entry for sleep.
- OCTOSPI RAM:
  - OCTOSPIM port routing; DCR1–4 (DEVSIZE, CSHT, clock mode 0, REFRESH, CSBOUND);
  - memory register config (latency, drive strength) through indirect writes;
  - HyperBus HLCR (TACC ≥ 6, tRWR) or Xccela CCR/TCR/WCCR (DQSE=1, ES 2.8.6);
  - TCEN/TIMEOUT so CS isn't held low forever, which matters for power and tCSM;
  - WRAPSIZE=0; MPU Normal WB read-allocate, ROM region read-only after load, rest of the
    256 MB window no-access (ES 2.1.1).

  It's about as much as SDRAM, and ST's BSP is a reference (BSD-3, read for behavior only).
- Sleep: either keep the RAM in self-refresh / hybrid sleep, or power it off and reload the
  ROM from eMMC on resume (~0.2 s). The second option is simpler and the same for every
  option.

## Risks

1. OCTOSPI configuration errors corrupt data silently (REFRESH vs tCSM, temperature-dependent;
   wrap errata). Mitigate with a load-time CRC plus a read-back soak test on the DK.
2. Real-game miss rate unknown. A game indexing large tables across banks every frame could
   reach thousands of misses per frame. The difference between memory options stays small,
   but the absolute cost could matter for the 50% bar.
3. HyperRAM bring-up reputation on the H735 (forum threads above). Supply and lifecycle for
   S70KL1281 vs S70KL1282 vs S27KL0642 and APS12808L-3OBM: **stock and prices not verified**.
   LCSC lists S27KL0642DPBHB023 (C5157921) and JLC lists W9825G6KH-6 (C62246); quantities
   not checked.
4. Revisiting an Accepted hardware ADR late. Mitigated if the DK measurement happens before
   layout starts.

## Open questions for the human (hardware owner)

1. Revisit ADR 0001 in favor of OCTOSPI HyperRAM (or Xccela PSRAM), pending a DK measurement?
2. Buy an STM32H735G-DK (exact MCU, HyperRAM, octal NOR, LTDC panel) for Stage 2b? Price not
   verified.
3. Display interface: if FMC 8080, OCTOSPI RAM removes bus sharing. If LTDC, the framebuffer
   location matters (the framebuffer could also go in 16 MB HyperRAM, but then it competes
   with ROM fills).
4. Maximum operating temperature: tCSM is 4 µs up to 85 °C and 1 µs above.
5. If SDRAM stays: move SDNE0 off PC2_C (timing finding above).
6. Is a NOR "recent games" XIP store (option 4) of any interest for UX?

## Stage 2 measurements that settle it

1. **Now, on desktop (no hardware):** a host-side cache model fed by ROM read addresses (32 KB,
   4-way, 32-byte lines) over synth:worst-case and the human's games, reporting misses/frame
   (mean, p99, max). This gives M directly. It needs a read hook; keep it out of the hot path
   (feature-gated, no alloc in core) or put it in the bench tool.
2. **NUCLEO-H753ZI:** DWT cycles/frame for a ≤ 128 KB ROM run from AXI SRAM vs memory-mapped
   QSPI. The difference divided by the modelled M gives the real per-miss cost and validates
   the model.
3. **STM32H735G-DK:** a pointer-chase microbenchmark (random and sequential 32-byte stride over
   8 MiB) for line-fill latency at 550 MHz in HyperRAM and octal NOR, plus whole-game
   cycles/frame with the ROM in HyperRAM vs internal SRAM. Pass criterion [proposal]: external
   ROM adds < 5% of the 50% budget on the worst real game. Also measure the memory supply
   current during play with TIMEOUT tuned.
