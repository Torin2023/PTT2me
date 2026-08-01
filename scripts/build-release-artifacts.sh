#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly TARGET="aarch64-apple-darwin"
readonly MODEL_ID="gigaam-v3-rnnt-v1"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

fail() {
    echo "PTT2me release artifact build failed: $*" >&2
    exit 1
}

usage() {
    fail "usage: $0 --version X.Y.Z --build YYYYMMDDHHMM --source-commit COMMIT --model-manifest PATH --model-source PATH --public-key PATH --private-key PATH --published-at YYYY-MM-DDTHH:MM:SSZ --output-dir PATH"
}

VERSION=""
BUILD=""
SOURCE_COMMIT=""
MODEL_MANIFEST=""
MODEL_SOURCE=""
PUBLIC_KEY=""
PRIVATE_KEY=""
PUBLISHED_AT=""
OUTPUT_DIR=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -ge 2 ]] || usage
            VERSION="$2"
            shift 2
            ;;
        --build)
            [[ $# -ge 2 ]] || usage
            BUILD="$2"
            shift 2
            ;;
        --source-commit)
            [[ $# -ge 2 ]] || usage
            SOURCE_COMMIT="$2"
            shift 2
            ;;
        --model-manifest)
            [[ $# -ge 2 ]] || usage
            MODEL_MANIFEST="$2"
            shift 2
            ;;
        --model-source)
            [[ $# -ge 2 ]] || usage
            MODEL_SOURCE="$2"
            shift 2
            ;;
        --public-key)
            [[ $# -ge 2 ]] || usage
            PUBLIC_KEY="$2"
            shift 2
            ;;
        --private-key)
            [[ $# -ge 2 ]] || usage
            PRIVATE_KEY="$2"
            shift 2
            ;;
        --published-at)
            [[ $# -ge 2 ]] || usage
            PUBLISHED_AT="$2"
            shift 2
            ;;
        --output-dir)
            [[ $# -ge 2 ]] || usage
            OUTPUT_DIR="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ -n "$VERSION" && -n "$BUILD" && -n "$SOURCE_COMMIT" ]] || usage
[[ -n "$MODEL_MANIFEST" && -n "$MODEL_SOURCE" ]] || usage
[[ -n "$PUBLIC_KEY" && -n "$PRIVATE_KEY" ]] || usage
[[ -n "$PUBLISHED_AT" && -n "$OUTPUT_DIR" ]] || usage

[[ "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ||
    fail "version must be a canonical stable semantic version"
[[ "$BUILD" =~ ^[0-9]{12}$ ]] || fail "build must be a 12-digit UTC calendar minute"
PARSED_BUILD="$(/bin/date -u -j -f '%Y%m%d%H%M' "$BUILD" '+%Y%m%d%H%M' 2>/dev/null)" ||
    fail "build must be a valid 12-digit UTC calendar minute"
[[ "$PARSED_BUILD" == "$BUILD" ]] || fail "build must be a valid UTC calendar minute"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail "source commit must be exactly 40 lowercase hexadecimal characters"
[[ "$PUBLISHED_AT" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] ||
    fail "published-at must be an exact UTC timestamp"

[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "production model manifest is required"
[[ -d "$MODEL_SOURCE" && ! -L "$MODEL_SOURCE" ]] ||
    fail "production model source directory is required"
[[ -f "$PUBLIC_KEY" && ! -L "$PUBLIC_KEY" ]] ||
    fail "production public key is required (--public-key PATH)"
[[ -f "$PRIVATE_KEY" && ! -L "$PRIVATE_KEY" ]] ||
    fail "production private key is required outside Git (--private-key PATH)"
[[ -d "$OUTPUT_DIR" && ! -L "$OUTPUT_DIR" ]] ||
    fail "output directory is not a real directory"

MODEL_MANIFEST_DIR="$(cd -- "$(dirname -- "$MODEL_MANIFEST")" && pwd -P)"
MODEL_MANIFEST="$MODEL_MANIFEST_DIR/$(basename -- "$MODEL_MANIFEST")"
MODEL_SOURCE="$(cd -- "$MODEL_SOURCE" && pwd -P)"
PUBLIC_KEY_DIR="$(cd -- "$(dirname -- "$PUBLIC_KEY")" && pwd -P)"
PUBLIC_KEY="$PUBLIC_KEY_DIR/$(basename -- "$PUBLIC_KEY")"
PRIVATE_KEY_DIR="$(cd -- "$(dirname -- "$PRIVATE_KEY")" && pwd -P)"
PRIVATE_KEY="$PRIVATE_KEY_DIR/$(basename -- "$PRIVATE_KEY")"
OUTPUT_DIR="$(cd -- "$OUTPUT_DIR" && pwd -P)"
readonly MODEL_MANIFEST MODEL_SOURCE PUBLIC_KEY PRIVATE_KEY OUTPUT_DIR

case "$PRIVATE_KEY" in
    "$REPO_ROOT" | "$REPO_ROOT"/*)
        fail "production private key must be stored outside the Git repository"
        ;;
esac

cd -- "$REPO_ROOT"
"$SCRIPT_DIR/check-production-model-manifest.sh" "$MODEL_MANIFEST"
cmp -s "$REPO_ROOT/models/manifests/$MODEL_ID.json" "$MODEL_MANIFEST" ||
    fail "model manifest must match the committed exact bytes"
"$SCRIPT_DIR/check-model-variant.sh" \
    --variant full \
    --model-manifest "$MODEL_MANIFEST" \
    --model-source "$MODEL_SOURCE"

CARGO_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
LOCK_VERSION="$(awk -F '"' '
    $0 == "name = \"ptt2me\"" { package = 1; next }
    package && /^version = "/ { print $2; exit }
' Cargo.lock)"
[[ "$VERSION" == "$CARGO_VERSION" && "$VERSION" == "$LOCK_VERSION" ]] ||
    fail "release version must match Cargo.toml and Cargo.lock"

HEAD_COMMIT="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" ||
    fail "could not resolve exact git HEAD"
[[ "$SOURCE_COMMIT" == "$HEAD_COMMIT" ]] ||
    fail "source commit must equal exact git HEAD"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] ||
    fail "release source tree must be clean"
[[ "$(uname -m)" == "arm64" ]] || fail "an Apple Silicon (arm64) Mac is required"

FULL_DMG_NAME="$PRODUCT-$VERSION-full-macos-arm64.dmg"
UPDATE_DMG_NAME="$PRODUCT-$VERSION-update-macos-arm64.dmg"
SIGNED_MANIFEST_NAME="$PRODUCT-$VERSION-signed-update-manifest.json"
for output_name in \
    "$FULL_DMG_NAME" "$FULL_DMG_NAME.sha256" \
    "$UPDATE_DMG_NAME" "$UPDATE_DMG_NAME.sha256" \
    "$SIGNED_MANIFEST_NAME"; do
    [[ ! -e "$OUTPUT_DIR/$output_name" && ! -L "$OUTPUT_DIR/$output_name" ]] ||
        fail "release output already exists: $OUTPUT_DIR/$output_name"
done

TEMP_ROOT="$(mktemp -d "$OUTPUT_DIR/.ptt2me-$VERSION-release.XXXXXX")" ||
    fail "could not create private release workspace"
chmod 700 "$TEMP_ROOT"
PUBLISHED_PATHS=()
BUILD_SUCCEEDED=false
cleanup() {
    if [[ "$BUILD_SUCCEEDED" != true ]]; then
        for path in "${PUBLISHED_PATHS[@]}"; do
            rm -f "$path"
        done
    fi
    rm -rf "$TEMP_ROOT"
}
trap cleanup EXIT

FULL_APP="$TEMP_ROOT/full/$PRODUCT.app"
UPDATE_APP="$TEMP_ROOT/update/$PRODUCT.app"
mkdir -p "$(dirname -- "$FULL_APP")" "$(dirname -- "$UPDATE_APP")"

"$SCRIPT_DIR/build-app.sh" \
    --variant full \
    --model-manifest "$MODEL_MANIFEST" \
    --model-source "$MODEL_SOURCE" \
    --version "$VERSION" \
    --build "$BUILD" \
    --source-commit "$SOURCE_COMMIT" \
    --output "$FULL_APP"

/usr/bin/ditto "$FULL_APP" "$UPDATE_APP"
rm -rf "$UPDATE_APP/Contents/Resources/models"
/usr/libexec/PlistBuddy -c \
    "Set :PTT2meDistributionVariant update" "$UPDATE_APP/Contents/Info.plist"
codesign --force --sign - "$UPDATE_APP"

"$SCRIPT_DIR/check-bundle.sh" \
    --variant full --model-manifest "$MODEL_MANIFEST" "$FULL_APP"
"$SCRIPT_DIR/check-bundle.sh" \
    --variant update --model-manifest "$MODEL_MANIFEST" "$UPDATE_APP"

for relative in \
    "Contents/MacOS/$PRODUCT" \
    "Contents/Frameworks/libsherpa-onnx-c-api.dylib" \
    "Contents/Frameworks/libonnxruntime.1.17.1.dylib"; do
    cmp -s "$FULL_APP/$relative" "$UPDATE_APP/$relative" ||
        fail "Full and Update payloads differ: $relative"
done
for key in \
    CFBundleIdentifier CFBundleShortVersionString CFBundleVersion \
    PTT2meSourceCommit LSMinimumSystemVersion; do
    FULL_VALUE="$(/usr/libexec/PlistBuddy -c "Print :$key" "$FULL_APP/Contents/Info.plist")"
    UPDATE_VALUE="$(/usr/libexec/PlistBuddy -c "Print :$key" "$UPDATE_APP/Contents/Info.plist")"
    [[ "$FULL_VALUE" == "$UPDATE_VALUE" ]] || fail "bundle identity differs for $key"
done
[[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleVersion' "$FULL_APP/Contents/Info.plist")" == "$BUILD" ]] ||
    fail "bundle build does not match the release input"
[[ "$(/usr/libexec/PlistBuddy -c 'Print :PTT2meSourceCommit' "$FULL_APP/Contents/Info.plist")" == "$SOURCE_COMMIT" ]] ||
    fail "bundle source commit does not match the release input"

FULL_DMG="$TEMP_ROOT/$FULL_DMG_NAME"
UPDATE_DMG="$TEMP_ROOT/$UPDATE_DMG_NAME"
"$SCRIPT_DIR/build-dmg.sh" \
    --variant full --model-manifest "$MODEL_MANIFEST" \
    --app "$FULL_APP" --output "$FULL_DMG"
"$SCRIPT_DIR/build-dmg.sh" \
    --variant update --model-manifest "$MODEL_MANIFEST" \
    --app "$UPDATE_APP" --output "$UPDATE_DMG"

FULL_SHA="$(shasum -a 256 "$FULL_DMG" | awk '{print $1}')"
UPDATE_SHA="$(shasum -a 256 "$UPDATE_DMG" | awk '{print $1}')"
MODEL_SHA="$(shasum -a 256 "$MODEL_MANIFEST" | awk '{print $1}')"
FULL_SIZE="$(stat -f '%z' "$FULL_DMG")"
UPDATE_SIZE="$(stat -f '%z' "$UPDATE_DMG")"
PAYLOAD="$TEMP_ROOT/release-payload.json"
SIGNED_MANIFEST="$TEMP_ROOT/$SIGNED_MANIFEST_NAME"
printf '%s\n' \
    "{\"channel\":\"stable\",\"version\":\"$VERSION\",\"build\":$BUILD,\"source_commit\":\"$SOURCE_COMMIT\",\"minimum_macos\":\"13.0\",\"architecture\":\"arm64\",\"required_model\":{\"id\":\"$MODEL_ID\",\"manifest_sha256\":\"$MODEL_SHA\"},\"fresh_install\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v$VERSION/$FULL_DMG_NAME\",\"sha256\":\"$FULL_SHA\",\"size\":$FULL_SIZE},\"application_update\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v$VERSION/$UPDATE_DMG_NAME\",\"sha256\":\"$UPDATE_SHA\",\"size\":$UPDATE_SIZE},\"published_at\":\"$PUBLISHED_AT\"}" \
    >"$PAYLOAD"

"$SCRIPT_DIR/sign-update-manifest.sh" "$PRIVATE_KEY" "$PAYLOAD" "$SIGNED_MANIFEST"
PTT2ME_MANIFEST_VERIFIER="$FULL_APP/Contents/MacOS/$PRODUCT" \
    "$SCRIPT_DIR/validate-update-manifest.sh" \
    "$PUBLIC_KEY" "$SIGNED_MANIFEST" "$FULL_DMG" "$UPDATE_DMG" "$MODEL_MANIFEST"

for source in \
    "$FULL_DMG" "$FULL_DMG.sha256" \
    "$UPDATE_DMG" "$UPDATE_DMG.sha256" \
    "$SIGNED_MANIFEST"; do
    destination="$OUTPUT_DIR/$(basename -- "$source")"
    ln "$source" "$destination" || fail "could not publish release output without overwrite"
    PUBLISHED_PATHS+=("$destination")
done
BUILD_SUCCEEDED=true

echo "Built deterministic Full and Update artifacts for $VERSION ($SOURCE_COMMIT)"
echo "Release outputs: $OUTPUT_DIR"
