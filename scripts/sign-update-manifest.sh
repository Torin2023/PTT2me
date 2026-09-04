#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"

fail() {
    echo "PTT2me update manifest signing failed: $*" >&2
    exit 1
}

[[ $# -eq 3 ]] ||
    fail "usage: $0 PRIVATE_KEY PAYLOAD OUTPUT (PRIVATE_KEY must be outside Git)"

PRIVATE_KEY="$1"
PAYLOAD="$2"
OUTPUT="$3"

[[ -f "$PRIVATE_KEY" && ! -L "$PRIVATE_KEY" ]] ||
    fail "private key is not a real file"
PRIVATE_KEY_DIR="$(cd -- "$(dirname -- "$PRIVATE_KEY")" && pwd -P)" ||
    fail "could not resolve private key directory"
PRIVATE_KEY="$PRIVATE_KEY_DIR/$(basename -- "$PRIVATE_KEY")"
case "$PRIVATE_KEY" in
    "$REPO_ROOT" | "$REPO_ROOT"/*)
        fail "private key must be stored outside the Git repository"
        ;;
esac

PRIVATE_MODE="$(stat -f '%Lp' "$PRIVATE_KEY" 2>/dev/null)" ||
    fail "could not inspect private key permissions"
[[ $((8#$PRIVATE_MODE & 077)) -eq 0 ]] ||
    fail "private key permissions must deny group and other access"
PRIVATE_ACL="$(/bin/ls -lde "$PRIVATE_KEY" 2>/dev/null)" ||
    fail "could not inspect private key ACL"
[[ "$(printf '%s\n' "$PRIVATE_ACL" | wc -l | tr -d ' ')" == "1" ]] ||
    fail "private key must not have ACL entries"

[[ -f "$PAYLOAD" && ! -L "$PAYLOAD" ]] ||
    fail "payload is not a real file"
[[ ! -e "$OUTPUT" && ! -L "$OUTPUT" ]] ||
    fail "output already exists"
OUTPUT_DIR="$(cd -- "$(dirname -- "$OUTPUT")" && pwd -P)" ||
    fail "output directory does not exist"
OUTPUT="$OUTPUT_DIR/$(basename -- "$OUTPUT")"

if [[ -n "${PTT2ME_MANIFEST_SIGNER:-}" ]]; then
    "$PTT2ME_MANIFEST_SIGNER" "$PRIVATE_KEY" "$PAYLOAD" "$OUTPUT"
else
    cd -- "$REPO_ROOT"
    cargo run --quiet --bin ptt2me-update-signer -- \
        "$PRIVATE_KEY" "$PAYLOAD" "$OUTPUT"
fi

echo "Signed update manifest: $OUTPUT"
