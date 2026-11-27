#!/usr/bin/env bash
# Tests the Stage 2 gate's own verdicts on its board steps (B1-B5; M7
# review T11): scripts/lib/gate-board.sh (board_rc's exit mapping, the
# board presence probe's parsing, and the checks on what run-qspi.sh,
# check-cycles.sh, run-headless.sh, check-stage2-sets.sh, run-replay.sh,
# run-paced.sh and run-player-save.sh print), run with the gate's own
# fail / tool_error / finish (taken from scripts/gate-stage2.sh, so
# "a check failed" is exit 1 and a tool error exit 2 as the gate exits),
# on canned good and bad output. Also: the gate sources the lib and calls
# each verdict, and its argument errors are exit 2. No board, no build, no
# lock. Exit 0 = all pass.
#   GATE_BOARD_LIB  the lib under test     (used to mutation-check this test)
#   GATE_STAGE2     the gate under test    (likewise)
set -u
here=$(cd "$(dirname "$0")" && pwd)
lib=${GATE_BOARD_LIB:-$here/lib/gate-board.sh}
gate=${GATE_STAGE2:-$here/gate-stage2.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

# fn_src <name>: the gate's definition of a top-level function (a one-line
# `name() { ...; }` or a block up to the next line that is `}`).
fn_src() {
  awk -v n="$1" '
    !on && index($0, n "() {") == 1 { on = 1; print; if ($0 ~ /}$/) exit; next }
    on { print; if ($0 == "}") exit }' "$gate"
}
defs=""
for f in finish fail tool_error; do
  src=$(fn_src "$f")
  [ -n "$src" ] || { echo "FAIL: no $f() in $gate"; exit 1; }
  defs+="$src"$'\n'
done

# run <name> <want rc> <command...>: the command in a subshell as the gate
# runs (set -euo pipefail, the gate's fail/tool_error, the lib). Want 0: it
# returns, and the shell goes on; 1: GATE FAILED, exit 1; 2: GATE ERROR,
# exit 2.
run() {
  local name=$1 want=$2 rc
  shift 2
  out=$( (
    set -euo pipefail
    host_only=0 partial=0 current=B0 _finished=0
    drive=E: label=NOD_H753ZI port=COM5
    eval "${defs}"
    # shellcheck source=lib/gate-board.sh
    . "${lib}"
    "$@"
    echo "RETURNED"
  ) 2>&1)
  rc=$?
  local tag
  case $want in 0) tag=RETURNED ;; 1) tag="GATE FAILED (B0)" ;; *) tag="GATE ERROR (B0)" ;; esac
  if [ "$rc" = "$want" ] && [[ "$out" == *"$tag"* ]]; then
    ok "$name (rc $rc)"
  else
    bad "$name: want rc $want ($tag), got $rc: $(head -c 300 <<< "$out" | tr '\n' ' ')"
  fi
}

# --- board_rc: a board script's exit status ---------------------------------
run "board_rc 0 goes on" 0 board_rc "x.sh" 0
run "board_rc 1 is a failure" 1 board_rc "x.sh" 1
[[ "$out" == *"x.sh: a check failed (exit 1)"* ]] && ok "board_rc 1 names the script" || bad "board_rc 1 message: $out"
run "board_rc 2 is a tool error" 2 board_rc "x.sh" 2
run "board_rc 3 is a tool error (never a partial pass)" 2 board_rc "x.sh" 3
run "board_rc 127 is a tool error" 2 board_rc "x.sh" 127

# --- board_absent: the presence probe's parsing ------------------------------
# probe <vol> <details> <ports>: board_absent on those answers; prints
# PRESENT or ABSENT: <why>.
probe() {
  T_VOL=$1 T_DET=$2 T_PORTS=$3
  board_vol() { printf '%s\n' "$T_VOL"; }
  board_details() { printf '%s\n' "$T_DET"; }
  board_ports() { printf '%s\n' "$T_PORTS"; }
  local why
  if why=$(board_absent); then echo "ABSENT: $why"; else echo "PRESENT[$why]"; fi
}
VOL=" Volume in drive E is NOD_H753ZI
 Volume Serial Number is 1234-ABCD"
PORTS="COM3
COM5"
expect() { # expect <name> <want substring> -- run's output
  [[ "$out" == *"$2"* ]] && ok "$1" || bad "$1: want '$2' in: $(tr '\n' ' ' <<< "$out")"
}
run "probe: board there" 0 probe "$VOL" yes "$PORTS"; expect "probe: board there is PRESENT, silent" "PRESENT[]"
run "probe: wrong label" 0 probe " Volume in drive E is USB_STICK" yes "$PORTS"
expect "probe: wrong label is absent" "ABSENT: volume E: is not labelled NOD_H753ZI ( Volume in drive E is USB_STICK)"
run "probe: no answer" 0 probe "" yes "$PORTS"; expect "probe: no answer is absent" "ABSENT: volume E: is not labelled NOD_H753ZI (no answer)"
run "probe: no DETAILS.TXT" 0 probe "$VOL" "" "$PORTS"; expect "probe: no DETAILS.TXT is absent" "ABSENT: E: has no DETAILS.TXT"
run "probe: port missing" 0 probe "$VOL" yes "COM3"; expect "probe: port missing is absent" "ABSENT: serial port COM5 is not present (ports: COM3 )"
run "probe: COM50 is not COM5" 0 probe "$VOL" yes "COM50
XCOM5"
expect "probe: COM50 / XCOM5 are not COM5" "ABSENT: serial port COM5"
run "probe: no ports" 0 probe "$VOL" yes ""; expect "probe: no ports is absent" "ABSENT: serial port COM5"

