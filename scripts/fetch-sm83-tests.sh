#!/usr/bin/env bash
# Downloads the SM83 single-step CPU tests (SingleStepTests/sm83, MIT
# license) pinned to one commit into test-data/sm83/ (gitignored). Used by
# tools/sm83-tests; without them those tests skip.
# https://github.com/SingleStepTests/sm83
set -euo pipefail

COMMIT="f9c30210245dd691661db39f5ace022c465ecc2f"
SHA256="ac364307f2bc012034ae55b14d237f832cb3f3d9b14bb9c7c80086165d1afeaf"
URL="https://codeload.github.com/SingleStepTests/sm83/tar.gz/${COMMIT}"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="${ROOT}/test-data/sm83"

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
rm -rf "${DEST}"
mkdir -p "${DEST}"
mv "${TMP}/x/sm83-${COMMIT}/v1" "${DEST}/v1"
mv "${TMP}/x/sm83-${COMMIT}/LICENSE" "${DEST}/LICENSE"
echo "${COMMIT}" > "${DEST}/.version"
echo "Extracted $(ls "${DEST}/v1" | wc -l) test files to ${DEST}/v1"
