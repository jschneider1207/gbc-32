# 0023: Desktop save files and save timing

- Status: Accepted
- Date: 2026-11-22
- Builds on: ADR 0015 (save persistence surface)

## Context
ADR 0015 left the on-disk format and timing to the front end. The desktop
front end (M7) must persist battery RAM and the MBC3 RTC next to the ROM,
without breaking the core's determinism (the core never reads a wall
clock) and without losing a save to a crash mid-write.

## Decision
**File:** `<rom stem>.sav` in the ROM's directory, only for cartridges
with `cart_info(rom).has_save()`. Contents: the cartridge RAM
(`ram_size` bytes), then for RTC carts the 48-byte footer used by
BGB, VBA-M and SameBoy: ten u32 LE (live S, M, H, DL, DH, then the
latched S, M, H, DL, DH) and a u64 LE unix time. DH is day bit 8 | halt
<< 6 | day carry << 7. `RtcState` maps onto it except `subsecond_clocks`,
which the format has no field for: it is not stored and loading restarts
the second (an error under one second per save). The 44-byte VBA footer
(u32 time) is accepted on load. An MBC2 save is the core's one byte per
cell form (ADR 0015).

**Load:** a missing file is a fresh save. A short file fills what it has
(rest zero), a long one is cut, an RTC cart whose size is not RAM + 48 or
44 gets its RAM but no clock; each warns on stderr. A read error other
than "not found" aborts (exit 1) rather than risk overwriting the file.
After `Emulator::load`, a stored clock goes through
`set_rtc_state(saved.advance_seconds(now - saved_unix))` (a stored time in
the future counts as zero elapsed; a halted clock does not advance).
`session::Options::sync_rtc_on_load` turns the advance off and `now` is
injectable, so tests don't depend on the host clock. Wall-clock reads
live in the front end only.

**Write:** atomically (`<name>.sav.tmp`, `sync_all`, rename), so a crash
leaves the old or the new file. Always at exit for a cartridge with a
save, because the RTC counts without setting the dirty flag; headless
runs do the same. During play, `Autosave::after_frame` (called once per
frame) peeks `save_dirty()`, clears it, and writes once one second of
emulated time (`CLOCK_HZ` base clocks, speed independent) passes with no
further change, since games write saves over several frames; a write owed for 10 s of emulated time is saved even if writes continue (amended after M7 review: a game using SRAM as work RAM never goes quiet). Peeking and
clearing at once makes the front end own the "write owed" state; a failed
write warns and is retried one second later. Emulated time, not wall
time, so headless runs and tests behave the same.

**CLI:** `gbc-desktop <rom> [--headless --frames N] [--scale K]`, no
dependencies. `--frames` needs `--headless` and `--headless` needs
`--frames` (else it would never exit). Usage errors exit 2, load or save
I/O errors exit 1. A locked CPU warns once and the run still exits 0.

## Consequences
- Saves are interchangeable with BGB, VBA-M and SameBoy for MBC3.
- The next steps (window, audio) call `session::run` with their loop as
  the body and `Autosave::after_frame` per frame; nothing else to wire.
- The footer's one-second resolution is a deliberate loss.
- Not covered: MBC7/TAMA5/HuC3 state (ADR 0015), a per-title DMG palette
  (the core default is used), saving on a crash or kill (only the
  periodic write protects against those).

## Amendment (after M7 review): sibling ROMs

`game.gb` and `game.gbc` side by side would both use `game.sav`. The save
path is now `<stem>.sav` as before, unless a sibling ROM with the same
stem and the other Game Boy extension (`gb`/`gbc`, either case) exists;
then it is `<file name>.sav` (`game.gbc.sav`). Common-case saves are
untouched; only someone who later adds the other-extension file next to an
existing `game.sav` has to rename it. Deterministic, no config.

## Amendment (2026-11-23): the save file is written on a thread

