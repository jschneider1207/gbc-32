#!/usr/bin/env bash
# Run the PC companion (tools/gbc-pc) against the board from WSL.
#
#   scripts/gbc-pc.sh load <rom> [--sav <file.sav>] [--raw-save] [link options]
#   scripts/gbc-pc.sh dump <out.sav> [--raw-save] [link options]
#   scripts/gbc-pc.sh play [--sav-out <file.sav>] [--seconds N] [--raw-save] [--audio-wav <file.wav>] [link options]
#   scripts/gbc-pc.sh hello                                     [link options]
#   link options: [--port COM5|null] [--baud 2000000] [--retries 5] [--timeout-ms 2000]
#
# WSL has poor COM-port access, so gbc-pc is a native Windows console .exe
# (plan stage-2.md, "Crate layout"). This cross-builds it for
# x86_64-pc-windows-gnu, copies the .exe (and, for `load`, the ROM) into a
# scratch dir under the Windows %TEMP% (as scripts/run-windows.sh does),
# runs it through WSL interop with the given arguments while holding the
# board lock (scripts/lib/board-lock.sh), deletes the scratch dir and exits
# with the exe's exit code (0 ok, 1 link/load/dump failure, 2 usage error).
#
# Saves (M5, docs/reference/link-protocol.md): `--sav` is copied into the
# scratch dir like the ROM. `dump` and `play --sav-out` write into the
# scratch dir (the exe writes only a complete, verified save, via a
# `.part` file and a rename), and the file is copied to the given path
# after the exe exits (again via `<path>.part` and mv), so in `play` an
# F5 dump reaches the WSL path only when play ends. `--port null` runs
# without a board (no COM port, no board lock), e.g. the interop smoke
# test `scripts/gbc-pc.sh play --port null --seconds 2`.
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# shellcheck source=lib/board-lock.sh
. "$root/scripts/lib/board-lock.sh"

usage() {
  echo "usage: $0 load <rom> [--sav <file.sav>] [--raw-save] [link options]" >&2
  echo "       $0 dump <out.sav> [--raw-save] [link options]" >&2
  echo "       $0 play [--sav-out <file.sav>] [--seconds N] [--raw-save] [--audio-wav <file.wav>] [link options]" >&2
  echo "       $0 hello [link options]" >&2
  echo "link options: [--port COM5|null] [--baud N] [--retries N] [--timeout-ms N]" >&2
  exit 2
}

[ $# -ge 1 ] || usage
cmd=$1; shift
rom=""
out=""   # where a dumped save goes (dump <out>, play --sav-out <out>)
case $cmd in
  load)
    [ $# -ge 1 ] || usage
    rom=$1; shift
    [ -f "$rom" ] || { echo "gbc-pc.sh: no such ROM: $rom" >&2; exit 2; }
    ;;
  dump)
    [ $# -ge 1 ] || usage
    out=$1; shift
    ;;
  play|hello) ;;
  *) usage ;;
esac

sav=""
wav=""
null_port=0
rest=()
while [ $# -gt 0 ]; do
  case $1 in
    --sav)
      [ $# -ge 2 ] || usage
      sav=$2; shift 2
      [ -f "$sav" ] || { echo "gbc-pc.sh: no such save: $sav" >&2; exit 2; }
      ;;
    --sav-out)
      [ $# -ge 2 ] || usage
      out=$2; shift 2
      ;;
    --audio-wav)
      [ $# -ge 2 ] || usage
      wav=$2; shift 2
      [ -d "$(dirname "$wav")" ] || { echo "gbc-pc.sh: no such directory for $wav" >&2; exit 2; }
      ;;
    --port)
      [ $# -ge 2 ] || usage
      [ "${2,,}" = null ] && null_port=1
      rest+=("$1" "$2"); shift 2
      ;;
    *) rest+=("$1"); shift ;;
  esac
done
if [ -n "$out" ]; then
  [ -d "$(dirname "$out")" ] || { echo "gbc-pc.sh: no such directory for $out" >&2; exit 2; }
fi

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
if [ -n "$sav" ]; then
  mkdir "$scratch/sav-in"
  cp "$sav" "$scratch/sav-in/$(basename "$sav")"
  args+=(--sav "$(wslpath -w "$scratch/sav-in/$(basename "$sav")")")
fi
win_out=""
if [ -n "$out" ]; then
  win_out=$(wslpath -w "$scratch")'\out.sav'
  case $cmd in
    dump) args+=("$win_out") ;;
    play) args+=(--sav-out "$win_out") ;;
  esac
fi
# The WAV is written in the Windows scratch dir, then copied out.
[ -z "$wav" ] || args+=(--audio-wav "$(wslpath -w "$scratch")\\audio.wav")
args+=("${rest[@]}")

run_exe() { (cd "$scratch" && ./gbc-pc.exe "${args[@]}"); }

set +e
if [ "$null_port" = 1 ]; then
  run_exe
else
  with_board_lock run_exe
fi
code=$?
set -e
if [ -n "$out" ] && [ -f "$scratch/out.sav" ]; then
  cp "$scratch/out.sav" "$out.part" && mv "$out.part" "$out"
  echo "gbc-pc.sh: save written to $out"
fi
if [ -n "$wav" ] && [ -f "$scratch/audio.wav" ]; then
  cp "$scratch/audio.wav" "$wav.part" && mv "$wav.part" "$wav"
  echo "gbc-pc.sh: audio written to $wav"
fi
echo "gbc-pc.sh: exit code $code"
exit "$code"
