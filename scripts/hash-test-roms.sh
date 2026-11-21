#!/usr/bin/env bash
# Writes acceptance/test-roms.sha256: the SHA-256 of every file under
# test-roms/, which romtest verifies before every run (ADR 0007,
# amendments). A missing, changed or unlisted file is then a runner error
# (exit 2) instead of a silently skipped ROM or a no-reference result.
#
# Run this only after scripts/fetch-test-roms.sh has fetched a NEW pinned
# release, and commit the list together with that change. Never regenerate
# it to make a run pass.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ROMS="${ROOT}/test-roms"
DEST="${ROOT}/acceptance/test-roms.sha256"

[[ -f "${ROMS}/.version" ]] || { echo "no ${ROMS}/.version: run scripts/fetch-test-roms.sh" >&2; exit 1; }
VERSION="$(tr -d '[:space:]' < "${ROMS}/.version")"

cd "${ROMS}"
# sha256sum escapes names containing a backslash or newline; romtest
# doesn't parse those, so refuse them here.
if find . -type f \( -name '*\\*' -o -name $'*\n*' \) | grep -q .; then
    echo "test-roms/ has a file name with a backslash or newline; not supported" >&2
    exit 1
fi

TMP="$(mktemp "${DEST}.XXXXXX")"
trap 'rm -f "${TMP}"' EXIT
{
    echo "# SHA-256 of every file under test-roms/ (c-sp/game-boy-test-roms ${VERSION})."
    echo "# romtest checks these before every run (ADR 0007). Regenerate only after"
    echo "# fetching a new pinned release: scripts/hash-test-roms.sh"
    echo "# Verify by hand: cd test-roms && sha256sum --quiet -c ../acceptance/test-roms.sha256"
    echo "# release: ${VERSION}"
    # Sorted by byte value, like romtest's paths.
    find . -type f -printf '%P\0' | LC_ALL=C sort -z | xargs -0 sha256sum --
} > "${TMP}"
chmod 644 "${TMP}"
mv "${TMP}" "${DEST}"
trap - EXIT
echo "wrote ${DEST} ($(grep -vc '^#' "${DEST}") files, release ${VERSION})"
