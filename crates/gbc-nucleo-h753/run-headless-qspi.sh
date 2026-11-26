#!/usr/bin/env bash
# Runs the `headless-qspi` image on a ROM loaded into the QSPI NOR and
# compares its hash lines with the host's (plan Stage 2 M2 acceptance):
#
#   ./run-headless-qspi.sh ../../test-roms/blargg/cpu_instrs/cpu_instrs.gb ../../tools/headless/expected/cpu_instrs.txt
#
# One board session, in order:
#   1. flash.sh out/loader.bin (the ROM loader, ADR 0038);
#   2. scripts/gbc-pc.sh load <rom> (the loader skips the load when the
#      NOR already holds this ROM: same length and hash);
#   3. RUN_HEADLESS_ROM=<rom> run-headless.sh out/headless-qspi.bin
#      <expected.txt>: flashes the QSPI image, reads its report, requires
#      its image_bytes/image_cksum to be the .bin's and its rom_len/rom_hash
#      (the NOR image's header, checked by the image against the data) to
#      be <rom>'s length and FNV-1a 64 computed from the file, then diffs
#      the `headless ` lines.
# Build the images first (build.sh). The board lock (scripts/lib/
# board-lock.sh) is held across all three steps (the inner scripts see
# GBC_BOARD_LOCK_HELD and do not take it again), so no other board user
# can flash or load in between.
#
# Exit: run-headless.sh's (0 equal, 1 differ, 2 tool error); 2 if the
# loader flash or the load fails.
#
#   GBC_BUILD_LOCK=<file>  build gbc-pc (scripts/gbc-pc.sh builds it) under
#                          `flock -x <file>` before taking the board lock, so
#                          the build inside gbc-pc.sh is a no-op
#   RUN_HEADLESS_READ_SECS, H7_DRIVE, H7_LABEL, H7_COM  as for run-headless.sh
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-headless-qspi.sh: $*" >&2; exit 2; }
[ $# -eq 2 ] || die "usage: run-headless-qspi.sh <rom> <expected.txt>"
[ -f "$1" ] || die "$1: no such ROM"
[ -f "$2" ] || die "$2: no such file"
# gbc-pc.sh runs from the repository root: absolute paths.
rom=$(realpath "$1") expected=$(realpath "$2")
loader=$here/out/loader.bin image=$here/out/headless-qspi.bin
for f in "$loader" "$image"; do [ -f "$f" ] || die "$f: no such file (run build.sh)"; done

. "$root/scripts/lib/board-lock.sh"
if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  build=(cargo build -p gbc-pc --release --target x86_64-pc-windows-gnu)
  if [ -n "${GBC_BUILD_LOCK:-}" ]; then build=(flock -x "$GBC_BUILD_LOCK" "${build[@]}"); fi
  (cd "$root" && "${build[@]}") || die "building gbc-pc failed"
fi
board_lock_reexec "$rom" "$expected"

echo "run-headless-qspi.sh: 1/3 flashing the loader"
"$here/flash.sh" "$loader" || die "flash.sh $loader failed"
echo "run-headless-qspi.sh: 2/3 loading $rom into the NOR"
"$root/scripts/gbc-pc.sh" load "$rom" || die "gbc-pc.sh load $rom failed"
echo "run-headless-qspi.sh: 3/3 running $image"
RUN_HEADLESS_ROM=$rom "$here/run-headless.sh" "$image" "$expected"
