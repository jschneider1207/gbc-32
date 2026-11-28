#!/usr/bin/env bash
# Layout-perturbed A/B sets (docs/perf.md "Session 30 (cont.): BTAC-off
# ratchet (M1), layout sets (M2), align-loops (P6)"; README.md "Layout sets").
#
# One build of the ITCM bench image is one sample of the branch predictor's
# layout noise (ADR 0042: up to about +-1.7% on bound workloads from a 16 B
# ITCM shift), so a variant is judged as a SET: the same source built at N
# ITCM start pads (memory_x::ItcmSet::with_pad, H7_BENCH_ITCM_PAD), each run
# once on the board, and the per-workload mean over the set compared.
#
#   ./layout-set.sh run <label> [--btac-off] [--pads "0 4 8 16 24"]
#                       [--rustflags "<flag> ..."] [--secs 85]
#   ./layout-set.sh summary <set dir>
#   ./layout-set.sh compare <set dir A> <set dir B>
#
# run: builds `bench-itcm-dtcm` (features board,itcm; with --btac-off also
#   btac-off) once per pad, all before the board, into out/h753/sets/<label>/
#   (pad<N>.elf/.bin, `.itcm` size in sizes.txt, the flags in flags.txt);
#   then, one at a time, run-qspi.sh on each (the loader, the bench pack in
#   the NOR, the image, read <secs> s; check-report.sh --qspi must pass:
#   hashes and image CRC), captures in <set dir>/runs/; then the summary.
#   --rustflags: extra rustc flags for the variant (split on spaces), added
#   to the target rustflags as build.sh's path remaps are. A run that leaves
#   a failed capture stops the set (exit 1).
# summary: per workload line (flash and qspi pass), the mean of the builds'
#   `mean=` and `max=`, their min and max, and the spread (max - min) /
#   set mean, from each capture's last complete report (layout-set.exs).
# compare: per line, B's set mean against A's, and whether the difference
#   is within noise (|delta| < the larger of the two sets' spreads).
#
# The pads are the one layout knob that moves only the hot set: `.text_core`
# is at a fixed flash address (ADR 0042), so the flash code does not move.
# Build in the main checkout (ADR 0042: the path moves code). The board
# steps hold the board lock per run (run-qspi.sh); nothing else may build
# meanwhile (AGENTS.md "Machine resources": one cargo command at a time).
#
# Exit: 0 done; 1 a build or a run failed; 2 usage.
set -euo pipefail
here=$(dirname "$(readlink -f "$0")")
cd "$here"
die() { echo "layout-set.sh: $*" >&2; exit 2; }
usage="usage: layout-set.sh run <label> [--btac-off] [--pads \"0 4 8 16 24\"] [--rustflags \"...\"] [--secs 85] | summary <dir> | compare <dirA> <dirB>"
exs() { elixir "$here/layout-set.exs" "$@"; }

case "${1:-}" in
  summary) [ $# -eq 2 ] || die "$usage"; exs summary "$2"; exit ;;
  compare) [ $# -eq 3 ] || die "$usage"; exs compare "$2" "$3"; exit ;;
  run) shift ;;
  *) die "$usage" ;;
esac
[ $# -ge 1 ] || die "$usage"
label=$1; shift
case "$label" in ''|*[!A-Za-z0-9._-]*) die "label '$label': letters, digits, . _ - only" ;; esac
features=board,itcm pads="0 4 8 16 24" extra="" secs=85
while [ $# -gt 0 ]; do
  case "$1" in
    --btac-off) features=$features,btac-off; shift ;;
    --pads) [ $# -ge 2 ] || die "$usage"; pads=$2; shift 2 ;;
    --rustflags) [ $# -ge 2 ] || die "$usage"; extra=$2; shift 2 ;;
    --secs) [ $# -ge 2 ] || die "$usage"; secs=$2; shift 2 ;;
    *) die "$usage" ;;
  esac
done
for p in $pads; do case "$p" in ''|*[!0-9]*) die "pad '$p' is not a number" ;; esac; done
[ "$(wc -w <<< "$pads")" -ge 2 ] || die "a set needs at least two pads"

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
TARGET=thumbv7em-none-eabihf
# As build.sh: path-independent images (its REMAP), plus the variant's flags.
ROOT=$(cd ../.. && pwd)
flags="'--remap-path-prefix=$ROOT=/gbc-32','--remap-path-prefix=${CARGO_HOME:-$HOME/.cargo}/registry/src=/cargo-registry','--remap-path-prefix=${RUSTUP_HOME:-$HOME/.rustup}=/rustup'"
for f in $extra; do flags="$flags,'$f'"; done
. ../../scripts/lib/cargo-bin.sh
host=$(rustc -vV | sed -n 's/^host: //p')
sysroot=$(rustc --print sysroot)
objcopy() { LD_LIBRARY_PATH="$sysroot/lib" "$sysroot/lib/rustlib/$host/bin/rust-objcopy" "$@"; }

dir=$here/out/h753/sets/$label
[ ! -e "$dir" ] || die "$dir exists (pick another label, or remove it)"
mkdir -p "$dir/runs"
{ echo "features=$features"; echo "rustflags=$extra"; echo "pads=$pads"
  echo "commit=$(git rev-parse --short=12 HEAD)$(git diff --quiet HEAD -- ../../crates . ../memory-x ../../perf ../../.cargo || echo +dirty)"; } > "$dir/flags.txt"

for p in $pads; do
  echo "== layout-set.sh: build pad $p ($features${extra:+; rustflags $extra})"
  elf=$(H7_BENCH_ITCM_PAD=$p build_bin h7-bench cargo build --release --features "$features" \
    --config "target.$TARGET.rustflags=[$flags]") || { echo "layout-set.sh: build failed (pad $p)" >&2; exit 1; }
  cp "$elf" "$dir/pad$p.elf"
  objcopy -O binary "$dir/pad$p.elf" "$dir/pad$p.bin"
  readelf -SW "$dir/pad$p.elf" | sed -n 's/^ *\[ *[0-9]*\] *//p' |
    awk -v p="$p" '$1 == ".itcm" || $1 == ".text_core" || $1 == ".text" { printf "pad%s %s %d\n", p, $1, strtonum("0x" $5) }' >> "$dir/sizes.txt"
done
cat "$dir/sizes.txt"

for p in $pads; do
  echo "== layout-set.sh: board run pad $p"
  rc=0
  H7_BENCH_RUNS=$dir/runs ./run-qspi.sh "$dir/pad$p.bin" "$secs" > "$dir/runs/pad$p.log" 2>&1 || rc=$?
  grep -E '^run-qspi.sh|^check-report.sh: (OK|FAIL)' "$dir/runs/pad$p.log" | tail -n 3 || true
  [ "$rc" -eq 0 ] || { echo "layout-set.sh: run-qspi.sh exited $rc on pad $p (log $dir/runs/pad$p.log)" >&2; exit 1; }
done
exs summary "$dir"
