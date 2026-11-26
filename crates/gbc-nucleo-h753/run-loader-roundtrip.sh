#!/usr/bin/env bash
# The loader's round trip on the board (plan Stage 2 M2 acceptance,
# docs/reference/m2-evidence.md):
#
#   ./run-loader-roundtrip.sh [--variant N] [--interrupt]
#
# One board session (the board lock, scripts/lib/board-lock.sh, is held
# across all of it), in order:
#   1. gen-test-image.sh: a 4 MiB pseudo-random image (variant N, default
#      1) and its FNV-1a 64, into a scratch dir;
#   2. flash.sh out/loader.bin;
#   3. gbc-pc load <image>: must print the same FNV for the file, end with
#      "verified hash <fnv>", and report link counters all 0;
#   4. the loader's status line (read at the loader's rate): the stored
#      image must be `valid len=4194304 hash=<fnv>`, and the board's
#      overruns, framing, noise, bad_frames and duplicates all 0;
#   5. gbc-pc load <image> again: must print "already loaded";
#   6. flash.sh out/qspi-mmap.bin and read it: at least one complete run,
#      every `compare` and `unaligned` line PASS, no FAIL line, and its
#      `first16=` the image header for 4 MiB (magic, version 1, flags 0,
#      length 0x400000);
#   7. with --interrupt: flash the loader, start a load of variant N+1,
#      kill gbc-pc.exe (taskkill.exe) after RUN_LOADER_KILL_SECS (default
#      10), and require the status line's `image=none(Erased)` with a load
#      in progress (the old image's header went with the block-0 erase and
#      no header was written for the partial one, ADR 0037). This leaves
#      the NOR without an image.
# The last line is the verdict: `run-loader-roundtrip: PASS ...` or
# `run-loader-roundtrip: FAIL <step>: <why>`. Captures stay in the scratch
# dir (printed).
#
# Exit: 0 pass, 1 a check failed, 2 tool error (build, flash, serial).
#
#   RUN_LOADER_BAUD=<bps>     the rate out/loader.bin serves at (its
#                             GBC_LOADER_BAUD; default gbc-pc's default,
#                             2000000)
#   RUN_LOADER_SCRATCH=<dir>  where images and captures go (default: a new
#                             mktemp -d)
#   GBC_BUILD_LOCK=<file>     build gbc-pc under `flock -x <file>` before
#                             taking the board lock
#   H7_DRIVE, H7_LABEL, H7_COM  as for flash.sh and read-serial.sh
# Build the images first (build.sh).
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-loader-roundtrip: ERROR $*" >&2; exit 2; }
fail() { echo "run-loader-roundtrip: FAIL $*"; exit 1; }

variant=1 interrupt=0
args=("$@")
while [ $# -gt 0 ]; do
  case $1 in
    --variant) variant=${2:-}; shift 2 || die "--variant needs a value" ;;
    --interrupt) interrupt=1; shift ;;
    *) die "usage: run-loader-roundtrip.sh [--variant N] [--interrupt]" ;;
  esac
done
case "$variant" in ''|*[!0-9]*) die "bad variant $variant";; esac
[ "$variant" -ge 1 ] && [ "$variant" -le 254 ] || die "variant $variant not in 1..254"
baud=${RUN_LOADER_BAUD:-2000000}
case "$baud" in ''|*[!0-9]*) die "bad RUN_LOADER_BAUD $baud";; esac
kill_secs=${RUN_LOADER_KILL_SECS:-10}
loader=$here/out/loader.bin mmap=$here/out/qspi-mmap.bin
for f in "$loader" "$mmap"; do [ -f "$f" ] || die "$f: no such file (run build.sh)"; done

. "$root/scripts/lib/board-lock.sh"
if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  build=(cargo build -p gbc-pc --release --target x86_64-pc-windows-gnu)
  if [ -n "${GBC_BUILD_LOCK:-}" ]; then build=(flock -x "$GBC_BUILD_LOCK" "${build[@]}"); fi
  (cd "$root" && "${build[@]}") || die "building gbc-pc failed"
fi
board_lock_reexec "${args[@]}"

scratch=${RUN_LOADER_SCRATCH:-$(mktemp -d)}
mkdir -p "$scratch" || die "cannot create $scratch"
echo "run-loader-roundtrip: scratch $scratch, loader at $baud baud, commit $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
pc() { "$root/scripts/gbc-pc.sh" "$@" --baud "$baud"; }
# status <capture>: read the loader for 15 s (a status line every 10 s of
# idle) and print its last status line.
status() {
  H7_BAUD=$baud "$root/tools/h7-bench/read-serial.sh" 15 | tr -d '\000' > "$1" || die "read-serial.sh failed"
  grep -a 'loader status ' "$1" | tail -n 1
}
# field <line> <name>: the value of ` name=value` in a status line.
field() { sed -n "s/.* $2=\([^ ]*\).*/\1/p" <<< "$1"; }

