#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
ICON="$REPO_ROOT/assets/PTT2me.icns"
BUILD_APP="$REPO_ROOT/scripts/build-app.sh"
CHECK_BUNDLE="$REPO_ROOT/scripts/check-bundle.sh"
CHECK_ICON="$REPO_ROOT/scripts/check-app-icon.sh"

[[ -s "$ICON" && ! -L "$ICON" ]] || {
    echo "missing real application icon: assets/PTT2me.icns" >&2
    exit 1
}

sips -g format "$ICON" 2>/dev/null | grep -Fq 'format: icns' || {
    echo "assets/PTT2me.icns is not a valid ICNS image" >&2
    exit 1
}

for contract in \
    'ICON_SOURCE="$REPO_ROOT/assets/PTT2me.icns"' \
    'install -m 644 "$ICON_SOURCE" "$RESOURCES/PTT2me.icns"' \
    'Add :CFBundleIconFile string PTT2me.icns'; do
    grep -Fq -- "$contract" "$BUILD_APP" || {
        echo "build-app.sh is missing icon contract: $contract" >&2
        exit 1
    }
done

for contract in \
    'ICON="$RESOURCES/PTT2me.icns"' \
    '"$SCRIPT_DIR/check-app-icon.sh" "$REPO_ROOT/assets/PTT2me.icns" "$ICON"' \
    'assert_plist CFBundleIconFile PTT2me.icns'; do
    grep -Fq -- "$contract" "$CHECK_BUNDLE" || {
        echo "check-bundle.sh is missing icon contract: $contract" >&2
        exit 1
    }
done

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

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-icon-contract.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT
VALID_ICON="$TEMP_ROOT/valid.icns"
cp "$ICON" "$VALID_ICON"
"$CHECK_ICON" "$ICON" "$VALID_ICON" >/dev/null

expect_failure_containing "bundle icon is not a real file" \
    "$CHECK_ICON" "$ICON" "$TEMP_ROOT/missing.icns"

printf '%s' 'not-an-icon' >"$TEMP_ROOT/invalid.icns"
expect_failure_containing "bundle icon is not a valid ICNS image" \
    "$CHECK_ICON" "$ICON" "$TEMP_ROOT/invalid.icns"

cp "$ICON" "$TEMP_ROOT/changed.icns"
printf '%s' 'changed' >>"$TEMP_ROOT/changed.icns"
expect_failure_containing "bundle icon does not match" \
    "$CHECK_ICON" "$ICON" "$TEMP_ROOT/changed.icns"

ln -s "$ICON" "$TEMP_ROOT/symlink.icns"
expect_failure_containing "bundle icon is not a real file" \
    "$CHECK_ICON" "$ICON" "$TEMP_ROOT/symlink.icns"

echo "Application icon contract checks passed"
