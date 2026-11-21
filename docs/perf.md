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
