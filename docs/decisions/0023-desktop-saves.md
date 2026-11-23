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
further change, since games write saves over several frames. Peeking and
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
