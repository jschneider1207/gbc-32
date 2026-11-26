#!/usr/bin/env bash
# Tests tools/h7-bench/check-selftest.sh (gate step B1) on synthetic
# captures in the format of the board's 2026-11-26 `selftest-pll480` run
# (tools/h7-bench/README.md "Read the output"): a pass case, the stale-boot
# and other-image cases, one case per check (exit 1) and per tool error
# (exit 2). No board. Exit 0 = all pass.
# CHECK_SELFTEST overrides the script under test (used to mutation-check
# this test).
set -u
here=$(cd "$(dirname "$0")" && pwd)
script=${CHECK_SELFTEST:-$here/../tools/h7-bench/check-selftest.sh}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fails=0
ok()  { echo "ok: $*"; }
bad() { echo "FAIL: $*"; fails=$((fails + 1)); }

printf 'synthetic selftest image\n' > "$tmp/img.bin"
read -r crc bytes < <(cksum < "$tmp/img.bin")
I=$tmp/img.bin C=$tmp/c

# block [image fields]: one banner block, as the board prints it.
block() {
  local imgf=${1:-image_bytes=$bytes image_cksum=$crc}
  echo
  echo "h7bench v1 banner image=selftest placement=flash-axi board=NUCLEO-H753ZI clock=pll472 cpu_hz=472000000 lse_cpu_hz=473538038 baud=115200 d3cr=0x0000e000 syscfg_pwrcr=0x00000081 oden=1 flash_acr=0x24 icache=1 dcache=1 mpu=1 cyccnt=1 $imgf"
  echo "h7bench v1 power cr3=0x00000042 csr1=0x0000e000 d3cr=0x0000e000 actvosrdy=1 supply=ldo"
  echo "h7bench v1 clock rcc_cr=0x0300c025 cfgr=0x0000001b d1cfgr=0x00000048 d2cfgr=0x00000440 d3cfgr=0x00000040 flash_acr=0x24 rsr=0x00fe0000"
  echo "h7bench v1 pll pllckselr=0x02020050 pllcfgr=0x01f9000d pll1divr=0x00000248 pll1fracr=0x0000c000"
  echo "h7bench v1 vos0 syscfg_pwrcr=0x00000081 oden=1 pwrcr_before=0x00000000 polls_to_drop=2 polls_low=5"
  echo "h7bench v1 cpu cpuid=0x411fc271 idcode=0x20036450 optsr=0x1bc6aaf0 mpu_ctrl=0x3 mpu_regions=16"
  echo "h7bench v1 tcm itcmcr=0x3b dtcmcr=0x43 at_init=0x3b,0x43 itcm_bytes=0 dtcm_table_bytes=0 msp=0x2403feb0"
}
st() { echo "h7bench v1 selftest n=$1 delay=8000000 cycles=${2:-8000019} cyccnt=$((991836526 + $1 * 472744606 % 4294967296)) stack_used=544"; }
probe="h7bench v1 tcmprobe want=0x5a17c0de itcm=0x5a17c0de dtcm=0x5a17c0de"
# boot <from> <to> [image fields]: the board's lines for selftest n = from..to
# (a block before every n divisible by 8, the probe after its first line;
# n=0 measures 8000022).
boot() {
  local n
  for n in $(seq "$1" "$2"); do
    if [ $((n % 8)) = 0 ]; then block "${3:-}"; [ "$n" = 0 ] && st 0 8000022 || st "$n"; echo "$probe"
    else st "$n"; fi
  done
}
# vcp: CRLF line ends and a NUL, as read-serial.sh gives them.
vcp() { sed 's/$/\r/'; printf '\000'; }
run() { # run <name> <want rc> <capture>
  local name=$1 want=$2 out rc
  out=$(bash "$script" "$3" "$I" 2>&1); rc=$?
  if [ $rc -eq "$want" ]; then ok "$name (rc $rc)"; else bad "$name: want rc $want, got $rc: $(echo "$out" | grep -E 'FAIL|ERROR' | head -3)"; fi
  last=$out
}
# mk <sed expr on the boot's lines> [from] [to]: a capture with old-image
# noise first, then one boot.
mk() { { echo "gbc-nucleo-h753 player stats frames=300"; boot "${2:-0}" "${3:-40}" | sed -E "${1:-}"; } | vcp > "$C"; }

# --- passes ---
mk; run "a boot from n=0 passes" 0 "$C"
[[ $last == *"cycles=8000019 (after n=0)"* ]] && ok "cycles reported" || bad "cycles not reported"
[[ $last == *"lse_cpu_hz=473538038"* ]] && ok "lse_cpu_hz reported" || bad "lse_cpu_hz not reported"
mk "" 21 50; run "a boot read from n=21 (no n=0) passes" 0 "$C"
{ boot 30 45; boot 0 40; } | vcp > "$C"; run "a stale boot of the same image first: the new boot is judged" 0 "$C"
{ boot 30 45 "image_bytes=1 image_cksum=2" | sed 's/cycles=8000019/cycles=8000500/'; boot 0 40; } | vcp > "$C"
run "a stale boot of another image first (bad cycles): ignored" 0 "$C"
[[ $last == *"current boot from line"* ]] && ok "window named" || bad "window not named"

