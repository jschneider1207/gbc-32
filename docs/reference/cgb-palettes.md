# CGB palette RAM (BCPS/BCPD, OCPS/OCPD)

Implemented in `crates/gbc-core/src/bus.rs` (`palette_index_step`,
`write_palette_data`, `read_io_other`, `Sys::reset`) and `ppu/mod.rs` (`Ppu::write_bcpd`,
`resolve_palettes`). The only model is CGB C (ADR 0003).

## Layout
64 bytes each for BG (FF69) and OBJ (FF6B): 8 palettes x 4 colors x 2
bytes, little-endian RGB555 (bits 0-4 red, 5-9 green, 10-14 blue; bit 15
unused). Same as our `Frame` pixels (ADR 0013), so colors are copied, not
converted; bit 15 is masked off for display but kept in RAM. Source: Pan
Docs "LCD Color Palettes (CGB only)".

## Index registers and auto-increment
- BCPS/OCPS bits 0-5: byte index; bit 7: auto-increment; bit 6 reads 1.
  Writable on a CGB in either mode (DMG compatibility mode included).
- A BCPD/OCPD **write** stores at the index, then, if bit 7 is set,
  increments bits 0-5; 3F wraps to 00 and bit 7 stays. A **read** returns
  the byte at the index and never increments.
- BCPD/OCPD writes are ignored and reads return FF in DMG compatibility
  mode.
Source: Pan Docs FF68-FF6B; SameBoy `Core/memory.c` `write_high_memory` /
`read_high_memory` (GB_IO_BGPI/BGPD/OBPI/OBPD).

## After the boot ROM (CGB mode)
The CGB boot ROM fades all 8 BG palettes to white (7FFF) before starting a
CGB game; OBJ palettes are not written (random on hardware, zero here).
Source: SameBoy `BootROMs/cgb_boot.asm`, `Preboot` (32 brightening steps
over all 64 bytes of `hBgPalettes`, then `LoadPalettesFromHRAM`). Without
this, CGB games that never write BCPD would show a black BG.

## Blocked around mode 3
BCPD/OCPD reads give FF and writes are dropped while the PPU reads
palettes, but a write still steps the index (Pan Docs: LCD Color
Palettes; SameBoy `cgb_palettes_blocked`). Window (`Ppu::palette_blocked`):
from line position 87, 86 in double speed (first line after LCD enable:
88 in both) to 4 dots after the mode-3 end (double speed: 2 after its
one-dot-later end). Calibrated by gambatte `cgbpal_m3/*`,
`enable_display/ly{0,1}_late_cgbp*`. The double-speed start is gambatte's
`cgbpAccessible` (`lineCycles + ds < 80`, behaviour only; its `lineCycles`
L is our `t = L + 7`): the default double-speed grid (odd dots) can't
tell 86 from 87, the `ds_lcdoffset1` grid (even dots) can:
`cgbpal_{read,write}_m3start_ds_lcdoffset1_1` access at 84 (open), `_2`
at 86 (blocked), from the sources in gambatte-core `test/hwtests/`.

## Not yet (M5)
Per-tile palettes from BG attributes, OBJ colors
from OCPD with the CGB OAM palette number (CGB-mode objects still use
OBP0/OBP1 and the compatibility colors).
