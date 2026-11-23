#!/usr/bin/env bash
# Fail if the MCU build (thumbv7em-none-eabihf, release, cortex-m7) reads
# ROM bytes with anything wider than a byte load (ADR 0026).
#
# The STM32H735 does not support unaligned reads from the FMC SDRAM that
# holds the ROM (ES0491 2.7.3). Every ROM read in gbc-core is `Rom::get`
# in crates/gbc-core/src/rom.rs, whose load line carries the marker
# comment below. This script emits gbc-core's assembly, follows the debug
# line directives (`.loc`, which name the innermost source line even after
# inlining), and checks every load attributed to the marker line:
#   - each must be a byte load (ldrb/ldrsb, any condition or .w suffix);
#   - there must be at least one (so a moved marker, a renamed file or
#     lost debug info fails instead of passing vacuously).
# Symbol names are not used, so renames and inlining don't matter.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

TARGET=thumbv7em-none-eabihf
TDIR=target/romloads
SRC=crates/gbc-core/src/rom.rs
MARK='check-rom-loads: the ROM load'

lines=$(grep -nF "$MARK" "$SRC" | cut -d: -f1 || true)
if [ "$(printf '%s\n' "$lines" | grep -c .)" -ne 1 ]; then
  echo "FAIL: expected exactly one '$MARK' marker in $SRC" >&2
  exit 1
fi

# The slice escapes `Rom` only through `into_inner`, to hand it back or
# re-wrap it; every use must be marked as reviewed.
unmarked=$(grep -rn "\.into_inner()" crates/gbc-core/src --include='*.rs' \
  | grep -v "^$SRC:" | grep -vF "check-rom-loads: allowed" || true)
if [ -n "$unmarked" ]; then
  printf '%s\n' "$unmarked" >&2
  echo "FAIL: Rom::into_inner use without a 'check-rom-loads: allowed' marker (ADR 0026)" >&2
  exit 1
fi

out="$TDIR/$TARGET/release/build/gbc-core"
rm -rf "$out"
# `cargo rustc` passes --emit only to gbc-core itself; debug line tables
# come from the release profile (debug = "line-tables-only"), forced here
# so a profile change can't silently remove them.
if ! RUSTFLAGS="-C target-cpu=cortex-m7" cargo rustc -p gbc-core --release \
    --target "$TARGET" --target-dir "$TDIR" -- \
    --emit=asm -C debuginfo=line-tables-only >&2; then
  echo "FAIL: gbc-core build for $TARGET failed" >&2
  exit 1
fi
mapfile -t asm < <(find "$out" -name '*.s')
[ "${#asm[@]}" -gt 0 ] || { echo "FAIL: no assembly emitted under $out" >&2; exit 1; }

awk -v line="$lines" -v src="$SRC" '
  FNR == 1 { delete isrom }
  # .file N "dir" "name"  or  .file N "path"
  $1 == ".file" {
    n = $2; path = $0
    sub(/^[ \t]*\.file[ \t]+[0-9]+[ \t]+/, "", path)
    gsub(/"[ \t]+"/, "/", path); gsub(/"/, "", path)
    if (path ~ ("(^|/)" src "$")) { isrom[n] = 1; seen = 1 }
    next
  }
  /^[_A-Za-z$][^ \t:]*:/ { fn = $1 }
  $1 == ".loc" { cur = (($2 in isrom) && $3 == line); next }
  cur && ($1 ~ /^v?ld/) {
    total++
    funcs[fn] = 1
    if ($1 !~ /^ldrs?b(eq|ne|cs|hs|cc|lo|mi|pl|vs|vc|hi|ls|ge|lt|gt|le|al)?(\.w)?$/) {
      bad++
      printf "WIDE LOAD at %s:%d: %s (in %s)\n", src, line, $0, fn
    }
  }
  END {
    nf = 0; for (f in funcs) nf++
    if (!seen) { print "FAIL: " src " not in the debug line tables" > "/dev/stderr"; exit 1 }
    if (total == 0) { print "FAIL: no loads attributed to " src ":" line " (marker moved, or Rom::get no longer used?)" > "/dev/stderr"; exit 1 }
    if (bad > 0) { printf "FAIL: %d of %d ROM loads are wider than a byte (ADR 0026)\n", bad, total > "/dev/stderr"; exit 1 }
    printf "OK: %d ROM loads in %d functions, all byte-wide\n", total, nf
  }
' "${asm[@]}"
