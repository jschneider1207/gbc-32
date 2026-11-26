#!/usr/bin/env bash
# Post-link check of the DEV-BOARD ITCM placement plan (perf/itcm-sections.txt;
# docs/perf.md "ITCM placement plan", ADR 0026) on a linked thumbv7em ELF:
# h7-bench's `itcm` images now, the gbc-nucleo-h753 firmware later. The
# pre-link check (scripts/check-itcm-set.sh) shows that every glob names a
# function of the rlib; this one shows where the linker put them.
#
#   scripts/check-itcm-elf.sh [--drop <tiers>] [--rlib <file|dir>] <elf>
#
#   --drop   the tiers of the set file the build left in flash on purpose,
#            comma-separated substrings of their `# Tier ...` header lines
#            (the syntax of h7-bench's H7_BENCH_ITCM_DROP). Default: none,
#            so every tier must be in ITCM. The caller says it because the
#            ELF does not: a check that guessed the drops from where the
#            functions landed would pass any misplacement.
#   --rlib   the gbc-core rlib the ELF was linked from, or a directory to
#            search for it (default: the ELF's directory). Of several, the
#            one whose crate disambiguator the ELF's gbc-core symbols carry.
#
# For each glob, the functions it names are the rlib's function sections the
# glob matches, as the linker matches them (`[[ == ]]` has the same `*`
# semantics), so a `#[cold]` function (`.text.unlikely.<sym>`) is never
# taken for a listed one. Plus any ELF symbol matching the glob that the
# rlib lacks (a gbc-core generic instantiated in a downstream crate).
#
# Fails (exit 1):
#   - a function of a kept tier that the link kept outside [0, 0x1_0000);
#     when the ELF has several function symbols of that name (a duplicate
#     local copy in flash), every copy must be inside it;
#   - a kept tier none of whose functions is in the link at all (a tier
#     wholly absent is a stale plan or a link that dropped it, not a pass;
#     only a --drop tier may be absent);
#   - a function of a --drop tier inside it (--drop disagrees with the link,
#     so the dropped-tier report would be false);
#   - a glob that matches no function section of the rlib (stale: renamed,
#     or inlined by rustc; check-itcm-set.sh fails the same list);
#   - no `.itcm` section, or one over 64 KiB, or no kept function in it;
#   - no `.text_core` section, or a gbc-core function of the rlib that the
#     link kept in flash outside it (ADR 0042; a name with several copies
#     passes when one is in ITCM or `.text_core`).
# Reported, exit 0: functions the rlib has and the ELF lacks (garbage-
# collected as unused, or inlined into their callers under LTO) as long as
# their tier has another function in the link, and the
# long-branch thunk (veneer) count by direction.
# Exit 2: tool error (bad arguments, missing ELF/rlib/tools, not ARM).
set -uo pipefail

usage() { echo "usage: $0 [--drop <tiers>] [--rlib <file|dir>] <elf>" >&2; exit 2; }
die() { echo "ERROR: $*" >&2; exit 2; }

drop=""; rlib=""; elf=""
while [ $# -gt 0 ]; do
  case "$1" in
    --drop) [ $# -ge 2 ] || usage; drop=$2; shift 2 ;;
    --rlib) [ $# -ge 2 ] || usage; rlib=$2; shift 2 ;;
    -h|--help) usage ;;
    -*) usage ;;
    *) [ -z "$elf" ] || usage; elf=$1; shift ;;
  esac
done
[ -n "$elf" ] || usage
# Paths are relative to the caller's directory; the set file to the repo.
abs() { realpath -e -- "$1" 2>/dev/null || die "no such file: $1"; }
elf=$(abs "$elf") || exit 2
if [ -n "$rlib" ]; then rlib=$(abs "$rlib") || exit 2; fi
if [ -n "${ITCM_SET_FILE:-}" ]; then SETFILE=$(abs "$ITCM_SET_FILE") || exit 2; fi
cd "$(dirname "$0")/.."
SETFILE=${SETFILE:-perf/itcm-sections.txt}
ITCM_END=$((0x10000))

