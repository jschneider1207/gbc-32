#!/usr/bin/env bash
# Stage 2 acceptance gate, first version (docs/plan/stage-2.md "Proposed
# `scripts/gate-stage2.sh`"; Stage 2 M1). Do not weaken this script without
# human approval.
#
#   scripts/gate-stage2.sh               host steps H1-H8, then the board
#                                        steps B1-B5
#   scripts/gate-stage2.sh --host-only   host steps H1-H8 only, for per-commit
#                                        use; never a Stage 2 pass
#   --step H4,B2   (development) H1 plus the listed steps only (host steps
#                  H1-H8, board steps B1-B5; with --host-only, host steps
#                  only). A partial run is never a pass: when every step
#                  that ran passed it ends with exit 3, as --host-only does.
#                  H8 reports on the ELFs of the last H4.
#
# Board steps (plan "Proposed scripts/gate-stage2.sh"; the board is the
# NUCLEO-H753ZI on H7_DRIVE / H7_COM, default E: / COM5). The images they
# flash are built by this run (H4, or the board steps' own build first);
# then the whole board phase holds the board lock
# (scripts/lib/board-lock.sh), and the board must be there (the ST-LINK
# drive with its label and DETAILS.TXT, and the COM port): a missing board
# is exit 1, never a pass. Every flash is followed by a serial read whose
# checker ignores the lines before the flashed image's own output (its
# banner's image cksum, or its fresh run):
#   B1  selftest-pll480: tools/h7-bench/check-selftest.sh (clock=pll472,
#       cpu_hz, VOS0/ODEN, actvosrdy, flash_acr, constant cycles=, tcmprobe)
#   B2  bench-itcm-dtcm-pll480 with the bench pack in the NOR
#       (tools/h7-bench/run-qspi.sh): internal-flash and QSPI hashes equal
#       expected-hashes.txt (check-report.sh); check-cycles.sh --fresh:
#       every line <= expected-cycles.txt + 3% (two agreeing reports),
#       worst-case and cpu_instrs max < 7,902,557 (real time: the two
#       sign-off workloads the bench runs; the other replay ROMs get their
#       real-time bound from B3, over_budget=0 in the player image); the
#       worst-case max against 3,951,279 is a REPORT line, not a check
#   B3  scripts/check-stage2-sets.sh (the run lists equal
#       acceptance/stage2.toml); the display readback self-test
#       (run-headless.sh, headless-display-cgb-acid2: 0 mismatches); the
#       replay set in the player-script image (run-replay.sh replay-set.txt:
#       paced lines equal the host's, 0 underruns, 0 frames over budget),
#       every entry PASS. Loads each replay ROM into the QSPI NOR.
#   B4  the 10-minute synth:worst-case soak (run-replay.sh soak-set.txt):
#       as B3, plus |ppm| <= 1000 of the board's cycle counter
#   B5  the save round trip on the player image (run-player-save.sh, the
#       manifest's fixture). Leaves `player` flashed, link-probe in the NOR.
# The stack paint depth of the board runs is printed at the end (report).
#
# Exit status:
#   0  Stage 2 gate passed: every host and board step, in one full run.
#   1  a check failed (also: the board is not attached)
#   2  a tool or runner error (missing tool, unreadable output, a script
#      reporting its own tool error, serial unreadable, flash refused, a
#      malformed line); results are not meaningful
#   3  --host-only (or --step): every step that ran passed. The run
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

HOST_STEPS="H1 H2 H3 H4 H5 H6 H7 H8"
BOARD_STEPS="B1 B2 B3 B4 B5"
ALL_STEPS="${HOST_STEPS} ${BOARD_STEPS}"
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
    case " ${ALL_STEPS} " in *" $s "*) ;; *) echo "gate-stage2.sh: unknown step '$s' (H1-H8, B1-B5)" >&2; exit 2 ;; esac
    if [ "${host_only}" = 1 ]; then
      case " ${BOARD_STEPS} " in *" $s "*) echo "gate-stage2.sh: --host-only runs no board step ('$s')" >&2; exit 2 ;; esac
    fi
  done
  partial=1
  # H1 (the preconditions) always runs, first; the rest in H, then B order.
  steps=$(for s in ${ALL_STEPS}; do case " H1 ${steps} " in *" $s "*) printf '%s ' "$s" ;; esac; done)
  steps=${steps% }
elif [ "${host_only}" = 1 ]; then
  steps=${HOST_STEPS}
