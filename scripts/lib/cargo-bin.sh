# shellcheck shell=bash
# Sourced by the firmware build scripts (crates/gbc-nucleo-h753/build.sh,
# tools/h7-bench/build.sh, tools/headless-qemu/run.sh).
#
#   build_bin <bin> CMD...   runs CMD (a `cargo build ...` command, possibly
#                            behind a lock wrapper) with
#                            --message-format=json-render-diagnostics
#                            appended, and prints the path of the ELF cargo
#                            reports for the bin target <bin>.
#
# The path comes from cargo itself (the `executable` of the bin's
# `compiler-artifact` message), not from an assumed
# target/<triple>/release/<bin>: a target directory moved by any cargo
# config (say an untracked .cargo/config.toml between the repository root
# and the crate) would otherwise leave the script copying and checking a
# stale ELF. Diagnostics still go to stderr; the JSON goes to a temp file.
# Returns non-zero (with a message) if CMD fails or the messages do not
# name exactly one such executable.

build_bin() {
  local bin=$1 msgs paths rc n
  shift
  msgs=$(mktemp) || return 2
  "$@" --message-format=json-render-diagnostics > "$msgs"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    rm -f "$msgs"
    echo "build_bin: '$*' exited $rc" >&2
    return "$rc"
  fi
  paths=$(grep -F '"reason":"compiler-artifact"' "$msgs" | grep -F '"kind":["bin"]' |
    grep -F "\"name\":\"$bin\"" | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p')
  rm -f "$msgs"
  n=$(grep -c . <<< "$paths")
  if [ "$n" -ne 1 ]; then
    echo "build_bin: cargo reported $n executables for bin '$bin', not 1: ${paths//$'\n'/ }" >&2
    return 2
  fi
  [ -f "$paths" ] || { echo "build_bin: cargo's executable $paths does not exist" >&2; return 2; }
  printf '%s\n' "$paths"
}
