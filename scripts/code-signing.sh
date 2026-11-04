#!/bin/bash
# Shared explicit policy arguments for the build and independent verifier.
SIGNING_POLICY_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
SIGNING_MODE=""
SIGNING_IDENTITY=""
SIGNING_TEAM_ID=""

signing_fail() {
    echo "PTT2me code signing failed [signing]: $*" >&2
    exit 1
}

signing_option() {
    [[ -n "$2" ]] || signing_fail "signing arguments must not be empty"
    case "$1" in
        --signing-mode)
            [[ -z "$SIGNING_MODE" ]] || signing_fail "duplicate signing mode"
            SIGNING_MODE="$2" ;;
        --signing-identity)
            [[ -z "$SIGNING_IDENTITY" ]] || signing_fail "duplicate signing identity"
            SIGNING_IDENTITY="$2" ;;
        --team-id)
            [[ -z "$SIGNING_TEAM_ID" ]] || signing_fail "duplicate Team ID"
            SIGNING_TEAM_ID="$2" ;;
        *) signing_fail "unknown signing argument" ;;
    esac
}

signing_validate_policy() {
    case "$SIGNING_MODE" in
        ad-hoc)
            [[ -z "$SIGNING_IDENTITY" && -z "$SIGNING_TEAM_ID" ]] ||
                signing_fail "ad-hoc mode does not accept an identity or Team ID"
            SIGNING_ARGS=(--signing-mode ad-hoc)
            ;;
        developer-id)
            [[ "$SIGNING_IDENTITY" =~ ^[0-9A-Fa-f]{40}$ ]] ||
                signing_fail "Developer ID requires an exact 40-hex certificate fingerprint"
            [[ "$SIGNING_TEAM_ID" =~ ^[A-Z0-9]{10}$ ]] ||
                signing_fail "Developer ID requires an exact 10-character Team ID"
            SIGNING_ARGS=(--signing-mode developer-id --signing-identity "$SIGNING_IDENTITY" --team-id "$SIGNING_TEAM_ID")
            ;;
        *) signing_fail "explicit --signing-mode ad-hoc|developer-id is required" ;;
    esac
}

signing_preflight() {
    python3 "$SIGNING_POLICY_DIR/code-signing.py" preflight "${SIGNING_ARGS[@]}"
}

signing_sign_app() {
    python3 "$SIGNING_POLICY_DIR/code-signing.py" sign "${SIGNING_ARGS[@]}" --app "$1"
}

signing_verify_app() {
    python3 "$SIGNING_POLICY_DIR/code-signing.py" verify "${SIGNING_ARGS[@]}" --app "$1"
}
