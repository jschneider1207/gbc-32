#!/usr/bin/env bash
# Installs the system packages needed to build gbc-desktop (window, input,
# audio) on Ubuntu/Debian, including WSL2.
#
#   sudo ./scripts/install-system-deps.sh     # install whatever is missing
#   ./scripts/install-system-deps.sh --check  # report only, no root needed
#
# This script deliberately does not call sudo itself: a human runs it with
# sudo. Agents may run --check, but never install (see AGENTS.md).
set -euo pipefail

PACKAGES=(
    pkg-config        # library discovery for -sys crates
    libasound2-dev    # ALSA: audio output (cpal)
    libudev-dev       # gamepad hotplug (gilrs)
    libwayland-dev    # Wayland windowing (WSLg default)
    libxkbcommon-dev  # keyboard layouts (winit)
    libx11-dev        # X11 windowing fallback
)

missing=()
for pkg in "${PACKAGES[@]}"; do
    if dpkg-query -W -f='${Status}' "${pkg}" 2>/dev/null | grep -q 'install ok installed'; then
        echo "  ok       ${pkg}"
    else
        echo "  MISSING  ${pkg}"
        missing+=("${pkg}")
    fi
done

if [[ ${#missing[@]} -eq 0 ]]; then
    echo "All system dependencies are installed."
    exit 0
fi

if [[ "${1:-}" == "--check" ]]; then
    echo "Missing: ${missing[*]}"
    echo "Install with: sudo ./scripts/install-system-deps.sh"
    exit 1
fi

if [[ ${EUID} -ne 0 ]]; then
    echo "Run as root: sudo ./scripts/install-system-deps.sh" >&2
    exit 1
fi

command -v apt-get >/dev/null || { echo "apt-get not found; this script supports Debian/Ubuntu only" >&2; exit 1; }

apt-get update
apt-get install -y --no-install-recommends "${missing[@]}"
echo "Installed: ${missing[*]}"
