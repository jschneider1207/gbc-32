# Fuzzing

Every untrusted input to `gbc-core` is fuzzed through the public API:
ROM bytes (header fields and the code the CPU executes), the save RAM buffer
(length and contents), `set_rtc_state` with arbitrary field values, and
button input. The property: no panic and no hang (a run call that stops
making progress trips an assertion) for any input.

## Targets

`tools/fuzz/src/lib.rs`. Each takes `&[u8]`: 64 control bytes (ROM length,
header bytes 0x143/0x147/0x148/0x149, save buffer length and fill, run
length, RTC state, stop events, audio buffer size), then code placed at
0x150 of the ROM. Missing control bytes read as 0; buttons and run chunk
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
same arguments give the same inputs). About 8 s in release. A panic prints
the target and input id, writes the input to `target/fuzz-failures/` and
exits 1. To reproduce: copy the file into `tools/fuzz/corpus/<target>/`, or
run with `--corpus` pointing at a directory holding it.

Release builds have no overflow checks; for a deeper pass that has them run
the debug binary: `cargo run -p fuzz --bin fuzz-pass -- --count 1500`
(about 15 s).

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
