#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly MODEL_ID="gigaam-v3-rnnt-v1"
readonly MINIMUM_MACOS_MAJOR=13
readonly FIXED_RESERVE_BYTES=1073741824

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

fail() {
    local category="$1"
    shift
    echo "PTT2me release preflight failed [$category]: $*" >&2
    exit 1
}

usage() {
    fail arguments "usage: $0 --version X.Y.Z --build YYYYMMDDHHMM --source-commit COMMIT --model-manifest PATH --model-source PATH --public-key PATH --private-key PATH --published-at YYYY-MM-DDTHH:MM:SSZ --output-dir PATH"
}

canonical_file() {
    local category="$1"
    local label="$2"
    local path="$3"
    local parent
    [[ -f "$path" && ! -L "$path" ]] || fail "$category" "$label is not a real file: $path"
    parent="$(cd -- "$(dirname -- "$path")" && pwd -P)" ||
        fail "$category" "could not resolve $label parent: $path"
    printf '%s/%s\n' "$parent" "$(basename -- "$path")"
}

canonical_directory() {
    local category="$1"
    local label="$2"
    local path="$3"
    [[ -d "$path" && ! -L "$path" ]] ||
        fail "$category" "$label is not a real directory: $path"
    (cd -- "$path" && pwd -P) || fail "$category" "could not resolve $label: $path"
}

