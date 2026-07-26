# Local DMG Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Повысить версию PTT2me до 1.0.1, добавить воспроизводимую сборку локального DMG-релиза для Apple Silicon и получить проверенные релизные артефакты.

**Architecture:** Новый shell-скрипт вызывает существующую сборку app bundle, создаёт сжатый read-only DMG через системный `hdiutil`, монтирует его в отдельную временную точку и повторно проверяет вложенное приложение. После успешной проверки скрипт атомарно публикует DMG в `dist/` и создаёт SHA-256; README документирует этот поток и ограничение ad-hoc подписи.

**Tech Stack:** Bash 3.2+, системные утилиты macOS (`hdiutil`, `shasum`, `mktemp`), существующие `scripts/build-app.sh` и `scripts/check-bundle.sh`.

## Global Constraints

- Целевая платформа: Apple Silicon (`arm64`) и macOS 13 Ventura или новее.
- Версия текущего релиза явно повышается с `1.0.0` до `1.0.1`.
- Повторная сборка версии `1.0.1` не повышает версию автоматически.
- `Cargo.toml` является единым источником версии; `Cargo.lock` синхронизируется Cargo.
- Имя образа: `PTT2me-1.0.1-macos-arm64.dmg`.
- DMG содержит только `PTT2me.app` и символическую ссылку `Applications` на `/Applications`.
- Существующий ZIP-архив не удаляется и не изменяется.
- Подпись остаётся ad-hoc; нотариализация Apple не выполняется.
- Частичный или непроверенный DMG не публикуется под итоговым именем.

---

### Task 1: Единый источник версии релиза

**Files:**
- Modify: `Cargo.toml`
- Modify: `Cargo.lock`
- Modify: `scripts/check-bundle.sh`

**Interfaces:**
- Consumes: версию пакета из `Cargo.toml`.
- Produces: версию `1.0.1` в метаданных Rust и динамическую проверку той же версии в app bundle.

- [ ] **Step 1: Зафиксировать текущую несовместимость проверки с новой версией**

Run:

```bash
rg -n 'version = "1.0.0"' Cargo.toml Cargo.lock
rg -n 'assert_plist CFBundleShortVersionString 1.0.0' scripts/check-bundle.sh
```

Expected: обе метадаты пакета и проверка bundle всё ещё содержат `1.0.0`.

- [ ] **Step 2: Повысить версию пакета**

В `Cargo.toml` изменить:

```toml
version = "1.0.1"
```

Run:

```bash
cargo check
```

Expected: `Cargo.lock` содержит пакет `ptt2me` версии `1.0.1`, а `cargo check` завершается успешно.

- [ ] **Step 3: Читать ожидаемую версию bundle из Cargo.toml**

Перед вызовами `assert_plist` в `scripts/check-bundle.sh` добавить:

```bash
EXPECTED_VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$EXPECTED_VERSION" ]] || fail "could not read version from Cargo.toml"
```

Заменить:

```bash
assert_plist CFBundleShortVersionString 1.0.0
```

на:

```bash
assert_plist CFBundleShortVersionString "$EXPECTED_VERSION"
```

- [ ] **Step 4: Проверить новую версию**

Run:

```bash
test "$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)" = "1.0.1"
rg -n -A1 'name = "ptt2me"' Cargo.lock
bash -n scripts/check-bundle.sh
```

Expected: версия пакета равна `1.0.1`, запись `ptt2me` в lockfile сопровождается `version = "1.0.1"`, shell-синтаксис корректен.

- [ ] **Step 5: Закоммитить повышение версии**

Run:

```bash
git add Cargo.toml Cargo.lock scripts/check-bundle.sh
git commit -m "release: bump version to 1.0.1"
```

Expected: коммит содержит только три перечисленных файла.

### Task 2: Воспроизводимая сборка и проверка DMG

**Files:**
- Create: `scripts/build-dmg.sh`

**Interfaces:**
- Consumes: `Cargo.toml`, `scripts/build-app.sh`, `scripts/check-bundle.sh`, `dist/PTT2me.app`.
- Produces: исполняемая команда `scripts/build-dmg.sh`, файлы `dist/PTT2me-<version>-macos-arm64.dmg` и `dist/PTT2me-<version>-macos-arm64.dmg.sha256`.

