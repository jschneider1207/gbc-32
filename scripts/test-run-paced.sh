#!/usr/bin/env bash
# Tests crates/gbc-nucleo-h753/run-paced.sh (the Stage 2 M4 acceptance
# checker) on serial captures built from the real M4 report committed in
# docs/reference/m4-evidence/ (M4 review T-1). RUN_PACED_CAPTURE and
# RUN_PACED_HOST are always set, so nothing flashes, reads the port, runs
# the host `headless` tool, runs cargo or takes the board lock. The image
# is a small synthetic file; the report's image fields are rewritten to
# match it. Every exit-1 path (a failed check) and exit-2 path (a tool
# error, never a pass) is asserted by exact exit code. No board. Exit 0 = all pass.
# RUN_PACED overrides the script under test (used to mutation-check this test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${RUN_PACED:-$here/../crates/gbc-nucleo-h753/run-paced.sh}
real=$here/../docs/reference/m4-evidence/report-cgb_sound.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export GBC_BOARD_LOCK=$tmp/lock
unset RUN_PACED_FRAMES RUN_PACED_MAX_PPM RUN_PACED_SAVE RUN_PACED_NO_FLASH RUN_PACED_READ_SECS RUN_PACED_HEADLESS
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }
P=gbc-nucleo-h753

printf 'synthetic paced image\n' > "$tmp/paced-x.bin"
printf 'other image\n' > "$tmp/other.bin"
read -r icrc ibytes < <(cksum < "$tmp/paced-x.bin")
imgf="image_bytes=$ibytes image_cksum=$icrc"

# body = the real report between its markers; host = its three paced lines.
head -n -1 "$real" | tail -n +2 > "$tmp/body"
# The committed M4 reports predate `restarts=` in `stats dac` (M4 review
# P-2): add it where the image now prints it, after udr2.
add_restarts() { sed -i -E 's/^(stats dac .* udr2=[0-9]+) /\1 restarts=0 /' "$1"; }
add_restarts "$tmp/body"
grep '^paced ' "$tmp/body" > "$tmp/host"
[ "$(wc -l < "$tmp/host")" = 3 ] || { echo "FAIL: committed report lacks 3 paced lines"; exit 1; }

# rep <body> <image fields> [n] [overflow] [bytes delta] [crc delta]: one report on stdout.
rep() {
  local body=$1 start=$2 n=${3:-1} ov=${4:-0} db=${5:-0} dc=${6:-0} crc bytes
  read -r crc bytes < <(cksum < "$body")
  echo "$P paced report n=$n rom=cgb_sound frames=35837 $start"
  cat "$body"
  echo "$P paced report-end n=$n rom=cgb_sound bytes=$((bytes + db)) cksum=$((crc + dc)) overflow=$ov"
}
# run <name> <want rc> [pattern in output]: capture $tmp/c, host $tmp/host, image $tmp/paced-x.bin.
run() {
  local name=$1 want=$2 pat=${3:-} out rc
  out=$(RUN_PACED_CAPTURE=$tmp/c RUN_PACED_HOST=$tmp/host bash "$script" "${IMG:-$tmp/paced-x.bin}" /nonexistent.gb 2>&1); rc=$?
  if [ $rc -ne "$want" ]; then bad "$name: want rc $want, got $rc: $(echo "$out" | tail -3 | tr '\n' '|')"
  elif [ -n "$pat" ] && ! grep -qE -- "$pat" <<< "$out"; then bad "$name: rc $rc but output lacks /$pat/"
  else ok "$name (rc $rc)"; fi
}
# mut <name> <want rc> <sed expr> [pattern]: the body with one edit, as a complete report.
mut() {
  sed -E "$3" "$tmp/body" > "$tmp/b2"
  if cmp -s "$tmp/body" "$tmp/b2"; then bad "$1: the edit changed nothing"; return; fi
  rep "$tmp/b2" "$imgf" > "$tmp/c"; run "$1" "$2" "${4:-}"
}
# drop <name> <want rc> <grep -v pattern> [pattern]
drop() {
  grep -vE "$3" "$tmp/body" > "$tmp/b2"
  if cmp -s "$tmp/body" "$tmp/b2"; then bad "$1: nothing dropped"; return; fi
  rep "$tmp/b2" "$imgf" > "$tmp/c"; run "$1" "$2" "${4:-}"
}

# --- pass ---
rep "$tmp/body" "$imgf" > "$tmp/c"
run "real report, equal lines, all checks" 0 "run-paced.sh: PASS"
RUN_PACED_MAX_PPM=82 run "ppm +82 at a bound of 82" 0 "PASS"
sed 's/$/\r/' "$tmp/c" > "$tmp/c2"; cp "$tmp/c2" "$tmp/c"; run "CRLF capture" 0 "PASS"
{ echo "junk"; rep "$tmp/body" "$imgf" 1; echo "$P paced report n=2 rom=x $imgf"; echo "stats paced ring junk"; } > "$tmp/c"
run "complete report used, later truncated one ignored" 0 "1 complete report\(s\), 1 incomplete"
mut "ppm -1000 at the bound" 0 's/ppm=\+82/ppm=-1000/' "PASS"
mut "drain underrun is report-only" 0 's/(stats paced drain underrun_events)=1/\1=9/' "PASS"

