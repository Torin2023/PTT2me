#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
VERIFIER_SOURCE="$REPO_ROOT/scripts/verify-release-artifacts.sh"

[[ -x "$VERIFIER_SOURCE" ]] || {
    echo "release artifact verifier is missing or not executable: $VERIFIER_SOURCE" >&2
    exit 1
}

cargo build --quiet --locked --bins
MANIFEST_VERIFIER="$REPO_ROOT/target/debug/ptt2me"
SIGNER="$REPO_ROOT/target/debug/ptt2me-update-signer"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-release-verifier-tests.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT
FIXTURE_INDEX=0

write_fake_hdiutil() {
    FAKE_BIN="$TEMP_ROOT/fake-bin-$FIXTURE_INDEX"
    FAKE_HDIUTIL_LOG="$TEMP_ROOT/hdiutil-$FIXTURE_INDEX.log"
    FAKE_HDIUTIL_STATE="$TEMP_ROOT/hdiutil-$FIXTURE_INDEX.state"
    mkdir -p "$FAKE_BIN"
    : >"$FAKE_HDIUTIL_LOG"
    : >"$FAKE_HDIUTIL_STATE"
    printf '%s\n' \
        '#!/bin/bash' \
        'set -euo pipefail' \
        'echo "$*" >>"$FAKE_HDIUTIL_LOG"' \
        'operation="$1"' \
        'shift' \
        'case "$operation" in' \
        '  verify)' \
        '    image="$1"' \
        '    variant=update' \
        '    if [[ "$(basename -- "$image")" == *-full-* ]]; then variant=full; fi' \
        '    if [[ "${FAKE_HDIUTIL_FAIL:-}" == "verify-$variant" ]]; then exit 81; fi' \
        '    ;;' \
        '  attach)' \
        '    image="$1"' \
        '    shift' \
        '    variant=update' \
        '    if [[ "$(basename -- "$image")" == *-full-* ]]; then variant=full; fi' \
        '    if [[ "${FAKE_HDIUTIL_FAIL:-}" == "attach-$variant" ]]; then exit 82; fi' \
        '    mountpoint=""' \
        '    readonly=false' \
        '    nobrowse=false' \
        '    noautoopen=false' \
        '    while [[ $# -gt 0 ]]; do' \
        '      case "$1" in' \
        '        -mountpoint) mountpoint="$2"; shift 2 ;;' \
        '        -readonly) readonly=true; shift ;;' \
        '        -nobrowse) nobrowse=true; shift ;;' \
        '        -noautoopen) noautoopen=true; shift ;;' \
        '        *) shift ;;' \
        '      esac' \
        '    done' \
        '    [[ -n "$mountpoint" ]] || exit 83' \
        '    [[ "$readonly" == true && "$nobrowse" == true && "$noautoopen" == true ]] || exit 86' \
        '    /bin/cp -R "$IMAGE_ROOT/$variant/." "$mountpoint/"' \
        '    printf "%s\n" "$mountpoint" >>"$FAKE_HDIUTIL_STATE"' \
        '    if [[ "${FAKE_HDIUTIL_SIGNAL_AFTER_ATTACH:-}" == "$variant" ]]; then /bin/kill -TERM "$PPID"; fi' \
        '    ;;' \
        '  detach)' \
        '    mountpoint="$1"' \
        '    variant=update' \
        '    if [[ "$(basename -- "$mountpoint")" == full-mount ]]; then variant=full; fi' \
        '    if [[ "${FAKE_HDIUTIL_FAIL:-}" == "detach-$variant" ]]; then exit 84; fi' \
        '    state_tmp="$FAKE_HDIUTIL_STATE.tmp.$$"' \
        '    /usr/bin/grep -Fvx -- "$mountpoint" "$FAKE_HDIUTIL_STATE" >"$state_tmp" || true' \
        '    /bin/mv "$state_tmp" "$FAKE_HDIUTIL_STATE"' \
        '    ;;' \
        '  *) exit 85 ;;' \
        'esac' >"$FAKE_BIN/hdiutil"
    printf '%s\n' \
        '#!/bin/bash' \
        'set -euo pipefail' \
        'while [[ $# -gt 0 && "$1" != "--" ]]; do shift; done' \
        '[[ "${1:-}" == "--" ]] || exit 87' \
        'shift' \
        'DYLD_LIBRARY_PATH="$FAKE_MANIFEST_LIBRARY_PATH" exec "$FAKE_MANIFEST_VERIFIER" "$@"' \
        >"$FAKE_BIN/cargo"
    chmod 755 "$FAKE_BIN/hdiutil" "$FAKE_BIN/cargo"
}

