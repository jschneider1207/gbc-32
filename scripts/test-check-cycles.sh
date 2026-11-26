#!/usr/bin/env bash
# Tests tools/h7-bench/check-cycles.sh (the M6 on-board cycle ratchet) on
# small synthetic captures and baselines: pass cases and one case per
# failure mode. No board. Exit 0 = all pass.
# CHECK_CYCLES overrides the script under test (used to mutation-check this
# test); check-report.sh and expected-hashes.txt are taken from its directory.
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${CHECK_CYCLES:-$here/../tools/h7-bench/check-cycles.sh}
exp=$(dirname "$script")/expected-hashes.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

printf 'synthetic image bytes\n' > "$tmp/img.bin"
read -r crc bytes < <(cksum < "$tmp/img.bin")
HZ=472000000
banner() { echo "h7bench v1 banner image=bench placement=${2:-itcm-dtcm} board=NUCLEO-H753ZI clock=pll472 cpu_hz=${1:-$HZ} image_bytes=$bytes image_cksum=$crc"; }
pack="h7bench v1 qspi pack ok bytes=1 same_as_flash=1 "
end="h7bench v1 end pass=1"
# Every workload at mean 1,000,000 / max 2,000,000; `qspi ` prefixed for the QSPI pass.
lines() { sed -E "s/ min=0 mean=0 max=0 / min=1 mean=1000000 max=2000000 /; s/^h7bench v1 /h7bench v1 $1/" "$exp"; }

# report [qspi]: one report's banner and workload lines (no end line).
report() { banner; lines ""; [ "${1:-}" = qspi ] && { echo "$pack"; lines "qspi "; }; }
# cap <file> [qspi] [sed expr on both reports] [on the previous only] [on the last only]:
# two complete reports, as the board repeats them, CRLF + NULs as the VCP gives it.
cap() {
  { printf 'noise\000\n'; report "${2:-}" | sed -E "${4:-}"; echo "h7bench v1 end pass=1"
    report "${2:-}" | sed -E "${5:-}"; echo "h7bench v1 end pass=2"; } |
    sed -E "${3:-}" | sed 's/$/\r/' > "$1"
}
# cap1 <file>: one complete report only.
cap1() { { printf 'noise\000\n'; report; echo "$end"; } | sed 's/$/\r/' > "$1"; }
# base <file> [qspi] [sed expr]: a baseline with every workload at the capture's values.
base() {
  { echo "# test baseline"; echo
    echo "h7cycles v1 image=img.bin placement=itcm-dtcm cpu_hz=$HZ commit=0123abcd image_bytes=$bytes image_cksum=$crc"
    sed -nE 's/^h7bench v1 ([^ ]+) .*/flash \1 mean=1000000 max=2000000/p' "$exp"
    [ "${2:-}" = qspi ] && sed -nE 's/^h7bench v1 ([^ ]+) .*/qspi \1 mean=1000000 max=2000000/p' "$exp"
  } | sed -E "${3:-}" > "$1"
}
# run <name> <want rc> <script args...>
run() {
  local name=$1 want=$2 out rc; shift 2
  out=$(bash "$script" "$@" 2>&1); rc=$?
  if [ $rc -eq "$want" ]; then ok "$name (rc $rc)"; else bad "$name: want rc $want, got $rc: $(echo "$out" | grep -E 'FAIL|check-cycles' | head -3)"; fi
  last=$out
}
C=$tmp/c B=$tmp/b I=$tmp/img.bin

