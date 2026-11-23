#!/usr/bin/env bash
# Pre-link check of the ITCM set (perf/itcm-sections.txt; docs/perf.md "ITCM
# placement plan (M8)", ADR 0026) on the gbc-core rlib for thumbv7em: every
# glob must match at least one function section (a rename or inlining shows up
# as MISSING), and the summed size of all matched sections must fit
# ITCM_BUDGET_BYTES. A section matched by two globs is counted once.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

SETFILE=${ITCM_SET_FILE:-perf/itcm-sections.txt}
TARGET=thumbv7em-none-eabihf
TDIR=target/codesize

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
# One line per function section: "<object>|<section name>|<size>".
for o in "$work"/*.o; do
  "$SIZE" -A "$o" | awk -v o="$(basename "$o")" '$1 ~ /^\.text\./ {print o "|" $1 "|" $2}'
done > "$work/secs"
[ -s "$work/secs" ] || { echo "FAIL: no .text.* sections read from $rlib" >&2; exit 1; }

budget=$(sed -n 's/^ITCM_BUDGET_BYTES=\([0-9][0-9]*\).*/\1/p' "$SETFILE")
[ -n "$budget" ] || { echo "FAIL: no ITCM_BUDGET_BYTES in $SETFILE" >&2; exit 1; }

: > "$work/matched"
fail=0
nglobs=0
printf '%-64s %5s %8s\n' glob match bytes
while read -r glob rest; do
  case "$glob" in ''|'#'*|ITCM_BUDGET_BYTES=*) continue;; esac
  nglobs=$((nglobs + 1))
  n=0; b=0
  while IFS='|' read -r obj sec sz; do
    # shellcheck disable=SC2053
    if [[ $sec == $glob ]]; then
      n=$((n + 1)); b=$((b + sz)); echo "$obj|$sec|$sz" >> "$work/matched"
    fi
  done < "$work/secs"
  printf '%-64s %5d %8d\n' "$glob" "$n" "$b"
  if [ "$n" -eq 0 ]; then
    echo "  ^ no section matches (renamed or inlined away?): update $SETFILE" >&2
    fail=1
  fi
done < "$SETFILE"
[ "$nglobs" -gt 0 ] || { echo "FAIL: no globs in $SETFILE" >&2; exit 1; }

total=$(sort -u "$work/matched" | awk -F'|' '{s += $3} END {print s + 0}')
margin=$((budget - total))
echo "ITCM set: $nglobs globs, $total B of $budget B budget, margin $margin B"
if [ "$total" -gt "$budget" ]; then
  echo "FAIL: ITCM set is over budget by $((-margin)) B (docs/perf.md 'ITCM placement plan (M8)')" >&2
  fail=1
fi
if [ "$fail" -ne 0 ]; then
  echo "FAIL: ITCM set check (perf/itcm-sections.txt)" >&2
  exit 1
fi
echo "OK: every ITCM glob matches and the set fits"
