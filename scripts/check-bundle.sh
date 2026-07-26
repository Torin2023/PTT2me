#!/bin/bash
set -euo pipefail

cd "$(dirname "$0")/.."

APP_PATH="${1:-dist/PTT2me.app}"
CONTENTS="$APP_PATH/Contents"
EXECUTABLE="$CONTENTS/MacOS/PTT2me"
FRAMEWORKS="$CONTENTS/Frameworks"
MODEL="$CONTENTS/Resources/models/gigaam-v3-rnnt"
PLIST="$CONTENTS/Info.plist"

fail() {
    echo "PTT2me bundle check failed: $*" >&2
    exit 1
}

require_file() {
    local path="$1"
    local relative="${path#"$APP_PATH"/}"
    [[ -f "$path" ]] || fail "missing $relative"
}

require_nonempty_file() {
    local path="$1"
    local relative="${path#"$APP_PATH"/}"
    [[ -s "$path" ]] || fail "missing or empty $relative"
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

check_linkage() {
    local path="$1"
    local dependency
    while IFS= read -r dependency; do
        dependency="${dependency#"${dependency%%[![:space:]]*}"}"
        dependency="${dependency%% *}"
        [[ -n "$dependency" ]] || continue
        case "$dependency" in
            /System/* | /usr/lib/* | @rpath/* | @executable_path/* | @loader_path/*) ;;
            /*) fail "${path#"$APP_PATH"/} links non-system absolute path $dependency" ;;
        esac
    done < <(otool -L "$path" | tail -n +2)
}

require_file "$EXECUTABLE"
require_arm64 "$EXECUTABLE"

require_nonempty_file "$MODEL/encoder.int8.onnx"
require_nonempty_file "$MODEL/decoder.onnx"
require_nonempty_file "$MODEL/joiner.onnx"
require_nonempty_file "$MODEL/tokens.txt"

SHERPA_DYLIB="$FRAMEWORKS/libsherpa-onnx-c-api.dylib"
ONNX_DYLIB="$FRAMEWORKS/libonnxruntime.1.17.1.dylib"
require_file "$SHERPA_DYLIB"
require_arm64 "$SHERPA_DYLIB"
require_file "$ONNX_DYLIB"
require_arm64 "$ONNX_DYLIB"

require_file "$PLIST"
assert_plist CFBundleIdentifier com.ptt2me.app
assert_plist CFBundleShortVersionString 1.0.0
assert_plist LSUIElement true
assert_plist LSMinimumSystemVersion 13.0

check_linkage "$EXECUTABLE"
check_linkage "$SHERPA_DYLIB"
check_linkage "$ONNX_DYLIB"

codesign --verify --deep --strict "$APP_PATH" >/dev/null 2>&1 ||
    fail "code signature verification failed"

echo "PTT2me bundle is valid: $APP_PATH"