# --- passes ---
cap "$C"; base "$B"; run "flash-only capture equal to baseline passes" 0 "$C" "$I" "$B"
[[ $last == *"REPORT: flash worst-case max 2000000 = 50.6% of 3951279"* ]] && ok "worst-case report line printed" || bad "no worst-case REPORT line"
[[ $last == *"| flash storm-ppu | 1000000 (1000000) | 2000000 (2000000) | +0.00% / +0.00% |"* ]] && ok "delta row printed" || bad "no delta row"
cap "$C" qspi; base "$B" qspi; run "flash + qspi capture equal to baseline passes" 0 "$C" "$I" "$B"
[[ $last == *"REPORT: qspi worst-case"* ]] && ok "qspi worst-case report line printed" || bad "no qspi REPORT line"
cap "$C" "" 's/^(h7bench v1 storm-div .*) mean=1000000 /\1 mean=1030000 /'; base "$B"
run "mean +3.00% exactly passes" 0 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 storm-div .*) max=2000000 /\1 max=2060000 /'; base "$B"
run "max +3.00% exactly passes" 0 "$C" "$I" "$B"
cap "$C" "" 's/ mean=1000000 max=2000000 / mean=500000 max=900000 /'; base "$B"
run "faster on every line passes" 0 "$C" "$I" "$B"
[[ $last == *"-50.00% / -55.00%"* ]] && ok "faster delta reported" || bad "faster delta not reported"
cap "$C" "" 's/^(h7bench v1 worst-case .*) max=2000000 /\1 max=7902556 /'; base "$B" "" 's/^(flash worst-case .*) max=2000000/\1 max=7902556/'
run "worst-case max 7,902,556 (< real time) passes" 0 "$C" "$I" "$B"

# --- check failures: rc 1 ---
cap "$C" "" 's/^(h7bench v1 storm-div .*) mean=1000000 /\1 mean=1030001 /'; base "$B"
run "mean +3.0001% fails" 1 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 storm-div .*) mean=1000000 /\1 mean=1030100 /'; base "$B"
run "mean +3.01% fails" 1 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 storm-div .*) max=2000000 /\1 max=2060200 /'; base "$B"
run "max +3.01% fails" 1 "$C" "$I" "$B"
cap "$C" qspi 's/^(h7bench v1 qspi cpu_instrs .*) max=2000000 /\1 max=2060200 /'; base "$B" qspi
run "qspi max +3.01% fails (flash line unchanged)" 1 "$C" "$I" "$B"
cap "$C"; base "$B" "" '$a flash ghost mean=1 max=1'
run "baseline workload missing from the capture fails" 1 "$C" "$I" "$B"
cap "$C"; base "$B" qspi; run "baseline has qspi lines, capture has none, fails" 1 "$C" "$I" "$B"
cap "$C"; base "$B" "" '/^flash storm-ppu /d'
run "capture workload missing from the baseline fails" 1 "$C" "$I" "$B"
cap "$C" qspi; base "$B"; run "capture qspi pass not in the baseline fails" 1 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 worst-case .*) max=2000000 /\1 max=7902557 /'; base "$B" "" 's/^(flash worst-case .*) max=2000000/\1 max=8000000/'
run "worst-case max 7,902,557 (real time) fails" 1 "$C" "$I" "$B"
cap "$C" qspi 's/^(h7bench v1 qspi worst-case .*) max=2000000 /\1 max=7902557 /'; base "$B" qspi 's/^(qspi worst-case .*) max=2000000/\1 max=8000000/'
run "qspi worst-case max over real time fails" 1 "$C" "$I" "$B"
cap "$C"; base "$B" "" 's/cpu_hz=472000000/cpu_hz=480000000/'; run "baseline cpu_hz differs from the banner fails" 1 "$C" "$I" "$B"
cap "$C"; base "$B" "" 's/placement=itcm-dtcm/placement=flash-axi/'; run "baseline placement differs from the banner fails" 1 "$C" "$I" "$B"
cap "$C"; run "missing baseline file fails" 1 "$C" "$I" "$tmp/nope"

