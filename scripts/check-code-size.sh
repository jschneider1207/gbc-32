#!/usr/bin/env bash
# Code size of gbc-core on the MCU build (thumbv7em-none-eabihf, release with
# the pinned `codegen-units = 1`, cortex-m7) against perf/code-size-budgets.txt.
# See docs/perf.md "Code-size budgets".
#
# Gating (exit 1):
#   - a `<bytes> <symbol>` line whose function is over its budget, or is not
#     found (renamed or inlined away: update the file);
#   - such a line for a function outside the ITCM set (perf/itcm-sections.txt),
#     or a `report` line for a function inside it: per-function budgets gate
#     exactly the functions that go in ITCM, where a byte is scarce;
#   - the build failing, or no symbols read.
# Reported, never failing: `report <bytes> <symbol>` lines (functions that stay
# in flash), `report <bytes> (total .text)`, and the on-target sizes of
# `Emulator` and `Memory`.
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

BUDGETS=perf/code-size-budgets.txt
SETFILE=perf/itcm-sections.txt
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
# One line per symbol: "<size>\t<demangled name>\t<mangled name>". The two nm
# runs print in symbol-table order (-p), so their lines pair up.
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

# The ITCM globs (not the FLASH_OK lines, which name functions kept in flash).
globs=()
while read -r glob rest; do
  case "$glob" in ''|'#'*|ITCM_BUDGET_BYTES=*|FLASH_OK) continue;; esac
  globs+=("$glob")
done < "$SETFILE"
[ "${#globs[@]}" -gt 0 ] || { echo "FAIL: no globs in $SETFILE" >&2; exit 1; }
# Is the function with this mangled name in the ITCM set?
in_itcm() {
  local g
  for g in "${globs[@]}"; do
    # shellcheck disable=SC2053
    [[ .text.$1 == $g ]] && return 0
  done
  return 1
}

fail=0
ngate=0
printf '%-62s %8s %8s %6s\n' symbol size budget used
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
    if [ "$report" -eq 0 ]; then
      echo "FAIL: '(total .text)' is report-only: write 'report $budget (total .text)' in $BUDGETS" >&2
      fail=1
    fi
    size=$total_text
  else
    size=$(awk -F'\t' -v n="$name" '$2==n {s+=$1; f=1} END {if (f) print s}' "$work/syms")
  fi
  if [ -z "$size" ]; then
    if [ "$report" -eq 1 ]; then
      printf '%-62s %8s %8s %6s  (report only)\n' "$name" MISSING "$budget" -
    else
      printf '%-62s %8s %8s %6s\n' "$name" MISSING "$budget" -
      echo "  ^ symbol not found (renamed or inlined away?): update $BUDGETS" >&2
      fail=1
    fi
    continue
  fi
  if [ "$name" != "(total .text)" ]; then
    hot=0
    while IFS=$'\t' read -r _ _ mangled; do
      if in_itcm "$mangled"; then hot=1; fi
    done < <(awk -F'\t' -v n="$name" '$2==n' "$work/syms")
    if [ "$report" -eq 0 ] && [ "$hot" -eq 0 ]; then
      echo "FAIL: $name has a gating budget but is not in the ITCM set ($SETFILE): mark the line 'report'" >&2
      fail=1
    elif [ "$report" -eq 1 ] && [ "$hot" -eq 1 ]; then
      echo "FAIL: $name is in the ITCM set ($SETFILE), so its budget gates: remove 'report'" >&2
      fail=1
    fi
  fi
  pct=$(( size * 100 / budget ))
  status=""
  if [ "$report" -eq 1 ]; then
    status="  (report only)"
    if [ "$size" -gt "$budget" ]; then status="  over, report only"; fi
  else
    ngate=$((ngate + 1))
    if [ "$size" -gt "$budget" ]; then status="  OVER BUDGET"; fail=1; fi
  fi
  printf '%-62s %8d %8d %5d%%%s\n' "$name" "$size" "$budget" "$pct" "$status"
done < "$BUDGETS"
[ "$ngate" -gt 0 ] || { echo "FAIL: no gating budget lines in $BUDGETS" >&2; fail=1; }

# On-target struct sizes, report only: a probe crate whose statics are as
# long as the types, compiled against the rlib just built (rustc alone,
# well under a second). `Emulator` is `Hw` plus the `&mut Memory` pointer.
probe_src="$work/size_probe.rs"
cat > "$probe_src" <<'EOF'
#![no_std]
use core::mem::size_of;
pub static GBC_SIZE_EMULATOR: [u8; size_of::<gbc_core::Emulator<'static>>()] =
    [0; size_of::<gbc_core::Emulator<'static>>()];
pub static GBC_SIZE_MEMORY: [u8; size_of::<gbc_core::Memory>()] =
    [0; size_of::<gbc_core::Memory>()];
EOF
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
echo "OK: $ngate ITCM-set functions within their budgets"
