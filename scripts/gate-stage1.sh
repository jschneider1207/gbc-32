#!/usr/bin/env bash
# Stage 1 acceptance gate. Exits 0 only if every condition in
# acceptance/README.md ("Gate") holds. Do not weaken this script without
# human approval.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

# The check scripts take these overrides for experiments (mutation checks
# against a scratch budget or ITCM file, a worktree reading another tree's
# ROMs). The gate judges against the committed files and the ROMs it
# fetches into test-roms/ below (pinned zip hash; the desktop step and
# romtest read test-roms/ too), so an exported override must not reach it.
unset CODE_SIZE_BUDGETS_FILE ITCM_SET_FILE TEST_ROMS
# The rustc-flag variables are refused, not unset: each replaces the flags
# of .cargo/config.toml (its `[build] rustflags`, ADR 0008's LLVM option,
# which every gate build relies on) and can turn on a cfg such as
# feature="eager-sync" that cargo does not report, so a gate run with one
# set judges another build. (The scripts that need other flags set their
# own, scripts/lib/rustflags.sh.) Set but empty counts as set.
for v in RUSTFLAGS CARGO_ENCODED_RUSTFLAGS CARGO_BUILD_RUSTFLAGS $(compgen -e); do
  case $v in
    RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_BUILD_RUSTFLAGS | CARGO_TARGET_*_RUSTFLAGS)
      if [ -n "${!v+x}" ]; then
        printf '\nGATE FAILED: %s is set (%q); it replaces the rustc flags of .cargo/config.toml. Unset it and rerun.\n' "$v" "${!v}" >&2
        exit 1
      fi
      ;;
  esac
done

step() { printf '\n==> %s\n' "$*"; }
fail() { printf '\nGATE FAILED: %s\n' "$*" >&2; exit 1; }

command -v cargo >/dev/null || fail "cargo not found (Rust toolchain not installed)"
[[ -f Cargo.toml ]] || fail "no Cargo workspace yet (milestone M0 not done)"

step "Test ROMs"
./scripts/fetch-test-roms.sh

step "SM83 single-step test data"
./scripts/fetch-sm83-tests.sh || fail "could not fetch the SM83 single-step test data"
# Without this, tools/sm83-tests skips (and passes) when test-data/ is
# missing, so a clean checkout would pass the gate with zero CPU cases run.
export GBC_REQUIRE_TEST_DATA=1

step "cargo fmt --check"
cargo fmt --all --check || fail "formatting"

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings || fail "clippy"

step "cargo doc (gbc-core, private items, warnings denied)"
# The core's comments cite hardware behaviour and each other through
# intra-doc links; a broken or ambiguous one is a rustdoc warning.
# CARGO_ENCODED_RUSTDOCFLAGS would replace RUSTDOCFLAGS (and drop -D warnings).
env -u CARGO_ENCODED_RUSTDOCFLAGS RUSTDOCFLAGS="-D warnings" \
    cargo doc -p gbc-core --no-deps --document-private-items || fail "cargo doc (gbc-core)"

step "cargo test"
cargo test --workspace || fail "tests"

step "gbc-core: no alloc, builds for thumbv7em-none-eabihf (no std there)"
# alloc is forbidden everywhere in the core, test code included. -z treats
# each file as one record so a declaration split across lines still matches;
# \s+ tolerates any whitespace. Deliberately strict: a comment spelling out
# `alloc::` also fails (rephrase it).
if grep -rlPz 'extern\s+crate\s+alloc\b|\buse\s+alloc\b|\balloc::' crates/gbc-core/src; then
    fail "gbc-core must not use alloc"
fi
# The detailed rules (std only inside #[cfg(test)] modules, no #[path] or
# include!, #![no_std] present, no dependencies of any kind) are enforced by
# this test. Naming it explicitly makes the gate fail if it is deleted or
# renamed, not just if it fails.
cargo test -p gbc-core --test hard_rules || fail "gbc-core hard rules (crates/gbc-core/tests/hard_rules.rs)"
cargo build -p gbc-core --release --target thumbv7em-none-eabihf || fail "gbc-core embedded build"

step "Code-size budgets (thumbv7em, cortex-m7)"
./scripts/check-code-size.sh || fail "code-size budgets (perf/code-size-budgets.txt, docs/perf.md)"

step "ITCM set fits (perf/itcm-sections.txt, thumbv7em)"
./scripts/check-itcm-set.sh || fail "ITCM set (perf/itcm-sections.txt, docs/perf.md ITCM placement plan)"

step "Instruction-count benchmark (valgrind)"
./scripts/check-bench.sh || fail "instruction-count benchmark (perf/bench-baseline.txt, docs/perf.md)"

step "Fuzz pass over untrusted inputs (fixed seed + corpus)"
./scripts/check-fuzz.sh || fail "fuzz pass (docs/fuzzing.md; failing input in target/fuzz-failures/)"

step "Invariance checks: sync placement, clock offset, 30-minute run"
./scripts/check-sync-invariance.sh --long || fail "invariance checks (docs/perf.md \"Invariance checks\")"

step "Desktop front end: release build + headless run (M7)"
# Run the binary cargo just built, wherever CARGO_TARGET_DIR or a cargo
# config put it: a hard-coded target/release path may be stale.
desktop=$(cargo build -p gbc-desktop --release --bin gbc-desktop --message-format=json-render-diagnostics \
    | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1) || fail "gbc-desktop build"
[ -n "${desktop}" ] && [ -x "${desktop}" ] || fail "cannot find the gbc-desktop binary cargo built"
"${desktop}" test-roms/blargg/cpu_instrs/cpu_instrs.gb --headless --frames 600 \
    || fail "gbc-desktop --headless --frames 600 did not exit 0"

step "Acceptance ROMs (must tier + regressions)"
set +e
cargo run -p romtest --release -- --manifest acceptance/stage1.toml --gate must
rc=$?
set -e
case ${rc} in
    0) ;;
    1) fail "must-tier ROM failures or regressions (see target/acceptance/stage1.md)" ;;
    *) fail "romtest runner error (exit ${rc}); results are not meaningful" ;;
esac

printf '\nGATE PASSED: stage 1\n'
