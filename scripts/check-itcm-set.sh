#!/usr/bin/env bash
# Pre-link check of the DEV-BOARD ITCM placement plan (perf/itcm-sections.txt;
# docs/perf.md "ITCM placement plan", ADR 0026) on the gbc-core rlib for
# thumbv7em. It only matters for Stage 2 on the NUCLEO-H753ZI (64 KiB ITCM,
# fixed). The final MCU, the STM32H735, places all of gbc-core in a 128 KiB
# ITCM; what gates there is the total in scripts/check-code-size.sh
# (human, 2026-11-24: the dev board may run slower).
#
# Fails (exit 1): a glob that matches no function section (a rename or
# inlining shows up as MISSING: a stale list is a bug, because the Stage 2
# linker rules are generated from it); a FLASH_OK line without a reason; the
# build failing or the relocations not being understood.
#
# Reported with a WARN line, exit 0:
#   - the summed size of all matched sections over ITCM_BUDGET_BYTES (a
#     section matched by two globs is counted once);
#   - callee closure: a direct call or tail call (R_ARM_THM_CALL /
#     R_ARM_THM_JUMP* relocation) from a section in the set to a gbc-core
#     function outside it that is neither `#[cold]` (rustc puts it in
#     `.text.unlikely.`) nor on a `FLASH_OK <glob>  # <reason>` line. Such a
#     call crosses ITCM -> flash through a linker veneer and runs behind the
#     H753's 16 KiB I-cache. A FLASH_OK line that no longer matches such a
#     callee is reported the same way.
# Not seen: calls through function pointers (gbc-core has none on
# these paths) and callees outside the rlib (`__aeabi_mem*`, core's panic
# paths; the front end's ITCM reserve and the post-link veneer count cover
# those, docs/perf.md).
set -euo pipefail
cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

SETFILE=${ITCM_SET_FILE:-perf/itcm-sections.txt}
TARGET=thumbv7em-none-eabihf
TDIR=target/codesize

SIZE=$(command -v llvm-size || command -v rust-size || command -v size) || { echo "no size found" >&2; exit 2; }
READELF=$(command -v readelf || command -v llvm-readelf) || { echo "no readelf found" >&2; exit 2; }

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
  case "$glob" in ''|'#'*|ITCM_BUDGET_BYTES=*|FLASH_OK) continue;; esac
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
echo "H753 ITCM set: $nglobs globs, $total B of $budget B budget, margin $margin B (report only)"
nwarn=0
if [ "$total" -gt "$budget" ]; then
  echo "WARN: H753 ITCM set is over its $budget B budget by $((-margin)) B: the Stage 2 build must drop functions from the set (docs/perf.md 'ITCM placement plan'; report only)"
  nwarn=$((nwarn + 1))
fi

# Callee closure. Edges "<calling section>|<target symbol>" from the
# relocation sections (`.rel.text.<fn>` holds the relocations of `.text.<fn>`).
"$READELF" -rW "$work"/*.o | awk '
  /^Relocation section/ { src = $3; gsub(/\x27/, "", src); sub(/^\.rela?/, "", src); next }
  $3 == "R_ARM_THM_CALL" || $3 ~ /^R_ARM_THM_JUMP/ { print src "|" $5 }' | sort -u > "$work/edges"
cut -d'|' -f2,3 "$work/secs" | sort -u > "$work/secsz"
cut -d'|' -f2 "$work/matched" | sort -u > "$work/inset"
# Calls out of the set to a gbc-core function that is neither in it nor cold:
# "<callee section>|<size>|<one calling section>".
awk -F'|' '
  FILENAME == ARGV[1] { inset[$1] = 1; next }
  FILENAME == ARGV[2] { sz[$1] = $2; next }
  ($1 in inset) {
    nedges++
    t = $2
    if (t ~ /^\.text\./) s = t                      # a section symbol
    else if ((".text." t) in sz) s = ".text." t
    else if ((".text.unlikely." t) in sz) s = ".text.unlikely." t
    else next                                        # not defined in gbc-core
    ncore++
    if ((s in inset) || s ~ /^\.text\.unlikely\./ || (s in out)) next
    out[s] = $1
  }
  END {
    for (s in out) print s "|" sz[s] "|" out[s]
    print nedges + 0, ncore + 0 > "/dev/stderr"
  }' "$work/inset" "$work/secsz" "$work/edges" 2> "$work/nedges" | sort > "$work/out"
read -r nedges ncore < "$work/nedges"
# A parse that finds no calls must not pass: Cpu::run alone has hundreds.
if [ "${ncore:-0}" -lt 50 ]; then
  echo "FAIL: only ${ncore:-0} gbc-core call relocations read from the ITCM set (readelf output not understood?)" >&2
  fail=1
fi
demangle() { if command -v c++filt >/dev/null; then c++filt; else cat; fi; }
: > "$work/allowed"
nallow=0
while read -r key glob rest; do
  [ "$key" = FLASH_OK ] || continue
  nallow=$((nallow + 1))
  case "$rest" in
    '#'*[![:space:]#]*) ;;
    *) echo "FAIL: FLASH_OK $glob has no '# <reason>'" >&2; fail=1 ;;
  esac
  n=0
  while IFS='|' read -r sec sz caller; do
    # shellcheck disable=SC2053
    if [[ $sec == $glob ]]; then n=$((n + 1)); echo "$sec" >> "$work/allowed"; fi
  done < "$work/out"
  if [ "$n" -eq 0 ]; then
    echo "WARN: FLASH_OK $glob matches no non-ITCM, non-cold function called from the ITCM set (stale: remove it; report only)"
    nwarn=$((nwarn + 1))
  fi
done < "$SETFILE"
sort -u -o "$work/allowed" "$work/allowed"
nopen=0; allowed_bytes=0
while IFS='|' read -r sec sz caller; do
  if grep -qxF -- "$sec" "$work/allowed"; then
    allowed_bytes=$((allowed_bytes + sz))
    continue
  fi
  nopen=$((nopen + 1))
  nwarn=$((nwarn + 1))
  echo "WARN: H753 ITCM function calls a non-ITCM, non-cold gbc-core function ($sz B; report only):"
  printf '  callee %s\n  caller %s\n' "${sec#.text.}" "${caller#.text.}" | demangle
  echo "  add the callee to the set, or a 'FLASH_OK <glob>  # <reason>' line, in $SETFILE"
done < "$work/out"
echo "H753 ITCM closure (report only): $nedges call relocations in the set, $ncore to gbc-core; $(wc -l < "$work/allowed") warm callees left in flash by FLASH_OK ($allowed_bytes B, $nallow lines), $nopen not allowed"

if [ "$fail" -ne 0 ]; then
  echo "FAIL: ITCM set check ($SETFILE)" >&2
  exit 1
fi
if [ "$nwarn" -ne 0 ]; then
  echo "OK with $nwarn WARN: every ITCM glob matches; H753 fit/closure warnings above are report only (Stage 2 dev board)"
else
  echo "OK: every ITCM glob matches, the set fits the H753 budget and its warm callees are in it or allowed"
fi
