#!/usr/bin/env bash
# Builds every h7-bench image into out/ (ELF and .bin), prints section sizes
# and checks what can be checked without the board. README.md "Build".
#
#   ./build.sh            selftest, clockcheck, the three placements, the host
#                         expected-hash test (and the M4 stub for h755)
#   H7_BENCH_BOARD        h753 (default: NUCLEO-H753ZI) or h755
#                         (NUCLEO-H755ZI-Q); images go to out/<board>/
#   H7_BENCH_PLL=1        also the phase B (PLL, 392 MHz) variants
#   H7_BENCH_PLL480=1     also the phase C (PLL, 472 MHz, VOS0)
#                         variants, *-pll480; h753 only
#   H7_BENCH_QEMU=1       also the QEMU smoke run (needs qemu-system-arm)
#   H7_BENCH_LOCK=<file>  run every cargo and QEMU command under
#                         `flock -x <file>` (agents: the shared build lock)
#   H7_BENCH_ITCM_DROP    tiers of perf/itcm-sections.txt left in flash
#                         (default "plain tile path"; build.rs)
#   H7_BENCH_CPU_INSTRS   path of blargg's cpu_instrs.gb (default: the
#                         repo's test-roms/)
#
# One target directory, images built one after the other (AGENTS.md
# "Machine resources"); never pass -j.
set -euo pipefail
cd "$(dirname "$0")"
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"

TARGET=thumbv7em-none-eabihf

# Path-independent images: the panic-location strings and debug info embed
# the checkout path, the cargo registry and the toolchain sources, so the
# same source gave different .bins in different worktrees. cargo's
# `trim-paths` is unstable on 1.100.0 and a cargo config cannot expand $PWD,
# so the prefixes are remapped here with --config: its rustflags array is
# appended to the one in .cargo/config.toml (CodeGenPrepare flag kept).
ROOT=$(cd ../.. && pwd)
REMAP=(--config "target.$TARGET.rustflags=['--remap-path-prefix=$ROOT=/gbc-32','--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/registry/src=/cargo-registry','--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup']")
BOARD=${H7_BENCH_BOARD:-h753}
case "$BOARD" in
  h753) MAX_BIN=$((2048 * 1024)) ;;  # both flash banks (single core)
  h755) MAX_BIN=$((1024 * 1024)) ;;  # flash bank 1; bank 2 is the Cortex-M4's
  *) echo "FAIL: H7_BENCH_BOARD=$BOARD: not h753 or h755" >&2; exit 1 ;;
esac
if [ -n "${H7_BENCH_PLL480:-}" ] && [ "$BOARD" != h753 ]; then
  echo "FAIL: H7_BENCH_PLL480 needs H7_BENCH_BOARD=h753 (VOS0 needs the LDO supply)" >&2; exit 1
fi
OUT=out/$BOARD
echo "== board $BOARD; images in $OUT"

locked() { if [ -n "${H7_BENCH_LOCK:-}" ]; then flock -x "$H7_BENCH_LOCK" "$@"; else "$@"; fi; }

# rust-objcopy ships inside the pinned toolchain; no llvm-tools install.
host=$(rustc -vV | sed -n 's/^host: //p')
sysroot=$(rustc --print sysroot)
[ -x "$sysroot/lib/rustlib/$host/bin/rust-objcopy" ] || { echo "FAIL: no rust-objcopy in $sysroot" >&2; exit 1; }
# It links against the toolchain's own libLLVM.
objcopy() { LD_LIBRARY_PATH="$sysroot/lib" "$sysroot/lib/rustlib/$host/bin/rust-objcopy" "$@"; }
command -v readelf >/dev/null || { echo "FAIL: readelf (binutils) not found" >&2; exit 1; }

mkdir -p "$OUT"
fail=0
bad() { echo "FAIL: $*" >&2; fail=1; }

# word <file> <index>: little-endian 32-bit word of a .bin, as 8 hex digits.
word() { od -An -tx4 -j $(($2 * 4)) -N4 "$1" | tr -d ' \n'; }

# sym <readelf -sW output> <name>: the symbol's value, 8 hex digits.
sym() { awk -v n="$2" '$8 == n {print $2; exit}' <<< "$1"; }

