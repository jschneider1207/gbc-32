# 0015: Save persistence surface (battery info, ejected RTC, dirty flag)

- Status: Accepted
- Date: 2026-11-22
- Extends: ADR 0006 (the `load`/`eject` return value; "buffers always
  come back" now includes the RTC state)

## Context
The caller owns persistence (ADR 0006): it lends the cartridge RAM buffer,
reads it back with `cart_ram()`, and saves the MBC3 clock with
`rtc_state()`. M4 has to finish that surface so a front end can persist
saves correctly. Three gaps:

- **No battery information.** Cartridge RAM without a battery is lost at
  power-off on hardware (Pan Docs, "The Cartridge Header", 0147). A front
  end that writes a `.sav` for every cartridge with RAM saves state the
  game never expects to see again, and loading it back can change
  behavior.
- **The RTC state was dropped on `load` over a cartridge and on `eject`.**
  Only the buffers came back. A caller had to remember to call
  `rtc_state()` first (M1 API review, low). On the MCU, switching games
  goes through exactly these calls.
- **No way to know the save changed.** The MCU saves to flash or eMMC,
  where every write costs time and wear. Without a signal it either
  rewrites the save on a timer or compares 8-128 KiB against the last
  saved copy, which needs a second buffer of that size.

Constraints: `no_std`, no `alloc` (ADR 0001); `Hw` under 1 KiB; the
cartridge RAM write is on the CPU's bus path (`write_mapped`,
`write_during_dma`, both code-size budgeted, ADR 0010).

## Decision

**Header query `cart_info(rom) -> Result<CartInfo, LoadErrorKind>`**,
usable before `load` (to decide whether to read a save file), and
`Emulator::cart_info() -> Option<CartInfo>` for the inserted cartridge.
`CartInfo { ram_size, battery, rtc }` is `#[non_exhaustive]` (a later
mapper may add fields). `CartInfo::has_save()` is `battery && (ram_size
!= 0 || rtc)`: a battery with nothing behind it is no save. Battery types
are every Pan Docs 0147 type named "+BATTERY" (03, 06, 09, 0D, 0F, 10,
13, 1B, 1E, 22, FF), plus FC (Pocket Camera), FD (TAMA5) and FE (HuC3),
which SameBoy's `GB_cart_defs` marks battery-backed. Mappers not emulated
yet still report their battery: the save is the game's, whatever the core
does with it today. `cart_ram_size` stays (it is `cart_info(rom)?.ram_size`
in effect) so existing callers don't change.

**`load` and `eject` return an `EjectedCart<'a>`** instead of the
`CartBuffers` tuple: `{ rom, cart_ram, rtc: Option<RtcState>, save_dirty:
bool }` (public fields, `#[non_exhaustive]`), with `save_ram()` (the
header-sized part of `cart_ram`, i.e. what `cart_ram()` returned) and
`into_buffers() -> CartBuffers<'a>`. `rtc` is the clock caught up to the
moment of removal (the same value `rtc_state()` would have returned just
before). Nothing is lost on a game switch, and the call that hands back
the buffers is the one place a caller needs to look. It is a value
returned by move, so the MCU's `&'static mut` buffers work as before and
nothing is stored. `LoadError` is unchanged: a failed `load` leaves the
old cartridge, and its RTC, in the emulator.

Rejected: keeping the tuple and documenting "call `rtc_state()` first" (the
status quo the review flagged); storing the last ejected RTC in the
emulator for a later query (state with an unclear lifetime, and `eject`
resets `Hw`); a callback (the core never calls out, ADR 0006).

**Dirty flag: `save_dirty() -> bool` and `clear_save_dirty()`**, now. It
is set by a game write to cartridge RAM that changes the stored byte (a
compare with the old byte: games that keep work data in cartridge RAM
rewrite the same values constantly), and by any game write to an RTC
register. It is not set by RTC counting, by latching (games latch every
frame), by `set_rtc_state` or by the MBC2 normalization in `load`: those
are not changes the caller needs to write out, and the caller saves the
RTC state with the wall-clock time whenever it saves the RAM. `reset`
keeps the flag (the save is still unsaved); a newly loaded cartridge
starts clean; `EjectedCart::save_dirty` carries it out. It is reported
whatever the battery, so callers combine it with `has_save()`. Peek and
clear are separate so a front end can wait for writes to settle (games
write a save over several frames) before saving.

Cost: one `bool` in `Cart`. `Cart` was exactly 128 bytes, so the flag
alone would have grown it to 136 and `Hw` from 992 to 1,000 bytes. To pay
for it, the separate `has_rtc` flag became a `Mapper` variant
(`Mbc3Rtc`, a unit variant so `Mapper` stays one byte): `Cart` stays 128
and `Hw` 992 bytes. On the write path the flag is a load, compare and OR
of the old byte:
`write_mapped` 322 -> 346 bytes and `write_during_dma` 616 -> 640 bytes
(budgets 480 and 860), a few cycles on cartridge RAM writes only. An
unconditional `dirty = true` would save a few bytes but report every
rewrite of the same value as a change.

**MBC2 save format is documented on `cart_ram()`**: one byte per 4-bit
cell as the CPU reads it, `0xF0 | value` (as SameBoy stores it); a loaded
save is normalized to that form, so files with the upper bits clear load
too.

## Consequences
- Breaking change for callers that destructured `load`/`eject` as a
  tuple: they call `.into_buffers()` (`tools/fuzz` updated; the desktop
  front end and `romtest` didn't use the return value).
- The desktop `.sav` task (M7) uses `cart_info().has_save()`,
  `save_dirty()` and the `EjectedCart` on exit.
- RTC persistence format (bytes on disk) is still the front end's choice;
  `RtcState` stays a plain struct. A common on-disk format (e.g. the
  BGB/VBA 48-byte RTC trailer, for compatibility with other emulators) is
  a front-end decision for M7.
- Not done: HuC3/TAMA5/MBC7 RTC or EEPROM state (those mappers aren't
  emulated); `CartInfo` can grow fields for them (`#[non_exhaustive]`).

## Amendment (2026-11-24)
"Mappers not emulated yet still report their battery" no longer applies:
a cartridge with an unsupported mapper does not load and has no
`CartInfo` (`cart_info` fails with `LoadErrorKind::UnsupportedMapper`,
ADR 0031, human's direction). `CartInfo` also gained `mapper: MapperKind`.

## Amendment (2026-11-24, HuC3)
"Not done: HuC3 ... RTC" no longer applies to HuC3: the mapper is emulated
and its clock is persisted through a second, additive surface
(`Emulator::huc3_rtc_state`/`set_huc3_rtc_state`, `Huc3RtcState`,
`EjectedCart::huc3_rtc`, `CartInfo::huc3_rtc`; ADR 0032). `RtcState`,
`rtc_state`, `set_rtc_state`, `EjectedCart::rtc` and `CartInfo::rtc` are
unchanged and stay MBC3-only. `CartInfo::has_save()` is now `battery &&
(ram_size != 0 || rtc || huc3_rtc)`. The dirty flag is also set by a HuC3
clock command that sets the time or the event time.
