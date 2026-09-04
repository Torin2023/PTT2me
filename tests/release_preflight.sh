#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
PREFLIGHT_SOURCE="$REPO_ROOT/scripts/release-preflight.sh"

[[ -x "$PREFLIGHT_SOURCE" ]] || {
    echo "release preflight script is missing or not executable: $PREFLIGHT_SOURCE" >&2
    exit 1
}

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-release-preflight-tests.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT
FIXTURE_INDEX=0

link_tool() {
    local name="$1"
    local source
    source="$(type -P "$name")"
    ln -s "$source" "$FAKE_BIN/$name"
}

write_fake_tools() {
    FAKE_BIN="$TEMP_ROOT/fake-bin-$FIXTURE_INDEX"
    FAKE_TOOL_LOG="$TEMP_ROOT/fake-tools-$FIXTURE_INDEX.log"
    mkdir -p "$FAKE_BIN"
    : >"$FAKE_TOOL_LOG"

    for tool in awk base64 basename cmp date dirname find git mktemp plutil printf pwd shasum stat tr wc; do
        link_tool "$tool"
    done

    printf '%s\n' \
        '#!/bin/bash' \
        'printf "%s\n" "${FAKE_ARCH:-arm64}"' >"$FAKE_BIN/uname"
    printf '%s\n' \
        '#!/bin/bash' \
        '[[ "$1" == "-productVersion" ]] || exit 91' \
        'printf "%s\n" "${FAKE_MACOS:-14.0}"' >"$FAKE_BIN/sw_vers"
    printf '%s\n' \
        '#!/bin/bash' \
        'printf "rustc %s (fixture 2026-08-01)\n" "${FAKE_RUSTC_VERSION:-1.94.0}"' \
        >"$FAKE_BIN/rustc"
    printf '%s\n' \
        '#!/bin/bash' \
        'echo "$*" >>"$FAKE_TOOL_LOG"' \
        'if [[ "$1" == "check" && "${FAKE_CARGO_FAIL_CHECK:-0}" == "1" ]]; then exit 71; fi' \
        'if [[ "$1" == "run" ]]; then' \
        '  output="${!#}"' \
        '  if [[ -n "${FAKE_DERIVED_KEY_CONTENT:-}" ]]; then' \
        '    printf "%s\n" "$FAKE_DERIVED_KEY_CONTENT" >"$output"' \
        '  else' \
        '    /bin/cp "$FAKE_PUBLIC_KEY_SOURCE" "$output"' \
        '  fi' \
        'fi' \
        'if [[ "$1" == "test" && "${FAKE_CARGO_FAIL_APPKIT:-0}" == "1" ]]; then exit 72; fi' \
        'exit 0' >"$FAKE_BIN/cargo"
    printf '%s\n' \
        '#!/bin/bash' \
        'available="${FAKE_AVAILABLE_KB:-4194304}"' \
        'printf "%s\n" "Filesystem 1024-blocks Used Available Capacity Mounted on"' \
        'printf "/dev/fixture 8388608 0 %s 0%% /fixture\n" "$available"' \
        >"$FAKE_BIN/df"
    for tool in hdiutil codesign lipo otool; do
        printf '%s\n' '#!/bin/bash' 'exit 0' >"$FAKE_BIN/$tool"
    done
    chmod 755 \
        "$FAKE_BIN/uname" "$FAKE_BIN/sw_vers" "$FAKE_BIN/rustc" \
        "$FAKE_BIN/cargo" "$FAKE_BIN/df" \
        "$FAKE_BIN/hdiutil" "$FAKE_BIN/codesign" "$FAKE_BIN/lipo" "$FAKE_BIN/otool"
}

