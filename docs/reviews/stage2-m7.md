# Stage 2 M7 review (`stage1-m10..264a4d6`, Stage 2 as a whole)

Three fresh read-only reviewers, 2026-11-26 (session 28): **platform
correctness**, **test integrity** and **performance and portability**. No
high findings. "Stage 3 task" = one-line task in `docs/plan/stage-2.md`
("Stage 3 carry-over tasks"); PENDING = not yet resolved.

## Platform correctness

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| C1 | med | audio_dac.rs `AudioDac::watchdog` (called player.rs:242, paced.rs:234) | `cycles()` is read before `LAST_AT`; an ISR in between plus a wrap gives a false DAC restart (~15 ms held) | **fixed** b6e9236 (`LAST_AT` read first); no host test feasible (read order vs ISR, board crate has no host harness); board: `dac_restarts=0` in 2 x 80 s player runs |
| C2 | med | clock bring-up (SYSCFG CCCSR) | I/O compensation cell never enabled while QSPI runs at 99.2 MHz; DS12117 Tables 170/149 note 5 and RM0433 11.3.11 assume it | **fixed** 47f804b (CSI on, CCCSR.EN, bounded waits); banner `cccsr=0x00000101` seen on the board; B2 QSPI hash-equal at 99.2 MHz; gate B1 requires it, 0e7ab23 (seen failing) |
| C3 | low | fault.rs `die()` | IRQs and DMA still running; `CR3.DMAT` left set interleaves TX with the FAULT line | Stage 3 task |
| C4 | low | DAC DMAUDR1 handling | not per RM0433 26.4.8 (re-init after underrun) | Stage 3 task |
| C5 | low | link_uart | RX overflow (ring lap) is silent | Stage 3 task |
| C6 | low | NDTR vs posted writes | ordering of NDTR reads against posted writes (reviewer's guess) | Stage 3 task |
| C7 | low | qspi.rs regs/`Mapped::exit`, mpu `qspi_window`/`fmc_window` | SAFETY comments rest on a false "no interrupt" premise | Stage 3 task |
| C8 | low | qspi.rs vs clock.rs | cites DS12117 rev Y, clock.rs rev V | Stage 3 task |
| C9 | low | clockcheck | ES0392 2.2.30: PC13 vs LSE during clockcheck | Stage 3 task |

## Test integrity

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| T1 | med | gate-stage2.sh:189-265 (H1) | H1 doesn't refuse `BASH_ENV`/`ENV`/`SHELLOPTS`; verified `BASH_ENV` re-injects `RUN_PACED_CAPTURE`, so B3/B4 pass from an old capture | **fixed** 3df7272: H1 refuses `BASH_ENV`, `ENV`, `SHELLOPTS`, `BASHOPTS`, `CDPATH`, `GLOBIGNORE`, exported functions; demo exit 1, clean exit 3 |
| T2 | med | gate-stage2.sh:846-856 (B3) | display readback runs in headless-display-cgb-acid2, not player-script (plan stage-2.md:423-426) | PENDING (readback into player-script; headless-display run kept) |
| T3 | med | gate-stage2.sh:469-472 (H5) | no committed, regenerated host hash lists for the replay set | **fixed** 08fb34b: `tools/headless/expected/replay/*.txt` regenerated and compared; tamper exit 1; fbs equal the M5 board evidence |
| T4 | med | gate-stage2.sh:546-607 (H7) | named tests missing: debounce, gbc-link cobs/crc, gbc-pc save/rtc, DC blocker, image header | **fixed** 9365cbf: 85 names; `#[ignore]` or rename gives exit 1 |
| T5 | low | B4 | soak runs player-script, not the live player | needs the human (asked) |
| T6 | low | plan text vs `acceptance/stage2.toml` | +-0.1% claim vs DWT on the HSI | needs the human (asked) |
| T7 | low | check-stage2-sets.sh | accepts unknown keys, no model check | **fixed** c4f5fa7: unknown keys exit 2, model `cgb-c` checked |
| T8 | low | board runs | no boot nonce (guess) | Stage 3 task |
| T9 | low | gate board steps | board loss after the lock is exit 2, not 1 (never a pass) | Stage 3 task |
| T10 | low | check-cycles.sh:82, gate-stage2.sh:717-719 | wrong comments | **fixed** 3f94b15 |
| T11 | low | gate board-step parsing | no tests of the gate's own parsing | Stage 3 task |

## Performance and portability

| # | Sev | Where | Finding | Outcome |
|---|---|---|---|---|
| P1 | med | paced.rs:432, :419, audio.rs:242 (FeedTap in the DAC ISR) | frame and feed hashes run every frame in the live player (~184,920 cycles mean); player.md's "fe mostly scaler+audio" was wrong | **fixed** 3c0a4a7 (+ docs 264a4d6): live player skips both hashes; fe mean 427,482 -> 244,589, max 516,128 -> 321,577 (Pokemon Red, 80 s); tests seen failing |
| P2 | med | display.rs:249, :159; mpu.rs:116, :134 | display stash is AXI NORMAL_WBWA: the 46,080 B copy on ~45% of frames evicts the D-cache (est. up to 220k cycles refetch) | PENDING (agent after the readback work; estimate only; coherence is fine, CPU-only; needs a 64 KiB MPU region with subregions) |
| P3 | med | benches | no bench with a realistic ROM / cart-RAM footprint; only storm-gdma (+7.56% / +15% QSPI) | accepted, Stage 3 task (HyperRAM miss path) |
| P4 | med | gbc-firmware audio.rs:6-14, :117; paced.rs:409-419 | audio sample format tied to the H753 DAC | accepted, Stage 3 task (generic when SAI lands; ADR 0036) |
| P5 | low | video.rs:54-60 | ST7789V pixel format lives in the shared crate | Stage 3 task |
| P6 | low | scaler | 196k cycles; STRD lever | Stage 3 task |
| P7 | low | NVIC | no priorities set | Stage 3 task |
| P8 | low | player.rs:118 | stack 19,800 of 22,912 B scripted; 4 KiB clock table on the stack | Stage 3 task |
| P9 | low | B4, ratchet | no fe regression bound (B4 only < 100%); ratchet on +-1.7% noise; bench vs player | Stage 3 task |
| P10 | low | player.rs:60-63, paced.rs:517 | p99 histogram range < 550 MHz frame; `Elapsed` wraps after 7.8 s at 550 MHz | Stage 3 task |

## Untested edge cases (platform review)

- Cold power-on (hardware sign-off item 5).
- NCS/CLK float before QSPI init.
- No IWDG.
- 128 B stack guard vs the largest frame; MLSPERR.
- DMAUDR, MDMA TEIF and USART error paths: no fault injection.
- Temperature and VDD corners.
- No NVIC priorities set.

## Found while fixing

- 47f804b's new `cccsr=` banner field broke `check-selftest.sh` (gate B1 tool error); fixed 0e7ab23, which now requires `cccsr=0x00000101` on the clock line.
