# shellcheck shell=bash
# Sourced. fnv1a64 <file>: FNV-1a 64 of the file's bytes as 16 hex digits,
# the hash of gbc-firmware's `fnv` (and of the ADR 0037 image header).
# Bash arithmetic is 64-bit and wraps, which is the FNV modulus; about 4 s
# per MiB.
fnv1a64() {
  local h=$((0xcbf29ce484222325)) b bs
  while read -r -a bs; do
    for b in "${bs[@]}"; do h=$(((h ^ b) * 0x100000001b3)); done
  done < <(od -An -v -tu1 -w32 "$1")
  printf '%016x\n' "$h"
}
