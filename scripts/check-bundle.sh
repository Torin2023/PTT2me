#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
cd -- "$REPO_ROOT"

fail() {
    echo "PTT2me bundle check failed: $*" >&2
    exit 1
}

usage() {
    fail "usage: $0 --variant full|update --model-manifest PATH APP_PATH"
}

VARIANT=""
MODEL_MANIFEST=""
APP_PATH=""
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
        --*) usage ;;
        *)
            [[ -z "$APP_PATH" ]] || usage
            APP_PATH="$1"
            shift
            ;;
    esac
done

[[ "$VARIANT" == "full" || "$VARIANT" == "update" ]] || usage
[[ -n "$MODEL_MANIFEST" && -n "$APP_PATH" ]] || usage
COMMITTED_MODEL_MANIFEST="$REPO_ROOT/models/manifests/gigaam-v3-rnnt-v1.json"
[[ -f "$MODEL_MANIFEST" && ! -L "$MODEL_MANIFEST" ]] ||
    fail "model manifest is not a real file: $MODEL_MANIFEST"
"$SCRIPT_DIR/check-production-model-manifest.sh" "$MODEL_MANIFEST"
cmp -s "$COMMITTED_MODEL_MANIFEST" "$MODEL_MANIFEST" ||
    fail "--model-manifest must match the committed exact bytes"

CONTENTS="$APP_PATH/Contents"
EXECUTABLE="$CONTENTS/MacOS/PTT2me"
FRAMEWORKS="$CONTENTS/Frameworks"
RESOURCES="$CONTENTS/Resources"
PLIST="$CONTENTS/Info.plist"

require_file() {
    local path="$1"
    local relative="${path#"$APP_PATH"/}"
    [[ -f "$path" ]] || fail "missing $relative"
}

require_arm64() {
    local path="$1"
    local relative="${path#"$APP_PATH"/}"
    lipo "$path" -verify_arch arm64 >/dev/null 2>&1 ||
        fail "$relative is not arm64"
}

assert_plist() {
    local key="$1"
    local expected="$2"
    local actual
    actual=$(/usr/libexec/PlistBuddy -c "Print :$key" "$PLIST" 2>/dev/null) ||
        fail "missing Info.plist key $key"
    [[ "$actual" == "$expected" ]] ||
        fail "Info.plist $key is '$actual', expected '$expected'"
}

mach_o_dependencies() {
    local path="$1"
    local output

    output="$(otool -arch arm64 -L "$path" 2>/dev/null)" ||
        fail "could not inspect arm64 dependencies for ${path#"$APP_PATH"/}"
    printf '%s\n' "$output" |
        tail -n +2 |
        sed -E 's/^[[:space:]]+//; s/[[:space:]]+[(]compatibility version.*[)]$//'
}

require_dylib_id() {
    local path="$1"
    local expected="$2"
    local output
    local actual

    output="$(otool -arch arm64 -D "$path" 2>/dev/null)" ||
        fail "could not inspect dylib ID for ${path#"$APP_PATH"/}"
    actual="$(printf '%s\n' "$output" | tail -n +2)"
    [[ "$actual" == "$expected" ]] ||
        fail "${path#"$APP_PATH"/} has dylib ID '$actual', expected '$expected'"
}

