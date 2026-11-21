#!/usr/bin/env bash
# Deterministic fuzz pass over every untrusted input to gbc-core
# (docs/fuzzing.md): the committed corpus plus generated inputs from a fixed
# seed, no panic and no hang allowed. Extra arguments go to fuzz-pass
# (--seed N, --count N). On failure the input is saved in target/fuzz-failures/.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

cargo build --release -p fuzz --bin fuzz-pass
exec ./target/release/fuzz-pass "$@"
