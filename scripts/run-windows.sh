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
# GBC_AUDIO_STATS=1 and/or GBC_FRAME_STATS=1 to see audio and frame-pacing
# statistics on stderr (GBC_PRESENT_PHASE=0 and GBC_PRECISE_WAIT=0, which
# turn off phase-aware presents and precise waits, are forwarded too).
#
# Files (docs/desktop-diagnostics.md): GBC_FRAME_LOG=<linux path> writes the
# per-frame CSV there (the exe writes it in the scratch dir; it is copied
# back). --record-input / --frame-hashes <linux path> are copied back the
# same way, and --replay-input <linux path> is copied in.
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
replay_in=""
record_out=""
hashes_out=""
while [ $# -gt 0 ]; do
  case $1 in
    --replay-input) [ $# -ge 2 ] || usage; replay_in=$2; shift 2 ;;
    --record-input) [ $# -ge 2 ] || usage; record_out=$(realpath -m "$2"); shift 2 ;;
    --frame-hashes) [ $# -ge 2 ] || usage; hashes_out=$(realpath -m "$2"); shift 2 ;;
    --out) [ $# -ge 2 ] || usage; out=$(realpath -m "$2"); shift 2 ;;
    --frames) has_frames=1; args+=("$1"); [ $# -ge 2 ] && { args+=("$2"); shift; }; shift ;;
    --screenshot) has_shot=1; args+=("$1"); [ $# -ge 2 ] && { args+=("$2"); shift; }; shift ;;
    *) args+=("$1"); shift ;;
  esac
done
[ "$has_frames" = 1 ] || { echo "run-windows: --frames N is required (the window must close itself)" >&2; exit 2; }
[ "$has_shot" = 0 ] || { echo "run-windows: use --out <file> instead of --screenshot" >&2; exit 2; }
[ -z "$replay_in" ] || [ -f "$replay_in" ] || { echo "run-windows: no such recording: $replay_in" >&2; exit 2; }
log_out=""
[ -z "${GBC_FRAME_LOG:-}" ] || log_out=$(realpath -m "$GBC_FRAME_LOG")

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

win() { wslpath -w "$scratch/$1"; }
if [ -n "$replay_in" ]; then
  cp "$replay_in" "$scratch/replay.txt"
  args+=(--replay-input "$(win replay.txt)")
fi
[ -z "$record_out" ] || args+=(--record-input "$(win record.txt)")
[ -z "$hashes_out" ] || args+=(--frame-hashes "$(win hashes.txt)")
[ -z "$log_out" ] || export GBC_FRAME_LOG="$(win frames.csv)"

# The GBC_* variables reach the Windows process only via WSLENV.
export WSLENV="${WSLENV:+$WSLENV:}GBC_AUDIO_STATS:GBC_FRAME_STATS:GBC_FRAME_LOG:GBC_PRESENT_PHASE:GBC_PRECISE_WAIT"

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
copy_back() { # <scratch file> <destination> <what>
  [ -n "$2" ] || return 0
  if [ -f "$scratch/$1" ]; then
    mkdir -p "$(dirname "$2")"
    cp "$scratch/$1" "$2"
    echo "run-windows: $3 $2"
  else
    echo "run-windows: no $3 was written" >&2
  fi
}
copy_back frames.csv "$log_out" "frame log"
copy_back record.txt "$record_out" "input recording"
copy_back hashes.txt "$hashes_out" "frame hashes"
exit "$code"
