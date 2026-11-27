#!/usr/bin/env bash
# The save round trip on the `player` image (plan Stage 2 M5 acceptance:
# "A save round-trips byte-identically, PC -> board -> PC: load a `.sav`
# (with RTC footer) with a ROM, dump it before and after a scripted run
# that doesn't touch cart RAM, and the file equals the original byte for
# byte"; docs/reference/m5-evidence.md):
#
#   ./run-player-save.sh
#
# Every gbc-pc load --sav and dump here runs with --raw-save: this is a
# transport check (the bytes PC -> board -> PC unchanged), so the RTC
# real-time sync on load and the restamp on dump (ADR 0040), which would
# change the footer's clock and timestamp, are off.
#
# run-save-roundtrip.sh does this against the link probe; this is the same
# round trip against the image that plays games. The `player-script` build
# has the link off (player.md "Scripted mode"), so the run is the live
# `player` with no input: its ROM is out/link-probe.gb (build.sh
# link-probe: `nop; jr -2`, MBC3+TIMER+RAM+BATTERY, 32 KiB RAM), which
# reads no buttons and never touches cartridge RAM or the clock, so the
# run is the same whatever input there is.
#
# One board session (the board lock, scripts/lib/board-lock.sh, held
# across all of it), in order:
#   1. flash `loader`, `gbc-pc load out/link-probe.gb` into the QSPI NOR
#      ("loaded" or "already loaded"), flash `player`;
#   2. gbc-pc dump: the power-on save must differ from the fixture (so the
#      later equality shows the load, not a coincidence);
#   3. gbc-pc load out/link-probe.gb --sav <fixture>: "already loaded"
#      (the player's ROM), then the save applied (exit 0);
#   4. gbc-pc dump right away (d0); `gbc-pc play --seconds RUN_SAVE_WAIT`
#      (default 12, ~700 frames; no keys; Stats come every 300 frames, so
#      12 s hold at least two) whose last `Stats` must show
#      saves_applied=1, rx_overflow=0 (the link receive ring never
#      lapped, M7 C5), rx_dma_errors=0 (its DMA stream never stopped,
#      K11) and total_frames grown by >= 300 (one Stats window)
#      over the first one (the emulator ran); gbc-pc dump again
#      (d1). Both must be byte-identical to RUN_SAVE_EXPECT (`cmp`). The
#      default fixture's clock is halted (DH bit 6), latched = live, so
#      emulated time leaves it unchanged.
# gbc-pc's counters must show no resend, Nak or timeout on the load and
# the dumps. The last line is the verdict: `run-player-save: PASS ...` or
# `run-player-save: FAIL <step>: <why>`. Dumps and captures stay in the
# scratch dir (printed).
#
# Exit: 0 pass, 1 a check failed, 2 tool error (missing file, build,
# flash, a gbc-pc failure on a setup step, no Stats line).
#
#   RUN_SAVE_FIXTURE=<file>   the save loaded (default
#                             tools/gbc-pc/tests/fixtures/link-probe.sav)
#   RUN_SAVE_EXPECT=<file>    what d0 and d1 must equal (default the
#                             fixture; a doctored copy shows the cmp fails)
#   RUN_SAVE_WAIT=<s>         seconds of play between the dumps (default 12,
#                             at least 11)
#   RUN_SAVE_BAUD=<bps>       the player's link rate (default 2000000)
#   RUN_SAVE_SCRATCH=<dir>    where dumps and captures go (default mktemp -d)
#   RUN_SAVE_NO_BUILD=1       use the existing out/{loader,player}.bin
#   GBC_BUILD_LOCK=<file>     run the builds under `flock -x <file>` before
#                             taking the board lock
#   H7_DRIVE, H7_LABEL        as for flash.sh
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-player-save: ERROR $*" >&2; exit 2; }
fail() { echo "run-player-save: FAIL $*"; exit 1; }

[ $# -eq 0 ] || die "usage: run-player-save.sh (settings via RUN_SAVE_* variables)"
rom=$here/out/link-probe.gb
fixture=${RUN_SAVE_FIXTURE:-$root/tools/gbc-pc/tests/fixtures/link-probe.sav}
expect=${RUN_SAVE_EXPECT:-$fixture}
wait_s=${RUN_SAVE_WAIT:-12}
baud=${RUN_SAVE_BAUD:-2000000}
case "$wait_s" in ''|*[!0-9]*) die "bad RUN_SAVE_WAIT";; esac
case "$baud" in ''|*[!0-9]*) die "bad RUN_SAVE_BAUD";; esac
[ "$wait_s" -ge 11 ] || die "RUN_SAVE_WAIT must be >= 11 (two Stats lines)"

