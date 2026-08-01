#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
CHECK_MODEL_VARIANT="$REPO_ROOT/scripts/check-model-variant.sh"
CHECK_PRODUCTION_MANIFEST="$REPO_ROOT/scripts/check-production-model-manifest.sh"

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
printf '%s\n' '#!/bin/bash' 'exit 0' >"$FAKE_TOOLS/lipo"
printf '%s\n' \
    '#!/bin/bash' \
    'echo "otool must not run before source commit validation" >&2' \
    'exit 94' >"$FAKE_TOOLS/otool"
chmod 755 "$FAKE_TOOLS"/*

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
INVALID_IDENTITY_PLIST="$INVALID_IDENTITY_APP/Contents/Info.plist"
plutil -create xml1 "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleIdentifier string com.ptt2me.app" "$INVALID_IDENTITY_PLIST"
/usr/libexec/PlistBuddy -c \
    "Add :CFBundleShortVersionString string 1.0.5" "$INVALID_IDENTITY_PLIST"
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
