#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
CHECK_MODEL_VARIANT="$REPO_ROOT/scripts/check-model-variant.sh"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-model-variants.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT

MANIFEST="$TEMP_ROOT/model-manifest.json"
FULL_RESOURCES="$TEMP_ROOT/full-resources"
UPDATE_RESOURCES="$TEMP_ROOT/update-resources"
MODEL_DIRECTORY="$FULL_RESOURCES/models/gigaam-v3-rnnt-v1"

mkdir -p "$MODEL_DIRECTORY" "$UPDATE_RESOURCES"
printf '%s' 'enc' >"$MODEL_DIRECTORY/encoder.int8.onnx"
printf '%s' 'dec' >"$MODEL_DIRECTORY/decoder.onnx"
printf '%s' 'join' >"$MODEL_DIRECTORY/joiner.onnx"
printf '%s' 'tok' >"$MODEL_DIRECTORY/tokens.txt"
chmod 600 "$MODEL_DIRECTORY"/*

printf '%s\n' \
    '{' \
    '  "schema": 1,' \
    '  "id": "gigaam-v3-rnnt-v1",' \
    '  "files": [' \
    '    {"name":"encoder.int8.onnx","size":3,"sha256":"5fb2ab76ed9bda034b192c48c7069359252fccda168d925acc0ae7d316c0b53e"},' \
    '    {"name":"decoder.onnx","size":3,"sha256":"e7502c799b8f76fbed077ff2cd55c906ab144d5b88ef09a71abc70b5fad601f1"},' \
    '    {"name":"joiner.onnx","size":4,"sha256":"58393216032be6257784ac0c6a73efb2a084e27b4cfff1e6acee7b7e6ab93b10"},' \
    '    {"name":"tokens.txt","size":3,"sha256":"1a7674eb4ee78df7e1ac439a93c3fa8e3c945784d4dec9fd8e3011738b2f1d62"}' \
    '  ]' \
    '}' >"$MANIFEST"

expect_failure_containing() {
    local expected="$1"
    shift
    local output
    if output="$("$@" 2>&1)"; then
        echo "expected command to fail: $*" >&2
        exit 1
    fi
    [[ "$output" == *"$expected"* ]] || {
        echo "failure did not contain '$expected': $output" >&2
        exit 1
    }
}

"$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
"$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --model-source "$MODEL_DIRECTORY"
"$CHECK_MODEL_VARIANT" \
    --variant update \
    --model-manifest "$MANIFEST" \
    --resources "$UPDATE_RESOURCES"

mkdir "$UPDATE_RESOURCES/models"
expect_failure_containing "must not contain Resources/models" \
    "$CHECK_MODEL_VARIANT" \
    --variant update \
    --model-manifest "$MANIFEST" \
    --resources "$UPDATE_RESOURCES"
rmdir "$UPDATE_RESOURCES/models"

printf '%s' 'extra' >"$MODEL_DIRECTORY/extra"
expect_failure_containing "exactly four entries" \
    "$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
rm "$MODEL_DIRECTORY/extra"

chmod 700 "$MODEL_DIRECTORY/tokens.txt"
expect_failure_containing "model entry is executable" \
    "$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
chmod 600 "$MODEL_DIRECTORY/tokens.txt"

printf '%s' 'bad' >"$MODEL_DIRECTORY/tokens.txt"
expect_failure_containing "model SHA-256 mismatch" \
    "$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
printf '%s' 'tok' >"$MODEL_DIRECTORY/tokens.txt"

mv "$MODEL_DIRECTORY/tokens.txt" "$TEMP_ROOT/real-tokens.txt"
ln -s "$TEMP_ROOT/real-tokens.txt" "$MODEL_DIRECTORY/tokens.txt"
expect_failure_containing "model entry is not a real file" \
    "$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
rm "$MODEL_DIRECTORY/tokens.txt"
mv "$TEMP_ROOT/real-tokens.txt" "$MODEL_DIRECTORY/tokens.txt"

expect_failure_containing "update variant does not accept --model-source" \
    "$REPO_ROOT/scripts/build-app.sh" \
    --variant update \
    --model-manifest "$MANIFEST" \
    --model-source "$MODEL_DIRECTORY"
expect_failure_containing "full variant requires --model-source" \
    "$REPO_ROOT/scripts/build-app.sh" \
    --variant full \
    --model-manifest "$MANIFEST"
expect_failure_containing "app bundle is not a real directory" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$MANIFEST" \
    --app "$TEMP_ROOT/missing.app" \
    --output "$TEMP_ROOT/update.dmg"
if grep -Fq '"$SCRIPT_DIR/build-app.sh"' "$REPO_ROOT/scripts/build-dmg.sh"; then
    echo "build-dmg.sh must consume a supplied app without rebuilding" >&2
    exit 1
fi
grep -Fq -- '--variant "$BUNDLE_VARIANT"' "$REPO_ROOT/scripts/grant-and-run.sh" || {
    echo "grant-and-run.sh must pass the bundle diagnostic variant" >&2
    exit 1
}

echo "Model bundle variant checks passed"
