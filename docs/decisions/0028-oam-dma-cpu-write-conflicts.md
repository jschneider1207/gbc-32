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

## Amendment (2026-11-23): conflicting reads with a VRAM source

**Rule.** On CGB C, a CPU read that conflicts with a transfer from VRAM
(`Conflict::DmaVram`: the CPU reads the VRAM bus in the M-cycle of byte
`i`) returns the byte the DMA reads, as before, and also clears OAM byte
`i` to 00. Source: gambatte cgb04c captures
`oamdma_src{8000,9F00}_busypop{7FFF,9FFF}*` (6 ROMs; e.g.
`src8000_busypop7FFF`: `POP DE` reads 7FFF (free) then 8000 in the M-cycle
of byte 9E, and OAM 9E reads back 00, CGB `65005576` vs DMG `65765576`).
gambatte `Memory::nontrivial_read` reproduces them (read for behavior
only, no code copied). Same deviation from SameBoy as the write rule.

**Design.** The read path stays on shared references: `read_high` is
shared by the CPU and `Emulator::peek`, and every way of giving the CPU a
`&mut` path without it cost measurably (a `&mut` `read_high` that
tail-calls a shared body: +2.1% Ir on `cpu_instrs`, +1.4% `cgb-raster`;
duplicating the body for `peek`: +1.0% `cpu_instrs` and ~+800 B). So the
read only *records* the clear, in `Memory::dma_read_clears`
(`dma::ReadClears`, 160 bits): interior mutability through `AtomicU32`
with relaxed loads and stores (plain `ldr`/`str` on the MCU), not `Cell`,
so `Memory` stays `Sync` and can still be a `static`. `Ctx::dma_sync`
applies the record after copying the bytes due (each recorded byte was
due by its read, so the lazy copy can't overwrite the 00); it runs before
anything observes OAM (a CPU write, a restart, the `Dma` deadline, a PPU
sync, the end of a run call), so the record is empty between run calls.
It lives in `Memory`, not `Sys`, because `Hw` would pass its 864 B limit.

`dma::conflict` returns `DmaVram`, tested on the CPU's bus rather than
the source's, so the source bus is dead by then: the first version kept
it live and cost one saved register on every read during a transfer
(+4% Ir on `synth:dma-storm`). The record is three inlined instructions in
`read_during_dma`; the apply is out of line and cold.

**Measured** (`scripts/check-bench.sh`, vs main): `cpu_instrs` 0.00%,
`cgb-acid2` -0.22%, `synth:worst-case` -0.10%, `synth:dma-storm` -3.5%,
the rest within ±0.6%. gbc-core `.text` +416 B (94,552 -> 94,968);
`read_during_dma` 522 -> 586 B (budget 530 -> 615). ITCM set unchanged.

**Consequence.** The 6 ROMs pass. `oamdma_src8000_srcchange0000_busyinc`
(group 13) is not this rule: it is a restart (FF46 rewritten during a
VRAM-source transfer) whose first start-delay M-cycle already conflicts
on the *new* source's bus (gambatte: the old transfer keeps running
until the new one starts, with the new source's conflict area and
pointer). Since modelled (2026-11-23, `OamDma::tail`;
`docs/reference/oam-dma.md`, "Restart").
