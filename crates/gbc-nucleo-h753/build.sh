#!/usr/bin/env bash
# Builds the gbc-nucleo-h753 images into out/ (ELF and .bin), prints their
# section sizes and checks what can be checked without the board, as
# tools/h7-bench/build.sh does.
#
#   ./build.sh            every image: boot, qspi-probe, qspi-mmap, qspi-write, loader,
#                         lcd-probe, lcd-dma-probe, dac-probe, headless-<rom> for each acceptance
#                         ROM (cpu_instrs, cgb-acid2), headless-qspi (the headless
#                         loop on whatever ROM the QSPI NOR holds), and
#                         headless-display-<rom> (the loop with the panel, M3) for
#                         cgb-acid2 and dmg-acid2
#                         and paced-<rom> (the audio-paced loop with display and
#                         DAC, M4) for cgb_sound and the synthetic worst-case ROM
#                         and link-probe (the serial link with a running emulator
#                         and the joystick, M5; ROM: GBC_LINK_ROM, else generated)
#                         and player / player-script (the M5 game player on the
#                         ROM in the QSPI NOR; player-script replays
#                         GBC_HEADLESS_INPUT for GBC_HEADLESS_FRAMES frames)
#   ./build.sh <image>... only the named images (e.g. `./build.sh lcd-probe`; a
#                         name that is no image is an error)
#   GBC_BUILD_LOCK=<file> run every cargo command under `flock -x <file>`
#                         (agents: the shared build lock)
#   GBC_TEST_ROMS=<dir>   where the test ROMs are (default: the repo's
#                         test-roms/; scripts/fetch-test-roms.sh)
#   GBC_ITCM_DROP         tiers of perf/itcm-sections.txt the headless
#                         images leave in flash (default "plain tile path";
#                         build.rs reads the same variable)
#   GBC_HEADLESS_INPUT    an input program for the headless images, and
#   GBC_HEADLESS_FRAMES   their frame count (build.rs; default: none, 600)
#   GBC_DISPLAY_START     the headless-display images' transfer start: te-wait
#                         (default), te, now or off (build.rs, src/bin/headless.rs)
#
# The `boot` image is built without features, the `headless` ones with
# `--features headless`, which selects the ITCM placement (build.rs), one
# build per ROM (the ROM is embedded): out/headless-<rom>.{elf,bin}; and
# once with `--features rom-qspi` (no ROM embedded): out/headless-qspi.{elf,bin};
# and with `--features display`, one per display ROM:
# out/headless-display-<rom>.{elf,bin}; and with `--features paced`, one per
# paced ROM: out/paced-<rom>.{elf,bin}. A paced ROM named `synth:<name>` is
# the bench's synthetic ROM (tools/bench/src/synth.rs, as h7-bench embeds
# it), written by `bench --write-synth` to out/<name>.gb first, so that the
# host's `headless --paced` can run the identical file (run-paced.sh).
#
# Checks per image: the vector table (initial SP in DTCM and 8-byte
# aligned, reset vector a Thumb address inside the image), the flash fit,
# the lib's handlers linked instead of cortex-m-rt's defaults, the startup
# symbols inside DTCM and in order, and the fault stack inside AXI SRAM
# and clear of the `.axisram` buffers. For the headless images also
# scripts/check-itcm-elf.sh (every kept function of the ITCM set in ITCM)
# and the DTCM use.
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
OUT=out
# name=path under GBC_TEST_ROMS: the M1 acceptance ROMs (plan Stage 2 M1).
HEADLESS_ROMS=(cpu_instrs=blargg/cpu_instrs/cpu_instrs.gb cgb-acid2=cgb-acid2/cgb-acid2.gbc)
# The M3 display ROMs: cgb-acid2 (the acceptance run) and dmg-acid2 (the
# human's look, plan Stage 2 M3 acceptance).
DISPLAY_ROMS=(cgb-acid2=cgb-acid2/cgb-acid2.gbc dmg-acid2=dmg-acid2/dmg-acid2.gb)
# The M4 acceptance ROMs (plan Stage 2 M4): cgb_sound and synth:worst-case.
PACED_ROMS=(cgb_sound=blargg/cgb_sound/cgb_sound.gb worst-case=synth:worst-case)
ROMS=${GBC_TEST_ROMS:-../../test-roms}
ITCM_DROP=${GBC_ITCM_DROP-plain tile path}
MAX_BIN=$((2048 * 1024)) # both flash banks (single core)
DTCM_LO=0x20000000 DTCM_HI=0x20020000 AXI_LO=0x24000000 AXI_HI=0x24080000
FAULT_STACK_BYTES=2048 # memory_x::FAULT_STACK_BYTES
STACK_GUARD_BYTES=128  # memory_x::STACK_GUARD_BYTES

