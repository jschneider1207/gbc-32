//! Records the rustc flags this build was compiled with (`bench
//! --rustflags`), so `scripts/check-bench.sh` refuses to measure a build
//! that lost the flags of `.cargo/config.toml` (docs/perf.md "Build
//! flags"): a RUSTFLAGS or CARGO_ENCODED_RUSTFLAGS variable in the
//! environment replaces them silently.

fn main() {
    // The flags cargo passes to rustc for this package, separated by 0x1f.
    // All of a build's target units, gbc-core included, get the same ones.
    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS").unwrap_or_default();
    println!(
        "cargo:rustc-env=BENCH_RUSTFLAGS={}",
        flags.replace('\x1f', " ")
    );
    println!("cargo:rerun-if-changed=build.rs");
}
