#!/usr/bin/env bash
# Tests scripts/check-stage2-sets.sh (gate step B3: the board run lists
# equal acceptance/stage2.toml) on copies of the committed files with one
# change each: the committed files pass, every difference is exit 1, every
# parse error exit 2. No board. Exit 0 = all pass.
# CHECK_SETS overrides the script under test (used to mutation-check this
# test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$here/..
script=${CHECK_SETS:-$here/check-stage2-sets.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

T=$tmp/stage2.toml R=$tmp/replay.txt S=$tmp/soak.txt S1=$tmp/stage1.toml CC=$tmp/check-cycles.sh
fresh() {
  cp "$root/acceptance/stage2.toml" "$T"; cp "$root/crates/gbc-nucleo-h753/replay-set.txt" "$R"
  cp "$root/crates/gbc-nucleo-h753/soak-set.txt" "$S"; cp "$root/acceptance/stage1.toml" "$S1"
  cp "$root/tools/h7-bench/check-cycles.sh" "$CC"
}
# run <name> <want rc> <file> <sed expr> [<file 2> <sed expr 2>]: fresh copies, edited.
run() {
  local name=$1 want=$2 f=$3 e=$4 out rc
  fresh; [ -z "$f" ] || sed -i -E "$e" "$f"
  [ -z "${5:-}" ] || sed -i -E "$6" "$5"
  out=$(bash "$script" "$T" "$R" "$S" "$S1" "$CC" 2>&1); rc=$?
  if [ $rc -eq "$want" ]; then ok "$name (rc $rc)"; else bad "$name: want rc $want, got $rc: $(echo "$out" | grep -E 'FAIL|sets.sh: ' | head -3)"; fi
  last=$out
}

run "the committed files pass" 0 "" ""
[[ $last == *"save_fixture=tools/gbc-pc/tests/fixtures/link-probe.sav" ]] && ok "fixture printed last" || bad "no save_fixture line last"
run "comments, blank lines and extra spaces in a set file pass" 0 "$R" 's/^(cgb-acid2\/cgb-acid2.gbc) 600$/\n\1   600  # x/'
run "explicit frames in the manifest vs - in the set differ" 1 "$T" 's/(input_group = "rtc3test-sub-second")/run_frames = 1793, \1/'
grep -q '^rtc3test/rtc3test.gb - rtc3test-sub-second$' "$R" || bad "fixture assumption: replay-set.txt has the rtc3test group line"

# --- differences: rc 1 ---
run "replay frames differ" 1 "$R" 's/^(dmg-acid2\/dmg-acid2.gb) 600$/\1 601/'
run "replay entry missing from the set" 1 "$R" '/^dmg-acid2/d'
run "extra replay entry in the set" 1 "$R" '$a blargg/cpu_instrs/cpu_instrs.gb 600'
run "replay order differs" 1 "$R" '/^cgb-acid2/{h;d}; /^dmg-acid2/{G}'
run "replay group differs" 1 "$R" 's/rtc3test-sub-second$/rtc3test-basic/'
run "manifest replay rom differs" 1 "$T" 's/hdma_mode0.gb/hdma_mode1.gb/'
run "manifest replay entry removed" 1 "$T" '/dmg-acid2/d'
run "soak frames differ in the set" 1 "$S" 's/35837/35836/'
run "soak max_ppm differs in the set" 1 "$S" 's/max_ppm=1000/max_ppm=2000/'
run "soak max_ppm missing from the set" 1 "$S" 's/ max_ppm=1000//'
run "soak frames not soak_minutes (both changed)" 1 "$T" 's/run_frames = 35837/run_frames = 35836/'
run "soak_minutes changed" 1 "$T" 's/^soak_minutes = 10/soak_minutes = 11/'
run "cpu_mhz changed" 1 "$T" 's/^cpu_mhz = 472/cpu_mhz = 480/'
run "frame_budget_cycles off by one" 1 "$T" 's/^frame_budget_cycles = 7902557/frame_budget_cycles = 7902558/'
run "worst_case_report_cycles off by one" 1 "$T" 's/^worst_case_report_cycles = 3951279/worst_case_report_cycles = 3951278/'
# Both copies moved together: only the frame arithmetic catches it.
run "frame_budget_cycles and REAL_TIME both off by one" 1 "$T" 's/^frame_budget_cycles = 7902557/frame_budget_cycles = 7902558/' "$CC" 's/^REAL_TIME=7902557/REAL_TIME=7902558/'
grep -q '^REAL_TIME=7902558' "$CC" && ok "(both edited)" || bad "the REAL_TIME edit did not apply"
run "worst_case_report_cycles and HALF both off by one" 1 "$T" 's/^worst_case_report_cycles = 3951279/worst_case_report_cycles = 3951278/' "$CC" 's/^HALF=3951279/HALF=3951278/'
grep -q '^HALF=3951278' "$CC" && ok "(both edited)" || bad "the HALF edit did not apply"
run "check-cycles.sh REAL_TIME differs" 1 "$CC" 's/^REAL_TIME=7902557/REAL_TIME=7902558/'
run "check-cycles.sh HALF differs" 1 "$CC" 's/^HALF=3951279/HALF=3951280/'
run "roms_release differs from stage1.toml" 1 "$S1" 's/^roms_release = "v7.0"/roms_release = "v6.0"/'
run "save_roundtrip rom differs" 1 "$T" 's/rom = "generated:link-probe"/rom = "generated:other"/'
run "save_roundtrip fixture missing" 1 "$T" 's/link-probe.sav/no-such.sav/'
run "save_roundtrip fixture outside the repository" 1 "$T" 's|fixture = "tools/|fixture = "../tools/|'

# --- parse errors: rc 2 ---
run "unknown key in a replay entry" 2 "$T" 's/(rom = "dmg-acid2\/dmg-acid2.gb",)/\1 frames = 1,/'
run "repeated key in an entry" 2 "$T" 's/(run_frames = 600 )/\1, run_frames = 600 /'
run "unquoted string" 2 "$T" 's/"cgb-acid2\/cgb-acid2.gbc"/cgb-acid2\/cgb-acid2.gbc/'
run "a two-line entry" 2 "$T" 's/(rom = "dmg-acid2\/dmg-acid2.gb",)/\1\n/'
run "unterminated replay array" 2 "$T" '/^]/d'
run "empty replay list" 2 "$T" '/^  \{ rom/d'
run "replay entry without frames or group" 2 "$T" 's/(rom = "dmg-acid2\/dmg-acid2.gb"),[^}]*/\1 /'
run "soak entry without max_ppm" 2 "$T" 's/, max_ppm = 1000//'
run "unknown key in the soak entry" 2 "$T" 's/max_ppm = 1000/max_ppm = 1000, minutes = 10/'
run "repeated top-level key" 2 "$T" '$a cpu_mhz = 472'
run "an unrecognized line" 2 "$T" '$a this is not toml'
run "a non-integer cpu_mhz" 2 "$T" 's/^cpu_mhz = 472/cpu_mhz = "472"/'
run "no frame_budget_cycles" 2 "$T" '/^frame_budget_cycles/d'
run "no save_roundtrip" 2 "$T" '/^save_roundtrip/d'
run "check-cycles.sh without REAL_TIME" 2 "$CC" '/^REAL_TIME=/d'
run "stage1.toml without roms_release" 2 "$S1" '/^roms_release/d'
out=$(bash "$script" "$T" 2>&1); rc=$?; [ $rc = 2 ] && ok "wrong argument count: rc 2" || bad "wrong argument count: rc $rc"
out=$(bash "$script" "$tmp/nope" "$R" "$S" "$S1" "$CC" 2>&1); rc=$?; [ $rc = 2 ] && ok "missing manifest: rc 2" || bad "missing manifest: rc $rc"

[ $fails -eq 0 ] && echo "all check-stage2-sets tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
