#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
RUNNER="$REPO_ROOT/scripts/test-shell-contracts.sh"
WORKFLOW="$REPO_ROOT/.github/workflows/ci.yml"

[[ -x "$RUNNER" ]] || {
    echo "shell contract runner is missing or not executable: $RUNNER" >&2
    exit 1
}

EXPECTED_TESTS='tests/model_bundle_variants.sh
tests/release_preflight.sh
tests/release_builder_contracts.sh
tests/verify_release_artifacts.sh
tests/release_ci_contracts.sh
tests/release_documentation.sh'
ACTUAL_TESTS="$(sed -n -E 's/^[[:space:]]*bash[[:space:]]+(tests\/[A-Za-z0-9_.-]+\.sh)[[:space:]]*$/\1/p' "$RUNNER")"
[[ "$ACTUAL_TESTS" == "$EXPECTED_TESTS" ]] || {
    echo "shell contract runner has an unexpected test list:" >&2
    printf '%s\n' "$ACTUAL_TESTS" >&2
    exit 1
}

RUST_TEST_LINE="$(grep -n -F 'run: cargo test --all-targets --features test-support -- --test-threads=1' "$WORKFLOW" | cut -d: -f1)"
SHELL_TEST_LINE="$(grep -n -F 'run: bash scripts/test-shell-contracts.sh' "$WORKFLOW" | cut -d: -f1)"
[[ -n "$RUST_TEST_LINE" && -n "$SHELL_TEST_LINE" ]] || {
    echo "CI must contain both Rust and shell contract test steps" >&2
    exit 1
}
[[ "$RUST_TEST_LINE" -lt "$SHELL_TEST_LINE" ]] || {
    echo "CI must run shell contracts after the Rust test step" >&2
    exit 1
}

echo "Release CI contract checks passed"
