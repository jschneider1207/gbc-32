#!/usr/bin/env bash
# Tests crates/gbc-nucleo-h753/run-headless.sh on synthetic serial captures
# (RUN_HEADLESS_CAPTURE, which never flashes, reads the port or takes the
# board lock): report completeness checks, image check, RUN_HEADLESS_ROM,
# line comparison and every display self-test field. No board. Exit 0 = all pass.
# RUN_HEADLESS overrides the script under test (used to mutation-check this test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${RUN_HEADLESS:-$here/../crates/gbc-nucleo-h753/run-headless.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
# Belt and braces: a run that did reach the lock would use a temp file.
export GBC_BOARD_LOCK=$tmp/lock
unset RUN_HEADLESS_ROM RUN_HEADLESS_SAVE RUN_HEADLESS_NO_FLASH
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
P=gbc-nucleo-h753

printf 'synthetic image\n' > "$tmp/headless-cpu.bin"
printf "other image\n" > "$tmp/other.bin"
read -r icrc ibytes < <(cksum < "$tmp/headless-cpu.bin")
imgf="image_bytes=$ibytes image_cksum=$icrc"
printf 'a' > "$tmp/rom.gb"                       # FNV-1a 64 of "a" = af63dc4c8601ec8c (published test vector)
romf="rom_len=1 rom_hash=af63dc4c8601ec8c exit_fallbacks=0"

printf 'headless frame=1 hash=aa ev=0001\nheadless frame=2 hash=bb ev=0000\nheadless frame=3 hash=cc ev=0003\n' > "$tmp/exp.txt"
cp "$tmp/exp.txt" "$tmp/body"
printf 'stats cpu cycles=5\n' >> "$tmp/body"

# rep <body file> <start fields> [n] [overflow] [bytes delta] [crc delta]: one report on stdout.
rep() {
  local body=$1 start=$2 n=${3:-1} ov=${4:-0} db=${5:-0} dc=${6:-0} crc bytes
  read -r crc bytes < <(cksum < "$body")
  echo "$P headless report n=$n $start"
  cat "$body"
  echo "$P headless end n=$n frames=3 bytes=$((bytes + db)) cksum=$((crc + dc)) overflow=$ov"
}
# run <name> <want rc> <image> <expected> [grep pattern in output]
run() {
  local name=$1 want=$2 img=$3 exp=$4 pat=${5:-} out rc
  out=$(bash "$script" "$img" "$exp" 2>&1); rc=$?
  if [ $rc -ne "$want" ]; then bad "$name: want rc $want, got $rc: $(echo "$out" | tail -3 | tr '\n' '|')"
  elif [ -n "$pat" ] && ! grep -qE -- "$pat" <<< "$out"; then bad "$name: rc $rc but output lacks /$pat/"
  else ok "$name (rc $rc)"; fi
}
cap() { export RUN_HEADLESS_CAPTURE=$1; }
img=$tmp/headless-cpu.bin exp=$tmp/exp.txt

# --- completeness, selection, comparison (plain image) ---
rep "$tmp/body" "$imgf" > "$tmp/c"; cap "$tmp/c"
run "complete report equal" 0 "$img" "$exp" "EQUAL: 3 headless lines"
{ echo "$P headless report n=0 image_bytes=1 image_cksum=1"; echo "headless frame=9 junk"; rep "$tmp/body" "$imgf" 1; } > "$tmp/c"
run "report cut by a later report: the complete one is used" 0 "$img" "$exp"
{ rep "$tmp/body" "$imgf" 1; echo "$P headless report n=2 $imgf"; echo "headless frame=1 hash=zz ev=0001"; } > "$tmp/c"
run "trailing incomplete report ignored, earlier complete used" 0 "$img" "$exp" "1 complete report\(s\), 1 incomplete"
{ echo "headless frame=1 hash=old"; echo "$P headless end n=1 bytes=0 cksum=0 overflow=0"; rep "$tmp/body" "$imgf"; } | sed 's/$/\r/' > "$tmp/c"
run "stale lines before the report ignored, CRLF tolerated" 0 "$img" "$exp"
{ echo "$P headless report n=1 $imgf"; cat "$tmp/body"; } > "$tmp/c"
run "truncated report (no end line)" 2 "$img" "$exp" "no complete report"
{ rep "$tmp/body" "$imgf" 1 0 0 1; } > "$tmp/c"
run "bad cksum" 2 "$img" "$exp" "failed its check"
{ rep "$tmp/body" "$imgf" 1 0 1 0; } > "$tmp/c"
run "bad byte count" 2 "$img" "$exp" "failed its check"
rep "$tmp/body" "$imgf" 1 1 > "$tmp/c"
run "capture buffer overflow=1" 2 "$img" "$exp" "overflowed"
{ rep "$tmp/body" "$imgf" 1; echo "$P headless end n=7 frames=3 bytes=1 cksum=1 overflow=0"; } > "$tmp/c"
run "end line of another n does not complete" 0 "$img" "$exp"
{ echo "$P headless report n=3 $imgf"; cat "$tmp/body"; echo "$P headless end n=4 bytes=1 cksum=1 overflow=0"; } > "$tmp/c"
run "mismatched n: not complete" 2 "$img" "$exp"
: > "$tmp/c"; run "empty capture" 2 "$img" "$exp" "no complete report"
{ echo "$P headless qspi error no valid image"; } > "$tmp/c"
run "board error line is shown" 2 "$img" "$exp" "the board says: .*no valid image"

