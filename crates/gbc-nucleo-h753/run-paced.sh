#!/usr/bin/env bash
# Runs a `paced` image on the NUCLEO-H753ZI and checks it against the host
# (plan Stage 2 M4 acceptance: a 10-minute run with the display and the DAC
# on, 0 underruns, emulated speed within +-0.1% of the cycle counter, the
# DAC-feed hash equal to the host's):
#
#   ./run-paced.sh out/paced-cgb_sound.bin ../../test-roms/blargg/cgb_sound/cgb_sound.gb
#   ./run-paced.sh out/paced-worst-case.bin out/worst-case.gb
#
# The second argument is the ROM file the image embeds (build.sh: for a
# synthetic ROM, out/<name>.gb). The script runs the host's
# `headless <rom> --frames <N> --paced` (tools/headless; the same loop and
# line format, gbc_firmware::paced), flashes the image (flash.sh), reads
# the serial port long enough for the whole run (read-serial.sh), takes
# the last complete report the image sent (src/bin/paced.rs re-sends it
# after the run) and checks:
#
# 1. Compared (exit 1 if they differ): the report's `paced ` lines equal
#    the host's, byte for byte (a trailing `\r` stripped). Both must be
#    exactly the three lines `paced v1`, `paced end`, `paced drain`, in
#    that order (else exit 2), so equal lines are equal ROM (rom_fnv),
#    input, frame count, emulated clock, frame-hash chain and DAC feed
#    (producer side, `end`, and what the DAC ISR handed the DAC, `drain`).
# 2. Report-only lines, each required exactly once with each field below
#    exactly once and numeric (else exit 2); exit 1 unless:
#    - `stats paced ring`: underrun_events=0 underrun_frames=0
#      overrun_events=0 overrun_frames=0 (during the run; the drain's own
#      last-pop underrun is printed, not checked: paced.rs docs);
#    - `stats paced speed`: |ppm| <= RUN_PACED_MAX_PPM (default 1000: the
#      +-0.1% criterion, the whole run against the cycle counter);
#      steady_ppm is printed;
#    - `stats display paced`: transfer_errors=0 stuck=0 and presented >= 1
#      (the display was on); presented and skipped are printed;
#    - `stats dac`: late=0 te=0 dme=0 fe=0 udr1=0 udr2=0 stop=ok (the DMA
#      and the DAC saw no error, no refill was missed, the stop ramp
#      finished).
#
# A report is complete when it runs from a `<prefix> paced report n=<k>`
# line to the `<prefix> paced report-end n=<k> ... bytes=<len> cksum=<crc>
# overflow=0` line of the same k and the lines between, `\n`-terminated,
# have that length and POSIX cksum; its report line's `image_bytes=<n>
# image_cksum=<crc>` must be `cksum` of <image.bin> (else exit 2: the board
# is not running the flashed image). As run-headless.sh.
#
# Exit: 0 all checks pass, 1 a check failed (each failure printed), 2 tool
# error (arguments, host run, flash, serial, no complete report, a missing,
# repeated or malformed line). A missing line is never a pass.
#
#   RUN_PACED_FRAMES     frames for the host run (default 35837, the image's
#                        default: 10 minutes)
#   RUN_PACED_HOST       a file with the host's output, instead of running it
#   RUN_PACED_HEADLESS   the host tool (default ../../target/release/headless;
#                        cargo build --release -p headless)
#   RUN_PACED_MAX_PPM    the speed bound (default 1000)
#   RUN_PACED_READ_SECS  seconds of serial to read (default 660: the run is
#                        about 600 s and starts during flash.sh's 25 s wait)
#   RUN_PACED_CAPTURE    a saved serial capture to use instead of flashing and
#                        reading the board (checks this script)
#   RUN_PACED_SAVE       also copy the serial read to this file
#   RUN_PACED_NO_FLASH=1 read the board without flashing (the image check
#                        still applies)
#   H7_DRIVE, H7_LABEL, H7_COM  as for flash.sh and read-serial.sh
#
# Holds the board lock (scripts/lib/board-lock.sh) across the flash and the
# read, so no other agent flashes in between.
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
. "$here/../../scripts/lib/board-lock.sh"
[ -n "${RUN_PACED_CAPTURE:-}" ] || board_lock_reexec "$@"

die() { echo "run-paced.sh: $*" >&2; exit 2; }
[ $# -eq 2 ] || die "usage: run-paced.sh <image.bin> <rom>"
img=$1 rom=$2
frames=${RUN_PACED_FRAMES:-35837}
secs=${RUN_PACED_READ_SECS:-660}
max_ppm=${RUN_PACED_MAX_PPM:-1000}
for v in frames secs max_ppm; do
  case "${!v}" in ''|*[!0-9]*) die "$v=${!v} is not a number";; esac
done
PREFIX=gbc-nucleo-h753

tmp=$(mktemp -d) || die "mktemp failed"
trap 'rm -rf "$tmp"' EXIT

[ -f "$img" ] || die "$img: no such file"
read -r img_crc img_bytes < <(cksum < "$img") || die "cksum $img failed"
want_image="image_bytes=$img_bytes image_cksum=$img_crc"

