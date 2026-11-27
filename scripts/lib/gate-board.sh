# shellcheck shell=bash
# Sourced by scripts/gate-stage2.sh: the board steps' (B1-B5) verdicts on
# what the board and the board scripts gave. Factored out of the gate so
# scripts/test-gate-board.sh can feed them canned output (M7 review T11);
# the gate's behavior is unchanged by the move.
#
# The caller defines:
#   fail <msg>         a check failed: exit 1
#   tool_error <msg>   a tool or runner error: exit 2
#   board_vol          prints the answer to `vol <drive>`
#   board_details      prints the answer to `if exist <drive>\DETAILS.TXT echo yes`
#   board_ports        prints the Windows serial port names, one per line
# and the variables drive, label and port (H7_DRIVE, H7_LABEL, H7_COM).

# board_rc <what> <rc>: a board script's exit: 0 on, 1 a check failed, else
# a tool error (the scripts' own contract: 2, or a crash).
board_rc() {
  case $2 in
    0) ;;
    1) fail "$1: a check failed (exit 1)" ;;
    *) tool_error "$1 exited $2" ;;
  esac
}

# board_absent: the board, as flash.sh and read-serial.sh find it: the
# ST-LINK drive with its label and DETAILS.TXT, and the COM port. When it
# is not all there, prints why and returns 0; returns 1 (prints nothing)
# when it is. Each probe runs only when the ones before it passed.
board_absent() {
  local vol ports
  vol=$(board_vol)
  case "${vol}" in *"${label}"*) ;; *)
    echo "volume ${drive} is not labelled ${label} (${vol:-no answer}); attach the NUCLEO-H753ZI"
    return 0 ;;
  esac
  case "$(board_details)" in
    *yes*) ;; *) echo "${drive} has no DETAILS.TXT (not an ST-LINK drive)"; return 0 ;;
  esac
  ports=$(board_ports)
  grep -qx "${port}" <<< "${ports}" ||
    { echo "serial port ${port} is not present (ports: $(tr '\n' ' ' <<< "${ports}"))"; return 0; }
  return 1
}

# board_lost_check <what>: the gate's tool_error and unexpected-exit paths
# call this once the board lock is held and the board was seen there (gate
# LOCK; board_held=1). If the board has gone since, the error is a missing
# board: a failure, exit 1 (the gate's contract: a missing board is exit 1,
# never a pass; M7 review T9), not exit 2. Returns, so the caller reports
# the error as it is, when the board is still there.
board_lost_check() {
  local why
  if why=$(board_absent); then
    fail "board lost after the board lock: ${why} (while: $1)"
  fi
  return 0
}

# sets_verdict <log>: check-stage2-sets.sh's save_fixture= line; sets
# SAVE_FIXTURE.
sets_verdict() {
  SAVE_FIXTURE=$(sed -n 's/^save_fixture=//p' "$1")
  [ -n "${SAVE_FIXTURE}" ] || tool_error "scripts/check-stage2-sets.sh printed no save_fixture="
}

# replay_verdict <log> <list> <label>: after run-replay.sh exited 0, every
# entry of the list must be in its summary as PASS with equal=yes and at
# least one panel readback with 0 mismatches.
replay_verdict() {
  local n_want n_pass
  n_want=$(sed 's/#.*//' "$2" | awk 'NF' | wc -l)
  n_pass=$(grep -cE '^  [^ ]+: PASS equal=yes underruns=0 over_budget=0 core_max=[0-9]+ readbacks=[1-9][0-9]* mismatches=0( |$)' "$1" || true)
  [ "${n_pass}" = "${n_want}" ] || tool_error "run-replay.sh $3 exited 0 with ${n_pass} PASS lines for ${n_want} entries"
}

# qspi_verdict <log> <dir>: after run-qspi.sh exited 0: exactly one
# capture (<dir>/*.txt; sets caps), and check-report.sh's OK in the log.
qspi_verdict() {
  caps=("$2"/*.txt)
  [ ${#caps[@]} -eq 1 ] && [ -f "${caps[0]}" ] || tool_error "run-qspi.sh left ${#caps[@]} captures in $2, not 1"
  grep -q '^check-report.sh: OK' "$1" || tool_error "run-qspi.sh exited 0 without check-report.sh's OK"
}

# cycles_verdict <log>: after check-cycles.sh exited 0: its OK, and the
# worst-case REPORT line of both passes.
cycles_verdict() {
  grep -q '^check-cycles.sh: OK' "$1" || tool_error "check-cycles.sh exited 0 without its OK"
  grep -q '^REPORT: flash worst-case ' "$1" && grep -q '^REPORT: qspi worst-case ' "$1" ||
    tool_error "check-cycles.sh printed no worst-case REPORT line for both passes"
}

# display_verdict <log>: after run-headless.sh exited 0: its display
# self-test line with 0 mismatches.
display_verdict() {
  grep -q '^run-headless.sh: display self-test passed: .* 0 mismatches' "$1" ||
    tool_error "run-headless.sh exited 0 without its display self-test line"
}

# speed_verdict <file>...: B4's emulated speed against the board's own
# cycle counter (`stats paced speed` ppm, run-paced.sh), from the last
# `run-paced.sh: speed ppm=` line of the files: the entry's bound must be
# the +-0.1%, and the speed within it. Sets ppm and bound.
speed_verdict() {
  local speed
  speed=$(grep -h '^run-paced.sh: speed ppm=' "$@" 2> /dev/null | tail -n 1 || true)
  [[ "${speed}" =~ ppm=([+-][0-9]+)\ \(bound\ ([0-9]+)\) ]] || tool_error "B4: no 'run-paced.sh: speed ppm=' line"
  ppm=${BASH_REMATCH[1]} bound=${BASH_REMATCH[2]}
  [ "${bound}" -le 1000 ] || fail "B4: the soak's speed bound was ${bound} ppm, not <= 1000 (soak-set.txt max_ppm)"
  [ "${ppm#[+-]}" -le 1000 ] || fail "B4: emulated speed ${ppm} ppm of the cycle counter (want within +-1000)"
}

# save_verdict <log>: after run-player-save.sh exited 0: its PASS line.
save_verdict() {
  grep -q "^run-player-save: PASS fixture " "$1" || tool_error "run-player-save.sh exited 0 without its PASS line"
}