# --- exit 1: the compared lines differ ---
mut "changed paced v1 line (rom_fnv)" 1 's/rom_fnv=[0-9a-f]+/rom_fnv=0000000000000000/' "paced lines differ"
mut "changed paced v1 line (frames: wrong frame count)" 1 's/^(paced v1 .*)frames=35837/\1frames=35836/' "paced lines differ"
mut "changed paced end line (clock)" 1 's/^(paced end .*)clock=[0-9]+/\1clock=1/' "paced lines differ"
mut "changed paced end line (fbs)" 1 's/fbs=[0-9a-f]+/fbs=0/' "paced lines differ"
mut "changed paced drain line (feed)" 1 's/^(paced drain .*)feed=[0-9a-f]+/\1feed=0/' "paced lines differ"
mut "changed paced drain line (feed_frames)" 1 's/^(paced drain feed_frames=)[0-9]+/\11/' "paced lines differ"
cp "$tmp/host" "$tmp/host.orig"
sed 's/^paced end frames=35837/paced end frames=35838/' "$tmp/host.orig" > "$tmp/host"
rep "$tmp/body" "$imgf" > "$tmp/c"; run "host differs from the board" 1 "paced lines differ"
cp "$tmp/host.orig" "$tmp/host"

# --- exit 1: report-only checks ---
for k in underrun_events underrun_frames overrun_events overrun_frames; do
  mut "ring $k=1" 1 "s/ $k=0/ $k=1/" "CHECK FAILED: stats paced ring $k=1"
done
mut "ppm +1001" 1 's/ppm=\+82/ppm=+1001/' "ppm=\+1001"
mut "ppm -1001" 1 's/ppm=\+82/ppm=-1001/' "ppm=-1001"
RUN_PACED_MAX_PPM=81 run "ppm +82 over a bound of 81" 1 "want \|ppm\| <= 81"
for k in transfer_errors stuck; do
  mut "display $k=1" 1 "s/ $k=0/ $k=1/" "stats display paced $k=1"
done
mut "display presented=0" 1 's/ presented=[0-9]+/ presented=0/' "presented=0"
for k in late te dme fe udr1 udr2 restarts; do
  mut "dac $k=1" 1 "s/^(stats dac .* $k=)0/\11/" "stats dac $k=1"
done
mut "dac stop=timeout" 1 's/stop=ok/stop=timeout/' "stop=timeout"
mut "two failures at once are both reported" 1 's/ udr1=0/ udr1=2/; s/ stuck=0/ stuck=3/' "udr1=2"
sed 's/stuck=0/stuck=3/; s/ udr1=0/ udr1=2/' "$tmp/body" > "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
out=$(RUN_PACED_CAPTURE=$tmp/c RUN_PACED_HOST=$tmp/host bash "$script" "$tmp/paced-x.bin" x 2>&1)
[ "$(grep -c 'CHECK FAILED' <<< "$out")" = 2 ] && ok "both failures printed" || bad "both failures printed"

# --- exit 2: tool errors ---
for pat in 'stats paced ring ' 'stats paced speed ' 'stats display paced ' 'stats dac ' 'stats paced drain '; do
  drop "missing line '$pat'" 2 "^$pat" "want exactly one '${pat% }' line, the report has 0"
done
for pat in 'paced v1 ' 'paced end ' 'paced drain '; do
  drop "board report lacks '$pat'" 2 "^$pat" "not exactly v1, end, drain"
done
{ cat "$tmp/body"; echo "paced drain feed_frames=1 feed=1"; } > "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
run "board report has a 4th paced line" 2 "not exactly v1, end, drain"
head -n 2 "$tmp/host" > "$tmp/h2"; cp "$tmp/host" "$tmp/host.orig"; cp "$tmp/h2" "$tmp/host"
rep "$tmp/body" "$imgf" > "$tmp/c"; run "host output lacks the drain line" 2 "the host's 'paced' lines are not exactly"
: > "$tmp/host"; run "host output empty" 2 "the host's 'paced' lines are not exactly"
cp "$tmp/host.orig" "$tmp/host"
for pat in 'stats paced ring ' 'stats paced speed ' 'stats display paced ' 'stats dac '; do
  { cat "$tmp/body"; grep "^$pat" "$tmp/body"; } > "$tmp/b2"; rep "$tmp/b2" "$imgf" > "$tmp/c"
  run "repeated line '$pat'" 2 "the report has 2"
