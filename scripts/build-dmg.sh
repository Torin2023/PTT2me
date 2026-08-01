#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT
cd -- "$REPO_ROOT"

fail() {
    echo "PTT2me DMG build failed: $*" >&2
    exit 1
}

usage() {
    fail "usage: $0 --variant full|update --model-manifest PATH --app APP_PATH --output DMG_PATH"
}

VARIANT=""
MODEL_MANIFEST=""
APP_PATH=""
DMG_PATH=""
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
        --app)
            [[ $# -ge 2 ]] || usage
            APP_PATH="$2"
            shift 2
            ;;
        --output)
            [[ $# -ge 2 ]] || usage
            DMG_PATH="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ "$VARIANT" == "full" || "$VARIANT" == "update" ]] || usage
[[ -n "$MODEL_MANIFEST" && -n "$APP_PATH" && -n "$DMG_PATH" ]] || usage
[[ -d "$APP_PATH" && ! -L "$APP_PATH" ]] ||
    fail "app bundle is not a real directory: $APP_PATH"
COMMITTED_MODEL_MANIFEST="$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"
[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "model manifest is not a real file: $MODEL_MANIFEST"
cmp -s "$COMMITTED_MODEL_MANIFEST" "$MODEL_MANIFEST" ||
    fail "--model-manifest must match the committed exact bytes"

cleanup() {
    if [[ "${DMG_MOUNTED:-false}" == true ]]; then
        hdiutil detach "$MOUNT_DIR" -quiet || true
    fi
    [[ -z "${TEMP_ROOT:-}" ]] || rm -rf "$TEMP_ROOT"
    [[ -z "${TEMP_DMG:-}" ]] || rm -f "$TEMP_DMG"
    [[ -z "${TEMP_CHECKSUM:-}" ]] || rm -f "$TEMP_CHECKSUM"
}
trap cleanup EXIT

[[ "$(uname -m)" == "arm64" ]] ||
    fail "an Apple Silicon (arm64) Mac is required"
for command in hdiutil shasum mktemp; do
    command -v "$command" >/dev/null 2>&1 ||
        fail "required command is unavailable: $command"
done

[[ "$DMG_PATH" == *.dmg ]] || fail "--output must end in .dmg"
DMG_OUTPUT_DIR="$(cd -- "$(dirname -- "$DMG_PATH")" 2>/dev/null && pwd -P)" ||
    fail "output directory does not exist: $(dirname -- "$DMG_PATH")"
DMG_NAME="$(basename -- "$DMG_PATH")"
DMG_PATH="$DMG_OUTPUT_DIR/$DMG_NAME"
readonly DMG_NAME DMG_PATH DMG_OUTPUT_DIR
readonly CHECKSUM_PATH="$DMG_PATH.sha256"

"$SCRIPT_DIR/check-bundle.sh" \
    --variant "$VARIANT" \
    --model-manifest "$MODEL_MANIFEST" \
    "$APP_PATH"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-dmg.XXXXXX")"
readonly TEMP_ROOT
readonly STAGING_DIR="$TEMP_ROOT/staging"
readonly MOUNT_DIR="$TEMP_ROOT/mount"
TEMP_DMG="$DMG_OUTPUT_DIR/.$DMG_NAME.tmp.dmg"
TEMP_CHECKSUM="$DMG_OUTPUT_DIR/.$DMG_NAME.sha256.tmp"

mkdir -p "$STAGING_DIR" "$MOUNT_DIR"
cp -R "$APP_PATH" "$STAGING_DIR/$PRODUCT.app"
ln -s /Applications "$STAGING_DIR/Applications"

hdiutil create \
    -volname "$PRODUCT" \
    -srcfolder "$STAGING_DIR" \
    -format UDZO \
    -ov \
    "$TEMP_DMG" >/dev/null

hdiutil attach \
    "$TEMP_DMG" \
    -readonly \
    -nobrowse \
    -noautoopen \
    -mountpoint "$MOUNT_DIR" >/dev/null
DMG_MOUNTED=true

[[ -d "$MOUNT_DIR/$PRODUCT.app" ]] || fail "mounted image has no app bundle"
[[ -L "$MOUNT_DIR/Applications" ]] || fail "mounted image has no Applications link"
[[ "$(readlink "$MOUNT_DIR/Applications")" == "/Applications" ]] ||
    fail "Applications link has the wrong target"
[[ "$(find "$MOUNT_DIR" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" == "2" ]] ||
    fail "mounted image contains unexpected root items"
"$SCRIPT_DIR/check-bundle.sh" \
    --variant "$VARIANT" \
    --model-manifest "$MODEL_MANIFEST" \
    "$MOUNT_DIR/$PRODUCT.app"

hdiutil detach "$MOUNT_DIR" -quiet
DMG_MOUNTED=false

CHECKSUM="$(shasum -a 256 "$TEMP_DMG" | awk '{print $1}')"
printf '%s  %s\n' "$CHECKSUM" "$DMG_NAME" >"$TEMP_CHECKSUM"
mv -f "$TEMP_DMG" "$DMG_PATH"
mv -f "$TEMP_CHECKSUM" "$CHECKSUM_PATH"

echo "Built $DMG_PATH"
echo "SHA-256: $CHECKSUM"
