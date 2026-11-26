#!/usr/bin/env bash
# The save round trip on the board (plan Stage 2 M5 acceptance, gate B5;
# docs/reference/link-protocol.md "Board side"):
#
#   ./run-save-roundtrip.sh
#
# Every gbc-pc load --sav and dump here runs with --raw-save: this is a
# transport check (the bytes PC -> board -> PC unchanged), so the RTC
# real-time sync on load and the restamp on dump (ADR 0040), which would
# change the footer's clock and timestamp, are off.
#
# One board session (the board lock, scripts/lib/board-lock.sh, held
# across all of it), in order:
#   1. flash.sh out/link-probe.bin (build.sh link-probe: the emulator on
#      out/link-probe.gb, an MBC3+TIMER+RAM+BATTERY ROM that never touches
#      cartridge RAM or the clock);
#   2. gbc-pc dump: the power-on save must differ from the fixture (so the
#      later equality shows the load, not a coincidence);
#   3. gbc-pc load out/link-probe.gb --sav <fixture>: "already loaded"
#      (the probe's ROM), then the save applied (exit 0);
#   4. gbc-pc dump right away, and again after RUN_SAVE_WAIT seconds
#      (default 10, ~600 frames): both byte-identical to the fixture
#      (`cmp`). The fixture's clock is halted (DH bit 6), latched = live;
#   5. a wrong-size save (the fixture plus one byte): refused with `Nak
#      OutOfRange`, and a dump still equals the fixture (nothing applied);
#   6. a bad footer (DH bit 5 set; the right length, refused only at the
#      verdict after every chunk): `Nak OutOfRange`, dump equals the
#      fixture (never half-applied);
#   7. the probe's status line (read at the link rate): saves_applied=1,
#      saves_refused=1, and overruns, framing, noise, bad_frames and
#      replies_dropped all 0.
# gbc-pc's counters must show no resend, Nak or timeout on the steps that
# should succeed. The last line is the verdict: `run-save-roundtrip: PASS
# ...` or `run-save-roundtrip: FAIL <step>: <why>`. Captures and dumps stay
# in the scratch dir (printed).
#
# Exit: 0 pass, 1 a check failed, 2 tool error (missing file, flash,
# serial).
#
#   RUN_SAVE_FIXTURE=<file>   the save loaded (default
#                             tools/gbc-pc/tests/fixtures/link-probe.sav)
#   RUN_SAVE_EXPECT=<file>    what every dump must equal (default the
#                             fixture; a doctored copy shows the cmp fails)
#   RUN_SAVE_WAIT=<s>         seconds between the two dumps (default 10)
#   RUN_SAVE_BAUD=<bps>       the probe's GBC_LINK_BAUD (default 2000000)
#   RUN_SAVE_SCRATCH=<dir>    where dumps and captures go (default mktemp -d)
#   GBC_BUILD_LOCK=<file>     build gbc-pc under `flock -x <file>` before
#                             taking the board lock
set -uo pipefail
here=$(dirname "$(readlink -f "$0")")
root=$(cd "$here/../.." && pwd)
die() { echo "run-save-roundtrip: ERROR $*" >&2; exit 2; }
fail() { echo "run-save-roundtrip: FAIL $*"; exit 1; }

[ $# -eq 0 ] || die "usage: run-save-roundtrip.sh (settings via RUN_SAVE_* variables)"
probe=$here/out/link-probe.bin rom=$here/out/link-probe.gb
fixture=${RUN_SAVE_FIXTURE:-$root/tools/gbc-pc/tests/fixtures/link-probe.sav}
expect=${RUN_SAVE_EXPECT:-$fixture}
wait_s=${RUN_SAVE_WAIT:-10}
baud=${RUN_SAVE_BAUD:-2000000}
case "$wait_s$baud" in ''|*[!0-9]*) die "bad RUN_SAVE_WAIT or RUN_SAVE_BAUD";; esac
for f in "$probe" "$rom" "$fixture" "$expect"; do [ -f "$f" ] || die "$f: no such file (build.sh link-probe)"; done

. "$root/scripts/lib/board-lock.sh"
if [ "${GBC_BOARD_LOCK_HELD:-}" != 1 ]; then
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
  build=(cargo build -p gbc-pc --release --target x86_64-pc-windows-gnu)
  if [ -n "${GBC_BUILD_LOCK:-}" ]; then build=(flock -x "$GBC_BUILD_LOCK" "${build[@]}"); fi
  (cd "$root" && "${build[@]}") || die "building gbc-pc failed"
fi
board_lock_reexec

