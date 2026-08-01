#!/bin/bash
set -euo pipefail

readonly MODEL_ID="gigaam-v3-rnnt-v1"

fail() {
    echo "PTT2me model variant check failed: $*" >&2
    exit 1
}

usage() {
    fail "usage: $0 --variant full|update --model-manifest PATH (--resources PATH | --model-source PATH)"
}

VARIANT=""
MODEL_MANIFEST=""
RESOURCES=""
MODEL_SOURCE=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --variant)
            [[ $# -ge 2 ]] || usage
            VARIANT="$2"
            shift 2
            ;;
        --model-manifest)
            [[ $# -ge 2 ]] || usage
            MODEL_MANIFEST="$2"
            shift 2
            ;;
        --resources)
            [[ $# -ge 2 ]] || usage
            RESOURCES="$2"
            shift 2
            ;;
        --model-source)
            [[ $# -ge 2 ]] || usage
            MODEL_SOURCE="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ "$VARIANT" == "full" || "$VARIANT" == "update" ]] || usage
[[ -n "$MODEL_MANIFEST" ]] || usage
[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "model manifest is not a real file: $MODEL_MANIFEST"
command -v plutil >/dev/null 2>&1 || fail "plutil is unavailable"

manifest_value() {
    local keypath="$1"
    local expected_type="$2"
    plutil -extract "$keypath" raw -expect "$expected_type" -o - -- "$MODEL_MANIFEST" 2>/dev/null ||
        fail "invalid model manifest field: $keypath"
}

[[ "$(manifest_value schema integer)" == "1" ]] ||
    fail "model manifest schema must be 1"
[[ "$(manifest_value id string)" == "$MODEL_ID" ]] ||
    fail "model manifest id must be $MODEL_ID"
[[ "$(manifest_value files array)" == "4" ]] ||
    fail "model manifest must contain exactly four files"

if [[ "$VARIANT" == "update" ]]; then
    [[ -n "$RESOURCES" && -z "$MODEL_SOURCE" ]] || usage
    [[ -d "$RESOURCES" && ! -L "$RESOURCES" ]] ||
        fail "Resources is not a real directory: $RESOURCES"
    MODEL_ROOT="$RESOURCES/models"
    [[ ! -e "$MODEL_ROOT" && ! -L "$MODEL_ROOT" ]] ||
        fail "update variant must not contain Resources/models, even when empty"
    echo "PTT2me update model layout is valid: $RESOURCES"
    exit 0
fi

if [[ -n "$MODEL_SOURCE" && -z "$RESOURCES" ]]; then
    MODEL_DIRECTORY="$MODEL_SOURCE"
elif [[ -n "$RESOURCES" && -z "$MODEL_SOURCE" ]]; then
    [[ -d "$RESOURCES" && ! -L "$RESOURCES" ]] ||
        fail "Resources is not a real directory: $RESOURCES"
    MODEL_DIRECTORY="$RESOURCES/models/$MODEL_ID"
else
    usage
fi
[[ -d "$MODEL_DIRECTORY" && ! -L "$MODEL_DIRECTORY" ]] ||
    fail "full variant model directory is not a real directory: $MODEL_DIRECTORY"

ENTRY_COUNT="$(find "$MODEL_DIRECTORY" -mindepth 1 -maxdepth 1 -exec printf x \; | wc -c | tr -d ' ')"
[[ "$ENTRY_COUNT" == "4" ]] ||
    fail "full model directory must contain exactly four entries"

SEEN=""
for index in 0 1 2 3; do
    name="$(manifest_value "files.$index.name" string)"
    size="$(manifest_value "files.$index.size" integer)"
    sha256="$(manifest_value "files.$index.sha256" string)"

    case "$name" in
        encoder.int8.onnx | decoder.onnx | joiner.onnx | tokens.txt) ;;
        *) fail "model manifest contains an unexpected filename: $name" ;;
    esac
    case "|$SEEN|" in
        *"|$name|"*) fail "model manifest contains a duplicate filename: $name" ;;
    esac
    SEEN="${SEEN:+$SEEN|}$name"
    [[ "$size" =~ ^[1-9][0-9]*$ ]] ||
        fail "model manifest contains an invalid size for $name"
    [[ "$sha256" =~ ^[0-9a-f]{64}$ ]] ||
        fail "model manifest contains an invalid SHA-256 for $name"

    path="$MODEL_DIRECTORY/$name"
    [[ -f "$path" && ! -L "$path" ]] || fail "model entry is not a real file: $name"
    [[ ! -x "$path" ]] || fail "model entry is executable: $name"
    actual_size="$(stat -f '%z' "$path")" || fail "could not inspect model size: $name"
    [[ "$actual_size" == "$size" ]] ||
        fail "model size mismatch for $name: expected $size, got $actual_size"
    actual_sha256="$(shasum -a 256 "$path" | awk '{print $1}')" ||
        fail "could not hash model file: $name"
    [[ "$actual_sha256" == "$sha256" ]] || fail "model SHA-256 mismatch for $name"
done

for required in encoder.int8.onnx decoder.onnx joiner.onnx tokens.txt; do
    case "|$SEEN|" in
        *"|$required|"*) ;;
        *) fail "model manifest is missing $required" ;;
    esac
done

echo "PTT2me full model layout is valid: $MODEL_DIRECTORY"
