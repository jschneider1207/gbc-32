# The `player` image: playing on the dev board (Stage 2 M5)

The `player` image runs one game from the shield's QSPI NOR with the
display, the DAC audio and live input, and talks to the PC over the
ST-LINK virtual COM port (`gbc-link`, `docs/reference/link-protocol.md`).
Source: `crates/gbc-nucleo-h753/src/bin/player.rs`.

## Wiring

- NUCLEO-H753ZI with the X-NUCLEO-GFX02Z1 shield (ST7789V panel,
  MX25L6433F QSPI NOR, joystick). One USB cable to the ST-LINK
  connector: power, flashing (mass-storage drive) and the COM port.
- Audio: DAC1 on PA4 (left) and PA5 (right), line level, 32,768 Hz
  (`docs/reference/audio-dac.md`). Use powered speakers or an amplifier;
  never a speaker straight on the pins.
- Nothing else: no extra buttons (plan D2).

## Playing

```
scripts/play-on-board.sh <rom> [--sav <file.sav>] [--sav-out <file.sav>] [--seconds N]
```

From WSL. It copies the ROM (and the save) to a scratch dir first and
never writes next to them, then (holding the board lock throughout):

1. If the board already runs the player with this ROM in the NOR (a 2 s
   `gbc-pc play` sees its `Stats` with `image=player`, then `gbc-pc load`
   answers `AlreadyLoaded`), nothing is flashed: about 10 s to play.
2. Otherwise it flashes the `loader`, loads the ROM into the NOR
   (`gbc-pc load`), and flashes the `player` (about 60 s, mostly the two
   ST-LINK flashes).
3. With `--sav`, the save goes to the player (`gbc-pc load <rom> --sav`).
4. `gbc-pc play` until Esc (or `--seconds N`).

Saves dumped during play go to `--sav-out` (default: `<scratch>/<rom
stem>.sav`, printed at the start); the file appears when play ends.

Builds: `crates/gbc-nucleo-h753/build.sh player` (`out/player.bin`). The
wrapper builds gbc-pc, the loader and the player first unless
`PLAY_NO_BUILD=1`.

## Keys and buttons

| Game Boy | PC keyboard (`gbc-pc play`) | Board |
|---|---|---|
| D-pad | arrow keys | joystick LEFT/RIGHT/UP/DOWN |
| A | X | |
| B | Z | |
| Start | Enter | B1 (blue button) |
| Select | Right Shift or Backspace | |
| save dump | F5 | joystick CENTER + B1 together |
| quit | Esc | |

Board and PC buttons are OR-ed, sampled once per frame (the joystick is
debounced over 2 frames). The PC repeats its state every 500 ms; with no
`Input` for 90 frames (1.5 s) the board releases the PC's buttons.

## What the player does

- ROM: the ADR 0037 image in the NOR, validated at boot (header, length,
  FNV-1a 64 hash). `RomBegin` with that length and hash gets
  `AlreadyLoaded`; any other ROM gets `Nak UnexpectedType` (the loader
  takes ROMs, the player does not).
- No valid image in the NOR: the panel goes dark red and a text line
  `gbc-nucleo-h753 player no-rom stage=<map|header|validate> error=...`
  repeats every 2 s at the link rate; `Hello` is answered so the wrapper
  can tell. Run `play-on-board.sh` to load a ROM.
- DMG-only games get one compatibility palette (romtest's common
  palette, `gbc_firmware::player::COMPAT_PALETTE`), set before the first
  frame and after every save load. No per-title palettes.
- Audio paces the emulator (the M4 paced loop): it runs a frame whenever
  the sample ring has room, else sleeps in `WFI`.
- Display: the newest completed frame goes to the panel at each TE; a
  frame superseded before TE is counted as skipped (M4 review P-7).
- Saves: loaded with the ROM or any time (`gbc-pc load <rom> --sav`, a
  power cycle of the game with the new RAM and RTC), dumped on request or
  on the chord. A dump of a just-loaded save is byte-identical
  (`--raw-save`). The board never reads a wall clock; `gbc-pc` syncs the
  clock footer to real time on load and restamps it on dump, as the
  desktop does (ADR 0040, `link-protocol.md` "Who syncs the RTC").

## Statistics