for t in readelf ar c++filt; do command -v "$t" >/dev/null || die "$t (binutils) not found"; done
[ -f "$elf" ] || die "no ELF at $elf"
[ -f "$SETFILE" ] || die "no set file $SETFILE"
readelf -hW "$elf" 2>/dev/null | grep -c 'Machine: *ARM' >/dev/null || die "$elf is not an ARM ELF"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# ELF function symbols: "<addr hex> <size> <name> <bind>" (every reader takes
# the name as field 3 and nothing after it); thunks apart.
readelf -sW "$elf" | awk '$4 == "FUNC" {print $2, $3, $8, $5}' > "$work/elfsyms"
[ -s "$work/elfsyms" ] || die "no function symbols in $elf (stripped?)"
hash=$(grep -o 'Cs[0-9A-Za-z]*_8gbc_core' "$work/elfsyms" | sort | uniq -c | sort -rn | awk 'NR == 1 {print $2}')
[ -n "$hash" ] || die "no gbc-core symbols in $elf"

# The rlib: the one whose symbols carry the ELF's gbc-core disambiguator.
[ -n "$rlib" ] || rlib=$(dirname "$elf")
if [ -d "$rlib" ]; then
  found=""
  while IFS= read -r r; do
    if readelf -SW "$r" 2>/dev/null | grep -c -- "$hash" >/dev/null; then found=$r; break; fi
  done < <(find "$rlib" -name 'libgbc_core-*.rlib' -printf '%T@ %p\n' | sort -rn | cut -d' ' -f2-)
  [ -n "$found" ] || die "no libgbc_core-*.rlib with crate hash $hash under $rlib (pass --rlib)"
  rlib=$found
fi
[ -f "$rlib" ] || die "no rlib at $rlib"
rlib=$(realpath "$rlib")
mkdir "$work/o" && (cd "$work/o" && ar x "$rlib") || die "ar x $rlib failed"
# The rlib's function sections, by name (one per function: -ffunction-sections).
for o in "$work"/o/*.o; do readelf -SW "$o"; done |
  sed -n 's/^ *\[ *[0-9]*\] *\(\.text\.[^ ]*\) .*/\1/p' | sort -u > "$work/secs"
[ -s "$work/secs" ] || die "no .text.* sections read from $rlib"
grep -q -- "$hash" "$work/secs" || die "$rlib is not the gbc-core the ELF was linked from (no $hash)"

