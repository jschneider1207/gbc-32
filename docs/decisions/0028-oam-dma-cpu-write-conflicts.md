# 0028: CPU writes during a conflicting OAM DMA follow gambatte's captures

Status: Accepted (2026-11-23)

## Context

While an OAM DMA transfer runs, a CPU access on the bus the DMA reads
conflicts with it (`docs/reference/oam-dma.md`, "CPU accesses during a
transfer"; `dma::conflict`). The two references disagree about what a
conflicting CPU *write* does on CGB C (ADR 0003):

- **SameBoy** (`GB_write_memory`, model checks): the write is dropped and,
  if the source is below A000, the OAM byte just transferred is cleared to
  00. We followed this until now.
- **gambatte** cgb04c results, which are captures from CGB C hardware: the
  CPU's data lands in that OAM byte. 58 ROMs pin it
  (`oamdma_src{0000,7F00,A000,BF00,E000,EF00,F000,FE00,FF00}_busy{push,call,rst}*`,
  `srcA000_busywrite4000`; M9 triage group 2), e.g.
  `oamdma_src0000_busypush0001` expects OAM byte 9D = 55 (the pushed H),
  where SameBoy's rule gives 00. Every pop/read variant (350 ROMs) passes
  under both rules, so nothing in the manifest pins SameBoy's version.

**Human decision (session 10, 2026-11-23):** follow gambatte's CGB C
captures ("gambatte is fine").

## Decision

For a CPU write below FE00 that conflicts with the transfer (`Conflict::Dma`
or `Conflict::Open`) in the M-cycle of byte `i`, on CGB C hardware in both
CGB and DMG-compatibility mode:

| Source page | CPU bus that conflicts | Target | OAM byte `i` |
|---|---|---|---|
| ROM 00-7F, SRAM A0-BF | external | unchanged | CPU data |
| VRAM 80-9F | VRAM | unchanged | 00 |
| WRAM C0-DF | WRAM | unchanged | unchanged (DMA byte) |
| E0-FF | external | unchanged | CPU data |

WRAM accesses with a non-WRAM source stay redirected (`Conflict::Wram`),
and OAM/HRAM/I/O are unchanged. Behavior learned from gambatte's
`Memory::nontrivial_write` (GPL; read for behavior only, no code copied),
which reproduces the captures. Reads are unchanged.

Off the hot path: only `Ctx::write_during_dma` (out of line, DMA active)
changed.

## Consequences

- The 58 group-2 ROMs pass; no pass/fail change elsewhere.
- Deviation from SameBoy, recorded in `docs/reference/oam-dma.md`.
- Not modelled: gambatte also clears OAM byte `i` on a conflicting CPU
  *read* with a VRAM source (group 13, 6 `src{8000,9F00}_busypop*` ROMs).
  Our read path is side-effect free by design (`bus::read` takes shared
  references); a read side effect needs either a branch in the inlined
  `Ctx::read` (code size, per-function budgets) or interior mutability,
  which is a separate design task.
