#!/usr/bin/env bash
# One bench run with the workload ROMs in the QSPI NOR as well as in
# internal flash (README.md "QSPI ROM placement"; plan Stage 2 M2,
# `itcm-dtcm-qspi`):
#
#   ./run-qspi.sh out/h753/bench-itcm-dtcm-pll480.bin [read seconds, default 100]
#
# One board session, in order:
#   1. flash.sh the gbc-nucleo-h753 loader (crates/gbc-nucleo-h753/out/
#      loader.bin, ADR 0038);
#   2. scripts/gbc-pc.sh load out/h753/bench-pack.bin (the loader skips the
#      load when the NOR already holds this pack: same length and hash);
#   3. flash.sh <image>; the image runs every workload on its ROM in
#      internal flash, then on the pack's copy through the QSPI window;
#   4. read-serial.sh for the given time into a capture file;
#   5. check-report.sh <capture> <image> --qspi: the banner's image CRC,
#      both sets of lines against expected-hashes.txt, the cycle table.
# Build first (build.sh here and in crates/gbc-nucleo-h753). The board
# lock (scripts/lib/board-lock.sh) is held across all of it.
#
# Exit: check-report.sh's (0 pass, 1 a check failed, 2 no complete
# report); 2 if a flash, the load or the read fails.
#
#   H7_BENCH_RUNS=<dir>    where captures go (default out/h753/runs)
#   GBC_BUILD_LOCK=<file>  build gbc-pc (scripts/gbc-pc.sh builds it) under
#                          `flock -x <file>` before taking the board lock
#   H7_DRIVE, H7_LABEL, H7_COM  as for flash.sh and read-serial.sh
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-qspi.sh: $*" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: run-qspi.sh <image.bin> [read seconds]"
img=$(realpath "$1") secs=${2:-100}
case "$secs" in ''|*[!0-9]*) die "bad read time $secs";; esac
loader=$root/crates/gbc-nucleo-h753/out/loader.bin pack=$here/out/h753/bench-pack.bin
for f in "$img" "$loader" "$pack"; do [ -f "$f" ] || die "$f: no such file (run build.sh)"; done
runs=${H7_BENCH_RUNS:-$here/out/h753/runs}
mkdir -p "$runs" || die "cannot create $runs"

. "$root/scripts/lib/board-lock.sh"
if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  build=(cargo build -p gbc-pc --release --target x86_64-pc-windows-gnu)
  if [ -n "${GBC_BUILD_LOCK:-}" ]; then build=(flock -x "$GBC_BUILD_LOCK" "${build[@]}"); fi
  (cd "$root" && "${build[@]}") || die "building gbc-pc failed"
fi
board_lock_reexec "$@"

# The name only orders the files; no figure depends on this clock.
cap=$runs/$(basename "$img" .bin)-$(date +%Y%m%d-%H%M%S).txt
echo "run-qspi.sh: 1/5 flashing the loader"
"$here/flash.sh" "$loader" || die "flash.sh $loader failed"
echo "run-qspi.sh: 2/5 loading the bench pack into the NOR"
"$root/scripts/gbc-pc.sh" load "$pack" || die "gbc-pc.sh load $pack failed"
echo "run-qspi.sh: 3/5 flashing $img"
"$here/flash.sh" "$img" || die "flash.sh $img failed"
echo "run-qspi.sh: 4/5 reading the report for $secs s into $cap"
"$here/read-serial.sh" "$secs" > "$cap" || die "read-serial.sh failed"
echo "run-qspi.sh: 5/5 checking"
"$here/check-report.sh" "$cap" "$img" --qspi
