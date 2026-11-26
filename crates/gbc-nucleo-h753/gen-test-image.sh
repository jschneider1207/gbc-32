#!/usr/bin/env bash
# Writes a deterministic 4 MiB pseudo-random test image for the ROM loader
# (docs/reference/loader.md "Baud rate", docs/reference/m2-evidence.md):
# the AES-128-CTR keystream of a fixed key (zeros encrypted), so every run
# gives the same bytes. It is not a ROM: the loader and rom_image treat any
# bytes as the image's data.
#
#   ./gen-test-image.sh <out> [variant 1..255, default 1]
#
# The variant is the IV's last byte, so variants are different images (a
# load of one over another is a full load, not "already loaded"). Prints
# one line: `gen-test-image: <out> bytes=4194304 variant=<v> fnv1a64=<16 hex>`.
# Needs openssl (enc) and coreutils.
set -euo pipefail
here=$(dirname "$(readlink -f "$0")")
die() { echo "gen-test-image.sh: $*" >&2; exit 2; }
[ $# -ge 1 ] && [ $# -le 2 ] || die "usage: gen-test-image.sh <out> [variant]"
out=$1 variant=${2:-1}
case "$variant" in ''|*[!0-9]*) die "bad variant $variant";; esac
[ "$variant" -ge 1 ] && [ "$variant" -le 255 ] || die "variant $variant not in 1..255"
command -v openssl >/dev/null || die "openssl not found"

BYTES=4194304
# "gbc-32-m2-loader" in ASCII.
KEY=6762632d33322d6d322d6c6f61646572
IV=$(printf '000000000000000000000000000000%02x' "$variant")
# CTR is a stream mode: as many bytes out as in, no padding.
head -c "$BYTES" /dev/zero | openssl enc -aes-128-ctr -K "$KEY" -iv "$IV" -nosalt > "$out"
[ "$(stat -c %s "$out")" -eq "$BYTES" ] || die "$out: not $BYTES bytes"
. "$here/../../scripts/lib/fnv1a64.sh"
echo "gen-test-image: $out bytes=$BYTES variant=$variant fnv1a64=$(fnv1a64 "$out")"
