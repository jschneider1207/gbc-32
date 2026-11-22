# 0013: Frame pixel format and stride

- Status: Accepted
- Date: 2026-11-21

## Context
ADR 0006 fixed `Frame` as `[u16; 160 * 144]` RGB555 (bits 0-4 red, 5-9
green, 10-14 blue). The M1 API review (finding 4) asked that the format and
stride be confirmed before the PPU is written, because the MCU displays
want something else: STM32 LTDC takes RGB565 or ARGB1555 (with blue in the
low bits), and the dev board's ILI9341V takes RGB565 over the 8080 bus.
Changing the format after the renderer exists means touching every pixel
write.

Facts that decide it:
- Neither MCU display shows the frame 1:1. The dev board's panel is
  320×240 and the custom board's panel is TBD but larger than 160×144
  (`../gbc-32-hardware/docs/firmware-interface.md`: "160×144 (scaled)").
  The platform layer therefore already reads and writes every pixel once
  per frame to scale it, so a format conversion fused into that pass costs
  a shift-and-mask per pixel, not a separate pass over memory.
- RGB555 in this layout is the CGB palette RAM layout (Pan Docs: LCD Color
  Palettes). CGB colors go from palette RAM to the frame with no
  conversion in the core.
- The renderer will resolve colors per palette entry anyway (a 4-entry
  color table per palette, rebuilt on palette writes), not per pixel. So
  if a later format change is needed, it is a change to the palette-write
  path, not to the pixel loop.

## Decision
- Keep ADR 0006's format: RGB555, CGB layout, bit 15 zero.
- Stride is exactly 160 pixels: the frame is packed row-major with no
  padding. Platforms that need a different stride or alignment copy while
  scaling.
- Inside the core, colors are resolved at palette-write time: DMG
  compatibility palettes (BGP/OBP0/OBP1 through the `Config` colors) and
  CGB palette RAM are kept as ready RGB555 `u16` entries. The pixel loop
  only indexes a color table; it never converts.
- Conversion to the display's format (RGB565, byte-swapped RGB565 for
  the ILI9341V, ARGB1555, ...) is the platform's job, fused with scaling.

## Consequences
- No public API change; ADR 0006 stands.
- The pixel loop costs the same whatever the display wants.
- If Stage 2 measurements show the platform's scale-and-convert pass is a
  problem (e.g. LTDC scanning out a 1:1 160×144 layer with hardware
  scaling via DMA2D, where the core's frame could be shown directly), the
  fix is a per-platform output format chosen at `load`, applied in the
  palette-write path. That would be a new ADR amending 0006, needing the
  human.