scratch=${RUN_SAVE_SCRATCH:-$(mktemp -d)}
mkdir -p "$scratch" || die "cannot create $scratch"
echo "run-save-roundtrip: scratch $scratch, fixture $fixture ($(stat -c %s "$fixture") B), expect $expect, at $baud baud, commit $(git -C "$root" rev-parse --short HEAD 2>/dev/null || echo unknown)"
pc() { "$root/scripts/gbc-pc.sh" "$@" --baud "$baud"; }
clean() { grep -q '^link: resends 0, naks 0, timeouts 0, ' "$1"; }
# dump <name>: gbc-pc dump into $scratch/<name>.sav (must succeed cleanly).
dump() {
  pc dump "$scratch/$1.sav" --raw-save > "$scratch/$1.txt" 2>&1 || { cat "$scratch/$1.txt"; fail "$2 dump $1: gbc-pc failed"; }
  grep -E '^(dumped|link:)' "$scratch/$1.txt"
  clean "$scratch/$1.txt" || fail "$2 dump $1: link counters: $(grep '^link:' "$scratch/$1.txt")"
}
same() { cmp "$scratch/$1.sav" "$expect" || fail "$2 dump $1 differs from $expect"; }
field() { sed -n "s/.* $2=\([^ ]*\).*/\1/p" <<< "$1"; }

echo "run-save-roundtrip: 1/7 flashing the link probe"
"$here/flash.sh" "$probe" || die "flash.sh $probe failed"
sleep 1

echo "run-save-roundtrip: 2/7 the power-on save"
dump d0 2
if cmp -s "$scratch/d0.sav" "$fixture"; then fail "2 the power-on save already equals the fixture"; fi

echo "run-save-roundtrip: 3/7 loading the fixture"
pc load "$rom" --sav "$fixture" --raw-save > "$scratch/load.txt" 2>&1 || { cat "$scratch/load.txt"; fail "3 load --sav: gbc-pc failed"; }
cat "$scratch/load.txt"
grep -q '^already loaded (length and hash match)$' "$scratch/load.txt" || fail "3 load: the probe's ROM was not 'already loaded'"
clean "$scratch/load.txt" || fail "3 load: link counters: $(grep '^link:' "$scratch/load.txt")"

echo "run-save-roundtrip: 4/7 dumping now and after $wait_s s"
dump d1 4; same d1 4
sleep "$wait_s"
dump d2 4; same d2 4

echo "run-save-roundtrip: 5/7 a wrong-size save"
cat "$fixture" > "$scratch/long.sav" && printf 'x' >> "$scratch/long.sav"
if pc load "$rom" --sav "$scratch/long.sav" --raw-save > "$scratch/long.txt" 2>&1; then fail "5 wrong size: gbc-pc reported success"; fi
grep -E '^gbc-pc:' "$scratch/long.txt"
grep -q 'Nak OutOfRange' "$scratch/long.txt" || fail "5 wrong size: not refused with Nak OutOfRange"
dump d3 5; same d3 5

echo "run-save-roundtrip: 6/7 a bad footer"
cp "$fixture" "$scratch/badfooter.sav"
dh=$(( $(stat -c %s "$fixture") - 48 + 16 ))
printf '\141' | dd of="$scratch/badfooter.sav" bs=1 seek=$dh conv=notrunc status=none
if pc load "$rom" --sav "$scratch/badfooter.sav" --raw-save > "$scratch/badfooter.txt" 2>&1; then fail "6 bad footer: gbc-pc reported success"; fi
grep -E '^gbc-pc:' "$scratch/badfooter.txt"
grep -q 'Nak OutOfRange' "$scratch/badfooter.txt" || fail "6 bad footer: not refused with Nak OutOfRange"
dump d4 6; same d4 6

echo "run-save-roundtrip: 7/7 the probe's status line"
H7_BAUD=$baud "$root/tools/h7-bench/read-serial.sh" 16 | tr -d '\000' > "$scratch/status.txt" || die "read-serial.sh failed"
st=$(grep -a 'link-probe status ' "$scratch/status.txt" | tail -n 1)
echo "$st"
[ -n "$st" ] || fail "7 status: no status line in $scratch/status.txt"
[ "$(field "$st" saves_applied)" = 1 ] || fail "7 status: saves_applied=$(field "$st" saves_applied)"
[ "$(field "$st" saves_refused)" = 1 ] || fail "7 status: saves_refused=$(field "$st" saves_refused)"
for c in overruns framing noise bad_frames replies_dropped; do
  [ "$(field "$st" "$c")" = 0 ] || fail "7 status: $c=$(field "$st" "$c")"
done
echo "run-save-roundtrip: PASS fixture $(stat -c %s "$fixture") B round-tripped (d1, d2 after ${wait_s} s), wrong size and bad footer refused with nothing applied; board overruns=0 framing=0 noise=0 bad_frames=0"
