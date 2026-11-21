# Performance

## Code-size budgets

Hot CPU/bus functions in `gbc-core` have code-size budgets so the MCU build
does not silently grow (flash and I-cache pressure on the Cortex-M7).
Budgets are in `perf/code-size-budgets.txt` (`<bytes> <exact demangled symbol>`).

Run: `scripts/check-code-size.sh` (about 0.3 s warm, a few seconds cold).
Exit is non-zero if a symbol is over budget, is not found (renamed or
inlined away: update the budget file, never drop the line to pass), or the
build fails. It prints symbol, size, budget and % used.

Measurement: `RUSTFLAGS="-C target-cpu=cortex-m7" cargo build -p gbc-core
--release --target thumbv7em-none-eabihf --target-dir target/codesize`, then
`nm -S -C --radix=d` (GNU binutils; `llvm-nm`/`rust-nm` preferred if present)
on each object in the rlib. Sizes of same-named symbols (CGU copies) are
summed. Sizes as of ADR 0011 step 4, unchanged by step 5 (lazy timer: the tick is only the
countdown, so `run` dropped from 36,416 B and `execute_cb` from 15,254 B):

| Symbol | Size (B) | Budget (B) |
|---|---|---|
| `Cpu::run::<Ctx>` | 28,560 | 32,500 |
| `Cpu::execute_cb::<Ctx>` | 13,158 | 14,600 |
| `Ctx::write_high` | 160 | 170 |
| `Ctx::write_io` | 1,180 | 1,240 |
| `bus::read_high` | 1,008 | 1,160 |
| `Ctx::sync_ppu` | 496 | 520 |
| `Ctx::write_mapped` | 428 | 440 |
| `Ctx::write_timer_io` (cold) | 1,538 | 1,620 |
| `Timer::read` (cold) | 362 | 380 |

OAM DMA (M2): every CPU access ORs `Hot::dma` (FE00 during a transfer)
into its region dispatch, so a transfer sends every address to the
E000-FFFF arm, the only one that tests the flag (`read_high`, and the last
arms of the out-of-line `write_mapped`). Echo RAM moved out of the inlined
read into `read_high`. `write_high`, `write_io` and `sync_ppu` stay out of
line. The OR is +1 instruction per access (x86 `movzwl`+`or` replacing
`test`); the ROM windows became `[&[u8]; 2]` indexed by address bit 14,
which removed a window select LLVM computed twice per ROM read. Measured
layouts (check-bench Ir vs the pre-DMA baseline; `run` on thumbv7em):

| Layout | cpu_instrs | cgb_sound | cgb-acid2 | `run` (B) |
|---|---|---|---|---|
| 7bdaee2: read OR + flag test first in `#[inline(never)]` `write_mapped` | +2.94% | +4.31% | +3.05% | 32,560 |
| same, `write_mapped` `#[inline]` | -2.70% | +2.91% | +1.70% | 40,862 |
| OR in reads and writes, echo out of line (u16 key) | +2.03% | +3.74% | +3.18% | 29,928 |
| same, `usize` key | | +5.50% | | 29,536 |
| same (u16) + ROM windows by bit 14 (chosen) | +0.40% | -2.45% | -1.01% | 28,560 |

Earlier 7bdaee2 measurements: a flag test with an out-of-line call per
read and write was 32,928 B (34,504 with `#[cold]`), an extra match bit
50,376 B. With the chosen layout `run` is back under the pre-DMA
30,920 B and its 32,500 B budget is restored.

Rule: raising a budget is committed with the change that needs it, with the
measured size and the reason in the commit body. Initial budgets are the
measured size plus about 5%.

## Instruction-count benchmark

`scripts/check-bench.sh` runs `tools/bench` (release build) under
`valgrind --tool=callgrind` and compares the instructions executed (Ir) per
ROM with `perf/bench-baseline.txt`. It exits non-zero if any ROM is more than
3% above its baseline; improvements are printed but pass. Ir is deterministic
(two runs gave identical counts), so the threshold covers real code change,
not noise.

What it measures: only the emulation loop. `bench` calls `run_emulation`
(`#[inline(never)]`) and callgrind is told `--toggle-collect='*run_emulation*'`,
so process startup, ROM loading and setup are not counted. The ROMs are
hash-checked against `acceptance/test-roms.sha256` first.

ROMs, each run for 20 emulated seconds (83,886,080 clocks): `blargg/cpu_instrs/cpu_instrs.gb`
(CPU), `blargg/cgb_sound/cgb_sound.gb` (APU), `cgb-acid2/cgb-acid2.gbc` (PPU).

Current numbers (Ir) and wall time (valgrind run of all three, ~22-24 s):

| ROM | Ir |
|---|---|
| cpu_instrs.gb | 1,382,484,838 |
| cgb_sound.gb | 622,018,571 |
| cgb-acid2.gbc | 13,094,335 |

(ADR 0011 step 4, the lazy timer, took these from 1,921.3 M, 922.7 M and
928.5 M: -28.0%, -32.6% and -24.9%. ADR 0011 step 5, HALT/STOP idling
straight to the next deadline, took cgb-acid2 from 697.2 M to 13.1 M
(-98.1%: it spends most of its time halted waiting for VBlank);
cpu_instrs -0.02%, cgb_sound unchanged.)

Run: `scripts/check-bench.sh`. Update the baseline:
`scripts/check-bench.sh --update-baseline`.

Rule: a baseline change is committed together with the change that caused it,
with the measured delta per ROM and the reason in the commit body. Changing the
ROM list or durations also needs a baseline update in the same commit.

## Behavior fingerprint

`bench --fingerprint [--seconds N] [--root DIR]` (N defaults to 3, root to
`test-roms`) prints one line per ROM, `<16-hex FNV-1a hash> <path>`, sorted by
path, so two builds can be compared bit for bit (ADR 0011 "Verification").
Corpus: every `.gb`/`.gbc` under Mooneye `acceptance`, `misc` and
`emulator-only`, `blargg`, `age-test-roms`, `same-suite` and `cgb-acid2`;
a ROM that fails to load prints `LOAD-ERROR <path>`. Each ROM runs on the
only supported model (CGB C, default palettes) in 456-clock `run_until`
chunks with `LD_B_B` and `SERIAL_TX` stops on. After each chunk the hash folds
the clock, the chunk's clocks and events, the CPU registers, the serial byte
(on `SERIAL_TX`) and, via `peek`, SB SC DIV TIMA TMA TAC IF LY; at the end it
folds WRAM `C000-DFFF` (the mapped bank only, since `peek` cannot switch
banks). ROMs run in parallel; output order does not depend on it.

`scripts/diff-fingerprint.sh <rev> [args]` builds `<rev>` in a temporary
worktree and the working tree, runs both fingerprints with the same args,
lists the differing ROMs and exits 0 only if none differ. Use it on commits
that restructure core timing, which must not change behavior.
