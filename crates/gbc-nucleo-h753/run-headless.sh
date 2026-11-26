#!/usr/bin/env bash
# Runs a `headless` image on the NUCLEO-H753ZI and compares its hash lines
# with the host's (plan Stage 2 M1 acceptance):
#
#   ./run-headless.sh out/headless-cpu_instrs.bin ../../tools/headless/expected/cpu_instrs.txt
#
# Flashes the image (flash.sh), reads the serial port
# (tools/h7-bench/read-serial.sh), takes the last complete report the image
# sent (src/bin/headless.rs: it re-sends its report after the run) and
# diffs its lines starting `headless ` against the expected file's lines
# starting `headless ` (a trailing `\r` stripped from both). Everything
# else (`stats ` lines, the banner) is not compared, except:
#
# A display image (plan Stage 2 M3, src/bin/headless.rs `display`) must
# also pass its self-test (M3 review T-1). The image is a display image if
# its file name starts `headless-display-`, or the .bin contains the
# `stats display start=` format text, or the report has a `stats display`
# line (any one is enough, so a missing line cannot skip the check). Then
# the report must have exactly one `stats display start=` line and one
# `stats display cpu ` line, each field below exactly once, with:
# frames, transfers and readbacks each equal to the number of compared
# `headless frame=` lines with FRAME_READY (ev bit 0; at least 1),
# mismatches=0, control_mismatches=2, transfer_errors=0,
# start_timeouts=0, dead=0, and the cpu line's ok=1 (the 5% budget).
# Otherwise exit 1, after the lines' comparison (so a `GBC_DISPLAY_START=off`
# image, which reads nothing back, exits 1 even with equal lines).
#
# A report is complete when it runs from a `<prefix> headless report n=<k>`
# line to the `<prefix> headless end n=<k> ... bytes=<len> cksum=<crc>
# overflow=0` line of the same k, and the lines between, `\n`-terminated,
# have that length and POSIX cksum. So a report cut or garbled by the
# serial link is never compared (it is a tool error, not a difference).
# Lines before a report (the previous image's, buffered by the VCP) are
# ignored.
#
# The compared report must come from the image given: its report line's
# `image_bytes=<n> image_cksum=<crc>` (src/image.rs: the image's own
# flash range, which build.sh checks is the .bin) must equal `cksum` of
# <image.bin>. Otherwise the board is not running the flashed image (the
# ST-LINK accepted the copy and did not program it, flash.sh) and the
# script exits 2, whatever its lines say.
#
# Exit: 0 equal (and a display image's self-test passed), 1 differ (the
# first differing line is printed) or the display self-test failed, 2 tool
# error (bad arguments, flash or serial failure, no complete report).
#
#   RUN_HEADLESS_ROM=<rom>  also require the report line's `rom_len=<n>
#                           rom_hash=<16 hex>` (the `headless-qspi` image's:
#                           the ADR 0037 header of the image in the NOR) to be
#                           the length and FNV-1a 64 of <rom> as computed here
#                           from the file, else exit 2: a stale or different
#                           NOR image cannot pass. Also require its
#                           `exit_fallbacks=` field (reported). Unset: no ROM
#                           check (the internal-flash images embed theirs).
#   RUN_HEADLESS_READ_SECS  seconds of serial to read (default 20; a 600-frame
#                           report takes about 3.5 s, and the image re-sends
#                           it after a 1 s pause)
#   RUN_HEADLESS_CAPTURE    a saved serial capture to use instead of flashing
#                           and reading the board (checks this script)
#   RUN_HEADLESS_SAVE      also copy the serial read to this file (for a
#                           later RUN_HEADLESS_CAPTURE); no other effect
#   RUN_HEADLESS_NO_FLASH=1 read the board without flashing (debugging; the
#                           image check still applies, so it passes only if
#                           the board already runs <image.bin>)
#   H7_DRIVE, H7_LABEL, H7_COM  as for flash.sh and read-serial.sh
#
# Holds the board lock (scripts/lib/board-lock.sh) across the flash and
# the read, so no other agent flashes in between.
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
. "$here/../../scripts/lib/board-lock.sh"
[ -n "${RUN_HEADLESS_CAPTURE:-}" ] || board_lock_reexec "$@"

die() { echo "run-headless.sh: $*" >&2; exit 2; }
[ $# -eq 2 ] || die "usage: run-headless.sh <image.bin> <expected.txt>"
img=$1 expected=$2
[ -f "$expected" ] || die "$expected: no such file"
secs=${RUN_HEADLESS_READ_SECS:-20}
case "$secs" in ''|*[!0-9]*) die "RUN_HEADLESS_READ_SECS=$secs is not a number";; esac
PREFIX=gbc-nucleo-h753

