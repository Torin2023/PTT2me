#!/usr/bin/env bash
set -euo pipefail

readonly SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly TOOLCHAIN_FILE="$REPO_ROOT/rust-toolchain.toml"
readonly AUDIT_VERSION="0.22.2"

fail() {
    echo "PTT2me cloud setup failed: $*" >&2
    exit 1
}

command -v rustup >/dev/null 2>&1 || fail "rustup is required"
command -v cargo >/dev/null 2>&1 || fail "cargo is required"
[[ -f "$TOOLCHAIN_FILE" ]] || fail "missing rust-toolchain.toml"

TOOLCHAIN="$({
    awk -F '"' '$1 ~ /^[[:space:]]*channel[[:space:]]*=/ { print $2; exit }' \
        "$TOOLCHAIN_FILE"
})"
[[ -n "$TOOLCHAIN" ]] || fail "could not read the pinned Rust toolchain"

rustup toolchain install "$TOOLCHAIN" \
    --profile minimal \
    --component rustfmt \
    --component clippy \
    --target aarch64-apple-darwin

cd -- "$REPO_ROOT"
rustup show active-toolchain
cargo fetch --locked --target aarch64-apple-darwin

CURRENT_AUDIT_VERSION="$(cargo audit --version 2>/dev/null | awk '{ print $NF }' || true)"
if [[ "$CURRENT_AUDIT_VERSION" != "$AUDIT_VERSION" ]]; then
    cargo install cargo-audit --version "$AUDIT_VERSION" --locked
fi

# Setup runs with network access in Codex Cloud. Refresh and verify RustSec now
# so the agent phase can keep internet disabled and use --no-fetch.
cargo audit --deny warnings

echo "PTT2me cloud environment is ready."
