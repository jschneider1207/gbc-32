#!/usr/bin/env bash
# Invariance checks for the lazy catch-up core (docs/perf.md, "Invariance
# checks"; ADR 0002, 0011, 0014, 0021 section 3):
#
#   1. Sync placement. A bench built with gbc-core's test-only `eager-sync`
#      feature (every lazy component brought up to the clock at every
#      M-cycle, before the access) must print the same `--fingerprint`
#      lines as the product build, without audio and with
#      `--audio-buffer 7 --hash-samples`.
#   2. Clock offset. A bench built with the test-only `clock-offset`
#      feature, powered on with the clock just below 2^31, 2^32, 2^33 and
#      2^40 (each boundary is crossed during every run), must print the
#      same lines as the product build run from clock 0.
#   3. With --long: `bench --long`, a few workloads for 30 emulated
#      minutes (past 2^32 clocks), must finish without a panic.
#
#   scripts/check-sync-invariance.sh [--long] [--long-only]
#
# The product build is the plain release bench (the main target dir, as
# check-bench.sh builds it). The two feature builds and the long run use
# the `checked` profile (overflow checks and debug assertions on) in their
# own target dir, target/invariance, so they neither thrash the main build
# cache nor leave a feature-built `bench` where a measured one is expected.
# Each build must have compiled gbc-core with exactly its features (none
# for the product and long-run builds), as cargo reports them, or the
# check fails before any run. So must its rustc flags (`bench --rustflags`),
# exactly those of .cargo/config.toml (scripts/lib/rustflags.sh): a
# RUSTFLAGS-style variable replaces them and can turn on a cfg (e.g.
# feature="eager-sync") that cargo does not report as a feature.
# Test ROMs: $TEST_ROMS (default test-roms/). About 4.5 minutes; --long adds
# about 1. Heavy (8 threads): run it alone.
#
# Exit 0 iff every line is identical, apart from the known differences
# listed below (each must still differ: a stale entry fails too).
set -euo pipefail

cd "$(dirname "$0")/.."
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
source scripts/lib/rustflags.sh

# Known sync-placement differences: `<run>|<diagnosis>`, one per line.
# Adding one is a decision, not a fix: say what state depends on when the
# sync happens. An entry whose run no longer differs fails the check.
# None today (the one there was, line 0's single-speed mode-2 pulse posted
# by deadline only, is a timed request now: `irq::PendingIrq`, ADR 0033).
KNOWN_EAGER=()

ROMS_DIR=${TEST_ROMS:-test-roms}
TDIR=target/invariance
# Clock offsets (multiples of 4): below each boundary by less than the
# shortest run (gambatte: 1,572,864 clocks), so every run crosses it; two
# are 4 mod 8 (the APU's 2 MHz phase moves with them).
OFFSETS=(
  $(( (1 << 31) - 400004 ))
  $(( (1 << 32) - 700000 ))
  $(( (1 << 33) - 1000004 ))
  $(( (1 << 40) - 1300000 ))
)

long=0
short=1
for a in "$@"; do
  case "$a" in
    --long) long=1 ;;
    --long-only) long=1; short=0 ;;
    *) echo "usage: $0 [--long] [--long-only]" >&2; exit 2 ;;
  esac
done