# Tiers and globs of the set file: "<tier header>|<glob>|<demangled name>".
awk '
  /^# Tier/ { tier = substr($0, 3); next }
  /^[[:space:]]*(#|$)/ || /^ITCM_BUDGET_BYTES=/ || /^FLASH_OK/ { next }
  { g = $1; n = $0; sub(/^[^#]*#[[:space:]]*/, "", n); if (n == $0) n = g; print tier "|" g "|" n }
' "$SETFILE" > "$work/globs"
[ -s "$work/globs" ] || die "no globs in $SETFILE"

# The --drop list must name tiers of the file (as h7-bench's build.rs asserts).
declare -a drops=()
IFS=, read -r -a raw <<< "$drop"
for d in "${raw[@]}"; do
  d=$(sed 's/^ *//; s/ *$//' <<< "$d")
  [ -n "$d" ] || continue
  grep -q '^# Tier.*'"$(sed 's/[][\.*^$/]/\\&/g' <<< "$d")" "$SETFILE" || die "--drop '$d' matches no '# Tier' line of $SETFILE"
  drops+=("$d")
done
dropped() { local d; for d in "${drops[@]}"; do [[ $1 == *"$d"* ]] && return 0; done; return 1; }

demangle() { c++filt | sed 's/\[[0-9a-f]*\]//g'; }  # without the crate hashes
loc() { if [ $(( (16#$1) & ~1 )) -lt "$ITCM_END" ]; then echo itcm; else echo flash; fi; }

fail=0
nkept=0; ndrop=0; nin=0; inbytes=0; nflash_ok=0; nabsent=0
declare -A tiers_dropped=() tier_present=()
: > "$work/absent"
while IFS='|' read -r tier glob name; do
  pat=${glob#.text.}
  # Symbols the glob names: from the rlib's sections, then the ELF's extras.
  : > "$work/want"
  while IFS= read -r sec; do
    # shellcheck disable=SC2053
    [[ $sec == $glob ]] && echo "${sec#.text.}" >> "$work/want"
  done < "$work/secs"
  if [ ! -s "$work/want" ]; then
    echo "FAIL: stale glob, no function section of the rlib matches it (renamed or inlined by rustc?): $glob  # $name" >&2
    fail=1
    continue
  fi
  while read -r _ _ sym _; do
    # shellcheck disable=SC2053
    [[ $sym == $pat ]] && ! grep -qxF -- ".text.$sym" "$work/secs" && ! grep -qxF -- ".text.unlikely.$sym" "$work/secs" && echo "$sym" >> "$work/want"
  done < "$work/elfsyms"
  sort -u -o "$work/want" "$work/want"
  isdrop=0
  if dropped "$tier"; then isdrop=1; ndrop=$((ndrop + 1)); tiers_dropped["$tier"]=1; else nkept=$((nkept + 1)); tier_present["$tier"]=${tier_present["$tier"]:-0}; fi
  while IFS= read -r sym; do
    # Every copy: a duplicate local symbol of the same name must not hide
    # behind the first one.
    copies=$(awk -v s="$sym" '$3 == s {print $1, $2}' "$work/elfsyms")
    if [ -z "$copies" ]; then
      nabsent=$((nabsent + 1)); echo "$sym" >> "$work/absent"; continue
    fi
    [ "$isdrop" -eq 1 ] || tier_present["$tier"]=$((tier_present["$tier"] + 1))
    while read -r addr sz; do
    where=$(loc "$addr")
    if [ "$isdrop" -eq 0 ] && [ "$where" = flash ]; then
      printf 'FAIL: kept-tier function in flash at 0x%s, not in ITCM: %s\n  glob %s (%s)\n' "$addr" "$(demangle <<< "$sym")" "$glob" "$tier" >&2
      fail=1
    elif [ "$isdrop" -eq 1 ] && [ "$where" = itcm ]; then
      printf 'FAIL: function of a --drop tier is in ITCM at 0x%s: %s\n  glob %s (%s): --drop disagrees with the link\n' "$addr" "$(demangle <<< "$sym")" "$glob" "$tier" >&2
      fail=1
    elif [ "$where" = itcm ]; then
      nin=$((nin + 1)); inbytes=$((inbytes + sz))
    else
      nflash_ok=$((nflash_ok + 1))
    fi
    done <<< "$copies"
  done < "$work/want"
done < "$work/globs"

echo "globs: $nkept kept, $ndrop in dropped tiers; kept functions in ITCM: $nin ($inbytes B); dropped-tier functions in flash: $nflash_ok"
if [ "${#tiers_dropped[@]}" -eq 0 ]; then
  echo "dropped tiers: none"
else
  for t in "${!tiers_dropped[@]}"; do echo "dropped tier (left in flash on purpose): $t"; done | sort
fi
if [ "$nabsent" -gt 0 ]; then
  echo "not in the link ($nabsent; unused and garbage-collected, or inlined under LTO; report only):"
  sort -u "$work/absent" | demangle | sed 's/^/  /'
fi
for t in "${!tier_present[@]}"; do
  if [ "${tier_present[$t]}" -eq 0 ]; then
    echo "FAIL: kept tier has no function in the link at all (not in --drop): $t" >&2; fail=1
  fi
done
if [ "$nin" -eq 0 ]; then
  echo "FAIL: no kept function of $SETFILE is in ITCM" >&2; fail=1
fi

# .itcm size against the 64 KiB ITCM.
itcm=$(readelf -SW "$elf" | sed -n 's/^ *\[ *[0-9]*\] *\.itcm  *[A-Z]*  *\([0-9a-f]*\) [0-9a-f]* \([0-9a-f]*\) .*/\1 \2/p')
if [ -z "$itcm" ]; then
  echo "FAIL: no .itcm section in $elf" >&2; fail=1
else
  read -r iaddr isize <<< "$itcm"
  isize=$((16#$isize)); iend=$((16#$iaddr + isize))
  echo ".itcm: $isize B at 0x$iaddr of 65536 B ITCM (ends 0x$(printf '%x' "$iend"), $((65536 - iend)) B free)"
  if [ "$isize" -gt 65536 ] || [ "$iend" -gt 65536 ]; then
    echo "FAIL: .itcm is over the 64 KiB ITCM" >&2; fail=1
  fi
fi

# gbc-core's flash code (ADR 0042, memory_x::CORE_TEXT_OFFSET): the rlib's
# functions that the link kept outside ITCM must be inside `.text_core`,
# the fixed-address section in front of the image's and the lib's code, or
# their edits move the core's code again. A name alone does not tell the
# rlib's copy from a downstream crate's: rustc also compiles small and
# generic gbc-core functions into the crates that call them, as LOCAL
# copies of the same name, which are those crates' code and belong in
# `.text`. So a copy in `.text_core` (which takes only gbc-core's archive
# member) is the rlib's, and the check fails on
#   - a GLOBAL copy of a gbc-core function outside ITCM and `.text_core`
#     (the linker kept the rlib's exported definition elsewhere);
#   - a function that a gbc-core function inside ITCM or `.text_core`
#     references (a relocation in the rlib), with no copy in either: the
#     reference binds to the rlib's copy, so that copy is in the link,
#     and it is not where it belongs.
# Other names with copies only in `.text` are reported as downstream copies.
core=$(readelf -SW "$elf" | sed -n 's/^ *\[ *[0-9]*\] *\.text_core  *[A-Z]*  *\([0-9a-f]*\) [0-9a-f]* \([0-9a-f]*\) .*/\1 \2/p')
if [ -z "$core" ]; then
  echo "FAIL: no .text_core section in $elf (gbc-core's flash code is not at its fixed address)" >&2; fail=1
else
  read -r caddr csize <<< "$core"
  clo=$((16#$caddr)); chi=$((16#$caddr + 16#$csize))
  sed 's/^\.text\.//; s/^unlikely\.//' "$work/secs" > "$work/coresyms"
  # References between the rlib's function sections: "<from> <to>".
  for o in "$work"/o/*.o; do readelf -rW "$o"; done | awk '
    /^Relocation section/ { s = $3; gsub(/\047/, "", s); ok = sub(/^\.rela?\.text\./, "", s); sub(/^unlikely\./, "", s); next }
    ok && NF >= 5 && $1 ~ /^[0-9a-f]+$/ { t = $5; sub(/^\.text\./, "", t); sub(/^unlikely\./, "", t); print s, t }' > "$work/corerefs"
  awk -v lo="$clo" -v hi="$chi" -v itcm="$ITCM_END" '
    FILENAME == ARGV[1] { want[$1] = 1; next }
    FILENAME == ARGV[2] {
      if (!($3 in want)) next
      a = and(strtonum("0x" $1), 0xfffffffe); inside = (a < itcm || (a >= lo && a < hi))
      kept[$3] = 1; if (inside) ok[$3] = 1
      else { at[$3] = $1; if ($4 == "GLOBAL" && !($3 in seen)) { seen[$3] = 1; print "global", $1, $3 } }
      next
    }
    ($1 in ok) && ($2 in kept) && !($2 in ok) && !(($2) in seen) { seen[$2] = 1; print "ref", at[$2], $2 }
    END {
      for (n in kept) { nk++; if (!(n in ok) && !(n in seen)) nd++ }
      printf "%d %d\n", nk, nd > "/dev/stderr"
    }' "$work/coresyms" "$work/elfsyms" "$work/corerefs" > "$work/misplaced" 2> "$work/ncore"
  read -r nkept_core ncopies < "$work/ncore"
  echo ".text_core: $((16#$csize)) B at 0x$caddr; $nkept_core gbc-core function names in the link; $ncopies other names with copies only in .text (downstream LOCAL copies)"
  if [ -s "$work/misplaced" ]; then
    echo "FAIL: gbc-core functions in flash outside .text_core ($(sort -u -k3 "$work/misplaced" | wc -l)):" >&2
    sort -u -k3 "$work/misplaced" | while read -r why a n; do printf '  %s 0x%s %s\n' "$why" "$a" "$(demangle <<< "$n")"; done | head -20 >&2
    fail=1
  fi
fi

# Veneers: lld's long-branch thunks, GNU ld's `*_veneer`.
awk '$3 ~ /LongThunk_|_veneer$/ {print $1}' "$work/elfsyms" > "$work/thunks"
tin=0; tout=0
while read -r a; do if [ "$(loc "$a")" = itcm ]; then tin=$((tin + 1)); else tout=$((tout + 1)); fi; done < "$work/thunks"
echo "veneers: $((tin + tout)) long-branch thunks ($tin in ITCM, calls to flash; $tout in flash, calls to ITCM)"

if [ "$fail" -ne 0 ]; then
  echo "FAIL: post-link ITCM check ($elf)" >&2
  exit 1
fi
echo "OK: every kept function of $SETFILE that the link kept is in ITCM"