write_fixture_helpers() {
    cat >"$FIXTURE_REPO/scripts/check-bundle.sh" <<'EOF'
#!/bin/bash
set -euo pipefail
variant=""
app=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --variant) variant="$2"; shift 2 ;;
        --model-manifest) shift 2 ;;
        *) app="$1"; shift ;;
    esac
done
[[ -n "$variant" && -d "$app" ]] || exit 91
[[ "${FAKE_BUNDLE_FAIL_VARIANT:-}" != "$variant" ]] || exit 92
[[ "$(cat "$app/variant.txt")" == "$variant" ]] || exit 93
[[ -x "$app/Contents/MacOS/PTT2me" ]] || exit 94
if [[ "$variant" == full ]]; then
    [[ -d "$app/Contents/Resources/models/gigaam-v3-rnnt-v1" ]] || exit 95
    "$app/Contents/MacOS/PTT2me" --smoke-model
else
    [[ ! -e "$app/Contents/Resources/models" ]] || exit 96
fi
EOF
    cat >"$FIXTURE_REPO/scripts/compare-macho-payload.sh" <<'EOF'
#!/bin/bash
set -euo pipefail
[[ "${FAKE_PARITY_FAIL:-0}" != 1 ]] || exit 97
cmp -s "$1" "$2"
EOF
    chmod 755 \
        "$FIXTURE_REPO/scripts/check-bundle.sh" \
        "$FIXTURE_REPO/scripts/compare-macho-payload.sh"
}

write_fake_plists() {
    local variant app
    for variant in full update; do
        app="$IMAGE_ROOT/$variant/PTT2me.app"
        cat >"$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleShortVersionString</key>
    <string>1.1.0</string>
    <key>CFBundleVersion</key>
    <string>202608111200</string>
    <key>PTT2meSourceCommit</key>
    <string>$SOURCE_COMMIT</string>
</dict>
</plist>
EOF
    done
}

write_fake_app() {
    local variant="$1"
    local app="$IMAGE_ROOT/$variant/PTT2me.app"
    mkdir -p \
        "$app/Contents/MacOS" \
        "$app/Contents/Frameworks" \
        "$app/Contents/Resources"
    printf '%s\n' "$variant" >"$app/variant.txt"
    cat >"$app/Contents/MacOS/PTT2me" <<'EOF'
#!/bin/bash
set -euo pipefail
[[ "${1:-}" == "--smoke-model" ]] || exit 98
[[ "${FAKE_SMOKE_FAIL:-0}" != 1 ]] || exit 99
exit 0
EOF
    chmod 755 "$app/Contents/MacOS/PTT2me"
    printf '%s' executable-payload >"$app/Contents/MacOS/payload.bin"
    printf '%s' sherpa-payload >"$app/Contents/Frameworks/libsherpa-onnx-c-api.dylib"
    printf '%s' onnx-payload >"$app/Contents/Frameworks/libonnxruntime.1.17.1.dylib"
    ln -s /Applications "$IMAGE_ROOT/$variant/Applications"
}

