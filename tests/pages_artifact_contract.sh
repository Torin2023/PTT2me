#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
ASSEMBLER="$REPO_ROOT/scripts/assemble-pages-artifact.sh"

[[ -x "$ASSEMBLER" ]] || {
    echo "Pages artifact assembler is missing or not executable: $ASSEMBLER" >&2
    exit 1
}

TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-pages-artifact.XXXXXX")"
trap 'rm -rf -- "$TEST_ROOT"' EXIT

SITE_OUTPUT="$TEST_ROOT/site"
UPDATES="$TEST_ROOT/updates"
ARTIFACT="$TEST_ROOT/artifact"

mkdir -p "$SITE_OUTPUT/assets" "$UPDATES/channels" "$UPDATES/releases"
printf '%s\n' '<!doctype html><title>PTT2me 1.1.1</title>' >"$SITE_OUTPUT/index.html"
printf '%s\n' 'body{}' >"$SITE_OUTPUT/assets/site.css"
: >"$SITE_OUTPUT/.nojekyll"
printf '%s\n' '{"version":"1.1.1"}' >"$UPDATES/channels/stable.json"
printf '%s\n' '{"version":"1.1.1"}' >"$UPDATES/releases/1.1.1.json"
printf '%s\n' 'public-key' >"$UPDATES/public-key.txt"

"$ASSEMBLER" "$SITE_OUTPUT" "$UPDATES" "$ARTIFACT"

[[ -f "$ARTIFACT/index.html" ]]
[[ -f "$ARTIFACT/assets/site.css" ]]
[[ -f "$ARTIFACT/.nojekyll" ]]
cmp -s "$UPDATES/channels/stable.json" "$ARTIFACT/channels/stable.json"
cmp -s "$UPDATES/releases/1.1.1.json" "$ARTIFACT/releases/1.1.1.json"
cmp -s "$UPDATES/public-key.txt" "$ARTIFACT/public-key.txt"
[[ ! -e "$ARTIFACT/server" ]]
[[ -z "$(find "$ARTIFACT" -type l -print -quit)" ]]

NONEMPTY="$TEST_ROOT/nonempty"
mkdir -p "$NONEMPTY"
printf '%s\n' 'unrelated' >"$NONEMPTY/keep.txt"
if "$ASSEMBLER" "$SITE_OUTPUT" "$UPDATES" "$NONEMPTY"; then
    echo "Pages artifact assembler accepted a non-empty output directory" >&2
    exit 1
fi
[[ -f "$NONEMPTY/keep.txt" ]]

SYMLINK_SITE="$TEST_ROOT/symlink-site"
cp -R "$SITE_OUTPUT" "$SYMLINK_SITE"
ln -s "$TEST_ROOT/outside" "$SYMLINK_SITE/unsafe-link"
if "$ASSEMBLER" "$SYMLINK_SITE" "$UPDATES" "$TEST_ROOT/symlink-artifact"; then
    echo "Pages artifact assembler accepted a symbolic link" >&2
    exit 1
fi

echo "Pages artifact contract checks passed"