# --- check failures: rc 1 ---
mk 's/ clock=pll472 / clock=pll480 /'; run "banner clock=pll480 fails" 1 "$C"
mk 's/ cpu_hz=472000000 / cpu_hz=480000000 /'; run "banner cpu_hz=480000000 fails" 1 "$C"
mk 's/ board=NUCLEO-H753ZI / board=NUCLEO-H755ZI /'; run "banner other board fails" 1 "$C"
mk '/banner/s/ d3cr=0x0000e000 / d3cr=0x0000c000 /'; run "banner d3cr not VOS0 fails" 1 "$C"
mk '/banner/s/ syscfg_pwrcr=0x00000081 oden=1 / syscfg_pwrcr=0x00000080 oden=0 /'; run "banner oden=0 fails" 1 "$C"
mk '/banner/s/ flash_acr=0x24 / flash_acr=0x23 /'; run "banner flash_acr=0x23 fails" 1 "$C"
mk 's/ icache=1 / icache=0 /'; run "banner icache=0 fails" 1 "$C"
mk 's/ dcache=1 / dcache=0 /'; run "banner dcache=0 fails" 1 "$C"
mk 's/ mpu=1 / mpu=0 /'; run "banner mpu=0 fails" 1 "$C"
mk 's/ cyccnt=1 / cyccnt=0 /'; run "banner cyccnt=0 fails" 1 "$C"
mk 's/ lse_cpu_hz=473538038 / lse_cpu_hz=479080001 /'; run "lse_cpu_hz +1.5% + 1 Hz fails" 1 "$C"
mk 's/ lse_cpu_hz=473538038 / lse_cpu_hz=479080000 /'; run "lse_cpu_hz +1.5% exactly passes" 0 "$C"
mk 's/ lse_cpu_hz=473538038 / lse_cpu_hz=464919999 /'; run "lse_cpu_hz -1.5% - 1 Hz fails" 1 "$C"
mk 's/ lse_cpu_hz=473538038 / lse_cpu_hz=fail /'; run "lse_cpu_hz=fail fails" 1 "$C"
mk 's/ actvosrdy=1 / actvosrdy=0 /'; run "power actvosrdy=0 fails" 1 "$C"
mk 's/ supply=ldo/ supply=smps/'; run "power supply=smps fails" 1 "$C"
mk '/power/s/ d3cr=0x0000e000 / d3cr=0x00004000 /'; run "power d3cr not VOS0 fails" 1 "$C"
mk '/clock rcc/s/ flash_acr=0x24 / flash_acr=0x22 /'; run "clock flash_acr=0x22 fails" 1 "$C"
mk '/vos0/s/ syscfg_pwrcr=0x00000081 / syscfg_pwrcr=0x00000080 /'; run "vos0 syscfg_pwrcr fails" 1 "$C"
mk '/vos0/s/ oden=1 / oden=0 /'; run "vos0 oden=0 fails" 1 "$C"
mk 's/ dtcm=0x5a17c0de/ dtcm=0x00000000/'; run "tcmprobe dtcm unequal fails" 1 "$C"
mk 's/ itcm=0x5a17c0de / itcm=0x5a17c0df /'; run "tcmprobe itcm unequal fails" 1 "$C"
mk '/selftest n=17 /s/cycles=8000019/cycles=8000008/'; run "one cycles= different after n=0 fails" 1 "$C"
mk 's/cycles=8000019/cycles=8000065/'; run "cycles= delay + 65 fails" 1 "$C"
mk '/selftest n=0 /s/cycles=8000022/cycles=7999999/'; run "n=0 cycles= below delay fails" 1 "$C"
mk 's/ delay=8000000 / delay=4000000 /'; run "delay=4000000 fails" 1 "$C"
mk '/selftest n=30 /a h7bench v1 FAULT hardfault sp=0x1'; run "a FAULT line fails" 1 "$C"

# --- tool errors: rc 2 ---
mk "" 0 40; sed -i 's/image_cksum=[0-9]*/image_cksum=1/' "$C"; run "banner of another image: rc 2" 2 "$C"
mk 's/ image_bytes=[0-9]+ image_cksum=[0-9]+//'; run "banner without image fields: rc 2" 2 "$C"
mk '/selftest n=/d'; run "no selftest line: rc 2" 2 "$C"
mk "" 0 14; run "15 selftest lines: rc 2" 2 "$C"
mk "" 0 7; run "one banner block: rc 2" 2 "$C"
mk '/selftest n=19 /s/cycles=/cycls=/'; run "a garbled selftest line: rc 2" 2 "$C"
mk '/selftest n=19 /s/^h7bench v1 /h7bnch v1 /'; run "a garbled line before the last 16: the lines after it are judged" 0 "$C"
mk '/selftest n=40 /{n;s/^h7bench v1 /h7bnch v1 /}'; run "a garbled probe line in the window: rc 2" 2 "$C"
mk '/selftest n=38 /{n;s/^h7bench v1 /h7bnch v1 /}'; run "a garbled line just before the last banner: rc 2" 2 "$C"
mk '0,/vos0 /s/polls_low=5/polls_low=6/'; run "banner blocks that differ: rc 2" 2 "$C"
mk '/selftest n=20 /a h7bench v1 mystery x=1'; run "an unknown line kind: rc 2" 2 "$C"
mk '/selftest n=33 /d' 0 50; run "a lost selftest line early: the 17 lines after it are judged" 0 "$C"
mk '/selftest n=40 /d' 0 50; run "a lost selftest line leaves 10 after it: rc 2" 2 "$C"
: > "$C"; run "empty capture: rc 2" 2 "$C"
out=$(bash "$script" 2>&1); rc=$?; [ $rc = 2 ] && ok "no arguments: rc 2" || bad "no arguments: rc $rc"
out=$(bash "$script" "$tmp/nope" "$I" 2>&1); rc=$?; [ $rc = 2 ] && ok "missing capture: rc 2" || bad "missing capture: rc $rc"

[ $fails -eq 0 ] && echo "all check-selftest tests passed" || echo "$fails failed"
[ $fails -eq 0 ]
