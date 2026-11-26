#!/usr/bin/env bash
# Stage 2 acceptance gate, first version (docs/plan/stage-2.md "Proposed
# `scripts/gate-stage2.sh`"; Stage 2 M1). Do not weaken this script without
# human approval.
#
#   scripts/gate-stage2.sh               host steps H1-H8, then the board
#                                        steps B1-B5
#   scripts/gate-stage2.sh --host-only   host steps H1-H8 only, for per-commit
#                                        use; never a Stage 2 pass
#   --step H4,H6   (development) H1 plus the listed host steps only. A
#                  partial run is never a pass: it ends as --host-only does
#                  (exit 3) or, without --host-only, at the board steps
#                  (exit 1). H8 reports on the ELFs of the last H4.
#
# Exit status:
#   0  Stage 2 gate passed. NOT REACHABLE in this version: the board steps
#      B1-B5 are not implemented yet (they come with M2-M5), so a run
#      without --host-only always ends with exit 1 after the host steps. A
#      missing board will be exit 1 too, never a pass.
#   1  a check failed
#   2  a tool or runner error (missing tool, unreadable output, a script
#      reporting its own tool error); results are not meaningful
#   3  --host-only (or --step): every host step that ran passed. The run
#      always ends with the line `NOT A STAGE 2 GATE PASS`.
#
# Heavy: H2 is the whole Stage 1 gate (valgrind, fuzz, the 30-minute
# invariance run). Run it alone (AGENTS.md "Machine resources"): every cargo
# command here runs one after the other, under the `jobs` cap of
# .cargo/config.toml, never with -j. GBC_BUILD_LOCK=<file> runs the whole
# gate under `flock -x <file>` (the shared build lock); the lock is not
# passed on, so the build scripts below do not try to take it again.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${ROOT}"

# ---- arguments ------------------------------------------------------------

ALL_STEPS="H1 H2 H3 H4 H5 H6 H7 H8"
host_only=0
steps=""
usage() {
  sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed '$d; s/^# \{0,1\}//' >&2
  exit 2
}
while [ $# -gt 0 ]; do
  case $1 in
    --host-only) host_only=1 ;;
    --step)
      [ $# -ge 2 ] || usage
      steps=$(tr ',' ' ' <<< "$2"); shift ;;
    --step=*) steps=$(tr ',' ' ' <<< "${1#--step=}") ;;
    -h | --help) usage ;;
    *) echo "gate-stage2.sh: unknown argument '$1'" >&2; usage ;;
  esac
  shift
done
partial=0
if [ -n "${steps}" ]; then
  for s in ${steps}; do
    case " ${ALL_STEPS} " in *" $s "*) ;; *) echo "gate-stage2.sh: unknown step '$s' (H1-H8)" >&2; exit 2 ;; esac
  done
  partial=1
  # H1 (the preconditions) always runs, first; the rest in H order.
  steps=$(for s in ${ALL_STEPS}; do case " H1 ${steps} " in *" $s "*) printf '%s ' "$s" ;; esac; done)
  steps=${steps% }
else
  steps=${ALL_STEPS}
fi
want() { case " ${steps} " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }

# ---- the whole run under the build lock, if one is given -------------------

# Only the gate's own re-exec skips taking the lock: it sets
# GATE_STAGE2_LOCKED to the pid of the flock it runs under (the gate's pid,
# kept by exec), so the re-run sees it equal to its parent's pid, and that
# parent is flock. An inherited value from anything else is ignored.
reexec=0
if [ -n "${GATE_STAGE2_LOCKED:-}" ] && [ "${GATE_STAGE2_LOCKED}" = "${PPID}" ] &&
  [ "$(cat "/proc/${PPID}/comm" 2>/dev/null)" = flock ]; then
  reexec=1
fi
unset GATE_STAGE2_LOCKED
if [ -n "${GBC_BUILD_LOCK:-}" ] && [ "${reexec}" = 0 ]; then
  echo "gate-stage2.sh: taking the build lock ${GBC_BUILD_LOCK}"
  args=()
  [ "${host_only}" = 1 ] && args+=(--host-only)
  [ "${partial}" = 1 ] && args+=(--step "$(tr ' ' , <<< "${steps}")")
  exec env GATE_STAGE2_LOCKED=$$ flock -x "${GBC_BUILD_LOCK}" "${ROOT}/scripts/gate-stage2.sh" "${args[@]}"
fi

# ---- reporting and exit ------------------------------------------------------

