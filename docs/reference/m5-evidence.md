# Stage 2 M5: scripted input and the player-vs-host replay (board evidence)

The replay part of the M5 acceptance: "the `player` image in scripted
mode with display and audio running gives frame-hash and DAC-feed-hash
lists equal to the host's for the Stage 2 replay set". NUCLEO-H753ZI with
the ST7789V shield, QSPI NOR and DAC1, ST-LINK VCP, CPU 472 MHz,
2026-11-26. The 10-minute `synth:worst-case` soak and the save round trip
are separate acceptance items and are not covered here.

**Commit.** Branch `m5-replay` at `cc57af5` (firmware `f2d0c50`,
`9853591`; runner `d055d95`, `cc57af5`). The per-ROM `.bin` images are not
committed (each one compiles in its input and frame count). Rebuild them
with `run-replay.sh` and compare the cksums printed in the reports here.

## How romtest applies `input`, and how the firmware matches it

- romtest (`tools/romtest/src/manifest.rs`) converts each `at_s`/`hold_s`
  to base clocks (`seconds × 2^22`, floored, then rounded down to a
  multiple of 4). `machine::input_timeline` turns the entries into a
  sorted list of `(clock, held set)` changes. `machine::run` hands every
  change the core's queue takes (`SCHEDULED_INPUTS` = 4) to
  `Emulator::set_buttons_at(clock, ..)` before each `run_until`. Each
  change lands on its exact M-cycle, mid-frame, wherever the run calls
  end (ADR 0012). A change whose clock has already passed is a crash.
- The paced loop used to sample input once per frame and apply it at the
  frame's start clock. That is not romtest's emulation: changes would
  move to frame boundaries.
- `gbc_firmware::replay` adds a `clocks v1` table (`&[Change]`, no alloc)
  and `ClockScript`. `InputSource` gains a provided `apply` (the old
  frame-start `set_buttons`, so `Script` and `LiveInput` runs are
  unchanged). `ClockScript::apply` does romtest's hand-over: every change
  the queue takes, at its own clock. A change that is already past is
  applied at once and counted `late`. The `stats input clock_changes=
  handed= late=` line reports it. Only the existing public core API is
  used.
- Converter: `tools/replay-input table --manifest acceptance/stage1.toml
  --group <id>` uses romtest's own `manifest::parse` and `input_timeline`
  (romtest became lib + bin by moving its modules; no logic change). Its
  output is the table text, with `end = ceil(limit / FRAME_CLOCKS) + 1`
  run calls. It refuses a list with more than 4 changes in 2 frames,
  where a change could come up late. `player-script` embeds the table
  through `GBC_HEADLESS_INPUT`, and `headless --paced --input` accepts it.

### Host equality proof

`replay-input check --manifest acceptance/stage1.toml --group <id> --rom
test-roms/rtc3test/rtc3test.gb` runs romtest's `machine::run` (its own
loop) on an `Emulator` wrapped to hash every `FRAME_READY` frame. It then
runs `gbc_firmware::paced::run_with` with the parsed table and compares
every frame hash. stage1.toml has input only in the three rtc3test
groups:

| group | romtest frames | chain (romtest = paced) | late | result |
|---|---|---|---|---|
| rtc3test-basic | 951 | 996681f89bb93e7a | 0 | equal=yes |
| rtc3test-range | 712 | bf6f59436cfead95 | 0 | equal=yes |
| rtc3test-sub-second | 1787 | 49fdfa7575eed354 | 0 | equal=yes |

Seen failing: a mutation of `ClockScript::apply` that applies each change
at the start of its frame differs from frame 57 on (exit 1).
`headless --paced` with the same table prints `fbs=` equal to the romtest
chain, and the board prints the same value (below).

Unchanged outputs: `headless --paced` on cgb_sound for 35,837 frames (no
`--input`) still prints the committed M4 lines
(`m4-evidence/report-cgb_sound.txt`), byte for byte.

## Proposed concrete replay set (for the human; acceptance/stage2.toml is human-owned)

`crates/gbc-nucleo-h753/replay-set.txt`:

| ROM | frames | input |
|---|---|---|
| blargg/cpu_instrs/cpu_instrs.gb | 3600 | none |
| cgb-acid2/cgb-acid2.gbc | 600 | none |
| dmg-acid2/dmg-acid2.gb | 600 | none |
| blargg/cgb_sound/cgb_sound.gb | 2400 | none |
| **same-suite/dma/hdma_mode0.gb** ("a SameSuite HDMA ROM") | 600 | none |
| **rtc3test/rtc3test.gb** ("an rtc3test ROM") | 1793 (= its 30 s run) | stage1.toml group `rtc3test-sub-second` (3 presses, 6 changes) |

