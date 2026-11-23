# 0025: Desktop picks DMG compatibility palettes like the CGB boot ROM

- Status: Accepted
- Date: 2026-11-22
- Builds on: ADR 0006 (core public API), ADR 0023

## Context
The core has no boot ROM. For DMG-only cartridges the caller passes the
BG/OBJ0/OBJ1 palettes in `Config::dmg_palettes` at `load`
(docs/architecture.md, "Core ↔ platform boundary"). The desktop front end
passed `Config::default()`, i.e. grayscale, so Pokémon Red played in gray.
A real CGB colorizes DMG games: the boot ROM hashes the title of
Nintendo-licensed carts and looks the hash up in a table (Pan Docs, "Power
Up Sequence" > "Compatibility palettes").

## Decision
`gbc-desktop/src/compat_palette.rs` reproduces the boot ROM's choice:
Nintendo licensee (old 0x01, or 0x33 with new "01") → sum of title bytes
0x134-0x143 → table lookup, with the 4th title byte as tie-break for the
14 shared sums; no match, another licensee, or a CGB-flagged cart →
palette ID 0 (the boot ROM's default: red OBJ, green/blue BG). Colors stay
in the boot ROM's RGB555 exactly.

The tables are SameBoy's `BootROMs/cgb_boot.asm` (MIT, notice kept next to
them). A script cross-checked them against ISSOtm's disassembly of the
real boot ROM, including its palette shuffle flags: all 94 entries resolve
to identical OBJ0/OBJ1/BG colors, so there was no disagreement to resolve.
SameBoy's own extra palettes and the logo-time button overrides are left
out (no boot animation to press buttons during).

`--dmg-palette auto|gray` (default `auto`) keeps grayscale available.

## Consequences
- DMG games look as they do on a CGB, without shipping a boot ROM.
- The table lives in the front end, about 0.6 KB of data plus the lookup.
  The MCU front end (Stage 2) will need the same choice. Moving it into
  `gbc-core` would let both share it, but core `.text` has only about
  1.2 KB of budget left, so that is a later decision; until then the MCU
  front end can reuse this module's tables.
- The core API is unchanged.
