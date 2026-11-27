#!/usr/bin/env bash
# Checks a capture of the `selftest-pll480` image (gate step B1; README.md
# "Read the output" and "Flashing order"):
#
#   ./check-selftest.sh <capture.txt> <selftest-pll480.bin>
#
# The capture is read-serial.sh's output after flash.sh (NULs and CRs as
# the VCP gives them are dropped). The board prints a banner block (banner,
# power, clock, pll, vos0, cpu, tcm lines) every 8 `selftest` lines, a
# `tcmprobe` line after each banner block's first `selftest` line, and one
# `selftest n=<n>` line a second from n=0 at reset.
#
# Which lines are judged (the new image's, never the previous one's that
# the VCP buffered): the current run, from the last `selftest` line whose
# n is 0 or does not follow the `selftest` line before it, to the end of
# the capture. Everything before it is ignored: the previous boot, and the
# junction where the ST-LINK's VCP lost bytes (it buffers the board's
# output while the port is closed, then drops a stretch when the port
# opens: seen 2026-11-26, a banner cut off and n=16..24 missing). A last
# line without its newline (the read stopped inside it) is dropped too.
#
# Tool errors (exit 2): no `selftest` line; in the judged lines, a line
# that is not blank and not one of the kinds above, a line of a kind that
# does not match its format, lines of one kind (other than `selftest`)
# that are not all identical (a byte lost on the link), fewer than 2
# banner blocks (each kind at least twice) or fewer than 16 `selftest`
# lines; the banner's `image_bytes=<n> image_cksum=<crc>` not `cksum` of
# <image.bin> (the board is not running the image given).
#
# Checks (exit 1), on the judged lines:
#   - no `FAULT` line;
#   - banner: board=NUCLEO-H753ZI clock=pll472 cpu_hz=472000000
#     d3cr=0x0000e000 (VOS0: VOS = 11 and VOSRDY) syscfg_pwrcr=0x00000081
#     oden=1 flash_acr=0x24 (4 wait states, WRHIGHFREQ 2) icache=1
#     dcache=1 mpu=1 cyccnt=1, and lse_cpu_hz (the CPU clock measured
#     against the LSE crystal) within 1.5% of 472 MHz;
#   - power: d3cr=0x0000e000 actvosrdy=1 supply=ldo;
#   - clock: flash_acr=0x24 cccsr=0x00000101 (the I/O compensation cell
#     enabled, EN, and READY; RM0433 12.3.7);  vos0: syscfg_pwrcr=0x00000081
#     oden=1;
#   - tcmprobe: itcm= and dtcm= both equal want=;
#   - selftest: delay=8000000 on every line; cycles= the same value on
#     every line of the current boot after n=0 (n=0 is the first
#     measurement, a few cycles more), and every cycles= in
#     delay .. delay + 64.
# Report lines: lse_cpu_hz, cycles, stack_used.
#
# Exit: 0 pass, 1 a check failed, 2 a tool error or usage error.
set -euo pipefail
die() { echo "check-selftest.sh: $*" >&2; exit 2; }
[ $# -eq 2 ] || die "usage: check-selftest.sh <capture.txt> <selftest-pll480.bin>"
cap=$1 img=$2
[ -f "$cap" ] || die "$cap: no such file"
[ -f "$img" ] || die "$img: no such file"
read -r crc bytes < <(cksum < "$img")

# partial_end: the input without a last line that has no newline.
partial_end() { awk '{ if (NR > 1) print prev; prev = $0 } END { if (NR && !partial) print prev }' partial="$1"; }
partial=0
# ($(...) drops a trailing newline: empty means the capture ends with one.)
[ -z "$(tr -d '\000\r' < "$cap" | tail -c 1)" ] || partial=1
tr -d '\000\r' < "$cap" | partial_end "$partial" | awk -v want_img="image_bytes=$bytes image_cksum=$crc" '
  function err(m) { print "ERR " m; exit 2 }
  function bad(m) { print "BAD " m; nbad++ }
  function field(line, k,   i, n, a) {
    n = split(line, a, " ")
    for (i = 1; i <= n; i++) if (index(a[i], k "=") == 1) return substr(a[i], length(k) + 2)
    return ""
  }
  { L[++N] = $0 }
  END {
    # The current boot: the last selftest line that starts a run.
    prev = -1; s = 0; p = 0; lastst = 0
    for (i = 1; i <= N; i++) {
      if (L[i] !~ /^h7bench v1 selftest n=[0-9]+ /) continue
      n = field(L[i], "n") + 0
      if (n == 0 || n != prev + 1) { s = i; p = lastst }
      prev = n; lastst = i
    }
    if (!s) err("no selftest line in the capture")
    fmt["banner"] = "^h7bench v1 banner image=selftest placement=[a-z-]+ board=[A-Z0-9-]+ clock=[a-z0-9]+ cpu_hz=[0-9]+ lse_cpu_hz=([0-9]+|unmeasured|fail) baud=[0-9]+ d3cr=0x[0-9a-f]{8} syscfg_pwrcr=0x[0-9a-f]{8} oden=[01] flash_acr=0x[0-9a-f]+ icache=[01] dcache=[01] mpu=[01] cyccnt=[01] image_bytes=[0-9]+ image_cksum=[0-9]+$"
    fmt["power"] = "^h7bench v1 power cr3=0x[0-9a-f]{8} csr1=0x[0-9a-f]{8} d3cr=0x[0-9a-f]{8} actvosrdy=[01] supply=[a-z-]+$"
    fmt["clock"] = "^h7bench v1 clock rcc_cr=0x[0-9a-f]{8} cfgr=0x[0-9a-f]{8} d1cfgr=0x[0-9a-f]{8} d2cfgr=0x[0-9a-f]{8} d3cfgr=0x[0-9a-f]{8} flash_acr=0x[0-9a-f]+ rsr=0x[0-9a-f]{8} cccsr=0x[0-9a-f]{8}$"
    fmt["pll"] = "^h7bench v1 pll pllckselr=0x[0-9a-f]{8} pllcfgr=0x[0-9a-f]{8} pll1divr=0x[0-9a-f]{8} pll1fracr=0x[0-9a-f]{8}$"
    fmt["vos0"] = "^h7bench v1 vos0 syscfg_pwrcr=0x[0-9a-f]{8} oden=[01] pwrcr_before=0x[0-9a-f]{8} polls_to_drop=[0-9]+ polls_low=[0-9]+$"
    fmt["cpu"] = "^h7bench v1 cpu cpuid=0x[0-9a-f]{8} idcode=0x[0-9a-f]{8} optsr=0x[0-9a-f]{8} mpu_ctrl=0x[0-9a-f]+ mpu_regions=[0-9]+$"
    fmt["tcm"] = "^h7bench v1 tcm itcmcr=0x[0-9a-f]+ dtcmcr=0x[0-9a-f]+ at_init=0x[0-9a-f]+,0x[0-9a-f]+ itcm_bytes=[0-9]+ dtcm_table_bytes=[0-9]+ msp=0x[0-9a-f]{8}$"
    fmt["tcmprobe"] = "^h7bench v1 tcmprobe want=0x[0-9a-f]+ itcm=0x[0-9a-f]+ dtcm=0x[0-9a-f]+$"
    fmt["selftest"] = "^h7bench v1 selftest n=[0-9]+ delay=[0-9]+ cycles=[0-9]+ cyccnt=[0-9]+ stack_used=[0-9]+$"
    nst = 0; cyc = ""; vary = 0
    # From the first line of the current run: the lines before it (the
    # previous boot, or where the VCP lost bytes) are not judged.
    for (i = s; i <= N; i++) {
      l = L[i]
      if (l ~ /^[ \t]*$/) continue
      if (l ~ /^h7bench v1 FAULT/) { bad("the board faulted: " l); continue }
      if (l !~ /^h7bench v1 [a-z0-9]+ /) err("line " i ": not an h7bench line: " l)
      split(l, t, " "); k = t[3]
      if (!(k in fmt)) err("line " i ": unknown kind " k ": " l)
      if (l !~ fmt[k]) err("line " i ": malformed " k " line: " l)
      if (k == "selftest") {
        if (i < s) continue
        nst++
        d = field(l, "delay"); c = field(l, "cycles")
        if (d != 8000000) bad("selftest n=" field(l, "n") ": delay=" d " (want 8000000)")
        if (c + 0 < d + 0 || c + 0 > d + 64) bad("selftest n=" field(l, "n") ": cycles=" c " not in delay .. delay + 64")
        if (field(l, "n") != 0) { if (cyc == "") cyc = c; else if (c != cyc) vary = 1 }
        su = field(l, "stack_used")
        continue
      }
      cnt[k]++
      if (!(k in first)) first[k] = l
      else if (l != first[k]) err("line " i ": " k " lines differ within the run (a byte lost on the link?): " l)
    }
    for (k in fmt) if (k != "selftest" && cnt[k] < 2) err(cnt[k] + 0 " " k " line(s) after the new banner (want at least 2: read longer)")
    if (nst < 16) err(nst " selftest lines in the current boot (want at least 16: read longer)")
    b = first["banner"]
    got_img = "image_bytes=" field(b, "image_bytes") " image_cksum=" field(b, "image_cksum")
    if (got_img != want_img) err("the board is not running the image given: banner " got_img ", the image is " want_img)
    if (vary) bad("selftest cycles= differ between lines after n=0 (want one constant value)")
    split("board=NUCLEO-H753ZI clock=pll472 cpu_hz=472000000 d3cr=0x0000e000 syscfg_pwrcr=0x00000081 oden=1 flash_acr=0x24 icache=1 dcache=1 mpu=1 cyccnt=1", want, " ")
    for (j in want) { split(want[j], kv, "="); if (field(b, kv[1]) != kv[2]) bad("banner " kv[1] "=" field(b, kv[1]) " (want " kv[2] ")") }
    lse = field(b, "lse_cpu_hz")
    if (lse !~ /^[0-9]+$/) bad("banner lse_cpu_hz=" lse " (the LSE measurement failed)")
    else if ((lse - 472000000) * 1000 > 15 * 472000000 || (472000000 - lse) * 1000 > 15 * 472000000) bad("banner lse_cpu_hz=" lse " is more than 1.5% from 472000000")
    split("d3cr=0x0000e000 actvosrdy=1 supply=ldo", want, " ")
    for (j in want) { split(want[j], kv, "="); if (field(first["power"], kv[1]) != kv[2]) bad("power " kv[1] "=" field(first["power"], kv[1]) " (want " kv[2] ")") }
    if (field(first["clock"], "flash_acr") != "0x24") bad("clock flash_acr=" field(first["clock"], "flash_acr") " (want 0x24)")
    if (field(first["clock"], "cccsr") != "0x00000101") bad("clock cccsr=" field(first["clock"], "cccsr") " (want 0x00000101: compensation cell EN and READY)")
    if (field(first["vos0"], "syscfg_pwrcr") != "0x00000081") bad("vos0 syscfg_pwrcr=" field(first["vos0"], "syscfg_pwrcr") " (want 0x00000081)")
    if (field(first["vos0"], "oden") != "1") bad("vos0 oden=" field(first["vos0"], "oden") " (want 1)")
    tp = first["tcmprobe"]
    if (field(tp, "itcm") != field(tp, "want") || field(tp, "dtcm") != field(tp, "want")) bad("tcmprobe not equal: " tp)
    print "INFO judged from line " s " (the current run: " nst " selftest lines, " cnt["banner"] " banners)"
    print "INFO banner " got_img " = the image; clock=" field(b, "clock") " cpu_hz=" field(b, "cpu_hz") " lse_cpu_hz=" lse " flash_acr=" field(b, "flash_acr") " oden=" field(b, "oden") " cccsr=" field(first["clock"], "cccsr") " rcc_cr=" field(first["clock"], "rcc_cr")
    print "INFO power actvosrdy=" field(first["power"], "actvosrdy") " supply=" field(first["power"], "supply") "; tcmprobe " tp
    print "INFO selftest cycles=" cyc " (after n=0) delay=8000000 stack_used=" su
    exit nbad ? 1 : 0
  }' > "$cap.check" && rc=0 || rc=$?
sed -n 's/^INFO /check-selftest.sh: /p; s/^BAD /check-selftest.sh: FAIL: /p; s/^ERR /check-selftest.sh: ERROR: /p' "$cap.check"
case $rc in
  0) echo "check-selftest.sh: OK" ;;
  1) echo "check-selftest.sh: FAILED" >&2 ;;
  *) echo "check-selftest.sh: tool error; nothing judged" >&2; rc=2 ;;
esac
exit "$rc"
