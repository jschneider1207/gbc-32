#!/usr/bin/env bash
# Fail if a budgeted gbc-core function, or all of its code ("(total .text)"
# in the budgets file), exceeds its code-size budget on the MCU build
# (thumbv7em-none-eabihf, release, cortex-m7). See docs/perf.md.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

BUDGETS=perf/code-size-budgets.txt
TARGET=thumbv7em-none-eabihf
TDIR=target/codesize

NM=$(command -v llvm-nm || command -v rust-nm || command -v nm) || { echo "no nm found" >&2; exit 2; }
SIZE=$(command -v llvm-size || command -v rust-size || command -v size) || { echo "no size found" >&2; exit 2; }

if ! RUSTFLAGS="-C target-cpu=cortex-m7" cargo build -p gbc-core --release \
    --target "$TARGET" --target-dir "$TDIR" >&2; then
  echo "FAIL: gbc-core build for $TARGET failed" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
rlib=$(ls "$TDIR/$TARGET/release/"libgbc_core.rlib)
(cd "$work" && ar x "$OLDPWD/$rlib")
: > "$work/syms"
for o in "$work"/*.o; do
  "$NM" -S -C --radix=d "$o" | awk 'NF>=4 {
    name=$4; for (i=5;i<=NF;i++) name=name " " $i
    print ($2+0) "\t" name }' >> "$work/syms"
done
[ -s "$work/syms" ] || { echo "FAIL: no symbols read from $rlib" >&2; exit 1; }
# Total code: every .text* section of every object in the rlib (all of
# gbc-core, before the final link drops what a front end does not use).
total_text=$("$SIZE" -A "$work"/*.o | awk '$1 ~ /^\.text/ {s += $2} END {print s + 0}')
[ "$total_text" -gt 0 ] || { echo "FAIL: no .text sections read from $rlib" >&2; exit 1; }

fail=0
printf '%-62s %8s %8s %6s\n' symbol size budget used
while read -r budget name; do
  case "$budget" in ''|'#'*) continue;; esac
  if [ "$name" = "(total .text)" ]; then
    size=$total_text
  else
    size=$(awk -F'\t' -v n="$name" '$2==n {s+=$1; f=1} END {if (f) print s}' "$work/syms")
  fi
  if [ -z "$size" ]; then
    printf '%-62s %8s %8s %6s\n' "$name" MISSING "$budget" -
    echo "  ^ symbol not found (renamed or inlined away?): update $BUDGETS" >&2
    fail=1
    continue
  fi
  pct=$(( size * 100 / budget ))
  status=""
  if [ "$size" -gt "$budget" ]; then status="  OVER BUDGET"; fail=1; fi
  printf '%-62s %8d %8d %5d%%%s\n' "$name" "$size" "$budget" "$pct" "$status"
done < "$BUDGETS"

if [ "$fail" -ne 0 ]; then
  echo "FAIL: code-size check (see docs/perf.md, 'Code-size budgets')" >&2
  exit 1
fi
echo "OK: all budgeted symbols within budget"