else
  steps=${ALL_STEPS}
fi
want() { case " ${steps} " in *" $1 "*) return 0 ;; *) return 1 ;; esac; }
board_wanted=0
for s in ${BOARD_STEPS}; do want "$s" && board_wanted=1; done

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
# The images the board steps flash must be newer than this (built by this run).
: > "${TMP}/gate-start"
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
# Refused too: the shell settings every child script inherits. Each step
# runs bash scripts, which are non-interactive shells: they source
# $BASH_ENV (ENV in sh/posix mode) before their first line, after the
# unsets below, so such a file could set RUN_PACED_CAPTURE and the like
# again and replace the board with a stored capture. SHELLOPTS and BASHOPTS
# would turn shell options on in every child (noglob, ...; bash 5 refuses
# to import SHELLOPTS, refused here all the same), CDPATH
# redirects their `cd DIR`, GLOBIGNORE drops files from their globs. An
# exported shell function replaces the command of that name (cargo, cmp,
# diff, ...) in every child bash.
for v in BASH_ENV ENV SHELLOPTS BASHOPTS CDPATH GLOBIGNORE; do
  if printenv "$v" > /dev/null; then
    printf '\nGATE FAILED (H1): %s is set in the environment (%q); child scripts would inherit it. Unset it and rerun.\n' "$v" "$(printenv "$v")" >&2
    finish 1
  fi
done
exported_fns=$(declare -Fx | sed 's/^declare -f[a-z]* //')
if [ -n "${exported_fns}" ]; then
  printf '\nGATE FAILED (H1): exported shell functions (%s) would replace commands in the child scripts. Unset them (unset -f) and rerun.\n' "$(tr '\n' ' ' <<< "${exported_fns}")" >&2
  finish 1
fi
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
# QEMU (tools/headless-qemu/run.sh), and every RUN_* and PLAY_* variable:
# the board run scripts' overrides (RUN_HEADLESS_*, RUN_PACED_*,
# RUN_REPLAY_*, RUN_SAVE_*, PLAY_*: a RUN_REPLAY_CAPTURES or
# RUN_PACED_CAPTURE would replace the board with a saved capture), and
# with CHECK_* the H7 tests' script overrides (RUN_HEADLESS, RUN_PACED,
# RUN_REPLAY, CHECK_REPORT, CHECK_CYCLES, CHECK_ITCM_ELF, CHECK_SELFTEST,
# CHECK_SETS: each would make its test test another script).
# ARM_OBJDUMP (a tool path for the report-only H8) is left alone.
unset_list=()
for v in $(compgen -e); do
  case $v in
    H7_DRIVE | H7_LABEL | H7_COM | GBC_BOARD_LOCK | GBC_BOARD_LOCK_WAIT) ;;
    H7_* | GBC_* | TEST_ROMS | CODE_SIZE_BUDGETS_FILE | ITCM_SET_FILE | HEADLESS_* | QEMU | RUN_* | PLAY_* | CHECK_*)
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
# Three sets of committed host lists, each regenerated here and compared
# byte for byte: h7-bench's expected-hashes.txt; the two M1 acceptance ROMs'
# 600-frame lines (tools/headless/expected/*.txt, the M1 board targets);
# and the plan's "replay set's host hash lists": one file per entry of
# crates/gbc-nucleo-h753/replay-set.txt (B3's check-stage2-sets.sh holds it
# equal to acceptance/stage2.toml's `replay`), at
# tools/headless/expected/replay/<name>.txt. Each is the `paced ` lines of
# exactly the host run run-replay.sh compares the board against (its step
# 2: `headless <rom> --paced --frames <N> [--input <replay-input table>]`;
# run-paced.sh compares the `paced ` lines). A missing list, an extra one,
# or a differing one fails H5. After an intended core change, regenerate
# them with this step's loop and commit them with the reason.

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