- [ ] **Step 1: Зафиксировать интеграционную проверку до реализации**

Run:

```bash
test -x scripts/build-dmg.sh && scripts/build-dmg.sh
```

Expected: FAIL, потому что `scripts/build-dmg.sh` ещё не существует.

- [ ] **Step 2: Создать минимальный скрипт сборки**

Create `scripts/build-dmg.sh` with:

```bash
#!/bin/bash
set -euo pipefail

readonly PRODUCT="PTT2me"
readonly TARGET_PLATFORM="macos-arm64"

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)"
readonly SCRIPT_DIR
REPO_ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd -P)"
readonly REPO_ROOT

fail() {
    echo "PTT2me DMG build failed: $*" >&2
    exit 1
}

cleanup() {
    if [[ "${DMG_MOUNTED:-false}" == true ]]; then
        hdiutil detach "$MOUNT_DIR" -quiet || true
    fi
    [[ -z "${TEMP_ROOT:-}" ]] || rm -rf "$TEMP_ROOT"
    [[ -z "${TEMP_DMG:-}" ]] || rm -f "$TEMP_DMG"
    [[ -z "${TEMP_CHECKSUM:-}" ]] || rm -f "$TEMP_CHECKSUM"
}
trap cleanup EXIT

[[ "$(uname -m)" == "arm64" ]] ||
    fail "an Apple Silicon (arm64) Mac is required"
for command in hdiutil shasum mktemp; do
    command -v "$command" >/dev/null 2>&1 ||
        fail "required command is unavailable: $command"
done

cd -- "$REPO_ROOT"
VERSION="$(awk -F '"' '/^version = "/ { print $2; exit }' Cargo.toml)"
[[ -n "$VERSION" ]] || fail "could not read version from Cargo.toml"

readonly DMG_NAME="$PRODUCT-$VERSION-$TARGET_PLATFORM.dmg"
readonly DMG_PATH="$REPO_ROOT/dist/$DMG_NAME"
readonly CHECKSUM_PATH="$DMG_PATH.sha256"

"$SCRIPT_DIR/build-app.sh"

TEMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/ptt2me-dmg.XXXXXX")"
readonly TEMP_ROOT
readonly STAGING_DIR="$TEMP_ROOT/staging"
readonly MOUNT_DIR="$TEMP_ROOT/mount"
TEMP_DMG="$REPO_ROOT/dist/.$DMG_NAME.tmp.dmg"
TEMP_CHECKSUM="$REPO_ROOT/dist/.$DMG_NAME.sha256.tmp"

mkdir -p "$STAGING_DIR" "$MOUNT_DIR"
cp -R "$REPO_ROOT/dist/$PRODUCT.app" "$STAGING_DIR/$PRODUCT.app"
ln -s /Applications "$STAGING_DIR/Applications"

hdiutil create \
    -volname "$PRODUCT" \
    -srcfolder "$STAGING_DIR" \
    -format UDZO \
    -ov \
    "$TEMP_DMG" >/dev/null

hdiutil attach \
    "$TEMP_DMG" \
    -readonly \
    -nobrowse \
    -noautoopen \
    -mountpoint "$MOUNT_DIR" >/dev/null
DMG_MOUNTED=true

[[ -d "$MOUNT_DIR/$PRODUCT.app" ]] || fail "mounted image has no app bundle"
[[ -L "$MOUNT_DIR/Applications" ]] || fail "mounted image has no Applications link"
[[ "$(readlink "$MOUNT_DIR/Applications")" == "/Applications" ]] ||
    fail "Applications link has the wrong target"
[[ "$(find "$MOUNT_DIR" -mindepth 1 -maxdepth 1 | wc -l | tr -d ' ')" == "2" ]] ||
    fail "mounted image contains unexpected root items"
"$SCRIPT_DIR/check-bundle.sh" "$MOUNT_DIR/$PRODUCT.app"

hdiutil detach "$MOUNT_DIR" -quiet
DMG_MOUNTED=false

CHECKSUM="$(shasum -a 256 "$TEMP_DMG" | awk '{print $1}')"
printf '%s  %s\n' "$CHECKSUM" "$DMG_NAME" >"$TEMP_CHECKSUM"
mv -f "$TEMP_DMG" "$DMG_PATH"
mv -f "$TEMP_CHECKSUM" "$CHECKSUM_PATH"

echo "Built $DMG_PATH"
echo "SHA-256: $CHECKSUM"
```

