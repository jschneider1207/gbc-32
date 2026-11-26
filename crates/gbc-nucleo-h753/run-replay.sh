#!/usr/bin/env bash
# The Stage 2 M5 replay check (plan stage-2.md M5 acceptance: "the `player`
# image in scripted mode with display and audio running gives frame-hash
# and DAC-feed-hash lists equal to the host's for the Stage 2 replay
# set"):
#
#   ./run-replay.sh [<list>]      default: replay-set.txt next to this script
#
# The list (the proposed set; acceptance/stage2.toml, once the human
# writes it, supersedes it) has one entry per line:
#
#   <rom under GBC_TEST_ROMS> <frames>            no input
#   <rom under GBC_TEST_ROMS> - <stage1 group>    the group's `input` (and
#                                                 its frame count)
#
# For a group, `replay-input table` converts the group's input from
# acceptance/stage1.toml with romtest's own code into a `clocks v1` table
# (changes at their exact clocks, handed to the core as romtest does;
# gbc_firmware::replay) whose `end` is the frame count.
#
# Per entry, holding the board lock (scripts/lib/board-lock.sh) for the
# whole list:
#   1. the ROM is copied to a scratch dir (nothing is written next to it);
#   2. host: `headless <rom> --paced --frames <N> [--input <table>]`;
#   3. build: `build.sh player-script` with GBC_HEADLESS_INPUT=<table>
#      (unset without input) and GBC_HEADLESS_FRAMES=<N>, the .bin copied
#      to the scratch dir (each entry is its own image: the input and N are
#      compiled in);
#   4. load: flash `loader`, `scripts/gbc-pc.sh load <rom>` into the NOR
#      (skipped when it answers "already loaded");
#   5. run: `run-paced.sh <image> <rom>` with RUN_PACED_HOST = step 2's
#      output. It flashes the image, reads the report and checks it: the
#      three `paced ` lines (rom, input, frames, clock, frame-hash chain,
#      DAC feed producer and consumer side) byte-equal to the host's, ring
#      underruns/overruns 0, display presented >= 1 with no errors, DAC no
#      error/late/restart, report and image integrity. Its speed bound is
#      lifted (RUN_PACED_MAX_PPM): a 10-60 s run's ppm is dominated by the
#      initial ring fill (up to 62.5 ms ahead: +6,250 ppm at 600 frames),
#      so speed stays the M4 10-minute criterion; ppm is printed.
#   6. on top, from the lines run-paced.sh printed from that verified
#      report: `stats player` total_over_budget=0 (exit 1 otherwise), and
#      for a table `stats input` handed=clock_changes and late=0 (a late
#      change is not romtest's emulation); core max (`stats paced core`)
#      is printed. Each line exactly once, each field exactly once and
#      numeric, else exit 2.
#
# Exit: 0 every entry passes; 1 a check failed (run-paced.sh exit 1, or a
# step 6 check); 2 a tool error in any entry (usage, build, flash, load,
# run-paced.sh exit 2, a missing or malformed line). A missing line is
# never a pass. Every entry runs; the worst result wins (2 > 1 > 0).
#
#   RUN_REPLAY_ONLY=<n,..>   only the entries with these names (the ROM's
#                            file stem, or the group id)
#   RUN_REPLAY_OUT=<dir>     keep the per-entry files here (host output,
#                            table, image, serial capture, run-paced log);
#                            default: a temp dir, removed at exit
#   RUN_REPLAY_CAPTURES=<dir> no build, no board, no lock: use
#                            <dir>/<name>.bin, .host and .serial (as saved
#                            in RUN_REPLAY_OUT) and run only steps 5-6 on
#                            them (RUN_PACED_CAPTURE); for tests
#   GBC_TEST_ROMS=<dir>      test ROMs (default: the repo's test-roms/)
#   GBC_BUILD_LOCK=<file>    run every cargo command under `flock -x <file>`
#   H7_DRIVE, H7_LABEL, H7_COM  as for flash.sh and read-serial.sh
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-replay.sh: $*" >&2; exit 2; }
[ $# -le 1 ] || die "usage: run-replay.sh [<list>]"
list=$(readlink -f "${1:-$here/replay-set.txt}")
[ -f "$list" ] || die "$list: no such file"
roms=${GBC_TEST_ROMS:-$root/test-roms}
caps=${RUN_REPLAY_CAPTURES:-}
locked() { if [ -n "${GBC_BUILD_LOCK:-}" ]; then flock -x "$GBC_BUILD_LOCK" "$@"; else "$@"; fi; }

if [ -z "$caps" ]; then
  . "$root/scripts/lib/board-lock.sh"
  board_lock_reexec "$@"
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  (cd "$root" && locked cargo build -q --release -p headless -p replay-input) || die "building headless and replay-input failed"
  (cd "$root" && locked cargo build -q -p gbc-pc --release --target x86_64-pc-windows-gnu) || die "building gbc-pc failed"
  "$here/build.sh" loader > /dev/null || die "building the loader image failed"
fi
headless=$root/target/release/headless
replay_input=$root/target/release/replay-input

if [ -n "${RUN_REPLAY_OUT:-}" ]; then
  out=$RUN_REPLAY_OUT; mkdir -p "$out" || die "cannot create $out"
else
  out=$(mktemp -d) || die "mktemp failed"; trap 'rm -rf "$out"' EXIT
fi

worst=0
result() { # result <rc>: keep the worst
  if [ "$1" = 2 ] || { [ "$1" = 1 ] && [ "$worst" = 0 ]; }; then worst=$1; fi
}
summary=()

# check_entry <name> <log> <rc> <clocked 0|1>: step 6 on run-paced.sh's
# output; prints one summary line; returns 0/1/2.
check_entry() {
  local name=$1 log=$2 rc=$3 clocked=$4 n v rc6=0 core ring player input
  line() { # line <prefix>: exactly one line of the log starting with it
    n=$(grep -c "^$1 " "$log")
    [ "$n" = 1 ] || { echo "run-replay.sh: $name: want exactly one '$1' line, run-paced.sh printed $n" >&2; return 2; }
    grep "^$1 " "$log"
  }
  val() { # val <line> <key>: the value, exactly once and numeric
    n=$(tr ' ' '\n' <<< "$1" | grep -c "^$2=")
    [ "$n" = 1 ] || { echo "run-replay.sh: $name: '$2=' appears $n times in: $1" >&2; return 2; }
    v=$(tr ' ' '\n' <<< "$1" | sed -n "s/^$2=//p")
    [[ "$v" =~ ^[0-9]+$ ]] || { echo "run-replay.sh: $name: '$2=$v' is malformed in: $1" >&2; return 2; }
    printf '%s\n' "$v"
  }
  [ "$rc" = 2 ] && return 2
  core=$(line 'stats paced core') || return 2
  ring=$(line 'stats paced ring') || return 2
  player=$(line 'stats player') || return 2
  local cmax ob under
  cmax=$(val "$core" max) || return 2
  ob=$(val "$player" total_over_budget) || return 2
  # The run's underruns (run-paced.sh checks them); `stats player`'s count
  # also includes the drain and the stop.
  under=$(val "$ring" underrun_events) || return 2
  local inp=""
  if [ "$clocked" = 1 ]; then
    input=$(line 'stats input') || return 2
    local total handed late
    total=$(val "$input" clock_changes) || return 2
    handed=$(val "$input" handed) || return 2
    late=$(val "$input" late) || return 2
    [ "$late" = 0 ] || { echo "run-replay.sh: CHECK FAILED: $name: stats input late=$late (want 0)"; rc6=1; }
    [ "$handed" = "$total" ] || { echo "run-replay.sh: CHECK FAILED: $name: stats input handed=$handed of clock_changes=$total"; rc6=1; }
    inp=" input=$handed/$total late=$late"
  elif grep -q '^stats input ' "$log"; then
    echo "run-replay.sh: $name: a 'stats input' line without a clock table" >&2; return 2
  fi
  [ "$ob" = 0 ] || { echo "run-replay.sh: CHECK FAILED: $name: stats player total_over_budget=$ob (want 0)"; rc6=1; }
  local equal=no
  grep -q '^run-paced.sh: EQUAL' "$log" && equal=yes
  local verdict=FAIL
  [ "$rc" = 0 ] && [ "$rc6" = 0 ] && verdict=PASS
  summary+=("$name: $verdict equal=$equal underruns=$under over_budget=$ob core_max=$cmax$inp")
  [ "$rc" = 1 ] && return 1
  return $rc6
}

entries=0 checked=0
while read -r rom_rel frames group extra; do
  case "$rom_rel" in ''|'#'*) continue;; esac
  [ -z "${extra:-}" ] || die "$list: too many fields: $rom_rel $frames $group $extra"
  name=$(basename "${rom_rel%.*}"); [ -n "${group:-}" ] && name=$group
  if [ -n "${RUN_REPLAY_ONLY:-}" ] && ! [[ ",$RUN_REPLAY_ONLY," == *",$name,"* ]]; then continue; fi
  entries=$((entries + 1))
  echo "run-replay.sh: === $name ($rom_rel, frames ${frames}${group:+, stage1 group $group})"
  clocked=0; [ -n "${group:-}" ] && clocked=1
  if [ -n "$caps" ]; then
    img=$caps/$name.bin host=$caps/$name.host serial=$caps/$name.serial
    for f in "$img" "$host" "$serial"; do [ -f "$f" ] || die "$f: no such file"; done
    rom=/nonexistent.gb
    # A group's length comes from its table; here, from the host's header.
    [ "$frames" = - ] && frames=$(sed -n 's/^paced v1 .* frames=\([0-9]*\)$/\1/p' "$host")
  else
    [ -f "$roms/$rom_rel" ] || die "$roms/$rom_rel: no such file"
    rom=$out/$(basename "$rom_rel"); cp "$roms/$rom_rel" "$rom" || die "cannot copy $rom_rel"
    input_args=()
    if [ "$clocked" = 1 ]; then
      "$replay_input" table --manifest "$root/acceptance/stage1.toml" --group "$group" > "$out/$name.clocks" ||
        die "$name: replay-input table failed"
      want_frames=$(sed -n 's/^end \([0-9]*\)$/\1/p' "$out/$name.clocks")
      [ "$frames" = - ] && frames=$want_frames
      input_args=(--input "$out/$name.clocks")
    fi
    case "$frames" in ''|*[!0-9]*) die "$name: frames '$frames' is not a number";; esac
    host=$out/$name.host img=$out/$name.bin serial=$out/$name.serial
    "$headless" "$rom" --paced --frames "$frames" "${input_args[@]}" > "$host" || die "$name: host run failed"
    if [ "$clocked" = 1 ]; then
      GBC_HEADLESS_INPUT=$out/$name.clocks GBC_HEADLESS_FRAMES=$frames "$here/build.sh" player-script > "$out/$name.build.log" 2>&1
    else
      env -u GBC_HEADLESS_INPUT GBC_HEADLESS_FRAMES=$frames "$here/build.sh" player-script > "$out/$name.build.log" 2>&1
    fi || { tail -5 "$out/$name.build.log" >&2; die "$name: build.sh player-script failed"; }
    cp "$here/out/player-script.bin" "$img" || die "cannot copy the image"
    "$here/flash.sh" "$here/out/loader.bin" > "$out/$name.flash-loader.log" 2>&1 || { tail -5 "$out/$name.flash-loader.log" >&2; die "$name: flash.sh loader failed"; }
    sleep 1
    "$root/scripts/gbc-pc.sh" load "$rom" --baud 2000000 > "$out/$name.load.log" 2>&1 || { tail -5 "$out/$name.load.log" >&2; die "$name: gbc-pc load failed"; }
    grep -E '^(loaded|already loaded)' "$out/$name.load.log"
  fi
  case "$frames" in ''|*[!0-9]*) die "$name: frames '$frames' is not a number";; esac
  secs=$((frames / 59 + 50))
  if [ -n "$caps" ]; then
    RUN_PACED_CAPTURE=$serial RUN_PACED_HOST=$host RUN_PACED_FRAMES=$frames RUN_PACED_MAX_PPM=999999999 \
      bash "$here/run-paced.sh" "$img" "$rom" > "$out/$name.run-paced.log" 2>&1
  else
    RUN_PACED_HOST=$host RUN_PACED_FRAMES=$frames RUN_PACED_MAX_PPM=999999999 RUN_PACED_READ_SECS=$secs \
      RUN_PACED_SAVE=$serial bash "$here/run-paced.sh" "$img" "$rom" > "$out/$name.run-paced.log" 2>&1
  fi
  rc=$?
  sed 's/^/  /' "$out/$name.run-paced.log"
  check_entry "$name" "$out/$name.run-paced.log" "$rc" "$clocked"
  rc=$?
  [ "$rc" = 2 ] && summary+=("$name: TOOL ERROR (see above)")
  result "$rc"
  checked=$((checked + 1))
done < "$list"
[ "$entries" -gt 0 ] || die "no entries selected from $list"
# An entry the loop left early (a shell error) was never checked.
[ "$checked" = "$entries" ] || die "only $checked of $entries entries were checked"

echo "run-replay.sh: summary"
printf '  %s\n' "${summary[@]}"
case $worst in
  0) echo "run-replay.sh: PASS ($entries entries)";;
  1) echo "run-replay.sh: FAILED";;
  *) echo "run-replay.sh: TOOL ERROR";;
esac
exit "$worst"