is_system_dependency() {
    case "$1" in
        /System/* | /usr/lib/*) return 0 ;;
        *) return 1 ;;
    esac
}

check_executable_linkage() {
    local path="$1"
    local dependencies
    local dependency
    local found_sherpa=false
    local found_onnx=false

    dependencies="$(mach_o_dependencies "$path")" ||
        fail "could not parse dependencies for ${path#"$APP_PATH"/}"
    while IFS= read -r dependency; do
        [[ -n "$dependency" ]] || continue
        case "$dependency" in
            @rpath/libsherpa-onnx-c-api.dylib) found_sherpa=true ;;
            @rpath/libonnxruntime.1.17.1.dylib) found_onnx=true ;;
            *)
                is_system_dependency "$dependency" ||
                    fail "${path#"$APP_PATH"/} has unexpected dependency $dependency"
                ;;
        esac
    done <<<"$dependencies"

    [[ "$found_sherpa" == true ]] ||
        fail "${path#"$APP_PATH"/} is missing @rpath/libsherpa-onnx-c-api.dylib"
    [[ "$found_onnx" == true ]] ||
        fail "${path#"$APP_PATH"/} is missing @rpath/libonnxruntime.1.17.1.dylib"
}

check_sherpa_linkage() {
    local path="$1"
    local dependencies
    local dependency
    local found_onnx=false

    dependencies="$(mach_o_dependencies "$path")" ||
        fail "could not parse dependencies for ${path#"$APP_PATH"/}"
    while IFS= read -r dependency; do
        [[ -n "$dependency" ]] || continue
        case "$dependency" in
            @rpath/libsherpa-onnx-c-api.dylib) ;;
            @rpath/libonnxruntime.1.17.1.dylib) found_onnx=true ;;
            *)
                is_system_dependency "$dependency" ||
                    fail "${path#"$APP_PATH"/} has unexpected dependency $dependency"
                ;;
        esac
    done <<<"$dependencies"

    [[ "$found_onnx" == true ]] ||
        fail "${path#"$APP_PATH"/} is missing @rpath/libonnxruntime.1.17.1.dylib"
}

check_onnx_linkage() {
    local path="$1"
    local dependencies
    local dependency

    dependencies="$(mach_o_dependencies "$path")" ||
        fail "could not parse dependencies for ${path#"$APP_PATH"/}"
    while IFS= read -r dependency; do
        [[ -n "$dependency" ]] || continue
        case "$dependency" in
            @rpath/libonnxruntime.1.17.1.dylib) ;;
            *)
                is_system_dependency "$dependency" ||
                    fail "${path#"$APP_PATH"/} has unexpected dependency $dependency"
                ;;
        esac
    done <<<"$dependencies"
}

require_exact_executable_rpath() {
    local path="$1"
    local output
    local rpaths

    output="$(otool -arch arm64 -l "$path" 2>/dev/null)" ||
        fail "could not inspect runtime paths for ${path#"$APP_PATH"/}"
    rpaths="$(
        printf '%s\n' "$output" |
            awk '$1 == "cmd" && $2 == "LC_RPATH" { getline; getline; print $2 }'
    )"
    [[ "$rpaths" == "@executable_path/../Frameworks" ]] ||
        fail "${path#"$APP_PATH"/} runtime paths are not exactly @executable_path/../Frameworks"
}

"$SCRIPT_DIR/check-model-variant.sh" \
    --variant "$VARIANT" \
    --model-manifest "$MODEL_MANIFEST" \
    --resources "$RESOURCES" || fail "$VARIANT model layout is invalid"

require_file "$EXECUTABLE"
require_arm64 "$EXECUTABLE"

SHERPA_DYLIB="$FRAMEWORKS/libsherpa-onnx-c-api.dylib"
ONNX_DYLIB="$FRAMEWORKS/libonnxruntime.1.17.1.dylib"
require_file "$SHERPA_DYLIB"
require_arm64 "$SHERPA_DYLIB"
require_file "$ONNX_DYLIB"
require_arm64 "$ONNX_DYLIB"

require_file "$PLIST"
EXPECTED_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$EXPECTED_VERSION" ]] || fail "could not read version from Cargo.toml"
assert_plist CFBundleIdentifier com.ptt2me.app
assert_plist CFBundleShortVersionString "$EXPECTED_VERSION"
assert_plist PTT2meDistributionVariant "$VARIANT"
assert_plist LSUIElement true
assert_plist LSMinimumSystemVersion 13.0

BUILD_VERSION=$(/usr/libexec/PlistBuddy -c "Print :CFBundleVersion" "$PLIST" 2>/dev/null) ||
    fail "missing Info.plist key CFBundleVersion"
[[ "$BUILD_VERSION" =~ ^[0-9]{12}$ ]] ||
    fail "Info.plist CFBundleVersion must be 12 decimal digits (YYYYMMDDHHMM)"
PARSED_BUILD_VERSION=$(
    /bin/date -u -j -f "%Y%m%d%H%M" "$BUILD_VERSION" "+%Y%m%d%H%M" 2>/dev/null
) || fail "Info.plist CFBundleVersion is not a valid UTC calendar minute"
[[ "$PARSED_BUILD_VERSION" == "$BUILD_VERSION" ]] ||
    fail "Info.plist CFBundleVersion is not a valid UTC calendar minute"
SOURCE_COMMIT=$(/usr/libexec/PlistBuddy -c "Print :PTT2meSourceCommit" "$PLIST" 2>/dev/null) ||
    fail "missing Info.plist key PTT2meSourceCommit"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail "Info.plist PTT2meSourceCommit must be 40 lowercase hexadecimal characters"

require_dylib_id "$SHERPA_DYLIB" "@rpath/libsherpa-onnx-c-api.dylib"
require_dylib_id "$ONNX_DYLIB" "@rpath/libonnxruntime.1.17.1.dylib"
require_exact_executable_rpath "$EXECUTABLE"
check_executable_linkage "$EXECUTABLE"
check_sherpa_linkage "$SHERPA_DYLIB"
check_onnx_linkage "$ONNX_DYLIB"

codesign --verify --deep --strict "$APP_PATH" >/dev/null 2>&1 ||
    fail "code signature verification failed"

if [[ "$VARIANT" == "full" ]]; then
    if "$EXECUTABLE" --smoke-model; then
        :
    else
        SMOKE_STATUS=$?
        if [[ "$SMOKE_STATUS" == 124 ]]; then
            fail "bundled model initialization exceeded 180 seconds"
        fi
        fail "bundled model initialization failed with status $SMOKE_STATUS"
    fi
fi

echo "PTT2me $VARIANT bundle is valid: $APP_PATH"