sub "replay set: regenerated host paced lines equal tools/headless/expected/replay/"
run_status cargo build -q --release -p replay-input
[ "${rc}" -eq 0 ] || fail "replay-input build failed (exit ${rc})"
replay_input=$(dirname "${headless}")/replay-input
[ -x "${replay_input}" ] || fail "cannot find the replay-input binary next to ${headless}"
REPLAY_SET=crates/gbc-nucleo-h753/replay-set.txt REPLAY_EXP=tools/headless/expected/replay
[ -f "${REPLAY_SET}" ] || fail "${REPLAY_SET} missing"
mkdir "${TMP}/replay"
replay_names=()
# Parsed as run-replay.sh parses it: <rom> <frames> [<stage1 group>], plus
# max_ppm= fields (a speed bound, no effect on the host run); the entry's
# name is the group, else the ROM's file stem.
while read -r -a fld <&3; do
  case "${fld[0]:-}" in '' | '#'*) continue ;; esac
  pos=()
  for x in "${fld[@]}"; do case $x in max_ppm=*) ;; *) pos+=("$x") ;; esac; done
  [ "${#pos[@]}" -ge 2 ] && [ "${#pos[@]}" -le 3 ] || tool_error "${REPLAY_SET}: malformed entry: ${fld[*]}"
  rom_rel=${pos[0]} frames=${pos[1]} group=${pos[2]:-}
  case ${rom_rel} in synth:*) tool_error "${REPLAY_SET}: ${rom_rel}: H5 has no host list for a synthetic entry" ;; esac
  name=$(basename "${rom_rel%.*}"); [ -n "${group}" ] && name=${group}
  case " ${replay_names[*]} " in *" ${name} "*) tool_error "${REPLAY_SET}: entry ${name} twice" ;; esac
  replay_names+=("${name}")
  [ -f "test-roms/${rom_rel}" ] || tool_error "test-roms/${rom_rel} missing"
  input_args=()
  if [ -n "${group}" ]; then
    run_status "${replay_input}" table --manifest acceptance/stage1.toml --group "${group}" > "${TMP}/replay/${name}.clocks"
    [ "${rc}" -eq 0 ] || fail "replay-input table --group ${group} exited ${rc}"
    want_frames=$(sed -n 's/^end \([0-9]*\)$/\1/p' "${TMP}/replay/${name}.clocks")
    [ "${frames}" = - ] && frames=${want_frames}
    [ "${frames}" = "${want_frames}" ] || fail "${name}: frames ${frames} differs from the table's end ${want_frames}"
    input_args=(--input "${TMP}/replay/${name}.clocks")
  fi
  case ${frames} in '' | *[!0-9]*) tool_error "${REPLAY_SET}: ${name}: frames '${frames}' is not a number" ;; esac
  run_status "${headless}" "test-roms/${rom_rel}" --paced --frames "${frames}" "${input_args[@]}" > "${TMP}/replay/${name}.out"
  [ "${rc}" -eq 0 ] || fail "headless ${name} --paced --frames ${frames} exited ${rc}"
  tr -d '\r' < "${TMP}/replay/${name}.out" | grep '^paced ' > "${TMP}/replay/${name}.got" || true
  grep -q "^paced v1 .* frames=${frames}\$" "${TMP}/replay/${name}.got" || fail "headless ${name}: no 'paced v1 ... frames=${frames}' line"
  grep -q '^paced end ' "${TMP}/replay/${name}.got" || fail "headless ${name}: no 'paced end' line"
  want_f=${REPLAY_EXP}/${name}.txt
  [ -f "${want_f}" ] || fail "${want_f} missing (the committed host list of replay entry ${name})"
  if ! cmp -s "${want_f}" "${TMP}/replay/${name}.got"; then
    diff "${want_f}" "${TMP}/replay/${name}.got" | head -n 6 || true
    fail "replay ${name}: host paced lines differ from ${want_f}"
  fi
  echo "  ${name}: equal, $(wc -l < "${want_f}") lines (frames ${frames})"
