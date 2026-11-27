#!/usr/bin/env bash
# Tests crates/gbc-nucleo-h753/run-replay.sh (the Stage 2 M5 replay
# checker) on captures built from a real board report committed in
# docs/reference/m5-evidence/ (rtc3test-sub-second, a clock-table entry,
# from the player-script image with the panel readback samples).
# RUN_REPLAY_CAPTURES is always set, so nothing builds, flashes, reads the
# port, runs a host tool or takes the board lock. The image is a small
# synthetic file; the report's image fields are rewritten to match it (as
# scripts/test-run-paced.sh). Every exit-1 path of run-replay.sh's own
# checks and its exit-2 paths are asserted by exact exit code, plus one
# run-paced.sh exit 1 (a doctored paced line) and exit 2 (no report)
# passed through. No board. Exit 0 = all pass.
# RUN_REPLAY overrides the script under test (used to mutation-check this test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${RUN_REPLAY:-$here/../crates/gbc-nucleo-h753/run-replay.sh}
real=$here/../docs/reference/m5-evidence/report-rtc3test-sub-second-readback.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GBC_BOARD_LOCK=$tmp/lock
unset RUN_REPLAY_ONLY RUN_REPLAY_OUT RUN_REPLAY_RUN_PACED RUN_REPLAY_MAX_PPM
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
P=gbc-nucleo-h753
C=$tmp/caps
mkdir -p "$C"

printf 'synthetic player-script image\n' > "$tmp/img.bin"
read -r icrc ibytes < <(cksum < "$tmp/img.bin")
imgf="image_bytes=$ibytes image_cksum=$icrc"

head -n -1 "$real" | tail -n +2 > "$tmp/body"
grep '^paced ' "$tmp/body" > "$tmp/host"
[ "$(wc -l < "$tmp/host")" = 3 ] || { echo "FAIL: committed report lacks 3 paced lines"; exit 1; }
frames=$(sed -n 's/^paced v1 .* frames=\([0-9]*\)$/\1/p' "$tmp/host")

# rep <body>: one complete report on stdout.
rep() {
  local crc bytes
  read -r crc bytes < <(cksum < "$1")
  echo "$P paced report n=1 rom=qspi frames=$frames $imgf"
  cat "$1"
  echo "$P paced report-end n=1 rom=qspi bytes=$bytes cksum=$crc overflow=0"
}
# entry <name> <body>: the capture files for one entry.
entry() {
  cp "$tmp/img.bin" "$C/$1.bin"
  cp "$tmp/host" "$C/$1.host"
  rep "$2" > "$C/$1.serial"
}
# run <name> <want rc> <list text> [pattern in output]
run() {
  local name=$1 want=$2 pat=${4:-} out rc
  printf '%s\n' "$3" > "$tmp/list"
  out=$(RUN_REPLAY_CAPTURES=$C bash "$script" "$tmp/list" 2>&1); rc=$?
  if [ $rc -ne "$want" ]; then bad "$name: want rc $want, got $rc: $(echo "$out" | tail -4 | tr '\n' '|')"
  elif [ -n "$pat" ] && ! grep -qE -- "$pat" <<< "$out"; then bad "$name: rc $rc but output lacks /$pat/"
  else ok "$name (rc $rc)"; fi
}
CLOCKED="rtc3test/rtc3test.gb - g"
PLAIN="rtc3test/rtc3test.gb $frames"
# mut <name> <want rc> <sed expr> [pattern] [list]: entry g (or the list's) with one edit.
mut() {
  sed -E "$3" "$tmp/body" > "$tmp/b2"
  if cmp -s "$tmp/body" "$tmp/b2"; then bad "$1: the edit changed nothing"; return; fi
  entry g "$tmp/b2"; entry rtc3test "$tmp/b2"; run "$1" "$2" "${5:-$CLOCKED}" "${4:-}"
}

entry g "$tmp/body"
run "real report passes" 0 "$CLOCKED" 'g: PASS equal=yes underruns=0 over_budget=0 core_max=[0-9]+ readbacks=[1-9][0-9]* mismatches=0 input=6/6 late=0'