# --- garbled or wrong capture: check-report.sh fails, cycles not judged ---
cap "$C" "" 's/^(h7bench v1 cgb-raster .*hash=)[0-9a-f]/\1z/'; base "$B"
run "garbled hash in the capture: check-report.sh fails" 1 "$C" "$I" "$B"
[[ $last == *"check-report.sh failed (exit 1); cycles not checked"* ]] && ok "check-report failure named" || bad "check-report failure not named"
cap "$C"; printf 'other image\n' > "$tmp/img2.bin"; base "$B"; run "capture of another image fails" 1 "$C" "$tmp/img2.bin" "$B"
cap "$C" "" '/^h7bench v1 end /d'; base "$B"; run "truncated capture (no end line): rc 2" 2 "$C" "$I" "$B"

# --- the last two reports: agreement and min <= mean <= max (M6 review) ---
# A digit the VCP dropped from the last report's max (check-report.sh zeroes it).
cap "$C" "" "" "" 's/^(h7bench v1 worst-case .*) max=2000000 /\1 max=200000 /'; base "$B"
run "a digit dropped from the last report's max fails" 1 "$C" "$I" "$B"
cap "$C" "" "" 's/^(h7bench v1 worst-case .*) max=2000000 /\1 max=200000 /'; base "$B"
run "a digit dropped from the previous report's max fails" 1 "$C" "$I" "$B"
cap1 "$C"; base "$B"; run "one complete report only fails" 1 "$C" "$I" "$B"
[[ $last == *"1 complete report(s)"* ]] && ok "one-report failure named" || bad "one-report failure not named"
cap "$C" "" "" "" 's/^(h7bench v1 storm-div .*) max=2000000 /\1 max=2020000 /'; base "$B"
run "last max 1.00% above the previous passes" 0 "$C" "$I" "$B"
cap "$C" "" "" "" 's/^(h7bench v1 storm-div .*) max=2000000 /\1 max=2020001 /'; base "$B"
run "last max 1.00005% above the previous fails" 1 "$C" "$I" "$B"
cap "$C" "" "" "" 's/^(h7bench v1 storm-div .*) mean=1000000 /\1 mean=989000 /'; base "$B"
run "last mean 1.1% below the previous fails" 1 "$C" "$I" "$B"
cap "$C" "" "" "" 's/^(h7bench v1 storm-div .*) min=1 /\1 min=2 /'; base "$B"
run "last min differing from the previous by > 1% fails" 1 "$C" "$I" "$B"
cap "$C" qspi "" "" 's/^(h7bench v1 qspi storm-div .*) max=2000000 /\1 max=2030000 /'; base "$B" qspi
run "qspi line: last max 1.5% above the previous fails" 1 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 storm-div .*) min=1 /\1 min=1000001 /'; base "$B"
run "min > mean in both reports fails" 1 "$C" "$I" "$B"
cap "$C" "" 's/^(h7bench v1 storm-div .*) mean=1000000 max=2000000 /\1 mean=1000000 max=999999 /'; base "$B"
run "mean > max in both reports fails" 1 "$C" "$I" "$B"
cap "$C" "" "" 's/^(h7bench v1 cgb-raster .*hash=)[0-9a-f]/\1z/'; base "$B"
run "a garbled hash in the previous report only fails" 1 "$C" "$I" "$B"
cap "$C" "" "" 's/^(h7bench v1 cgb-raster calls=)[0-9]/\19/'; base "$B"
run "a calls= field differing in the previous report fails" 1 "$C" "$I" "$B"
cap "$C" "" "" '/^h7bench v1 storm-ppu /d'; base "$B"
run "a workload line missing from the previous report fails" 1 "$C" "$I" "$B"
cap "$C" "" "" 's/^(h7bench v1 storm-ppu .*) max=[0-9]+ /\1 /'; base "$B"
run "no max= in a previous-report line fails" 1 "$C" "$I" "$B"

