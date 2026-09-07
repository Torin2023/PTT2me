#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
CHECK_MODEL_VARIANT="$REPO_ROOT/scripts/check-model-variant.sh"
CHECK_PRODUCTION_MANIFEST="$REPO_ROOT/scripts/check-production-model-manifest.sh"
CURRENT_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' "$REPO_ROOT/Cargo.toml")"
[[ "$CURRENT_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] || {
    echo "could not read canonical package version from Cargo.toml" >&2
    exit 1
}

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

expect_exact_architecture() {
    local expected="$1"
    local path="$2"
    local actual
    actual="$(lipo -archs "$path")"
    [[ "$actual" == "$expected" ]] || {
        echo "expected $path architecture '$expected', got '$actual'" >&2
        exit 1
    }
}

compile_macho_fixture() {
    local architecture="$1"
    local output_directory="$2"
    mkdir -p "$output_directory"

    printf '%s\n' 'int onnx_symbol(void) { return 0; }' |
        xcrun clang \
            -arch "$architecture" \
            -mmacosx-version-min=13.0 \
            -x c - \
            -dynamiclib \
            -Wl,-install_name,@rpath/libonnxruntime.1.17.1.dylib \
            -o "$output_directory/libonnxruntime.1.17.1.dylib"
    printf '%s\n' \
        'extern int onnx_symbol(void);' \
        'int sherpa_symbol(void) { return onnx_symbol(); }' |
        xcrun clang \
            -arch "$architecture" \
            -mmacosx-version-min=13.0 \
            -x c - \
            -x none \
            -dynamiclib \
            "$output_directory/libonnxruntime.1.17.1.dylib" \
            -Wl,-install_name,@rpath/libsherpa-onnx-c-api.dylib \
            -o "$output_directory/libsherpa-onnx-c-api.dylib"
    printf '%s\n' \
        'extern int onnx_symbol(void);' \
        'extern int sherpa_symbol(void);' \
        'int main(void) { return onnx_symbol() + sherpa_symbol(); }' |
        xcrun clang \
            -arch "$architecture" \
            -mmacosx-version-min=13.0 \
            -x c - \
            -x none \
            "$output_directory/libsherpa-onnx-c-api.dylib" \
            "$output_directory/libonnxruntime.1.17.1.dylib" \
            -Wl,-rpath,@executable_path/../Frameworks \
            -o "$output_directory/PTT2me"
}

create_update_bundle_fixture() {
    local app="$1"
    local executable="$2"
    local sherpa_dylib="$3"
    local onnx_dylib="$4"
    local contents="$app/Contents"
    local plist="$contents/Info.plist"

    mkdir -p \
        "$contents/MacOS" \
        "$contents/Frameworks" \
        "$contents/Resources"
    install -m 755 "$executable" "$contents/MacOS/PTT2me"
    install -m 755 "$sherpa_dylib" \
        "$contents/Frameworks/libsherpa-onnx-c-api.dylib"
    install -m 755 "$onnx_dylib" \
        "$contents/Frameworks/libonnxruntime.1.17.1.dylib"
    install -m 644 "$REPO_ROOT/assets/PTT2me.icns" \
        "$contents/Resources/PTT2me.icns"

    plutil -create xml1 "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :CFBundleIdentifier string com.ptt2me.app" "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :CFBundleIconFile string PTT2me.icns" "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :CFBundleShortVersionString string $CURRENT_VERSION" "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :CFBundleVersion string 202609061200" "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :PTT2meSourceCommit string 0000000000000000000000000000000000000000" \
        "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :PTT2meDistributionVariant string update" "$plist"
    /usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$plist"
    /usr/libexec/PlistBuddy -c \
        "Add :LSMinimumSystemVersion string 13.0" "$plist"

    codesign --force --sign - \
        "$contents/Frameworks/libonnxruntime.1.17.1.dylib"
    codesign --force --sign - \
        "$contents/Frameworks/libsherpa-onnx-c-api.dylib"
    codesign --force --sign - "$contents/MacOS/PTT2me"
    codesign --force --sign - "$app"
}

FAKE_TOOLS="$TEMP_ROOT/fake-tools"
mkdir "$FAKE_TOOLS"
printf '%s\n' \
    '#!/bin/bash' \
    '[[ "$1" == "-m" ]] || exit 91' \
    'printf "%s\\n" arm64' >"$FAKE_TOOLS/uname"
printf '%s\n' \
    '#!/bin/bash' \
    '[[ "$*" == "rev-parse --verify HEAD^{commit}" ]] || exit 92' \
    'printf "%s\\n" ABC' >"$FAKE_TOOLS/git"
printf '%s\n' \
    '#!/bin/bash' \
    'echo "cargo must not run before source commit validation" >&2' \
    'exit 93' >"$FAKE_TOOLS/cargo"
printf '%s\n' \
    '#!/bin/bash' \
    '[[ "$*" == *"-archs"* ]] && printf "%s\\n" arm64' \
    'exit 0' >"$FAKE_TOOLS/lipo"
printf '%s\n' \
    '#!/bin/bash' \
    'echo "otool must not run before source commit validation" >&2' \
    'exit 94' >"$FAKE_TOOLS/otool"
chmod 755 "$FAKE_TOOLS"/*

ARM64_FIXTURE="$TEMP_ROOT/macho-arm64"
X86_64_FIXTURE="$TEMP_ROOT/macho-x86_64"
UNIVERSAL_FIXTURE="$TEMP_ROOT/macho-universal"
compile_macho_fixture arm64 "$ARM64_FIXTURE"
compile_macho_fixture x86_64 "$X86_64_FIXTURE"
mkdir "$UNIVERSAL_FIXTURE"
for macho_name in \
    PTT2me \
    libsherpa-onnx-c-api.dylib \
    libonnxruntime.1.17.1.dylib; do
    lipo -create \
        "$ARM64_FIXTURE/$macho_name" \
        "$X86_64_FIXTURE/$macho_name" \
        -output "$UNIVERSAL_FIXTURE/$macho_name"
    lipo "$UNIVERSAL_FIXTURE/$macho_name" -verify_arch arm64 >/dev/null 2>&1 || {
        echo "former arm64-presence check must accept universal $macho_name" >&2
        exit 1
    }
done

THIN_BUNDLE="$TEMP_ROOT/thin-arm64.app"
create_update_bundle_fixture \
    "$THIN_BUNDLE" \
    "$ARM64_FIXTURE/PTT2me" \
    "$ARM64_FIXTURE/libsherpa-onnx-c-api.dylib" \
    "$ARM64_FIXTURE/libonnxruntime.1.17.1.dylib"
"$REPO_ROOT/scripts/check-bundle.sh" \
    --variant update \
    --model-manifest "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" \
    "$THIN_BUNDLE"

for universal_name in \
    Contents/MacOS/PTT2me \
    Contents/Frameworks/libsherpa-onnx-c-api.dylib \
    Contents/Frameworks/libonnxruntime.1.17.1.dylib; do
    UNIVERSAL_BUNDLE="$TEMP_ROOT/universal-$(basename "$universal_name").app"
    case "$universal_name" in
        Contents/MacOS/PTT2me)
            create_update_bundle_fixture \
                "$UNIVERSAL_BUNDLE" \
                "$UNIVERSAL_FIXTURE/PTT2me" \
                "$ARM64_FIXTURE/libsherpa-onnx-c-api.dylib" \
                "$ARM64_FIXTURE/libonnxruntime.1.17.1.dylib"
            ;;
        Contents/Frameworks/libsherpa-onnx-c-api.dylib)
            create_update_bundle_fixture \
                "$UNIVERSAL_BUNDLE" \
                "$ARM64_FIXTURE/PTT2me" \
                "$UNIVERSAL_FIXTURE/libsherpa-onnx-c-api.dylib" \
                "$ARM64_FIXTURE/libonnxruntime.1.17.1.dylib"
            ;;
        Contents/Frameworks/libonnxruntime.1.17.1.dylib)
            create_update_bundle_fixture \
                "$UNIVERSAL_BUNDLE" \
                "$ARM64_FIXTURE/PTT2me" \
                "$ARM64_FIXTURE/libsherpa-onnx-c-api.dylib" \
                "$UNIVERSAL_FIXTURE/libonnxruntime.1.17.1.dylib"
            ;;
    esac
    expect_failure_containing \
        "$universal_name architectures must be exactly arm64" \
        "$REPO_ROOT/scripts/check-bundle.sh" \
        --variant update \
        --model-manifest "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" \
        "$UNIVERSAL_BUNDLE"
done

BUILD_FIXTURE_REPO="$TEMP_ROOT/build-fixture-repo"
mkdir -p \
    "$BUILD_FIXTURE_REPO/scripts" \
    "$BUILD_FIXTURE_REPO/models/manifests" \
    "$BUILD_FIXTURE_REPO/assets" \
    "$BUILD_FIXTURE_REPO/licenses" \
    "$BUILD_FIXTURE_REPO/fake-tools"
cp "$REPO_ROOT/Cargo.toml" "$BUILD_FIXTURE_REPO/Cargo.toml"
cp "$REPO_ROOT/assets/PTT2me.icns" "$BUILD_FIXTURE_REPO/assets/PTT2me.icns"
cp "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" \
    "$BUILD_FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json"
cp "$REPO_ROOT"/licenses/* "$BUILD_FIXTURE_REPO/licenses/"
for fixture_script in \
    build-app.sh \
    check-app-icon.sh \
    check-bundle.sh \
    check-model-variant.sh \
    check-production-model-manifest.sh; do
    cp "$REPO_ROOT/scripts/$fixture_script" "$BUILD_FIXTURE_REPO/scripts/$fixture_script"
done
printf '%s\n' \
    '#!/bin/bash' \
    'set -euo pipefail' \
    '[[ "$*" == "build --release --target aarch64-apple-darwin" ]] || exit 91' \
    'release="$PWD/target/aarch64-apple-darwin/release"' \
    'mkdir -p "$release"' \
    'cp "$FIXTURE_BIN_DIR/PTT2me" "$release/ptt2me"' \
    'cp "$FIXTURE_BIN_DIR/libsherpa-onnx-c-api.dylib" "$release/"' \
    'cp "$FIXTURE_BIN_DIR/libonnxruntime.1.17.1.dylib" "$release/"' \
    >"$BUILD_FIXTURE_REPO/fake-tools/cargo"
printf '%s\n' \
    '#!/bin/bash' \
    '[[ "$*" == "rev-parse --verify HEAD^{commit}" ]] || exit 92' \
    'printf "%s\\n" 1111111111111111111111111111111111111111' \
    >"$BUILD_FIXTURE_REPO/fake-tools/git"
chmod 755 "$BUILD_FIXTURE_REPO/fake-tools"/*

UNIVERSAL_LIBRARY_INPUT="$TEMP_ROOT/universal-library-input"
mkdir "$UNIVERSAL_LIBRARY_INPUT"
cp "$ARM64_FIXTURE/PTT2me" "$UNIVERSAL_LIBRARY_INPUT/PTT2me"
cp "$UNIVERSAL_FIXTURE/libsherpa-onnx-c-api.dylib" "$UNIVERSAL_LIBRARY_INPUT/"
cp "$UNIVERSAL_FIXTURE/libonnxruntime.1.17.1.dylib" "$UNIVERSAL_LIBRARY_INPUT/"
PACKAGED_APP="$TEMP_ROOT/packaged-universal-input.app"
env \
    PATH="$BUILD_FIXTURE_REPO/fake-tools:$PATH" \
    FIXTURE_BIN_DIR="$UNIVERSAL_LIBRARY_INPUT" \
    "$BUILD_FIXTURE_REPO/scripts/build-app.sh" \
    --variant update \
    --model-manifest \
    "$BUILD_FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json" \
    --output "$PACKAGED_APP"
expect_exact_architecture \
    arm64 "$PACKAGED_APP/Contents/Frameworks/libsherpa-onnx-c-api.dylib"
expect_exact_architecture \
    arm64 "$PACKAGED_APP/Contents/Frameworks/libonnxruntime.1.17.1.dylib"

PACKAGED_THIN_APP="$TEMP_ROOT/packaged-thin-input.app"
env \
    PATH="$BUILD_FIXTURE_REPO/fake-tools:$PATH" \
    FIXTURE_BIN_DIR="$ARM64_FIXTURE" \
    "$BUILD_FIXTURE_REPO/scripts/build-app.sh" \
    --variant update \
    --model-manifest \
    "$BUILD_FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json" \
    --output "$PACKAGED_THIN_APP"
expect_exact_architecture \
    arm64 "$PACKAGED_THIN_APP/Contents/Frameworks/libsherpa-onnx-c-api.dylib"
expect_exact_architecture \
    arm64 "$PACKAGED_THIN_APP/Contents/Frameworks/libonnxruntime.1.17.1.dylib"

X86_64_LIBRARY_INPUT="$TEMP_ROOT/x86_64-library-input"
mkdir "$X86_64_LIBRARY_INPUT"
cp "$ARM64_FIXTURE/PTT2me" "$X86_64_LIBRARY_INPUT/PTT2me"
cp "$X86_64_FIXTURE/libsherpa-onnx-c-api.dylib" "$X86_64_LIBRARY_INPUT/"
cp "$X86_64_FIXTURE/libonnxruntime.1.17.1.dylib" "$X86_64_LIBRARY_INPUT/"
expect_failure_containing \
    "does not contain arm64" \
    env \
    PATH="$BUILD_FIXTURE_REPO/fake-tools:$PATH" \
    FIXTURE_BIN_DIR="$X86_64_LIBRARY_INPUT" \
    "$BUILD_FIXTURE_REPO/scripts/build-app.sh" \
    --variant update \
    --model-manifest \
    "$BUILD_FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json" \
    --output "$TEMP_ROOT/packaged-x86_64-input.app"

"$CHECK_PRODUCTION_MANIFEST" \
    "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"
WHITESPACE_MUTATION="$TEMP_ROOT/production-manifest-whitespace.json"
cp "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" "$WHITESPACE_MUTATION"
printf ' ' >>"$WHITESPACE_MUTATION"
expect_failure_containing "production manifest SHA-256 mismatch" \
    "$CHECK_PRODUCTION_MANIFEST" "$WHITESPACE_MUTATION"
FAKE_APP="$TEMP_ROOT/fake.app"
mkdir "$FAKE_APP"
expect_failure_containing "production manifest SHA-256 mismatch" \
    "$REPO_ROOT/scripts/build-app.sh" \
    --variant update \
    --model-manifest "$WHITESPACE_MUTATION"
expect_failure_containing "production manifest SHA-256 mismatch" \
    "$REPO_ROOT/scripts/check-bundle.sh" \
    --variant update \
    --model-manifest "$WHITESPACE_MUTATION" \
    "$FAKE_APP"
expect_failure_containing "production manifest SHA-256 mismatch" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$WHITESPACE_MUTATION" \
    --app "$FAKE_APP" \
    --output "$TEMP_ROOT/fake.dmg"

expect_failure_containing "git HEAD must be exactly 40 lowercase hexadecimal characters" \
    env PATH="$FAKE_TOOLS:$PATH" \
    "$REPO_ROOT/scripts/build-app.sh" \
    --variant update \
    --model-manifest "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"

expect_failure_containing "explicit build must be a valid 12-digit UTC calendar minute" \
    env PATH="$FAKE_TOOLS:$PATH" \
    "$REPO_ROOT/scripts/build-app.sh" \
    --variant update \
    --model-manifest "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" \
    --build invalid

INVALID_IDENTITY_APP="$TEMP_ROOT/invalid-identity.app"
mkdir -p \
    "$INVALID_IDENTITY_APP/Contents/MacOS" \
    "$INVALID_IDENTITY_APP/Contents/Frameworks" \
    "$INVALID_IDENTITY_APP/Contents/Resources"
touch \
    "$INVALID_IDENTITY_APP/Contents/MacOS/PTT2me" \
    "$INVALID_IDENTITY_APP/Contents/Frameworks/libsherpa-onnx-c-api.dylib" \
    "$INVALID_IDENTITY_APP/Contents/Frameworks/libonnxruntime.1.17.1.dylib"
cp "$REPO_ROOT/assets/PTT2me.icns" \
    "$INVALID_IDENTITY_APP/Contents/Resources/PTT2me.icns"
INVALID_IDENTITY_PLIST="$INVALID_IDENTITY_APP/Contents/Info.plist"
plutil -create xml1 "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleIdentifier string com.ptt2me.app" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleIconFile string PTT2me.icns" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleShortVersionString string $CURRENT_VERSION" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleVersion string 202608011200" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :PTT2meSourceCommit string ABC" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :PTT2meDistributionVariant string update" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c "Add :LSUIElement bool true" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :LSMinimumSystemVersion string 13.0" "$INVALID_IDENTITY_PLIST"
expect_failure_containing \
    "Info.plist PTT2meSourceCommit must be 40 lowercase hexadecimal characters" \
    env PATH="$FAKE_TOOLS:$PATH" \
    "$REPO_ROOT/scripts/check-bundle.sh" \
    --variant update \
    --model-manifest "$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json" \
    "$INVALID_IDENTITY_APP"

grep -Fq 'Add :PTT2meSourceCommit string $SOURCE_COMMIT' \
    "$REPO_ROOT/scripts/build-app.sh" || {
    echo "build-app.sh must write PTT2meSourceCommit before signing" >&2
    exit 1
}

"$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
"$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --model-source "$MODEL_DIRECTORY"
printf '\n' >"$MODEL_DIRECTORY/.gitkeep"
"$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --model-source "$MODEL_DIRECTORY"
expect_failure_containing "exactly four entries" \
    "$CHECK_MODEL_VARIANT" \
    --variant full \
    --model-manifest "$MANIFEST" \
    --resources "$FULL_RESOURCES"
rm "$MODEL_DIRECTORY/.gitkeep"
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

PRODUCTION_MANIFEST="$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"
OUTPUT_DIRECTORY="$TEMP_ROOT/existing-output.dmg"
mkdir "$OUTPUT_DIRECTORY"
expect_failure_containing "output path already exists" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$PRODUCTION_MANIFEST" \
    --app "$FAKE_APP" \
    --output "$OUTPUT_DIRECTORY"

CHECKSUM_DIRECTORY_OUTPUT="$TEMP_ROOT/checksum-directory.dmg"
mkdir "$CHECKSUM_DIRECTORY_OUTPUT.sha256"
expect_failure_containing "checksum path already exists" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$PRODUCTION_MANIFEST" \
    --app "$FAKE_APP" \
    --output "$CHECKSUM_DIRECTORY_OUTPUT"

CHECKSUM_SYMLINK_OUTPUT="$TEMP_ROOT/checksum-symlink.dmg"
ln -s "$TEMP_ROOT/missing-checksum-target" "$CHECKSUM_SYMLINK_OUTPUT.sha256"
expect_failure_containing "checksum path already exists" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$PRODUCTION_MANIFEST" \
    --app "$FAKE_APP" \
    --output "$CHECKSUM_SYMLINK_OUTPUT"

expect_failure_containing "output must not be inside the supplied app" \
    "$REPO_ROOT/scripts/build-dmg.sh" \
    --variant update \
    --model-manifest "$PRODUCTION_MANIFEST" \
    --app "$FAKE_APP" \
    --output "$FAKE_APP/inside.dmg"

grep -Fq 'mktemp -d "$DMG_OUTPUT_DIR/.$DMG_NAME.work.XXXXXX"' \
    "$REPO_ROOT/scripts/build-dmg.sh" || {
    echo "build-dmg.sh must use a unique private workspace on the output filesystem" >&2
    exit 1
}
grep -Fq 'ln "$TEMP_CHECKSUM" "$CHECKSUM_PATH"' \
    "$REPO_ROOT/scripts/build-dmg.sh" || {
    echo "build-dmg.sh must publish the checksum through a no-overwrite hard link" >&2
    exit 1
}
grep -Fq 'ln "$TEMP_DMG" "$DMG_PATH"' "$REPO_ROOT/scripts/build-dmg.sh" || {
    echo "build-dmg.sh must publish the DMG through a no-overwrite hard link" >&2
    exit 1
}
if grep -Fq 'mv -f' "$REPO_ROOT/scripts/build-dmg.sh"; then
    echo "build-dmg.sh must not overwrite concurrent outputs" >&2
    exit 1
fi

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

PAGES_WORKFLOW="$REPO_ROOT/.github/workflows/pages.yml"
IMMUTABILITY_GATE_LINE="$(grep -n 'git diff --name-status' "$PAGES_WORKFLOW" | cut -d: -f1)"
NO_STABLE_SKIP_LINE="$(grep -n 'if \[\[ ! -e "\$stable" \]\]' "$PAGES_WORKFLOW" | cut -d: -f1)"
[[ -n "$IMMUTABILITY_GATE_LINE" && -n "$NO_STABLE_SKIP_LINE" ]] || {
    echo "Pages workflow must contain immutability and no-stable gates" >&2
    exit 1
}
[[ "$IMMUTABILITY_GATE_LINE" -lt "$NO_STABLE_SKIP_LINE" ]] || {
    echo "Pages workflow must enforce immutable records before skipping absent stable" >&2
    exit 1
}

echo "Model bundle variant checks passed"