# The host's lines.
if [ -n "${RUN_PACED_HOST:-}" ]; then
  [ -f "$RUN_PACED_HOST" ] || die "RUN_PACED_HOST=$RUN_PACED_HOST: no such file"
  tr -d '\r' < "$RUN_PACED_HOST" > "$tmp/host"
else
  [ -f "$rom" ] || die "$rom: no such file"
  tool=${RUN_PACED_HEADLESS:-$here/../../target/release/headless}
  [ -x "$tool" ] || die "$tool: not built (cargo build --release -p headless)"
  "$tool" "$rom" --frames "$frames" --paced > "$tmp/host" || die "the host run failed: $tool $rom --frames $frames --paced"
  echo "run-paced.sh: host: $tool $rom --frames $frames --paced"
fi
grep '^paced ' "$tmp/host" > "$tmp/want"

if [ -n "${RUN_PACED_CAPTURE:-}" ]; then
  [ -f "$RUN_PACED_CAPTURE" ] || die "RUN_PACED_CAPTURE=$RUN_PACED_CAPTURE: no such file"
  tr -d '\r' < "$RUN_PACED_CAPTURE" > "$tmp/serial"
else
  start=$(date +%s)
  if [ "${RUN_PACED_NO_FLASH:-}" = 1 ]; then
    echo "run-paced.sh: RUN_PACED_NO_FLASH=1: not flashing $img"
  else
    "$here/flash.sh" "$img" || die "flash.sh failed"
  fi
  echo "run-paced.sh: reading the serial port for $secs s"
  "$here/../../tools/h7-bench/read-serial.sh" "$secs" > "$tmp/serial" || die "read-serial.sh failed"
  if [ -n "${RUN_PACED_SAVE:-}" ]; then
    cp "$tmp/serial" "$RUN_PACED_SAVE" || die "RUN_PACED_SAVE=$RUN_PACED_SAVE: copy failed"
  fi
  echo "run-paced.sh: read in $(($(date +%s) - start)) s (flash included unless RUN_PACED_NO_FLASH)"
fi

# Split the capture into reports (as run-headless.sh): report.<k> holds the
# lines between the markers, report.<k>.end the end line; a report without
# its end line, or interrupted by another report line, has no .end file.
awk -v dir="$tmp" -v p="$PREFIX" '
  function field(line, key,   i, n, a) {
    n = split(line, a, " ")
    for (i = 1; i <= n; i++) if (index(a[i], key "=") == 1) return substr(a[i], length(key) + 2)
    return ""
  }
  index($0, p " paced report ") == 1 {
    if (cur != "") { close(cur); close(cur ".start"); close(cur ".end") }
    k = field($0, "n"); seq++; cur = dir "/report." seq; printf "" > cur; open = 1
    print $0 > (cur ".start"); next
  }
  index($0, p " paced report-end ") == 1 {
    if (open && field($0, "n") == k) print $0 > (cur ".end")
    open = 0; next
  }
  open { print > cur }
' "$tmp/serial"

chosen="" partial=0 complete=0
for f in $(ls "$tmp"/report.* 2>/dev/null | grep -E '/report\.[0-9]+$' | sort -t. -k2 -n); do
  if [ ! -f "$f.end" ]; then partial=$((partial + 1)); continue; fi
  end=$(cat "$f.end")
  want_bytes=$(sed -n 's/.* bytes=\([0-9]*\).*/\1/p' <<< "$end")
  want_crc=$(sed -n 's/.* cksum=\([0-9]*\).*/\1/p' <<< "$end")
  overflow=$(sed -n 's/.* overflow=\([0-9]*\).*/\1/p' <<< "$end")
  read -r crc bytes < <(cksum < "$f")
  if [ "$crc" = "$want_crc" ] && [ "$bytes" = "$want_bytes" ]; then
    [ "$overflow" = 0 ] || die "the image's capture buffer overflowed: $end"
    complete=$((complete + 1)); chosen=$f
  else
    partial=$((partial + 1))
    echo "run-paced.sh: a report failed its check (got $bytes B cksum $crc; $end)" >&2
  fi
done
if [ -z "$chosen" ]; then
  grep -E "^$PREFIX paced (input )?error |^paced error " "$tmp/serial" | tail -n 1 | sed 's/^/run-paced.sh: the board says: /' >&2
  die "no complete report in ${secs} s of serial ($partial incomplete); serial: $(wc -l < "$tmp/serial") lines"
fi
echo "run-paced.sh: $complete complete report(s), $partial incomplete; checking the last"
cat "$chosen.start"
grep '^stats ' "$chosen"
cat "$chosen.end"
got_image=$(grep -oE ' image_bytes=[0-9]+ image_cksum=[0-9]+( |$)' "$chosen.start" | sed 's/^ //; s/ $//')
[ "$got_image" = "$want_image" ] ||
  die "the board is not running the flashed image: its report has '${got_image:-no image_bytes/image_cksum}', $img has '$want_image'"
