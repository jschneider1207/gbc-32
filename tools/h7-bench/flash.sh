#!/usr/bin/env bash
# Flashes one .bin onto the NUCLEO-H755ZI-Q by copying it (with a native
# Windows copy) to the ST-LINK
# mass-storage drive, which programs it at 0x0800_0000 (the Cortex-M7's
# flash bank). README.md "Flash".
#
#   ./flash.sh out/selftest.bin
#
#   H7_DRIVE   mount point of the drive in WSL (default /mnt/e)
#   H7_LABEL   expected volume label (default NOD_H755ZIQ)
#
# It refuses anything that does not look like an M7 image for this board,
# and a drive that is not the ST-LINK's. Never used for the M4 stub: the
# drive only programs at 0x0800_0000 (use STM32CubeProgrammer for
# 0x0810_0000).
set -euo pipefail

img=${1:?usage: flash.sh <image.bin>}
DRIVE=${H7_DRIVE:-/mnt/e}
LABEL=${H7_LABEL:-NOD_H755ZIQ}

die() { echo "flash.sh: $*" >&2; exit 1; }
word() { od -An -tx4 -j $(($2 * 4)) -N4 "$1" | tr -d ' \n'; }

[ -f "$img" ] || die "$img: no such file"
case "$img" in *.bin) ;; *) die "$img: not a .bin (the drive takes raw binaries)";; esac
case "$(basename "$img")" in m4-stub*) die "the M4 stub goes to 0x08100000 with STM32CubeProgrammer, not through the drive";; esac
size=$(stat -c %s "$img")
[ "$size" -ge 64 ] && [ "$size" -le $((1024 * 1024)) ] || die "$img: $size bytes is not a plausible image"
sp=$((16#$(word "$img" 0)))
reset=$((16#$(word "$img" 1)))
{ { [ "$sp" -ge $((0x20000000)) ] && [ "$sp" -le $((0x20020000)) ]; } ||
  { [ "$sp" -ge $((0x24000000)) ] && [ "$sp" -le $((0x24080000)) ]; }; } ||
  die "$img: initial SP $(printf 0x%08x "$sp") is not in DTCM or AXI SRAM"
[ "$reset" -ge $((0x08000000)) ] && [ "$reset" -lt $((0x08000000 + size)) ] && [ $((reset & 1)) -eq 1 ] ||
  die "$img: reset vector $(printf 0x%08x "$reset") is not a Thumb address inside the image"

# The drive: mounted, the ST-LINK's files on it, and the right label.
mountpoint -q "$DRIVE" || die "$DRIVE is not a mount point (mount the ST-LINK drive: sudo mount -t drvfs E: /mnt/e)"
[ -e "$DRIVE/DETAILS.TXT" ] || [ -e "$DRIVE/MBED.HTM" ] ||
  die "$DRIVE has neither DETAILS.TXT nor MBED.HTM: not an ST-LINK drive"
letter=$(findmnt -no SOURCE "$DRIVE" | head -n 1 | tr -d '\\')        # "E:"
case "$letter" in [A-Za-z]:) ;; *) die "cannot tell the Windows drive letter of $DRIVE (source: $letter)";; esac
# cmd.exe needs a Windows cwd; its exit status is unreliable here (a broken
# AutoRun entry on this host makes it non-zero), so the label match decides.
vol=$( (cd /mnt/c && cmd.exe /c vol "$letter" 2>/dev/null || true) | tr -d '\r')
case "$vol" in *"$LABEL"*) ;; *) die "volume $letter is not labelled $LABEL: $vol";; esac
[ ! -e "$DRIVE/FAIL.TXT" ] || echo "flash.sh: note: a FAIL.TXT from an earlier attempt is on the drive:" "$(tr -d '\r' < "$DRIVE/FAIL.TXT")" >&2

# The copy is done by Windows itself: a `cp` through the WSL mount of the
# drive proved unreliable for images of a few hundred KB (2026-11-24). The
# image goes to the Windows temp directory first; cmd.exe needs a Windows
# working directory, and its exit status is unreliable on this host (see
# above), so its "1 file(s) copied" message decides.
echo "flash.sh: copying $img ($size bytes) to $letter ($LABEL)"
wintemp=$( (cd /mnt/c && cmd.exe /c "echo %TEMP%" 2>/dev/null || true) | tr -d '\r' | tail -n 1)
[ -n "$wintemp" ] || die "cannot find the Windows temp directory"
cp "$img" "$(wslpath "$wintemp")/h7bench.bin"
out=$( (cd /mnt/c && cmd.exe /c "copy /b %TEMP%\\h7bench.bin $letter\\h7bench.bin" 2>/dev/null || true) | tr -d '\r')
case "$out" in *"1 file(s) copied"*) ;; *) die "the copy to $letter failed: $out";; esac

# The ST-LINK programs the file, resets the target and re-enumerates the
# drive; on an error it leaves a FAIL.TXT with the reason. Give it time,
# then look. Close any program that has the COM port open first: with the
# port held open, one flash was accepted and never programmed.
for _ in $(seq 1 25); do
  sleep 1
  if [ -e "$DRIVE/FAIL.TXT" ]; then
    echo "flash.sh: the ST-LINK reports a failure:" >&2
    tr -d '\r' < "$DRIVE/FAIL.TXT" >&2 || true
    exit 1
  fi
done
echo "flash.sh: copied; no FAIL.TXT after 25 s. Check the board with ./read-serial.sh"
echo "flash.sh: the first lines read may still be the previous image's (the VCP buffers them)"
