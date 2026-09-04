#!/bin/bash
set -euo pipefail

fail() {
    echo "PTT2me icon check failed: $*" >&2
    exit 1
}

[[ $# -eq 2 ]] || fail "usage: $0 COMMITTED_ICON BUNDLE_ICON"
COMMITTED_ICON="$1"
BUNDLE_ICON="$2"

[[ -f "$COMMITTED_ICON" && ! -L "$COMMITTED_ICON" ]] ||
    fail "committed icon is not a real file: $COMMITTED_ICON"
[[ -f "$BUNDLE_ICON" && ! -L "$BUNDLE_ICON" ]] ||
    fail "bundle icon is not a real file: $BUNDLE_ICON"

validate_icns() {
    local path="$1"
    local label="$2"
    local properties
    properties="$(sips -g format -g pixelWidth -g pixelHeight "$path" 2>/dev/null)" ||
        fail "$label icon is not a valid ICNS image"
    [[ "$properties" == *'format: icns'* && \
        "$properties" == *'pixelWidth: 1024'* && \
        "$properties" == *'pixelHeight: 1024'* ]] ||
        fail "$label icon is not a valid ICNS image"
}

validate_icns "$COMMITTED_ICON" committed
validate_icns "$BUNDLE_ICON" bundle
cmp -s "$COMMITTED_ICON" "$BUNDLE_ICON" ||
    fail "bundle icon does not match the committed application icon"

echo "PTT2me application icon is valid: $BUNDLE_ICON"