# Exit 1: a failed check.
mut "doctored frame chain (run-paced.sh exit 1)" 1 's/^(paced end .* fbs=)[0-9a-f]/\1x/' 'g: FAIL equal=no'
mut "a late change" 1 's/^(stats input .*) late=0/\1 late=1/' 'CHECK FAILED: g: stats input late=1'
mut "not every change handed" 1 's/^(stats input .*) handed=6/\1 handed=5/' 'handed=5 of clock_changes=6'
mut "a frame over budget" 1 's/ total_over_budget=0 / total_over_budget=1 /' 'total_over_budget=1'
# The panel readback (plan B3: 0 mismatches).
mut "readback mismatches" 1 's/^(stats display readback .*) mismatches=0 /\1 mismatches=3 /' 'CHECK FAILED: g: stats display readback mismatches=3 \(want 0\)'
mut "no readback done" 1 's/^(stats display readback .*) readbacks=[0-9]+ /\1 readbacks=0 /' 'CHECK FAILED: g: stats display readback readbacks=0'
mut "control readback 0" 1 's/ control_mismatches=2 / control_mismatches=0 /' 'control_mismatches=0 \(want 2\)'
mut "no control readback" 1 's/ control_mismatches=2 / control_mismatches=none /' 'control_mismatches=none \(want 2\)'

# Exit 2: a missing, repeated or malformed line.
mut "no stats input line" 2 '/^stats input /d' "want exactly one 'stats input' line"
mut "two stats input lines" 2 's/^(stats input .*)$/\1\n\1/' "want exactly one 'stats input' line"
mut "no stats player line" 2 '/^stats player /d' "want exactly one 'stats player' line"
mut "malformed late" 2 's/^(stats input .*) late=0/\1 late=x/' "'late=x' is malformed"
mut "late twice" 2 's/^(stats input .*) late=0/\1 late=0 late=0/' "'late=' appears 2 times"
mut "no total_over_budget" 2 's/ total_over_budget=0 / /' "'total_over_budget=' appears 0 times"
mut "no core max" 2 's/^(stats paced core .*) max=[0-9]+/\1/' "'max=' appears 0 times"
mut "no readback line" 2 '/^stats display readback /d' "want exactly one 'stats display readback' line"
mut "two readback lines" 2 's/^(stats display readback .*)$/\1\n\1/' "want exactly one 'stats display readback' line"
mut "no readbacks field" 2 's/^(stats display readback .*) readbacks=[0-9]+ /\1 /' "'readbacks=' appears 0 times"
mut "no mismatches field" 2 's/^(stats display readback .*) mismatches=0 /\1 /' "'mismatches=' appears 0 times"
mut "malformed mismatches" 2 's/^(stats display readback .*) mismatches=0 /\1 mismatches=-1 /' "'mismatches=-1' is malformed"
mut "no control field" 2 's/ control_mismatches=2 / /' "'control_mismatches=' appears 0 times"
mut "malformed control" 2 's/ control_mismatches=2 / control_mismatches=2x /' "'control_mismatches=2x' is malformed"
mut "run-paced.sh exit 2 with every line present" 2 's/^(stats dac .*) late=0/\1 late=x/' 'g: TOOL ERROR'
sed -E '/^stats input /d' "$tmp/body" > "$tmp/b3"
entry rtc3test "$tmp/body"
run "stats input on an entry without a table" 2 "$PLAIN" "'stats input' line without a clock table"
entry rtc3test "$tmp/b3"
run "an entry without a table passes without stats input" 0 "$PLAIN" 'rtc3test: PASS equal=yes'
: > "$C/g.serial"
run "no report (run-paced.sh exit 2)" 2 "$CLOCKED" 'g: TOOL ERROR'
run "missing capture file" 2 "x/missing.gb 600" 'no such file'
run "empty list" 2 "# nothing" 'no entries selected'