write_fixture() {
    FIXTURE_INDEX=$((FIXTURE_INDEX + 1))
    FIXTURE_REPO="$TEMP_ROOT/repo-$FIXTURE_INDEX"
    IMAGE_ROOT="$TEMP_ROOT/images-$FIXTURE_INDEX"
    OUTPUT_DIR="$TEMP_ROOT/output-$FIXTURE_INDEX"
    KEY_DIR="$TEMP_ROOT/keys-$FIXTURE_INDEX"
    mkdir -p \
        "$FIXTURE_REPO/scripts" \
        "$FIXTURE_REPO/models/manifests" \
        "$FIXTURE_REPO/updates" \
        "$IMAGE_ROOT/full" "$IMAGE_ROOT/update" \
        "$OUTPUT_DIR" "$KEY_DIR"

    cp "$VERIFIER_SOURCE" "$FIXTURE_REPO/scripts/verify-release-artifacts.sh"
    cp "$REPO_ROOT/scripts/validate-update-manifest.sh" \
        "$FIXTURE_REPO/scripts/validate-update-manifest.sh"
    cp "$REPO_ROOT/scripts/check-model-variant.sh" \
        "$FIXTURE_REPO/scripts/check-model-variant.sh"
    chmod 755 "$FIXTURE_REPO/scripts"/*.sh
    write_fixture_helpers
    write_fake_app full
    write_fake_app update

    MODEL_MANIFEST="$FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json"
    MODEL_DIRECTORY="$IMAGE_ROOT/full/PTT2me.app/Contents/Resources/models/gigaam-v3-rnnt-v1"
    mkdir -p "$MODEL_DIRECTORY"
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
        '}' >"$MODEL_MANIFEST"

    PRIVATE_KEY="$KEY_DIR/private-key.txt"
    PUBLIC_KEY="$KEY_DIR/public-key.txt"
    printf '%s\n' 'QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=' >"$PRIVATE_KEY"
    chmod 600 "$PRIVATE_KEY"
    "$SIGNER" --derive-public-key "$PRIVATE_KEY" "$PUBLIC_KEY"
    cp "$PUBLIC_KEY" "$FIXTURE_REPO/updates/public-key.txt"

    /usr/bin/git -C "$FIXTURE_REPO" init -q
    /usr/bin/git -C "$FIXTURE_REPO" config user.email fixture@ptt2me.invalid
    /usr/bin/git -C "$FIXTURE_REPO" config user.name 'PTT2me Fixture'
    /usr/bin/git -C "$FIXTURE_REPO" add .
    /usr/bin/git -C "$FIXTURE_REPO" commit -qm fixture
    SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPO" rev-parse HEAD)"
    /usr/bin/git -C "$FIXTURE_REPO" tag v1.1.0
    write_fake_plists

    FULL_DMG="$OUTPUT_DIR/PTT2me-1.1.0-full-macos-arm64.dmg"
    FULL_CHECKSUM="$FULL_DMG.sha256"
    UPDATE_DMG="$OUTPUT_DIR/PTT2me-1.1.0-update-macos-arm64.dmg"
    UPDATE_CHECKSUM="$UPDATE_DMG.sha256"
    SIGNED_MANIFEST="$OUTPUT_DIR/PTT2me-1.1.0-signed-update-manifest.json"
    printf '%s' 'synthetic full dmg' >"$FULL_DMG"
    printf '%s' 'synthetic update dmg' >"$UPDATE_DMG"
    FULL_SHA="$(shasum -a 256 "$FULL_DMG" | awk '{print $1}')"
    UPDATE_SHA="$(shasum -a 256 "$UPDATE_DMG" | awk '{print $1}')"
    MODEL_SHA="$(shasum -a 256 "$MODEL_MANIFEST" | awk '{print $1}')"
    FULL_SIZE="$(stat -f '%z' "$FULL_DMG")"
    UPDATE_SIZE="$(stat -f '%z' "$UPDATE_DMG")"
    PAYLOAD="$TEMP_ROOT/payload-$FIXTURE_INDEX.json"
    printf '%s\n' \
        "{\"channel\":\"stable\",\"version\":\"1.1.0\",\"build\":202608111200,\"source_commit\":\"$SOURCE_COMMIT\",\"minimum_macos\":\"13.0\",\"architecture\":\"arm64\",\"required_model\":{\"id\":\"gigaam-v3-rnnt-v1\",\"manifest_sha256\":\"$MODEL_SHA\"},\"fresh_install\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.1.0/PTT2me-1.1.0-full-macos-arm64.dmg\",\"sha256\":\"$FULL_SHA\",\"size\":$FULL_SIZE},\"application_update\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v1.1.0/PTT2me-1.1.0-update-macos-arm64.dmg\",\"sha256\":\"$UPDATE_SHA\",\"size\":$UPDATE_SIZE},\"published_at\":\"2026-08-11T12:00:00Z\"}" \
        >"$PAYLOAD"
    "$SIGNER" "$PRIVATE_KEY" "$PAYLOAD" "$SIGNED_MANIFEST"
    printf '%s  %s\n' "$FULL_SHA" "$(basename -- "$FULL_DMG")" >"$FULL_CHECKSUM"
    printf '%s  %s\n' "$UPDATE_SHA" "$(basename -- "$UPDATE_DMG")" >"$UPDATE_CHECKSUM"
    write_fake_hdiutil
}

output_fingerprint() {
    find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -type f -print |
        LC_ALL=C sort |
        while IFS= read -r path; do
            shasum -a 256 "$path"
        done
}

verify_command() {
    env \
        PATH="$FAKE_BIN:/usr/bin:/bin" \
        IMAGE_ROOT="$IMAGE_ROOT" \
        FAKE_HDIUTIL_LOG="$FAKE_HDIUTIL_LOG" \
        FAKE_HDIUTIL_STATE="$FAKE_HDIUTIL_STATE" \
        FAKE_MANIFEST_VERIFIER="$MANIFEST_VERIFIER" \
        FAKE_MANIFEST_LIBRARY_PATH="$REPO_ROOT/target/debug" \
        "$FIXTURE_REPO/scripts/verify-release-artifacts.sh" \
        --version "${TEST_VERSION:-1.1.0}" \
        --source-commit "${TEST_SOURCE_COMMIT:-$SOURCE_COMMIT}" \
        ${TEST_EXPECTED_TAG:+--expected-tag "$TEST_EXPECTED_TAG"} \
        --full-dmg "$FULL_DMG" \
        --full-checksum "$FULL_CHECKSUM" \
        --update-dmg "$UPDATE_DMG" \
        --update-checksum "$UPDATE_CHECKSUM" \
        --manifest "$SIGNED_MANIFEST" \
        --public-key "$PUBLIC_KEY" \
        --model-manifest "$MODEL_MANIFEST"
}

expect_failure() {
    local category="$1"
    shift
    local before output after
    before="$(output_fingerprint)"
    if output="$("$@" 2>&1)"; then
        echo "expected verifier failure [$category]: $*" >&2
        exit 1
    fi
    [[ "$output" == *"[$category]"* ]] || {
        echo "verifier failure did not contain [$category]: $output" >&2
        exit 1
    }
    after="$(output_fingerprint)"
    [[ "$before" == "$after" ]] || {
        echo "verifier modified release outputs during failure [$category]" >&2
        exit 1
    }
}

expect_detach_logged() {
    local variant="$1"
    grep -Eq "^detach .*/$variant-mount -quiet$" "$FAKE_HDIUTIL_LOG" || {
        echo "verifier did not detach $variant mount after failure" >&2
        exit 1
    }
}