- hdma_mode0 runs HBlank HDMA. It passes in stage1 (group same-suite-cgb).
- rtc3test-sub-second has the most input of the three groups. basic and
  range pass the same host check (above) and can be added.

As a stage2.toml `replay` entry, this could read `{ rom =
"rtc3test/rtc3test.gb", input = <stage1 rtc3test-sub-second's>, exit =
{ run_s = 30 } }`. That is the human's call.

## Run command

```
cd crates/gbc-nucleo-h753
GBC_BUILD_LOCK=<lock> GBC_TEST_ROMS=<test-roms> RUN_REPLAY_OUT=<dir> ./run-replay.sh
```

Per entry: host `headless --paced` (with the table), a `player-script`
build with that input and frame count, the loader puts the ROM in the
NOR, then `run-paced.sh` checks the board's report against the host. It
checks: the three `paced` lines byte-equal; ring underruns and overruns
0; display presented ≥ 1 with no errors; DAC with no error, late refill
or restart; report and image cksums. On top of that, run-replay checks
`stats player total_over_budget=0` and, for a table, `stats input`
handed = all and late = 0. Speed (ppm) is printed but not checked here.
Over 10-60 s it is dominated by the initial ring fill (+6,250 ppm at 600
frames), so it stays the M4 10-minute criterion.

## Board results (one run of the whole list, exit 0)

| ROM | equal | underruns | over budget | core max (cycles) | slack min | `paced end` fbs / feed |
|---|---|---|---|---|---|---|
| cpu_instrs (3600) | yes | 0 | 0 | 1,766,374 | 6,762,239 | 104d757be654e571 / c500c437b50b10f9 |
| cgb-acid2 (600) | yes | 0 | 0 | 756,513 | 5,686,281 | 7e2cf3ebe88f49ac / 2afaea184d989e85 |
| dmg-acid2 (600) | yes | 0 | 0 | 755,300 | 6,759,289 | c0f4c5d148655c60 / 27b721de09365165 |
| cgb_sound (2400) | yes | 0 | 0 | 2,476,473 | 4,983,716 | bf833017780c5a60 / a47140640767c765 |
| hdma_mode0 (600) | yes | 0 | 0 | 1,053,747 | 6,404,218 | 9e715d5737062c81 / ac02af63243bea39 |
| rtc3test-sub-second (1793) | yes | 0 | 0 | 1,451,955 | 5,998,689 | 49fdfa7575eed354 / 9f5855f8e1d9f365 |

Budget 7,902,557 cycles per frame. The `drain` line equals `end` in
every run. rtc3test: input 6/6 handed, late 0. Its board `fbs` is the
romtest chain from the host proof. The full verified reports (between
their markers, with image cksums) are in `m5-evidence/report-<name>.txt`.

## Runner tests

`scripts/test-run-replay.sh` (no board) runs on the committed
rtc3test-sub-second report. It covers:
- exit 1: a doctored `paced end` (passed up from run-paced.sh), late=1,
  handed=5 of 6, total_over_budget=1;
- exit 2: a missing, repeated or malformed `stats input`, `stats player`,
  total_over_budget or core max; run-paced.sh exit 2 with every line
  present; no report; a missing capture; an empty list; a bad frame
  count;
- worst-of: pass + fail = 1, fail + tool error = 2;
- a checker that reads stdin.

The first full board run checked only cpu_instrs and reported PASS: the
flash, gbc-pc or serial step read the rest of the list from stdin. Fixed
in `cc57af5`. The list is now read on fd 3, and the selected entries are
counted up front. Each fix alone was mutation-checked against the stdin
test. Other mutations, each failing the tests: removing the late, handed,
over-budget, rc-1, rc-2, line-count, "stats input without a table" and
worst-of checks. With the frame-count guard removed, the loop's
arithmetic error is still caught by the checked-entries count (exit 2).

## Board state left

The `player` image (live mode) is flashed and cgb-acid2 is in the NOR
(hash e00f807ed1acf443). That was set by `scripts/play-on-board.sh
cgb-acid2.gbc --seconds 4` after the run.