# differences: exit 1
sed 's/hash=bb/hash=XX/' "$tmp/body" > "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
run "differing line" 1 "$img" "$exp" "first difference at line 2"
head -n 2 "$tmp/exp.txt" > "$tmp/b2"; echo "stats cpu cycles=5" >> "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
run "report shorter than expected (prefix)" 1 "$img" "$exp" "DIFFER"
rep "$tmp/body" "$imgf" > "$tmp/c"
cp "$tmp/body" "$tmp/b2"; echo "headless frame=4 hash=dd ev=0000" >> "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
run "report longer than expected" 1 "$img" "$exp" "DIFFER"
rep "$tmp/body" "$imgf" > "$tmp/c"

# usage and tool errors: exit 2
run "missing expected file" 2 "$img" "$tmp/nope" "no such file"
run "missing image" 2 "$tmp/nope.bin" "$exp" "no such file"
printf 'stats only\n' > "$tmp/nolines.txt"; run "expected has no headless lines" 2 "$img" "$tmp/nolines.txt" "no headless lines"
out=$(bash "$script" 2>&1); [ $? -eq 2 ] && ok "no arguments (rc 2)" || bad "no arguments"
out=$(RUN_HEADLESS_READ_SECS=abc bash "$script" "$img" "$exp" 2>&1); [ $? -eq 2 ] && ok "non-numeric READ_SECS (rc 2)" || bad "non-numeric READ_SECS"
out=$(RUN_HEADLESS_CAPTURE=$tmp/nope bash "$script" "$img" "$exp" 2>&1); [ $? -eq 2 ] && ok "missing capture file (rc 2)" || bad "missing capture file"

# --- image check ---
run "report from a different image" 2 "$tmp/other.bin" "$exp" "not running the flashed image"
printf 'headless-cpu.bin but longer\n' > "$tmp/longer.bin"
run "different image (bytes and cksum differ)" 2 "$tmp/longer.bin" "$exp" "not running the flashed image"
rep "$tmp/body" "image_bytes=$ibytes" > "$tmp/c"
run "report without image_cksum" 2 "$img" "$exp" "no image_bytes/image_cksum|has 'no "
rep "$tmp/body" "" > "$tmp/c"
run "report without image fields" 2 "$img" "$exp" "not running the flashed image"
rep "$tmp/body" "image_bytes=$ibytes image_cksum=$((icrc + 1))" > "$tmp/c"
run "image_cksum off by one" 2 "$img" "$exp"

# --- RUN_HEADLESS_ROM ---
runrom() { # runrom <name> <want rc> <rom> [pat]
  local name=$1 want=$2; RUN_HEADLESS_ROM=$3 run "$name" "$want" "$img" "$exp" "${4:-}"
}
rep "$tmp/body" "$imgf $romf" > "$tmp/c"
runrom "ROM matches" 0 "$tmp/rom.gb" "ROM matched"
printf 'b' > "$tmp/rom2.gb"; runrom "same length, different content" 2 "$tmp/rom2.gb" "ROM is not"
printf 'aa' > "$tmp/rom3.gb"; runrom "different length" 2 "$tmp/rom3.gb" "ROM is not"
runrom "ROM file missing" 2 "$tmp/nope.gb" "no such file"
rep "$tmp/body" "$imgf" > "$tmp/c"
runrom "report without rom fields" 2 "$tmp/rom.gb" "no rom_len/rom_hash"
rep "$tmp/body" "$imgf rom_len=1 rom_hash=af63dc4c8601ec8c" > "$tmp/c"
runrom "report without exit_fallbacks" 2 "$tmp/rom.gb" "no exit_fallbacks"
rep "$tmp/body" "$imgf rom_len=1 rom_hash=AF63DC4C8601EC8C exit_fallbacks=0" > "$tmp/c"
runrom "upper-case/garbled hash not accepted" 2 "$tmp/rom.gb"
rep "$tmp/body" "$imgf rom_len=1 rom_hash=0000000000000000 exit_fallbacks=0" > "$tmp/c"
run "ROM unset: rom fields not checked" 0 "$img" "$exp"
rep "$tmp/body" "$imgf $romf" > "$tmp/c"
run "ROM unset: no ROM check" 0 "$img" "$exp"

