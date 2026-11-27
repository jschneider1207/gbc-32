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
| `busy_mean/max` | the whole iteration **awake**: from one service-hook call to the next minus the cycles asleep in `WFI` (link, saves, statistics, input, core, audio push, frame hash (scripted image only: the live image does not hash), display hand-off, the idle hook's display and DAC checks, interrupt handlers) |
| `fe_mean/max` | front end: `busy - core` |
| `slack_min` | `budget - busy_max` (negative: over) |
| `over_budget` | frames with `busy > budget` |
| `budget` | one Game Boy frame of real time: 70,224 / 4,194,304 s = 1/59.7275 s = **7,902,557** cycles |
| `stats_max` | cycles of the statistics report itself (formatting these fields, and for a `Stats` frame its COBS/CRC encoding), the largest in the window. The report runs in one frame per window, and its cycles **stay in that frame's `busy` and `fe`** (and so in `over_budget`); review M4 |
| `fe_max_ex_stats` | the largest `fe` with that frame's report cycles taken out: the loop's own front end (the report frame is otherwise usually `fe_max`) |

Since boot: `underruns`/`underrun_frames` (DAC refills that found the
ring short; 0 = no audible gap), `overruns`, `presented`/`skipped`
(frames shown / superseded before TE), `display_errors`, `stuck`,
`dac_restarts` (watchdog), `total_frames`, `total_over_budget`,
`total_core_max`, `total_busy_max`, `reloads` (save loads), `rom_len`,
`rom_hash`; link: `link_frames`, `bad_frames`, `hellos`, `inputs`,
`saves_applied`, `saves_refused`, `dumps`, `dumps_done`, `naks`,
`replies_dropped`, `sessions`, `stats_sent`, `stats_dropped`, `rx_high`,
`rx_overflow` (reads that found the link receive ring possibly lapped:
every lap counts, and any read of more than half the 4 KiB ring may; 0 =
no received byte overwritten unread; `link_uart.rs`, M7 C5),
`rx_dma_errors` (times the link receive DMA stream stopped on a transfer
error and was re-armed; 0 = never; `link_uart.rs`, K11), `tx_errors`,
`baud`.

Audio to the PC (live mode only, before `stack_used`; ADR 0041):
`audio_on` (1 while streaming), `audio_sent` (`Audio` messages),
`audio_frames` (real frames in them), `audio_dropped` /
`audio_dropped_halves` (frames / DMA halves dropped because the link fell
behind; 0 = a complete stream), `audio_timeouts` (streams stopped for want
of the PC), `audio_cyc_per_frame` (main-loop cycles to pack and encode,
per frame sent: 107 measured), `audio_isr_max` (the DAC ISR's most
cycles **in this window**, reset per window: 8,356 with audio off, 10,496
on, so the copy adds ~2,140 cycles per half, 8.4 per frame).

`stack_used=<used>/<size>` (also since boot, in both modes; between
`stats_dropped` and `rx_high`): the deepest stack use in bytes, from the
paint high-water mark (`stack::usage`), of the DTCM stack region. **Stack
figures** (M5 review P-1, 2026-11-26, branch `m5-fix-stack`; region
22,912 B with the guard): live mode 12,968 B after boot and the first
`Stats`, and still 12,968 after a save load (`reload`), two dumps
(`save::take` + FNV) and more `Stats` windows (`gbc-pc load --sav`,
`dump`, `play`; run-player-save.sh PASS at 12,968): 9.9 KB of headroom,
the save paths are not the deepest. Scripted mode measured 19,800 B
before the guard (its end-of-run report path; about 3.1 KB headroom).

**Stack guard** (M1 review F-2): 128 B of no-access MPU region
(`mpu::REGION_STACK_GUARD`, region 11) at `__stack_guard`, between the
statics and `_stack_end` (memory_x::firmware_memory_x; every image of
this crate and h7-bench; build.sh checks the layout). An overflow that
reaches it faults; a deliberate recursion on the `boot` image
(2026-11-26) gave `hardfault sp=0x20000150 frame=ok pc=0 cfsr=0x00000092
hfsr=0x40000000 mmfar=0x2000017c`: DACCVIOL with MMFAR in the guard
(0x20000100..0x20000180), MSTKERR (the HardFault frame stacked into the
guard too, refused; the handler then skips the frame, and now prints
`frame=none` for it), no lockup. A frame larger than the guard can still
skip it, so the headroom above still matters.

**Over budget** means the loop could not keep real time if every frame
cost that much (plan D7, M6 "the whole loop fits real time"). The ring
(about 75 ms of audio) absorbs single slow frames, so an over-budget
frame is not necessarily an underrun.

## Audio to the PC

There is no speaker on the DAC pins, so the player can also stream what
the DAC plays to the PC (ADR 0041; protocol in `link-protocol.md`,
"Audio to the PC"):

    scripts/play-on-board.sh <rom> --seconds 60 --audio-wav /path/out.wav

or, with the player already running, `scripts/gbc-pc.sh play --seconds
60 --audio-wav out.wav`. The WAV (16-bit stereo, 32,768 Hz; the board
plays at 32,768.675 Hz) holds the real frames the DAC was fed, from the
first one received; `gbc-pc` prints `audio:` lines (bytes/s, `first_frame`,
gaps, the board's `audio_*` counters). Without `--audio` or
`--audio-wav` nothing is streamed.

**Listening live** (`--audio`; ADR 0041, "Amendment: live playback"):

    scripts/play-on-board.sh <rom> --audio [--seconds N] [--audio-wav out.wav]

or `scripts/gbc-pc.sh play --audio` with the player running. The sound
comes out of the PC's **default Windows output device** (waveOut), about
**90 ms** behind the board: 50 ms in gbc-pc's ring plus ~35 ms queued in
the device (4 x 10 ms buffers), plus whatever the Windows audio engine
adds. Both options together play and record. The volume is the game's:
use the Windows mixer.

At start gbc-pc prints the device format (`live: device 16-bit stereo
32768 Hz ...`; 48,000 or 44,100 Hz if the device refuses the board's
rate), then a `live:` line after each board `stats:` line (every 5 s)
and one at exit:

| Field | Meaning |
|---|---|
| `Playing` / `Prefill` / `Rebuffer` | waiting for the first 90 ms; playing; refilling after a PC-side underrun |
| `fill`, `min`, `max` | ring fill now and over the last window, ms of audio (target 50) |
| `ema` | the low-pass filtered fill the controller steers (0.5 s) |
| `latency~` | `ema` plus the device queue: the delay behind the board, Windows engine not counted |
| `corr` | the resampling ratio correction, ppm (+3,300 = 0.33% faster: the board's clock runs about that much fast); held within +-1% |
| `underruns`, `underrun_frames` | the ring ran dry while playing (the last frame is held until it is back at 50 ms); output frames held |
| `late` | wakes that found every device buffer finished: the device may have run dry (Windows was late, not the board) |
| `skips`, `skipped` | ring above 160 ms trimmed back to 50 ms (a PC stall); frames dropped |
| `start_trim` | the start-up backlog dropped when playing began (latency, not sound) |
| `gaps`, `missing`, `filled` | jumps in the board's frame counter (dropped halves); frames missing; frames filled with the last one (<= 125 ms per gap) |
| `held` | board-side underrun filler (`held`), replayed as the DAC played it |
| `overflow` | frames lost to a full 500 ms ring (should never happen) |
| `frames` | real frames received |

A clean session has `underruns=0 late=0 skips=0 gaps=0` and the board's
`audio_dropped=0`.
2026-11-26, Pokemon Red, 60 s: all of those 0, fill 30..94 ms (filtered
45..51), latency~ 80..86 ms, `corr` settling toward +3,300 ppm after
the start-up burst (ADR 0041).

**Checking a capture against the host** (deterministic with no keys and
no save): `headless <rom> --paced --frames N --dac-feed feed.bin` writes
the host's feed (since ADR 0044 s16le stereo, 4 bytes per frame; N video
frames give ~547 N frames, so pick N > (first_frame + frames) / 547); WAV
sample i maps back to the DAC code `(s >> 4) + 2048`, which must equal
the code of feed frame `first_frame + i`, `clamp(((f + 8) >> 4) + 2048,
0, 4095)` (`gbc_link::audio::code`; before ADR 0044 the feed held the
codes). 2026-11-26, Pokemon Red (no keys), 60 s from frame
901,120: all 1,971,968 frames equal, 0 gaps, 0 drops.

## Scripted mode

`GBC_HEADLESS_INPUT=<script> GBC_HEADLESS_FRAMES=<n> build.sh
player-script` builds `out/player-script.bin`: the same loop on the NOR's
ROM with the script as input (joystick and PC ignored, link off, UART at
115200, the core's default DMG palette). It ends with the `paced`
image's framed report (`paced` lines, `stats display paced`, `stats
display readback`, `stats dac`, `stats player`), so with the ROM in the
NOR:

```
RUN_PACED_FRAMES=<n> RUN_PACED_READ_SECS=<s> crates/gbc-nucleo-h753/run-paced.sh out/player-script.bin <rom>
```

compares its `paced` lines with the host's `headless --paced` (without
`--input`, so build it without `GBC_HEADLESS_INPUT` for that check).

**Panel readback** (scripted image only; plan B3 "the display readback
has 0 mismatches"; `src/display.rs` "Readback samples"). A full RAMRD
readback takes 90 ms (five frames), so it cannot follow every frame in
real time. Instead the image samples by a fixed rule that does not depend
on the ROM: the first frame shown at or after loop frame 120, then the
first one at least 300 frames after the previous sample started. Each
sample is a sentinel (the frame's inverse written by the CPU, 27 rows a
step), the normal TE transfer, then a readback against the buffer (4
rows a step). That is 62 steps of about 0.8 M cycles (1.7 ms), one per
loop iteration and only while the loop is ahead of the DAC. Newer frames
wait in the stash meanwhile (more `skipped`). After the run comes a
control: one buffer word inverted must read back as exactly 2 bad
pixels. The result is the line `stats display readback samples=
readbacks= mismatches= control_mismatches= first=120 every=300
readback_rows=4 sentinel_rows=27 steps= step_mean= step_max=
[first_bad=...]`. `run-replay.sh` requires readbacks >= 1, mismatches=0
and control_mismatches=2 for every entry. The live `player` has none of
this code.

Measured 2026-11-26 (branch `m7-readback`, the same entries without and
with the readback). The `paced end` lines (fbs, feed, clock) are
identical, and core max is within run-to-run noise:

| entry | readbacks | fe_max before / after | busy_max before / after | over budget | underruns |
|---|---|---|---|---|---|
| cgb-acid2 (600) | 2 | 456,798 / 1,238,060 | 1,144,009 / 1,920,777 | 0 | 0 |
| synth:worst-case (3600) | 12 | 457,375 / 1,240,179 | 3,839,866 / 4,574,504 (58% of 7,902,557) | 0 | 0 |
| rtc3test-sub-second (1793) | 6 | - / 1,244,511 | - / 2,676,382 | 0 | 0 |

step_max is 799,616 cycles. A scratch build that inverted one buffer
word before the first readback gave `mismatches=2
first_bad=frame120:(0,0)` and run-replay.sh exit 1.

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
