#!/usr/bin/env bash
# Code size of gbc-core on the MCU build (thumbv7em-none-eabihf, release with
# the pinned `codegen-units = 1`, cortex-m7) against perf/code-size-budgets.txt.
# See docs/perf.md "Code-size budgets".
#
# Sized for the final MCU, the STM32H735 (human, 2026-11-24): its ITCM is
# configured to 128 KiB and holds all of gbc-core, so the one constraint is
# the total, not any single function.
#
# Gating (exit 1):
#   - total .text (every .text* section of the rlib, alignment padding
#     included) over the `<bytes> (total .text)` line, or that line missing,
#     doubled or marked `report`;
#   - a per-function line not marked `report`, or a malformed line;
#   - the build failing, or no symbols read.
# Reported, never failing: every `report <bytes> <symbol>` line (size, the
# old figure, % of it; "over, report only" above it; a symbol that is not
# found prints a WARN), and the on-target sizes of `Emulator` and `Memory`.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

BUDGETS=${CODE_SIZE_BUDGETS_FILE:-perf/code-size-budgets.txt}
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
# One line per symbol: "<size>\t<demangled name>\t<mangled name>". The two
# nm runs print in symbol-table order (-p), so their lines pair up; the
# pairing check guards the parse.
: > "$work/syms"
for o in "$work"/*.o; do
  "$NM" -p -S -C --radix=d "$o" > "$work/dem"
  "$NM" -p -S --radix=d "$o" > "$work/man"
  [ "$(wc -l < "$work/dem")" -eq "$(wc -l < "$work/man")" ] \
    || { echo "FAIL: nm with and without -C disagree on $o" >&2; exit 1; }
  paste -d'\n' "$work/dem" "$work/man" | awk '
    NR % 2 == 1 { ok = (NF >= 4); if (ok) { size = $2 + 0; name = $4; for (i = 5; i <= NF; i++) name = name " " $i }; next }
    ok && NF == 4 { print size "\t" name "\t" $4 }' >> "$work/syms"
done
[ -s "$work/syms" ] || { echo "FAIL: no symbols read from $rlib" >&2; exit 1; }
# Total code: every .text* section of every object in the rlib (all of
# gbc-core, before the final link drops what a front end does not use).
total_text=$("$SIZE" -A "$work"/*.o | awk '$1 ~ /^\.text/ {s += $2} END {print s + 0}')
[ "$total_text" -gt 0 ] || { echo "FAIL: no .text sections read from $rlib" >&2; exit 1; }

fail=0
ntotal=0
nreport=0
nmissing=0
nover=0
total_budget=
printf '%-62s %8s %8s %6s\n' symbol size figure used
while read -r budget name; do
  case "$budget" in ''|'#'*) continue;; esac
  report=0
  if [ "$budget" = report ]; then
    report=1
    read -r budget name <<<"$name"
  fi
  case "$budget" in
    ''|*[!0-9]*) echo "FAIL: bad line in $BUDGETS: '$budget $name'" >&2; fail=1; continue;;
  esac
  if [ "$name" = "(total .text)" ]; then
    if [ "$report" -eq 1 ]; then
      echo "FAIL: '(total .text)' gates: write '$budget (total .text)' in $BUDGETS, without 'report'" >&2
      fail=1
      continue
    fi
    ntotal=$((ntotal + 1))
    total_budget=$budget
    continue
  fi
  if [ "$report" -eq 0 ]; then
    echo "FAIL: per-function lines are report-only: write 'report $budget $name' in $BUDGETS" >&2
    fail=1
    continue
  fi
  nreport=$((nreport + 1))
  size=$(awk -F'\t' -v n="$name" '$2==n {s+=$1; f=1} END {if (f) print s}' "$work/syms")
  if [ -z "$size" ]; then
    printf '%-62s %8s %8s %6s  WARN: not found (renamed or inlined away?), report only\n' "$name" MISSING "$budget" -
    nmissing=$((nmissing + 1))
    continue
  fi
  pct=$(( size * 100 / budget ))
  status="  (report only)"
  if [ "$size" -gt "$budget" ]; then status="  over, report only"; nover=$((nover + 1)); fi
  printf '%-62s %8d %8d %5d%%%s\n' "$name" "$size" "$budget" "$pct" "$status"
done < "$BUDGETS"
[ "$nreport" -gt 0 ] || echo "note: no per-function report lines in $BUDGETS"
[ "$nmissing" -eq 0 ] || echo "WARN: $nmissing report line(s) name a symbol that was not found: update $BUDGETS (report only)"

# The gate: all of gbc-core must fit the H735 ITCM share
# (docs/perf.md "Code-size budgets").
if [ "$ntotal" -ne 1 ]; then
  echo "FAIL: $BUDGETS needs exactly one '<bytes> (total .text)' line (found $ntotal)" >&2
  fail=1
else
  pct=$(( total_text * 100 / total_budget ))
  status="  GATE"
  if [ "$total_text" -gt "$total_budget" ]; then status="  OVER BUDGET"; fail=1; fi
  printf '%-62s %8d %8d %5d%%%s\n' "(total .text)" "$total_text" "$total_budget" "$pct" "$status"
fi

# On-target struct sizes, report only: a probe crate whose statics are as
# long as the types, compiled against the rlib just built (rustc alone,
# well under a second). `Emulator` is `Hw` plus the `&mut Memory` pointer.
probe_src="$work/size_probe.rs"
cat > "$probe_src" <<'PROBE'
#![no_std]
use core::mem::size_of;
pub static GBC_SIZE_EMULATOR: [u8; size_of::<gbc_core::Emulator<'static>>()] =
    [0; size_of::<gbc_core::Emulator<'static>>()];
pub static GBC_SIZE_MEMORY: [u8; size_of::<gbc_core::Memory>()] =
    [0; size_of::<gbc_core::Memory>()];
PROBE
if rustc --edition 2024 --crate-type rlib --crate-name size_probe --target "$TARGET" \
    -C target-cpu=cortex-m7 --emit=obj -o "$work/size_probe.o" \
    --extern gbc_core="$rlib" "$probe_src" 2> "$work/probe.err"; then
  "$NM" -S --radix=d "$work/size_probe.o" | awk '
    /GBC_SIZE_EMULATOR/ { printf "size_of::<Emulator>() on %s: %d B (report only)\n", t, $2 }
    /GBC_SIZE_MEMORY/ { printf "size_of::<Memory>() on %s: %d B (report only)\n", t, $2 }' t="$TARGET"
else
  echo "struct sizes: not measured (size probe did not compile; report only)"
  sed 's/^/  /' "$work/probe.err" | head -n 5
fi

if [ "$fail" -ne 0 ]; then
  echo "FAIL: code-size check (see docs/perf.md, 'Code-size budgets')" >&2
  exit 1
fi
echo "OK: total .text $total_text B within $total_budget B (margin $((total_budget - total_text)) B); $nreport per-function lines reported ($nover over their figure, $nmissing missing)"