write_fixture_repo() {
    FIXTURE_INDEX=$((FIXTURE_INDEX + 1))
    FIXTURE_REPO="$TEMP_ROOT/repo-$FIXTURE_INDEX"
    MODEL_SOURCE="$TEMP_ROOT/model-$FIXTURE_INDEX"
    EXTERNAL_INPUTS="$TEMP_ROOT/external-$FIXTURE_INDEX"
    OUTPUT_DIR="$TEMP_ROOT/output-$FIXTURE_INDEX"
    mkdir -p \
        "$FIXTURE_REPO/scripts" \
        "$FIXTURE_REPO/models/manifests" \
        "$FIXTURE_REPO/updates" \
        "$FIXTURE_REPO/src/bin" \
        "$MODEL_SOURCE" "$EXTERNAL_INPUTS" "$OUTPUT_DIR"

    cp "$PREFLIGHT_SOURCE" "$FIXTURE_REPO/scripts/release-preflight.sh"
    cp "$REPO_ROOT/scripts/check-model-variant.sh" \
        "$FIXTURE_REPO/scripts/check-model-variant.sh"
    chmod 755 "$FIXTURE_REPO/scripts"/*.sh

    printf '%s\n' \
        '[package]' \
        'name = "ptt2me"' \
        'version = "1.1.0"' \
        'edition = "2021"' >"$FIXTURE_REPO/Cargo.toml"
    printf '%s\n' \
        'version = 4' \
        '' \
        '[[package]]' \
        'name = "ptt2me"' \
        'version = "1.1.0"' >"$FIXTURE_REPO/Cargo.lock"
    printf '%s\n' \
        '[toolchain]' \
        'channel = "1.94.0"' \
        'profile = "minimal"' >"$FIXTURE_REPO/rust-toolchain.toml"
    printf '%s\n' 'fn main() {}' >"$FIXTURE_REPO/src/main.rs"
    printf '%s\n' 'fn main() {}' >"$FIXTURE_REPO/src/bin/ptt2me-update-signer.rs"

    printf '%s' 'enc' >"$MODEL_SOURCE/encoder.int8.onnx"
    printf '%s' 'dec' >"$MODEL_SOURCE/decoder.onnx"
    printf '%s' 'join' >"$MODEL_SOURCE/joiner.onnx"
    printf '%s' 'tok' >"$MODEL_SOURCE/tokens.txt"
    chmod 600 "$MODEL_SOURCE"/*
    MODEL_MANIFEST="$FIXTURE_REPO/models/manifests/gigaam-v3-rnnt-v1.json"
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

    PUBLIC_KEY="$EXTERNAL_INPUTS/public-key.txt"
    PRIVATE_KEY="$EXTERNAL_INPUTS/private-key.txt"
    printf '%s\n' 'AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=' >"$PUBLIC_KEY"
    cp "$PUBLIC_KEY" "$FIXTURE_REPO/updates/public-key.txt"
    printf '%s\n' 'QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=' >"$PRIVATE_KEY"
    chmod 600 "$PRIVATE_KEY"

    /usr/bin/git -C "$FIXTURE_REPO" init -q
    /usr/bin/git -C "$FIXTURE_REPO" config user.email fixture@ptt2me.invalid
    /usr/bin/git -C "$FIXTURE_REPO" config user.name 'PTT2me Fixture'
    /usr/bin/git -C "$FIXTURE_REPO" add .
    /usr/bin/git -C "$FIXTURE_REPO" commit -qm fixture
    SOURCE_COMMIT="$(/usr/bin/git -C "$FIXTURE_REPO" rev-parse HEAD)"
    write_fake_tools
}

preflight_command() {
    env \
        PATH="$FAKE_BIN:/bin" \
        FAKE_TOOL_LOG="$FAKE_TOOL_LOG" \
        FAKE_PUBLIC_KEY_SOURCE="$PUBLIC_KEY" \
        "$FIXTURE_REPO/scripts/release-preflight.sh" \
        --version "${TEST_VERSION:-1.1.0}" \
        --build "${TEST_BUILD:-202608111200}" \
        --source-commit "${TEST_SOURCE_COMMIT:-$SOURCE_COMMIT}" \
        --model-manifest "${TEST_MODEL_MANIFEST:-$MODEL_MANIFEST}" \
        --model-source "${TEST_MODEL_SOURCE:-$MODEL_SOURCE}" \
        --public-key "${TEST_PUBLIC_KEY:-$PUBLIC_KEY}" \
        --private-key "${TEST_PRIVATE_KEY:-$PRIVATE_KEY}" \
        --published-at "${TEST_PUBLISHED_AT:-2026-08-11T12:00:00Z}" \
        --output-dir "${TEST_OUTPUT_DIR:-$OUTPUT_DIR}"
}

expect_failure() {
    local category="$1"
    shift
    local output
    if output="$("$@" 2>&1)"; then
        echo "expected failure [$category]: $*" >&2
        exit 1
    fi
    [[ "$output" == *"[$category]"* ]] || {
        echo "failure did not contain [$category]: $output" >&2
        exit 1
    }
    [[ "$output" != *'QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI='* ]] || {
        echo "failure leaked private key bytes" >&2
        exit 1
    }
}

write_fixture_repo
TEST_VERSION='1.1' expect_failure arguments preflight_command

write_fixture_repo
TEST_BUILD='202602300101' expect_failure arguments preflight_command

write_fixture_repo
TEST_SOURCE_COMMIT='ABC' expect_failure arguments preflight_command

write_fixture_repo
TEST_PUBLISHED_AT='2026-02-29T12:00:00Z' expect_failure arguments preflight_command

write_fixture_repo
FAKE_ARCH='x86_64' expect_failure environment preflight_command

write_fixture_repo
FAKE_MACOS='12.6' expect_failure environment preflight_command

write_fixture_repo
FAKE_RUSTC_VERSION='1.93.0' expect_failure toolchain preflight_command

write_fixture_repo
printf '%s\n' '# dirty' >>"$FIXTURE_REPO/Cargo.toml"
expect_failure git preflight_command

write_fixture_repo
TEST_SOURCE_COMMIT='0000000000000000000000000000000000000000' \
    expect_failure git preflight_command

write_fixture_repo
sed -i '' 's/version = "1.1.0"/version = "1.2.0"/' "$FIXTURE_REPO/Cargo.lock"
expect_failure identity preflight_command

write_fixture_repo
REAL_OUTPUT="$TEMP_ROOT/real-output-$FIXTURE_INDEX"
mkdir "$REAL_OUTPUT"
ln -s "$REAL_OUTPUT" "$TEMP_ROOT/output-link-$FIXTURE_INDEX"
TEST_OUTPUT_DIR="$TEMP_ROOT/output-link-$FIXTURE_INDEX" \
    expect_failure output preflight_command

write_fixture_repo
touch "$OUTPUT_DIR/PTT2me-1.1.0-full-macos-arm64.dmg"
expect_failure output preflight_command

write_fixture_repo
touch "$OUTPUT_DIR/unrelated.txt"
expect_failure output preflight_command

write_fixture_repo
FAKE_AVAILABLE_KB=1024 expect_failure disk preflight_command

write_fixture_repo
printf '%s\n' 'different-public-key' >"$TEMP_ROOT/wrong-public-$FIXTURE_INDEX"
TEST_PUBLIC_KEY="$TEMP_ROOT/wrong-public-$FIXTURE_INDEX" \
    expect_failure key preflight_command

write_fixture_repo
printf '%s\n' 'inside-repository-secret' >"$FIXTURE_REPO/private-key.txt"
chmod 600 "$FIXTURE_REPO/private-key.txt"
TEST_PRIVATE_KEY="$FIXTURE_REPO/private-key.txt" expect_failure key preflight_command

write_fixture_repo
SIBLING_WORKTREE="$TEMP_ROOT/sibling-worktree-$FIXTURE_INDEX"
/usr/bin/git -C "$FIXTURE_REPO" worktree add -q \
    -b "fixture-sibling-$FIXTURE_INDEX" "$SIBLING_WORKTREE"
printf '%s\n' 'sibling-worktree-secret' >"$SIBLING_WORKTREE/private-key.txt"
chmod 600 "$SIBLING_WORKTREE/private-key.txt"
TEST_PRIVATE_KEY="$SIBLING_WORKTREE/private-key.txt" expect_failure key preflight_command

write_fixture_repo
chmod 644 "$PRIVATE_KEY"
expect_failure key preflight_command

write_fixture_repo
chmod +a 'everyone allow read' "$PRIVATE_KEY"
expect_failure key preflight_command

write_fixture_repo
FAKE_DERIVED_KEY_CONTENT='BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB' \
    expect_failure key preflight_command

write_fixture_repo
MUTATED_MANIFEST="$EXTERNAL_INPUTS/mutated-model.json"
cp "$MODEL_MANIFEST" "$MUTATED_MANIFEST"
printf ' ' >>"$MUTATED_MANIFEST"
TEST_MODEL_MANIFEST="$MUTATED_MANIFEST" expect_failure model preflight_command

write_fixture_repo
rm "$MODEL_SOURCE/tokens.txt"
expect_failure model preflight_command

write_fixture_repo
printf '%s' extra >"$MODEL_SOURCE/extra.bin"
expect_failure model preflight_command

write_fixture_repo
mv "$MODEL_SOURCE/tokens.txt" "$EXTERNAL_INPUTS/tokens.txt"
ln -s "$EXTERNAL_INPUTS/tokens.txt" "$MODEL_SOURCE/tokens.txt"
expect_failure model preflight_command

write_fixture_repo
chmod 700 "$MODEL_SOURCE/tokens.txt"
expect_failure model preflight_command

write_fixture_repo
printf '%s' bad >"$MODEL_SOURCE/tokens.txt"
expect_failure model preflight_command

write_fixture_repo
rm "$FAKE_BIN/otool"
expect_failure tools preflight_command

write_fixture_repo
FAKE_CARGO_FAIL_CHECK=1 expect_failure toolchain preflight_command

write_fixture_repo
FAKE_CARGO_FAIL_APPKIT=1 expect_failure appkit preflight_command

write_fixture_repo
OUTPUT="$(preflight_command 2>&1)" || {
    echo "positive preflight fixture failed: $OUTPUT" >&2
    exit 1
}
[[ "$OUTPUT" == *'PTT2me release preflight passed: 1.1.0'* ]] || {
    echo "positive preflight output was unexpected: $OUTPUT" >&2
    exit 1
}
[[ "$(sed -n '1p' "$FAKE_TOOL_LOG")" == 'check --locked --bins' ]] || {
    echo "preflight must check signer/verifier binaries before AppKit" >&2
    exit 1
}
[[ "$(sed -n '2p' "$FAKE_TOOL_LOG")" == \
    run\ --quiet\ --locked\ --bin\ ptt2me-update-signer\ --\ --derive-public-key* ]] || {
    echo "preflight must derive the public key from the supplied private key" >&2
    exit 1
}
[[ "$(sed -n '3p' "$FAKE_TOOL_LOG")" == \
    'test --locked --test pasteboard_main --features test-support -- --test-threads=1' ]] || {
    echo "preflight must run the dedicated AppKit test last" >&2
    exit 1
}
[[ "$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" == '0' ]] || {
    echo "preflight must not create release outputs" >&2
    exit 1
}

echo "Release preflight contract checks passed"