# in_range <hex> <lo> <hi>: lo <= value <= hi
in_range() { [ $((16#$1)) -ge $(($2)) ] && [ $((16#$1)) -le $(($3)) ]; }

# image <name> <features> <ram lo> <ram hi>
image() {
  local name=$1 features=$2 ram_lo=$3 ram_hi=$4
  echo "== $name (features: ${features:-none})"
  locked cargo build --release --no-default-features --features "$BOARD${features:+,$features}" "${REMAP[@]}"
  cp "target/$TARGET/release/h7-bench" "$OUT/$name.elf"
  objcopy -O binary "$OUT/$name.elf" "$OUT/$name.bin"

  # Allocated sections: name, address, size in bytes.
  readelf -SW "$OUT/$name.elf" | sed -n 's/^ *\[ *[0-9]*\] *//p' | awk '
    $1 ~ /^\./ && $7 ~ /A/ { printf "  %-14s %-8s addr 0x%s size %7d\n", $1, $2, $3, strtonum("0x" $5) }'
  local size sp reset
  size=$(stat -c %s "$OUT/$name.bin")
  sp=$(word "$OUT/$name.bin" 0)
  reset=$(word "$OUT/$name.bin" 1)
  echo "  bin $size bytes; vector[0] (initial SP) 0x$sp; vector[1] (reset) 0x$reset"
  [ "$size" -le "$MAX_BIN" ] || bad "$name.bin is $size bytes: over $MAX_BIN B of flash"
  in_range "$sp" "$ram_lo" "$ram_hi" || bad "$name: initial SP 0x$sp outside RAM"
  [ $((16#$sp % 8)) -eq 0 ] || bad "$name: initial SP 0x$sp not 8-byte aligned"
  in_range "$reset" 0x08000000 $((0x08000000 + size)) || bad "$name: reset vector 0x$reset outside the image"
  [ $((16#$reset & 1)) -eq 1 ] || bad "$name: reset vector 0x$reset lacks the Thumb bit"
  # The supply configuration must be the first code after reset.
  local syms
  syms=$(readelf -sW "$OUT/$name.elf")
  grep -q ' FUNC .* __pre_init$' <<< "$syms" || bad "$name: no __pre_init function"
  # What the startup code zeroes, copies and paints must be inside RAM and
  # in order (a section inserted after .bss in another region broke this).
  local sbss ebss send sstart sdata edata v
  sbss=$(sym "$syms" __sbss); ebss=$(sym "$syms" __ebss)
  sdata=$(sym "$syms" __sdata); edata=$(sym "$syms" __edata)
  send=$(sym "$syms" _stack_end); sstart=$(sym "$syms" _stack_start)
  echo "  .data $sdata..$edata  .bss $sbss..$ebss  stack $send..$sstart"
  for v in "$sdata" "$edata" "$sbss" "$ebss" "$send" "$sstart"; do
    in_range "$v" "$ram_lo" "$ram_hi" || bad "$name: startup symbol 0x$v outside RAM"
  done
  { [ $((16#$sdata)) -le $((16#$edata)) ] && [ $((16#$edata)) -le $((16#$sbss)) ] &&
    [ $((16#$sbss)) -le $((16#$ebss)) ] && [ $((16#$ebss)) -le $((16#$send)) ] &&
    [ $((16#$send)) -lt $((16#$sstart)) ]; } || bad "$name: startup symbols out of order"
}

# itcm_check <name>: the post-link ITCM check (scripts/check-itcm-elf.sh,
# docs/perf.md "ITCM placement plan"), told the tiers build.rs left in flash
# (same variable, same default) and the rlib the image was linked from.
itcm_check() {
  local drop=${H7_BENCH_ITCM_DROP-plain tile path}
  echo "  post-link ITCM check (--drop '${drop}')"
  ../../scripts/check-itcm-elf.sh --drop "$drop" --rlib "target/$TARGET/release" "$OUT/$1.elf" | sed 's/^/  /' ||
    bad "$1: post-link ITCM check (scripts/check-itcm-elf.sh)"
}

AXI_LO=0x24000000 AXI_HI=0x24080000 DTCM_LO=0x20000000 DTCM_HI=0x20020000

image selftest selftest "$AXI_LO" "$AXI_HI"
image clockcheck clockcheck "$AXI_LO" "$AXI_HI"
image bench-flash-axi "" "$AXI_LO" "$AXI_HI"
image bench-flash-dtcm dtcm "$DTCM_LO" "$DTCM_HI"
image bench-itcm-dtcm itcm "$DTCM_LO" "$DTCM_HI"
itcm_check bench-itcm-dtcm
if [ -n "${H7_BENCH_PLL:-}" ]; then
  image selftest-pll selftest,pll "$AXI_LO" "$AXI_HI"
  image clockcheck-pll clockcheck,pll "$AXI_LO" "$AXI_HI"
  image bench-flash-axi-pll pll "$AXI_LO" "$AXI_HI"
  image bench-flash-dtcm-pll dtcm,pll "$DTCM_LO" "$DTCM_HI"
  image bench-itcm-dtcm-pll itcm,pll "$DTCM_LO" "$DTCM_HI"
  itcm_check bench-itcm-dtcm-pll
fi
if [ -n "${H7_BENCH_PLL480:-}" ]; then
  image selftest-pll480 selftest,pll480 "$AXI_LO" "$AXI_HI"
  image clockcheck-pll480 clockcheck,pll480 "$AXI_LO" "$AXI_HI"
  image bench-flash-axi-pll480 pll480 "$AXI_LO" "$AXI_HI"
  image bench-flash-dtcm-pll480 dtcm,pll480 "$DTCM_LO" "$DTCM_HI"
  image bench-itcm-dtcm-pll480 itcm,pll480 "$DTCM_LO" "$DTCM_HI"
  itcm_check bench-itcm-dtcm-pll480
fi

# The H753 has no Cortex-M4: no stub.
if [ "$BOARD" = h755 ]; then
  echo "== m4-stub (optional Cortex-M4 image for 0x08100000)"
  (cd m4-stub && locked cargo build --release)
  cp "m4-stub/target/$TARGET/release/h7-m4-stub" "$OUT/m4-stub.elf"
  objcopy -O binary "$OUT/m4-stub.elf" "$OUT/m4-stub.bin"
  m4_reset=$(word "$OUT/m4-stub.bin" 1)
  echo "  bin $(stat -c %s "$OUT/m4-stub.bin") bytes; vector[0] 0x$(word "$OUT/m4-stub.bin" 0); vector[1] (reset) 0x$m4_reset"
  in_range "$m4_reset" 0x08100040 0x08100100 || bad "m4-stub: reset vector 0x$m4_reset"
  [ $((16#$m4_reset & 1)) -eq 1 ] || bad "m4-stub: reset vector lacks the Thumb bit"
fi

echo "== host: expected lines (calls, frames, clocks, hash) against expected-hashes.txt"
(cd host && locked cargo test --release)
(cd host && locked cargo run --release)

# qemu <name> <features>: run the image on QEMU's Cortex-M7 board and
# compare its report with expected-hashes.txt.
qemu() {
  echo "== QEMU mps2-an500: $1 (features: $2)"
  locked cargo build --release --no-default-features --features "$BOARD,$2" "${REMAP[@]}"
  cp "target/$TARGET/release/h7-bench" "$OUT/$1.elf"
  locked timeout 900 qemu-system-arm -M mps2-an500 -cpu cortex-m7 -display none -monitor none \
    -serial none -semihosting-config enable=on,target=native -kernel "$OUT/$1.elf" \
    | tr -d '\r' > "$OUT/$1.txt" || bad "$1: QEMU exited with an error"
  if diff -u expected-hashes.txt "$OUT/$1.txt"; then
    echo "  QEMU lines match expected-hashes.txt"
  else
    bad "$1: QEMU lines differ from expected-hashes.txt"
  fi
}
if [ -n "${H7_BENCH_QEMU:-}" ]; then
  # The thumbv7em build of the frame loop; then the same with the hot set
  # in a stand-in ITCM (the copy, the linker rules and the thunks).
  qemu qemu qemu
  qemu qemu-itcm qemu,itcm
  thunks=$(readelf -sW "$OUT/qemu-itcm.elf" | grep -c 'Thumbv7ABSLongThunk' || true)
  echo "  qemu-itcm: $thunks long-branch thunks"
  [ "$thunks" -gt 0 ] || bad "qemu-itcm: no thunks, so the placement was not exercised"
fi

[ "$fail" -eq 0 ] || { echo "build.sh: FAILED" >&2; exit 1; }
echo "build.sh: OK; images in $(pwd)/$OUT"