echo "run-loader-roundtrip: 1/6 generating variant $variant"
img=$scratch/test-image-$variant.bin
gen=$("$here/gen-test-image.sh" "$img" "$variant") || die "gen-test-image.sh failed"
echo "$gen"
fnv=${gen##*fnv1a64=}
[ ${#fnv} -eq 16 ] || die "no FNV from gen-test-image.sh"

echo "run-loader-roundtrip: 2/6 flashing the loader"
"$here/flash.sh" "$loader" || die "flash.sh $loader failed"

echo "run-loader-roundtrip: 3/6 loading $img"
pc load "$img" 2>&1 | tee "$scratch/load1.txt"
grep -q "^ROM: 4194304 bytes, FNV-1a 64 0x$fnv\$" "$scratch/load1.txt" ||
  fail "3 load: gbc-pc's FNV of the file is not $fnv"
grep -q "verified hash 0x$fnv\$" "$scratch/load1.txt" || fail "3 load: no 'verified hash 0x$fnv'"
grep -q '^link: resends 0, naks 0, timeouts 0, stale 0, corrupt 0, other 0$' "$scratch/load1.txt" ||
  fail "3 load: link counters not all 0: $(grep '^link:' "$scratch/load1.txt")"
load1=$(grep -o 'loaded 4194304 bytes in [0-9.]* s ([0-9]* B/s effective)' "$scratch/load1.txt")

echo "run-loader-roundtrip: 4/6 reading the loader's status"
st=$(status "$scratch/status1.txt")
echo "$st"
[ -n "$st" ] || fail "4 status: no status line in $scratch/status1.txt"
grep -q " image=valid len=4194304 hash=0x$fnv " <<< "$st" || fail "4 status: stored image is not len=4194304 hash=0x$fnv"
for c in overruns framing noise bad_frames duplicates; do
  [ "$(field "$st" "$c")" = 0 ] || fail "4 status: $c=$(field "$st" "$c")"
done
board="overruns=0 framing=0 noise=0 bad_frames=0 duplicates=0"

echo "run-loader-roundtrip: 5/6 loading it again"
pc load "$img" 2>&1 | tee "$scratch/load2.txt"
grep -q '^already loaded (length and hash match)$' "$scratch/load2.txt" || fail "5 reload: not 'already loaded'"

echo "run-loader-roundtrip: 6/6 flashing and reading qspi-mmap"
"$here/flash.sh" "$mmap" || die "flash.sh $mmap failed"
"$root/tools/h7-bench/read-serial.sh" 14 | tr -d '\000' > "$scratch/qspi-mmap.txt" || die "read-serial.sh failed"
cap=$scratch/qspi-mmap.txt
# The header's first 16 bytes for a 4 MiB image (rom_image: b"GBC32ROM",
# version 1, flags 0, length 0x400000, little-endian).
want16=4742433332524f4d0100000000004000
! grep -aq 'FAIL' "$cap" || fail "6 qspi-mmap: $(grep -a FAIL "$cap" | head -n 1)"
results=$(grep -ac 'qspi-mmap result run=[0-9]* .* PASS' "$cap")
[ "$results" -ge 1 ] || fail "6 qspi-mmap: no complete PASS run in $cap"
grep -aq 'qspi-mmap compare .* PASS' "$cap" || fail "6 qspi-mmap: no compare PASS"
grep -aq 'qspi-mmap unaligned .* PASS' "$cap" || fail "6 qspi-mmap: no unaligned PASS"
# Only whole lines (the first report after a flash can be cut short).
heads=$(grep -aoE 'qspi-mmap config .* first16=[0-9a-f]{32}$' "$cap" | sed 's/.*first16=//' | sort -u)
[ "$heads" = "$want16" ] || fail "6 qspi-mmap: first16 '${heads//$'\n'/ }' is not $want16"
grep -a 'qspi-mmap compare \|qspi-mmap unaligned \|qspi-mmap result ' "$cap" | tail -n 3

verdict="PASS variant=$variant fnv1a64=$fnv baud=$baud; $load1; $board; reload already loaded; qspi-mmap $results run(s) PASS first16=$want16"

if [ "$interrupt" = 1 ]; then
  next=$((variant + 1))
  echo "run-loader-roundtrip: 7 interrupted load of variant $next after ${kill_secs} s"
  img2=$scratch/test-image-$next.bin
  "$here/gen-test-image.sh" "$img2" "$next" || die "gen-test-image.sh failed"
  "$here/flash.sh" "$loader" || die "flash.sh $loader failed"
  pc load "$img2" > "$scratch/load-interrupted.txt" 2>&1 &
  pid=$!
  sleep "$kill_secs"
  (cd /mnt/c && taskkill.exe /IM gbc-pc.exe /F) || die "taskkill.exe failed"
  wait "$pid"
  tail -n 3 "$scratch/load-interrupted.txt"
  st=$(status "$scratch/status-interrupted.txt")
  echo "$st"
  grep -q ' image=none(Erased) ' <<< "$st" || fail "7 interrupt: stored image is not none(Erased)"
  grep -q ' state=loading([0-9]*/4194304) ' <<< "$st" || fail "7 interrupt: no load in progress"
  verdict="$verdict; interrupted at $(field "$st" state): image=none(Erased)"
fi
echo "run-loader-roundtrip: $verdict"