Every 300 frames (5.02 s), and right when a PC session starts. With a
PC session active (a link frame within the last 180 frames) they go as a
`Stats` frame (`frames`, `cycles_mean`, `cycles_max` of the core, and
the fields below as `key=value` text in `extra`), which `gbc-pc play`
prints as `stats: image=player ...`; without one, as a text line
`gbc-nucleo-h753 player stats image=player ...` (ended by 0x00, sent
only when no frame is in flight). Cycles are CPU cycles at 472 MHz.

Per window (the frames since the last report):

| Field | Meaning |
|---|---|
| `frames` | iterations of the loop (one `run_frame` call each) |
| `core_min/mean/p99/max` | cycles of the `run_frame` call alone. `p99` from a 128-bin histogram of 65,536-cycle bins: the top of the bin holding the 99th percentile, capped at `core_max` |
| `busy_mean/max` | the whole iteration **awake**: from one service-hook call to the next minus the cycles asleep in `WFI` (link, saves, statistics, input, core, audio push, frame hash, display hand-off, the idle hook's display and DAC checks, interrupt handlers) |
| `fe_mean/max` | front end: `busy - core` |
| `slack_min` | `budget - busy_max` (negative: over) |
| `over_budget` | frames with `busy > budget` |
| `budget` | one Game Boy frame of real time: 70,224 / 4,194,304 s = 1/59.7275 s = **7,902,557** cycles |

Since boot: `underruns`/`underrun_frames` (DAC refills that found the
ring short; 0 = no audible gap), `overruns`, `presented`/`skipped`
(frames shown / superseded before TE), `display_errors`, `stuck`,
`dac_restarts` (watchdog), `total_frames`, `total_over_budget`,
`total_core_max`, `total_busy_max`, `reloads` (save loads), `rom_len`,
`rom_hash`; link: `link_frames`, `bad_frames`, `hellos`, `inputs`,
`saves_applied`, `saves_refused`, `dumps`, `dumps_done`, `naks`,
`replies_dropped`, `sessions`, `stats_sent`, `stats_dropped`, `rx_high`,
`tx_errors`, `baud`.

**Over budget** means the loop could not keep real time if every frame
cost that much (plan D7, M6 "the whole loop fits real time"). The ring
(about 75 ms of audio) absorbs single slow frames, so an over-budget
frame is not necessarily an underrun.

## Scripted mode

`GBC_HEADLESS_INPUT=<script> GBC_HEADLESS_FRAMES=<n> build.sh
player-script` builds `out/player-script.bin`: the same loop on the NOR's
ROM with the script as input (joystick and PC ignored, link off, UART at
115200, the core's default DMG palette). It ends with the `paced`
image's framed report (`paced` lines, `stats display paced`, `stats dac`,
`stats player`), so with the ROM in the NOR:

```
RUN_PACED_FRAMES=<n> RUN_PACED_READ_SECS=<s> crates/gbc-nucleo-h753/run-paced.sh out/player-script.bin <rom>
```

compares its `paced` lines with the host's `headless --paced` (without
`--input`, so build it without `GBC_HEADLESS_INPUT` for that check).

The script may also be a `clocks v1` table (`gbc_firmware::replay`): an
`acceptance/*.toml` group's `input`, converted with romtest's own code by
`target/release/replay-input table --manifest acceptance/stage1.toml
--group <id>`. Its changes are scheduled at their exact clocks as romtest
does (not on frame starts), and a `stats input clock_changes= handed=
late=` line follows the run. `replay-input check ... --rom <rom>` shows
on the host that this loop gives romtest's frames.

The M5 replay check runs a whole list (build, NOR load, run, compare per
ROM; docs/reference/m5-evidence.md):

```
GBC_BUILD_LOCK=<lock> crates/gbc-nucleo-h753/run-replay.sh   # replay-set.txt
```

## Limits

- Saves up to 64 KiB of cartridge RAM (+ the RTC footer): AXI SRAM has
  no room for a 128 KiB save buffer beside the 128 KiB cartridge RAM and
  the 128 KiB-aligned panel buffer. A game with 128 KiB of RAM runs, but
  its saves are refused (`Nak OutOfRange`) and dumps fail.
- The core's frame buffer is in DTCM in this image (the M6 target
  placement), the rest as the `paced` image.
- No persistent saves on the board (plan D6): dump before power-off.
- One ROM at a time in the NOR; changing games re-flashes twice.
- In scripted mode the `stats player` underrun fields count since boot,
  so they include the drain's last pop and the DAC's stop ramp after the
  run (both pop an empty ring); the run's own figure is `stats paced ring`.
