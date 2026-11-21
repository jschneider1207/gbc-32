#!/usr/bin/env bash
# Deterministic fuzz pass over every untrusted input to gbc-core
# (docs/fuzzing.md): the committed corpus plus generated inputs from a fixed
# seed, no panic and no hang allowed. Extra arguments go to fuzz-pass
# (--seed N, --count N). On failure the input is saved in target/fuzz-failures/.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Run the binary cargo just built, wherever CARGO_TARGET_DIR or a cargo config
# put it: a hard-coded target/release/fuzz-pass may be stale.
bin=$(cargo build --release -p fuzz --bin fuzz-pass --message-format=json-render-diagnostics \
  | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)
[ -n "$bin" ] && [ -x "$bin" ] || { echo "check-fuzz: cannot find the fuzz-pass binary cargo built" >&2; exit 2; }
exec "$bin" "$@"
