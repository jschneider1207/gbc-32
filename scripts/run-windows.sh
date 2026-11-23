#!/usr/bin/env bash
# Smoke-test the native Windows build from WSL (AGENTS.md, "Machine resources":
# the one allowed way to open the desktop window; never run the Linux binary's
# window under WSLg).
#
#   scripts/run-windows.sh <rom> --frames N [--out shot.png] [gbc-desktop args...]
#
# Cross-builds gbc-desktop for x86_64-pc-windows-gnu, copies the .exe and the
# ROM into a scratch dir under the Windows %TEMP% (so the save file lands there,
# never next to your ROM), runs it with --frames and --screenshot via WSL
# interop, copies the PNG back (--out, default target/run-windows/last.png),
# deletes the scratch dir and exits with the exe's exit code. Set
# GBC_AUDIO_STATS=1 to see the audio statistics on stderr.
set -euo pipefail

cd "$(dirname "$0")/.."
root=$PWD
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

usage() { echo "usage: $0 <rom> --frames N [--out shot.png] [gbc-desktop args...]" >&2; exit 2; }

[ $# -ge 1 ] || usage
rom=$1; shift
[ -f "$rom" ] || { echo "run-windows: no such ROM: $rom" >&2; exit 2; }

out="$root/target/run-windows/last.png"
has_frames=0
has_shot=0
args=()
while [ $# -gt 0 ]; do
  case $1 in
    --out) [ $# -ge 2 ] || usage; out=$(realpath -m "$2"); shift 2 ;;
    --frames) has_frames=1; args+=("$1"); [ $# -ge 2 ] && { args+=("$2"); shift; }; shift ;;
    --screenshot) has_shot=1; args+=("$1"); [ $# -ge 2 ] && { args+=("$2"); shift; }; shift ;;
    *) args+=("$1"); shift ;;
  esac
done
[ "$has_frames" = 1 ] || { echo "run-windows: --frames N is required (the window must close itself)" >&2; exit 2; }
[ "$has_shot" = 0 ] || { echo "run-windows: use --out <file> instead of --screenshot" >&2; exit 2; }

exe=$(cargo build -p gbc-desktop --release --target x86_64-pc-windows-gnu \
  --message-format=json-render-diagnostics \
  | sed -n 's/.*"executable":"\([^"]*\)".*/\1/p' | tail -n 1)
[ -n "$exe" ] && [ -f "$exe" ] || { echo "run-windows: cannot find the .exe cargo built" >&2; exit 2; }

# Run cmd.exe from a Windows cwd: from a Linux path it warns about UNC paths.
wintemp=$(cd /mnt/c && cmd.exe /d /c 'echo %TEMP%' | tr -d '\r')
scratch=$(mktemp -d "$(wslpath -u "$wintemp")/gbc-32-run-XXXXXX")
trap 'rm -rf "$scratch"' EXIT

cp "$exe" "$scratch/gbc-desktop.exe"
rom_name=$(basename "$rom")
cp "$rom" "$scratch/$rom_name"

# GBC_AUDIO_STATS reaches the Windows process only if listed in WSLENV.
export WSLENV="${WSLENV:+$WSLENV:}GBC_AUDIO_STATS"

set +e
(cd "$scratch" && ./gbc-desktop.exe "$(wslpath -w "$scratch/$rom_name")" \
  --screenshot "$(wslpath -w "$scratch/shot.png")" "${args[@]}")
code=$?
set -e
echo "run-windows: exit code $code"

if [ -f "$scratch/shot.png" ]; then
  mkdir -p "$(dirname "$out")"
  cp "$scratch/shot.png" "$out"
  echo "run-windows: screenshot $out"
else
  echo "run-windows: no screenshot was written" >&2
fi
exit "$code"
