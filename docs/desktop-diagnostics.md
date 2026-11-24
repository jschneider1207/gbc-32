# Desktop front end: measuring stutter

Tools for finding out *where* a stutter comes from: the game's own
motion, our frame loop, or the display. Design and limits: ADR 0024
(pacing, `GBC_FRAME_STATS`, presentation phase and precise waits) and
ADR 0030 (recordings, frame log).

| What | How |
|---|---|
| 4 s pacing summary on stderr | `GBC_FRAME_STATS=1` |
| one CSV row per frame | `GBC_FRAME_LOG=<file.csv>` (window and `--headless`) |
| record your buttons | `--record-input <file.txt>` (key **M** marks a moment) |
| play a recording back | `--replay-input <file.txt>` (exits at its end) |
| compare two runs | `--frame-hashes <file.txt>` |
| unattended run, ignore keys/pads | `--no-input` |
| summarise a CSV | `elixir scripts/analyze-frame-log.exs <file.csv>` |
| script a recording without playing | `elixir scripts/synth-input.exs` (see its header) |
| today's presenting, for an A/B run | `GBC_PRESENT_PHASE=0` (present each picture at once) |
| no precise waits (A/B, CPU cost) | `GBC_PRECISE_WAIT=0` (wake when the OS wakes us) |

On exit, a window run with `GBC_FRAME_STATS` or `GBC_FRAME_LOG` prints
one more line: the precise waits (spins, time spun, share of one core),
the process's CPU time (Windows), and how pictures were presented
(`timed` by the presenter, `at once` without DWM timing, `redraw events`
from the OS), then the present-duration estimate.

## For the human: recording a session (Git Bash on Windows)

From the folder with `gbc-desktop.exe` and the ROM (paths are examples;
any folder works). The recording starts from your current save but never
changes how the save works: play normally.

```bash
cd /c/gbc-32
GBC_FRAME_LOG=ladx-frames.csv ./gbc-desktop.exe "Link's Awakening DX.gbc" --record-input ladx-session.txt
```

Play (for the screen-slide stutter: walk back and forth across a screen
edge for a minute or two), optionally press **M** right after a stutter
(prints `mark at frame N`), then close the window (Esc). That leaves:

- `ladx-session.txt` in the current folder: the recording (your buttons,
  plus a copy of the save you started from, so it replays exactly);
- `ladx-frames.csv`: the frame log of that session.