# --- sets_verdict: check-stage2-sets.sh's save_fixture= -----------------------
L=$tmp/log
printf 'check-stage2-sets.sh: OK\nsave_fixture=test-roms/fixture.sav\n' > "$L"
run "sets: save_fixture= read" 0 eval 'sets_verdict "$L"; echo "fixture<$SAVE_FIXTURE>"'
expect "sets: SAVE_FIXTURE is the path" "fixture<test-roms/fixture.sav>"
printf 'check-stage2-sets.sh: OK\n' > "$L"; run "sets: no save_fixture= line" 2 sets_verdict "$L"
printf 'save_fixture=\n' > "$L"; run "sets: empty save_fixture=" 2 sets_verdict "$L"
printf '  save_fixture=x.sav\n' > "$L"; run "sets: indented save_fixture= is not the line" 2 sets_verdict "$L"

# --- replay_verdict: run-replay.sh's summary against the list ------------------
LIST=$tmp/list
printf '# a comment line\n\ncpu_instrs 3000 # trailing comment\n   \nsynth:worst-case 600 max_ppm=1000\n' > "$LIST"
pass_line() { echo "  $1: PASS equal=yes underruns=0 over_budget=0 core_max=3412142 readbacks=${2:-12} mismatches=${3:-0} control=2"; }
replay() { # replay <name> <want> <summary lines...>
  local name=$1 want=$2; shift 2
  { echo "run-replay.sh: summary"; printf '%s\n' "$@"; echo "run-replay.sh: PASS (2 entries)"; } > "$L"
  run "replay: $name" "$want" replay_verdict "$L" "$LIST" B3
}
replay "both entries PASS" 0 "$(pass_line cpu_instrs)" "$(pass_line worst-case)"
replay "both PASS, no trailing fields" 0 "$(pass_line cpu_instrs | sed 's/ control=2$//')" "$(pass_line worst-case)"
replay "one PASS line for two entries" 2 "$(pass_line cpu_instrs)"
replay "three PASS lines for two entries" 2 "$(pass_line a)" "$(pass_line b)" "$(pass_line c)"
replay "readbacks=0" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case 0)"
replay "mismatches=3" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case 12 3)"
replay "mismatches=10" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case 12 10)"
replay "mismatches=05 (not 0)" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case 12 05)"
replay "equal=no" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case | sed 's/equal=yes/equal=no/')"
replay "underruns=1" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case | sed 's/underruns=0/underruns=1/')"
replay "over_budget=1" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case | sed 's/over_budget=0/over_budget=1/')"
replay "core_max missing" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case | sed 's/core_max=[0-9]*/core_max=/')"
replay "FAIL entry" 2 "$(pass_line cpu_instrs)" "  worst-case: FAIL equal=no"
replay "unindented PASS line" 2 "$(pass_line cpu_instrs)" "$(pass_line worst-case | sed 's/^  //')"
[[ "$out" == *"run-replay.sh B3 exited 0 with 1 PASS lines for 2 entries"* ]] && ok "replay: message counts" || bad "replay message: $out"

# --- qspi_verdict: run-qspi.sh's capture and check-report.sh's OK --------------
Q=$tmp/b2
mkdir -p "$Q"
printf 'run-qspi.sh: flashing\ncheck-report.sh: OK (%s/cap.txt)\n' "$Q" > "$L"
: > "$Q/cap.txt"
run "qspi: one capture and OK" 0 eval 'qspi_verdict "$L" "$Q"; echo "cap<${caps[0]}>"'
expect "qspi: caps[0] is the capture" "cap<$Q/cap.txt>"
: > "$Q/cap2.txt"; run "qspi: two captures" 2 qspi_verdict "$L" "$Q"; rm "$Q/cap2.txt"
rm "$Q/cap.txt"; run "qspi: no capture" 2 qspi_verdict "$L" "$Q"; : > "$Q/cap.txt"
printf 'check-report.sh: FAIL\n' > "$L"; run "qspi: no OK line" 2 qspi_verdict "$L" "$Q"
printf '  check-report.sh: OK\n' > "$L"; run "qspi: indented OK is not the line" 2 qspi_verdict "$L" "$Q"