# --- display self-test ---
# dbody <start line> <cpu line> [extra]: expected frames: ev odd on 2 of 3 lines.
D="stats display start=1 frames=2 transfers=2 readbacks=2 mismatches=0 control_mismatches=2 transfer_errors=0 start_timeouts=0 dead=0"
C="stats display cpu ok=1 pct=3"
dimg=$tmp/headless-display-x.bin; printf 'display image\n' > "$dimg"
read -r dcrc dbytes < <(cksum < "$dimg"); dimgf="image_bytes=$dbytes image_cksum=$dcrc"
drun() { # drun <name> <want rc> <start line> <cpu line> [pat] [image]
  { cat "$tmp/exp.txt"; [ -n "$3" ] && echo "$3"; [ -n "$4" ] && echo "$4"; } > "$tmp/db"
  rep "$tmp/db" "$dimgf" > "$tmp/c"
  run "display: $1" "$2" "${6:-$dimg}" "$exp" "${5:-}"
}
drun "all fields good" 0 "$D" "$C" "display self-test passed"
for kv in "frames=1" "transfers=1" "readbacks=3" "mismatches=1" "control_mismatches=0" "control_mismatches=3" \
          "transfer_errors=1" "start_timeouts=1" "dead=1"; do
  k=${kv%%=*}; drun "$k wrong" 1 "$(sed -E "s/ $k=[0-9]+/ $kv/" <<< "$D")" "$C" "DISPLAY SELF-TEST FAILED"
done
drun "field missing (dead)" 1 "${D/ dead=0/}" "$C" "dead=<missing>"
drun "cpu ok=0" 1 "$D" "stats display cpu ok=0 pct=9" "ok=0"
drun "cpu ok missing" 1 "$D" "stats display cpu pct=3" "ok=<missing>"
drun "no start line" 1 "" "$C" "FAILED"
drun "no cpu line" 1 "$D" "" "FAILED"
{ cat "$tmp/exp.txt"; echo "$D"; echo "$D"; echo "$C"; } > "$tmp/db"; rep "$tmp/db" "$dimgf" > "$tmp/c"
run "display: duplicate start line" 1 "$dimg" "$exp" "exactly one 'stats display start="
{ cat "$tmp/exp.txt"; echo "$D"; echo "$C"; echo "$C"; } > "$tmp/db"; rep "$tmp/db" "$dimgf" > "$tmp/c"
run "display: duplicate cpu line" 1 "$dimg" "$exp" "exactly one 'stats display cpu"
# no frame has FRAME_READY: expected file whose ev bits are all even
printf 'headless frame=1 hash=aa ev=0000\nheadless frame=2 hash=bb ev=0002\n' > "$tmp/exp0.txt"
{ cat "$tmp/exp0.txt"; echo "${D/frames=2 transfers=2 readbacks=2/frames=0 transfers=0 readbacks=0}"; echo "$C"; } > "$tmp/db"; rep "$tmp/db" "$dimgf" > "$tmp/c"
run "display: no FRAME_READY frame" 1 "$dimg" "$tmp/exp0.txt" "no compared frame has FRAME_READY"
# lines differ AND display bad: differ wins (exit 1 either way, DIFFER printed)
{ sed 's/hash=bb/hash=XX/' "$tmp/exp.txt"; echo "$D"; echo "$C"; } > "$tmp/db"; rep "$tmp/db" "$dimgf" > "$tmp/c"
run "display: lines differ" 1 "$dimg" "$exp" "DIFFER"

# detection of a display image by each of its three signs (all with a bad self-test)
badD=${D/mismatches=0/mismatches=4}
{ cat "$tmp/exp.txt"; echo "$badD"; echo "$C"; } > "$tmp/db"
printf 'plain name, text: stats display start= inside\n' > "$tmp/plain-a.bin"
read -r c b < <(cksum < "$tmp/plain-a.bin"); rep "$tmp/db" "image_bytes=$b image_cksum=$c" > "$tmp/c"
run "display detected by .bin content" 1 "$tmp/plain-a.bin" "$exp" "FAILED"
printf 'plain name, nothing inside\n' > "$tmp/plain-b.bin"
read -r c b < <(cksum < "$tmp/plain-b.bin"); rep "$tmp/db" "image_bytes=$b image_cksum=$c" > "$tmp/c"
run "display detected by 'stats display' report line" 1 "$tmp/plain-b.bin" "$exp" "FAILED"
{ cat "$tmp/exp.txt"; echo "stats cpu ok=0"; } > "$tmp/db"
read -r c b < <(cksum < "$tmp/plain-b.bin"); rep "$tmp/db" "image_bytes=$b image_cksum=$c" > "$tmp/c"
run "non-display image: no display checks" 0 "$tmp/plain-b.bin" "$exp"
cp "$tmp/plain-b.bin" "$tmp/headless-display-y.bin"; rep "$tmp/db" "image_bytes=$b image_cksum=$c" > "$tmp/c"
run "display detected by file name (no display lines)" 1 "$tmp/headless-display-y.bin" "$exp" "FAILED"

# RUN_HEADLESS_SAVE has no effect on the capture path (nothing to save), and no lock file appears.
[ ! -e "$GBC_BOARD_LOCK" ] && ok "capture runs never touch the board lock" || bad "lock file was created"

[ $fails -eq 0 ] && echo "all run-headless tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
