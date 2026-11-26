#!/usr/bin/env bash
# Builds every h7-bench image into out/ (ELF and .bin), prints section sizes
# and checks what can be checked without the board. README.md "Build".
#
#   ./build.sh            the NUCLEO-H753ZI images, all at the lib's 472 MHz
#                         HSI clock (plan D11) and named *-pll480 as the
#                         figures in README.md are: selftest, clockcheck,
#                         the three placements; then the host expected-hash
#                         test. Images go to out/h753/.
#   H7_BENCH_QEMU=1       also the QEMU smoke run (needs qemu-system-arm)
#   H7_BENCH_LOCK=<file>  run every cargo and QEMU command under
#                         `flock -x <file>` (agents: the shared build lock)
#   H7_BENCH_ITCM_DROP    tiers of perf/itcm-sections.txt left in flash
#                         (default "plain tile path"; build.rs)
#   H7_BENCH_CPU_INSTRS   path of blargg's cpu_instrs.gb (default: the
#                         repo's test-roms/)
#
# The H755 board, the 392 MHz (`pll`) and the 64 MHz HSI images are gone
# (ADR 0036: the board bring-up is the gbc-nucleo-h753 lib's, which has
# only the H753 at 472 MHz): H7_BENCH_BOARD other than h753 and
# H7_BENCH_PLL are refused; H7_BENCH_PLL480 is accepted and changes
# nothing. The m4-stub/ sources are kept, unbuilt.
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
[ "$BOARD" = h753 ] || { echo "FAIL: H7_BENCH_BOARD=$BOARD: only h753 is built (the H755 is retired, ADR 0036)" >&2; exit 1; }
[ -z "${H7_BENCH_PLL:-}" ] || { echo "FAIL: H7_BENCH_PLL: the 392 MHz images are gone (ADR 0036); every image runs at 472 MHz" >&2; exit 1; }
MAX_BIN=$((2048 * 1024)) # both flash banks (single core)
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
  locked cargo build --release --features "board${features:+,$features}" "${REMAP[@]}"
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
  # The supply configuration must be the first code after reset, and the
  # fault handlers the lib's: cortex-m-rt PROVIDEs its defaults when an
  # image does not link them.
  local syms f d
  syms=$(readelf -sW "$OUT/$name.elf")
  grep -q ' FUNC .* __pre_init$' <<< "$syms" || bad "$name: no __pre_init function"
  for f in HardFault DefaultHandler; do
    grep -q " FUNC .* $f\$" <<< "$syms" || bad "$name: no $f function"
  done
  for d in DefaultPreInit HardFault_ DefaultHandler_; do
    ! grep -q " $d\$" <<< "$syms" || bad "$name: cortex-m-rt's default $d is linked"
  done
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
  # The fault stack the lib's HardFault moves to: in AXI SRAM, 8-byte
  # aligned, its full size above the end of the `.axisram` buffers.
  local ftop axi_end
  ftop=$(sym "$syms" __fault_stack_top)
  axi_end=$(readelf -SW "$OUT/$name.elf" | sed -n 's/^ *\[ *[0-9]*\] *//p' |
    awk '$1 == ".axisram" {printf "%08x", strtonum("0x" $3) + strtonum("0x" $5)}')
  echo "  fault stack top 0x$ftop; .axisram ends 0x${axi_end:-none}"
  [ -n "$ftop" ] && in_range "$ftop" "$AXI_LO" "$AXI_HI" || bad "$name: __fault_stack_top 0x$ftop outside AXI SRAM"
  [ -n "$ftop" ] && [ $((16#$ftop % 8)) -eq 0 ] || bad "$name: __fault_stack_top 0x$ftop not 8-byte aligned"
  [ -n "$axi_end" ] && [ -n "$ftop" ] && [ $((16#$ftop - 16#$axi_end)) -ge "$FAULT_STACK_BYTES" ] ||
    bad "$name: fewer than $FAULT_STACK_BYTES B between .axisram and __fault_stack_top"
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
FAULT_STACK_BYTES=2048 # memory_x::FAULT_STACK_BYTES

image selftest-pll480 selftest "$AXI_LO" "$AXI_HI"
image clockcheck-pll480 clockcheck "$AXI_LO" "$AXI_HI"
image bench-flash-axi-pll480 "" "$AXI_LO" "$AXI_HI"
image bench-flash-dtcm-pll480 dtcm "$DTCM_LO" "$DTCM_HI"
image bench-itcm-dtcm-pll480 itcm "$DTCM_LO" "$DTCM_HI"
itcm_check bench-itcm-dtcm-pll480

echo "== host: expected lines (calls, frames, clocks, hash) against expected-hashes.txt"
(cd host && locked cargo test --release)
(cd host && locked cargo run --release)

# qemu <name> <features>: run the image on QEMU's Cortex-M7 board and
# compare its report with expected-hashes.txt.
qemu() {
  echo "== QEMU mps2-an500: $1 (features: $2)"
  locked cargo build --release --no-default-features --features "$2" "${REMAP[@]}"
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
