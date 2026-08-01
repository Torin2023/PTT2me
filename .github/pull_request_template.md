## Задача

<!-- Ссылка на Issue или краткая формулировка одной задачи. -->

## Что изменено

<!-- Перечислите только изменения этой ветки. -->

## Что не входит в задачу

<!-- Зафиксируйте намеренно не затронутые области. -->

## Проверка

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --all-targets --features test-support --target aarch64-apple-darwin` или объяснено ограничение cloud environment
- [ ] Полный macOS check `Format, test, lint, and audit` завершён успешно
- [ ] Ручная проверка выполнена или не требуется

## Безопасность и релиз

- [ ] В diff нет секретов, model assets, DMG, `target/` или `dist/`
- [ ] GitHub Actions используют полный commit SHA
- [ ] Release, Pages manifest, TCC и updater trust boundary не менялись либо изменение явно описано

## Риски и ограничения

<!-- Укажите macOS-only проверки, совместимость, миграции и известные риски. -->
