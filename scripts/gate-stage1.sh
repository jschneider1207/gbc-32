#!/usr/bin/env bash
# Stage 1 acceptance gate. Exits 0 only if every condition in
# acceptance/README.md ("Gate") holds. Do not weaken this script without
# human approval.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

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

step "Instruction-count benchmark (valgrind)"
./scripts/check-bench.sh || fail "instruction-count benchmark (perf/bench-baseline.txt, docs/perf.md)"

step "Fuzz pass over untrusted inputs (fixed seed + corpus)"
./scripts/check-fuzz.sh || fail "fuzz pass (docs/fuzzing.md; failing input in target/fuzz-failures/)"

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
