#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
cd -- "$REPO_ROOT"

fail() {
    echo "PTT2me app build failed: $*" >&2
    exit 1
}

usage() {
    fail "usage: $0 --variant full|update --model-manifest PATH [--model-source PATH] [--version X.Y.Z --build YYYYMMDDHHMM --source-commit COMMIT --output APP_PATH]"
}

VARIANT=""
MODEL_MANIFEST=""
MODEL_SOURCE=""
EXPLICIT_VERSION=""
EXPLICIT_BUILD=""
EXPLICIT_SOURCE_COMMIT=""
OUTPUT_APP=""
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
        --model-source)
            [[ $# -ge 2 ]] || usage
            MODEL_SOURCE="$2"
            shift 2
            ;;
        --version)
            [[ $# -ge 2 ]] || usage
            EXPLICIT_VERSION="$2"
            shift 2
            ;;
        --build)
            [[ $# -ge 2 ]] || usage
            EXPLICIT_BUILD="$2"
            shift 2
            ;;
        --source-commit)
            [[ $# -ge 2 ]] || usage
            EXPLICIT_SOURCE_COMMIT="$2"
            shift 2
            ;;
        --output)
            [[ $# -ge 2 ]] || usage
            OUTPUT_APP="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ "$VARIANT" == "full" || "$VARIANT" == "update" ]] || usage
[[ -n "$MODEL_MANIFEST" ]] || usage
if [[ "$VARIANT" == "full" && -z "$MODEL_SOURCE" ]]; then
    fail "full variant requires --model-source"
fi
if [[ "$VARIANT" == "update" && -n "$MODEL_SOURCE" ]]; then
    fail "update variant does not accept --model-source"
fi

COMMITTED_MODEL_MANIFEST="$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"
[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "model manifest is not a real file: $MODEL_MANIFEST"
"$SCRIPT_DIR/check-production-model-manifest.sh" "$MODEL_MANIFEST"
cmp -s "$COMMITTED_MODEL_MANIFEST" "$MODEL_MANIFEST" ||
    fail "--model-manifest must match the committed exact bytes"
if [[ "$VARIANT" == "full" ]]; then
    "$SCRIPT_DIR/check-model-variant.sh" \
        --variant full \
        --model-manifest "$MODEL_MANIFEST" \
        --model-source "$MODEL_SOURCE"
fi

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "PTT2me build requires an Apple Silicon (arm64) Mac." >&2
    exit 1
fi

TARGET="aarch64-apple-darwin"
PRODUCT="PTT2me"
if [[ -n "$OUTPUT_APP" ]]; then
    [[ "$OUTPUT_APP" == *.app ]] || fail "--output must end in .app"
    OUTPUT_PARENT="$(cd -- "$(dirname -- "$OUTPUT_APP")" 2>/dev/null && pwd -P)" ||
        fail "output parent does not exist: $(dirname -- "$OUTPUT_APP")"
    APP="$OUTPUT_PARENT/$(basename -- "$OUTPUT_APP")"
else
    APP="$REPO_ROOT/dist/$PRODUCT.app"
fi
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
FRAMEWORKS="$CONTENTS/Frameworks"
MODEL_DESTINATION="$RESOURCES/models/gigaam-v3-rnnt-v1"
LICENSE_SOURCE="licenses"
LICENSE_DESTINATION="$RESOURCES/licenses"
RELEASE_DIR="target/$TARGET/release"
SHERPA_DYLIB="libsherpa-onnx-c-api.dylib"
ONNX_DYLIB="libonnxruntime.1.17.1.dylib"

require_nonempty_file() {
    [[ -s "$1" ]] || {
        echo "Missing or empty required build asset: $1" >&2
        exit 1
    }
}

for license_file in \
    GIGAAM-MIT.txt \
    RTRB-MIT.txt \
    SHERPA-RS-MIT.txt \
    SHERPA-ONNX-APACHE-2.0.txt \
    ONNXRUNTIME-MIT.txt \
    ONNXRUNTIME-NOTICES.txt; do
    require_nonempty_file "$LICENSE_SOURCE/$license_file"
done

CARGO_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$CARGO_VERSION" ]] || {
    echo "Could not read package version from Cargo.toml" >&2
    exit 1
}
VERSION="${EXPLICIT_VERSION:-$CARGO_VERSION}"
[[ "$VERSION" == "$CARGO_VERSION" ]] ||
    fail "explicit version must match Cargo.toml ($CARGO_VERSION)"

if [[ -n "$EXPLICIT_BUILD" ]]; then
    [[ "$EXPLICIT_BUILD" =~ ^[0-9]{12}$ ]] ||
        fail "explicit build must be a valid 12-digit UTC calendar minute"
    PARSED_BUILD="$(/bin/date -u -j -f '%Y%m%d%H%M' "$EXPLICIT_BUILD" '+%Y%m%d%H%M' 2>/dev/null)" ||
        fail "explicit build must be a valid 12-digit UTC calendar minute"
    [[ "$PARSED_BUILD" == "$EXPLICIT_BUILD" ]] ||
        fail "explicit build must be a valid 12-digit UTC calendar minute"
    BUILD_VERSION="$EXPLICIT_BUILD"
else
    BUILD_VERSION="$(date -u +%Y%m%d%H%M)"
fi

HEAD_COMMIT="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" ||
    fail "could not resolve exact git HEAD"
SOURCE_COMMIT="${EXPLICIT_SOURCE_COMMIT:-$HEAD_COMMIT}"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail "git HEAD must be exactly 40 lowercase hexadecimal characters"
[[ "$SOURCE_COMMIT" == "$HEAD_COMMIT" ]] ||
    fail "explicit source commit must match exact git HEAD"

export MACOSX_DEPLOYMENT_TARGET=13.0
cargo build --release --target "$TARGET"

EXECUTABLE_SOURCE="$RELEASE_DIR/ptt2me"
require_nonempty_file "$EXECUTABLE_SOURCE"
require_nonempty_file "$RELEASE_DIR/$SHERPA_DYLIB"
require_nonempty_file "$RELEASE_DIR/$ONNX_DYLIB"

if [[ -e "$APP" || -L "$APP" ]]; then
    [[ -z "$OUTPUT_APP" ]] || fail "explicit output path already exists"
    [[ -d "$APP" && ! -L "$APP" ]] || fail "output app path is not a real directory"
    rm -rf "$APP"
fi
mkdir -p "$MACOS" "$LICENSE_DESTINATION" "$FRAMEWORKS"
if [[ "$VARIANT" == "full" ]]; then
    mkdir -p "$MODEL_DESTINATION"
fi

install -m 755 "$EXECUTABLE_SOURCE" "$MACOS/$PRODUCT"
if [[ "$VARIANT" == "full" ]]; then
    for model_file in encoder.int8.onnx decoder.onnx joiner.onnx tokens.txt; do
        install -m 644 "$MODEL_SOURCE/$model_file" "$MODEL_DESTINATION/$model_file"
    done
fi
for license_file in \
    GIGAAM-MIT.txt \
    RTRB-MIT.txt \
    SHERPA-RS-MIT.txt \
    SHERPA-ONNX-APACHE-2.0.txt \
    ONNXRUNTIME-MIT.txt \
    ONNXRUNTIME-NOTICES.txt; do
    install -m 644 "$LICENSE_SOURCE/$license_file" "$LICENSE_DESTINATION/$license_file"
done
install -m 755 "$RELEASE_DIR/$SHERPA_DYLIB" "$FRAMEWORKS/$SHERPA_DYLIB"
install -m 755 "$RELEASE_DIR/$ONNX_DYLIB" "$FRAMEWORKS/$ONNX_DYLIB"

lipo "$MACOS/$PRODUCT" -verify_arch arm64
lipo "$FRAMEWORKS/$SHERPA_DYLIB" -verify_arch arm64
lipo "$FRAMEWORKS/$ONNX_DYLIB" -verify_arch arm64

install_name_tool -id "@rpath/$SHERPA_DYLIB" "$FRAMEWORKS/$SHERPA_DYLIB"
install_name_tool -id "@rpath/$ONNX_DYLIB" "$FRAMEWORKS/$ONNX_DYLIB"

rewrite_dependency() {
    local binary="$1"
    local basename="$2"
    local desired="@rpath/$basename"
    local dependency

    while IFS= read -r dependency; do
        dependency="${dependency#"${dependency%%[![:space:]]*}"}"
        dependency="${dependency%% *}"
        [[ -n "$dependency" ]] || continue
        if [[ "${dependency##*/}" == "$basename" && "$dependency" != "$desired" ]]; then
            install_name_tool -change "$dependency" "$desired" "$binary"
        fi
    done < <(otool -L "$binary" | tail -n +2)
}

rewrite_dependency "$MACOS/$PRODUCT" "$SHERPA_DYLIB"
rewrite_dependency "$MACOS/$PRODUCT" "$ONNX_DYLIB"
rewrite_dependency "$FRAMEWORKS/$SHERPA_DYLIB" "$ONNX_DYLIB"

if ! otool -l "$MACOS/$PRODUCT" |
    awk '$1 == "cmd" && $2 == "LC_RPATH" { getline; getline; print $2 }' |
    grep -Fxq '@executable_path/../Frameworks'; then
    install_name_tool -add_rpath '@executable_path/../Frameworks' "$MACOS/$PRODUCT"
fi

PLIST="$CONTENTS/Info.plist"
plutil -create xml1 "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleDevelopmentRegion string en" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleDisplayName string $PRODUCT" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleExecutable string $PRODUCT" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleIdentifier string com.ptt2me.app" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleInfoDictionaryVersion string 6.0" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleName string $PRODUCT" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundlePackageType string APPL" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleShortVersionString string $VERSION" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :CFBundleVersion string $BUILD_VERSION" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :PTT2meSourceCommit string $SOURCE_COMMIT" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :PTT2meDistributionVariant string $VARIANT" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :LSMinimumSystemVersion string 13.0" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :NSMicrophoneUsageDescription string PTT2me uses the microphone only while you hold Fn to dictate text." \
    "$PLIST"

codesign --force --sign - "$FRAMEWORKS/$ONNX_DYLIB"
codesign --force --sign - "$FRAMEWORKS/$SHERPA_DYLIB"
codesign --force --sign - "$MACOS/$PRODUCT"
codesign --force --sign - "$APP"

scripts/check-bundle.sh \
    --variant "$VARIANT" \
    --model-manifest "$MODEL_MANIFEST" \
    "$APP"
echo "Built $APP ($VARIANT)"