locked() { if [ -n "${GBC_BUILD_LOCK:-}" ]; then flock -x "$GBC_BUILD_LOCK" "$@"; else "$@"; fi; }
# link_probe_rom <out>: the link probe's ROM (plan Stage 2 M5). No test ROM
# fits: rtc3test is MBC3+TIMER+BATTERY with no RAM, and mealybug's
# mbc3_rtc rewrites the clock as its test, so a save could not round-trip
# across a run. This one is 32 KiB of zeros with `nop; jr -2` at 0x100
# (it never touches cartridge RAM or the clock), cartridge type 0x10
# (MBC3+TIMER+RAM+BATTERY) and RAM size code 3 (32 KiB): its save is
# 32,768 + 48 bytes. The same bytes as `link_probe_rom()` in
# crates/gbc-firmware/tests/save.rs, which checks the save fixture
# (tools/gbc-pc/tests/fixtures/link-probe.sav) against it on the host.
link_probe_rom() {
  head -c 32768 /dev/zero > "$1.tmp"
  printf '\000\030\376\000' | dd of="$1.tmp" bs=1 seek=256 conv=notrunc status=none
  printf '\020' | dd of="$1.tmp" bs=1 seek=$((0x147)) conv=notrunc status=none
  printf '\003' | dd of="$1.tmp" bs=1 seek=$((0x149)) conv=notrunc status=none
  mv "$1.tmp" "$1"
}
# build_bin: the ELF path as cargo reports it (not an assumed target/ path).
. ../../scripts/lib/cargo-bin.sh
# check_image_range: the image's hashed range equals the .bin.
. ../../scripts/lib/image-range.sh

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

# The images to build: the arguments, or all of them.
ALL_IMAGES="boot qspi-probe qspi-mmap qspi-write loader lcd-probe lcd-dma-probe dac-probe"
for entry in "${HEADLESS_ROMS[@]}"; do ALL_IMAGES="$ALL_IMAGES headless-${entry%%=*}"; done
ALL_IMAGES="$ALL_IMAGES headless-qspi"
for entry in "${DISPLAY_ROMS[@]}"; do ALL_IMAGES="$ALL_IMAGES headless-display-${entry%%=*}"; done
for entry in "${PACED_ROMS[@]}"; do ALL_IMAGES="$ALL_IMAGES paced-${entry%%=*}"; done
ALL_IMAGES="$ALL_IMAGES link-probe player player-script"
for a in "$@"; do
  case " $ALL_IMAGES " in *" $a "*) ;; *) echo "build.sh: no image named $a (images: $ALL_IMAGES)" >&2; exit 2;; esac
done
SELECTED=("$@")
# want <image>: whether to build it.
want() {
  [ "${#SELECTED[@]}" -eq 0 ] && return 0
  local a
  for a in "${SELECTED[@]}"; do [ "$a" = "$1" ] && return 0; done
  return 1
}

