#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
BUILDER="$REPO_ROOT/scripts/build-release-artifacts.sh"

line_number() {
    local pattern="$1"
    local line
    line="$(grep -n -F -- "$pattern" "$BUILDER" | head -n 1 | cut -d: -f1)"
    [[ -n "$line" ]] || {
        echo "missing builder contract: $pattern" >&2
        exit 1
    }
    printf '%s\n' "$line"
}

PREFLIGHT_LINE="$(line_number '"$SCRIPT_DIR/release-preflight.sh"')"
PUBLIC_KEY_REPEAT_LINE="$(line_number 'git cat-file -e "HEAD:$COMMITTED_PUBLIC_KEY"')"
MODEL_REPEAT_LINE="$(line_number '"$SCRIPT_DIR/check-model-variant.sh"')"
GIT_REPEAT_LINE="$(line_number 'git status --porcelain=v1 --untracked-files=all')"
GIT_COMMON_REPEAT_LINE="$(line_number 'git rev-parse --git-common-dir')"
WORKTREE_REPEAT_LINE="$(line_number 'git worktree list --porcelain')"
TEMP_LINE="$(line_number 'TEMP_ROOT="$(mktemp -d')"
BUILD_LINE="$(line_number '"$SCRIPT_DIR/build-app.sh"')"

for later in \
    "$PUBLIC_KEY_REPEAT_LINE" "$MODEL_REPEAT_LINE" "$GIT_REPEAT_LINE" \
    "$GIT_COMMON_REPEAT_LINE" "$WORKTREE_REPEAT_LINE" \
    "$TEMP_LINE" "$BUILD_LINE"; do
    [[ "$PREFLIGHT_LINE" -lt "$later" ]] || {
        echo "builder must run release preflight before its repeated checks and build side effects" >&2
        exit 1
    }
done

for argument in \
    '--version "$VERSION"' \
    '--build "$BUILD"' \
    '--source-commit "$SOURCE_COMMIT"' \
    '--model-manifest "$MODEL_MANIFEST"' \
    '--model-source "$MODEL_SOURCE"' \
    '--public-key "$PUBLIC_KEY"' \
    '--private-key "$PRIVATE_KEY"' \
    '--published-at "$PUBLISHED_AT"' \
    '--output-dir "$OUTPUT_DIR"'; do
    grep -Fq -- "$argument" "$BUILDER" || {
        echo "builder preflight invocation is missing $argument" >&2
        exit 1
    }
done

grep -Fq 'ln "$source" "$destination"' "$BUILDER" || {
    echo "builder must preserve no-overwrite hard-link publication" >&2
    exit 1
}
grep -Fq 'PUBLISHED_PATHS+=("$destination")' "$BUILDER" || {
    echo "builder must preserve partial-publication cleanup tracking" >&2
    exit 1
}
grep -Fq 'PTT2ME_MANIFEST_SIGNER="$SIGNER_BINARY"' "$BUILDER" || {
    echo "builder must override inherited signer selection with the pinned release binary" >&2
    exit 1
}
grep -Fq 'PTT2ME_MANIFEST_LIBRARY_PATH=' "$BUILDER" || {
    echo "builder must clear inherited manifest verifier library overrides" >&2
    exit 1
}
for contract in \
    'IN_FLIGHT_SOURCE="$source"' \
    'IN_FLIGHT_DESTINATION="$destination"' \
    '"$IN_FLIGHT_SOURCE" -ef "$IN_FLIGHT_DESTINATION"'; do
    grep -Fq -- "$contract" "$BUILDER" || {
        echo "builder is missing signal-safe publication contract: $contract" >&2
        exit 1
    }
done
if grep -Eq '(^|[[:space:]])(mv[[:space:]]+-f|cp[[:space:]]+-f)' "$BUILDER"; then
    echo "builder must not force-overwrite release outputs" >&2
    exit 1
fi

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-builder-contracts.XXXXXX")"
trap 'rm -rf "$TEMP_ROOT"' EXIT
FIXTURE_REPO="$TEMP_ROOT/repo"
mkdir -p "$FIXTURE_REPO/scripts" "$TEMP_ROOT/model" "$TEMP_ROOT/output"
cp "$BUILDER" "$FIXTURE_REPO/scripts/build-release-artifacts.sh"
cat >"$FIXTURE_REPO/scripts/release-preflight.sh" <<'EOF'
#!/bin/bash
printf '%s\n' "$@" >"$PREFLIGHT_INVOCATION_LOG"
exit 73
EOF
chmod 755 "$FIXTURE_REPO/scripts"/*.sh
touch "$TEMP_ROOT/model-manifest.json" "$TEMP_ROOT/public-key.txt" "$TEMP_ROOT/private-key.txt"
chmod 600 "$TEMP_ROOT/private-key.txt"
PREFLIGHT_INVOCATION_LOG="$TEMP_ROOT/preflight.log" \
    "$FIXTURE_REPO/scripts/build-release-artifacts.sh" \
    --version 1.1.0 \
    --build 202608111200 \
    --source-commit 1111111111111111111111111111111111111111 \
    --model-manifest "$TEMP_ROOT/model-manifest.json" \
    --model-source "$TEMP_ROOT/model" \
    --public-key "$TEMP_ROOT/public-key.txt" \
    --private-key "$TEMP_ROOT/private-key.txt" \
    --published-at 2026-08-11T12:00:00Z \
    --output-dir "$TEMP_ROOT/output" >/dev/null 2>&1 && {
        echo "builder unexpectedly continued after dynamic preflight fixture" >&2
        exit 1
    }
STATUS=$?
[[ "$STATUS" == 73 && -s "$TEMP_ROOT/preflight.log" ]] || {
    echo "builder did not dynamically execute release preflight before side effects" >&2
    exit 1
}
[[ -z "$(find "$TEMP_ROOT/output" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
    echo "builder created output side effects before release preflight completed" >&2
    exit 1
}

SIGNER_SCRIPT="$REPO_ROOT/scripts/sign-update-manifest.sh"
printf '%s\n' 'QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI=' \
    >"$TEMP_ROOT/acl-private-key.txt"
printf '%s\n' '{}' >"$TEMP_ROOT/payload.json"
chmod 600 "$TEMP_ROOT/acl-private-key.txt"
chmod +a 'everyone allow read' "$TEMP_ROOT/acl-private-key.txt"
if SIGNER_OUTPUT="$("$SIGNER_SCRIPT" \
    "$TEMP_ROOT/acl-private-key.txt" \
    "$TEMP_ROOT/payload.json" \
    "$TEMP_ROOT/signed.json" 2>&1)"; then
    echo "manifest signer accepted a private key with a macOS ACL" >&2
    exit 1
fi
[[ "$SIGNER_OUTPUT" == *'must not have ACL entries'* && ! -e "$TEMP_ROOT/signed.json" ]] || {
    echo "manifest signer did not fail closed on a private-key ACL: $SIGNER_OUTPUT" >&2
    exit 1
}

echo "Release builder contract checks passed"