done 3< "${REPLAY_SET}" < /dev/null
[ "${#replay_names[@]}" -gt 0 ] || fail "${REPLAY_SET} has no entries"
for f in "${REPLAY_EXP}"/*; do
  [ -e "$f" ] || continue
  n=$(basename "$f" .txt)
  case " ${replay_names[*]} " in *" ${n} "*) [ "$f" = "${REPLAY_EXP}/${n}.txt" ] && continue ;; esac
  fail "$f is not the host list of a ${REPLAY_SET} entry"
done
echo "  ${#replay_names[@]} replay entries checked"
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
# by `cargo test -p <crate>`, exactly once. The plan's list: the video
# converter (gbc-firmware video), the DC blocker and DAC conversion
# (gbc-firmware audio), debounce (gbc-firmware buttons), the image header
# (gbc-firmware rom_image), the save footer (gbc-firmware save), serial
# framing (gbc-link COBS, CRC and message framing), and tools/gbc-pc's
# ROM load, input and save/RTC host logic.
sub "scripts/test-board-lock.sh (scripts/lib/board-lock.sh: serializes, re-enters, times out)"
run_status scripts/test-board-lock.sh
[ "${rc}" -eq 0 ] || fail "scripts/test-board-lock.sh exited ${rc}"
# The board run scripts' pass/fail logic, on synthetic and committed
# captures (never the board, never the board lock; human, 2026-11-26).
for t in test-run-headless test-check-report test-check-cycles test-check-itcm-elf test-run-paced test-run-replay \
  test-check-selftest test-check-stage2-sets; do
  sub "scripts/${t}.sh (every failure mode of the script it tests, plus a pass case)"
  run_status "scripts/${t}.sh"
  [ "${rc}" -eq 0 ] || fail "scripts/${t}.sh exited ${rc}"
done
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
gbc-firmware audio::tests::conversion_endpoints_and_rounding
gbc-firmware audio::tests::extreme_steps_do_not_overflow
gbc-firmware audio::tests::full_scale_square_waves_stay_bounded
gbc-firmware audio::tests::negative_step_decays_to_exactly_zero_too
gbc-firmware audio::tests::silence_is_exactly_mid_scale
gbc-firmware audio::tests::small_constants_do_not_stick
gbc-firmware audio::tests::step_response_decays_monotonically_to_zero
gbc-firmware audio::tests::step_response_follows_a_to_the_n
gbc-firmware buttons::tests::a_clean_press_and_release_take_samples_minus_one_periods
gbc-firmware buttons::tests::chord_edge_fires_once_per_hold
gbc-firmware buttons::tests::lines_debounce_independently_and_extra_bits_are_ignored
gbc-firmware buttons::tests::live_input_debounces_merges_and_flags_the_chord_once
gbc-firmware buttons::tests::mapping
gbc-firmware buttons::tests::pc_bitmap_and_merge
gbc-firmware buttons::tests::runs_shorter_than_samples_are_rejected
gbc-firmware rom_image::tests::corrupt_fields
gbc-firmware rom_image::tests::data_bit_flip
gbc-firmware rom_image::tests::erased_is_distinct
gbc-firmware rom_image::tests::flags_and_padding_not_checked
gbc-firmware rom_image::tests::full_window_rom
gbc-firmware rom_image::tests::length_bounds
gbc-firmware rom_image::tests::matches_by_len_and_hash
gbc-firmware rom_image::tests::round_trip
gbc-firmware rom_image::tests::short_window
gbc-firmware rom_image::tests::title_copied_from_rom
gbc-firmware rom_image::tests::title_is_informational_but_checksummed
gbc-firmware video::tests::bands_match_whole_frame
gbc-firmware video::tests::matches_reference_on_random_frames
gbc-firmware video::tests::row_and_column_map_is_floor_two_thirds
gbc-firmware video::tests::white_black_and_single_channels
gbc-firmware bad_footers_are_refused_at_the_field
gbc-firmware footer_fields_decode_as_documented
gbc-firmware link_probe_fixture_round_trips_across_frames
gbc-firmware reload_replaces_ram_and_clock_of_a_running_game
gbc-firmware round_trips_are_byte_identical
gbc-firmware take_drops_the_part_second_and_stamps_huc3_at_the_minute
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
gbc-link cobs::tests::empty_input_encodes_to_one_code_byte
gbc-link cobs::tests::invalid_inputs_are_rejected
gbc-link cobs::tests::small_output_buffer_is_an_error
gbc-link cobs::tests::trailing_empty_block_after_full_block_is_accepted
gbc-link cobs::tests::wikipedia_vectors
gbc-link crc::tests::check_value
gbc-link crc::tests::known_values
gbc-link crc::tests::table_matches_bitwise
gbc-link any_single_bit_flip_is_detected
gbc-link bit_flip_in_body_is_a_crc_error
gbc-link byte_soup_never_panics
gbc-link empty_frames_are_ignored
gbc-link encode_errors
gbc-link every_message_round_trips
gbc-link feed_slice_returns_frames_one_at_a_time
gbc-link frame_of_exactly_the_buffer_size_is_accepted
gbc-link garbage_then_valid_frame_resyncs
gbc-link max_size_messages_fit_max_frame_exactly
gbc-link oversize_frame_is_dropped_then_decoder_recovers
gbc-link save_dump_stream_decodes_in_order
gbc-link short_and_non_cobs_frames
gbc-link unknown_types_are_errors
gbc-link wrong_body_length_is_bad_length
gbc-pc already_loaded_is_skipped
gbc-pc corrupt_reply_is_resent
gbc-pc corrupt_request_is_naked_and_resent
gbc-pc dropped_reply_is_resent_and_deduplicated
gbc-pc faults_on_every_stage
gbc-pc full_round_trip
gbc-pc read_back_hash_mismatch_is_an_error
gbc-pc every_link_bit_is_the_firmware_button_of_the_same_name
gbc-pc input_frames_carry_the_bitmap_unchanged
gbc-pc input_frames_on_change_then_release_at_exit
gbc-pc desktop_to_board_to_desktop_counts_real_time_once
gbc-pc dump_restamp_changes_only_the_stamp
gbc-pc dump_to_file_restamps_unless_raw
gbc-pc load_advances_a_running_clock_by_the_time_since_the_stamp
gbc-pc load_day_overflow_wraps_and_sets_the_carry
gbc-pc load_leaves_a_halted_clock_and_stamps_it_now
gbc-pc load_refuses_what_the_board_refuses_unless_raw
gbc-pc load_syncs_huc3_as_the_desktop
gbc-pc raw_save_is_byte_exact
gbc-pc broken_dumps_are_errors_and_write_no_file
gbc-pc corrupted_save_chunk_on_the_link_is_resent
gbc-pc dump_file_equals_the_boards_save
gbc-pc dump_receiver_checks_order_and_seq
gbc-pc load_then_dump_round_trips_byte_identically
gbc-pc save_failing_the_boards_hash_check_is_an_error_and_not_applied
gbc-pc wrong_size_save_is_refused_and_nothing_applied
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
if [ "${board_wanted}" = 1 ]; then
  echo "  stack paint depth: from this run's board steps, printed after them"
else
  echo "  stack paint depth: n/a (no board step in this run)"
fi
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
if [ "${host_only}" = 1 ]; then
  step_secs[${current}]=$(($(date +%s) - step_start))
  printf '\n==> Summary (%s s in all)\n' "$(($(date +%s) - gate_start))"
  for s in ${steps}; do printf '  %s OK  %6s s\n' "$s" "${step_secs[$s]:-?}"; done
  if [ "${partial}" = 1 ]; then
    printf '\nHOST STEPS PASSED (partial run: %s)\n' "${steps}"
  else
    printf '\nHOST STEPS H1-H8 PASSED\n'
  fi
  finish 3
fi

# =============================================================================
# Board steps B1-B5 (header; plan "Proposed scripts/gate-stage2.sh")
# =============================================================================
FW=crates/gbc-nucleo-h753
H7B=tools/h7-bench
SELFTEST_BIN=${H7B}/out/h753/selftest-pll480.bin
BENCH_BIN=${H7B}/out/h753/bench-itcm-dtcm-pll480.bin
DISPLAY_BIN=${FW}/out/headless-display-cgb-acid2.bin
stack_lines=()

if [ "${board_wanted:-0}" = 1 ]; then
# board_rc <what> <rc>: a board script's exit: 0 on, 1 a check failed, else
# a tool error (the scripts' own contract: 2, or a crash).
board_rc() {
  case $2 in
    0) ;;
    1) fail "$1: a check failed (exit 1)" ;;
    *) tool_error "$1 exited $2" ;;
  esac
}
# fresh_image <file>...: each was built by this run (H4 or the build below).
fresh_image() {
  local f
  for f in "$@"; do
    { [ -s "$f" ] && [ "$f" -nt "${TMP}/gate-start" ]; } || tool_error "$f missing or not built by this run"
  done
}

step BUILD "Builds for the board steps (before the board lock)"
# Everything the run scripts flash or run is built now, from this tree,
# before the board lock. Most of them then build nothing under the lock
# (run-player-save.sh skips its builds under GBC_BOARD_LOCK_HELD), but
# run-replay.sh (B3, B4) still runs cargo build (headless, replay-input,
# gbc-pc), build.sh loader, and per entry build.sh player-script (each
# entry's input and frame count are compiled in) and, for a synth entry,
# bench --write-synth, all while the gate holds the board lock; those
# builds are incremental after this step, except player-script. h7-bench's images come from H4 when it ran; else they are built here.
if want B1 || want B2; then
  if ! want H4; then
    sub "tools/h7-bench/build.sh (selftest, the bench images, the bench pack)"
    run_status ${H7B}/build.sh
    [ "${rc}" -eq 0 ] || fail "tools/h7-bench/build.sh exited ${rc}"
  fi
  want B1 && fresh_image "${SELFTEST_BIN}"
  want B2 && fresh_image "${BENCH_BIN}" "${H7B}/out/h753/bench-pack.bin"
fi
fw_images=()
{ want B2 || want B3 || want B4 || want B5; } && fw_images+=(loader)
want B3 && fw_images+=(headless-display-cgb-acid2)
want B5 && fw_images+=(player link-probe)
if [ ${#fw_images[@]} -gt 0 ]; then
  sub "crates/gbc-nucleo-h753/build.sh ${fw_images[*]}"
  run_status ${FW}/build.sh "${fw_images[@]}"
  [ "${rc}" -eq 0 ] || fail "crates/gbc-nucleo-h753/build.sh ${fw_images[*]} exited ${rc}"
  for i in "${fw_images[@]}"; do
    case $i in link-probe) fresh_image "${FW}/out/link-probe.bin" "${FW}/out/link-probe.gb" ;; *) fresh_image "${FW}/out/$i.bin" ;; esac
  done
  sub "gbc-pc (x86_64-pc-windows-gnu), headless, replay-input, bench"
  cargo build -q -p gbc-pc --release --target x86_64-pc-windows-gnu || fail "building gbc-pc failed"
  cargo build -q --release -p headless -p replay-input -p bench || fail "building headless, replay-input and bench failed"
fi

step LOCK "Board lock and board presence"
command -v cmd.exe > /dev/null || tool_error "cmd.exe not found (the board is reached through Windows interop)"
command -v powershell.exe > /dev/null || tool_error "powershell.exe not found (read-serial.sh needs it)"
# shellcheck source=lib/board-lock.sh
. scripts/lib/board-lock.sh
run_status board_lock_hold
[ "${rc}" -eq 0 ] || tool_error "could not take the board lock (${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock})"
echo "  holding the board lock ${GBC_BOARD_LOCK:-/tmp/gbc-32-board.lock} until the gate exits"
# The board, as flash.sh and read-serial.sh find it: the ST-LINK drive with
# its label and DETAILS.TXT, and the COM port. Missing: exit 1, never a pass.
drive=${H7_DRIVE:-E:} label=${H7_LABEL:-NOD_H753ZI} port=${H7_COM:-COM5}
win() { (cd /mnt/c && cmd.exe /c "$1" 2> /dev/null || true) | tr -d '\r'; }
case "${drive}" in [A-Za-z]:) ;; *) tool_error "H7_DRIVE=${drive} is not a drive letter like E:" ;; esac
case "${port}" in COM[0-9] | COM[0-9][0-9]) ;; *) tool_error "H7_COM=${port} is not a port like COM5" ;; esac
vol=$(win "vol ${drive}")
case "${vol}" in *"${label}"*) ;; *) fail "no board: volume ${drive} is not labelled ${label} (${vol:-no answer}); attach the NUCLEO-H753ZI" ;; esac
case "$(win "if exist ${drive}\\DETAILS.TXT echo yes")" in
  *yes*) ;; *) fail "no board: ${drive} has no DETAILS.TXT (not an ST-LINK drive)" ;;
esac
ports=$(powershell.exe -NoProfile -NonInteractive -Command "[System.IO.Ports.SerialPort]::GetPortNames()" 2> /dev/null | tr -d '\r' || true)
grep -qx "${port}" <<< "${ports}" || fail "no board: serial port ${port} is not present (ports: $(tr '\n' ' ' <<< "${ports}"))"
echo "  board: ${drive} (${label}), ${port}"

# sets_check: the run lists equal acceptance/stage2.toml; sets SAVE_FIXTURE.
sets_check() {
  sub "scripts/check-stage2-sets.sh (replay-set.txt, soak-set.txt and the save fixture vs acceptance/stage2.toml)"
  set +e
  scripts/check-stage2-sets.sh > "${TMP}/sets.log" 2>&1
  rc=$?
  set -e
  sed 's/^/  /' "${TMP}/sets.log"
  board_rc "scripts/check-stage2-sets.sh" "${rc}"
  SAVE_FIXTURE=$(sed -n 's/^save_fixture=//p' "${TMP}/sets.log")
  [ -n "${SAVE_FIXTURE}" ] || tool_error "scripts/check-stage2-sets.sh printed no save_fixture="
}
# replay_run <dir> <list> <label>: run-replay.sh over a list; every entry
# of the list must be in its summary as PASS with equal=yes.
replay_run() {
  local out=$1 list=$2 n_want n_pass
  set +e
  RUN_REPLAY_OUT=${out} ${FW}/run-replay.sh "${list}" > "${out}.log" 2>&1
  rc=$?
  set -e
  grep -E '^run-replay.sh: (===|summary|PASS|FAILED|TOOL ERROR)|^  [^ ]+: (PASS|FAIL|TOOL ERROR)|^  run-paced.sh: (speed|CHECK FAILED)|CHECK FAILED' "${out}.log" || true
  board_rc "run-replay.sh $3" "${rc}"
  n_want=$(sed 's/#.*//' "${list}" | awk 'NF' | wc -l)
  n_pass=$(grep -cE '^  [^ ]+: PASS equal=yes underruns=0 over_budget=0 ' "${out}.log" || true)
  [ "${n_pass}" = "${n_want}" ] || tool_error "run-replay.sh $3 exited 0 with ${n_pass} PASS lines for ${n_want} entries"
  stack_lines+=("$(grep -ho 'stack_used=[0-9]*/[0-9]*' "${out}"/*.run-paced.log 2> /dev/null |
    sort -t= -k2 -n | tail -n 1 | sed "s|^|$3 player-script, deepest of the entries: |" || true)")
}

