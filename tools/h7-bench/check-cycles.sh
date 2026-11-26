#!/usr/bin/env bash
# The on-board cycle ratchet (plan Stage 2 M6, gate step B2): checks the
# cycles per frame of one bench report against the committed baseline.
#
#   ./check-cycles.sh [--fresh] <capture.txt> <image.bin> [<expected-cycles.txt>]
#   ./check-cycles.sh --update [--force [--absorb-regression]] <capture.txt> <image.bin> <out-file>
#
# The capture is read-serial.sh's output of a `bench-itcm-dtcm-pll480` run
# (the one check-report.sh takes, README.md "QSPI ROM placement"); the
# baseline defaults to expected-cycles.txt next to this script.
#
# First, check-report.sh must pass on the capture (with --qspi when the
# capture or the baseline has QSPI lines), so cycles are never judged on a
# garbled report or on another image; its exit code is passed on. It
# zeroes the cycle fields before comparing, and the VCP drops bytes, so a
# dropped digit in `max=` would pass it. So, in both modes (exit 1):
#   - the capture has at least two complete reports (banner .. `end
#     pass=N`; the board repeats its report), and of the last two, the last
#     is the one check-report.sh checked;
#   - the two have the same workload lines (both passes), each with
#     identical fields other than min/mean/max, and min, mean and max each
#     within 1% of the other report's (|a - b| * 100 <= the smaller; the
#     board reprints the same numbers, so they are equal in practice);
#   - every workload line of both has min <= mean <= max.
# Cycles are judged on the last complete report. Then (check mode):
#   - the banner's `placement=` and `cpu_hz=` equal the baseline header's;
#   - every baseline line has its workload line in the report (`flash <w>`
#     is `h7bench v1 <w> calls=...`, `qspi <w>` is `h7bench v1 qspi <w>
#     calls=...`), and both its mean and max are at most the baseline's
#     + 3%, in integers: value * 100 <= base * 103. Faster is reported,
#     never a failure. Each line's delta % is printed;
#   - every workload line in the report has a baseline line (a new
#     workload, or a QSPI pass the baseline lacks, needs a baseline line);
#   - real time: every sign-off workload (`worst-case`, synth:worst-case,
#     and `cpu_instrs`, the one replay ROM in this bench) has max <
#     7,902,557 cycles per frame in every pass.
# --fresh (check mode; gate B2): also, both of the last two complete
# reports start after the capture's last `h7bench v1 start ` line (the
# workloads' start lines of the boot that was flashed), else exit 2, so a
# report the VCP buffered from an earlier boot is never judged.
# `worst-case` max against 3,951,279 (the 50% figure) is a REPORT line,
# never a check (human, 2026-11-25).
#
# The baseline file: `#` comments and blank lines, one header line
#   h7cycles v1 image=<bin name> placement=<p> cpu_hz=472000000 commit=<sha>[+dirty] image_bytes=<n> image_cksum=<crc>
# (image, commit, image_bytes and image_cksum record what it was measured
# on; only placement and cpu_hz are compared), then one line per pass and
# workload, in report order:
#   flash worst-case mean=3301784 max=3339795
#   qspi worst-case mean=3380000 max=3417095
#
# --update writes such a file from a capture, only after check-report.sh
# and the report checks above pass and only for a 472 MHz banner. It
# refuses an existing <out-file> unless --force. With --force it compares
# the new lines with the existing file's and refuses (exit 1, listing them)
# when any line's mean or max is above its old value, or an old line has
# no new one, unless --absorb-regression is also given. The recorded
# commit is HEAD, `+dirty` when the tree differs from HEAD (tracked or
# untracked files) under crates/, tools/h7-bench/, tools/memory-x/,
# perf/itcm-sections.txt, .cargo/ or rust-toolchain.toml; there is no
# override. Rules for the committed baseline:
#   - --absorb-regression, i.e. raising any line of the committed
#     baseline (not only a > +3% failure), needs the human's approval, as
#     for check-bench.sh (plan M6); the commit that uses it quotes the
#     human. Refreshing it after an improvement does not.
#   - Measure it on an image built in the main checkout path
#     (/home/jess/gbc-32), after ADR 0042's core-first layout: crate hashes
#     in symbol names depend on the build path, which moves code and
#     changes cycles (ADR 0042).
#
# Exit: 0 all checks pass; 1 a check fails (also a missing baseline file,
# missing or extra lines, or check-report.sh's own 1); 2 a tool, usage or
# parse error (malformed baseline, no complete report: check-report.sh's 2).
set -euo pipefail
here=$(dirname "$(readlink -f "$0")")
die() { echo "check-cycles.sh: $*" >&2; exit 2; }
usage="usage: check-cycles.sh [--fresh] <capture> <image.bin> [<expected-cycles.txt>] | --update [--force [--absorb-regression]] <capture> <image.bin> <out-file>"
REAL_TIME=7902557   # one real-time frame at 472 MHz (472e6 / 59.7275)
HALF=3951279        # the 50% figure (plan M6), report only
# The sign-off workloads in this bench (plan B2: synth:worst-case and the
# replay ROMs; cpu_instrs is the only replay ROM the bench runs).
SIGNOFF="worst-case cpu_instrs"