# Every deliberate exit goes through finish(). Anything else (a command that
# failed under `set -e`, with any status, 3 included) is a tool error, so an
# unexpected exit can never look like a pass or a host-only success.
_finished=0
finish() {
  _finished=1
  [ "${host_only}" = 1 ] || [ "${partial}" = 1 ] && printf '\nNOT A STAGE 2 GATE PASS\n'
  exit "$1"
}
TMP=""
on_exit() {
  local rc=$?
  [ -z "${TMP}" ] || rm -rf "${TMP}"
  [ "${_finished}" = 1 ] && return
  printf '\nGATE ERROR: unexpected exit (status %s) during %s; results are not meaningful\n' "${rc}" "${current:-startup}" >&2
  [ "${host_only}" = 1 ] || [ "${partial}" = 1 ] && printf '\nNOT A STAGE 2 GATE PASS\n'
  exit 2
}
trap on_exit EXIT

current=""
declare -A step_secs=()
step_start=0
step() {
  [ -z "${current}" ] || step_secs[${current}]=$(($(date +%s) - step_start))
  current=$1; step_start=$(date +%s)
  printf '\n==> %s\n' "$*"
}
sub() { printf -- '--- %s\n' "$*"; }
fail() { printf '\nGATE FAILED (%s): %s\n' "${current:-H1}" "$*" >&2; finish 1; }
tool_error() { printf '\nGATE ERROR (%s): %s; results are not meaningful\n' "${current:-H1}" "$*" >&2; finish 2; }
# run_status CMD...: runs CMD; prints nothing; sets $rc (never trips set -e).
run_status() { set +e; "$@"; rc=$?; set -e; }

[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
TMP=$(mktemp -d)
gate_start=$(date +%s)

# The M1 acceptance ROMs (plan Stage 2 M1), name=path under test-roms/.
HEADLESS_ROMS=(cpu_instrs=blargg/cpu_instrs/cpu_instrs.gb cgb-acid2=cgb-acid2/cgb-acid2.gbc)
FIRMWARE_DIRS=(crates/gbc-nucleo-h753 tools/h7-bench tools/headless-qemu)

# =============================================================================
step H1 "Preconditions (Stage 1's, plus the firmware build overrides)"
# =============================================================================

# Refused, as in gate-stage1.sh: each replaces the rustc flags of the cargo
# configs (ADR 0008's LLVM option, the firmware's target-cpu and -Tlink.x).
# Set but empty counts as set. Also refused here, because they change what
# the firmware steps build or where they look for it: the build scripts
# (crates/gbc-nucleo-h753/build.sh, tools/h7-bench/build.sh,
# tools/headless-qemu/run.sh) take their ELFs from cargo's own report
# (scripts/lib/cargo-bin.sh), but a CARGO_TARGET_DIR still moves the
# firmware builds away from the crate's target/ (H8 reads it);
# CARGO_BUILD_* / CARGO_PROFILE_* replace the build target, the jobs cap and the release profile (codegen-units = 1, ADR on sizes);
# RUSTC* and RUSTUP_TOOLCHAIN replace the pinned compiler.
for v in $(compgen -e); do
  case $v in
    RUSTFLAGS | CARGO_ENCODED_RUSTFLAGS | CARGO_TARGET_* | CARGO_BUILD_* | CARGO_PROFILE_* | \
      RUSTC | RUSTC_WRAPPER | RUSTC_WORKSPACE_WRAPPER | RUSTUP_TOOLCHAIN)
      printf '\nGATE FAILED (H1): %s is set (%q); it changes what the gate builds. Unset it and rerun.\n' "$v" "${!v}" >&2
      finish 1
      ;;
  esac
done
# The same settings from a cargo config file outside the repository (cargo
# merges $CARGO_HOME/config.toml and those of every parent directory).
for d in "${CARGO_HOME:-$HOME/.cargo}" $(p=$(dirname "${ROOT}"); while :; do echo "$p/.cargo"; [ "$p" = / ] && break; p=$(dirname "$p"); done); do
  for f in "$d/config" "$d/config.toml"; do
    [ -f "$f" ] || continue
    if grep -nE '^[[:space:]]*([A-Za-z0-9_."-]+\.)?(rustflags|target-dir|build-dir|rustc|rustc-wrapper|rustc-workspace-wrapper|linker|runner|jobs|target)[[:space:]]*=|^[[:space:]]*\[(profile|target|build)' "$f" >&2; then
      fail "$f sets build options (above) that would apply to the gate's builds; move them out and rerun"
    fi
  done