expect_no_mounts() {
    [[ ! -s "$FAKE_HDIUTIL_STATE" ]] || {
        echo "verifier left synthetic DMG mounts active:" >&2
        cat "$FAKE_HDIUTIL_STATE" >&2
        exit 1
    }
}

expect_signal_cleanup() {
    local before after
    before="$(output_fingerprint)"
    if FAKE_HDIUTIL_SIGNAL_AFTER_ATTACH=update verify_command >/dev/null 2>&1; then
        echo "expected verifier termination during Update attach" >&2
        exit 1
    fi
    after="$(output_fingerprint)"
    [[ "$before" == "$after" ]] || {
        echo "verifier modified release outputs during signal cleanup" >&2
        exit 1
    }
    expect_detach_logged full
    expect_detach_logged update
    expect_no_mounts
}

resign_payload_version() {
    local version="$1"
    local replacement="$TEMP_ROOT/replacement-payload-$FIXTURE_INDEX.json"
    printf '%s\n' \
        "{\"channel\":\"stable\",\"version\":\"$version\",\"build\":202608111200,\"source_commit\":\"$SOURCE_COMMIT\",\"minimum_macos\":\"13.0\",\"architecture\":\"arm64\",\"required_model\":{\"id\":\"gigaam-v3-rnnt-v1\",\"manifest_sha256\":\"$MODEL_SHA\"},\"fresh_install\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v$version/PTT2me-$version-full-macos-arm64.dmg\",\"sha256\":\"$FULL_SHA\",\"size\":$FULL_SIZE},\"application_update\":{\"url\":\"https://github.com/Torin2023/PTT2me/releases/download/v$version/PTT2me-$version-update-macos-arm64.dmg\",\"sha256\":\"$UPDATE_SHA\",\"size\":$UPDATE_SIZE},\"published_at\":\"2026-08-11T12:00:00Z\"}" \
        >"$replacement"
    rm "$SIGNED_MANIFEST"
    "$SIGNER" "$PRIVATE_KEY" "$replacement" "$SIGNED_MANIFEST"
}

