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
summed. Measured numbers reproduce ADR 0008/0009 (`run` 37,064 B,
`execute_cb` 15,702 B).

| Symbol | Size (B) | Budget (B) |
|---|---|---|
| `Cpu::run::<Ctx>` | 37,064 | 39,000 |
| `Cpu::execute_cb::<Ctx>` | 15,702 | 16,500 |
| `Ctx::write_high` | 3,080 | 3,300 |
| `bus::read_high` | 1,080 | 1,200 |
| `Ctx::sync_ppu` | 482 | 520 |
| `Ctx::write_mapped` | 396 | 420 |

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
| cpu_instrs.gb | 1,926,303,053 |
| cgb_sound.gb | 922,727,228 |
| cgb-acid2.gbc | 928,451,269 |

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
