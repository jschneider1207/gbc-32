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

step "cargo fmt --check"
cargo fmt --all --check || fail "formatting"

step "cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings || fail "clippy"

step "cargo test"
cargo test --workspace || fail "tests"

step "gbc-core: no alloc, builds for thumbv7em-none-eabihf (no std there)"
if grep -rnE 'extern crate alloc' crates/gbc-core/src; then
    fail "gbc-core must not use alloc"
fi
cargo build -p gbc-core --release --target thumbv7em-none-eabihf || fail "gbc-core embedded build"

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
