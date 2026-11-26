# shellcheck shell=bash
# Sourced by the firmware build scripts (crates/gbc-nucleo-h753/build.sh,
# tools/h7-bench/build.sh).
#
#   check_image_range <name> <elf> <bin> [max_bin]
#
# The image's identity range (crates/gbc-nucleo-h753/src/image.rs) is what
# the image hashes at run time: from the start of flash to the largest of
# the end of `.rodata` (__erodata) and the ends of the load images in flash
# of `.data`, `.itcm` and `.dtcm_rodata` (a section the placement leaves
# out has start = end outside flash and is skipped). It must be exactly the
# `.bin` objcopy wrote, or the banner's image_bytes/image_cksum (and
# run-headless.sh's check against `cksum < .bin`) can never match, and a
# section added to flash later would be silently left out.
#
# Prints `  image range 0x08000000..<end>; cksum <crc>` on success. On failure
# prints a `FAIL: ...` line to stderr and returns 1 (the caller's `bad`).

check_image_range() {
  local name=$1 elf=$2 bin=$3 max_bin=${4:-$((2048 * 1024))}
  local syms img_end size t s e li v end_ r
  syms=$(readelf -sW "$elf") || { echo "FAIL: $name: readelf -sW $elf failed" >&2; return 1; }
  # _sym <name>: the symbol's value in decimal; empty when absent.
  _sym() { local h; h=$(awk -v n="$1" '$8 == n {print $2; exit}' <<< "$syms"); [ -n "$h" ] && echo $((16#$h)); }
  img_end=$(_sym __erodata)
  [ -n "$img_end" ] || { echo "FAIL: $name: no __erodata symbol, so no image range (src/image.rs)" >&2; return 1; }
  for t in "__sdata __edata __sidata" "__sitcm __eitcm __siitcm" "__sdtcm_ro __edtcm_ro __sidtcm_ro"; do
    read -r s e li <<< "$t"
    for v in "$s" "$e" "$li"; do
      [ -n "$(_sym "$v")" ] || { echo "FAIL: $name: no $v symbol, so no image range (src/image.rs)" >&2; return 1; }
    done
    end_=$(( $(_sym "$li") + $(_sym "$e") - $(_sym "$s") ))
    if [ "$end_" -gt $((0x08000000)) ] && [ "$end_" -le $((0x08000000 + max_bin)) ] && [ "$end_" -gt "$img_end" ]; then img_end=$end_; fi
  done
  size=$(stat -c %s "$bin")
  read -r r _ < <(cksum < "$bin")
  echo "  image range 0x08000000..$(printf 0x%08x "$img_end"); cksum $r"
  if [ $((img_end - 0x08000000)) -ne "$size" ]; then
    echo "FAIL: $name: image range $((img_end - 0x08000000)) B is not the .bin's $size B (src/image.rs)" >&2
    return 1
  fi
}
