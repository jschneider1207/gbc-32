# 0027: Byte-wide ROM reads

Status: Accepted (2026-11-22)
Status: Retired (2026-11-24, human decision; see the amendment "Retired" below)

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

## Amendments

### 2026-11-24: ROM memory is HyperRAM on OCTOSPI

Human decision, 2026-11-24: the Stage 3 ROM memory is HyperRAM on OCTOSPI,
not FMC SDRAM (hardware ADR 0001 has not been updated yet). The erratum
that motivated this ADR, ES0491 Rev 9 §2.7.3, applies to the FMC. With
HyperRAM on OCTOSPI: ES0491 Rev 9 §2.8.4 ("Odd address alignment and odd
byte number not supported at specific conditions", Table 5) allows
memory-mapped reads at any address and with any byte number in HyperBus
mode (and in octal-SPI RAM DTR mode with RDS); its alignment limits apply
to indirect reads and writes and to memory-mapped writes in other modes,
none of which the core's ROM reads are. No other OCTOSPI erratum on that
sheet restricts memory-mapped read alignment (§2.8.12 is about a
misaligned address written to `OCTOSPI_AR` before an indirect write). So
on the planned ROM memory, unaligned or merged ROM reads would be
supported. This ADR stays in force, and `scripts/check-rom-loads.sh` keeps
gating, until the human retires it.

### 2026-11-24: Retired

Human decision, 2026-11-24 ("Yes retire that ADR"): this ADR is retired.
Reason: the ROM will be in HyperRAM on OCTOSPI, not FMC SDRAM. ES0491
Rev 9 §2.7.3 is an FMC erratum ("Compile the software that accesses the
fmc region ..."); for OCTOSPI, §2.8.4 Table 5 lists memory-mapped reads
in HyperBus mode with address "any" and byte number "any" (both checked
against `../gbc-32-hardware/datasheets/es0491-stm32h72x-73x-errata.pdf`).
The core may read the ROM with loads of any width and alignment.

A platform that does put the ROM behind the FMC (the SDRAM option of
`docs/reference/stage3-rom-memory-options.md`) would need this rule
back; nothing in the core provides it any more.

What was undone, each step measured on its own (x86 Ir from
`check-bench.sh`, thumbv7em `.text` from `check-code-size.sh`; before:
113,154 B):

| Step | Ir | `.text` |
|---|---|---|
| VRAM DMA takes a ROM block as one `[u8; 16]` chunk; `rom_block`, `rom_tail`, `rom_byte` removed | `synth:worst-case` -0.03% (278,145,324 -> 278,059,754), others identical | -254 B |
| `Cart::dma_block` split into `rom_block` (inline) and `ram_block` | `synth:worst-case` -0.025% (-> 277,991,298), others identical | 0 |
| MBC1M logo test as two slice compares | identical (load time) | -32 B |
| `header` no longer `#[inline(never)]` | identical (load time) | -114 B |
| `rom::Rom`, its `compiler_fence`, `into_inner` and the marker comments removed; the ROM is `&[u8]` | identical | +72 B (`boot_logo` 264 -> 328: its loop is unrolled by two without the fence) |

Total: `.text` 112,826 B (-328); `synth:worst-case` -0.055%, the other
eight bench lines identical; `diff-fingerprint.sh main --audio-buffer 7
--hash-samples` identical.

Not taken:
- `read_rom` as `window.get(i).copied().unwrap_or(0xFF)`: +0.03% to
  +1.71% Ir on the nine lines (`synth:event-storm` +1.71%,
  `synth:cgb-raster` +1.12%, `synth:worst-case` +0.95%) and +52 B. It
  keeps the compare-then-index shape `Rom::get` had, as an early return.
- The VRAM DMA block written into its VRAM slot through an out pointer
  instead of returned as `[u8; 16]`: `synth:worst-case` +0.006%
  (+17,114 Ir) and +40 B.

`scripts/check-rom-loads.sh` no longer has anything to check (its marker
file `rom.rs` is gone, so it fails). The script and its step in
`scripts/gate-stage1.sh` are the human's to remove.
