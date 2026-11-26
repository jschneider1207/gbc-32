#!/usr/bin/env bash
# Checks one bench report read from the board (README.md "QSPI ROM
# placement") and prints its cycle table:
#
#   ./check-report.sh <capture.txt> <image.bin> [--qspi]
#
# The capture is read-serial.sh's output. The report checked is the last
# complete one in it: a `h7bench v1 banner image=bench` line through the
# next `h7bench v1 end pass=` line (the board repeats the report; the first
# one after a flash is often cut by the ST-LINK's VCP). It is saved next to
# the capture as <capture>.report. Checks:
#   - the banner's `image_bytes=<n> image_cksum=<crc>` is `cksum` of
#     <image.bin> (the board runs the image that was flashed);
#   - the internal-flash lines (`h7bench v1 <name> calls=...`), with the
#     cycle fields (min, mean, max) set to 0, are exactly
#     expected-hashes.txt: every workload, in order;
#   - with --qspi, also a `h7bench v1 qspi pack ok` line with
#     `same_as_flash=1`, and the `h7bench v1 qspi <name>` lines, with
#     `qspi ` removed and the cycle fields 0, exactly expected-hashes.txt.
# Without --qspi a report with QSPI lines is still checked for its flash
# lines only.
#
# Then it prints one row per workload: mean / max from internal flash,
# from QSPI, and QSPI against flash in % (with --qspi).
#
# Exit: 0 all checks pass, 1 a check fails, 2 no complete report or a
# usage error.
set -euo pipefail
here=$(dirname "$(readlink -f "$0")")
die() { echo "check-report.sh: $*" >&2; exit 2; }
[ $# -ge 2 ] && [ $# -le 3 ] || die "usage: check-report.sh <capture.txt> <image.bin> [--qspi]"
cap=$1 img=$2 qspi=0
[ $# -eq 3 ] && { [ "$3" = --qspi ] || die "unknown option $3"; qspi=1; }
[ -f "$cap" ] || die "$cap: no such file"
[ -f "$img" ] || die "$img: no such file"
expected=$here/expected-hashes.txt
report=$cap.report

# The last complete report (NULs from the VCP removed).
tr -d '\000\r' < "$cap" | awk '
  /^h7bench v1 banner image=bench / { inb = 1; buf = "" }
  inb { buf = buf $0 "\n" }
  inb && /^h7bench v1 end pass=[0-9]+$/ { last = buf; inb = 0 }
  END { printf "%s", last }' > "$report"
[ -s "$report" ] || die "$cap: no complete report (banner image=bench .. end pass=N)"

fail=0
bad() { echo "check-report.sh: FAIL: $*" >&2; fail=1; }

read -r crc bytes < <(cksum < "$img")
got=$(grep -oE ' image_bytes=[0-9]+ image_cksum=[0-9]+( |$)' "$report" | head -n 1 | sed 's/^ //; s/ $//' || true)
if [ "$got" = "image_bytes=$bytes image_cksum=$crc" ]; then
  echo "check-report.sh: banner $got = cksum of $img"
else
  bad "banner has '${got:-no image_bytes/image_cksum}', $img is image_bytes=$bytes image_cksum=$crc"
fi

# The lines of one source, cycle fields zeroed, source word removed.
names=$(sed -n 's/^h7bench v1 \([^ ]*\) .*/\1/p' "$expected" | paste -sd'|')
lines() { # lines <source word or empty>
  grep -E "^h7bench v1 $1($names) " "$report" |
    sed -E "s/^h7bench v1 $1/h7bench v1 /; s/ min=[0-9]+ mean=[0-9]+ max=[0-9]+ / min=0 mean=0 max=0 /"
}
check() { # check <source word or empty> <label>
  if diff -u "$expected" <(lines "$1") > "$report.$2.diff"; then
    echo "check-report.sh: $2: $(wc -l < "$expected") lines equal expected-hashes.txt"
    rm -f "$report.$2.diff"
  else
    bad "$2 lines differ from expected-hashes.txt ($report.$2.diff)"
  fi
}
check "" flash
if [ "$qspi" = 1 ]; then
  pack=$(grep -E '^h7bench v1 qspi pack ' "$report" || true)
  echo "check-report.sh: ${pack:-no qspi pack line}"
  case "$pack" in
    "h7bench v1 qspi pack ok "*" same_as_flash=1 "*) ;;
    *) bad "no 'qspi pack ok ... same_as_flash=1' line" ;;
  esac
  check "qspi " qspi
fi

# The cycle table.
awk -v q="$qspi" '
  function f(n,   s, r) { s = sprintf("%d", n); r = ""; while (length(s) > 3) { r = "," substr(s, length(s) - 2) r; s = substr(s, 1, length(s) - 3) } return s r }
  function field(k,   i, a) { for (i = 1; i <= NF; i++) { split($i, a, "="); if (a[1] == k) return a[2] } }
  $1 == "h7bench" && $3 == "qspi" && $4 != "pack" && $5 ~ /^calls=/ { qm[$4] = field("mean"); qx[$4] = field("max"); next }
  $1 == "h7bench" && $4 ~ /^calls=/ { order[++n] = $3; fm[$3] = field("mean"); fx[$3] = field("max") }
  END {
    if (q == 1) print "| Workload | internal mean / max | QSPI mean / max | QSPI vs internal, mean / max |"
    else print "| Workload | internal mean / max |"
    for (i = 1; i <= n; i++) {
      w = order[i]
      if (q == 1 && (w in qm))
        printf "| %s | %s / %s | %s / %s | %+.2f%% / %+.2f%% |\n", w, f(fm[w]), f(fx[w]), f(qm[w]), f(qx[w]),
          (qm[w] / fm[w] - 1) * 100, (qx[w] / fx[w] - 1) * 100
      else printf "| %s | %s / %s |\n", w, f(fm[w]), f(fx[w])
    }
  }' "$report"

[ "$fail" -eq 0 ] || { echo "check-report.sh: FAILED ($report)" >&2; exit 1; }
echo "check-report.sh: OK ($report)"