write_fixture
printf '%s' extra >"$OUTPUT_DIR/extra.log"
expect_failure outputs verify_command

write_fixture
mv "$FULL_CHECKSUM" "$TEMP_ROOT/real-full-checksum-$FIXTURE_INDEX"
ln -s "$TEMP_ROOT/real-full-checksum-$FIXTURE_INDEX" "$FULL_CHECKSUM"
expect_failure outputs verify_command

write_fixture
printf '%s\n' malformed >"$FULL_CHECKSUM"
expect_failure checksum verify_command

write_fixture
printf '%064d  %s\n' 0 "$(basename -- "$FULL_DMG")" >"$FULL_CHECKSUM"
expect_failure checksum verify_command

write_fixture
printf '%s' changed >>"$FULL_DMG"
FULL_SHA="$(shasum -a 256 "$FULL_DMG" | awk '{print $1}')"
printf '%s  %s\n' "$FULL_SHA" "$(basename -- "$FULL_DMG")" >"$FULL_CHECKSUM"
expect_failure manifest verify_command

write_fixture
sed -i '' 's/"signature":"./"signature":"A/' "$SIGNED_MANIFEST"
expect_failure manifest verify_command

write_fixture
printf '%s\n' changed-key >"$FIXTURE_REPO/updates/public-key.txt"
/usr/bin/git -C "$FIXTURE_REPO" add updates/public-key.txt
/usr/bin/git -C "$FIXTURE_REPO" commit -qm changed-key
SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPO" rev-parse HEAD)"
expect_failure key verify_command

write_fixture
ORIGINAL_MODEL_MANIFEST="$TEMP_ROOT/original-model-$FIXTURE_INDEX.json"
cp "$MODEL_MANIFEST" "$ORIGINAL_MODEL_MANIFEST"
printf ' ' >>"$FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json"
/usr/bin/git -C "$FIXTURE_REPO" add models/manifests/gigaam-v3-rnnt-v1.json
/usr/bin/git -C "$FIXTURE_REPO" commit -qm changed-model
SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPO" rev-parse HEAD)"
MODEL_MANIFEST="$ORIGINAL_MODEL_MANIFEST"
expect_failure model verify_command

write_fixture
resign_payload_version 1.2.0
expect_failure identity verify_command

write_fixture
TEST_SOURCE_COMMIT=0000000000000000000000000000000000000000 \
    expect_failure git verify_command

write_fixture
/usr/bin/git -C "$FIXTURE_REPO" commit --allow-empty -qm newer-existing-commit
NEW_SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPO" rev-parse HEAD)"
plutil -replace PTT2meSourceCommit -string "$NEW_SOURCE_COMMIT" \
    "$IMAGE_ROOT/full/PTT2me.app/Contents/Info.plist"
plutil -replace PTT2meSourceCommit -string "$NEW_SOURCE_COMMIT" \
    "$IMAGE_ROOT/update/PTT2me.app/Contents/Info.plist"
