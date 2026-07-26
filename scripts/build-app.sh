#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "PTT2me build requires an Apple Silicon (arm64) Mac." >&2
    exit 1
fi

TARGET="aarch64-apple-darwin"
PRODUCT="PTT2me"
APP="dist/$PRODUCT.app"
CONTENTS="$APP/Contents"
MACOS="$CONTENTS/MacOS"
RESOURCES="$CONTENTS/Resources"
FRAMEWORKS="$CONTENTS/Frameworks"
MODEL_SOURCE="vendor/models/gigaam-v3-rnnt"
MODEL_DESTINATION="$RESOURCES/models/gigaam-v3-rnnt"
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

for model_file in encoder.int8.onnx decoder.onnx joiner.onnx tokens.txt; do
    require_nonempty_file "$MODEL_SOURCE/$model_file"
done
for license_file in \
    GIGAAM-MIT.txt \
    SHERPA-RS-MIT.txt \
    SHERPA-ONNX-APACHE-2.0.txt \
    ONNXRUNTIME-MIT.txt \
    ONNXRUNTIME-NOTICES.txt; do
    require_nonempty_file "$LICENSE_SOURCE/$license_file"
done

VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$VERSION" ]] || {
    echo "Could not read package version from Cargo.toml" >&2
    exit 1
}
BUILD_VERSION="$(date -u +%Y%m%d%H%M)"

export MACOSX_DEPLOYMENT_TARGET=13.0
cargo build --release --target "$TARGET"

EXECUTABLE_SOURCE="$RELEASE_DIR/ptt2me"
require_nonempty_file "$EXECUTABLE_SOURCE"
require_nonempty_file "$RELEASE_DIR/$SHERPA_DYLIB"
require_nonempty_file "$RELEASE_DIR/$ONNX_DYLIB"

rm -rf "$APP"
mkdir -p "$MACOS" "$MODEL_DESTINATION" "$LICENSE_DESTINATION" "$FRAMEWORKS"

install -m 755 "$EXECUTABLE_SOURCE" "$MACOS/$PRODUCT"
for model_file in encoder.int8.onnx decoder.onnx joiner.onnx tokens.txt; do
    install -m 644 "$MODEL_SOURCE/$model_file" "$MODEL_DESTINATION/$model_file"
done
for license_file in \
    GIGAAM-MIT.txt \
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
/usr/libexec/PlistBuddy -c "Add :LSMinimumSystemVersion string 13.0" "$PLIST"
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :NSMicrophoneUsageDescription string PTT2me uses the microphone only while you hold Fn to dictate text." \
    "$PLIST"

codesign --force --sign - "$FRAMEWORKS/$ONNX_DYLIB"
codesign --force --sign - "$FRAMEWORKS/$SHERPA_DYLIB"
codesign --force --sign - "$MACOS/$PRODUCT"
codesign --force --sign - "$APP"

scripts/check-bundle.sh "$APP"
echo "Built $APP"