. "$root/scripts/lib/board-lock.sh"
if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ] && [ "${RUN_SAVE_NO_BUILD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  locked() { if [ -n "${GBC_BUILD_LOCK:-}" ]; then flock -x "$GBC_BUILD_LOCK" "$@"; else "$@"; fi; }
  (cd "$root" && locked cargo build -q -p gbc-pc --release --target x86_64-pc-windows-gnu) || die "building gbc-pc failed"
  "$here/build.sh" loader player link-probe > /dev/null || die "build.sh loader player link-probe failed"
fi
for f in "$here/out/loader.bin" "$here/out/player.bin" "$rom" "$fixture" "$expect"; do
  [ -f "$f" ] || die "$f: no such file (build.sh loader player link-probe)"
done
board_lock_reexec

scratch=${RUN_SAVE_SCRATCH:-$(mktemp -d)}
mkdir -p "$scratch" || die "cannot create $scratch"
echo "run-player-save: scratch $scratch, fixture $fixture ($(stat -c %s "$fixture") B), expect $expect, at $baud baud, commit $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
pc() { "$root/scripts/gbc-pc.sh" "$@" --baud "$baud" < /dev/null; }
clean() { grep -q '^link: resends 0, naks 0, timeouts 0, ' "$1"; }
# dump <name> <step>: gbc-pc dump into $scratch/<name>.sav (must succeed cleanly).
dump() {
  pc dump "$scratch/$1.sav" --raw-save > "$scratch/$1.txt" 2>&1 || { cat "$scratch/$1.txt"; fail "$2 dump $1: gbc-pc failed"; }
  grep -E '^(dumped|link:)' "$scratch/$1.txt"
  clean "$scratch/$1.txt" || fail "$2 dump $1: link counters: $(grep '^link:' "$scratch/$1.txt")"
}
same() { cmp "$scratch/$1.sav" "$expect" || fail "$2 dump $1 differs from $expect"; }
field() { tr ' ' '\n' <<< "$1" | sed -n "s/^$2=//p"; }

echo "run-player-save: 1/4 the ROM into the NOR, then the player"
"$here/flash.sh" "$here/out/loader.bin" > "$scratch/flash-loader.txt" 2>&1 || { tail -5 "$scratch/flash-loader.txt"; die "flash.sh loader failed"; }
sleep 1
pc load "$rom" > "$scratch/load-rom.txt" 2>&1 || { cat "$scratch/load-rom.txt"; die "gbc-pc load (loader) failed"; }
grep -E '^(loaded|already loaded)' "$scratch/load-rom.txt" || die "gbc-pc load (loader): no 'loaded' line"
"$here/flash.sh" "$here/out/player.bin" > "$scratch/flash-player.txt" 2>&1 || { tail -5 "$scratch/flash-player.txt"; die "flash.sh player failed"; }
sleep 1

echo "run-player-save: 2/4 the power-on save"
dump dp 2
if cmp -s "$scratch/dp.sav" "$fixture"; then fail "2 the power-on save already equals the fixture"; fi

echo "run-player-save: 3/4 loading the fixture"
pc load "$rom" --sav "$fixture" --raw-save > "$scratch/load.txt" 2>&1 || { cat "$scratch/load.txt"; fail "3 load --sav: gbc-pc failed"; }
cat "$scratch/load.txt"
grep -q '^already loaded (length and hash match)$' "$scratch/load.txt" || fail "3 load: the player's ROM was not 'already loaded'"
clean "$scratch/load.txt" || fail "3 load: link counters: $(grep '^link:' "$scratch/load.txt")"

echo "run-player-save: 4/4 dump, $wait_s s of play, dump"
dump d0 4
pc play --seconds "$wait_s" > "$scratch/play.txt" 2>&1 || { tail -5 "$scratch/play.txt"; die "gbc-pc play failed"; }
stats=$(grep -a '^stats: image=player ' "$scratch/play.txt")
[ "$(wc -l <<< "$stats")" -ge 2 ] || { cat "$scratch/play.txt"; die "4 play: fewer than two 'stats: image=player' lines"; }
first=$(head -n 1 <<< "$stats") last=$(tail -n 1 <<< "$stats")
echo "$last"
f0=$(field "$first" total_frames) f1=$(field "$last" total_frames) applied=$(field "$last" saves_applied)
for v in "$f0" "$f1" "$applied"; do [[ "$v" =~ ^[0-9]+$ ]] || die "4 play: malformed Stats: $last"; done
[ "$applied" = 1 ] || fail "4 play: saves_applied=$applied (want 1)"
ovf=$(field "$last" rx_overflow)
[ "$ovf" = 0 ] || fail "4 play: rx_overflow=$ovf (want 0: the link receive ring was lapped, M7 C5)"
dmae=$(field "$last" rx_dma_errors)
[ "$dmae" = 0 ] || fail "4 play: rx_dma_errors=$dmae (want 0: the link receive DMA stream stopped on an error, K11)"
[ $((f1 - f0)) -ge 300 ] || fail "4 play: total_frames $f0 -> $f1 in $wait_s s (the emulator did not run)"
dump d1 4
same d0 4; same d1 4
echo "run-player-save: PASS fixture $(stat -c %s "$fixture") B round-tripped through the player (d0, and d1 after $wait_s s of play; Stats total_frames $f0 -> $f1), saves_applied=1"
