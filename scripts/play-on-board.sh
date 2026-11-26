#!/usr/bin/env bash
# Play a game on the dev board (plan Stage 2 M5; docs/reference/player.md):
#
#   scripts/play-on-board.sh <rom> [--sav <file.sav>] [--sav-out <file.sav>] [--seconds N]
#
# One board session (the board lock, scripts/lib/board-lock.sh, held from
# the first probe to the end of play, so no other agent flashes in
# between):
#   1. the ROM (and --sav) are copied into a scratch dir first; nothing is
#      ever written next to them (the human's games are read-only);
#   2. fast path: if the board already runs the `player` image (a short
#      `gbc-pc play` sees its `Stats`, `image=player`) and its NOR holds
#      this ROM (`gbc-pc load` answers AlreadyLoaded), no flashing at all;
#   3. otherwise: flash `loader`, `gbc-pc load <rom>` into the QSPI NOR,
#      flash `player` (each flash takes ~25 s);
#   4. with --sav: `gbc-pc load <rom> --sav <file>` against the player
#      (AlreadyLoaded, then the save is applied; gbc-pc first syncs its
#      clock footer to the PC's time, as the desktop does on load, ADR 0040);
#   5. `gbc-pc play`: keys to the board, stats printed every 5 s; Esc ends
#      it (or --seconds N); F5 dumps the save to --sav-out (default: the
#      scratch dir, printed), as does joystick CENTER + B1. A dump's clock
#      footer is stamped with the PC's time, so the desktop loads it right.
# Builds gbc-pc and the loader and player images first (crates/gbc-nucleo-h753/build.sh),
# unless PLAY_NO_BUILD=1.
#
#   PLAY_SCRATCH=<dir>     scratch dir (default mktemp -d)
#   PLAY_BAUD=<bps>        link rate (default 2000000, the images' default)
#   GBC_BUILD_LOCK=<file>  run the builds under `flock -x <file>`
#   H7_DRIVE, H7_LABEL     as for flash.sh
#
# Exit: 0 played, 1 a load, save or play step failed, 2 usage, build or
# flash error.
set -uo pipefail
root=$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)
fw=$root/crates/gbc-nucleo-h753
die() { echo "play-on-board: ERROR $*" >&2; exit 2; }
fail() { echo "play-on-board: FAIL $*" >&2; exit 1; }
usage() { echo "usage: $0 <rom> [--sav <file.sav>] [--sav-out <file.sav>] [--seconds N]" >&2; exit 2; }

[ $# -ge 1 ] || usage
rom_in=$1; shift
sav_in="" sav_out="" seconds=""
while [ $# -gt 0 ]; do
  case $1 in
    --sav) [ $# -ge 2 ] || usage; sav_in=$2; shift 2 ;;
    --sav-out) [ $# -ge 2 ] || usage; sav_out=$2; shift 2 ;;
    --seconds) [ $# -ge 2 ] || usage; seconds=$2; shift 2 ;;
    *) usage ;;
  esac
done
[ -f "$rom_in" ] || die "no such ROM: $rom_in"
[ -z "$sav_in" ] || [ -f "$sav_in" ] || die "no such save: $sav_in"
case "$seconds" in ''|*[!0-9]*) [ -z "$seconds" ] || die "--seconds wants a number";; esac
baud=${PLAY_BAUD:-2000000}

if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ] && [ "${PLAY_NO_BUILD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  locked() { if [ -n "${GBC_BUILD_LOCK:-}" ]; then flock -x "$GBC_BUILD_LOCK" "$@"; else "$@"; fi; }
  (cd "$root" && locked cargo build -q -p gbc-pc --release --target x86_64-pc-windows-gnu) || die "building gbc-pc failed"
  "$fw/build.sh" loader player > /dev/null || die "building the loader and player images failed ($fw/build.sh loader player)"
fi
for f in "$fw/out/loader.bin" "$fw/out/player.bin"; do [ -f "$f" ] || die "$f missing (build.sh loader player)"; done

. "$root/scripts/lib/board-lock.sh"
board_lock_reexec "$rom_in" ${sav_in:+--sav "$sav_in"} ${sav_out:+--sav-out "$sav_out"} ${seconds:+--seconds "$seconds"}

scratch=${PLAY_SCRATCH:-$(mktemp -d)}
mkdir -p "$scratch" || die "cannot create $scratch"
name=$(basename "$rom_in")
rom=$scratch/$name
cp "$rom_in" "$rom" || die "cannot copy $rom_in"
sav=""
if [ -n "$sav_in" ]; then sav=$scratch/in-$(basename "$sav_in"); cp "$sav_in" "$sav" || die "cannot copy $sav_in"; fi
[ -n "$sav_out" ] || sav_out=$scratch/${name%.*}.sav
echo "play-on-board: scratch $scratch; ROM $name ($(stat -c %s "$rom") B); dumps go to $sav_out"
pc() { "$root/scripts/gbc-pc.sh" "$@" --baud "$baud"; }

# 2. The fast path.
fast=0
pc play --seconds 2 > "$scratch/probe.txt" 2>&1
if grep -q '^stats: image=player ' "$scratch/probe.txt"; then
  if pc load "$rom" > "$scratch/load-fast.txt" 2>&1 && grep -q '^already loaded' "$scratch/load-fast.txt"; then
    fast=1
    echo "play-on-board: the player is running with this ROM in the NOR: no flashing"
  else
    echo "play-on-board: the player is running with another ROM: loading this one"
  fi
else
  echo "play-on-board: the board is not running the player (or has no ROM): loading"
fi

# 3. Loader, ROM, player.
if [ "$fast" = 0 ]; then
  echo "play-on-board: flashing the loader"
  "$fw/flash.sh" "$fw/out/loader.bin" > "$scratch/flash-loader.txt" 2>&1 || { cat "$scratch/flash-loader.txt"; die "flash.sh loader failed"; }
  sleep 1
  pc load "$rom" > "$scratch/load.txt" 2>&1 || { cat "$scratch/load.txt"; fail "gbc-pc load $name into the NOR failed"; }
  grep -E '^(loaded|already loaded)' "$scratch/load.txt"
  echo "play-on-board: flashing the player"
  "$fw/flash.sh" "$fw/out/player.bin" > "$scratch/flash-player.txt" 2>&1 || { cat "$scratch/flash-player.txt"; die "flash.sh player failed"; }
  sleep 1
fi

# 4. The save.
if [ -n "$sav" ]; then
  pc load "$rom" --sav "$sav" > "$scratch/load-sav.txt" 2>&1 || { cat "$scratch/load-sav.txt"; fail "loading the save failed"; }
  grep -q '^already loaded' "$scratch/load-sav.txt" || { cat "$scratch/load-sav.txt"; fail "the player does not hold $name"; }
  grep -E '^(save|already)' "$scratch/load-sav.txt"
fi

# 5. Play.
echo "play-on-board: playing${seconds:+ for $seconds s} (Esc ends; F5 dumps the save)"
pc play --sav-out "$sav_out" ${seconds:+--seconds "$seconds"}
rc=$?
[ -f "$sav_out" ] && echo "play-on-board: save dump at $sav_out ($(stat -c %s "$sav_out") B)"
exit $rc