tmp=$(mktemp -d) || die "mktemp failed"
trap 'rm -rf "$tmp"' EXIT

[ -f "$img" ] || die "$img: no such file"
read -r img_crc img_bytes < <(cksum < "$img") || die "cksum $img failed"
want_image="image_bytes=$img_bytes image_cksum=$img_crc"

# fnv1a64 <file>: FNV-1a 64 of the file's bytes, 16 hex digits (bash
# arithmetic is 64-bit and wraps, which is the FNV modulus; about 4 s per
# MiB).
fnv1a64() {
  local h=$((0xcbf29ce484222325)) b bs
  while read -r -a bs; do
    for b in "${bs[@]}"; do h=$(((h ^ b) * 0x100000001b3)); done
  done < <(od -An -v -tu1 -w32 "$1")
  printf '%016x\n' "$h"
}
want_rom=""
if [ -n "${RUN_HEADLESS_ROM:-}" ]; then
  [ -f "$RUN_HEADLESS_ROM" ] || die "RUN_HEADLESS_ROM=$RUN_HEADLESS_ROM: no such file"
  want_rom="rom_len=$(stat -c %s "$RUN_HEADLESS_ROM") rom_hash=$(fnv1a64 "$RUN_HEADLESS_ROM")"
fi

if [ -n "${RUN_HEADLESS_CAPTURE:-}" ]; then
  [ -f "$RUN_HEADLESS_CAPTURE" ] || die "RUN_HEADLESS_CAPTURE=$RUN_HEADLESS_CAPTURE: no such file"
  tr -d '\r' < "$RUN_HEADLESS_CAPTURE" > "$tmp/serial"
else
  start=$(date +%s)
  if [ "${RUN_HEADLESS_NO_FLASH:-}" = 1 ]; then
    echo "run-headless.sh: RUN_HEADLESS_NO_FLASH=1: not flashing $img"
  else
    "$here/flash.sh" "$img" || die "flash.sh failed"
  fi
  "$here/../../tools/h7-bench/read-serial.sh" "$secs" > "$tmp/serial" || die "read-serial.sh failed"
  if [ -n "${RUN_HEADLESS_SAVE:-}" ]; then
    cp "$tmp/serial" "$RUN_HEADLESS_SAVE" || die "RUN_HEADLESS_SAVE=$RUN_HEADLESS_SAVE: copy failed"
  fi
  echo "run-headless.sh: read in $(($(date +%s) - start)) s (flash included unless RUN_HEADLESS_NO_FLASH)"
fi

