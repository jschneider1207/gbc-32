#!/usr/bin/env bash
# Run the PC companion (tools/gbc-pc) against the board from WSL.
#
#   scripts/gbc-pc.sh load <rom> [--port COM5] [--baud 2000000] [--retries 5] [--timeout-ms 2000]
#   scripts/gbc-pc.sh hello      [--port COM5] [--baud 2000000]
#
# WSL has poor COM-port access, so gbc-pc is a native Windows console .exe
# (plan stage-2.md, "Crate layout"). This cross-builds it for
# x86_64-pc-windows-gnu, copies the .exe (and, for `load`, the ROM) into a
# scratch dir under the Windows %TEMP% (as scripts/run-windows.sh does),
# runs it through WSL interop with the given arguments while holding the
# board lock (scripts/lib/board-lock.sh), deletes the scratch dir and exits
# with the exe's exit code (0 ok, 1 link/load failure, 2 usage error).
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# shellcheck source=lib/board-lock.sh
. "$root/scripts/lib/board-lock.sh"

usage() {
  echo "usage: $0 load <rom> [--port COM5] [--baud N] [--retries N] [--timeout-ms N]" >&2
  echo "       $0 hello [--port COM5] [--baud N] [--retries N] [--timeout-ms N]" >&2
  exit 2
}

[ $# -ge 1 ] || usage
cmd=$1; shift
rom=""
case $cmd in
  load)
    [ $# -ge 1 ] || usage
    rom=$1; shift
    [ -f "$rom" ] || { echo "gbc-pc.sh: no such ROM: $rom" >&2; exit 2; }
    ;;
  hello) ;;
  *) usage ;;
esac

exe=$(cargo build -p gbc-pc --release --target x86_64-pc-windows-gnu \
  --message-format=json-render-diagnostics \
  | sed -n 's/.*"executable":"\([^"]*gbc-pc\.exe\)".*/\1/p' | tail -n 1)
[ -n "$exe" ] && [ -f "$exe" ] || { echo "gbc-pc.sh: cannot find the .exe cargo built" >&2; exit 2; }

# Run cmd.exe from a Windows cwd: from a Linux path it warns about UNC paths.
wintemp=$(cd /mnt/c && cmd.exe /d /c 'echo %TEMP%' | tr -d '\r')
scratch=$(mktemp -d "$(wslpath -u "$wintemp")/gbc-32-pc-XXXXXX")
trap 'rm -rf "$scratch"' EXIT

cp "$exe" "$scratch/gbc-pc.exe"
args=("$cmd")
if [ -n "$rom" ]; then
  rom_name=$(basename "$rom")
  cp "$rom" "$scratch/$rom_name"
  args+=("$(wslpath -w "$scratch/$rom_name")")
fi
args+=("$@")

run_exe() { (cd "$scratch" && ./gbc-pc.exe "${args[@]}"); }

set +e
with_board_lock run_exe
code=$?
set -e
echo "gbc-pc.sh: exit code $code"
exit "$code"