manifest_value() {
    local keypath="$1"
    local expected_type="$2"
    plutil -extract "$keypath" raw -expect "$expected_type" -o - -- "$MODEL_MANIFEST" 2>/dev/null ||
        fail model "invalid production model manifest field: $keypath"
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
    fail arguments "version must be a canonical stable semantic version: $VERSION"
[[ "$BUILD" =~ ^[0-9]{12}$ ]] ||
    fail arguments "build must be a 12-digit UTC calendar minute: $BUILD"
PARSED_BUILD="$(/bin/date -u -j -f '%Y%m%d%H%M' "$BUILD" '+%Y%m%d%H%M' 2>/dev/null)" ||
    fail arguments "build must be a valid UTC calendar minute: $BUILD"
[[ "$PARSED_BUILD" == "$BUILD" ]] ||
    fail arguments "build must be a valid UTC calendar minute: $BUILD"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail arguments "source commit must be exactly 40 lowercase hexadecimal characters"
[[ "$PUBLISHED_AT" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] ||
    fail arguments "published-at must be an exact UTC timestamp: $PUBLISHED_AT"
PARSED_PUBLISHED_AT="$(/bin/date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$PUBLISHED_AT" '+%Y-%m-%dT%H:%M:%SZ' 2>/dev/null)" ||
    fail arguments "published-at must be a valid UTC timestamp: $PUBLISHED_AT"
[[ "$PARSED_PUBLISHED_AT" == "$PUBLISHED_AT" ]] ||
    fail arguments "published-at must be a valid UTC timestamp: $PUBLISHED_AT"

MODEL_MANIFEST="$(canonical_file model "production model manifest" "$MODEL_MANIFEST")"
MODEL_SOURCE="$(canonical_directory model "production model source" "$MODEL_SOURCE")"
PUBLIC_KEY="$(canonical_file key "production public key" "$PUBLIC_KEY")"
PRIVATE_KEY="$(canonical_file key "production private key" "$PRIVATE_KEY")"
OUTPUT_DIR="$(canonical_directory output "output directory" "$OUTPUT_DIR")"
readonly MODEL_MANIFEST MODEL_SOURCE PUBLIC_KEY PRIVATE_KEY OUTPUT_DIR

case "$PRIVATE_KEY" in
    "$REPO_ROOT" | "$REPO_ROOT"/*)
        fail key "production private key must be stored outside the Git repository: $PRIVATE_KEY"
        ;;
esac
PRIVATE_MODE="$(stat -f '%Lp' "$PRIVATE_KEY" 2>/dev/null)" ||
    fail key "could not inspect private key permissions: $PRIVATE_KEY"
[[ "$PRIVATE_MODE" =~ ^[0-7]{3,4}$ ]] ||
    fail key "private key has an invalid permission mode: $PRIVATE_KEY"
[[ $((8#$PRIVATE_MODE & 077)) -eq 0 ]] ||
    fail key "private key permissions must deny group and other access: $PRIVATE_KEY"

for command_name in \
    awk base64 cargo codesign cmp df find git hdiutil lipo otool plutil rustc \
    shasum stat tr wc; do
    command -v "$command_name" >/dev/null 2>&1 ||
        fail tools "required command is unavailable: $command_name"
done
[[ -x /usr/libexec/PlistBuddy ]] ||
    fail tools "required command is unavailable: /usr/libexec/PlistBuddy"

[[ "$(uname -m)" == "arm64" ]] ||
    fail environment "an Apple Silicon (arm64) Mac is required"
MACOS_VERSION="$(sw_vers -productVersion 2>/dev/null)" ||
    fail environment "could not determine the macOS version"
[[ "$MACOS_VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(\.(0|[1-9][0-9]*))?$ ]] ||
    fail environment "macOS version is not canonical: $MACOS_VERSION"
MACOS_MAJOR="${MACOS_VERSION%%.*}"
[[ "$MACOS_MAJOR" -ge "$MINIMUM_MACOS_MAJOR" ]] ||
    fail environment "macOS 13.0 or newer is required, found $MACOS_VERSION"

TOOLCHAIN_CHANNEL="$(awk -F '"' '/^[[:space:]]*channel = "/ { print $2; exit }' "$REPO_ROOT/rust-toolchain.toml")"
[[ "$TOOLCHAIN_CHANNEL" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
    fail toolchain "could not read an exact channel from rust-toolchain.toml"
ACTIVE_RUSTC="$(rustc --version 2>/dev/null | awk '{print $2}')" ||
    fail toolchain "could not inspect the active Rust toolchain"
[[ "$ACTIVE_RUSTC" == "$TOOLCHAIN_CHANNEL" ]] ||
    fail toolchain "active rustc $ACTIVE_RUSTC does not match rust-toolchain.toml $TOOLCHAIN_CHANNEL"

CARGO_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' "$REPO_ROOT/Cargo.toml")"
LOCK_VERSION="$(awk -F '"' '
    $0 == "name = \"ptt2me\"" { package = 1; next }
    package && /^version = "/ { print $2; exit }
' "$REPO_ROOT/Cargo.lock")"
[[ "$VERSION" == "$CARGO_VERSION" && "$VERSION" == "$LOCK_VERSION" ]] ||
    fail identity "release version must match Cargo.toml and Cargo.lock: $VERSION"

cd -- "$REPO_ROOT"
HEAD_COMMIT="$(git rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" ||
    fail git "could not resolve exact git HEAD"
[[ "$SOURCE_COMMIT" == "$HEAD_COMMIT" ]] ||
    fail git "source commit must equal exact git HEAD: $SOURCE_COMMIT"
[[ -z "$(git status --porcelain=v1 --untracked-files=all)" ]] ||
    fail git "release source tree must be clean: $REPO_ROOT"

COMMITTED_PUBLIC_KEY="$REPO_ROOT/updates/public-key.txt"
git cat-file -e 'HEAD:updates/public-key.txt' 2>/dev/null ||
    fail key "updates/public-key.txt must be tracked in git HEAD"
git diff --quiet HEAD -- updates/public-key.txt ||
    fail key "updates/public-key.txt worktree bytes must equal git HEAD"
cmp -s "$COMMITTED_PUBLIC_KEY" "$PUBLIC_KEY" ||
    fail key "public key must match updates/public-key.txt: $PUBLIC_KEY"

COMMITTED_MODEL_MANIFEST="$REPO_ROOT/models/manifests/$MODEL_ID.json"
git cat-file -e "HEAD:models/manifests/$MODEL_ID.json" 2>/dev/null ||
    fail model "production model manifest must be tracked in git HEAD"
git diff --quiet HEAD -- "models/manifests/$MODEL_ID.json" ||
    fail model "committed production model manifest worktree bytes must equal git HEAD"
cmp -s "$COMMITTED_MODEL_MANIFEST" "$MODEL_MANIFEST" ||
    fail model "model manifest must match the committed exact bytes: $MODEL_MANIFEST"
"$SCRIPT_DIR/check-model-variant.sh" \
    --variant full \
    --model-manifest "$MODEL_MANIFEST" \
    --model-source "$MODEL_SOURCE" ||
    fail model "production model source failed exact layout verification: $MODEL_SOURCE"

TOTAL_MODEL_SIZE=0
for index in 0 1 2 3; do
    MODEL_SIZE="$(manifest_value "files.$index.size" integer)"
    [[ "$MODEL_SIZE" =~ ^[1-9][0-9]*$ ]] ||
        fail model "model manifest contains an invalid size at files.$index.size"
    TOTAL_MODEL_SIZE=$((TOTAL_MODEL_SIZE + MODEL_SIZE))
done
REQUIRED_BYTES=$((TOTAL_MODEL_SIZE * 2 + FIXED_RESERVE_BYTES))
REQUIRED_KB=$(((REQUIRED_BYTES + 1023) / 1024))
AVAILABLE_KB="$(df -Pk "$OUTPUT_DIR" 2>/dev/null | awk 'NR == 2 { print $4 }')" ||
    fail disk "could not inspect available space for output workspace: $OUTPUT_DIR"
[[ "$AVAILABLE_KB" =~ ^[0-9]+$ ]] ||
    fail disk "available space is not measurable for output workspace: $OUTPUT_DIR"
[[ "$AVAILABLE_KB" -ge "$REQUIRED_KB" ]] ||
    fail disk "output workspace requires at least $REQUIRED_BYTES bytes: $OUTPUT_DIR"

FULL_DMG_NAME="$PRODUCT-$VERSION-full-macos-arm64.dmg"
UPDATE_DMG_NAME="$PRODUCT-$VERSION-update-macos-arm64.dmg"
SIGNED_MANIFEST_NAME="$PRODUCT-$VERSION-signed-update-manifest.json"
for output_name in \
    "$FULL_DMG_NAME" "$FULL_DMG_NAME.sha256" \
    "$UPDATE_DMG_NAME" "$UPDATE_DMG_NAME.sha256" \
    "$SIGNED_MANIFEST_NAME"; do
    [[ ! -e "$OUTPUT_DIR/$output_name" && ! -L "$OUTPUT_DIR/$output_name" ]] ||
        fail output "release output already exists: $OUTPUT_DIR/$output_name"
done

[[ -f "$REPO_ROOT/src/main.rs" && ! -L "$REPO_ROOT/src/main.rs" ]] ||
    fail toolchain "manifest verifier source is unavailable: $REPO_ROOT/src/main.rs"
[[ -f "$REPO_ROOT/src/bin/ptt2me-update-signer.rs" && \
    ! -L "$REPO_ROOT/src/bin/ptt2me-update-signer.rs" ]] ||
    fail toolchain "manifest signer source is unavailable: $REPO_ROOT/src/bin/ptt2me-update-signer.rs"
cargo check --locked --bins ||
    fail toolchain "signer/verifier binaries failed cargo check"
cargo test --locked --test pasteboard_main --features test-support -- --test-threads=1 ||
    fail appkit "dedicated NSPasteboard test requires a passing GUI/AppKit session"

echo "PTT2me release preflight passed: $VERSION ($SOURCE_COMMIT)"
