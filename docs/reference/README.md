# Reference notes

Distilled hardware knowledge, **in our own words, with sources**. One file
per topic (e.g. `ppu-mode3-timing.md`, `mbc3-rtc.md`, `stm32h7-fmc.md`).

Rules:
- Summarize and cite (URL plus section). Don't paste datasheets or whole
  spec pages. Keep notes short enough to read in a minute.
- Record what a test or experiment proved, plus the source, especially when
  sources disagree.
- Large PDFs (reference manuals, datasheets) aren't committed. Link to them.

## Primary sources

- Pan Docs: https://gbdev.io/pandocs/
- Game Boy: Complete Technical Reference (Gekkio): https://gekkio.fi/files/gb-docs/gbctr.pdf
- SameBoy source (behavior reference): https://github.com/LIJI32/SameBoy
  (`mode3-oracle.md`: its mode-3 loop as a test-only oracle)
- Test suite how-tos: `test-roms/<suite>/game-boy-test-roms-howto.md`
- STM32H742/743/750/753 reference manual RM0433 (Stage 2)
- X-NUCLEO-GFX02Z1 user manual (Stage 2)
- STM32H723/733/725/735/730 reference manual RM0468, STM32H735xG datasheet DS13312, and errata sheet ES0491 (Stage 3+, custom board MCU STM32H735IGK6)
