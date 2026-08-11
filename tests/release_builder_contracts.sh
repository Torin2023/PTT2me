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
TEMP_LINE="$(line_number 'TEMP_ROOT="$(mktemp -d')"
BUILD_LINE="$(line_number '"$SCRIPT_DIR/build-app.sh"')"

for later in \
    "$PUBLIC_KEY_REPEAT_LINE" "$MODEL_REPEAT_LINE" "$GIT_REPEAT_LINE" \
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
if grep -Eq '(^|[[:space:]])(mv[[:space:]]+-f|cp[[:space:]]+-f)' "$BUILDER"; then
    echo "builder must not force-overwrite release outputs" >&2
    exit 1
fi

echo "Release builder contract checks passed"
