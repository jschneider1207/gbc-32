# 0026: Byte-wide ROM reads

Status: Accepted (2026-11-22)

## Context

On the Stage 3 board the active ROM sits in FMC SDRAM
(`../gbc-32-hardware/docs/firmware-interface.md`). STM32H72x/73x erratum
ES0491 Rev 9, 2.7.3 "Unsupported read access with unaligned address"
(status P, partial workaround): "Read access with unaligned address, such
as a half-word read access starting at odd address, is not supported."
Workaround: compile code that accesses the FMC region "with a compiler
option that ensures data alignment, such as --no_unaligned_access".

Scope: reads only, and only unaligned ones. Aligned halfword and word
reads are fine, and writes are not affected. Cartridge RAM is in internal
SRAM (same document), so only the ROM is affected. (The frame buffer, if
a platform ever put it in SDRAM, is written, not read, by the core; see
the M4 note on `fill_tiles` in the plan.)

The Cortex-M7 allows unaligned `ldrh`/`ldr`, so LLVM freely merges byte
reads into them. Rust's equivalent of the workaround,
`-C target-feature=+strict-align`, is unstable on Rust 1.100 (it warns and
is slated to become an error, rust-lang/rust#162235). The core cannot
know where the platform put the ROM, so it cannot rely on alignment.

Before this change the thumbv7em build (release, cortex-m7) had three
wide ROM reads:
- `Cart::dma_block` (VRAM DMA from ROM) copied the 16-byte block with
  four `ldr` (`ldr r0, [r1, r2]`, `ldr r3, [r0, #4/#8/#12]`);
- `Registers::post_boot` compared header 0144-0145 with `b"01"` as one
  `ldrh.w r1, [r2, #324]`;
- `is_mbc1m` compared the two 48-byte logos with `bl memcmp`, whose
  load width is the platform library's choice.
The CPU's reads (`Cart::read_rom`, about 120 inlined sites) were all
`ldrb`: each byte picks its bank window and is bounds-checked separately.

## Decision

1. The ROM is a crate-private newtype, `rom::Rom<'a>`, with a private
   field. Its only read is `Rom::get(i) -> Option<u8>`: a byte index and
   then `compiler_fence(SeqCst)`. The fence emits no instruction but LLVM
   doesn't merge or move memory accesses across it, so adjacent reads
   stay separate byte loads even where the bounds checks fold away.
   `into_inner` exists only to hand the slice back to the caller (eject,
   load errors) or re-wrap it; each use carries a marker comment.
   All header parsing, the MBC1M logo compare, VRAM DMA blocks and the
   CPU bus read go through `Rom::get`.
2. `scripts/check-rom-loads.sh` emits gbc-core's thumbv7em assembly and
   checks every load whose debug line (`.loc`, the innermost source line
   after inlining) is the marked load line in `rom.rs`: all must be
   `ldrb`/`ldrsb`, and there must be at least one. It also fails on an
   unmarked `into_inner()`. It doesn't use symbol names. It runs in
   `gate-stage1.sh`, after the code-size check.

Mutation checks of the script (scratch worktree): removing the fence
alone changed nothing (the per-byte bounds checks still kept the loads
apart). Removing it with a bounds-hoisted VRAM DMA block made LLVM emit
four `ldr` at `rom.rs:62`, and the script failed ("4 of 249 ROM loads are
wider than a byte"). The same hoisted block with the fence stayed `ldrb`.
An unmarked `into_inner()` read and a removed marker also fail.

## Cost

- x86 Ir (`check-bench.sh`, vs the parent commit): `synth:worst-case`
  467,219,398 -> 467,243,766 (+0.005%); `synth:dma-storm` +0.48% (16
  byte loads replace one 16-byte copy); the rest within +-0.7%.
- thumbv7em `.text`: 94,770 -> 95,346 B (+576 of the 96,000 budget):
  `dma_block` +496 (16 unrolled checked byte loads), `Emulator::load`
  +200 (the byte-wise logo compare), `cart_ram_size` +90 (now out of
  line), `vram_dma` -140. A hoisted bounds check in `dma_block` made the
  block smaller but needs a separate tail path for a short last bank, and
  that was larger in total, so it wasn't kept.
- No behavior change: `diff-fingerprint.sh` identical over 3822 ROMs.

## Alternatives rejected

- `+strict-align`: unstable, and it would apply to the whole core.
- `core::hint::black_box` on the index: spills the index to the stack
  and back on every read (`str`, empty asm, `ldr` on thumbv7em).
- Volatile or atomic reads: need `unsafe` (ADR 0009).
- Word reads when the ROM base is aligned: matches the erratum exactly,
  but adds a runtime branch and a check exemption for a cold path.