# Split the capture into reports: report.<k> holds the lines between the
# markers, report.<k>.end the end line; a report without its end line, or
# interrupted by another report line, is left incomplete (no .end file).
awk -v dir="$tmp" -v p="$PREFIX" '
  function field(line, key,   i, n, a) {
    n = split(line, a, " ")
    for (i = 1; i <= n; i++) if (index(a[i], key "=") == 1) return substr(a[i], length(key) + 2)
    return ""
  }
  index($0, p " headless report ") == 1 {
    if (cur != "") { close(cur); close(cur ".start"); close(cur ".end") }
    k = field($0, "n"); seq++; cur = dir "/report." seq; printf "" > cur; open = 1
    print $0 > (cur ".start"); next
  }
  index($0, p " headless end ") == 1 {
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
    echo "run-headless.sh: a report failed its check (got $bytes B cksum $crc; $end)" >&2
  fi
done
# An image that refuses to run (a bad input program, or for headless-qspi
# no valid image in the NOR) says why instead of reporting.
if [ -z "$chosen" ]; then
  grep -E "^$PREFIX headless (qspi |input )?error " "$tmp/serial" | tail -n 1 | sed 's/^/run-headless.sh: the board says: /' >&2
fi
[ -n "$chosen" ] || die "no complete report in ${secs} s of serial ($partial incomplete); serial: $(wc -l < "$tmp/serial") lines"
echo "run-headless.sh: $complete complete report(s), $partial incomplete; comparing the last"
grep "^$PREFIX headless qspi image " "$tmp/serial" | tail -n 1
cat "$chosen.start"
grep '^stats ' "$chosen"
cat "$chosen.end"
# Exactly the two fields, with the values of the .bin; a report without
# them is from an image older than the check.
got_image=$(grep -oE ' image_bytes=[0-9]+ image_cksum=[0-9]+( |$)' "$chosen.start" | sed 's/^ //; s/ $//')
[ "$got_image" = "$want_image" ] ||
  die "the board is not running the flashed image: its report has '${got_image:-no image_bytes/image_cksum}', $img has '$want_image'"
echo "run-headless.sh: image matched: $want_image"
if [ -n "$want_rom" ]; then
  got_rom=$(grep -oE ' rom_len=[0-9]+ rom_hash=[0-9a-f]{16}( |$)' "$chosen.start" | sed 's/^ //; s/ $//')
  [ "$got_rom" = "$want_rom" ] ||
    die "the board's ROM is not $RUN_HEADLESS_ROM: its report has '${got_rom:-no rom_len/rom_hash}', the file is '$want_rom'"
  fallbacks=$(grep -oE ' exit_fallbacks=[0-9]+( |$)' "$chosen.start" | tr -d ' ')
  [ -n "$fallbacks" ] || die "the report has no exit_fallbacks field"
  echo "run-headless.sh: ROM matched: $want_rom ($fallbacks)"
fi

grep '^headless ' "$chosen" > "$tmp/got"

# The display self-test (header): exits 1 if a display image's figures
# are missing, malformed or not a pass; returns for any other image.
check_display() {
  local display=0 bad=0 ready start cpu
  case "$(basename "$img")" in headless-display-*) display=1 ;; esac
  LC_ALL=C grep -aqF 'stats display start=' "$img" && display=1
  grep -q '^stats display' "$chosen" && display=1
  [ "$display" = 1 ] || return 0
  # FRAME_READY frames among the compared lines: ev's last hex digit odd.
  ready=$(grep -E '^headless frame=' "$tmp/got" | grep -cE ' ev=[0-9a-f]{3}[13579bdf]( |$)')
  start=$(grep '^stats display start=' "$chosen")
  cpu=$(grep '^stats display cpu ' "$chosen")
  dbad() { echo "run-headless.sh: display self-test: $*" >&2; bad=1; }
  # need <line name> <line> <key> <want>: the key exactly once, = want.
  need() {
    local got
    got=$(tr ' ' '\n' <<< "$2" | sed -n "s/^$3=//p")
    [ "$got" = "$4" ] || dbad "$1 line: $3=${got:-<missing>} (want $4)"
  }
  [ "$ready" -ge 1 ] || dbad "no compared frame has FRAME_READY"
  [ "$(grep -c '^stats display start=' "$chosen")" = 1 ] ||
    dbad "want exactly one 'stats display start=' line, the report has $(grep -c '^stats display start=' "$chosen")"
  [ "$(grep -c '^stats display cpu ' "$chosen")" = 1 ] ||
    dbad "want exactly one 'stats display cpu' line, the report has $(grep -c '^stats display cpu ' "$chosen")"
  for k in frames transfers readbacks; do need start "$start" "$k" "$ready"; done
  need start "$start" mismatches 0
  need start "$start" control_mismatches 2
  need start "$start" transfer_errors 0
  need start "$start" start_timeouts 0
  need start "$start" dead 0
  need cpu "$cpu" ok 1
  if [ "$bad" != 0 ]; then
    echo "run-headless.sh: DISPLAY SELF-TEST FAILED (lines equal; the panel's readback check did not pass)"
    exit 1
  fi
  echo "run-headless.sh: display self-test passed: $ready FRAME_READY frames, as many transfers and readbacks, 0 mismatches, control 2, 5% budget ok"
}
tr -d '\r' < "$expected" | grep '^headless ' > "$tmp/want"
[ -s "$tmp/want" ] || die "$expected has no headless lines"
if cmp -s "$tmp/want" "$tmp/got"; then
  echo "run-headless.sh: EQUAL: $(wc -l < "$tmp/got") headless lines match $expected"
  check_display
  exit 0
fi
line=$(cmp "$tmp/want" "$tmp/got" 2>&1 | sed -n 's/.* line \([0-9]*\).*/\1/p')
echo "run-headless.sh: DIFFER from $expected (got $(wc -l < "$tmp/got") lines, want $(wc -l < "$tmp/want"))"
if [ -n "$line" ]; then
  echo "  first difference at line $line:"
  echo "  want: $(sed -n "${line}p" "$tmp/want")"
  echo "  got:  $(sed -n "${line}p" "$tmp/got")"
else
  echo "  one is a prefix of the other"
fi
exit 1
