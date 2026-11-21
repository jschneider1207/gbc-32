#!/usr/bin/env bash
# Downloads the SM83 single-step CPU tests (SingleStepTests/sm83, MIT
# license) pinned to one commit into test-data/sm83/ (gitignored). Used by
# tools/sm83-tests; without them those tests skip (or fail, with
# GBC_REQUIRE_TEST_DATA=1).
# https://github.com/SingleStepTests/sm83
#
# After the tarball's SHA-256 check, the extracted JSON files are checked
# against the committed per-file list tools/sm83-tests/sm83-v1.sha256 (the
# test re-verifies every file it loads against the same list). If that
# list doesn't exist yet, it is written from the verified tarball.
set -euo pipefail

COMMIT="f9c30210245dd691661db39f5ace022c465ecc2f"
SHA256="ac364307f2bc012034ae55b14d237f832cb3f3d9b14bb9c7c80086165d1afeaf"
URL="https://codeload.github.com/SingleStepTests/sm83/tar.gz/${COMMIT}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/test-data/sm83"
HASHES="${ROOT}/tools/sm83-tests/sm83-v1.sha256"

if [[ -f "${DEST}/.version" && "$(cat "${DEST}/.version")" == "${COMMIT}" ]]; then
    echo "sm83 tests ${COMMIT} already present in ${DEST}"
    exit 0
fi

TMP="$(mktemp -d)"
trap 'rm -rf "${TMP}"' EXIT

echo "Downloading SingleStepTests/sm83 ${COMMIT}..."
curl -fsSL -o "${TMP}/sm83.tar.gz" "${URL}"
echo "${SHA256}  ${TMP}/sm83.tar.gz" | sha256sum -c --quiet -

mkdir -p "${TMP}/x"
tar -xzf "${TMP}/sm83.tar.gz" -C "${TMP}/x"
# Per-file hashes of v1/*.json, sorted by name ("<sha256>  <file>").
(cd "${TMP}/x/sm83-${COMMIT}/v1" && export LC_ALL=C && sha256sum -- *.json) > "${TMP}/v1.sha256"
if [[ -f "${HASHES}" ]]; then
    if ! cmp -s "${TMP}/v1.sha256" "${HASHES}"; then
        echo "error: extracted files don't match ${HASHES}" >&2
        exit 1
    fi
else
    cp "${TMP}/v1.sha256" "${HASHES}"
    echo "Wrote $(wc -l < "${HASHES}") per-file hashes to ${HASHES}"
fi
rm -rf "${DEST}"
mkdir -p "${DEST}"
mv "${TMP}/x/sm83-${COMMIT}/v1" "${DEST}/v1"
mv "${TMP}/x/sm83-${COMMIT}/LICENSE" "${DEST}/LICENSE"
echo "${COMMIT}" > "${DEST}/.version"
echo "Extracted $(ls "${DEST}/v1" | wc -l) test files to ${DEST}/v1"
