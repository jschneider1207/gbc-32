# CGB palette RAM (BCPS/BCPD, OCPS/OCPD)

Implemented in `crates/gbc-core/src/bus.rs` (`palette_data_write`,
`read_io`, `Sys::reset`) and `ppu/mod.rs` (`Ppu::write_bcpd`,
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

## Not yet (M5)
Palette access blocked while the PPU reads palettes (mode 3: writes
ignored but the index still increments, reads FF; same-suite
`blocking_bgpi_increase`), per-tile palettes from BG attributes, OBJ colors
from OCPD with the CGB OAM palette number (CGB-mode objects still use
OBP0/OBP1 and the compatibility colors).
