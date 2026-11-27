#!/usr/bin/env bash
# Tests tools/h7-bench/check-report.sh on small synthetic captures: one passing
# case per mode and one per failure mode. No board. Exit 0 = all pass.
# CHECK_REPORT overrides the script under test (used to mutation-check this test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${CHECK_REPORT:-$here/../tools/h7-bench/check-report.sh}
exp=$(dirname "$script")/expected-hashes.txt
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

printf 'synthetic image bytes\n' > "$tmp/img.bin"
read -r crc bytes < <(cksum < "$tmp/img.bin")
banner="h7bench v1 banner image=bench image_bytes=$bytes image_cksum=$crc"
flash_lines() { sed -E 's/ min=0 mean=0 max=0 / min=100 mean=150 max=200 /; s/ max_at=0$/ max_at=7/' "$exp"; }
qspi_lines()  { sed -E 's/ min=0 mean=0 max=0 / min=101 mean=151 max=201 /; s/ max_at=0$/ max_at=9/; s/^h7bench v1 /h7bench v1 qspi /' "$exp"; }
pack="h7bench v1 qspi pack ok bytes=1 same_as_flash=1 "
end="h7bench v1 end pass=1"

# mk <file> [flash|qspi]: a complete report (CRLF + NULs as the VCP gives it).
mk() {
  { echo "$banner"; flash_lines; [ "${2:-}" = qspi ] && { echo "$pack"; qspi_lines; }; echo "$end"; } > "$1"
}
# run <name> <want rc> <script args...>
run() {
  local name=$1 want=$2 out rc; shift 2
  out=$(bash "$script" "$@" 2>&1); rc=$?
  if [ $rc -eq "$want" ]; then ok "$name (rc $rc)"; else bad "$name: want rc $want, got $rc: $(echo "$out" | head -3)"; fi
  last=$out
}

mk "$tmp/c" ; run "flash-only report passes" 0 "$tmp/c" "$tmp/img.bin"
mk "$tmp/c" qspi ; run "report with qspi passes with --qspi" 0 "$tmp/c" "$tmp/img.bin" --qspi
mk "$tmp/c" qspi ; run "qspi lines ignored without --qspi" 0 "$tmp/c" "$tmp/img.bin"
{ printf 'noise\n\000\000'; mk "$tmp/x"; sed 's/$/\r/' "$tmp/x"; } > "$tmp/c"
run "NULs, CRs and leading noise tolerated" 0 "$tmp/c" "$tmp/img.bin"
mk "$tmp/x"; { cat "$tmp/x"; head -n 3 "$tmp/x"; } > "$tmp/c"
run "partial report after a complete one: last complete used" 0 "$tmp/c" "$tmp/img.bin"
[[ $last == *"| Workload |"* ]] && ok "cycle table printed" || bad "no cycle table"

# usage / tool errors: rc 2
run "no arguments" 2
run "missing capture" 2 "$tmp/nope" "$tmp/img.bin"
mk "$tmp/c"; run "missing image" 2 "$tmp/c" "$tmp/nope"
run "unknown option" 2 "$tmp/c" "$tmp/img.bin" --bogus
{ echo "$banner"; flash_lines | head -n 3; } > "$tmp/c"; run "truncated report (no end line)" 2 "$tmp/c" "$tmp/img.bin"
: > "$tmp/c"; run "empty capture" 2 "$tmp/c" "$tmp/img.bin"
{ flash_lines; echo "$end"; } > "$tmp/c"; run "no banner" 2 "$tmp/c" "$tmp/img.bin"

# check failures: rc 1
{ echo "h7bench v1 banner image=bench image_bytes=$bytes image_cksum=$((crc + 1))"; flash_lines; echo "$end"; } > "$tmp/c"
run "wrong image cksum" 1 "$tmp/c" "$tmp/img.bin"
{ echo "h7bench v1 banner image=bench image_cksum=$crc"; flash_lines; echo "$end"; } > "$tmp/c"
run "banner without image_bytes" 1 "$tmp/c" "$tmp/img.bin"
mk "$tmp/c"; printf 'other image\n' > "$tmp/img2.bin"; run "different image file" 1 "$tmp/c" "$tmp/img2.bin"
{ echo "$banner"; flash_lines | sed '1s/hash=[0-9a-f]*/hash=0000000000000000/'; echo "$end"; } > "$tmp/c"
run "flash hash mismatch" 1 "$tmp/c" "$tmp/img.bin"
{ echo "$banner"; flash_lines | sed '1d'; echo "$end"; } > "$tmp/c"
run "flash line missing" 1 "$tmp/c" "$tmp/img.bin"
{ echo "$banner"; { flash_lines | sed -n 2p; flash_lines | sed -n 1p; flash_lines | sed -n "3,\$p"; }; echo "$end"; } > "$tmp/c"
run "flash lines out of order" 1 "$tmp/c" "$tmp/img.bin"
{ echo "$banner"; flash_lines | sed '1s/frames=[0-9]*/frames=1/'; echo "$end"; } > "$tmp/c"
run "non-hash field differs" 1 "$tmp/c" "$tmp/img.bin"
{ echo "$banner"; flash_lines; qspi_lines; echo "$end"; } > "$tmp/c"
run "--qspi without pack line" 1 "$tmp/c" "$tmp/img.bin" --qspi
{ echo "$banner"; flash_lines; echo "${pack/same_as_flash=1/same_as_flash=0}"; qspi_lines; echo "$end"; } > "$tmp/c"
run "pack same_as_flash=0" 1 "$tmp/c" "$tmp/img.bin" --qspi
{ echo "$banner"; flash_lines; echo "h7bench v1 qspi pack fail same_as_flash=1 "; qspi_lines; echo "$end"; } > "$tmp/c"
run "pack not ok" 1 "$tmp/c" "$tmp/img.bin" --qspi
{ echo "$banner"; flash_lines; echo "$pack"; qspi_lines | sed '2s/hash=[0-9a-f]*/hash=0000000000000000/'; echo "$end"; } > "$tmp/c"
run "qspi hash mismatch" 1 "$tmp/c" "$tmp/img.bin" --qspi
{ echo "$banner"; flash_lines; echo "$pack"; qspi_lines | sed '$d'; echo "$end"; } > "$tmp/c"
run "qspi line missing" 1 "$tmp/c" "$tmp/img.bin" --qspi

[ $fails -eq 0 ] && echo "all check-report tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
