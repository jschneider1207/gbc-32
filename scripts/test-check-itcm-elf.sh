#!/usr/bin/env bash
# Tests scripts/check-itcm-elf.sh (the post-link ITCM check) on the linked
# ITCM ELFs that the firmware builds write (no board; build them first:
# tools/h7-bench/build.sh, crates/gbc-nucleo-h753/build.sh). Exit 0 = all
# pass, 1 = a test fails, 2 = an ELF or rlib is missing.
#
# Per ELF:
#   - the check passes on the ELF as linked;
#   - its "not in the link" list is short (a handful of functions inlined or
#     garbage-collected; M6 review: a parse bug listed ~58, 53 of them
#     "<name> LOCAL"/"<name> GLOBAL") and has no name with a bind suffix;
#   - a downstream instance of a kept-tier function (an ELF symbol matching
#     a glob that the rlib lacks) placed in flash fails the check; in ITCM
#     it passes. The symbol is added to `readelf -sW`'s output by a shim.
#
#   CHECK_ITCM_ELF   the script under test (used to mutation-check this test)
#   ITCM_TEST_ELFS   "<elf>:<rlib dir> ..." (default: the three below)
set -u
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/.." && pwd)
script=${CHECK_ITCM_ELF:-$here/check-itcm-elf.sh}
MAX_ABSENT=12   # 5 on each ELF at M6 close
T=thumbv7em-none-eabihf/release
elfs=${ITCM_TEST_ELFS:-"$root/tools/h7-bench/out/h753/bench-itcm-dtcm-pll480.elf:$root/tools/h7-bench/target/$T
$root/crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf:$root/crates/gbc-nucleo-h753/target/$T
$root/crates/gbc-nucleo-h753/out/player.elf:$root/crates/gbc-nucleo-h753/target/$T"}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

real_readelf=$(command -v readelf) || { echo "readelf not found" >&2; exit 2; }
mkdir "$tmp/bin"
cat > "$tmp/bin/readelf" <<EOF
#!/usr/bin/env bash
"$real_readelf" "\$@" || exit \$?
if [ "\$1" = -sW ] && [ -n "\${FAKE_SYM:-}" ]; then echo "\$FAKE_SYM"; fi
EOF
chmod +x "$tmp/bin/readelf"
# A name the first kept glob (`Cpu::run`) matches that no rlib section has.
glob=$(grep -m1 '^\.text\._R' "$root/perf/itcm-sections.txt" | awk '{print $1}')
fake=$(sed 's/^\.text\.//; s/\*/Zdownstream/g' <<< "$glob")

for pair in $elfs; do
  elf=${pair%%:*} rlib=${pair#*:} n=$(basename "$elf")
  { [ -f "$elf" ] && [ -d "$rlib" ]; } || { echo "missing $elf or $rlib (build the firmware first)" >&2; exit 2; }
  out=$(bash "$script" --drop "plain tile path" --rlib "$rlib" "$elf" 2>&1); rc=$?
  [ $rc -eq 0 ] && ok "$n: passes as linked" || bad "$n: rc $rc: $(grep FAIL <<< "$out" | head -3)"
  nabs=$(sed -n 's/^not in the link (\([0-9]*\);.*/\1/p' <<< "$out"); nabs=${nabs:-0}
  [ "$nabs" -le $MAX_ABSENT ] && ok "$n: $nabs functions not in the link (<= $MAX_ABSENT)" || bad "$n: $nabs functions not in the link (> $MAX_ABSENT)"
  sfx=$(sed -n '/^not in the link/,/^[^ ]/p' <<< "$out" | grep -cE '^  .* (LOCAL|GLOBAL|WEAK)$')
  [ "$sfx" -eq 0 ] && ok "$n: no absent name with a bind suffix" || bad "$n: $sfx absent names end in LOCAL/GLOBAL/WEAK (the symbol name read with the bind column)"
  out=$(FAKE_SYM="99999: 08070001   200 FUNC    LOCAL  DEFAULT    2 $fake" PATH="$tmp/bin:$PATH" bash "$script" --drop "plain tile path" --rlib "$rlib" "$elf" 2>&1); rc=$?
  { [ $rc -eq 1 ] && grep -q "kept-tier function in flash at 0x08070001.*Zdownstream" <<< "$out"; } &&
    ok "$n: downstream kept-tier instance in flash fails" || bad "$n: downstream kept-tier instance in flash: rc $rc, not named"
  out=$(FAKE_SYM="99999: 0000fe01   200 FUNC    LOCAL  DEFAULT    2 $fake" PATH="$tmp/bin:$PATH" bash "$script" --drop "plain tile path" --rlib "$rlib" "$elf" 2>&1); rc=$?
  [ $rc -eq 0 ] && ok "$n: downstream kept-tier instance in ITCM passes" || bad "$n: downstream kept-tier instance in ITCM: rc $rc: $(grep FAIL <<< "$out" | head -2)"
done

[ $fails -eq 0 ] && echo "all check-itcm-elf tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
