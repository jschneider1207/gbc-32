# 0038: The board side of the ROM loader

- Status: Accepted
- Date: 2026-11-25
- Builds on: ADR 0036 (crate layout), ADR 0037 (image format), plan
  `docs/plan/stage-2.md` M2 "Loader"

## Context
The `loader` image receives a ROM from `tools/gbc-pc` over the ST-LINK VCP
(`gbc-link` v1, stop-and-wait, 2 s PC timeout, 1,024-byte chunks) and
stores it in the NOR. Three things needed deciding: where the session
logic lives so that it can be tested off the board, how to erase 8 MiB of
NOR without any reply taking near 2 s, and how the UART rate is chosen
when v1 has no baud-switch message.

## Decision
**Session logic in `gbc-nucleo-h753/src/loader.rs`, host-tested from
`tools/gbc-pc/tests/board.rs` by `#[path]`.** The module uses only `core`,
`gbc_link` and `gbc_firmware::rom_image`, and reaches the NOR through a
five-method trait (`size`, `erase_4k`, `erase_64k`, `program`,
`with_window`). `gbc-firmware` may not depend on `gbc-link` (ADR 0036, D9)
and the nucleo crate builds only for thumbv7em, so no crate can host its
unit tests; a new crate would change the human-approved layout. gbc-pc's
test compiles the file with `gbc-firmware` as a dev-dependency and runs the
real `Client` against it over a fake NOR that, like the real one, can only
clear bits. The `loader` bin keeps only the UART and QUADSPI glue.

**Lazy erase.** `RomBegin` (when the stored image is not already this one)
erases the 64 KiB block at offset 0 at once: it holds the header, so from
that reply on the NOR holds no image, and an interrupted load reads as
`Erased` (ADR 0037). Each further block is erased when the first chunk
reaching it arrives, as a 64 KiB erase while a whole block fits below the
image's end rounded up to 4 KiB, else as 4 KiB sectors. So one reply costs
at most one 64 KiB erase (0.31-0.34 s measured, 1 s datasheet max) plus four
page programs. No chip erase (one reply would wait for the whole chip, far
over the timeout; the driver refuses it anyway),
and no status or configuration register writes.

**Verify through the window.** `RomEnd` hashes the data as read through the
memory-mapped window, by building the header from it
(`rom_image::encode_header`), and programs that header only when its hash
equals the `RomBegin` hash; then `rom_image::validate` over the window must
pass, else `Nak FlashError`. `RomDone` always carries the read-back hash.

**Resends.** A frame byte-identical (on the wire) to the last answered
request is answered from the stored reply. `Hello` is always handled anew.

**Baud rate: a build-time constant** (`GBC_LOADER_BAUD`, default in
`src/bin/loader.rs`), chosen from measurements (`docs/reference/loader.md`).
The banner goes out at 115200 at reset, then the loader switches. The PC is
told the same rate with `gbc-pc --baud`. A protocol-level switch would need
gbc-link v2.

## Consequences
- The board logic and the simulated board in `gbc_pc::sim` are both tested
  against the same client, but they are separate code; a behavior change
  must be made in both.
- The `#[path]` include means the module must stay free of `crate::` paths
  and board types.
- A load of an image that is not already stored always starts by
  destroying the stored one, even if the new load then fails.
- Changing the loader's rate means rebuilding the image and telling
  `gbc-pc` (its `--baud` default is the loader's, 4,000,000) and
  `read-serial.sh` (`H7_BAUD`) the new rate.