update=0 force=0 absorb=0 fresh=0
if [ "${1:-}" = --fresh ]; then fresh=1; shift; [ "${1:-}" != --update ] || die "--fresh is for check mode, not --update"; fi
if [ "${1:-}" = --update ]; then
  update=1; shift
  while :; do
    case "${1:-}" in
      --force) force=1; shift ;;
      --absorb-regression) absorb=1; shift ;;
      *) break ;;
    esac
  done
  [ "$absorb" = 0 ] || [ "$force" = 1 ] || die "--absorb-regression needs --force"
fi
if [ "$update" = 1 ]; then
  [ $# -eq 3 ] || die "$usage"
  cap=$1 img=$2 out=$3
  if [ -e "$out" ] && [ "$force" = 0 ]; then
    echo "check-cycles.sh: FAIL: $out exists; not overwritten without --force (absorbing a regression needs the human)" >&2
    exit 1
  fi
else
  [ $# -ge 2 ] && [ $# -le 3 ] || die "$usage"
  cap=$1 img=$2 base=${3:-$here/expected-cycles.txt}
fi
[ -f "$cap" ] || die "$cap: no such file"

# Does the capture have QSPI workload lines? (NULs and CRs as the VCP gives them.)
cap_qspi=0
# Not `tr | grep -q`: under pipefail grep's early exit gives tr SIGPIPE (141)
# on a large capture and the match is lost.
grep -qE '^h7bench v1 qspi [^ ]+ calls=' <(tr -d '\000\r' < "$cap") && cap_qspi=1

# --- the baseline (check mode; the old file with --update --force): parsed
# before anything is judged ---------------------------------------------
parse_base() { # parse_base <file>: "H <placement> <hz>" and "L <pass> <workload> <mean> <max>" lines
  awk '
    function bad(m) { printf "check-cycles.sh: %s:%d: %s\n", FILENAME, FNR, m > "/dev/stderr"; err = 1; exit 2 }
    /^[ \t]*(#|$)/ { next }
    $1 == "h7cycles" {
      if (hdr) bad("second header line")
      if ($2 != "v1" || NF != 8) bad("header: want h7cycles v1 image= placement= cpu_hz= commit= image_bytes= image_cksum=")
      split("image placement cpu_hz commit image_bytes image_cksum", k, " ")
      for (i = 1; i <= 6; i++) { if (index($(i + 2), k[i] "=") != 1) bad("header field " i + 2 ": want " k[i] "="); v[k[i]] = substr($(i + 2), length(k[i]) + 2) }
      if (v["placement"] == "" || v["cpu_hz"] !~ /^[0-9]+$/) bad("header: empty placement or non-numeric cpu_hz")
      hdr = 1; print "H", v["placement"], v["cpu_hz"]; next
    }
    $1 == "flash" || $1 == "qspi" {
      if (!hdr) bad("workload line before the header")
      if (NF != 4 || $2 !~ /^[A-Za-z0-9_:.-]+$/ || $3 !~ /^mean=[0-9]+$/ || $4 !~ /^max=[0-9]+$/) bad("want <flash|qspi> <workload> mean=<n> max=<n>")
      key = $1 " " $2; if (key in seen) bad("duplicate line " key); seen[key] = 1
      print "L", $1, $2, substr($3, 6), substr($4, 5); n++; next
    }
    { bad("unrecognized line") }
    END { if (err) exit 2
          if (!n) { printf "check-cycles.sh: %s: no workload lines\n", FILENAME > "/dev/stderr"; exit 2 } }' "$1"
}
if [ "$update" = 0 ]; then
  [ -f "$base" ] || { echo "check-cycles.sh: FAIL: baseline $base: no such file" >&2; exit 1; }
  parsed=$(parse_base "$base") || exit 2
  base_qspi=0
  grep -q '^L qspi ' <<< "$parsed" && base_qspi=1
fi

# --- check-report.sh: the capture is the right image, ungarbled ----------
qflag=()
if [ "$update" = 1 ]; then [ "$cap_qspi" = 1 ] && qflag=(--qspi)
else { [ "$cap_qspi" = 1 ] || [ "$base_qspi" = 1 ]; } && qflag=(--qspi); fi
rc=0
bash "$here/check-report.sh" "$cap" "$img" "${qflag[@]}" || rc=$?
if [ "$rc" -ne 0 ]; then
  echo "check-cycles.sh: check-report.sh failed (exit $rc); cycles not checked" >&2
  exit "$rc"
fi
report=$cap.report

# --- the two last complete reports agree (check-report.sh's extraction) ---
prev=$(mktemp) last=$(mktemp)
trap 'rm -f "$prev" "$last"' EXIT
nrep=$(tr -d '\000\r' < "$cap" | awk -v pf="$prev" -v lf="$last" '
  /^h7bench v1 banner image=bench / { inb = 1; buf = "" }
  inb { buf = buf $0 "\n" }
  inb && /^h7bench v1 end pass=[0-9]+$/ { p = l; l = buf; inb = 0; n++ }
  END { printf "%s", p > pf; printf "%s", l > lf; print n + 0 }')
if [ "$nrep" -lt 2 ]; then
  echo "check-cycles.sh: FAIL: $nrep complete report(s) in $cap; two are needed to check the cycle fields (capture for longer)" >&2
  exit 1
fi
cmp -s "$last" "$report" || die "the last complete report is not the one check-report.sh checked ($report)"
# --fresh: both reports come from the boot that this capture saw run its
# workloads: they start after the last `h7bench v1 start ` line (printed as
# each workload starts, before the first report), so a report the VCP
# buffered from an earlier boot of the same image is never judged.
if [ "$fresh" = 1 ]; then
  read -r last_start prev_banner < <(tr -d '\000\r' < "$cap" | awk '
    /^h7bench v1 start [^ ]+ / { ls = NR }
    /^h7bench v1 banner image=bench / { inb = 1; b = NR }
    inb && /^h7bench v1 end pass=[0-9]+$/ { pb = lb; lb = b; inb = 0 }
    END { print ls + 0, pb + 0 }')
  [ "$last_start" -gt 0 ] || die "--fresh: no 'h7bench v1 start' line: the capture does not show this boot running its workloads"
  [ "$prev_banner" -gt "$last_start" ] ||
    die "--fresh: the last two complete reports do not both follow the last 'start' line (line $last_start): fewer than two reports from this boot (read longer)"
  echo "check-cycles.sh: --fresh: the last two reports follow the boot's last workload start (line $last_start)"
fi
agree=$(awk '
  function field(k,   i, a) { for (i = 1; i <= NF; i++) { split($i, a, "="); if (a[1] == k) return a[2] } return "" }
  function num(v) { return v ~ /^[0-9]+$/ }
  function close1(a, b,   d, m) { d = a - b; if (d < 0) d = -d; m = (a < b) ? a : b; return d * 100 <= m }
  {
    if ($1 == "h7bench" && $3 == "qspi" && $5 ~ /^calls=/) k = "qspi " $4
    else if ($1 == "h7bench" && $4 ~ /^calls=/) k = "flash " $3
    else next
    r = (FILENAME == ARGV[1]) ? "previous" : "last"
    lo = field("min"); me = field("mean"); hi = field("max")
    if (!num(lo) || !num(me) || !num(hi)) { print "FAIL " k ": " r " report: no numeric min= / mean= / max="; next }
    if (!(lo + 0 <= me + 0 && me + 0 <= hi + 0)) print "FAIL " k ": " r " report: not min " lo " <= mean " me " <= max " hi
    rest = $0; gsub(/ (min|mean|max)=[0-9]+/, "", rest)
    if (r == "previous") { if (k in pr) print "FAIL " k ": twice in the previous report"; pr[k] = rest; pl[k] = lo; pm[k] = me; px[k] = hi; next }
    if (k in seen) print "FAIL " k ": twice in the last report"
    seen[k] = 1
    if (!(k in pr)) { print "FAIL " k ": in the last report, not in the previous one"; next }
    if (rest != pr[k]) print "FAIL " k ": fields other than min/mean/max differ between the last two reports"
    if (!close1(lo + 0, pl[k] + 0)) print "FAIL " k ": min " lo " vs " pl[k] " in the previous report (> 1% apart)"
    if (!close1(me + 0, pm[k] + 0)) print "FAIL " k ": mean " me " vs " pm[k] " in the previous report (> 1% apart)"
    if (!close1(hi + 0, px[k] + 0)) print "FAIL " k ": max " hi " vs " px[k] " in the previous report (> 1% apart)"
  }
  END { for (k in pr) if (!(k in seen)) print "FAIL " k ": in the previous report, not in the last one" }' "$prev" "$last")
if [ -n "$agree" ]; then
  sed 's/^FAIL /check-cycles.sh: FAIL: /' <<< "$agree" >&2
  echo "check-cycles.sh: the last two reports disagree or are inconsistent (a byte the VCP dropped?); cycles not checked" >&2
  exit 1
fi
echo "check-cycles.sh: the last two of $nrep complete reports agree (min <= mean <= max, within 1%)"

banner=$(grep -m1 '^h7bench v1 banner image=bench ' "$report")
bfield() { sed -nE "s/.* $1=([^ ]+)( .*|$)/\1/p" <<< "$banner"; }
b_place=$(bfield placement) b_hz=$(bfield cpu_hz) b_bytes=$(bfield image_bytes) b_crc=$(bfield image_cksum)

# The report's workload lines: "<pass> <workload> <mean> <max>", report order.
got=$(awk '
  function field(k,   i, a) { for (i = 1; i <= NF; i++) { split($i, a, "="); if (a[1] == k) return a[2] } return "" }
  $1 == "h7bench" && $3 == "qspi" && $5 ~ /^calls=/ { print "qspi", $4, field("mean"), field("max"); next }
  $1 == "h7bench" && $4 ~ /^calls=/ { print "flash", $3, field("mean"), field("max") }' "$report")
if grep -qvE '^(flash|qspi) [^ ]+ [0-9]+ [0-9]+$' <<< "$got"; then
  die "$report: a workload line without numeric mean= / max="
fi

# --- --update: write the baseline ----------------------------------------
if [ "$update" = 1 ]; then
  [ "$b_hz" = 472000000 ] || { echo "check-cycles.sh: FAIL: banner cpu_hz=$b_hz, the baseline is at 472000000" >&2; exit 1; }
  # --force over an existing baseline: no line may get slower or vanish
  # unless --absorb-regression (the human's approval) says so.
  if [ -e "$out" ]; then
    old=$(parse_base "$out") || exit 2
    worse=$(awk '
      FNR == NR { if ($1 == "L") { k = $2 " " $3; om[k] = $4; ox[k] = $5; order[++n] = k } next }
      { nm[$1 " " $2] = $3; nx[$1 " " $2] = $4 }
      END {
        for (i = 1; i <= n; i++) {
          k = order[i]
          if (!(k in nm)) { print "  " k ": in the old baseline, not in the capture"; continue }
          if (nm[k] > om[k] || nx[k] > ox[k]) printf "  %s: mean %d -> %d, max %d -> %d\n", k, om[k], nm[k], ox[k], nx[k]
        }
      }' <(echo "$old") <(echo "$got"))
    if [ -n "$worse" ]; then
      if [ "$absorb" = 0 ]; then
        echo "check-cycles.sh: FAIL: --force would raise or drop these lines of $out (--absorb-regression, with the human's approval, absorbs them):" >&2
        echo "$worse" >&2
        exit 1
      fi
      echo "check-cycles.sh: WARNING: --absorb-regression: raising or dropping these lines of $out (needs the human's approval; quote it in the commit):" >&2
      echo "$worse" >&2
    fi
  fi
  # The tree the image was built from: HEAD, +dirty for any difference
  # (tracked or untracked) under what goes into the bench image. No
  # override: a recorded commit is always what HEAD was.
  inputs=(../../crates . ../../tools/memory-x ../../perf/itcm-sections.txt ../../.cargo ../../rust-toolchain.toml)
  commit=$(git -C "$here" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)
  if ! git -C "$here" diff --quiet HEAD -- "${inputs[@]}" 2>/dev/null ||
    [ -n "$(git -C "$here" ls-files --others --exclude-standard -- "${inputs[@]}" 2>/dev/null)" ]; then
    commit=$commit+dirty
  fi
  tmpout=$(mktemp "$out.XXXXXX")
  {
    echo "# On-board cycles per frame, the M6 ratchet (check-cycles.sh; gate B2: > +3% fails)."
    echo "# Written by check-cycles.sh --update from $(basename "$cap")."
    echo "# Absorbing a regression into this file needs the human. Measure in the main checkout (ADR 0042)."
    echo "h7cycles v1 image=$(basename "$img") placement=$b_place cpu_hz=$b_hz commit=$commit image_bytes=$b_bytes image_cksum=$b_crc"
    awk '{ printf "%s %s mean=%s max=%s\n", $1, $2, $3, $4 }' <<< "$got"
  } > "$tmpout"
  mv -f "$tmpout" "$out"
  echo "check-cycles.sh: wrote $out ($(wc -l <<< "$got") lines)"
  exit 0
fi

# --- check mode -----------------------------------------------------------
fail=0
bad() { echo "check-cycles.sh: FAIL: $*" >&2; fail=1; }
read -r _ h_place h_hz <<< "$(grep '^H ' <<< "$parsed")"
[ "$b_place" = "$h_place" ] || bad "banner placement=$b_place, baseline placement=$h_place"
[ "$b_hz" = "$h_hz" ] || bad "banner cpu_hz=$b_hz, baseline cpu_hz=$h_hz"

cmp=$(awk -v rt="$REAL_TIME" -v half="$HALF" -v so=" $SIGNOFF " '
  function pct(v, b) { return sprintf("%+.2f%%", (v / b - 1) * 100) }
  FNR == NR { if ($1 == "L") { k = $2 " " $3; bm[k] = $4; bx[k] = $5; border[++nb] = k } next }
  { k = $1 " " $2; gm[k] = $3; gx[k] = $4; gorder[++ng] = k }
  END {
    print "| Line | mean (baseline) | max (baseline) | mean / max vs baseline |"
    for (i = 1; i <= nb; i++) {
      k = border[i]
      if (!(k in gm)) { print "FAIL " k ": no such workload line in the report"; continue }
      printf "| %s | %d (%d) | %d (%d) | %s / %s |\n", k, gm[k], bm[k], gx[k], bx[k], pct(gm[k], bm[k]), pct(gx[k], bx[k])
      if (gm[k] * 100 > bm[k] * 103) print "FAIL " k ": mean " gm[k] " > baseline " bm[k] " + 3% (" pct(gm[k], bm[k]) ")"
      if (gx[k] * 100 > bx[k] * 103) print "FAIL " k ": max " gx[k] " > baseline " bx[k] " + 3% (" pct(gx[k], bx[k]) ")"
    }
    for (i = 1; i <= ng; i++) {
      k = gorder[i]; split(k, p, " ")
      if (!(k in bm)) print "FAIL " k ": in the report but not in the baseline (a new workload needs a baseline line)"
      if (index(so, " " p[2] " ")) {
        if (gx[k] >= rt) print "FAIL " k ": max " gx[k] " cycles per frame >= " rt " (one real-time frame at 472 MHz)"
        else print "real time: " k " max " gx[k] " < " rt
        if (p[2] == "worst-case") printf "REPORT: %s max %d = %.1f%% of %d (the 50%% figure; not a check)\n", k, gx[k], gx[k] / half * 100, half
      }
    }
  }' <(echo "$parsed") <(echo "$got"))
while IFS= read -r line; do
  case "$line" in
    FAIL\ *) bad "${line#FAIL }" ;;
    *) echo "$line" ;;
  esac
done <<< "$cmp"

[ "$fail" -eq 0 ] || { echo "check-cycles.sh: FAILED against $base" >&2; exit 1; }
echo "check-cycles.sh: OK: $(grep -c '^L ' <<< "$parsed") lines within +3% of $base"