done
# Inside the repository cargo also merges the config of every directory
# from the crate's own up to ROOT (the firmware crates build from their own
# directories, the rest from ROOT). Each such file must be the committed
# one: tracked by git and unchanged from HEAD, so an untracked or edited
# config (say a tools/.cargo/config.toml with build.target-dir) cannot
# change or redirect the builds the gate checks.
for d in . "${FIRMWARE_DIRS[@]}" tools/h7-bench/host; do
  p=$d
  while :; do
    for f in "$p/.cargo/config" "$p/.cargo/config.toml"; do
      f=${f#./}
      [ -e "$f" ] || [ -L "$f" ] || continue
      git ls-files --error-unmatch -- "$f" > /dev/null 2>&1 ||
        fail "$f is not tracked by git; cargo merges it into the builds from $d. Remove it and rerun"
      git diff --quiet HEAD -- "$f" ||
        fail "$f differs from HEAD; cargo merges it into the builds from $d. Commit or revert it and rerun"
    done
    [ "$p" = . ] && break
    p=$(dirname "$p")
  done
done

# Unset, as gate-stage1.sh unsets its overrides: every experiment override
# of the firmware and Stage 1 scripts, so the gate judges the committed
# files and defaults. Every H7_* and GBC_* variable is unset except these,
# which only say where the board is or which lock to take:
#   H7_DRIVE, H7_LABEL, H7_COM   board drive and serial port (flash.sh,
#                                read-serial.sh; board steps)
#   GBC_BOARD_LOCK, GBC_BOARD_LOCK_WAIT
#                                scripts/lib/board-lock.sh
#   GBC_BUILD_LOCK               consumed above (the whole run's lock)
# GBC_BOARD_LOCK_HELD is unset too: an inherited `1` would make every board
# step skip the board lock (board-lock.sh's nested-call shortcut).
# Unset among others: GBC_TEST_ROMS, GBC_ITCM_DROP, GBC_HEADLESS_ROM,
# GBC_HEADLESS_INPUT, GBC_HEADLESS_FRAMES, H7_BENCH_ITCM_DROP,
# H7_BENCH_CPU_INSTRS (set again below), H7_BENCH_BOARD, H7_BENCH_PLL,
# H7_BENCH_QEMU (H6 sets it), H7_BENCH_LOCK (the build scripts' own
# per-command lock: the gate holds GBC_BUILD_LOCK for the whole run
# instead), GBC_FRAME_LOG and the other desktop variables. Plus the
# overrides without those prefixes: TEST_ROMS, CODE_SIZE_BUDGETS_FILE,
# ITCM_SET_FILE (gate-stage1.sh's), HEADLESS_EXPECTED, HEADLESS_QEMU_TIMEOUT,
# QEMU (tools/headless-qemu/run.sh), RUN_HEADLESS_CAPTURE,
# RUN_HEADLESS_READ_SECS (crates/gbc-nucleo-h753/run-headless.sh).
# ARM_OBJDUMP (a tool path for the report-only H8) is left alone.
unset_list=()
for v in $(compgen -e); do
  case $v in
    H7_DRIVE | H7_LABEL | H7_COM | GBC_BOARD_LOCK | GBC_BOARD_LOCK_WAIT) ;;
    H7_* | GBC_* | TEST_ROMS | CODE_SIZE_BUDGETS_FILE | ITCM_SET_FILE | HEADLESS_* | QEMU | RUN_HEADLESS_*)
      unset_list+=("$v") ;;
  esac
