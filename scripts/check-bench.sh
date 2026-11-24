#!/usr/bin/env bash
# Deterministic instruction-count benchmark (docs/perf.md).
#
#   scripts/check-bench.sh                  compare with perf/bench-baseline.txt
#   scripts/check-bench.sh --update-baseline  rewrite the baseline
#
# Runs tools/bench under callgrind, collecting only inside the emulation loop
# (--toggle-collect), and fails if any ROM executes more than 3% more
# instructions than the baseline.
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

# Test ROMs: $TEST_ROMS (default test-roms/, e.g. the main checkout's from a
# git worktree).
ROMS_DIR=${TEST_ROMS:-test-roms}
BASELINE=perf/bench-baseline.txt
THRESHOLD_PCT=3
# ROM (relative to $ROMS_DIR, or `synth:<name>` for a ROM tools/bench builds
# in memory, docs/perf.md) and emulated seconds. Changing this list or
# the durations changes every count: update the baseline in the same commit.
BENCHES=(
  "blargg/cpu_instrs/cpu_instrs.gb 20"
  "blargg/cgb_sound/cgb_sound.gb 20"
  "cgb-acid2/cgb-acid2.gbc 20"
  # Synthetic ROMs (tools/bench/src/synth.rs): paths the ROMs above miss.
  "synth:event-storm 2"
  "synth:dma-storm 3"
  "synth:halt-irq 10"
  "synth:worst-case 2"
  "synth:cgb-raster 2"
  "synth:apu-writes 2"
)

update=0
case "${1:-}" in
  "") ;;
  --update-baseline) update=1 ;;
  *) echo "usage: $0 [--update-baseline]" >&2; exit 2 ;;
esac

command -v valgrind >/dev/null || { echo "check-bench: valgrind not found" >&2; exit 2; }

# Run the binary cargo just built, wherever CARGO_TARGET_DIR or a cargo config
# put it: a hard-coded target/release/bench may be stale.
bench=$(cargo build --release -p bench --quiet --message-format=json \
  | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)
[ -n "$bench" ] && [ -x "$bench" ] || { echo "check-bench: cannot find the bench binary cargo built" >&2; exit 2; }
# The build must carry the flags of .cargo/config.toml (ADR 0008's LLVM
# option, docs/perf.md "Build flags"): a RUSTFLAGS or
# CARGO_ENCODED_RUSTFLAGS variable replaces them without a word, and the
# numbers would be another build's. The binary reports what it was built
# with (tools/bench/build.rs).
source scripts/lib/rustflags.sh
built=$("$bench" --rustflags)
case " $built " in
  *" $GBC_RUSTFLAGS "*) ;;
  *)
    echo "check-bench: FAIL: bench was built with rustflags '$built'," >&2
    echo "  which lack '$GBC_RUSTFLAGS' (.cargo/config.toml); unset RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS" >&2
    exit 1
    ;;
esac
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

declare -A current
for b in "${BENCHES[@]}"; do
  read -r rom secs <<<"$b"
  if [[ $rom == synth:* ]]; then
    path=$rom
  else
    path=$ROMS_DIR/$rom
    [ -f "$path" ] || { echo "check-bench: $path missing (run scripts/fetch-test-roms.sh or set TEST_ROMS)" >&2; exit 2; }
    want=$(awk -v r="$rom" '$2 == r { print $1 }' acceptance/test-roms.sha256)
    have=$(sha256sum "$path" | cut -d' ' -f1)
    [ -n "$want" ] && [ "$want" = "$have" ] || { echo "check-bench: $path does not match acceptance/test-roms.sha256" >&2; exit 2; }
  fi
  out=$(valgrind --tool=callgrind --callgrind-out-file="$tmp/cg.out" \
    --toggle-collect='*run_emulation*' \
    "$bench" "$path" "$secs" 2>&1) || { echo "$out" >&2; exit 2; }
  ir=$(sed -n 's/^==[0-9]*== Collected : \([0-9]*\).*/\1/p' <<<"$out")
  [ -n "$ir" ] && [ "$ir" -gt 0 ] || { echo "check-bench: no Ir count for $rom" >&2; echo "$out" >&2; exit 2; }
  current[$rom]=$ir
done

if [ "$update" = 1 ]; then
  mkdir -p perf
  {
    echo "# Instruction-count baseline (docs/perf.md). Columns: ROM, emulated seconds, Ir."
    echo "# Rewritten by: scripts/check-bench.sh --update-baseline"
    echo "# Commit changes together with the change that caused them (delta and reason in the body)."
    for b in "${BENCHES[@]}"; do
      read -r rom secs <<<"$b"
      echo "$rom $secs ${current[$rom]}"
    done
  } > "$BASELINE"
  echo "check-bench: wrote $BASELINE"
  exit 0
fi

[ -f "$BASELINE" ] || { echo "check-bench: $BASELINE missing (run with --update-baseline)" >&2; exit 2; }

fail=0
printf '%-36s %15s %15s %8s\n' ROM baseline current delta
for b in "${BENCHES[@]}"; do
  read -r rom secs <<<"$b"
  base=$(awk -v r="$rom" -v s="$secs" '$1 == r && $2 == s { print $3 }' "$BASELINE")
  if [ -z "$base" ]; then
    echo "check-bench: no baseline for '$rom $secs' (run with --update-baseline)" >&2
    fail=1
    continue
  fi
  cur=${current[$rom]}
  # Delta in hundredths of a percent, integer maths.
  d=$(( (cur - base) * 10000 / base ))
  note=""
  if [ "$d" -gt $((THRESHOLD_PCT * 100)) ]; then note="REGRESSION"; fail=1
  elif [ "$d" -lt 0 ]; then note="improved"; fi
  sign=+; [ "$d" -lt 0 ] && sign=-; a=${d#-}
  printf "%-36s %15d %15d %s%d.%02d%% %s\n" "$rom" "$base" "$cur" "$sign" $((a / 100)) $((a % 100)) "$note"
done
if [ "$fail" = 1 ]; then
  echo "check-bench: FAIL (limit +${THRESHOLD_PCT}%)" >&2
  exit 1
fi
echo "check-bench: ok (limit +${THRESHOLD_PCT}%)"
