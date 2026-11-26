#!/usr/bin/env bash
# Checks that the board run lists say what the human-owned Stage 2 manifest
# says (gate step B3; acceptance/stage2.toml "Consumers": until a tool reads
# the manifest, replay-set.txt and soak-set.txt carry the same entries and
# must be kept equal to it):
#
#   scripts/check-stage2-sets.sh [<stage2.toml> <replay-set.txt> <soak-set.txt> <stage1.toml> <check-cycles.sh>]
#
# Defaults: acceptance/stage2.toml, crates/gbc-nucleo-h753/replay-set.txt
# and soak-set.txt, acceptance/stage1.toml, tools/h7-bench/check-cycles.sh.
#
# Checks (exit 1 on any difference):
#   - `replay = [...]`: each entry `{ rom = "<path>", run_frames = <n> }`
#     is the set line `<path> <n>`, `{ rom = "<path>", input_group = "<g>" }`
#     is `<path> - <g>` (with both keys: `<path> <n> <g>`); the list equals
#     replay-set.txt's entries, in order, one for one;
#   - `soak = [...]`: `{ synth = "<name>", run_frames = <n>, max_ppm = <p> }`
#     is `synth:<name> <n> max_ppm=<p>`; the list equals soak-set.txt's;
#   - soak run_frames is soak_minutes of emulated time: the least frame
#     count (70,224 clocks at 4,194,304 Hz) that covers soak_minutes * 60 s;
#   - cpu_mhz = 472; frame_budget_cycles is cpu_mhz * 1e6 * 70224 / 4194304
#     rounded to the nearest integer, and worst_case_report_cycles half of
#     that, rounded; they equal check-cycles.sh's REAL_TIME= and HALF=;
#   - roms_release equals stage1.toml's;
#   - `save_roundtrip = { fixture = "<path>", rom = "generated:link-probe" }`
#     (the one ROM run-player-save.sh runs), the fixture a file under the
#     repository root. On success the last line is `save_fixture=<path>`
#     (the gate passes it to run-player-save.sh).
# The set files: `#` comments and blank lines skipped, fields split on
# white space and rejoined with one space.
#
# The manifest is parsed for exactly the forms above: a key = value line
# (string, integer) or an array of one-line inline tables. A line it
# cannot parse, a missing or repeated key, an unknown key in an entry, or
# an empty list is exit 2, never a pass.
#
# Exit: 0 equal, 1 a difference, 2 a parse or usage error.
set -euo pipefail
root=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
die() { echo "check-stage2-sets.sh: $*" >&2; exit 2; }
[ $# -eq 0 ] || [ $# -eq 5 ] || die "usage: check-stage2-sets.sh [<stage2.toml> <replay-set.txt> <soak-set.txt> <stage1.toml> <check-cycles.sh>]"
toml=${1:-$root/acceptance/stage2.toml}
replay=${2:-$root/crates/gbc-nucleo-h753/replay-set.txt}
soak=${3:-$root/crates/gbc-nucleo-h753/soak-set.txt}
s1=${4:-$root/acceptance/stage1.toml}
cc=${5:-$root/tools/h7-bench/check-cycles.sh}
for f in "$toml" "$replay" "$soak" "$s1" "$cc"; do [ -f "$f" ] || die "$f: no such file"; done

# parse <toml>: "K <key> <value>" for top-level keys (strings unquoted),
# "R <set line>" per replay entry, "S <set line>" per soak entry. Exit 2 on
# anything else.
parse() {
  awk '
    function bad(m) { printf "check-stage2-sets.sh: %s:%d: %s: %s\n", FILENAME, FNR, m, raw > "/dev/stderr"; err = 1; exit 2 }
    # value(v): a "string" (no quotes or backslashes inside) or an integer; else bad.
    function value(v) {
      if (v ~ /^"[^"\\]*"$/) return substr(v, 2, length(v) - 2)
      if (v ~ /^[0-9]+$/) return v
      bad("not a string or an integer: " v)
    }
    function entry(s, kind,   n, i, kv, k, v, seen, line, a) {
      if (s !~ /^\{.*\},?$/) bad("not a one-line inline table")
      sub(/,$/, "", s); s = substr(s, 2, length(s) - 2)
      n = split(s, a, ",")
      for (i = 1; i <= n; i++) {
        kv = a[i]; gsub(/^[ \t]+|[ \t]+$/, "", kv)
        if (kv !~ /^[a-z_]+[ \t]*=[ \t]*[^ \t].*$/) bad("not key = value: " kv)
        k = kv; sub(/[ \t]*=.*/, "", k); v = kv; sub(/^[^=]*=[ \t]*/, "", v)
        if (k in seen) bad("repeated key " k)
        seen[k] = value(v)
      }
      if (kind == "V") {
        for (k in seen) if (k != "fixture" && k != "rom") bad("unknown key in save_roundtrip: " k)
        if (!("fixture" in seen) || !("rom" in seen)) bad("save_roundtrip needs fixture and rom")
        print "V", seen["fixture"], seen["rom"]; count[kind]++; return
      }
      if (kind == "R") {
        for (k in seen) if (k != "rom" && k != "run_frames" && k != "input_group") bad("unknown key in a replay entry: " k)
        if (!("rom" in seen)) bad("replay entry without rom")
        if (seen["rom"] !~ /^[A-Za-z0-9_.\/-]+$/) bad("rom path with unexpected characters")
        if ("run_frames" in seen && seen["run_frames"] !~ /^[0-9]+$/) bad("run_frames is not an integer")
        if (!("run_frames" in seen) && !("input_group" in seen)) bad("replay entry without run_frames or input_group")
        line = seen["rom"] " " (("run_frames" in seen) ? seen["run_frames"] : "-")
        if ("input_group" in seen) line = line " " seen["input_group"]
      } else {
        for (k in seen) if (k != "synth" && k != "run_frames" && k != "max_ppm") bad("unknown key in a soak entry: " k)
        if (!("synth" in seen) || !("run_frames" in seen) || !("max_ppm" in seen)) bad("soak entry needs synth, run_frames and max_ppm")
        if (seen["run_frames"] !~ /^[0-9]+$/ || seen["max_ppm"] !~ /^[0-9]+$/) bad("run_frames and max_ppm must be integers")
        line = "synth:" seen["synth"] " " seen["run_frames"] " max_ppm=" seen["max_ppm"]
      }
      print kind, line; count[kind]++
    }
    {
      raw = $0; s = $0
      sub(/#.*/, "", s); gsub(/^[ \t]+|[ \t]+$/, "", s)
      if (s == "") next
      if (inarr) {
        if (s == "]") { inarr = ""; next }
        entry(s, inarr); next
      }
      if (s ~ /^(replay|soak)[ \t]*=[ \t]*\[$/) {
        k = s; sub(/[ \t]*=.*/, "", k)
        if (k in top) bad("repeated key " k)
        top[k] = 1; inarr = (k == "replay") ? "R" : "S"; next
      }
      if (s ~ /^[a-z_]+[ \t]*=/) {
        k = s; sub(/[ \t]*=.*/, "", k); v = s; sub(/^[^=]*=[ \t]*/, "", v)
        if (k in top) bad("repeated key " k)
        if (k == "save_roundtrip") { top[k] = 1; entry(v, "V"); next }
        top[k] = 1; print "K", k, (v ~ /^"/ ? "S" : "I"), value(v); next
      }
      bad("unrecognized line")
    }
    END {
      if (err) exit 2
      if (inarr) { raw = "(end of file)"; bad("unterminated array") }
      if (!count["R"] || !count["S"]) { printf "check-stage2-sets.sh: %s: empty or missing replay or soak list\n", FILENAME > "/dev/stderr"; exit 2 }
    }' "$1"
}
parsed=$(parse "$toml") || exit 2
key() { # key <name>: the top-level value, which must be there
  local v
  v=$(awk -v k="$1" '$1 == "K" && $2 == k { $1 = ""; $2 = ""; sub(/^  /, ""); print; n++ } END { exit n == 1 ? 0 : 1 }' <<< "$parsed") ||
    die "$toml: no top-level $1 ="
  printf '%s\n' "$v"
}
# set_lines <file>: entries, comments and blank lines dropped, spaces normalized.
set_lines() { sed 's/#.*//' "$1" | awk 'NF { $1 = $1; print }'; }

fail=0
bad() { echo "check-stage2-sets.sh: FAIL: $*" >&2; fail=1; }
cmp_list() { # cmp_list <R|S> <set file> <label>
  local want got
  want=$(sed -n "s/^$1 //p" <<< "$parsed")
  got=$(set_lines "$2")
  if [ "$want" = "$got" ]; then
    echo "check-stage2-sets.sh: $3: $(wc -l <<< "$want") entries, $2 equals $toml"
  else
    bad "$3: $2 differs from $toml (manifest < > set file):"
    diff <(echo "$want") <(echo "$got") | sed 's/^/  /' >&2 || true
  fi
}
cmp_list R "$replay" replay
cmp_list S "$soak" soak

int() { case "$2" in "I "[0-9]*) ;; *) die "$toml: $1 = '${2#? }' is not an integer";; esac; }
mhz=$(key cpu_mhz); int cpu_mhz "$mhz"; mhz=${mhz#I }
budget=$(key frame_budget_cycles); int frame_budget_cycles "$budget"; budget=${budget#I }
half=$(key worst_case_report_cycles); int worst_case_report_cycles "$half"; half=${half#I }
minutes=$(key soak_minutes); int soak_minutes "$minutes"; minutes=${minutes#I }
[ "$mhz" = 472 ] || bad "cpu_mhz = $mhz; the board and every figure here are at 472 MHz (plan D11)"
# round(a / b) = (2a + b) / (2b), in integers.
num=$((mhz * 1000000 * 70224)) den=4194304
want_budget=$(((2 * num + den) / (2 * den)))
want_half=$(((num + den) / (2 * den)))
[ "$budget" = "$want_budget" ] || bad "frame_budget_cycles = $budget; one frame at $mhz MHz is $want_budget"
[ "$half" = "$want_half" ] || bad "worst_case_report_cycles = $half; half a frame at $mhz MHz is $want_half"
cc_rt=$(sed -n 's/^REAL_TIME=\([0-9]*\).*/\1/p' "$cc") cc_half=$(sed -n 's/^HALF=\([0-9]*\).*/\1/p' "$cc")
[ -n "$cc_rt" ] && [ -n "$cc_half" ] || die "$cc: no REAL_TIME= or HALF= line"
[ "$cc_rt" = "$budget" ] || bad "$cc REAL_TIME=$cc_rt, manifest frame_budget_cycles = $budget"
[ "$cc_half" = "$half" ] || bad "$cc HALF=$cc_half, manifest worst_case_report_cycles = $half"
# The soak's frames: ceil(minutes * 60 * 4194304 / 70224).
clocks=$((minutes * 60 * 4194304))
want_frames=$(((clocks + 70224 - 1) / 70224))
while read -r _ name frames _; do
  [ "$frames" = "$want_frames" ] || bad "soak $name run_frames = $frames; $minutes minutes (soak_minutes) is $want_frames frames"
done < <(grep '^S ' <<< "$parsed")
rel=$(key roms_release); case "$rel" in "S "*) rel=${rel#S } ;; *) die "$toml: roms_release is not a string";; esac
rel1=$(sed -n 's/^roms_release[ \t]*=[ \t]*"\([^"]*\)".*/\1/p' "$s1")
[ -n "$rel1" ] || die "$s1: no roms_release"
[ "$rel" = "$rel1" ] || bad "roms_release = $rel, $s1 has $rel1"

# The save round trip: run-player-save.sh runs only the generated link-probe
# ROM; the fixture (relative to the repository root) must exist.
nv=$(grep -c '^V ' <<< "$parsed" || true)
[ "$nv" = 1 ] || die "$toml: want one save_roundtrip = { fixture = ..., rom = ... }, found $nv"
read -r _ fixture srom <<< "$(grep '^V ' <<< "$parsed")"
[ "$srom" = generated:link-probe ] || bad "save_roundtrip rom = $srom; run-player-save.sh runs generated:link-probe only"
case "$fixture" in /* | *..*) bad "save_roundtrip fixture = $fixture: want a path inside the repository" ;; esac
[ -f "$root/$fixture" ] || bad "save_roundtrip fixture $fixture: no such file under $root"

[ "$fail" = 0 ] || { echo "check-stage2-sets.sh: FAILED" >&2; exit 1; }
echo "check-stage2-sets.sh: OK (cpu_mhz $mhz, frame budget $budget, report line $half, soak $want_frames frames = $minutes min, roms $rel)"
echo "save_fixture=$fixture"
