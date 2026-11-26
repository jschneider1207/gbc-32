# 0040: The PC syncs the board's save clocks to real time

- Status: Accepted (agent decision)
- Date: 2026-11-26
- Builds on: ADR 0023 (desktop save files, the sync on load), ADR 0036
  (firmware crate), plan `docs/plan/stage-2.md` M5 (decision D6); M5
  review finding T-2

## Context
The board stamps a dumped save with the unix time of the save it loaded
and never advances a loaded clock (it has no wall clock; determinism).
The desktop syncs on load: live registers += now − stamp (ADR 0023).
`gbc-pc` moved saves without touching them, so a save went wrong on the
way desktop → board → desktop: saved at T0, played 2 h on the board (its
emulated RTC runs them), dumped with stamp T0, loaded on the desktop at
T0 + 2 h, which adds 2 h more: the clock is 2 h fast. The board also lost
the wall time between the stamp and the board load.

## Decision
The PC does the wall-clock half, so the firmware stays clock-free:

- **Shared rule.** `gbc_firmware::save::Clock::sync(saved_unix, now)`
  (no_std, `now` a parameter) is the ADR 0023 rule: advance by
  `now.saturating_sub(saved_unix)` with the core's `advance_seconds`
  (halted MBC3 clocks stay put, day overflow wraps and sets the carry,
  HuC3 counts minutes and days). The desktop's `Clock::sync` delegates to
  it; `gbc-desktop` takes `gbc-firmware` as a normal dependency. Before
  the old desktop body was deleted, a table test (running, halted,
  latched, day 511 rollover, carry set, HuC3 day wrap and seconds into
  the minute, future stamps, `u64::MAX`) showed both equal.
- **`load --sav`.** `gbc-pc` checks the save exactly as the board will
  (`save::check` for the ROM's layout; what the board would refuse is
  refused locally, exit 1) and sends `Image::encode_synced(now)`: the RAM
  as is, the clock synced to now, stamped now (HuC3: minus the seconds
  into the minute, as the desktop encodes). A 44-byte footer goes as 48.
- **`dump` and `play` dumps.** A clock footer is restamped with the PC's
  time when the dump arrived (`Image::encode_restamped`): the board's
  registers are the time at the dump, since its clock ran in emulated
  real time from the synced load. RAM and registers are untouched. `dump`
  has no ROM, so the footer is found from the length: every RAM size the
  core reports is 0 or a power of two ≥ 512, so length mod 512 = 48 is
  MBC3 and 17 is HuC3; a dump that does not then pass `save::check` is
  written as received.
- **`--raw-save`** (load, dump, play): no sync, no restamp, no local
  check; bytes as given. The board round-trip scripts
  (`run-save-roundtrip.sh`, `run-player-save.sh`) use it, since they
  check byte identity of the transport; this keeps exactly the bytes
  they compared before. `play-on-board.sh` (playing) uses the synced
  default.

## Consequences
- A save keeps real time across desktop ↔ board moves: each wall-clock
  second is counted once (host test
  `desktop_to_board_to_desktop_counts_real_time_once`).
- Losses per board load and per dump, as the format allows: MBC3 drops
  the RTC's part second, HuC3 the seconds into the current minute (the
  board restarts the minute and cannot report it). The board clock is as
  true as its emulation speed is real time.
- `gbc-pc` now depends on `gbc-firmware` (and so `gbc-core`), both
  no_std; the Windows build carries the core's save code.
- Not done: syncing the board's clock while a game keeps running across
  PC sessions (only load and dump see the PC's time).