# word <file> <index>: little-endian 32-bit word of a .bin, as 8 hex digits.
word() { od -An -tx4 -j $(($2 * 4)) -N4 "$1" | tr -d ' \n'; }
# sym <readelf -sW output> <name>: the symbol's value, 8 hex digits.
sym() { awk -v n="$2" '$8 == n {print $2; exit}' <<< "$1"; }
# in_range <hex> <lo> <hi>: lo <= value <= hi
in_range() { [ -n "$1" ] && [ $((16#$1)) -ge $(($2)) ] && [ $((16#$1)) -le $(($3)) ]; }

# check_image <name> <elf>: copies the ELF cargo just built (build_bin) to
# out/<name> and runs the checks above.
check_image() {
  local name=$1
  echo "== $name"
  local elf=$OUT/$name.elf bin=$OUT/$name.bin
  cp "$2" "$elf"
  objcopy -O binary "$elf" "$bin"

  # Allocated sections: name, address, size in bytes.
  readelf -SW "$elf" | sed -n 's/^ *\[ *[0-9]*\] *//p' | awk '
    $1 ~ /^\./ && $7 ~ /A/ { printf "  %-14s %-8s addr 0x%s size %7d\n", $1, $2, $3, strtonum("0x" $5) }'
  size=$(stat -c %s "$bin")
  sp=$(word "$bin" 0)
  reset=$(word "$bin" 1)
  echo "  bin $size bytes; vector[0] (initial SP) 0x$sp; vector[1] (reset) 0x$reset"
  [ "$size" -le "$MAX_BIN" ] || bad "$name.bin is $size bytes: over $MAX_BIN B of flash"
  in_range "$sp" "$DTCM_LO" "$DTCM_HI" || bad "$name: initial SP 0x$sp outside DTCM"
  [ $((16#$sp % 8)) -eq 0 ] || bad "$name: initial SP 0x$sp not 8-byte aligned"
  in_range "$reset" 0x08000000 $((0x08000000 + size)) || bad "$name: reset vector 0x$reset outside the image"
  [ $((16#$reset & 1)) -eq 1 ] || bad "$name: reset vector 0x$reset lacks the Thumb bit"

  syms=$(readelf -sW "$elf")
  # The supply configuration must be the first code after reset, and the
  # fault handlers the lib's: cortex-m-rt PROVIDEs its defaults when an
  # image does not link them.
  grep -q ' FUNC .* __pre_init$' <<< "$syms" || bad "$name: no __pre_init function"
  for f in HardFault DefaultHandler; do
    grep -q " FUNC .* $f\$" <<< "$syms" || bad "$name: no $f function"
  done
  for d in DefaultPreInit HardFault_ DefaultHandler_; do
    ! grep -q " $d\$" <<< "$syms" || bad "$name: cortex-m-rt's default $d is linked"
  done

  # What the startup code zeroes, copies and paints must be inside DTCM and
  # in order (a section inserted after .bss in another region broke this
  # in h7-bench).
  sbss=$(sym "$syms" __sbss); ebss=$(sym "$syms" __ebss)
  sdata=$(sym "$syms" __sdata); edata=$(sym "$syms" __edata)
  send=$(sym "$syms" _stack_end); sstart=$(sym "$syms" _stack_start)
  echo "  .data $sdata..$edata  .bss $sbss..$ebss  stack $send..$sstart"
  for v in "$sdata" "$edata" "$sbss" "$ebss" "$send" "$sstart"; do
    in_range "$v" "$DTCM_LO" "$DTCM_HI" || bad "$name: startup symbol 0x$v outside DTCM"
  done
  { [ $((16#$sdata)) -le $((16#$edata)) ] && [ $((16#$edata)) -le $((16#$sbss)) ] &&
    [ $((16#$sbss)) -le $((16#$ebss)) ] && [ $((16#$ebss)) -le $((16#$send)) ] &&
    [ $((16#$send)) -lt $((16#$sstart)) ]; } || bad "$name: startup symbols out of order"
  # The stack guard (mpu::REGION_STACK_GUARD): aligned to its size, above
  # the statics, the stack (and its paint) right above it.
  guard=$(sym "$syms" __stack_guard)
  echo "  stack guard 0x${guard:-none}, $STACK_GUARD_BYTES B"
  { [ -n "$guard" ] && [ $((16#$guard % STACK_GUARD_BYTES)) -eq 0 ] &&
    [ $((16#$guard)) -ge $((16#$ebss)) ] &&
    [ $((16#$guard + STACK_GUARD_BYTES)) -eq $((16#$send)) ]; } ||
    bad "$name: stack guard 0x${guard:-none} misplaced (.bss ends 0x$ebss, stack ends 0x$send)"

  # The image's identity (src/image.rs): the range the image hashes must be
  # exactly the .bin (scripts/lib/image-range.sh).
  check_image_range "$name" "$elf" "$bin" "$MAX_BIN" || fail=1

  # The fault stack: in AXI SRAM, 8-byte aligned, its full size above the
  # end of the `.axisram` buffers.
  ftop=$(sym "$syms" __fault_stack_top)
  axi_end=$(readelf -SW "$elf" | sed -n 's/^ *\[ *[0-9]*\] *//p' |
    awk '$1 == ".axisram" {printf "%08x", strtonum("0x" $3) + strtonum("0x" $5)}')
  echo "  fault stack top 0x$ftop; .axisram ends 0x${axi_end:-none}"
  in_range "$ftop" "$AXI_LO" "$AXI_HI" || bad "$name: __fault_stack_top 0x$ftop outside AXI SRAM"
  [ -n "$ftop" ] && [ $((16#$ftop % 8)) -eq 0 ] || bad "$name: __fault_stack_top 0x$ftop not 8-byte aligned"
  [ -n "$axi_end" ] && [ -n "$ftop" ] && [ $((16#$ftop - 16#$axi_end)) -ge "$FAULT_STACK_BYTES" ] ||
    bad "$name: fewer than $FAULT_STACK_BYTES B between .axisram and __fault_stack_top"
  echo "  DTCM: $((16#$send - DTCM_LO)) B of statics and tables, $((16#$sstart - 16#$send)) B of stack region"
}

# plain <image>: an image built as `boot` (no features), the
# bin of the same name.
plain() {
  want "$1" || return 0
  built=$(build_bin "$1" locked cargo build --release --bin "$1" "${REMAP[@]}")
  check_image "$1" "$built"
}
plain boot
# The QSPI NOR probe (plan Stage 2 M2).
plain qspi-probe
# The NOR in memory-mapped mode (plan Stage 2 M2).
plain qspi-mmap
# The NOR write side and UART receive (plan Stage 2 M2).
plain qspi-write
# The ROM loader (plan Stage 2 M2); GBC_LOADER_BAUD sets its serving rate
# (src/bin/loader.rs).
plain loader
# The ST7789V panel over the FMC (plan Stage 2 M3).
plain lcd-probe
# The MDMA frame transfer on TE (plan Stage 2 M3).
plain lcd-dma-probe
# DAC1 audio out (plan Stage 2 M4).
plain dac-probe

for entry in "${HEADLESS_ROMS[@]}"; do
  rom=${entry%%=*} path=$ROMS/${entry#*=}
  want "headless-$rom" || continue
  [ -f "$path" ] || { bad "headless-$rom: no ROM at $path (scripts/fetch-test-roms.sh, or set GBC_TEST_ROMS)"; continue; }
  built=$(GBC_HEADLESS_ROM=$(realpath "$path") GBC_ITCM_DROP=$ITCM_DROP build_bin headless locked cargo build --release --bin headless --features headless "${REMAP[@]}")
  check_image "headless-$rom" "$built"
  # The post-link ITCM check (docs/perf.md "ITCM placement plan"), told
  # the tiers build.rs left in flash and the rlib the image was linked
  # from.
  echo "  post-link ITCM check (--drop '$ITCM_DROP')"
  ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/headless-$rom.elf" | sed 's/^/  /' ||
    bad "headless-$rom: post-link ITCM check (scripts/check-itcm-elf.sh)"
done

# The headless image with its ROM from the QSPI NOR (plan Stage 2 M2,
# `rom-qspi`): no ROM embedded, so one image for every ROM; the same checks
# as the headless-<rom> images.
if want headless-qspi; then
  built=$(GBC_ITCM_DROP=$ITCM_DROP build_bin headless locked env -u GBC_HEADLESS_ROM cargo build --release --bin headless --features rom-qspi "${REMAP[@]}")
  check_image headless-qspi "$built"
  echo "  post-link ITCM check (--drop '$ITCM_DROP')"
  ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/headless-qspi.elf" | sed 's/^/  /' ||
    bad "headless-qspi: post-link ITCM check (scripts/check-itcm-elf.sh)"
fi

# The headless loop with the panel (plan Stage 2 M3, `display`): the same
# checks as the headless-<rom> images.
for entry in "${DISPLAY_ROMS[@]}"; do
  rom=${entry%%=*} path=$ROMS/${entry#*=}
  want "headless-display-$rom" || continue
  [ -f "$path" ] || { bad "headless-display-$rom: no ROM at $path (scripts/fetch-test-roms.sh, or set GBC_TEST_ROMS)"; continue; }
  echo "  GBC_DISPLAY_START=${GBC_DISPLAY_START:-te-wait}"
  built=$(GBC_HEADLESS_ROM=$(realpath "$path") GBC_ITCM_DROP=$ITCM_DROP build_bin headless locked cargo build --release --bin headless --features display "${REMAP[@]}")
  check_image "headless-display-$rom" "$built"
  echo "  post-link ITCM check (--drop '$ITCM_DROP')"
  ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/headless-display-$rom.elf" | sed 's/^/  /' ||
    bad "headless-display-$rom: post-link ITCM check (scripts/check-itcm-elf.sh)"
done

# The audio-paced loop with the display and the DAC (plan Stage 2 M4,
# `paced`): the same checks as the headless-<rom> images.
for entry in "${PACED_ROMS[@]}"; do
  rom=${entry%%=*} src=${entry#*=}
  want "paced-$rom" || continue
  case "$src" in
    synth:*)
      # The host build of the root workspace (its own target, not this
      # package's thumbv7em config).
      path=$OUT/$rom.gb
      (cd ../.. && locked cargo run --quiet --release -p bench -- --write-synth "${src#synth:}" "$OLDPWD/$path") ||
        { bad "paced-$rom: bench --write-synth ${src#synth:} failed"; continue; }
      echo "  $path: bench --write-synth ${src#synth:} ($(stat -c %s "$path") B)" ;;
    *)
      path=$ROMS/$src
      [ -f "$path" ] || { bad "paced-$rom: no ROM at $path (scripts/fetch-test-roms.sh, or set GBC_TEST_ROMS)"; continue; } ;;
  esac
  built=$(GBC_HEADLESS_ROM=$(realpath "$path") GBC_ITCM_DROP=$ITCM_DROP build_bin paced locked cargo build --release --bin paced --features paced "${REMAP[@]}")
  check_image "paced-$rom" "$built"
  echo "  post-link ITCM check (--drop '$ITCM_DROP')"
  ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/paced-$rom.elf" | sed 's/^/  /' ||
    bad "paced-$rom: post-link ITCM check (scripts/check-itcm-elf.sh)"
done

# The serial-link probe (plan Stage 2 M5, `link`): the same checks as the
# headless-<rom> images. Its ROM is GBC_LINK_ROM if set, else generated
# here (link_probe_rom), so the run scripts and gbc-pc have the same file.
if want link-probe; then
  path=${GBC_LINK_ROM:-$OUT/link-probe.gb}
  if [ -z "${GBC_LINK_ROM:-}" ]; then
    link_probe_rom "$path"
    echo "  $path: generated MBC3+TIMER+RAM+BATTERY test ROM ($(stat -c %s "$path") B)"
  fi
  if [ -f "$path" ]; then
    built=$(GBC_HEADLESS_ROM=$(realpath "$path") GBC_ITCM_DROP=$ITCM_DROP build_bin link-probe locked cargo build --release --bin link-probe --features link "${REMAP[@]}")
    check_image link-probe "$built"
    echo "  post-link ITCM check (--drop '$ITCM_DROP')"
    ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/link-probe.elf" | sed 's/^/  /' ||
      bad "link-probe: post-link ITCM check (scripts/check-itcm-elf.sh)"
  else
    bad "link-probe: no ROM at $path (GBC_LINK_ROM)"
  fi
fi

# The game player (plan Stage 2 M5; src/bin/player.rs): no ROM embedded
# (the QSPI NOR's image); `player-script` is the same bin in scripted mode
# (GBC_HEADLESS_INPUT, GBC_HEADLESS_FRAMES). The same checks as the
# headless-<rom> images.
for img in player player-script; do
  want "$img" || continue
  built=$(GBC_ITCM_DROP=$ITCM_DROP build_bin player locked env -u GBC_HEADLESS_ROM cargo build --release --bin player --features "$img" "${REMAP[@]}")
  check_image "$img" "$built"
  echo "  post-link ITCM check (--drop '$ITCM_DROP')"
  ../../scripts/check-itcm-elf.sh --drop "$ITCM_DROP" --rlib "$(dirname "$built")" "$OUT/$img.elf" | sed 's/^/  /' ||
    bad "$img: post-link ITCM check (scripts/check-itcm-elf.sh)"
done

[ "$fail" -eq 0 ] || { echo "build.sh: FAILED" >&2; exit 1; }

echo "build.sh: OK; images in $(pwd)/$OUT"
