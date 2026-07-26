#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly TARGET_PLATFORM="macos-arm64"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

fail() {
    echo "PTT2me DMG build failed: $*" >&2
    exit 1
}

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

cd -- "$REPO_ROOT"
VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$VERSION" ]] || fail "could not read version from Cargo.toml"

readonly DMG_NAME="$PRODUCT-$VERSION-$TARGET_PLATFORM.dmg"
readonly DMG_PATH="$REPO_ROOT/dist/$DMG_NAME"
readonly CHECKSUM_PATH="$DMG_PATH.sha256"

"$SCRIPT_DIR/build-app.sh"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-dmg.XXXXXX")"
readonly TEMP_ROOT
readonly STAGING_DIR="$TEMP_ROOT/staging"
readonly MOUNT_DIR="$TEMP_ROOT/mount"
TEMP_DMG="$REPO_ROOT/dist/.$DMG_NAME.tmp.dmg"
TEMP_CHECKSUM="$REPO_ROOT/dist/.$DMG_NAME.sha256.tmp"

mkdir -p "$STAGING_DIR" "$MOUNT_DIR"
cp -R "$REPO_ROOT/dist/$PRODUCT.app" "$STAGING_DIR/$PRODUCT.app"
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
"$SCRIPT_DIR/check-bundle.sh" "$MOUNT_DIR/$PRODUCT.app"

hdiutil detach "$MOUNT_DIR" -quiet
DMG_MOUNTED=false

CHECKSUM="$(shasum -a 256 "$TEMP_DMG" | awk '{print $1}')"
printf '%s  %s\n' "$CHECKSUM" "$DMG_NAME" >"$TEMP_CHECKSUM"
mv -f "$TEMP_DMG" "$DMG_PATH"
mv -f "$TEMP_CHECKSUM" "$CHECKSUM_PATH"

echo "Built $DMG_PATH"
echo "SHA-256: $CHECKSUM"
