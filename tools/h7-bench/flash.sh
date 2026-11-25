#!/usr/bin/env bash
# Flashes one .bin onto the NUCLEO-H755ZI-Q by copying it to the ST-LINK
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
letter=$(findmnt -no SOURCE "$DRIVE" | tr -d '\\')        # "E:"
case "$letter" in [A-Za-z]:) ;; *) die "cannot tell the Windows drive letter of $DRIVE (source: $letter)";; esac
vol=$(cmd.exe /c vol "$letter" 2>/dev/null | tr -d '\r') || die "cannot read the volume label of $letter"
case "$vol" in *"$LABEL"*) ;; *) die "volume $letter is not labelled $LABEL: $vol";; esac
[ ! -e "$DRIVE/FAIL.TXT" ] || echo "flash.sh: note: a FAIL.TXT from an earlier attempt is on the drive:" "$(tr -d '\r' < "$DRIVE/FAIL.TXT")" >&2

echo "flash.sh: copying $img ($size bytes) to $DRIVE ($LABEL)"
cp "$img" "$DRIVE/$(basename "$img")"
sync

# The ST-LINK programs the file, then re-enumerates the drive; on an error
# it leaves a FAIL.TXT with the reason. Give it time, then look.
for _ in $(seq 1 20); do
  sleep 1
  if [ -e "$DRIVE/FAIL.TXT" ]; then
    echo "flash.sh: the ST-LINK reports a failure:" >&2
    tr -d '\r' < "$DRIVE/FAIL.TXT" >&2 || true
    exit 1
  fi
done
echo "flash.sh: copied; no FAIL.TXT after 20 s. Check the board with ./read-serial.sh"
