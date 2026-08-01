#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"

fail() {
    echo "PTT2me update manifest validation failed: $*" >&2
    exit 1
}

[[ $# -eq 5 ]] ||
    fail "usage: $0 PUBLIC_KEY MANIFEST FULL_DMG UPDATE_DMG MODEL_MANIFEST"

for path in "$@"; do
    [[ -f "$path" && ! -L "$path" ]] || fail "input is not a real file: $path"
done

if [[ -n "${PTT2ME_MANIFEST_VERIFIER:-}" ]]; then
    if [[ -n "${PTT2ME_MANIFEST_LIBRARY_PATH:-}" ]]; then
        DYLD_LIBRARY_PATH="$PTT2ME_MANIFEST_LIBRARY_PATH" \
            "$PTT2ME_MANIFEST_VERIFIER" --verify-update-manifest "$@"
    else
        "$PTT2ME_MANIFEST_VERIFIER" --verify-update-manifest "$@"
    fi
else
    cd -- "$REPO_ROOT"
    cargo run --quiet --bin ptt2me -- --verify-update-manifest "$@"
fi

echo "Signed update manifest and release inputs are valid"