# The worst entry wins: a failing and a passing entry is 1, a tool error 2.
entry g "$tmp/body"
sed -E 's/^(stats input .*) late=0/\1 late=1/' "$tmp/body" > "$tmp/b4"
entry h "$tmp/b4"
run "pass + fail = 1" 1 "$CLOCKED
rtc3test/rtc3test.gb - h" 'h: FAIL'
: > "$C/g.serial"
run "fail + tool error = 2" 2 "rtc3test/rtc3test.gb - h
$CLOCKED" 'g: TOOL ERROR'
entry g "$tmp/body"
RUN_REPLAY_ONLY=g
export RUN_REPLAY_ONLY
run "RUN_REPLAY_ONLY selects" 0 "$CLOCKED
rtc3test/rtc3test.gb - h" 'PASS \(1 entries\)'
unset RUN_REPLAY_ONLY
# A checker that reads stdin must not eat the rest of the list (the first
# board run lost 5 of 6 entries that way).
entry g "$tmp/body"; entry h "$tmp/body"
printf '#!/usr/bin/env bash\ncat > /dev/null\nexec bash "%s" "$@"\n' "$here/../crates/gbc-nucleo-h753/run-paced.sh" > "$tmp/eat-stdin.sh"
RUN_REPLAY_RUN_PACED=$tmp/eat-stdin.sh
export RUN_REPLAY_RUN_PACED
run "a checker reading stdin sees no list" 0 "$CLOCKED
rtc3test/rtc3test.gb - h" 'PASS \(2 entries\)'
unset RUN_REPLAY_RUN_PACED
# A non-numeric frame count is a tool error, never a skipped entry.
run "bad frames" 2 "rtc3test/rtc3test.gb 6x0 g" "is not a number"

# The speed bound (the M5 soak runs with RUN_REPLAY_MAX_PPM=1000): the
# committed 30 s report has ppm=+1116, inside the default (lifted) bound.
entry g "$tmp/body"
RUN_REPLAY_MAX_PPM=1000
export RUN_REPLAY_MAX_PPM
run "RUN_REPLAY_MAX_PPM=1000 fails ppm=+1116" 1 "$CLOCKED" 'g: FAIL'
RUN_REPLAY_MAX_PPM=1200
run "RUN_REPLAY_MAX_PPM=1200 passes ppm=+1116" 0 "$CLOCKED" 'g: PASS'
RUN_REPLAY_MAX_PPM=1e3
run "a malformed RUN_REPLAY_MAX_PPM" 2 "$CLOCKED" 'is not a number'
unset RUN_REPLAY_MAX_PPM
# A synthetic ROM entry (soak-set.txt) is named after the ROM, takes no group.
entry worst-case "$tmp/b3"
run "a synth: entry" 0 "synth:worst-case $frames" 'worst-case: PASS equal=yes'
run "a synth: entry with a group" 2 "synth:worst-case - g" 'takes a frame count and no group'

# run-paced.sh exits other than 0/1 are tool errors even when every line is
# present and fine (review T-1: 127, 137, 124 gave PASS exit 0).
entry g "$tmp/body"
for code in 3 137; do
  printf '#!/usr/bin/env bash\nbash "%s" "$@"\nexit %s\n' "$here/../crates/gbc-nucleo-h753/run-paced.sh" "$code" > "$tmp/stub$code.sh"
  RUN_REPLAY_RUN_PACED=$tmp/stub$code.sh
  export RUN_REPLAY_RUN_PACED
  run "checker exits $code after good lines" 2 "$CLOCKED" 'g: TOOL ERROR'
done
unset RUN_REPLAY_RUN_PACED
# A per-entry max_ppm= bound (review T-3); the env can only tighten it.
run "max_ppm=1000 on the entry fails ppm=+1116" 1 "$CLOCKED max_ppm=1000" 'g: FAIL'
run "max_ppm=1200 on the entry passes" 0 "$CLOCKED max_ppm=1200" 'g: PASS'
RUN_REPLAY_MAX_PPM=999999
export RUN_REPLAY_MAX_PPM
run "env cannot loosen the entry's max_ppm=1000" 1 "$CLOCKED max_ppm=1000" 'g: FAIL'
RUN_REPLAY_MAX_PPM=1000
run "env tightens an entry's max_ppm=1200" 1 "$CLOCKED max_ppm=1200" 'g: FAIL'
unset RUN_REPLAY_MAX_PPM
run "max_ppm on a plain entry" 0 "$PLAIN max_ppm=1200" 'rtc3test: PASS'
run "malformed max_ppm=" 2 "$CLOCKED max_ppm=1e3" 'is not a number'
# An explicit frame count on a group must equal the table's end (T-7).
run "group with an explicit frame count that differs" 2 "rtc3test/rtc3test.gb $((frames + 1)) g" "differs from the table's end"
run "group with the explicit frame count equal" 0 "rtc3test/rtc3test.gb $frames g" 'g: PASS'
if [ "$fails" = 0 ]; then echo "test-run-replay.sh: all passed"; exit 0; fi
echo "test-run-replay.sh: $fails FAILED"
exit 1
