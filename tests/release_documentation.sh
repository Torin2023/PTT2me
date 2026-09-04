#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
CHECKLIST="$REPO_ROOT/docs/release/MANUAL_P0_CHECKLIST.md"
README="$REPO_ROOT/README.md"

[[ -f "$CHECKLIST" ]] || {
    echo "manual P0 checklist is missing: $CHECKLIST" >&2
    exit 1
}

require_text() {
    local file="$1"
    local text="$2"
    grep -Fqi -- "$text" "$file" || {
        echo "missing documentation contract '$text' in $file" >&2
        exit 1
    }
}

for text in \
    '# Manual P0 checklist PTT2me' \
    'Версия:' \
    'Build (YYYYMMDDHHMM):' \
    'Source commit:' \
    'Full DMG SHA-256:' \
    'Проверяющий:' \
    'Дата проверки:' \
    '## 1. Запуск, установка и TCC' \
    '20 коротких Fn/Globe' \
    '20 длинных Fn/Globe' \
    'назначенная обычная клавиша' \
    'комбинация с назначенной клавишей' \
    'autorepeat' \
    'tap loss/restore' \
    'capture-start failure' \
    'microphone start/stop failure' \
    'пустой recognition output' \
    'punctuation модели' \
    'Пробел в конце' \
    'Accessibility selected-text' \
    'Unicode' \
    'pasteboard fallback' \
    'plain/rich/image/file representations' \
    'новый пользовательский clipboard' \
    'focused field' \
    'проверенная внешняя модель выбирает Update' \
    'missing/changed model выбирает Full' \
    'manifest/network/digest/quarantine failures' \
    'verified DMG открывается только по действию пользователя' \
    'приложение завершает работу только после успешного workspace open' \
    'Finder replacement' \
    '## Финальное решение владельца'; do
    require_text "$CHECKLIST" "$text"
done

for text in \
    'bash scripts/test-shell-contracts.sh' \
    'scripts/release-preflight.sh' \
    'scripts/build-release-artifacts.sh' \
    'scripts/verify-release-artifacts.sh' \
    '--expected-tag vX.Y.Z' \
    'do not publish GitHub Release or the Pages stable channel' \
    'docs/release/MANUAL_P0_CHECKLIST.md'; do
    require_text "$README" "$text"
done

echo "Release documentation contract checks passed"
