# Инструкции для AI-агентов

## Назначение и платформа

PTT2me — локальное menu-bar приложение для голосового ввода. Код написан на
Rust и поддерживает только Apple Silicon (`aarch64-apple-darwin`) с macOS 13+.
Linux-окружение Codex Cloud предназначено для редактирования, форматирования,
статического анализа и подготовки Pull Request. Запуск приложения, полный
набор тестов и release gate выполняются на macOS.

## Структура репозитория

- `src/` — код приложения. `runtime.rs` координирует состояния, запись,
  распознавание и вставку; `hotkey.rs`, `audio.rs`, `text_inserter.rs` и
  `permissions.rs` образуют macOS-границы.
- `src/updater.rs`, `src/updater_runtime.rs`, `src/update_manifest.rs` —
  проверка подписанных обновлений, выбор Full/Update и безопасная загрузка.
- `src/model.rs`, `src/model_store.rs` — проверка и подготовка фиксированной
  модели GigaAM.
- `tests/` — интеграционные тесты, включая AppKit/NSPasteboard и модельные
  контракты.
- `scripts/` — cloud setup, сборка `.app`/DMG, bundle checks и release tooling.
- `models/manifests/` и `updates/` — коммитируемые неизменяемые контракты
  модели и подписанных release records.
- `vendor/models/` — локальные большие model assets; они не входят в Git.

## Подготовка окружения

В Codex Cloud и на новой машине сначала выполните:

```bash
bash scripts/cloud-setup.sh
```

Скрипт устанавливает точный toolchain из `rust-toolchain.toml`, `rustfmt`,
Clippy, target `aarch64-apple-darwin`, `cargo-audit 0.22.2` и загружает
зависимости строго по `Cargo.lock`. Секреты для разработки и CI не нужны.

Codex Cloud environment должен использовать setup command
`bash scripts/cloud-setup.sh`. Не добавляйте личные токены в setup script,
репозиторий или логи агента.

## Проверки

Минимальная проверка в Linux/Codex Cloud:

```bash
cargo fmt --all -- --check
cargo check --all-targets --features test-support --target aarch64-apple-darwin
cargo audit --no-fetch --deny warnings
```

Если cross-target `cargo check` ограничен отсутствием Apple SDK или
нативного runtime, зафиксируйте точную ошибку в Pull Request и не обходите
macOS platform guards.

Полный gate на Apple Silicon macOS и в GitHub Actions:

```bash
cargo fmt --all -- --check
cargo test --all-targets --features test-support -- --test-threads=1
cargo clippy --all-targets --features test-support -- -D warnings
cargo audit --no-fetch --deny warnings
```

`tests/pasteboard_main.rs` требует полноценную AppKit-сессию. Сбой
`NSPasteboard pasteboardWithUniqueName` в headless/sandbox окружении нужно
отличать от регрессии и проверять тем же тестом в GitHub Actions.

Сборку Full/Update DMG и ручные проверки TCC выполняйте только для явно
поставленной release-задачи и по release gate из `README.md`.

## Границы изменений

- Не изменяйте `src/`, `tests/`, product behavior или публичные тексты без
  прямой связи с задачей.
- Не изменяйте `vendor/models/`, `models/manifests/`, `updates/releases/`,
  `updates/public-key.txt`, license texts и release scripts без явной задачи.
- Не генерируйте, не заменяйте и не загружайте модель автоматически.
- Не коммитьте `target/`, `dist/`, `.env*`, `.DS_Store`, agent logs, DMG,
  model files, signing keys или другие секреты.
- `Cargo.lock` меняйте только вместе с осознанным изменением зависимостей или
  версии пакета.
- Не ослабляйте TCC, updater signature verification, path/symlink checks,
  pasteboard preservation и PTT short-press/combination semantics.
- Не публикуйте Release, Pages manifest и DMG без отдельного разрешения.

## Git и Pull Request

- Одна задача — одна ветка — один Pull Request.
- Никогда не вносите изменения напрямую в `main` и не выполняйте force push.
- Ветки агента называйте `codex/<короткое-описание>`.
- Перед правками обновите базу от `origin/main`; не переносите в ветку
  несвязанные локальные изменения.
- Коммитьте только файлы текущей задачи.
- Заполните `.github/pull_request_template.md`: цель, границы, проверки,
  риски и platform limitations.
- Не merge Pull Request, пока required check
  `Format, test, lint, and audit` не завершён успешно.
- Merge и публикация релиза остаются решением владельца репозитория.

## Правила code review

- Проверяйте сохранение существующих продуктовых контрактов, а не только
  компиляцию.
- Считайте изменение immutable model/release records критическим и требуйте
  явного release scope.
- Отмечайте появление секретов, неприкреплённых внешних GitHub Actions,
  ослабление `permissions:` и команды, зависящие от локального компьютера.
- Проверяйте, что документация разделяет возможности Linux cloud, macOS CI и
  ручного release gate.
