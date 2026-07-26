#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly BUNDLE_ID="com.ptt2me.app"
readonly ACCESSIBILITY_URL="x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
readonly INPUT_MONITORING_URL="x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"
readonly MICROPHONE_URL="x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT
readonly APP_PATH="$REPO_ROOT/dist/$PRODUCT.app"
readonly PLIST_PATH="$APP_PATH/Contents/Info.plist"

usage() {
    cat <<'EOF'
Usage:
  scripts/grant-and-run.sh
  scripts/grant-and-run.sh --reset
  scripts/grant-and-run.sh --open-panes
  scripts/grant-and-run.sh --help

Modes:
  (default)     Preserve permissions and launch PTT2me.
  --open-panes  Open the three required Privacy & Security panes.
  --reset       Reset only PTT2me permissions and guide setup again.
EOF
}

fail_usage() {
    echo "Unknown or combined arguments." >&2
    usage >&2
    exit 2
}

if (( $# > 1 )); then
    fail_usage
fi

case "${1:-}" in
    "")
        MODE="run"
        ;;
    --reset)
        MODE="reset"
        ;;
    --open-panes)
        MODE="open-panes"
        ;;
    --help)
        usage
        exit 0
        ;;
    *)
        fail_usage
        ;;
esac
readonly MODE

cd -- "$REPO_ROOT"

if [[ "$(uname -m)" != "arm64" ]]; then
    echo "PTT2me requires an Apple Silicon (arm64) Mac." >&2
    exit 1
fi

if [[ ! -d "$APP_PATH" ]]; then
    echo "PTT2me app bundle is missing: $APP_PATH" >&2
    echo "Build it first with scripts/build-app.sh." >&2
    exit 1
fi

"$SCRIPT_DIR/check-bundle.sh" "$APP_PATH"

CARGO_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
if [[ -z "$CARGO_VERSION" ]]; then
    echo "Could not read package version from Cargo.toml." >&2
    exit 1
fi

BUNDLE_VERSION=$(
    /usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$PLIST_PATH" 2>/dev/null
) || {
    echo "Could not read CFBundleShortVersionString from $PLIST_PATH." >&2
    exit 1
}

if [[ "$CARGO_VERSION" != "$BUNDLE_VERSION" ]]; then
    echo "Version mismatch: Cargo.toml is $CARGO_VERSION, bundle is $BUNDLE_VERSION." >&2
    exit 1
fi

open_permission_pane() {
    open "$1"
}

wait_for_return() {
    local prompt="$1"
    printf '%s' "$prompt"
    IFS= read -r _
}

stop_existing_instance() {
    pkill -TERM -x "$PRODUCT" 2>/dev/null || true

    local poll
    for poll in 1 2 3 4 5; do
        if ! pgrep -x "$PRODUCT" >/dev/null 2>&1; then
            return 0
        fi
        sleep 1
    done

    if pgrep -x "$PRODUCT" >/dev/null 2>&1; then
        echo "PTT2me did not stop after 5 seconds; refusing to escalate." >&2
        return 1
    fi
}

case "$MODE" in
    open-panes)
        open_permission_pane "$ACCESSIBILITY_URL"
        open_permission_pane "$INPUT_MONITORING_URL"
        open_permission_pane "$MICROPHONE_URL"
        ;;
    run)
        stop_existing_instance
        open "$APP_PATH"
        ;;
    reset)
        stop_existing_instance

        tccutil reset Accessibility "$BUNDLE_ID" || true
        tccutil reset ListenEvent "$BUNDLE_ID" || true
        tccutil reset Microphone "$BUNDLE_ID" || true

        open_permission_pane "$ACCESSIBILITY_URL"
        wait_for_return "Enable PTT2me in Accessibility, then press Return..."

        open_permission_pane "$INPUT_MONITORING_URL"
        wait_for_return "Enable PTT2me in Input Monitoring, then press Return..."

        if ! open -W "$APP_PATH" --args --prime-microphone-and-exit; then
            echo "Microphone priming did not complete; continue in System Settings." >&2
        fi

        open_permission_pane "$MICROPHONE_URL"
        wait_for_return "Enable PTT2me in Microphone, then press Return..."

        open "$APP_PATH"
        ;;
esac
