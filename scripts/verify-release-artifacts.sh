#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly MODEL_ID="gigaam-v3-rnnt-v1"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

fail() {
    local category="$1"
    shift
    echo "PTT2me release artifact verification failed [$category]: $*" >&2
    exit 1
}

usage() {
    fail arguments "usage: $0 --version X.Y.Z --source-commit COMMIT [--expected-tag vX.Y.Z] --full-dmg PATH --full-checksum PATH --update-dmg PATH --update-checksum PATH --manifest PATH --public-key PATH --model-manifest PATH"
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

payload_value() {
    local keypath="$1"
    local expected_type="$2"
    plutil -extract "$keypath" raw -expect "$expected_type" -o - -- "$PAYLOAD_PATH" 2>/dev/null ||
        fail manifest "verified signed payload has an invalid field: $keypath"
}

VERSION=""
SOURCE_COMMIT=""
EXPECTED_TAG=""
FULL_DMG=""
FULL_CHECKSUM=""
UPDATE_DMG=""
UPDATE_CHECKSUM=""
SIGNED_MANIFEST=""
PUBLIC_KEY=""
MODEL_MANIFEST=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            [[ $# -ge 2 ]] || usage
            VERSION="$2"
            shift 2
            ;;
        --source-commit)
            [[ $# -ge 2 ]] || usage
            SOURCE_COMMIT="$2"
            shift 2
            ;;
        --expected-tag)
            [[ $# -ge 2 ]] || usage
            EXPECTED_TAG="$2"
            shift 2
            ;;
        --full-dmg)
            [[ $# -ge 2 ]] || usage
            FULL_DMG="$2"
            shift 2
            ;;
        --full-checksum)
            [[ $# -ge 2 ]] || usage
            FULL_CHECKSUM="$2"
            shift 2
            ;;
        --update-dmg)
            [[ $# -ge 2 ]] || usage
            UPDATE_DMG="$2"
            shift 2
            ;;
        --update-checksum)
            [[ $# -ge 2 ]] || usage
            UPDATE_CHECKSUM="$2"
            shift 2
            ;;
        --manifest)
            [[ $# -ge 2 ]] || usage
            SIGNED_MANIFEST="$2"
            shift 2
            ;;
        --public-key)
            [[ $# -ge 2 ]] || usage
            PUBLIC_KEY="$2"
            shift 2
            ;;
        --model-manifest)
            [[ $# -ge 2 ]] || usage
            MODEL_MANIFEST="$2"
            shift 2
            ;;
        *) usage ;;
    esac
done

[[ -n "$VERSION" && -n "$SOURCE_COMMIT" ]] || usage
[[ -n "$FULL_DMG" && -n "$FULL_CHECKSUM" ]] || usage
[[ -n "$UPDATE_DMG" && -n "$UPDATE_CHECKSUM" ]] || usage
[[ -n "$SIGNED_MANIFEST" && -n "$PUBLIC_KEY" && -n "$MODEL_MANIFEST" ]] || usage
[[ "$VERSION" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]] ||
    fail arguments "version must be a canonical stable semantic version: $VERSION"
[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] ||
    fail arguments "source commit must be exactly 40 lowercase hexadecimal characters"
if [[ -n "$EXPECTED_TAG" && "$EXPECTED_TAG" != "v$VERSION" ]]; then
    fail arguments "expected tag must be exactly v$VERSION: $EXPECTED_TAG"
fi
[[ -z "${PTT2ME_MANIFEST_VERIFIER:-}" ]] ||
    fail environment "PTT2ME_MANIFEST_VERIFIER must be unset for production verification"
[[ -z "${PTT2ME_MANIFEST_LIBRARY_PATH:-}" ]] ||
    fail environment "PTT2ME_MANIFEST_LIBRARY_PATH must be unset for production verification"

for command_name in awk base64 cmp find git hdiutil plutil readlink shasum stat tr wc; do
    command -v "$command_name" >/dev/null 2>&1 ||
        fail tools "required command is unavailable: $command_name"
done

FULL_DMG="$(canonical_file outputs "Full DMG" "$FULL_DMG")"
FULL_CHECKSUM="$(canonical_file outputs "Full checksum" "$FULL_CHECKSUM")"
UPDATE_DMG="$(canonical_file outputs "Update DMG" "$UPDATE_DMG")"
UPDATE_CHECKSUM="$(canonical_file outputs "Update checksum" "$UPDATE_CHECKSUM")"
SIGNED_MANIFEST="$(canonical_file outputs "signed update manifest" "$SIGNED_MANIFEST")"
PUBLIC_KEY="$(canonical_file key "public key" "$PUBLIC_KEY")"
MODEL_MANIFEST="$(canonical_file model "production model manifest" "$MODEL_MANIFEST")"
readonly FULL_DMG FULL_CHECKSUM UPDATE_DMG UPDATE_CHECKSUM SIGNED_MANIFEST
readonly PUBLIC_KEY MODEL_MANIFEST

OUTPUT_DIR="$(dirname -- "$FULL_DMG")"
for release_path in "$FULL_CHECKSUM" "$UPDATE_DMG" "$UPDATE_CHECKSUM" "$SIGNED_MANIFEST"; do
    [[ "$(dirname -- "$release_path")" == "$OUTPUT_DIR" ]] ||
        fail outputs "all release outputs must share one closed directory: $release_path"
done

EXPECTED_FULL_NAME="$PRODUCT-$VERSION-full-macos-arm64.dmg"
EXPECTED_UPDATE_NAME="$PRODUCT-$VERSION-update-macos-arm64.dmg"
EXPECTED_MANIFEST_NAME="$PRODUCT-$VERSION-signed-update-manifest.json"
[[ "$(basename -- "$FULL_DMG")" == "$EXPECTED_FULL_NAME" ]] ||
    fail outputs "unexpected Full DMG name: $FULL_DMG"
[[ "$(basename -- "$FULL_CHECKSUM")" == "$EXPECTED_FULL_NAME.sha256" ]] ||
    fail outputs "unexpected Full checksum name: $FULL_CHECKSUM"
[[ "$(basename -- "$UPDATE_DMG")" == "$EXPECTED_UPDATE_NAME" ]] ||
    fail outputs "unexpected Update DMG name: $UPDATE_DMG"
[[ "$(basename -- "$UPDATE_CHECKSUM")" == "$EXPECTED_UPDATE_NAME.sha256" ]] ||
    fail outputs "unexpected Update checksum name: $UPDATE_CHECKSUM"
[[ "$(basename -- "$SIGNED_MANIFEST")" == "$EXPECTED_MANIFEST_NAME" ]] ||
    fail outputs "unexpected signed manifest name: $SIGNED_MANIFEST"
OUTPUT_COUNT="$(find "$OUTPUT_DIR" -mindepth 1 -maxdepth 1 -exec printf x \; | wc -c | tr -d ' ')"
[[ "$OUTPUT_COUNT" == "5" ]] ||
    fail outputs "release output directory must contain exactly five entries: $OUTPUT_DIR"

COMMITTED_PUBLIC_KEY="$REPO_ROOT/updates/public-key.txt"
[[ -f "$COMMITTED_PUBLIC_KEY" && ! -L "$COMMITTED_PUBLIC_KEY" ]] ||
    fail key "committed public key is unavailable: $COMMITTED_PUBLIC_KEY"
git -C "$REPO_ROOT" cat-file -e "$SOURCE_COMMIT^{commit}" 2>/dev/null ||
    fail git "expected source commit is unavailable in the verifier repository: $SOURCE_COMMIT"
HEAD_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify 'HEAD^{commit}' 2>/dev/null)" ||
    fail git "could not resolve exact verifier git HEAD"
[[ "$HEAD_COMMIT" == "$SOURCE_COMMIT" ]] ||
    fail git "verifier git HEAD must equal the expected source commit: $SOURCE_COMMIT"
[[ -z "$(git -C "$REPO_ROOT" status --porcelain=v1 --untracked-files=all)" ]] ||
    fail git "verifier source tree must be clean: $REPO_ROOT"
git -C "$REPO_ROOT" cat-file -e "$SOURCE_COMMIT:updates/public-key.txt" 2>/dev/null ||
    fail key "expected source commit does not contain updates/public-key.txt"
git -C "$REPO_ROOT" diff --quiet "$SOURCE_COMMIT" -- updates/public-key.txt ||
    fail key "updates/public-key.txt does not match the expected source commit"
cmp -s "$COMMITTED_PUBLIC_KEY" "$PUBLIC_KEY" ||
    fail key "public key must match updates/public-key.txt: $PUBLIC_KEY"
COMMITTED_MODEL_MANIFEST="$REPO_ROOT/models/manifests/$MODEL_ID.json"
[[ -f "$COMMITTED_MODEL_MANIFEST" && ! -L "$COMMITTED_MODEL_MANIFEST" ]] ||
    fail model "committed production model manifest is unavailable: $COMMITTED_MODEL_MANIFEST"
git -C "$REPO_ROOT" cat-file -e "$SOURCE_COMMIT:models/manifests/$MODEL_ID.json" 2>/dev/null ||
    fail model "expected source commit does not contain the production model manifest"
git -C "$REPO_ROOT" diff --quiet "$SOURCE_COMMIT" -- "models/manifests/$MODEL_ID.json" ||
    fail model "production model manifest does not match the expected source commit"
cmp -s "$COMMITTED_MODEL_MANIFEST" "$MODEL_MANIFEST" ||
    fail model "model manifest must match the committed exact bytes: $MODEL_MANIFEST"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-release-verify.XXXXXX")" ||
    fail tools "could not create a private verification workspace"
chmod 700 "$TEMP_ROOT"
FULL_MOUNT="$TEMP_ROOT/full-mount"
UPDATE_MOUNT="$TEMP_ROOT/update-mount"
mkdir -p "$FULL_MOUNT" "$UPDATE_MOUNT"
FULL_MOUNTED=false
UPDATE_MOUNTED=false

cleanup() {
    local original_status=$?
    set +e
    if [[ "$UPDATE_MOUNTED" == true ]]; then
        hdiutil detach "$UPDATE_MOUNT" -quiet >/dev/null 2>&1
    fi
    if [[ "$FULL_MOUNTED" == true ]]; then
        hdiutil detach "$FULL_MOUNT" -quiet >/dev/null 2>&1
    fi
    rm -rf "$TEMP_ROOT"
    exit "$original_status"
}
trap cleanup EXIT
trap 'exit 1' HUP INT TERM

verify_checksum() {
    local artifact="$1"
    local checksum_file="$2"
    local artifact_name="$3"
    local digest
    local expected_file
    digest="$(shasum -a 256 "$artifact" | awk '{print $1}')" ||
        fail checksum "could not hash release artifact: $artifact"
    [[ "$digest" =~ ^[0-9a-f]{64}$ ]] ||
        fail checksum "release artifact digest is not canonical: $artifact"
    expected_file="$TEMP_ROOT/$(basename -- "$checksum_file").expected"
    printf '%s  %s\n' "$digest" "$artifact_name" >"$expected_file"
    cmp -s "$expected_file" "$checksum_file" ||
        fail checksum "checksum file does not match release artifact: $checksum_file"
}

verify_checksum "$FULL_DMG" "$FULL_CHECKSUM" "$EXPECTED_FULL_NAME"
verify_checksum "$UPDATE_DMG" "$UPDATE_CHECKSUM" "$EXPECTED_UPDATE_NAME"

if ! VALIDATION_OUTPUT="$("$SCRIPT_DIR/validate-update-manifest.sh" "$PUBLIC_KEY" "$SIGNED_MANIFEST" "$FULL_DMG" "$UPDATE_DMG" "$MODEL_MANIFEST" 2>&1)"; then
    fail manifest "signed manifest, artifact sizes/hashes or Ed25519 signature are invalid: $VALIDATION_OUTPUT"
fi
VALIDATED_VERSION="$(printf '%s\n' "$VALIDATION_OUTPUT" | awk -F= '$1 == "version" { print $2; exit }')"
VALIDATED_SOURCE_COMMIT="$(printf '%s\n' "$VALIDATION_OUTPUT" | awk -F= '$1 == "source_commit" { print $2; exit }')"
[[ "$VALIDATED_VERSION" == "$VERSION" ]] ||
    fail identity "signed version does not match expected version: $VALIDATED_VERSION"
[[ "$VALIDATED_SOURCE_COMMIT" == "$SOURCE_COMMIT" ]] ||
    fail identity "signed source commit does not match expected source commit: $VALIDATED_SOURCE_COMMIT"

PAYLOAD_PATH="$TEMP_ROOT/release-payload.json"
PAYLOAD_BASE64="$(plutil -extract payload raw -expect string -o - -- "$SIGNED_MANIFEST" 2>/dev/null)" ||
    fail manifest "signed manifest payload envelope is invalid: $SIGNED_MANIFEST"
printf '%s' "$PAYLOAD_BASE64" | base64 -D >"$PAYLOAD_PATH" 2>/dev/null ||
    fail manifest "signed manifest payload is not canonical base64: $SIGNED_MANIFEST"

[[ "$(payload_value channel string)" == "stable" ]] ||
    fail identity "signed channel must be stable"
[[ "$(payload_value version string)" == "$VERSION" ]] ||
    fail identity "signed payload version does not match expected version"
PAYLOAD_BUILD="$(payload_value build integer)"
[[ "$PAYLOAD_BUILD" =~ ^[0-9]{12}$ ]] ||
    fail identity "signed build must be a 12-digit UTC calendar minute: $PAYLOAD_BUILD"
PARSED_BUILD="$(/bin/date -u -j -f '%Y%m%d%H%M' "$PAYLOAD_BUILD" '+%Y%m%d%H%M' 2>/dev/null)" ||
    fail identity "signed build is not a valid UTC calendar minute: $PAYLOAD_BUILD"
[[ "$PARSED_BUILD" == "$PAYLOAD_BUILD" ]] ||
    fail identity "signed build is not a valid UTC calendar minute: $PAYLOAD_BUILD"
[[ "$(payload_value source_commit string)" == "$SOURCE_COMMIT" ]] ||
    fail identity "signed payload source commit does not match expected source commit"
[[ "$(payload_value minimum_macos string)" == "13.0" ]] ||
    fail identity "signed minimum_macos must be exactly 13.0"
[[ "$(payload_value architecture string)" == "arm64" ]] ||
    fail identity "signed architecture must be arm64"
[[ "$(payload_value required_model.id string)" == "$MODEL_ID" ]] ||
    fail identity "signed required model id must be $MODEL_ID"
MODEL_MANIFEST_SHA="$(shasum -a 256 "$MODEL_MANIFEST" | awk '{print $1}')"
[[ "$(payload_value required_model.manifest_sha256 string)" == "$MODEL_MANIFEST_SHA" ]] ||
    fail identity "signed model manifest digest does not match the supplied model manifest"

if [[ -n "$EXPECTED_TAG" ]]; then
    TAG_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify \
        "refs/tags/$EXPECTED_TAG^{commit}" 2>/dev/null)" ||
        fail git "expected release tag does not resolve to a commit: $EXPECTED_TAG"
    [[ "$TAG_COMMIT" == "$SOURCE_COMMIT" ]] ||
        fail git "expected release tag does not point to the signed source commit: $EXPECTED_TAG"
fi

hdiutil verify "$FULL_DMG" >/dev/null ||
    fail dmg "hdiutil verify failed for Full DMG: $FULL_DMG"
hdiutil verify "$UPDATE_DMG" >/dev/null ||
    fail dmg "hdiutil verify failed for Update DMG: $UPDATE_DMG"
FULL_MOUNTED=true
hdiutil attach "$FULL_DMG" -readonly -nobrowse -noautoopen -mountpoint "$FULL_MOUNT" >/dev/null ||
    fail mount "could not mount Full DMG read-only: $FULL_DMG"
UPDATE_MOUNTED=true
hdiutil attach "$UPDATE_DMG" -readonly -nobrowse -noautoopen -mountpoint "$UPDATE_MOUNT" >/dev/null ||
    fail mount "could not mount Update DMG read-only: $UPDATE_DMG"

verify_mounted_root() {
    local label="$1"
    local mountpoint="$2"
    local root_count
    [[ -d "$mountpoint/$PRODUCT.app" && ! -L "$mountpoint/$PRODUCT.app" ]] ||
        fail bundle "$label DMG has no real $PRODUCT.app bundle"
    [[ -L "$mountpoint/Applications" ]] ||
        fail bundle "$label DMG has no Applications symlink"
    [[ "$(readlink "$mountpoint/Applications")" == "/Applications" ]] ||
        fail bundle "$label DMG Applications symlink has the wrong target"
    root_count="$(find "$mountpoint" -mindepth 1 -maxdepth 1 -exec printf x \; | wc -c | tr -d ' ')"
    [[ "$root_count" == "2" ]] ||
        fail bundle "$label DMG contains unexpected root entries"
}

verify_mounted_root Full "$FULL_MOUNT"
verify_mounted_root Update "$UPDATE_MOUNT"
FULL_APP="$FULL_MOUNT/$PRODUCT.app"
UPDATE_APP="$UPDATE_MOUNT/$PRODUCT.app"

verify_bundle_identity() {
    local label="$1"
    local app="$2"
    local plist="$app/Contents/Info.plist"
    local bundle_version bundle_build bundle_source_commit
    [[ -f "$plist" && ! -L "$plist" ]] ||
        fail identity "$label bundle has no real Info.plist"
    bundle_version="$(plutil -extract CFBundleShortVersionString raw -expect string -o - -- "$plist" 2>/dev/null)" ||
        fail identity "$label bundle has no valid CFBundleShortVersionString"
    bundle_build="$(plutil -extract CFBundleVersion raw -expect string -o - -- "$plist" 2>/dev/null)" ||
        fail identity "$label bundle has no valid CFBundleVersion"
    bundle_source_commit="$(plutil -extract PTT2meSourceCommit raw -expect string -o - -- "$plist" 2>/dev/null)" ||
        fail identity "$label bundle has no valid PTT2meSourceCommit"
    [[ "$bundle_version" == "$VERSION" ]] ||
        fail identity "$label bundle version does not match signed version: $bundle_version"
    [[ "$bundle_build" == "$PAYLOAD_BUILD" ]] ||
        fail identity "$label bundle build does not match signed build: $bundle_build"
    [[ "$bundle_source_commit" == "$SOURCE_COMMIT" ]] ||
        fail identity "$label bundle source commit does not match signed source commit: $bundle_source_commit"
}

verify_bundle_identity Full "$FULL_APP"
verify_bundle_identity Update "$UPDATE_APP"
"$SCRIPT_DIR/check-bundle.sh" --variant full --model-manifest "$MODEL_MANIFEST" "$FULL_APP" ||
    fail bundle "Full bundle recheck failed"
"$SCRIPT_DIR/check-bundle.sh" --variant update --model-manifest "$MODEL_MANIFEST" "$UPDATE_APP" ||
    fail bundle "Update bundle recheck failed"
"$SCRIPT_DIR/check-model-variant.sh" --variant full --model-manifest "$MODEL_MANIFEST" --resources "$FULL_APP/Contents/Resources" ||
    fail bundle "Full bundle does not contain the exact production model"
"$SCRIPT_DIR/check-model-variant.sh" --variant update --model-manifest "$MODEL_MANIFEST" --resources "$UPDATE_APP/Contents/Resources" ||
    fail bundle "Update bundle unexpectedly contains model resources"

for relative in \
    "Contents/MacOS/$PRODUCT" \
    "Contents/Frameworks/libsherpa-onnx-c-api.dylib" \
    "Contents/Frameworks/libonnxruntime.1.17.1.dylib"; do
    "$SCRIPT_DIR/compare-macho-payload.sh" "$FULL_APP/$relative" "$UPDATE_APP/$relative" ||
        fail parity "Full and Update unsigned Mach-O payloads differ: $relative"
done

hdiutil detach "$UPDATE_MOUNT" -quiet >/dev/null ||
    fail cleanup "could not detach Update DMG mount: $UPDATE_MOUNT"
UPDATE_MOUNTED=false
hdiutil detach "$FULL_MOUNT" -quiet >/dev/null ||
    fail cleanup "could not detach Full DMG mount: $FULL_MOUNT"
FULL_MOUNTED=false

echo "PTT2me release artifacts verified: $VERSION ($SOURCE_COMMIT)"