done
mut "non-numeric ring field" 2 's/underrun_events=0/underrun_events=x/' "malformed"
mut "empty field value" 2 's/ overrun_frames=0/ overrun_frames=/' "malformed"
mut "ppm without a sign" 2 's/ppm=\+82/ppm=82/' "malformed"
mut "ppm not a number" 2 's/ppm=\+82/ppm=+8x/' "malformed"
mut "stop value unknown" 2 's/stop=ok/stop=maybe/' "malformed"
mut "field missing (udr2)" 2 's/ udr2=0//' "appears 0 times"
mut "field missing (restarts: an image before M4 review P-2)" 2 's/ restarts=0//' "appears 0 times"
mut "field repeated (late)" 2 's/ late=0/ late=0 late=0/' "appears 2 times"
mut "display field missing" 2 's/ stuck=0//' "appears 0 times"
mut "steady_ppm garbage" 2 's/steady_ppm=\+9/steady_ppm=q/' "malformed"
echo "$P paced error no valid image" > "$tmp/c"
run "a paced error line, no report" 2 "the board says: .*paced error"
: > "$tmp/c"; run "empty capture" 2 "no complete report"
{ echo "$P paced report n=1 rom=x $imgf"; cat "$tmp/body"; } > "$tmp/c"
run "truncated report (no end line)" 2 "no complete report"
head -n 5 "$tmp/body" > "$tmp/b2"; rep "$tmp/body" "$imgf" | head -n 8 > "$tmp/c"
run "garbled/truncated mid-report" 2 "no complete report"
rep "$tmp/body" "$imgf" 1 0 0 1 > "$tmp/c"; run "bad cksum" 2 "failed its check"
rep "$tmp/body" "$imgf" 1 0 1 0 > "$tmp/c"; run "bad byte count" 2 "failed its check"
rep "$tmp/body" "$imgf" 1 1 > "$tmp/c"; run "capture buffer overflow=1" 2 "overflowed"
{ echo "$P paced report n=3 rom=x $imgf"; cat "$tmp/body"; echo "$P paced report-end n=4 rom=x bytes=1 cksum=1 overflow=0"; } > "$tmp/c"
run "end line of another n does not complete" 2 "no complete report"
sed 's/^\(.*\)$/\1x/' "$tmp/body" > "$tmp/b2"; rep "$tmp/body" "$imgf" | sed '3s/$/ junk/' > "$tmp/c"
run "a line altered after the cksum" 2 "failed its check"
rep "$tmp/body" "image_bytes=$ibytes image_cksum=$((icrc + 1))" > "$tmp/c"
run "wrong image (cksum)" 2 "not running the flashed image"
rep "$tmp/body" "image_bytes=$((ibytes + 1)) image_cksum=$icrc" > "$tmp/c"
run "wrong image (bytes)" 2 "not running the flashed image"
rep "$tmp/body" "" > "$tmp/c"; run "report without image fields" 2 "no image_bytes/image_cksum"
rep "$tmp/body" "$imgf" > "$tmp/c"
IMG=$tmp/other.bin run "flashed a different image than the board runs" 2 "not running the flashed image"
IMG=$tmp/missing.bin run "image file missing" 2 "no such file"
RUN_PACED_FRAMES=abc run "RUN_PACED_FRAMES not a number" 2 "not a number"
RUN_PACED_MAX_PPM=-5 run "RUN_PACED_MAX_PPM not a number" 2 "not a number"
out=$(RUN_PACED_CAPTURE=$tmp/c RUN_PACED_HOST=$tmp/host bash "$script" 2>&1); [ $? = 2 ] && ok "no arguments (rc 2)" || bad "no arguments"
out=$(RUN_PACED_CAPTURE=$tmp/nonexistent RUN_PACED_HOST=$tmp/host bash "$script" "$tmp/paced-x.bin" x 2>&1); [ $? = 2 ] && ok "capture file missing (rc 2)" || bad "capture file missing"
out=$(RUN_PACED_CAPTURE=$tmp/c RUN_PACED_HOST=$tmp/none bash "$script" "$tmp/paced-x.bin" x 2>&1); [ $? = 2 ] && ok "host file missing (rc 2)" || bad "host file missing"

# --- the committed M4 captures: their framing is intact and their lines are equal ---
for r in cgb_sound worst-case; do
  f=$here/../docs/reference/m4-evidence/report-$r.txt
  e=$(tail -n 1 "$f"); wb=$(sed -n 's/.* bytes=\([0-9]*\).*/\1/p' <<< "$e"); wc=$(sed -n 's/.* cksum=\([0-9]*\).*/\1/p' <<< "$e")
  read -r c b < <(head -n -1 "$f" | tail -n +2 | cksum)
  [ "$b $c" = "$wb $wc" ] && ok "committed $r report: framing cksum" || bad "committed $r report: framing cksum"
  head -n -1 "$f" | tail -n +2 > "$tmp/b3"; add_restarts "$tmp/b3"; grep '^paced ' "$tmp/b3" > "$tmp/host"
  rep "$tmp/b3" "$imgf" > "$tmp/c"; run "committed $r report passes (image fields swapped for the synthetic one)" 0 "PASS"
done

[ ! -e "$GBC_BOARD_LOCK" ] && ok "capture runs never touch the board lock" || bad "lock file was created"
[ $fails -eq 0 ] && echo "all run-paced tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