- [ ] **Step 3: Сделать скрипт исполняемым и проверить shell-синтаксис**

Run:

```bash
chmod +x scripts/build-dmg.sh
bash -n scripts/build-dmg.sh
```

Expected: обе команды завершаются с кодом `0`.

- [ ] **Step 4: Запустить реальную сборку и встроенную проверку**

Run:

```bash
scripts/build-dmg.sh
```

Expected:

```text
Built /Users/andrey/dev/ptt2me/dist/PTT2me-1.0.1-macos-arm64.dmg
SHA-256: 64-символьная шестнадцатеричная контрольная сумма
```

Перед этими строками `check-bundle.sh` сообщает, что `PTT2me.app` во временной
точке монтирования валиден.

- [ ] **Step 5: Независимо проверить итоговые артефакты**

Run:

```bash
test -s dist/PTT2me-1.0.1-macos-arm64.dmg
(cd dist && shasum -a 256 -c PTT2me-1.0.1-macos-arm64.dmg.sha256)
test -s dist/PTT2me-1.0.0-macos-arm64.zip
```

Expected: проверка SHA-256 печатает `PTT2me-1.0.1-macos-arm64.dmg: OK`, а существующий ZIP остаётся на месте.

- [ ] **Step 6: Закоммитить скрипт**

Run:

```bash
git add scripts/build-dmg.sh
git commit -m "build: add local DMG release"
```

Expected: коммит содержит только `scripts/build-dmg.sh`.

### Task 3: Документация и финальная проверка релиза

**Files:**
- Modify: `README.md`

**Interfaces:**
- Consumes: `scripts/build-dmg.sh` и создаваемые им артефакты.
- Produces: пользовательскую инструкцию по локальной сборке DMG и явное описание ограничений подписи.

- [ ] **Step 1: Добавить раздел о DMG в README**

После раздела `Build` добавить:

````markdown
## Local DMG release

To rebuild the app and create a local Apple Silicon DMG, run:

```bash
scripts/build-dmg.sh
```

The command creates `dist/PTT2me-1.0.1-macos-arm64.dmg` and its
`.sha256` checksum. The image contains `PTT2me.app` and an `Applications`
link for drag-and-drop installation. It uses ad-hoc signing and is not
notarized for public distribution.

Before each new release, explicitly bump the package version in `Cargo.toml`
and synchronize `Cargo.lock` with Cargo. Rebuilding an existing release does
not change its version automatically.
````

- [ ] **Step 2: Проверить документацию**

Run:

```bash
rg -n "Local DMG release|build-dmg.sh|ad-hoc|notarized" README.md
git diff --check
```

Expected: найдены все четыре фрагмента, `git diff --check` не сообщает ошибок.

- [ ] **Step 3: Выполнить полную проверку исходного проекта**

Run:

```bash
cargo test
cargo clippy --all-targets -- -D warnings
scripts/check-bundle.sh dist/PTT2me.app
(cd dist && shasum -a 256 -c PTT2me-1.0.1-macos-arm64.dmg.sha256)
```

Expected: 70 тестов проходят, Clippy не выдаёт предупреждений, app bundle валиден, SHA-256 совпадает.

- [ ] **Step 4: Закоммитить документацию**

Run:

```bash
git add README.md
git commit -m "docs: document local DMG release"
```

Expected: коммит содержит только `README.md`.

- [ ] **Step 5: Зафиксировать итог релиза**

Run:

```bash
git status --short
ls -lh \
    dist/PTT2me.app \
    dist/PTT2me-1.0.1-macos-arm64.dmg \
    dist/PTT2me-1.0.1-macos-arm64.dmg.sha256 \
    dist/PTT2me-1.0.0-macos-arm64.zip
```

Expected: из ранее существовавших незакоммиченных файлов остаются только `.DS_Store`; все четыре релизных артефакта существуют и не пусты.