echo "run-paced.sh: image matched: $want_image"

grep '^paced ' "$chosen" > "$tmp/got"

# shape <file> <what>: exactly the three compared lines, in order.
shape() {
  local kinds
  kinds=$(awk '{print $1 " " $2}' "$1" | tr '\n' ',')
  [ "$kinds" = "paced v1,paced end,paced drain," ] ||
    die "$2 'paced' lines are not exactly v1, end, drain (got: ${kinds:-none}): $(tr '\n' '|' < "$1")"
}
shape "$tmp/want" "the host's"
shape "$tmp/got" "the board's"

# line <prefix>: the one report line starting with "<prefix> " (else exit 2).
line() {
  local n
  n=$(grep -c "^$1 " "$chosen")
  [ "$n" = 1 ] || die "want exactly one '$1' line, the report has $n"
  grep "^$1 " "$chosen"
}
# val <line> <key> [re]: the key's value, exactly once and matching re
# (default: a decimal number), else exit 2.
val() {
  local v n re=${3:-'^[0-9]+$'}
  n=$(tr ' ' '\n' <<< "$1" | grep -c "^$2=")
  [ "$n" = 1 ] || die "'$2=' appears $n times in: $1"
  v=$(tr ' ' '\n' <<< "$1" | sed -n "s/^$2=//p")
  [[ "$v" =~ $re ]] || die "'$2=$v' is malformed in: $1"
  printf '%s\n' "$v"
}
# Every value read before any check, so a malformed line is a tool error
# whatever the checks say. (`val` in a command substitution exits only its
# subshell: `|| exit 2` carries it.)
ring=$(line 'stats paced ring') || exit 2
speed=$(line 'stats paced speed') || exit 2
display=$(line 'stats display paced') || exit 2
dac=$(line 'stats dac') || exit 2
drain=$(line 'stats paced drain') || exit 2
declare -A V
for k in underrun_events underrun_frames overrun_events overrun_frames min_fill max_fill; do
  V[ring.$k]=$(val "$ring" "$k") || exit 2
done
V[speed.ppm]=$(val "$speed" ppm '^[+-][0-9]+$') || exit 2
V[speed.steady_ppm]=$(val "$speed" steady_ppm '^([+-][0-9]+|-)$') || exit 2
V[speed.elapsed]=$(val "$speed" elapsed) || exit 2
for k in presented skipped transfers transfer_errors stuck; do
  V[display.$k]=$(val "$display" "$k") || exit 2
done
for k in late te dme fe udr1 udr2; do
  V[dac.$k]=$(val "$dac" "$k") || exit 2
done
V[dac.stop]=$(val "$dac" stop '^(ok|timeout)$') || exit 2
for k in underrun_events underrun_frames; do
  V[drain.$k]=$(val "$drain" "$k") || exit 2
done

fail=0
bad() { echo "run-paced.sh: CHECK FAILED: $*"; fail=1; }

if cmp -s "$tmp/want" "$tmp/got"; then
  echo "run-paced.sh: EQUAL: the board's 3 paced lines match the host's"
  cat "$tmp/got"
else
  bad "the paced lines differ from the host's"
  diff "$tmp/want" "$tmp/got" | sed 's/^/  /'
fi

for k in underrun_events underrun_frames overrun_events overrun_frames; do
  [ "${V[ring.$k]}" = 0 ] || bad "stats paced ring $k=${V[ring.$k]} (want 0)"
done
ppm=${V[speed.ppm]}
abs=${ppm#[+-]}
[ "$abs" -le "$max_ppm" ] || bad "stats paced speed ppm=$ppm (want |ppm| <= $max_ppm)"
for k in transfer_errors stuck; do
  [ "${V[display.$k]}" = 0 ] || bad "stats display paced $k=${V[display.$k]} (want 0)"
done
[ "${V[display.presented]}" -ge 1 ] || bad "stats display paced presented=0 (the display was not on)"
for k in late te dme fe udr1 udr2; do
  [ "${V[dac.$k]}" = 0 ] || bad "stats dac $k=${V[dac.$k]} (want 0)"
done
[ "${V[dac.stop]}" = ok ] || bad "stats dac stop=${V[dac.stop]} (want ok)"

echo "run-paced.sh: ring underruns=${V[ring.underrun_events]}/${V[ring.underrun_frames]} overruns=${V[ring.overrun_events]}/${V[ring.overrun_frames]} fill=${V[ring.min_fill]}..${V[ring.max_fill]}; drain underrun=${V[drain.underrun_events]}/${V[drain.underrun_frames]} (not checked)"
echo "run-paced.sh: speed ppm=$ppm (bound $max_ppm) steady_ppm=${V[speed.steady_ppm]} elapsed=${V[speed.elapsed]}"
echo "run-paced.sh: display presented=${V[display.presented]} skipped=${V[display.skipped]} transfers=${V[display.transfers]} errors=${V[display.transfer_errors]}"
if [ "$fail" != 0 ]; then
  echo "run-paced.sh: FAILED"
  exit 1
fi
echo "run-paced.sh: PASS"
exit 0
