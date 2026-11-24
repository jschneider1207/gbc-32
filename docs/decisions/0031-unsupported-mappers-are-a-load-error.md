# 0031: A cartridge with an unsupported mapper is a load error

- Status: Accepted
- Date: 2026-11-24

## Context
Until now `Mapper::from_header` mapped every cartridge type (header 0147)
the core does not emulate to "no MBC" (`_ => Self::None`, ADR 0016 "Out of
scope for Stage 1"). Such a cartridge loaded, got RAM sized from 0149
that is always enabled (no MBC has no enable register), and `CartInfo`
reported it battery-backed (ADR 0015). Stage 1 M10 review, findings A4-1
(`docs/reviews/stage1-m10/A4-dma-cart.md`) and D12 (`D-rust.md`): a HuC1
game (type FF) with an existing save runs bank 1 where it expects another
bank, executes garbage, stray writes to A000-BFFF dirty the save, and the
front end overwrites the user's good `.sav`. The caller gets no signal;
on the handheld the result is a hang with no message.

Confirmed by tests written against the old code: `cart_info` of a type FE
ROM returned `ram_size: 32768, battery: true`, `Emulator::load` returned
`Ok`, and the desktop front end exited 0 after such a run.

The human set the direction: fail safely and visibly. Refuse the
cartridge with a typed error that names the mapper; no RAM, no battery
flag, no run.

## Decision

### API (additive; `LoadErrorKind` and `CartInfo` are `#[non_exhaustive]`)
```text
enum MapperKind { RomOnly, Mbc1, Mbc2, Mbc3, Mbc5,              // emulated
                  Mmm01, Mbc6, Mbc7, PocketCamera, Tama5, HuC3, HuC1,
                  Unknown }                                     // #[non_exhaustive]
  const fn from_cart_type(u8) -> Self     // header 0147 only
  const fn is_supported(self) -> bool
  const fn name(self) -> &'static str     // also Display
LoadErrorKind::UnsupportedMapper { mapper: MapperKind, cart_type: u8 }
CartInfo { mapper: MapperKind, .. }       // always a supported kind
```
`Emulator::load`, `cart_ram_size` and `cart_info` all fail with
`UnsupportedMapper` for a kind that is not supported. `load` hands the
buffers back in the `LoadError` and leaves the emulator unchanged, like
the other load errors (ADR 0006). Signatures are unchanged.

`cart_ram_size`/`cart_info` fail too, not only `load`, because front ends
call them first, to size the RAM buffer and to decide whether to open a
save file. Failing there means the desktop front end stops before it
looks at the `.sav`, with no change to its code, and the MCU front end
gets the same order for free. A `CartInfo::mapper_supported` flag (D12's
additive proposal) was rejected: a caller that ignores it still runs the
game and still risks the save.

Error order: `RomTooSmall`, then `UnsupportedMapper`, then
`CartRamTooSmall`.

### Which type codes load
| 0147 | Kind | Loads |
|---|---|---|
| 00, 08, 09 | no mapper (ROM, ROM+RAM, ROM+RAM+BATTERY) | yes |
| 01-03 | MBC1 (MBC1M by contents, ADR 0016) | yes |
| 05-06 | MBC2 | yes |
| 0F-13 | MBC3 / MBC30 (0F, 10: RTC) | yes |
| 19-1E | MBC5 (1C-1E: rumble) | yes |
| 0B-0D | MMM01 | no |
| 20 | MBC6 | no |
| 22 | MBC7 | no |
| FC | Pocket Camera | no |
| FD | TAMA5 | no |
| FE | HuC3 | no |
| FF | HuC1 | no |
| 04, 07, 0A, 0E, 14-18, 1F, 21, 23-FB | not assigned (Pan Docs 0147); 15-17 were "MBC4" in old lists, a chip that does not exist | no |

Unassigned codes are refused as well. The alternative, keeping them as
"no MBC" for sloppy homebrew headers, was checked against the test ROMs:
all 4,510 ROM files under `test-roms/` (a superset of the Stage 1
manifest) use only 00, 01, 02, 03, 05, 06, 0F, 10, 11, 19, 1A and 1B, so
nothing relies on the leniency, and an unknown code gives no reason to
believe the cartridge has no mapper. ROM-file padding makes this matter:
a header left at FF or FE filler reads as HuC1 or HuC3. Tolerances that
ROMs do rely on stay: short ROMs (`MIN_ROM_SIZE`), unknown RAM size codes
(no RAM), and RAM on an MBC type that does not declare it (ADR 0016).

### Internal structure
`Mapper::from_header` resolves the emulated mapper once at load, by an
exhaustive match on `MapperKind` (plus the type byte for RTC and rumble,
and the ROM contents for MBC1M), and returns the error for the
unsupported kinds. Adding HuC1 or HuC3 is: a `Mapper` variant, its arm in
that match, its kind in `MapperKind::is_supported`, and its bank logic.
The test `supported_kinds_are_the_ones_that_load` checks all 256 codes
across `is_supported`, `from_header`, `cart_ram_size` and `Cart::new`, so
the two lists cannot drift apart.

### Callers
- **Desktop:** `session::run` reports `cart_info`'s error and exits 1
  before any save path is built (`<rom>: unsupported cartridge: HuC1
  (header cartridge type FF)`). Test:
  `unsupported_mapper_exits_1_and_leaves_the_save_alone`.
- **romtest:** a load error (`crash` status), as for any ROM the core
  rejects. No pass/fail logic changed.
- **Fuzz:** the load error path already hands the buffers back and checks
  the emulator is still usable. The generator now picks a refused type
  for about one input in six (before: about two in five would have been
  refused), so most inputs still run.

## Consequences
- This reverses two statements in Accepted ADRs, on the human's
  direction: ADR 0016 "They stay 'run as no MBC'" / "Unknown type codes
  already run as 'no MBC'", and ADR 0015 "Mappers not emulated yet still
  report their battery". Both ADRs carry a dated note pointing here.
- Games on unsupported mappers no longer start at all. That is the point,
  but it also removes the accident of a small unsupported-type ROM that
  happened to work without banking.
- `has_battery` keeps the full Pan Docs/SameBoy table (0D, 22, FC-FF
  included); those entries become live again when their mapper is added.
- HuC1 and HuC3 are planned next (A4-1); MBC7 needs a sensor input in the
  API (ADR 0006 extension) and stays refused until a target game needs it.

## Amendment (2026-11-24): HuC1 and HuC3 load
The human approved both for Stage 1 (STATUS, "Decided in session 16"), and
they are emulated (`docs/reference/hudson-mappers.md`; HuC3 clock state:
ADR 0032). In the table above, **FE (HuC3) and FF (HuC1) now load**;
`MapperKind::is_supported` is true for `HuC1` and `HuC3`, and the "emulated"
list in the API sketch gains them. Still refused: MMM01 (0B-0D), MBC6
(20), MBC7 (22), Pocket Camera (FC), TAMA5 (FD) and every unassigned code.

One consequence for the padding case named above: a ROM file whose header
byte 0147 is FF or FE filler now loads as HuC1 or HuC3 instead of being
refused. That is what the header says; such a file has no valid header at
all (no logo, wrong checksum), which the core does not check.

The tests that used FF or FE as their example of a refused mapper use MBC7
(22) or TAMA5 (FD) instead; `supported_kinds_are_the_ones_that_load` lists
FE and FF.
