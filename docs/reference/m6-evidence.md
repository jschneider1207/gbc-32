# M6 evidence: 10-minute `synth:worst-case` soak, shipped placement

Acceptance (docs/plan/stage-2.md M6): in the `player` image with display,
audio and input running, the whole loop fits real time (under 7,902,557
cycles), with 0 frames over budget in 10 minutes of `synth:worst-case`.

2026-11-26, same dev board (COM5, 2,000,000 baud), main at `aabda84`.
Firmware sources are those of `6ad0066`: every commit after it is
docs-only (`git diff --stat 6ad0066..aabda84` touches only
`docs/plan/stage-2.md` and `docs/reviews/stage2-m6.md`). Shipped
placement: ADR 0042 layout, QSPI at 100 MHz, SIOO. Method as in
`m5-evidence.md`, "The 10-minute `synth:worst-case` soak".

## Scripted

```
cd crates/gbc-nucleo-h753
GBC_BUILD_LOCK=<lock> GBC_TEST_ROMS=<test-roms> RUN_REPLAY_OUT=<dir> \
  RUN_REPLAY_MAX_PPM=1000 ./run-replay.sh soak-set.txt
```

Result: exit 0, `worst-case: PASS equal=yes underruns=0 over_budget=0
core_max=3390856`.

| | scripted player, 35,837 frames |
|---|---|
| `paced` lines | equal to the host's and to M5's (`fbs=329470c6bc19a93f`, `feed=4b3d013ebbd78714`, drain = end; rom_fnv 8813cb5d7ea113b3) |
| over budget | `total_over_budget=0` (budget 7,902,557 cycles) |
| ring underruns / overruns (run) | 0 / 0 (min_fill 1,243 of 2,047) |
| speed | ppm +73, steady_ppm +20 (bound +-1000) |
| core min / mean / p99 / max | 786,580 (10.0%) / 3,350,505 (42.4%) / 3,390,856 (42.9%) / 3,390,856 (42.9% of budget) |
| busy max, slack min | 3,846,037, 4,056,520 |
| display | presented 33,032, skipped 2,804, transfers 33,032, errors 0, stuck 0 |
| DAC | late 0, te/dme/fe/udr 0, restarts 0, stop ok |

Image `player-script`: 272,728 B, cksum 4264904139 (matched by the
runner). `stats player underruns=6 underrun_frames=1310` counts since
boot and includes the drain and the stop ramp (player.md "Limits"); the
run's own figure is `stats paced ring`, 0.

M5 comparison: core max 3,412,142 (M5) -> 3,390,856 (-21,286 cycles,
-0.6%); mean 3,359,213 -> 3,350,505; busy max 3,862,965 -> 3,846,037;
slack min 4,039,592 -> 4,056,520.

## Live

`player` image (cksum 1831137809, 256,392 B, built from the same
sources), link on, `gbc-pc play` sending `Input` with no keys held:

```
PLAY_NO_BUILD=1 scripts/play-on-board.sh <dir>/worst-case.gb --seconds 640
```

Result: exit 0 (link: resends 0, naks 0, timeouts 0, stale 0, corrupt 0).
Last `Stats`: `total_frames=39702` (over 10 minutes),
**`total_over_budget=0`, `underruns=0 underrun_frames=0`**, `overruns=0`,
`total_core_max=3385726` (42.8% of budget), `total_busy_max=3902609`, last
window `slack_min=4062939`, `presented=37839 skipped=1862`,
`display_errors=0 stuck=0 dac_restarts=0`, link `bad_frames=0 naks=0`,
Counters run since boot with no drain or stop, so 0 underruns covers the
whole run. Audio streaming to the PC was off
underruns covers the whole run. Audio streaming to the PC was off
(`audio_on=0`); the DAC path itself runs on the board as in M5.

## Board state left

`player` image running (live mode) with Pokemon Red loaded in the NOR
(rom_hash 0x0bfa5d71919acbbc, confirmed in the `Stats` of the 4 s
`play-on-board.sh` run), restored after the live run.