# =============================================================================
if want B1; then
step B1 "selftest-pll480: clock, VOS0 and ODEN, flash latency, cycle counter, TCM probe"
# =============================================================================
run_status ${H7B}/flash.sh "${SELFTEST_BIN}"
[ "${rc}" -eq 0 ] || tool_error "flash.sh ${SELFTEST_BIN} exited ${rc} (flash refused)"
set +e
${H7B}/read-serial.sh 30 > "${TMP}/b1-serial.txt"
rc=$?
set -e
[ "${rc}" -eq 0 ] || tool_error "read-serial.sh exited ${rc} (serial unreadable)"
run_status ${H7B}/check-selftest.sh "${TMP}/b1-serial.txt" "${SELFTEST_BIN}"
board_rc "check-selftest.sh" "${rc}"
stack_lines+=("B1 selftest: $(grep -o 'stack_used=[0-9]*' "${TMP}/b1-serial.txt" | tail -n 1 || true)")
fi

# =============================================================================
if want B2; then
step B2 "bench-itcm-dtcm-pll480, ROMs in internal flash and in the QSPI NOR: hashes, +3% cycles, real time"
# =============================================================================
mkdir -p "${TMP}/b2"
set +e
H7_BENCH_RUNS=${TMP}/b2 ${H7B}/run-qspi.sh "${BENCH_BIN}" 100 > "${TMP}/b2.log" 2>&1
rc=$?
set -e
sed 's/^/  /' "${TMP}/b2.log"
board_rc "run-qspi.sh (check-report.sh --qspi: expected-hashes.txt, both passes)" "${rc}"
caps=("${TMP}"/b2/*.txt)
[ ${#caps[@]} -eq 1 ] && [ -f "${caps[0]}" ] || tool_error "run-qspi.sh left ${#caps[@]} captures in ${TMP}/b2, not 1"
grep -q '^check-report.sh: OK' "${TMP}/b2.log" || tool_error "run-qspi.sh exited 0 without check-report.sh's OK"
sub "check-cycles.sh --fresh (expected-cycles.txt +3%, two agreeing reports, real time)"
set +e
${H7B}/check-cycles.sh --fresh "${caps[0]}" "${BENCH_BIN}" > "${TMP}/b2-cycles.log" 2>&1
rc=$?
set -e
grep -v '^check-report.sh: ' "${TMP}/b2-cycles.log" | sed 's/^/  /' || true
board_rc "check-cycles.sh" "${rc}"
grep -q '^check-cycles.sh: OK' "${TMP}/b2-cycles.log" || tool_error "check-cycles.sh exited 0 without its OK"
grep -q '^REPORT: flash worst-case ' "${TMP}/b2-cycles.log" && grep -q '^REPORT: qspi worst-case ' "${TMP}/b2-cycles.log" ||
  tool_error "check-cycles.sh printed no worst-case REPORT line for both passes"
stack_lines+=("B2 bench: $(tr -d '\000\r' < "${caps[0]}" | grep '^h7bench v1 stack used=' | tail -n 1 | sed 's/^h7bench v1 stack //' || true)")
fi

# =============================================================================
if want B3; then
step B3 "Replay set in the player image (and the display readback), equal to the host"
# =============================================================================
sets_check
sub "run-headless.sh headless-display-cgb-acid2 (display readback self-test, against tools/headless/expected/cgb-acid2.txt)"
set +e
# 120 s: 600 frames with a 90 ms readback each take about a minute before
# the first report (the default 20 s read sees none).
RUN_HEADLESS_READ_SECS=120 ${FW}/run-headless.sh "${DISPLAY_BIN}" tools/headless/expected/cgb-acid2.txt > "${TMP}/b3-display.log" 2>&1
rc=$?
set -e
grep -E '^run-headless.sh: ' "${TMP}/b3-display.log" | sed 's/^/  /' || true
board_rc "run-headless.sh (display readback)" "${rc}"
grep -q '^run-headless.sh: display self-test passed: .* 0 mismatches' "${TMP}/b3-display.log" ||
  tool_error "run-headless.sh exited 0 without its display self-test line"
sub "run-replay.sh replay-set.txt (loads each ROM into the QSPI NOR)"
replay_run "${TMP}/b3" ${FW}/replay-set.txt B3
fi

# =============================================================================
if want B4; then
step B4 "10-minute synth:worst-case soak in the player image (soak-set.txt)"
# =============================================================================
sets_check
sub "run-replay.sh soak-set.txt (35,837 frames; about 12 minutes)"
replay_run "${TMP}/b4" ${FW}/soak-set.txt B4
# Emulated speed against the board's own cycle counter (`stats paced
# speed` ppm, run-paced.sh): the entry's bound must be the +-0.1%.
speed=$(grep -h '^run-paced.sh: speed ppm=' "${TMP}/b4.log" "${TMP}"/b4/*.run-paced.log 2> /dev/null | tail -n 1 || true)
[[ "${speed}" =~ ppm=([+-][0-9]+)\ \(bound\ ([0-9]+)\) ]] || tool_error "B4: no 'run-paced.sh: speed ppm=' line"
ppm=${BASH_REMATCH[1]} bound=${BASH_REMATCH[2]}
[ "${bound}" -le 1000 ] || fail "B4: the soak's speed bound was ${bound} ppm, not <= 1000 (soak-set.txt max_ppm)"
[ "${ppm#[+-]}" -le 1000 ] || fail "B4: emulated speed ${ppm} ppm of the cycle counter (want within +-1000)"
echo "  emulated speed ${ppm} ppm of the board's cycle counter (bound ${bound})"
fi

# =============================================================================
if want B5; then
step B5 "Save round trip on the player image (PC -> board -> PC)"
# =============================================================================
sets_check
mkdir -p "${TMP}/b5"
set +e
RUN_SAVE_FIXTURE=${ROOT}/${SAVE_FIXTURE} RUN_SAVE_SCRATCH=${TMP}/b5 ${FW}/run-player-save.sh > "${TMP}/b5.log" 2>&1
rc=$?
set -e
sed 's/^/  /' "${TMP}/b5.log"
board_rc "run-player-save.sh" "${rc}"
grep -q "^run-player-save: PASS fixture " "${TMP}/b5.log" || tool_error "run-player-save.sh exited 0 without its PASS line"
stack_lines+=("B5 player (live): $(grep -ao 'stack_used=[0-9]*/[0-9]*' "${TMP}/b5/play.txt" 2> /dev/null | tail -n 1 || true)")
fi
fi

# =============================================================================
step_secs[${current}]=$(($(date +%s) - step_start))
printf '\n==> Summary (%s s in all)\n' "$(($(date +%s) - gate_start))"
for s in ${HOST_STEPS} BUILD LOCK ${BOARD_STEPS}; do
  case " ${steps} BUILD LOCK " in *" $s "*) ;; *) continue ;; esac
  [ -n "${step_secs[$s]:-}" ] || { want "$s" || continue; }
  printf '  %s OK  %6s s\n' "$s" "${step_secs[$s]:-?}"
done
if [ ${#stack_lines[@]} -gt 0 ]; then
  printf '\n==> Stack paint depth on the board (report only; H8)\n'
  printf '  %s\n' "${stack_lines[@]}"
fi
if [ "${partial}" = 1 ]; then
  printf '\nSTEPS PASSED (partial run: %s); a partial run is never a pass\n' "${steps}"
  finish 3
fi
for s in ${ALL_STEPS}; do want "$s" || tool_error "step $s did not run in a full run"; done
printf '\nSTAGE 2 GATE PASSED (H1-H8, B1-B5)\n'
finish 0
