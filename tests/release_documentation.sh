#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
PROCESS="$REPO_ROOT/docs/release/AUTOMATED_RELEASE.md"
RETIRED="$REPO_ROOT/docs/release/MANUAL_P0_CHECKLIST.md"
README="$REPO_ROOT/README.md"

require_text() {
    grep -Fqi -- "$2" "$1" || {
        echo "missing documentation contract '$2' in $1" >&2
        exit 1
    }
}

while IFS= read -r text; do
    require_text "$PROCESS" "$text"
done <<'REQUIRED'
ручное тестирование полностью исключено
Format, test, lint, and audit
24 часов
RustSec
scripts/build-release-artifacts.sh
scripts/release-preflight.sh
scripts/verify-release-artifacts.sh --expected-tag vX.Y.Z
pasteboard_main
Ed25519
SHA-256
Mach-O parity
Что автоматизация пока не подтверждает
Fn/Globe
TCC
Chrome
микрофон
непроверенными
REQUIRED

while IFS= read -r text; do
    require_text "$README" "$text"
done <<'REQUIRED'
bash scripts/test-shell-contracts.sh
scripts/release-preflight.sh
scripts/build-release-artifacts.sh
scripts/verify-release-artifacts.sh
--expected-tag vX.Y.Z
do not publish GitHub Release or the Pages stable channel
Manual testing is
not a release or publication requirement
docs/release/AUTOMATED_RELEASE.md
REQUIRED

require_text "$RETIRED" 'чек-лист больше не заполняется'
if grep -Eq '^[-*] \[ \]|Публикация разрешена только при' "$RETIRED"; then
    echo "retired manual checklist must not retain active publication gates" >&2
    exit 1
fi
if grep -Eq '^scripts/release-preflight.sh ' "$README"; then
    echo "README must not require preflight twice; the builder already runs it" >&2
    exit 1
fi

echo "Automated release documentation contract checks passed"