# --- cycles_verdict: check-cycles.sh's OK and REPORT lines ---------------------
OKC="check-cycles.sh: OK: 34 lines within +3% of expected-cycles.txt"
RF="REPORT: flash worst-case max 3412142 = 86.4% of 3951279 (the 50% figure; not a check)"
RQ="REPORT: qspi worst-case max 3500000 = 88.6% of 3951279 (the 50% figure; not a check)"
printf '%s\n' "$RF" "$RQ" "$OKC" > "$L"; run "cycles: OK and both REPORTs" 0 cycles_verdict "$L"
printf '%s\n' "$RF" "$RQ" > "$L"; run "cycles: no OK" 2 cycles_verdict "$L"
printf '%s\n' "$RQ" "$OKC" > "$L"; run "cycles: no flash REPORT" 2 cycles_verdict "$L"
printf '%s\n' "$RF" "$OKC" > "$L"; run "cycles: no qspi REPORT" 2 cycles_verdict "$L"
printf '%s\n' "$RF" "$RQ" "  $OKC" > "$L"; run "cycles: indented OK is not the line" 2 cycles_verdict "$L"

# --- display_verdict: run-headless.sh's display self-test line -----------------
DL="run-headless.sh: display self-test passed: 600 FRAME_READY frames, as many transfers and readbacks, 0 mismatches, control 2, 5% budget ok"
printf '%s\n' "$DL" > "$L"; run "display: self-test line" 0 display_verdict "$L"
printf '%s\n' "${DL/ 0 mismatches/ 10 mismatches}" > "$L"; run "display: 10 mismatches" 2 display_verdict "$L"
printf '%s\n' "${DL/passed/FAILED}" > "$L"; run "display: not passed" 2 display_verdict "$L"
printf 'run-headless.sh: lines equal\n' > "$L"; run "display: no self-test line" 2 display_verdict "$L"

# --- speed_verdict: B4's run-paced.sh speed line -------------------------------
P=$tmp/paced.log
speed() { # speed <name> <want> <line>
  printf 'run-replay.sh: summary\n' > "$L"
  printf 'run-paced.sh: lines equal\n%s\n' "$3" > "$P"
  run "speed: $1" "$2" eval 'speed_verdict "$L" "$P"; echo "ppm<$ppm> bound<$bound>"'
}
sl() { echo "run-paced.sh: speed ppm=$1 (bound ${2:-1000}) steady_ppm=+3 elapsed=607000000"; }
speed "+250 within 1000" 0 "$(sl +250)"; expect "speed: ppm and bound read" "ppm<+250> bound<1000>"
speed "-1000 at the bound" 0 "$(sl -1000)"
speed "+1000 at the bound" 0 "$(sl +1000)"
speed "+1001 over" 1 "$(sl +1001)"
speed "-1001 over" 1 "$(sl -1001)"
speed "bound 1001" 1 "$(sl +5 1001)"
speed "unsigned ppm" 2 "$(sl 250)"
speed "no speed line" 2 "run-paced.sh: CHECK FAILED"
speed "indented speed line" 2 "  $(sl +5)"
run "speed: missing files" 2 speed_verdict "$tmp/none.log" "$tmp/none"/*.run-paced.log

# --- save_verdict: run-player-save.sh's PASS line -------------------------------
printf 'run-player-save: PASS fixture 32768 B round-tripped through the player\n' > "$L"; run "save: PASS line" 0 save_verdict "$L"
printf 'run-player-save: FAIL d1 differs\n' > "$L"; run "save: FAIL line" 2 save_verdict "$L"
printf '  run-player-save: PASS fixture 1 B\n' > "$L"; run "save: indented PASS is not the line" 2 save_verdict "$L"

# --- the gate: sources the lib, calls every verdict, defines none itself -------
grep -qx '\. scripts/lib/gate-board.sh' "$gate" && ok "gate sources scripts/lib/gate-board.sh" || bad "gate does not source scripts/lib/gate-board.sh"
for f in board_rc board_absent sets_verdict replay_verdict qspi_verdict cycles_verdict display_verdict speed_verdict save_verdict; do
  if grep -qE "^[[:space:]]*${f}\(\)" "$gate"; then bad "gate defines its own ${f}()"
  elif ! grep -vE '^[[:space:]]*#' "$gate" | grep -qE "(^|[^a-z_])${f}( |\)|$)"; then bad "gate never calls ${f}"
  else ok "gate calls ${f}"; fi
done

# --- the gate's argument errors: exit 2, before any build or lock -------------
garg() { # garg <name> <args...>
  local name=$1 rc; shift
  out=$(env -u GBC_BUILD_LOCK bash "$gate" "$@" 2>&1); rc=$?
  [ "$rc" = 2 ] && ok "gate $name: exit 2" || bad "gate $name: want exit 2, got $rc: $(head -c 200 <<< "$out")"
}
garg "unknown step B9" --step B9
garg "--host-only with a board step" --host-only --step B1
garg "unknown argument" --bogus

[ "$fails" = 0 ] && { echo "test-gate-board.sh: all passed"; exit 0; }
echo "test-gate-board.sh: $fails FAILED"
exit 1
