# Fuzzing

Every untrusted input to `gbc-core` is fuzzed through the public API:
ROM bytes (header fields and the code the CPU executes), the save RAM buffer
(length and contents), RTC restore as a front end does it (raw, unmasked
field values from a save file through `RtcState::advance_seconds` with up
to `u64::MAX` elapsed seconds, then `set_rtc_state`; again mid-run from
`rtc_state()`), and button input. The property: no panic and no hang (a run call that stops
making progress trips an assertion) for any input.

## Targets

`tools/fuzz/src/lib.rs`. Each takes `&[u8]`: 64 control bytes (ROM length,
header bytes 0x143/0x147/0x148/0x149, save buffer length and fill, run
length, RTC state, stop events, audio buffer size, RTC seconds to
advance), then code placed at 0x150 of the ROM. Missing control bytes read as 0; buttons and run chunk
sizes cycle through the body. Each input loads, runs with button changes,
resets, loads a second cartridge over the first, ejects and reloads.

- `load`: header and buffer handling; short runs.
- `run`: long runs of arbitrary code.
- `rtc`: MBC3+timer cartridge types forced, arbitrary RTC state, longer runs.

## Gate pass (stable, deterministic)

```
scripts/check-fuzz.sh [--seed N] [--count N]
```

Runs every target over `tools/fuzz/corpus/<target>/` and then over `--count`
(default 5000) generated inputs per target from a fixed seed (SplitMix64;
same arguments give the same inputs). About 9 s. It builds with the
`checked` profile (`Cargo.toml`: release optimizations plus integer-overflow
checks and `debug_assert!`s), so an arithmetic slip that release would wrap
silently panics here; that costs about 4% over `--release` (8.2 s -> 8.5 s).
A panic prints the target and input id, writes the input to
`target/fuzz-failures/` and exits 1. To reproduce: copy the file into
`tools/fuzz/corpus/<target>/`, or run with `--corpus` pointing at a
directory holding it.

For a longer pass: `cargo run --profile checked -p fuzz --bin fuzz-pass --
--count 50000`. The profile works for any other workload too, e.g.
`cargo run --profile checked -p bench -- --fingerprint` or
`cargo test --profile checked -p gbc-core`.

The fuzz pass checks "no panic, no hang" on short runs of arbitrary input.
Its complements for known-good ROMs are in `docs/perf.md`, "Invariance
checks" (`scripts/check-sync-invariance.sh`): the fingerprint corpus under
the `checked` profile with every lazy component synced at every M-cycle
and with the clock started just below 2^31, 2^32, 2^33 and 2^40, and a
30-emulated-minute run (`--long`) past 2^32 clocks. Neither fuzz target
runs that long or starts the clock anywhere but 0.

## Coverage-guided (off the gate)

`tools/fuzz/fuzz/` is a cargo-fuzz crate outside the workspace. It needs
nightly and `cargo install cargo-fuzz` (both user-level):

```
cd tools/fuzz
cargo +nightly fuzz run load -- -max_len=4096 -timeout=10
```

Same for `run` and `rtc`. libFuzzer's working corpus lives in the
git-ignored `tools/fuzz/fuzz/corpus/`; seed it from the committed corpus
(`cargo +nightly fuzz run run corpus/run`).

## Corpus policy

`tools/fuzz/corpus/` holds generated inputs only (no ROM data), under 1 MB.
`fuzz-pass --emit-corpus tools/fuzz/corpus` recreates the hand-made edge
cases. Minimized cargo-fuzz findings go there once fixed (`cargo fuzz tmin`).
Inputs that crash the core and are not fixed yet go in
`tools/fuzz/known-failures/`, which the gate does not run.
