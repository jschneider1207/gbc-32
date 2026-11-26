#!/usr/bin/env bash
# The QEMU smoke run of the `headless` loop (plan Stage 2 M1, gate H6):
# builds this image once per ROM (the ROM is embedded), runs it on QEMU
# `mps2-an500` (Cortex-M7) with a timeout, and diffs its lines starting
# `headless ` with the expected file's lines starting `headless ` (a
# trailing `\r` stripped from both; `stats ` lines are not compared).
#
#   ./run.sh [<rom> ...]   ROM names: cpu_instrs, cgb-acid2 (default: both,
#                          the M1 acceptance ROMs), each compared against
#                          tools/headless/expected/<rom>.txt
#
#   GBC_TEST_ROMS=<dir>    where the test ROMs are (default: the repo's
#                          test-roms/; scripts/fetch-test-roms.sh)
#   GBC_BUILD_LOCK=<file>  run every cargo and QEMU command under
#                          `flock -x <file>` (agents: the shared build lock)
#   HEADLESS_EXPECTED=<dir> the expected files (default
#                          tools/headless/expected; for checking this script)
#   QEMU=<binary>          default qemu-system-arm
#   HEADLESS_QEMU_TIMEOUT  seconds per QEMU run (default 300; 600 frames of
#                          either ROM take about 1.5 s)
#   GBC_ITCM_DROP          as for gbc-nucleo-h753 (build.rs)
#
# Exit: 0 every ROM equal; 1 some ROM differs (first differing line
# printed); 2 tool error (bad arguments, missing ROM or expected file,
# build failure, QEMU missing, failing, timing out or exiting non-zero,
# no thunks in the image). A tool error wins over a difference.
#
# The image is the gbc-nucleo-h753 `headless` image's placement on QEMU's
# map: the hot set in a stand-in ITCM far from the flash code. The
# placement is only exercised if the linker needed long-branch thunks, so
# an image with none is a tool error (as h7-bench's qemu-itcm run).
set -uo pipefail
here=$(cd "$(dirname "$0")" && pwd)
cd "$here"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

die() { echo "run.sh: $*" >&2; exit 2; }
TARGET=thumbv7em-none-eabihf
ROOT=$(cd ../.. && pwd)
ROMS=${GBC_TEST_ROMS:-$ROOT/test-roms}
EXPECTED=${HEADLESS_EXPECTED:-$ROOT/tools/headless/expected}
QEMU=${QEMU:-qemu-system-arm}
SECS=${HEADLESS_QEMU_TIMEOUT:-300}
case "$SECS" in ''|*[!0-9]*|0) die "HEADLESS_QEMU_TIMEOUT=$SECS is not a positive number";; esac
command -v "$QEMU" >/dev/null || die "QEMU binary '$QEMU' not found (qemu-system-arm, package qemu-system-arm)"
command -v readelf >/dev/null || die "readelf (binutils) not found"

# Path-independent builds, as gbc-nucleo-h753/build.sh.
REMAP=(--config "target.$TARGET.rustflags=['--remap-path-prefix=$ROOT=/gbc-32','--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/registry/src=/cargo-registry','--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup']")

locked() { if [ -n "${GBC_BUILD_LOCK:-}" ]; then flock -x "$GBC_BUILD_LOCK" "$@"; else "$@"; fi; }

rom_path() {
  case "$1" in
    cpu_instrs) echo "$ROMS/blargg/cpu_instrs/cpu_instrs.gb" ;;
    cgb-acid2) echo "$ROMS/cgb-acid2/cgb-acid2.gbc" ;;
    *) return 1 ;;
  esac
}

[ $# -gt 0 ] || set -- cpu_instrs cgb-acid2
OUT=out
mkdir -p "$OUT"
differ=0
for rom in "$@"; do
  path=$(rom_path "$rom") || die "unknown ROM '$rom' (cpu_instrs, cgb-acid2)"
  [ -f "$path" ] || die "$rom: no ROM at $path (scripts/fetch-test-roms.sh, or set GBC_TEST_ROMS)"
  want=$EXPECTED/$rom.txt
  [ -f "$want" ] || die "$rom: no expected file $want"
  echo "== $rom"
  GBC_HEADLESS_ROM=$(realpath "$path") locked cargo build --release --quiet "${REMAP[@]}" ||
    die "$rom: build failed"
  elf=$OUT/headless-qemu-$rom.elf
  cp "target/$TARGET/release/headless-qemu" "$elf"
  thunks=$(readelf -sW "$elf" | grep -c 'Thumbv7ABSLongThunk' || true)
  [ "$thunks" -gt 0 ] || die "$rom: no long-branch thunks, so the ITCM placement was not exercised"

  start=$(date +%s%N)
  locked timeout "$SECS" "$QEMU" -M mps2-an500 -cpu cortex-m7 -display none -monitor none \
    -serial none -semihosting-config enable=on,target=native -kernel "$elf" > "$OUT/$rom.txt"
  rc=$?
  ms=$((($(date +%s%N) - start) / 1000000))
  [ "$rc" -ne 124 ] || die "$rom: QEMU timed out after $SECS s"
  [ "$rc" -eq 0 ] || die "$rom: QEMU exited $rc (a panic, a write error or a bad input program)"
  echo "  QEMU ran in ${ms} ms; $thunks thunks; output $OUT/$rom.txt"
  grep '^stats ' "$OUT/$rom.txt" | sed 's/^/  /'

  tr -d '\r' < "$OUT/$rom.txt" | grep '^headless ' > "$OUT/$rom.got"
  tr -d '\r' < "$want" | grep '^headless ' > "$OUT/$rom.want"
  [ -s "$OUT/$rom.want" ] || die "$want has no headless lines"
  if cmp -s "$OUT/$rom.want" "$OUT/$rom.got"; then
    echo "  EQUAL: $(wc -l < "$OUT/$rom.got") headless lines match $want"
    continue
  fi
  differ=1
  line=$(cmp "$OUT/$rom.want" "$OUT/$rom.got" 2>&1 | sed -n 's/.* line \([0-9]*\).*/\1/p')
  echo "  DIFFER from $want (got $(wc -l < "$OUT/$rom.got") lines, want $(wc -l < "$OUT/$rom.want"))"
  if [ -n "$line" ]; then
    echo "  first difference at line $line:"
    echo "  want: $(sed -n "${line}p" "$OUT/$rom.want")"
    echo "  got:  $(sed -n "${line}p" "$OUT/$rom.got")"
  else
    echo "  one is a prefix of the other"
  fi
done
[ "$differ" -eq 0 ] || { echo "run.sh: DIFFER" >&2; exit 1; }
echo "run.sh: EQUAL"
