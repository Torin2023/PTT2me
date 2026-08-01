#!/bin/bash
set -euo pipefail

readonly EXPECTED_SHA256="d012004c0706adafdcfa05677f0c10679ef844810e2ebc297f9dc9689150b239"

fail() {
    echo "PTT2me production model manifest check failed: $*" >&2
    exit 1
}

[[ $# -eq 1 ]] || fail "usage: $0 MODEL_MANIFEST"
MODEL_MANIFEST="$1"
[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "manifest is not a real file: $MODEL_MANIFEST"

ACTUAL_SHA256="$(shasum -a 256 "$MODEL_MANIFEST" | awk '{print $1}')" ||
    fail "could not hash manifest: $MODEL_MANIFEST"
[[ "$ACTUAL_SHA256" == "$EXPECTED_SHA256" ]] ||
    fail "production manifest SHA-256 mismatch: expected $EXPECTED_SHA256, got $ACTUAL_SHA256"

echo "PTT2me production model manifest is exact: $MODEL_MANIFEST"