Hand both files to the agents (e.g. copy them to `X:\gbc-roms\` or
anywhere in WSL). An agent replays the recording natively with:

```bash
GBC_FRAME_LOG=/tmp/replay.csv scripts/run-windows.sh <scratch copy of the ROM> \
  --frames 100000 --replay-input ladx-session.txt
elixir scripts/analyze-frame-log.exs /tmp/replay.csv
```

The replay reads the save from the recording, never from or to your
`.sav`.

## For agents: unattended measurements

No human needed. Work on scratch copies of the ROM (never next to the
human's `.sav`); `run-windows.sh` copies files in and out of its own
scratch dir.

```bash
S=<scratch dir>; cp "/mnt/x/gbc-roms/Link's Awakening DX.gbc" $S/ladx.gbc
# 1. a header (starting state) from a fresh boot, no .sav beside the copy
target/release/gbc-desktop $S/ladx.gbc --headless --frames 1 --record-input $S/head.txt
rm -f $S/ladx.sav
# 2. a scripted route: boot -> new file -> 11 screen slides
elixir scripts/synth-input.exs $S/head.txt scripts/inputs/ladx-new-file-slides.txt $S/slides.txt
# 3. game side, headless (seconds): scroll cadence per slide
GBC_FRAME_LOG=$S/headless.csv target/release/gbc-desktop $S/ladx.gbc --headless \
  --replay-input $S/slides.txt --frame-hashes $S/h-linux.txt
elixir scripts/analyze-frame-log.exs $S/headless.csv
# 4. display side, native window (~2 min; the window closes itself)
GBC_FRAME_LOG=$S/native.csv scripts/run-windows.sh $S/ladx.gbc --frames 6880 \
  --replay-input $S/slides.txt --frame-hashes $S/h-win.txt
cmp $S/h-linux.txt $S/h-win.txt   # bit-exact replay on both platforms
elixir scripts/analyze-frame-log.exs $S/native.csv
```

Without a route, `--no-input --frames N` measures a title or demo (e.g.
Wario Land 3's title scroll). A recording the human made replays the same
way (step 4 with their file).

## The frame log (`GBC_FRAME_LOG`)

First lines are comments (`#`): format version, ROM title, and the display
refresh rate DWM reports (`unavailable` off Windows). Then a header row
and one row per emulated frame. Times are microseconds since the log
started (`Instant`, which is QPC on Windows). Empty = not applicable.

| Column | Meaning |
|---|---|
| `frame` | frame number: the n-th `run_frame` call since load (same numbering as recordings) |
| `wake_us` | when the event-loop wake-up that ran this frame began (window only) |
| `wake_late_us` | how late that wake-up started its work against the time the loop aimed for: a frame's or a timed present's due time (the loop wakes 2 ms early and spins the rest; ADR 0024 amendment "presentation phase"), else the plain wake-up time; empty when woken early by an event (window only) |
| `emu_start_us`, `emu_end_us` | `run_frame` call began / returned |
| `ready` | 1 = the frame completed a picture (`FRAME_READY`) |
| `blank` | 1 = the screen is white after it (LCD off, or the first frame after it turned on) |
| `changed` | 1 = what the screen shows differs from before this frame |
| `motion_dx`, `motion_dy` | game-side motion: how far the picture's content shifted since the previous picture, best match over shifts of up to 8 px along one axis (content moving right/down is positive, i.e. SCX/SCY going down). 0,0 = still. Empty unless this and the previous frame were both pictures |
| `motion_match` | share of compared pixels that match at that shift (1.000 = pure shift; lower with moving sprites or new tiles scrolling in) |
| `audio_fill_ms` | audio pacing fill estimate at the wake-up (window with audio) |
| `mark` | 1 = marked (key M while recording or logging, or a replayed mark) |
| `presents` | presents while this was the newest frame (0 = never shown by our loop, or nothing new to show) |
| `present_start_us`, `present_end_us` | the first of them: before the blit, after `present` returned |
| `refresh_period_us` | display refresh period from DWM (Windows) |
| `refresh_at_present` | number of the last vblank at or before `present_end_us`, counted in refresh periods from the first vblank seen in this run (DWM's last vblank time, extrapolated with the period; DWM's own `cRefresh` counts compositions, not vblanks, so it is not used) |
| `vblank_phase_us` | time from that vblank to `present_end_us` |
| `landed_refresh` | estimated refresh the picture first appears on: `refresh_at_present + 1` |
| `on_screen_refreshes` | the next present's `landed_refresh` minus this one's: refreshes this picture stayed on screen. 1 is ideal at 60 Hz; 0 = replaced before it was shown (dropped); 2+ = shown again (repeated). Empty for the last present |

Limits: DWM reports only its global clock (Windows 8.1+); a GDI
(softbuffer) present gives no per-window statistics, so landing refreshes
are estimates. A constant composition delay cancels out in
`on_screen_refreshes`; a present within a fraction of a millisecond of a
vblank may be counted a refresh early or late (check `vblank_phase_us`).
Variable-refresh (G-Sync/FreeSync) displays break the fixed-period
assumption. Rows are written by a background thread in batches of 600
and at exit; a crash loses the last batch.

## Reading the analysis

`scripts/analyze-frame-log.exs` prints:

- **game side**: every scroll of 8+ pictures (one axis, one direction),
  its usual step and the frames that moved differently (`f1234=0` = the
  picture stood still mid-scroll). Headline: *scroll irregularities per
  minute of game time*. Non-zero means the game itself moved unevenly
  (as it would on hardware, or an emulation bug); no display fixes that.
  Works headless.
- **display side** (native window only): the distribution of on-screen
  refresh counts; r = refresh rate / emulated rate (about 1.0045 at
  60 Hz); the expected "beat" (at 60 Hz a 59.73 fps game must show one
  picture twice every ~3.7 s, about 16 per minute, unless the game speed
  follows the display); and the headline *display irregularities per
  minute*: pictures shown outside {floor(r), ceil(r)} refreshes, plus
  ceil(r) pictures beyond the beat, plus pictures never presented. Blank
  (LCD-off) frames and pictures followed by one are excluded. Also how
  many of those fell during scrolls, and the present-to-vblank phase.
- timing: picture, present and emulation intervals; the *present delay*
  (picture completed to its present starting: the latency phase-aware
  presents add, normally 0-5 ms, at most ~8.4 ms); wake-up lateness
  (mean, percentiles, worst 10 s window).
- a table per 10 s of game time (`--window S` to change).

With phase-aware presents (Windows, ADR 0024 amendment "presentation
phase") the present-end phase histogram should be empty in its first bin
(no present returns within ~2.5 ms after a vblank), the 2-refresh count
should match the beat, and 0-refresh pictures should be rare. Whatever
remains usually lines up with wake-ups several ms late (`wake_late_us`):
the host did not run us, which no presenting can fix. To compare with the
old behaviour on the same build, run once more with `GBC_PRESENT_PHASE=0
GBC_PRECISE_WAIT=0` (`scripts/run-windows.sh` forwards both).
