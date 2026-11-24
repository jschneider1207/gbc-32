# 0030: Desktop input recordings and the per-frame log

Status: Accepted (2026-11-23)

## Context

The human sees occasional stutter in Wario Land 3 and, most reproducibly,
in Link's Awakening DX screen slides, on native Windows; SameBoy shows the
slide stutter too, with vsync off and on. `GBC_FRAME_STATS` (ADR 0024)
gives 4-second aggregates, which can't say which frame stuttered or
whether the cause is the game's own motion, our loop, or the display.
Agents need objective, repeatable measurements they can take without the
human describing what they saw.

## Decision

**Input recordings** (`--record-input`, `--replay-input`,
`input_log.rs`). A recording is a text file: a header, then button
changes per frame, optional marks (key M), and an `end <frames>` line
(format in the module docs). Frame `n` is the `n`-th `run_frame` call
since load; both the window and the headless loop set the buttons once
before each call, so a recording replays identically in either.

Determinism contract: the core is deterministic given the ROM, the
cartridge RAM and RTC it starts from, its config (the DMG palette) and the
buttons per `run_frame` call. The header stores the ROM's checksums and a
FNV-1a 64 hash (the ROM itself is never stored), the palette, **the
cartridge RAM and RTC as loaded** (RTC after the wall-clock sync) and, for
information, the length and hash of the `.sav` it came from. A replay
refuses another ROM, starts from the header's RAM and RTC, never reads or
writes the `.sav` (no autosave, no exit save), and stops at the end of the
recording. We embed the RAM rather than requiring `--no-save` or a
sidecar file so a recording is self-contained (a 32 KB save is 64 KB of
hex) and recording needs no special mode: the human just plays. A
recording ends with `end`; a file without it (a crash mid-recording) is
refused rather than guessed at.

`--frame-hashes` writes a FNV-1a 64 of the visible picture per frame, so
two replays (Linux headless vs native Windows window) can be compared
line by line. Headless runs now double-buffer like the window, so both
hash the last complete picture. `--no-input` holds no buttons whatever is
pressed, for unattended runs of a title or demo.
`scripts/synth-input.exs` writes a recording from a short button program,
so a route through a game (e.g. LADX boot to screen slides) needs no
human.

**Per-frame log** (`GBC_FRAME_LOG=<csv>`, `frame_log.rs`, `dwm.rs`): one
row per emulated frame, in the window and in headless runs. The main
thread fills fixed-size rows in a preallocated buffer; every 600 rows the
batch goes to a writer thread that formats and writes them. Columns are
documented in `docs/desktop-diagnostics.md`. Two kinds of measurement:

- **Game side:** the shift of the picture since the previous picture
  (`motion_dx`, `motion_dy`, `motion_match`), found by matching the two
  frame buffers over shifts up to 8 px on one axis. The core's public API
  does not expose SCX/SCY/WX/WY, and adding that would be a core API
  change (needs an ADR and review); the frame buffer gives the motion the
  player actually sees, sprites and window included, at about 0.25 M
  pixel compares per changed frame, only when the log is on.
- **Display side (Windows):** after each present, `DwmGetCompositionTimingInfo`
  (a query; no GPU use by us) gives the vblank of the compositor's last
  composition (QPC) and the refresh period; refreshes are numbered from
  time alone (`cRefresh` counts compositions, not vblanks: measured, it
  advanced exactly once per present and hid every repeat). Since Windows 8.1 it only reports this global
  clock, not per-window statistics, and softbuffer presents through GDI,
  so where a picture landed is an **estimate**: the first vblank after the
  present returned. "Refreshes on screen" is the next present's landing
  refresh minus this one's (0 = replaced before a refresh, 2+ = shown
  again). The constant composition latency cancels in that difference; a
  present within a fraction of a millisecond of a vblank can be counted
  on the wrong side, which the vblank phase column shows. VRR displays
  break the fixed-period assumption.

`scripts/analyze-frame-log.exs` separates the two: game-side scroll
irregularities (a step that differs from the scroll's usual step) and
display-side irregularities (pictures shown outside {floor(r), ceil(r)}
refreshes, r = refresh rate / emulated rate, plus ceil(r) pictures beyond
what the 59.73 vs 60 Hz beat explains, plus pictures never presented),
each per minute, and per 10 s window.

## Consequences

- The window loop's cost with the log off is one `Option` check per frame
  and per present; with it on, a picture compare and a motion search per
  frame and a DWM query per present.
- `SaveFile::load` became `load_raw` + `decode` (the `.sav` hash).
- `session::run` gives the body the start state as a recording header.