done
for v in "${unset_list[@]}"; do unset "$v"; done
[ ${#unset_list[@]} -eq 0 ] || echo "unset: ${unset_list[*]}"
# Tests that need ROMs or test data fail instead of skipping.
export GBC_REQUIRE_TEST_DATA=1
# h7-bench's build.rs and host/ default to an absolute path in the main
# checkout; the gate uses the ROM it fetched into this tree.
export H7_BENCH_CPU_INSTRS="${ROOT}/test-roms/blargg/cpu_instrs/cpu_instrs.gb"

command -v cargo >/dev/null || fail "cargo not found (Rust toolchain not installed)"
for t in qemu-system-arm readelf elixir flock git cmp diff; do
  command -v "$t" >/dev/null || tool_error "$t not found"
done
[[ -f Cargo.toml ]] || fail "no Cargo workspace"
for d in "${FIRMWARE_DIRS[@]}" tools/h7-bench/host; do
  [ -f "$d/Cargo.toml" ] || fail "$d/Cargo.toml missing"
done

# The ROMs for every step (H2 fetches them again, a no-op).
./scripts/fetch-test-roms.sh || tool_error "could not fetch the test ROMs"
for entry in "${HEADLESS_ROMS[@]}"; do
  [ -f "test-roms/${entry#*=}" ] || tool_error "test-roms/${entry#*=} missing after fetch-test-roms.sh"
done
CPU_INSTRS="${ROOT}/test-roms/blargg/cpu_instrs/cpu_instrs.gb"

# =============================================================================
if want H2; then
step H2 "Stage 1 gate (scripts/gate-stage1.sh, unchanged and in full)"
# =============================================================================
# How Stage 1 keeps guarding the core (plan "How Stage 1's gates keep
# running"): fmt, clippy, rustdoc, cargo test (incl. gbc-firmware,
# memory-x, headless), no-alloc and hard rules, thumbv7em build, code
# size, ITCM set, Ir benchmark, fuzz, invariance --long, desktop headless,
# romtest must + regressions.
run_status ./scripts/gate-stage1.sh
[ "${rc}" -eq 0 ] || fail "scripts/gate-stage1.sh exited ${rc}"
fi

# =============================================================================
if want H3; then
step H3 "fmt --check and clippy -D warnings for the firmware workspaces (Stage 1 C-T6)"
# =============================================================================
# These are separate workspaces (own Cargo.lock, thumbv7em target), which
# the root `cargo fmt --all` and `cargo clippy --workspace` of H2 do not
# reach.
for d in crates/gbc-nucleo-h753 tools/h7-bench tools/h7-bench/host tools/headless-qemu; do
  sub "cargo fmt --all --check: $d"
  (cd "$d" && cargo fmt --all --check) || fail "formatting in $d"
done
clippy() { # clippy <dir> <description> [env assignments...] -- <cargo args...>
  local d=$1 what=$2; shift 2
  local envs=()
  while [ "$1" != -- ]; do envs+=("$1"); shift; done; shift
  sub "cargo clippy: $d ($what)"
  (cd "$d" && env "${envs[@]}" cargo clippy --release "$@" -- -D warnings) || fail "clippy in $d ($what)"
}
# gbc-nucleo-h753: the lib and `boot`; then the `headless` feature (the
# `headless` bin, the ITCM placement; its build.rs needs a ROM).
clippy crates/gbc-nucleo-h753 "no features" -- --all-targets
clippy crates/gbc-nucleo-h753 "--features headless" GBC_HEADLESS_ROM="${CPU_INSTRS}" -- --all-targets --features headless
# h7-bench: every feature set tools/h7-bench/build.sh builds. Not
# --all-targets: the bin has a test target, and thumbv7em has no `test`
# crate.
for f in board board,selftest board,clockcheck board,dtcm board,itcm qemu qemu,itcm; do
  clippy tools/h7-bench "--features $f" -- --no-default-features --features "$f"
done
clippy tools/h7-bench/host "host" -- --all-targets
clippy tools/headless-qemu "headless-qemu" GBC_HEADLESS_ROM="${CPU_INSTRS}" -- --all-targets
fi

# =============================================================================
if want H4; then
step H4 "Firmware images: rustflags copies, lockfile pins, release builds and their checks"
# =============================================================================

# (a) ADR 0036 "Consequences": the firmware configs repeat the root's rustc
# flags (their target rustflags replace the root's [build] rustflags), so
# each copy must contain the root's ADR 0008 LLVM option, with the root's
# value. scripts/lib/rustflags.sh checks the root copy and exits 1 without
# it. Every `rustflags` key of every tracked .cargo/config.toml is checked,
# and each firmware config must have one.
sub "rustflags copies contain the root's LLVM option"
# shellcheck source=lib/rustflags.sh
. scripts/lib/rustflags.sh
llvm_flag=""
for w in ${GBC_RUSTFLAGS}; do
  case $w in llvm-args=-cgp-max-address-users-to-scan=*) llvm_flag=$w ;; esac
done
[ -n "${llvm_flag}" ] || fail "no llvm-args=-cgp-max-address-users-to-scan= in the root rustflags (${GBC_RUSTFLAGS})"
echo "  root: -C ${llvm_flag}"
# rustflags_values <config>: one line per `rustflags` key (any table, dotted
# keys included), the value's text with comments dropped (the flags hold no
# `#`), a multi-line array joined.
rustflags_values() {
  awk '
    { sub(/#.*/, "") }
    !open && /^[[:space:]]*([A-Za-z0-9_."-]+\.)?rustflags[[:space:]]*=/ {
      sub(/^[^=]*=/, ""); buf = $0; open = 1
      if (buf !~ /\[/ || buf ~ /\]/) { print buf; open = 0 }
      next
    }
    open { buf = buf " " $0; if ($0 ~ /\]/) { print buf; open = 0 } }
    END { if (open) print "UNTERMINATED" }
  ' "$1"
}
# has_flag <value>: the value holds "-C" followed by the flag, or "-C<flag>".
has_flag() {
  local toks=() t prev=""
  if [[ $1 == *"["* ]]; then
    mapfile -t toks < <(grep -oE "\"[^\"]*\"|'[^']*'" <<< "$1" | sed "s/^[\"']//; s/[\"']\$//")
  else
    read -r -a toks <<< "$(tr -d "\"'" <<< "$1")"
  fi
  for t in "${toks[@]}"; do
    if { [ "$prev" = -C ] && [ "$t" = "${llvm_flag}" ]; } || [ "$t" = "-C${llvm_flag}" ]; then return 0; fi
    prev=$t
  done
  return 1
}
mapfile -t configs < <(git ls-files -- '*.cargo/config.toml' '*.cargo/config' '.cargo/config.toml' | sort -u)
[ ${#configs[@]} -gt 0 ] || tool_error "git ls-files found no .cargo/config.toml"
for d in "${FIRMWARE_DIRS[@]}"; do
  printf '%s\n' "${configs[@]}" | grep -qxF "$d/.cargo/config.toml" || fail "$d/.cargo/config.toml is missing or untracked"
  [ -n "$(rustflags_values "$d/.cargo/config.toml")" ] || fail "$d/.cargo/config.toml has no rustflags (its builds would lack the firmware's flags)"
done
for c in "${configs[@]}"; do
  n=0
  while IFS= read -r val; do
    n=$((n + 1))
    [ "$val" != UNTERMINATED ] || fail "$c: unterminated rustflags array"
    has_flag "$val" || fail "$c: rustflags = ${val# } lack \"-C\", \"${llvm_flag}\" (the root's ADR 0008 option, ADR 0036 Consequences)"
  done < <(rustflags_values "$c")
  echo "  $c: ${n} rustflags key(s)$([ "$n" -eq 0 ] || echo ", each with the flag")"
done

# (b) ADR 0036 "Consequences": the firmware workspaces' lockfiles resolve
# cortex-m, cortex-m-rt and stm32h7 to the same versions. Reference: the
# gbc-nucleo-h753 lock (its Cargo.toml pins them with `=`). Every tracked
# Cargo.lock that has one of them must have exactly that one version, and
# the firmware locks must have the ones they build with.
sub "cortex-m, cortex-m-rt, stm32h7 pinned alike in every firmware Cargo.lock"
lock_versions() { # lock_versions <Cargo.lock> <package>: its versions, sorted, space-separated
  awk -v p="$2" '
    /^\[\[package\]\]/ { n = "" }
    /^name = /    { n = $3; gsub(/"/, "", n) }
    /^version = / { v = $3; gsub(/"/, "", v); if (n == p) print v }
  ' "$1" | sort -u | tr '\n' ' ' | sed 's/ $//'
}
REF_LOCK=crates/gbc-nucleo-h753/Cargo.lock
declare -A ref_ver=()
for p in cortex-m cortex-m-rt stm32h7; do
  ref_ver[$p]=$(lock_versions "${REF_LOCK}" "$p")
  case ${ref_ver[$p]} in
    "" | *" "*) fail "${REF_LOCK}: $p resolves to '${ref_ver[$p]}', not exactly one version" ;;
  esac
  echo "  ${REF_LOCK}: $p ${ref_ver[$p]}"
done
# required presence: <lockfile>=<packages>
for entry in "tools/h7-bench/Cargo.lock=cortex-m cortex-m-rt stm32h7" "tools/headless-qemu/Cargo.lock=cortex-m cortex-m-rt"; do
  f=${entry%%=*}
  [ -f "$f" ] || fail "$f missing"
  for p in ${entry#*=}; do
    [ -n "$(lock_versions "$f" "$p")" ] || fail "$f does not resolve $p (it builds with it)"
  done
done
mapfile -t locks < <(git ls-files -- '*Cargo.lock' | sort -u)
for f in "${locks[@]}"; do
  for p in cortex-m cortex-m-rt stm32h7; do
    got=$(lock_versions "$f" "$p")
    [ -z "$got" ] || [ "$got" = "${ref_ver[$p]}" ] ||
      fail "$f resolves $p to ${got}, ${REF_LOCK} to ${ref_ver[$p]} (ADR 0036: pin them alike)"
  done
done
echo "  ${#locks[@]} tracked lockfiles agree"
lock_sums() { git ls-files -z -- '*Cargo.lock' | xargs -0 sha256sum; }
lock_sums > "${TMP}/locks.before"

# (c) The release builds of every firmware image, with the checks of their
# build.sh: vector table (initial SP, reset vector), flash fit, the lib's
# handlers, the startup symbols' region and order, the fault stack, and for
# the ITCM images scripts/check-itcm-elf.sh (the post-link ITCM check). The
# h7-bench script also checks the host hashes (H5 does it again, directly).
# The QEMU images are built in H6.
sub "crates/gbc-nucleo-h753/build.sh (boot, headless-cpu_instrs, headless-cgb-acid2)"
run_status crates/gbc-nucleo-h753/build.sh
[ "${rc}" -eq 0 ] || fail "crates/gbc-nucleo-h753/build.sh exited ${rc}"
sub "tools/h7-bench/build.sh (selftest, clockcheck, three placements)"
run_status tools/h7-bench/build.sh
[ "${rc}" -eq 0 ] || fail "tools/h7-bench/build.sh exited ${rc}"
for img in crates/gbc-nucleo-h753/out/boot crates/gbc-nucleo-h753/out/headless-cpu_instrs \
  crates/gbc-nucleo-h753/out/headless-cgb-acid2 tools/h7-bench/out/h753/selftest-pll480 \
  tools/h7-bench/out/h753/clockcheck-pll480 tools/h7-bench/out/h753/bench-flash-axi-pll480 \
  tools/h7-bench/out/h753/bench-flash-dtcm-pll480 tools/h7-bench/out/h753/bench-itcm-dtcm-pll480; do
  { [ -s "$img.elf" ] && [ -s "$img.bin" ] && [ "$img.elf" -nt "${TMP}/locks.before" ]; } ||
    fail "$img.{elf,bin} missing or not rebuilt by this run"
done
lock_sums > "${TMP}/locks.after"
cmp -s "${TMP}/locks.before" "${TMP}/locks.after" ||
  fail "the builds changed a Cargo.lock: $(diff "${TMP}/locks.before" "${TMP}/locks.after" | tr '\n' ' ')"
fi

# =============================================================================
if want H5; then
step H5 "Host hash lists equal the committed lists"
# =============================================================================
# The plan's H5 compares the replay set's host hash lists (the replay list
# of acceptance/stage2.toml). That manifest does not exist yet: the human
# creates it (plan D10). Until then the set is the two M1 acceptance ROMs,
# whose committed board targets are tools/headless/expected/*.txt.

sub "tools/h7-bench/host: report equals tools/h7-bench/expected-hashes.txt"
set +e
(cd tools/h7-bench/host && cargo run --release --quiet) > "${TMP}/h7-host.txt"
rc=$?
set -e
case ${rc} in
  0) ;;
  2) tool_error "h7-bench-host exited 2 (could not compute its report)" ;;
  *) fail "h7-bench-host build or run failed (exit ${rc})" ;;
esac
[ -s "${TMP}/h7-host.txt" ] || fail "h7-bench-host printed nothing"
diff -u tools/h7-bench/expected-hashes.txt "${TMP}/h7-host.txt" ||
  fail "h7-bench host hashes differ from tools/h7-bench/expected-hashes.txt"
echo "  equal: $(wc -l < "${TMP}/h7-host.txt") lines"

sub "tools/headless: regenerated 600-frame lines equal tools/headless/expected/"
headless=$(cargo build -p headless --release --bin headless --message-format=json-render-diagnostics |
  sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1) || fail "headless build"
[ -n "${headless}" ] && [ -x "${headless}" ] || fail "cannot find the headless binary cargo built"
for entry in "${HEADLESS_ROMS[@]}"; do
  rom=${entry%%=*} want_f=tools/headless/expected/${rom}.txt
  [ -f "${want_f}" ] || fail "${want_f} missing"
  run_status "${headless}" "test-roms/${entry#*=}" --frames 600 > "${TMP}/${rom}.out"
  [ "${rc}" -eq 0 ] || fail "headless ${rom} --frames 600 exited ${rc}"
  tr -d '\r' < "${TMP}/${rom}.out" | grep '^headless ' > "${TMP}/${rom}.got" || true
  frames=$(grep -c '^headless frame=' "${TMP}/${rom}.got" || true)
  [ "${frames}" -eq 600 ] || fail "headless ${rom}: ${frames} frame lines, not 600"
  # The whole expected file, byte for byte (not just its `headless ` lines).
  if ! cmp -s "${want_f}" "${TMP}/${rom}.got"; then
    diff "${want_f}" "${TMP}/${rom}.got" | head -n 6 || true
    fail "headless ${rom}: lines differ from ${want_f}"
  fi
  echo "  ${rom}: equal, $(wc -l < "${TMP}/${rom}.got") lines"
done
fi

# =============================================================================
if want H6; then
step H6 "QEMU smoke runs: the headless loop, and h7-bench's ITCM stand-in"
# =============================================================================
sub "tools/headless-qemu/run.sh cpu_instrs cgb-acid2 (against tools/headless/expected/)"
set +e
tools/headless-qemu/run.sh cpu_instrs cgb-acid2 | tee "${TMP}/qemu-headless.log"
rc=${PIPESTATUS[0]}
set -e
case ${rc} in
  0) ;;
  1) fail "tools/headless-qemu/run.sh: QEMU lines differ from tools/headless/expected/" ;;
  *) tool_error "tools/headless-qemu/run.sh exited ${rc}" ;;
esac
n=$(grep -c '^  EQUAL: ' "${TMP}/qemu-headless.log" || true)
[ "${n}" -eq 2 ] || fail "tools/headless-qemu/run.sh: ${n} ROMs reported EQUAL, not 2"

sub "H7_BENCH_QEMU=1 tools/h7-bench/build.sh (qemu and qemu-itcm against expected-hashes.txt)"
set +e
H7_BENCH_QEMU=1 tools/h7-bench/build.sh 2>&1 | tee "${TMP}/qemu-h7.log"
rc=${PIPESTATUS[0]}
set -e
[ "${rc}" -eq 0 ] || fail "H7_BENCH_QEMU=1 tools/h7-bench/build.sh exited ${rc}"
n=$(grep -c '^  QEMU lines match expected-hashes.txt$' "${TMP}/qemu-h7.log" || true)
[ "${n}" -eq 2 ] || fail "h7-bench: ${n} QEMU runs matched expected-hashes.txt, not 2 (qemu, qemu-itcm)"
grep -qE '^  qemu-itcm: [1-9][0-9]* long-branch thunks$' "${TMP}/qemu-h7.log" ||
  fail "h7-bench qemu-itcm: no long-branch thunk line, so the stand-in ITCM placement was not exercised"
fi

# =============================================================================
if want H7; then
step H7 "Host unit tests of the firmware logic, named"
# =============================================================================
# H2's `cargo test --workspace` runs these too; naming them makes deleting,
# renaming or #[ignore]-ing one fail the gate. Each must be reported `ok`
# by `cargo test -p <crate>`. The plan's list (converter, DC blocker,
# debounce, serial framing, image header; gbc-link, tools/gbc-pc) is added
# as those crates arrive (M2-M5).
sub "scripts/test-board-lock.sh (scripts/lib/board-lock.sh: serializes, re-enters, times out)"
run_status scripts/test-board-lock.sh
[ "${rc}" -eq 0 ] || fail "scripts/test-board-lock.sh exited ${rc}"
NAMED_TESTS="
gbc-firmware fnv::tests::known_vectors
gbc-firmware fnv::tests::frame_is_little_endian_pixel_bytes
gbc-firmware fnv::tests::samples_are_left_then_right_little_endian
gbc-firmware headless::tests::error_lines_format
gbc-firmware headless::tests::stats_lines_format
gbc-firmware input::tests::held_does_not_overflow_near_u32_max
gbc-firmware input::tests::held_is_the_union_of_covering_presses
gbc-firmware input::tests::parses_the_program_format
gbc-firmware input::tests::rejects_bad_lines_with_their_line_number
gbc-firmware input::tests::script_hash_covers_every_field
gbc-firmware input::tests::the_committed_ladx_program_parses
gbc-firmware allows_test_modules_comments_strings_and_core_alloc
gbc-firmware catches_extern_crate_however_written
gbc-firmware catches_path_and_include
gbc-firmware catches_std_and_alloc_paths
gbc-firmware catches_unsafe_anywhere
gbc-firmware crate_attributes_are_found
gbc-firmware dependency_tables_are_found
gbc-firmware escaped_quote_char_literals_lex_as_one_token
gbc-firmware firmware_depends_on_gbc_core_only
gbc-firmware firmware_has_no_build_script_or_lib_path
gbc-firmware firmware_sources_follow_the_rules
gbc-firmware leading_inner_attributes_only
gbc-firmware manifest_escape_hatches_are_found
gbc-firmware cpu_instrs_is_deterministic
gbc-firmware errors_print_one_line_after_the_header
gbc-firmware hashes_match_the_buffers
gbc-firmware input_changes_exactly_the_pressed_frames
gbc-firmware line_format
gbc-firmware synthetic_run_is_deterministic
memory-x tests::comments_budget_and_flash_ok_are_not_globs
memory-x tests::drop_list_trims_and_skips_empty_entries
memory-x tests::drops_a_tier_until_the_next_tier_header
memory-x tests::dtcm_rodata_holds_gbc_core_constant_pools
memory-x tests::fault_stack_follows_the_axi_buffers
memory-x tests::firmware_map_always_defines_the_lib_symbols
memory-x tests::firmware_map_without_a_set_has_no_itcm_section
memory-x tests::itcm_section_holds_the_rules_and_the_asserts
memory-x tests::keeps_every_glob_without_drops
memory-x tests::memory_maps_and_placements
memory-x tests::odd_glob_panics
memory-x tests::the_real_set_parses_with_the_default_drop
memory-x tests::too_few_globs_panics
memory-x tests::unmatched_drop_panics
headless cgb_acid2_matches_expected
headless cpu_instrs_matches_expected
headless exit_codes
"
for crate in $(awk 'NF { print $1 }' <<< "${NAMED_TESTS}" | uniq); do
  sub "cargo test -p ${crate}"
  set +e
  cargo test -p "${crate}" > "${TMP}/test-${crate}.log" 2>&1
  rc=$?
  set -e
  [ "${rc}" -eq 0 ] || { tail -n 30 "${TMP}/test-${crate}.log"; fail "cargo test -p ${crate} exited ${rc}"; }
  missing=0 count=0
  while read -r c name; do
    [ "$c" = "${crate}" ] || continue
    count=$((count + 1))
    # Exactly once, passed (an #[ignore]d test reports `ignored`).
    hits=$(grep -cE "^test ${name//./\\.}( - should panic)? \.\.\. ok\$" "${TMP}/test-${crate}.log" || true)
    if [ "${hits}" -ne 1 ]; then
      echo "  ${crate}: '${name}' reported ok ${hits} times, not once" >&2
      missing=$((missing + 1))
    fi
  done <<< "${NAMED_TESTS}"
  [ "${missing}" -eq 0 ] || fail "${crate}: ${missing} named test(s) not run and passed (list in H7 of this script)"
  echo "  ${crate}: ${count} named tests ok"
done
fi

# =============================================================================
if want H8; then
step H8 "Firmware size and memory use (report only)"
# =============================================================================
# region_use <elf>: allocated section bytes per memory region.
region_use() {
  readelf -SW "$1" | sed -n 's/^ *\[ *[0-9]*\] *//p' | awk -v elf="$1" '
    $1 ~ /^\./ && $7 ~ /A/ {
      a = strtonum("0x" $3); s = strtonum("0x" $5)
      if (a < 0x10000) itcm += s
      else if (a >= 0x08000000 && a < 0x08200000) flash += s
      else if (a >= 0x20000000 && a < 0x20020000) dtcm += s
      else if (a >= 0x24000000 && a < 0x24080000) axi += s
      else other += s
    }
    END {
      printf "  %s\n", elf
      printf "    ITCM  %7d of  65536 B (.itcm)\n", itcm
      printf "    DTCM  %7d of 131072 B (sections; the stack region is the rest, see build.sh)\n", dtcm
      printf "    AXI   %7d of 524288 B\n", axi
      printf "    flash %7d B of sections (load images of the RAM sections not counted)\n", flash
      if (other) printf "    other %7d B\n", other
    }'
}
for elf in crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf crates/gbc-nucleo-h753/out/headless-cgb-acid2.elf \
  tools/h7-bench/out/h753/bench-itcm-dtcm-pll480.elf; do
  [ -f "${elf}" ] || tool_error "${elf} missing (built by H4)"
  region_use "${elf}"
done
echo "  stack paint depth: n/a (needs a board run; the board steps B1-B5 are not implemented)"
sub "scripts/report-firmware-size.exs crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf"
# The rlib the image was linked from: the newest gbc-core rlib of that
# target directory (cargo keeps it under release/build/gbc-core/<hash>/out/).
rlib=$(find crates/gbc-nucleo-h753/target/thumbv7em-none-eabihf/release -name 'libgbc_core-*.rlib' -printf '%T@ %p\n' 2>/dev/null |
  sort -rn | sed -n '1s/^[^ ]* //p' || true)
[ -n "${rlib}" ] || tool_error "no gbc-core rlib under crates/gbc-nucleo-h753/target (built by H4)"
run_status elixir scripts/report-firmware-size.exs crates/gbc-nucleo-h753/out/headless-cpu_instrs.elf --rlib "${rlib}"
[ "${rc}" -eq 0 ] || tool_error "scripts/report-firmware-size.exs exited ${rc}"
fi

# =============================================================================
step_secs[${current}]=$(($(date +%s) - step_start))
printf '\n==> Summary (%s s in all)\n' "$(($(date +%s) - gate_start))"
for s in ${steps}; do printf '  %s OK  %6s s\n' "$s" "${step_secs[$s]:-?}"; done

if [ "${host_only}" = 1 ]; then
  if [ "${partial}" = 1 ]; then
    printf '\nHOST STEPS PASSED (partial run: %s)\n' "${steps}"
  else
    printf '\nHOST STEPS H1-H8 PASSED\n'
  fi
  finish 3
fi
[ "${partial}" = 1 ] && printf '\nHOST STEPS PASSED (partial run: %s); a partial run is never a pass\n' "${steps}"
# Board steps B1-B5 (plan "Proposed scripts/gate-stage2.sh"): the selftest
# image's clock and registers, the bench hashes and cycles, the replay set
# in the player image, the soak and the save round trip. They need the
# images of M2-M5 and are serialized under the board lock
# (scripts/lib/board-lock.sh). Until they exist, the gate cannot pass.
printf '\nGATE FAILED (B1-B5): board steps not implemented (B1-B5); this version of the gate cannot pass\n' >&2
finish 1
