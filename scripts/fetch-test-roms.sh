#!/usr/bin/env bash
# Downloads the pinned c-sp/game-boy-test-roms release into test-roms/ (gitignored).
# https://github.com/c-sp/game-boy-test-roms
set -euo pipefail

VERSION="v7.0"
SHA256="b9a9d7a1075aa35a3d07c07c34974048672d8520dca9e07a50178f5860c3832c"
URL="https://github.com/c-sp/game-boy-test-roms/releases/download/${VERSION}/game-boy-test-roms-${VERSION}.zip"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/test-roms"

if [[ -f "${DEST}/.version" && "$(cat "${DEST}/.version")" == "${VERSION}" ]]; then
    echo "test-roms ${VERSION} already present in ${DEST}"
    exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

echo "Downloading game-boy-test-roms ${VERSION}..."
curl -fsSL -o "${TMP}/roms.zip" "${URL}"
echo "${SHA256}  ${TMP}/roms.zip" | sha256sum -c --quiet -

rm -rf "${DEST}"
mkdir -p "${DEST}"
unzip -q "${TMP}/roms.zip" -d "${DEST}"
echo "${VERSION}" > "${DEST}/.version"
echo "Extracted to ${DEST}"
