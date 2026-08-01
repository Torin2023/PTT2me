#!/bin/bash
set -euo pipefail

fail() {
    echo "PTT2me Mach-O payload comparison failed: $*" >&2
    exit 1
}

[[ $# -eq 2 ]] || fail "usage: $0 LEFT_MACHO RIGHT_MACHO"
LEFT="$1"
RIGHT="$2"
for input in "$LEFT" "$RIGHT"; do
    [[ -f "$input" && ! -L "$input" ]] || fail "input is not a real file: $input"
done

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-macho-compare.XXXXXX")" ||
    fail "could not create comparison workspace"
trap 'rm -rf "$TEMP_ROOT"' EXIT
chmod 700 "$TEMP_ROOT"

LEFT_UNSIGNED="$TEMP_ROOT/left"
RIGHT_UNSIGNED="$TEMP_ROOT/right"
cp "$LEFT" "$LEFT_UNSIGNED"
cp "$RIGHT" "$RIGHT_UNSIGNED"
chmod u+w "$LEFT_UNSIGNED" "$RIGHT_UNSIGNED"

codesign --remove-signature "$LEFT_UNSIGNED" >/dev/null 2>&1 ||
    fail "could not remove the left ad-hoc signature"
codesign --remove-signature "$RIGHT_UNSIGNED" >/dev/null 2>&1 ||
    fail "could not remove the right ad-hoc signature"

cmp -s "$LEFT_UNSIGNED" "$RIGHT_UNSIGNED" ||
    fail "unsigned Mach-O payloads differ"