[ -d "$ROMS_DIR" ] || { echo "check-sync-invariance: $ROMS_DIR missing (run scripts/fetch-test-roms.sh or set TEST_ROMS)" >&2; exit 2; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
fail=0

# build <tag> <cargo args...>: builds bench, keeps cargo's JSON messages in
# $tmp/<tag>.json, checks the gbc-core features (expect_core_features) and
# its rustc flags (expect_rustflags) and
# prints the path of the binary cargo just built (never a hard-coded,
# possibly stale, path).
build() {
  local tag=$1 want=$2 bin
  shift 2
  cargo build -p bench --quiet --message-format=json "$@" > "$tmp/$tag.json" \
    || { echo "check-sync-invariance: cargo build $* failed" >&2; exit 2; }
  expect_core_features "$tag" "$want"
  bin=$(sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' "$tmp/$tag.json" | tail -n 1)
  [ -n "$bin" ] && [ -x "$bin" ] || { echo "check-sync-invariance: cannot find the bench binary cargo built ($*)" >&2; exit 2; }
  expect_rustflags "$tag" "$bin"
  echo "$bin"
}

# expect_rustflags <tag> <binary>: fails unless build <tag> was compiled
# with exactly the rustc flags of .cargo/config.toml ($GBC_RUSTFLAGS), as
# the binary reports them (tools/bench/build.rs: CARGO_ENCODED_RUSTFLAGS,
# the flags of every unit of the build, gbc-core included). RUSTFLAGS,
# CARGO_ENCODED_RUSTFLAGS, CARGO_BUILD_RUSTFLAGS and
# CARGO_TARGET_<triple>_RUSTFLAGS replace them; with
# RUSTFLAGS='--cfg feature="eager-sync"' the product build would be an
# eager one while cargo reports no feature.
expect_rustflags() {
  local tag=$1 bin=$2 got
  got=$("$bin" --rustflags)
  if [ "$got" != "$GBC_RUSTFLAGS" ]; then
    echo "FAIL: the $tag build has rustc flags '$got', expected exactly '$GBC_RUSTFLAGS' (.cargo/config.toml); unset RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS / CARGO_BUILD_RUSTFLAGS / CARGO_TARGET_*_RUSTFLAGS" >&2
    exit 1
  fi
}

# expect_core_features <tag> <features>: fails unless the one gbc-core
# library cargo compiled for build <tag> had exactly <features> on (a
# space-separated list, "" for none). Read from cargo's compiler-artifact
# message, i.e. what was linked into the binary: bench's own feature
# (`cfg!` in bench) stays on even when its forwarding to gbc-core is
# emptied, and then the "eager" build is a lazy one and the comparison of
# two lazy builds proves nothing.
expect_core_features() {
  local tag=$1 want=$2 art got
  art=$(grep '"reason":"compiler-artifact"' "$tmp/$tag.json" | grep '"name":"gbc_core"' || true)
  if [ "$(printf '%s\n' "$art" | grep -c .)" -ne 1 ]; then
    echo "FAIL: expected one gbc-core artifact in cargo's messages for the $tag build" >&2
    exit 1
  fi
  got=$(printf '%s\n' "$art" | sed -n 's/.*"features":\[\([^]]*\)\].*/\1/p' | tr -d '"' | tr ',' '\n' | sort | tr '\n' ' ')
  want=$(printf '%s\n' $want | sort | tr '\n' ' ')
  if [ "$got" != "$want" ]; then
    echo "FAIL: the $tag build's gbc-core has features [${got% }], expected [${want% }] (tools/bench/Cargo.toml forwarding?)" >&2
    exit 1
  fi
  echo "check-sync-invariance: $tag build: gbc-core features [${got% }]" >&2
}

# fingerprint <binary> <out> [bench --fingerprint args]: a run that dies
# (a panic under the checked profile) or loads fewer ROMs fails the check.
fingerprint() {
  local bin=$1 out=$2
  shift 2
  if ! "$bin" --fingerprint --root "$ROMS_DIR" "$@" > "$out"; then
    echo "FAIL: $bin --fingerprint $* did not finish (a panic is printed above)" >&2
    exit 1
  fi
  if grep -q '^LOAD-ERROR' "$out"; then
    echo "FAIL: $(grep -c '^LOAD-ERROR' "$out") ROMs failed to load ($bin $*)" >&2
    exit 1
  fi
}

# compare <label> <reference> <output> [known entries...]: prints the runs
# whose hash differs. Sets fail=1 for a run that differs and is not known,
# a known run that does not differ, and a run on one side only.
compare() {
  local label=$1 ref=$2 out=$3
  shift 3
  local diffs="$tmp/diffs" n=0 known=0 e p
  awk '
    { h = $1; p = $0; sub(/^[^ ]* /, "", p) }
    NR == FNR { old[p] = h; seen[p] = 0; next }
    { if (!(p in old)) print "ONLY-NEW\t" p
      else { seen[p] = 1; if (old[p] != h) print "DIFF\t" p "\t" old[p] " -> " h } }
    END { for (p in seen) if (!seen[p]) print "ONLY-REF\t" p }
  ' "$ref" "$out" | sort > "$diffs"
  while IFS=$'\t' read -r kind path hashes; do
    if [ "$kind" != DIFF ]; then
      echo "FAIL  $label: $kind $path"
      fail=1
      continue
    fi
    local why=""
    for e in "$@"; do
      [ "${e%%|*}" = "$path" ] && why=${e#*|}
    done
    if [ -n "$why" ]; then
      echo "KNOWN $label: $path  $hashes  ($why)"
      known=$((known + 1))
    else
      echo "DIFF  $label: $path  $hashes"
      n=$((n + 1))
      fail=1
    fi
  done < "$diffs"
  for e in "$@"; do
    p=${e%%|*}
    if ! grep -qF "DIFF	$p	" "$diffs"; then
      echo "STALE $label: $p is listed as a known difference but does not differ: remove the entry"
      fail=1
    fi
  done
  echo "$label: $(wc -l < "$out") runs, $n differ, $known known differences"
}

if [ "$short" = 1 ]; then
  echo "check-sync-invariance: building (release; checked + eager-sync; checked + clock-offset)" >&2
  ref=$(build product "" --release)
  # The two feature builds share a target dir, so each binary is copied
  # before the next build replaces it.
  bin=$(build eager eager-sync --profile checked --features eager-sync --target-dir "$TDIR")
  cp "$bin" "$tmp/bench-eager"
  bin=$(build offset clock-offset --profile checked --features clock-offset --target-dir "$TDIR")
  cp "$bin" "$tmp/bench-offset"

  AUDIO=(--audio-buffer 7 --hash-samples)
  fingerprint "$ref" "$tmp/ref.txt"
  fingerprint "$ref" "$tmp/ref-audio.txt" "${AUDIO[@]}"

  fingerprint "$tmp/bench-eager" "$tmp/eager.txt"
  compare "eager-sync" "$tmp/ref.txt" "$tmp/eager.txt" "${KNOWN_EAGER[@]}"
  fingerprint "$tmp/bench-eager" "$tmp/eager-audio.txt" "${AUDIO[@]}"
  compare "eager-sync, ${AUDIO[*]}" "$tmp/ref-audio.txt" "$tmp/eager-audio.txt" "${KNOWN_EAGER[@]}"

  for off in "${OFFSETS[@]}"; do
    fingerprint "$tmp/bench-offset" "$tmp/offset.txt" "${AUDIO[@]}" --clock-offset "$off"
    compare "clock-offset $off, ${AUDIO[*]}" "$tmp/ref-audio.txt" "$tmp/offset.txt"
  done
fi

if [ "$long" = 1 ]; then
  echo "check-sync-invariance: long run (checked profile, no features)" >&2
  bench=$(build long "" --profile checked --target-dir "$TDIR")
  if ! "$bench" --long --root "$ROMS_DIR"; then
    echo "FAIL  long run"
    fail=1
  fi
fi

if [ "$fail" != 0 ]; then
  echo "check-sync-invariance: FAILED" >&2
  exit 1
fi
echo "check-sync-invariance: ok"
