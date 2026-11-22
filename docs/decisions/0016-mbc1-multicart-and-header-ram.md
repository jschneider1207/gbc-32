# 0016: MBC1 multicart detection and header RAM rules

- Status: Accepted
- Date: 2026-11-22

## Context
M4 completes cartridge support. Two gaps from the M0 review:

1. **MBC1M multicarts.** Multi-game MBC1 cartridges wire BANK1 to the ROM
   address with only 4 bits, and BANK2 above them (ROM bank bits 4-5, not
   5-6). The header has no multicart marker: a multicart reports type
   01-03 and a normal ROM size, so the mapper wiring has to be guessed
   from the ROM contents. Pan Docs (MBC1, "MBC1M"); SameBoy
   `Core/mbc.c` (`GB_MBC1M_WIRING`).
2. **Header edge cases** listed in the M4 plan: a cartridge type without
   RAM (0147) with a nonzero RAM size (0149), MBC7 EEPROM size, HuC3 RTC.

## Decision

### Multicart detection (`is_mbc1m` in `cart/mod.rs`)
A type 01-03 ROM is treated as MBC1M when it is at least 0x44000 bytes and
its Nintendo logo (0104-0133, 0x30 bytes) is repeated at 0x40104, i.e. at
the start of bank 0x10. This is SameBoy's test, unchanged.

Why it works: a multicart holds several complete games, each with its own
header and logo, and the menu stage of the cartridge maps each game's
first bank to bank 0x10 multiples (0x00, 0x10, 0x20, 0x30 in 4-bit
wiring). The logo copy at bank 0x10 is therefore the second game's
header. Hardware needs the real logo only in bank 0; the copy exists
because the second game is a stand-alone ROM image.

False positives: a normal MBC1 game of 1 MiB or more would need the
bytes at 0x40104 (the middle of bank 0x10, in code or data) to equal its
48-byte logo. A real game doesn't, and no ROM in the acceptance manifests
changes behavior (`scripts/diff-fingerprint.sh`: only the multicart test
ROM differs). False negatives: a multicart whose second game is not at
bank 0x10 (e.g. a 2-game 4 Mbit cart, which has no bank 0x10) is not
detected. On a 4 Mbit cart with 4-bit wiring and two games at 0x00/0x10
the ROM is 0x80000 bytes and fails the size test; this matches SameBoy and
no known test needs it. A synthetic ROM whose logo bytes are all zero in
both places is also "detected"; test fixtures must not do that (the bus
unit test sets a byte).

This is not test-ROM detection (AGENTS.md hard rule). It inspects
cartridge contents, the way the physical cartridge variants differ and
the way SameBoy does, and doesn't look at the title, a hash or a path.
Any ROM with this layout gets the wiring.

Implementation: a unit `Mapper::Mbc1M` variant (a data-carrying variant
would make `Mapper` two bytes and push `Hw` over its limit, as in ADR
0015). Only the bank computation in `update_banks` differs, so the
precomputed `rom_win` and the read path are unchanged. `Hw` stays at 992
of 1,024 bytes. The "0 selects 1" fix-up applies to the 5-bit BANK1 value
before masking to 4 bits, as in SameBoy.

### RAM size rules
- **Type without RAM, nonzero 0149 on an MBC type** (e.g. 01 with 0149 =
  02): in scope; the existing behavior stays. SameBoy
  (`GB_configure_cart`) logs "Assuming cartridge has RAM" and uses the
  0149 size, which is what `cart_ram_size` already does. Real MBC1
  dumps with a mislabeled type exist.
- **ROM ONLY (00) with nonzero 0149**: implemented. SameBoy only upgrades
  types whose mapper isn't "no MBC", so type 00 gets no RAM (ROM+RAM is
  08). `cart_ram_size` returns 0 for type 00. Unit tests that relied on
  RAM at type 00 now use 08. No ROM in the manifests changes (fingerprint).
- Unknown type codes already run as "no MBC" (M0). SameBoy gives them no
  RAM either; not changed, since no ROM needs it. The same goes for
  SameBoy's "type 00 but ROM over 32 KiB: assume MBC3" guess.

### Out of scope for Stage 1
- **MBC7** (accelerometer, 93LC56 EEPROM; SameBoy sizes it 0x100 bytes)
  and **HuC3** (RTC, IR) and **HuC1**, MMM01, TAMA5, Pocket Camera: no
  test ROM in the Stage 1 manifests and no named Stage 1 game needs them.
  They stay "run as no MBC" (battery flags are still reported, ADR 0015).
  Add them as post-Stage-1 tasks if a target game requires one; MBC7
  needs a sensor input in the core API (an ADR 0006 extension), which is
  a reason to wait for a concrete need.

## Consequences
- The multicart test ROM passes; no other ROM's behavior changes.
- Detection is a heuristic. If a multicart is found that it misses, add a
  second structural check (not a title or hash).
