#!/usr/bin/env bash
# Flashes one .bin onto the board by copying it (with a native Windows
# copy) to the ST-LINK mass-storage drive, which programs it at
# 0x0800_0000. README.md "Flash".
#
#   ./flash.sh out/h753/selftest.bin
#
#   H7_BENCH_BOARD  h753 (default: NUCLEO-H753ZI) or h755 (NUCLEO-H755ZI-Q)
#   H7_DRIVE        Windows drive letter of the ST-LINK drive (default E:)
#   H7_LABEL        expected volume label (default NOD_H753ZI, or
#                   NOD_H755ZIQ for h755)
#
# Everything on the drive is done through cmd.exe, not a WSL mount: the
# drvfs mount of a removable drive can go stale when the ST-LINK
# re-enumerates, and remounting needs sudo. It refuses an image that does
# not look like an M7 image built for this board (the board name is in
# every image's banner), and a drive that is not the ST-LINK's. Never used
# for the M4 stub: the drive only programs at 0x0800_0000 (use
# STM32CubeProgrammer for 0x0810_0000).
set -euo pipefail

# One flash or serial session at a time (ADR 0036); re-entrant.
. "$(dirname "$(readlink -f "$0")")/../../scripts/lib/board-lock.sh"
board_lock_reexec "$@"

img=${1:?usage: flash.sh <image.bin>}
BOARD=${H7_BENCH_BOARD:-h753}
case "$BOARD" in
  h753) LABEL=${H7_LABEL:-NOD_H753ZI} NAME=NUCLEO-H753ZI ;;
  h755) LABEL=${H7_LABEL:-NOD_H755ZIQ} NAME=NUCLEO-H755ZI-Q ;;
  *) echo "flash.sh: H7_BENCH_BOARD=$BOARD: not h753 or h755" >&2; exit 1 ;;
esac
letter=${H7_DRIVE:-E:}

die() { echo "flash.sh: $*" >&2; exit 1; }
word() { od -An -tx4 -j $(($2 * 4)) -N4 "$1" | tr -d ' \n'; }

[ -f "$img" ] || die "$img: no such file"
case "$img" in *.bin) ;; *) die "$img: not a .bin (the drive takes raw binaries)";; esac
case "$(basename "$img")" in m4-stub*) die "the M4 stub goes to 0x08100000 with STM32CubeProgrammer, not through the drive";; esac
size=$(stat -c %s "$img")
[ "$size" -ge 64 ] && [ "$size" -le $((2048 * 1024)) ] || die "$img: $size bytes is not a plausible image"
grep -qaF "$NAME" "$img" || die "$img: not built for the $NAME (no board name in it; H7_BENCH_BOARD?)"
sp=$((16#$(word "$img" 0)))
reset=$((16#$(word "$img" 1)))
{ { [ "$sp" -ge $((0x20000000)) ] && [ "$sp" -le $((0x20020000)) ]; } ||
  { [ "$sp" -ge $((0x24000000)) ] && [ "$sp" -le $((0x24080000)) ]; }; } ||
  die "$img: initial SP $(printf 0x%08x "$sp") is not in DTCM or AXI SRAM"
[ "$reset" -ge $((0x08000000)) ] && [ "$reset" -lt $((0x08000000 + size)) ] && [ $((reset & 1)) -eq 1 ] ||
  die "$img: reset vector $(printf 0x%08x "$reset") is not a Thumb address inside the image"

# The drive: the right label and the ST-LINK's files on it.
case "$letter" in [A-Za-z]:) ;; *) die "H7_DRIVE=$letter is not a drive letter like E:";; esac
# cmd.exe needs a Windows cwd; its exit status is unreliable here (a broken
# AutoRun entry on this host makes it non-zero), so its output decides.
win() { (cd /mnt/c && cmd.exe /c "$1" 2>/dev/null || true) | tr -d '\r'; }
vol=$(win "vol $letter")
case "$vol" in *"$LABEL"*) ;; *) die "volume $letter is not labelled $LABEL: $vol";; esac
case "$(win "if exist $letter\\DETAILS.TXT echo yes")" in
  *yes*) ;; *) die "$letter has no DETAILS.TXT: not an ST-LINK drive";;
esac
fail_txt() { win "if exist $letter\\FAIL.TXT type $letter\\FAIL.TXT"; }
old=$(fail_txt)
[ -z "$old" ] || echo "flash.sh: note: a FAIL.TXT from an earlier attempt is on the drive: $old" >&2

# The copy is done by Windows itself: a `cp` through the WSL mount of the
# drive proved unreliable for images of a few hundred KB (2026-11-24). The
# image goes to the Windows temp directory first; its "1 file(s) copied"
# message decides (see above).
echo "flash.sh: copying $img ($size bytes) to $letter ($LABEL)"
wintemp=$(win "echo %TEMP%" | tail -n 1)
[ -n "$wintemp" ] || die "cannot find the Windows temp directory"
cp "$img" "$(wslpath "$wintemp")/h7bench.bin"
out=$(win "copy /b %TEMP%\\h7bench.bin $letter\\h7bench.bin")
case "$out" in *"1 file(s) copied"*) ;; *) die "the copy to $letter failed: $out";; esac

# The ST-LINK programs the file, resets the target and re-enumerates the
# drive; on an error it leaves a FAIL.TXT with the reason. Give it time,
# then look. Close any program that has the COM port open first: with the
# port held open, one flash was accepted and never programmed.
for _ in $(seq 1 25); do
  sleep 1
  now=$(fail_txt)
  if [ -n "$now" ]; then
    echo "flash.sh: the ST-LINK reports a failure: $now" >&2
    exit 1
  fi
done
echo "flash.sh: copied; no FAIL.TXT after 25 s. Check the board with ./read-serial.sh"
echo "flash.sh: the first lines read may still be the previous image's (the VCP buffers them)"
