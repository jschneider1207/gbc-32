# 0037: QSPI NOR ROM image format

- Status: Accepted
- Date: 2026-11-25
- Builds on: ADR 0036 (firmware crate layout), plan `docs/plan/stage-2.md` M2

## Context
M2 stores the ROM in the board's QSPI NOR flash, read through an 8 MiB
memory-mapped window. The firmware must tell "no image", "interrupted or
corrupt load" and "good image" apart, and let the loader skip reprogramming
an image that is already there. The code is `gbc_firmware::rom_image`
(host-tested, `no_std`, no unsafe, no panics on any input).

## Decision
**Layout.** One 256-byte header at window offset 0; ROM data at offset 256
(one NOR page, so data programming is page-aligned). Maximum ROM length is
window length minus 256. The length check rejects only 0 and over-large
values; it does not check that the length is a valid Game Boy ROM size.

**Header byte map** (little-endian):
```
  0..8    magic b"GBC32ROM"
  8..10   format version u16 = 1
 10..12   flags u16 = 0 (not checked on read; reserved for later versions)
 12..16   ROM length u32
 16..24   FNV-1a 64 of the ROM data
 24..40   cartridge title, ROM 0x134..0x144 (informational; covered by the
          header checksum but never otherwise used)
 40..48   header checksum: FNV-1a 64 of bytes 0..40
 48..256  0xFF padding (not checked on read)
```

**Hash.** 64-bit FNV-1a, reusing `fnv.rs`. It protects against bad
programming and unwritten flash, not against an adversary, so collision
resistance is not a goal. It is a byte-at-a-time multiply loop: cheap on a
Cortex-M7 and no new code or dependency. The header has its own checksum so a
torn header is rejected without hashing the whole ROM.

**Write order and the erased rule.** The loader programs the ROM data first
and the header last. An interrupted load therefore leaves an erased
(all-0xFF) header, which is `ImageError::Erased` ("no image"), distinct from
`BadMagic`/`HeaderChecksum` (something was written and is wrong). An
all-0xFF header is checked before anything else. A corrupt header over
stale data cannot validate, because the checksum and data hash must both
match.

**API.** `encode_header(rom)`, `parse_header(header, window_len)` (no data
hash), `read_header(window)`, `validate(window) -> &[u8]` (header and data
hash), and `ImageHeader::matches(rom)` (length, then hash) for the "already
stored" test. `parse_header` takes the window length so the bound is
enforced there, not only by slicing in `validate`.

## Consequences
Changing the layout or hash needs a new version number; readers reject
other versions. A full `validate` hashes the whole ROM (up to 8 MiB), so the
board should run it once at boot, not per frame.