# --- malformed baseline / usage: rc 2 ---
cap "$C"
base "$B" "" '$a flash ghost mean=1x max=1'; run "non-numeric mean in the baseline: rc 2" 2 "$C" "$I" "$B"
base "$B" "" '$a junk line'; run "unrecognized baseline line: rc 2" 2 "$C" "$I" "$B"
base "$B" "" '/^h7cycles /d'; run "baseline without header: rc 2" 2 "$C" "$I" "$B"
base "$B" "" 's/ commit=0123abcd//'; run "baseline header missing a field: rc 2" 2 "$C" "$I" "$B"
base "$B" "" 's/ commit=/ comit=/'; run "baseline header with a misnamed field: rc 2" 2 "$C" "$I" "$B"
base "$B" "" '$a flash worst-case mean=1 max=1'; run "duplicate baseline line: rc 2" 2 "$C" "$I" "$B"
base "$B" "" '/^flash /d'; run "baseline with no workload lines: rc 2" 2 "$C" "$I" "$B"
run "no arguments: rc 2" 2
run "missing capture: rc 2" 2 "$tmp/nope" "$I" "$B"

# --- --update ---
O=$tmp/out.txt
cap "$C" qspi; H7_COMMIT=feedc0de run "--update writes a baseline" 0 --update "$C" "$I" "$O"
grep -q "^h7cycles v1 image=img.bin placement=itcm-dtcm cpu_hz=$HZ commit=feedc0de image_bytes=$bytes image_cksum=$crc\$" "$O" && ok "--update header" || bad "--update header: $(grep h7cycles "$O")"
[ "$(grep -c '^flash ' "$O")" = "$(wc -l < "$exp")" ] && [ "$(grep -c '^qspi ' "$O")" = "$(wc -l < "$exp")" ] && ok "--update has every flash and qspi line" || bad "--update line count"
run "the written baseline passes against its capture" 0 "$C" "$I" "$O"
cp "$O" "$tmp/out.keep"; cap "$C" qspi 's/ mean=1000000 / mean=900000 /'
run "--update refuses an existing file" 1 --update "$C" "$I" "$O"
cmp -s "$O" "$tmp/out.keep" && ok "existing file unchanged" || bad "--update changed an existing file"
run "--update --force overwrites" 0 --update --force "$C" "$I" "$O"
grep -q ' mean=900000 ' "$O" && ok "--force wrote the new values" || bad "--force did not write"
rm -f "$O"; cap "$C" "" 's/^(h7bench v1 cgb-raster .*hash=)[0-9a-f]/\1z/'
run "--update on a garbled capture refuses" 1 --update "$C" "$I" "$O"
[ ! -e "$O" ] && ok "no file written from a garbled capture" || bad "file written from a garbled capture"
cap "$C" qspi 's/^(h7bench v1 qspi cgb-raster .*hash=)[0-9a-f]/\1z/'
run "--update on a capture with a garbled qspi line refuses" 1 --update "$C" "$I" "$O"
# A capture as big as a real one (~230 KB): the QSPI-line probe must not lose
# the match to SIGPIPE under pipefail (grep -q exits early, tr gets 141).
yes '' | head -c 400000 >> "$C"
run "--update on a large capture with a garbled qspi line refuses" 1 --update "$C" "$I" "$O"
[ ! -e "$O" ] && ok "no file written from a large garbled-qspi capture" || bad "file written from a large garbled-qspi capture"
cap "$C" "" 's/cpu_hz=472000000/cpu_hz=480000000/'
run "--update refuses a non-472 MHz banner" 1 --update "$C" "$I" "$O"
[[ $last == *"banner cpu_hz=480000000"* ]] && [ ! -e "$O" ] && ok "no file written at 480 MHz" || bad "file written at 480 MHz, or not refused for the clock"
cap "$C" "" "" "" 's/^(h7bench v1 worst-case .*) max=2000000 /\1 max=200000 /'
run "--update refuses a capture whose last two reports disagree" 1 --update "$C" "$I" "$O"
[ ! -e "$O" ] && ok "no file written from disagreeing reports" || bad "file written from disagreeing reports"

[ $fails -eq 0 ] && echo "all check-cycles tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