TEST_SOURCE_COMMIT="$NEW_SOURCE_COMMIT" expect_failure identity verify_command

write_fixture
MALICIOUS_VERIFIER="$TEMP_ROOT/malicious-verifier-$FIXTURE_INDEX"
printf '%s\n' \
    '#!/bin/bash' \
    'printf "version=1.1.0\nsource_commit=%s\n" "$FAKE_SIGNED_SOURCE_COMMIT"' \
    >"$MALICIOUS_VERIFIER"
chmod 755 "$MALICIOUS_VERIFIER"
PTT2ME_MANIFEST_VERIFIER="$MALICIOUS_VERIFIER" \
    FAKE_SIGNED_SOURCE_COMMIT="$SOURCE_COMMIT" \
    expect_failure environment verify_command

write_fixture
PTT2ME_MANIFEST_LIBRARY_PATH="$TEMP_ROOT/untrusted-library-path" \
    expect_failure environment verify_command

write_fixture
/usr/bin/git -C "$FIXTURE_REPO" tag -d v1.1.0 >/dev/null
/usr/bin/git -C "$FIXTURE_REPO" branch v1.1.0 "$SOURCE_COMMIT"
TEST_EXPECTED_TAG=v1.1.0 expect_failure git verify_command

write_fixture
/usr/bin/git -C "$FIXTURE_REPO" commit --allow-empty -qm moved-tag
/usr/bin/git -C "$FIXTURE_REPO" tag -f v1.1.0 >/dev/null
TEST_EXPECTED_TAG=v1.1.0 expect_failure git verify_command

write_fixture
rm -rf "$IMAGE_ROOT/full/PTT2me.app/Contents/Resources/models"
expect_failure bundle verify_command
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

write_fixture
mkdir -p "$IMAGE_ROOT/update/PTT2me.app/Contents/Resources/models"
expect_failure bundle verify_command
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

write_fixture
plutil -replace CFBundleShortVersionString -string 1.2.0 \
    "$IMAGE_ROOT/full/PTT2me.app/Contents/Info.plist"
expect_failure identity verify_command
expect_no_mounts

write_fixture
plutil -replace CFBundleVersion -string 202608111201 \
    "$IMAGE_ROOT/full/PTT2me.app/Contents/Info.plist"
expect_failure identity verify_command
expect_no_mounts

write_fixture
plutil -replace PTT2meSourceCommit -string 1111111111111111111111111111111111111111 \
    "$IMAGE_ROOT/update/PTT2me.app/Contents/Info.plist"
expect_failure identity verify_command
expect_no_mounts

write_fixture
FAKE_HDIUTIL_FAIL=verify-full expect_failure dmg verify_command

write_fixture
FAKE_HDIUTIL_FAIL=attach-update expect_failure mount verify_command
expect_detach_logged full
expect_no_mounts

write_fixture
FAKE_BUNDLE_FAIL_VARIANT=full expect_failure bundle verify_command
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

write_fixture
FAKE_PARITY_FAIL=1 expect_failure parity verify_command
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

write_fixture
FAKE_SMOKE_FAIL=1 expect_failure bundle verify_command
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

write_fixture
FAKE_HDIUTIL_FAIL=detach-update expect_failure cleanup verify_command
expect_detach_logged full
expect_detach_logged update

write_fixture
expect_signal_cleanup

write_fixture
TEST_EXPECTED_TAG=v1.1.0
BEFORE="$(output_fingerprint)"
OUTPUT="$(verify_command 2>&1)" || {
    echo "positive verifier fixture failed: $OUTPUT" >&2
    exit 1
}
AFTER="$(output_fingerprint)"
[[ "$BEFORE" == "$AFTER" ]] || {
    echo "verifier modified the positive release output set" >&2
    exit 1
}
[[ "$OUTPUT" == *"PTT2me release artifacts verified: 1.1.0 ($SOURCE_COMMIT)"* ]] || {
    echo "positive verifier output was unexpected: $OUTPUT" >&2
    exit 1
}
expect_detach_logged full
expect_detach_logged update
expect_no_mounts

echo "Release artifact verifier contract checks passed"