`write_atomic` (temp file, `sync_all`, rename) blocks for as long as the
disk and any antivirus take; on the main thread that time is a frame
stall. Measured on native Windows: 2.2-7.2 ms for a 32 KiB save (C: temp
and X:), one Wario Land 3 autosave 3.4 ms. Not the cause of the reported
hitches (those were the game's LCD-off periods, ADR 0024 amendment), but
a slower drive or a scan would be, so writes moved off the main thread.

- `save_writer::SaveWriter` (one per session, for a cartridge with a
  save) owns a thread that writes with `write_atomic`, unchanged.
- `Autosave::after_frame` decides as before, then encodes a snapshot
  (RAM copy plus RTC footer, stamped with the current unix time) and
  `submit`s it: it never waits for the disk. A snapshot submitted while
  another is still queued replaces it; one write is in flight at most.
  Writes therefore happen in submission order and the newest snapshot
  is always the last written: none is reordered, and only snapshots
  superseded before they were written are skipped.
- A failed write warns on stderr from the writer thread and sets a flag;
  the next `after_frame` sees it and owes the save again (retried one
  second of emulated time later, as before).
- Exit: `session::run` ejects the cartridge as before, then
  `SaveWriter::finish(exit save)` queues the exit save behind anything
  queued or in flight and joins the thread, so it is on disk before
  `run` returns, and its result is the exit status (a failure is exit 1,
  reported once). This path is the same for Esc, window close,
  Ctrl+C/SIGTERM (which only end the loop, ADR 0023 M7 review) and a
  panic in the loop (caught, saved, then resumed). Dropping a writer that
  was never finished (an early error return) still writes what was
  queued.
- Not covered, as before: a crash or kill of the process mid-play loses
  only what the last completed write did not have.

## Amendment (2026-11-24): HuC3 clock footer

"Not covered: ... HuC3 state" no longer holds for HuC3 (ADR 0032). A HuC3
cartridge's `.sav` is the cartridge RAM followed by 17 bytes, SameBoy's
packed `GB_huc3_rtc_time_t` (`Core/gb.h`, written by its
`GB_save_battery_to_buffer`; its `Core/save_state.c` notes BGB uses the
same layout): a u64 LE unix time, then u16 LE minute of the day, day,
alarm minute and alarm day, then one alarm-enabled byte. So HuC3 saves
are interchangeable with SameBoy and BGB, like the MBC3 ones.

- The timestamp is the time of the clock's last minute tick (the save
  time minus the seconds into the current minute), which is what
  SameBoy's `last_rtc_second` means. A synced load advances the stored
  clock by `now - timestamp`, so the seconds into the minute survive;
  the part second is lost, as for MBC3. With the sync off (tests,
  headless) the clock restarts at the top of its minute.
- Load rules mirror the MBC3 ones: a file of exactly RAM + 17 bytes
  restores the clock; a plain RAM dump loads quietly without one; any
  other longer file keeps the RAM and warns. The footer's fields go to
  the core unmasked: `Huc3RtcState::advance_seconds` and
  `set_huc3_rtc_state` cut them to their widths.
- In the code the two clocks travel as one `save::Clock` enum (`Mbc3` or
  `Huc3`) with `of(emu)`, `of_ejected`, `sync` and `restore`;
  `save::sync_rtc` became `Clock::sync`.
- Not covered: the input recording (ADR 0030) has no field for the HuC3
  clock, so a replay of a HuC3 game starts its clock at zero.

## Amendment (2026-11-26, Stage 2 board): the PC syncs for the board

Files move between the desktop and the dev board through `tools/gbc-pc`.
The board has no wall clock, so `gbc-pc` applies this ADR's sync on
load when it sends a save to the board (the same function: `Clock::sync`
now lives in `gbc_firmware::save` and the desktop calls it) and restamps
dumps with the PC's time. The desktop's own load and save are unchanged.
Details and the `--raw-save` mode: ADR 0040.
