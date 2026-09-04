# Стабилизация приложения и воспроизводимого release-контура PTT2me

**Дата:** 2026-08-11
**Целевая платформа:** Apple Silicon (`aarch64-apple-darwin`), macOS 13+
**Статус:** согласованная архитектура, одобрена владельцем для реализации

## Цель

Сделать выпуск PTT2me воспроизводимым и fail-closed: один чистый Git commit и
явно переданные release-входы должны давать проверяемые Full/Update DMG, а любой
неподтверждённый build, model, AppKit, TCC, bundle, DMG или manifest контракт
должен блокировать публикацию.

Стабилизация не изменяет пользовательское поведение PTT2me. Она укрепляет
разработку, сборку, проверку артефактов и ручной P0 gate вокруг существующих
контрактов.

## Текущее подтверждённое состояние

- `cargo fmt`, Rust unit/integration tests кроме GUI-зависимого
  `pasteboard_main`, Clippy, RustSec и release build могут выполняться локально.
- `tests/pasteboard_main.rs` требует полноценную AppKit GUI-сессию; headless
  `+[NSPasteboard pasteboardWithUniqueName]` не является доказательством
  продуктовой регрессии.
- существующие v1.1.0 Full/Update DMG соответствуют `.sha256`, а signed manifest
  проходит `scripts/validate-update-manifest.sh`.
- воспроизводимая Full-сборка в текущем checkout невозможна без локального
  `vendor/models/gigaam-v3-rnnt/encoder.int8.onnx`.
- `tests/model_bundle_variants.sh` содержит release-версию как literal; замена
  `1.0.5` на `1.1.0` устраняет текущий сбой, но не предотвращает следующий
  version drift.
- CI проверяет код и тесты, но не создаёт и не валидирует полный release-набор.

## Принципы

1. Git commit, tag и committed signed records остаются источником истины.
2. `scripts/build-release-artifacts.sh` остаётся единственным production
   builder для Full/Update набора.
3. Builder и verifier являются разными компонентами. Verifier повторно
   проверяет готовые outputs и не доверяет промежуточному состоянию builder.
4. Каждый gate работает fail-closed. Невыполненная проверка не считается
   пройденной.
5. Модель, private signing key, `.app`, DMG, `dist/` и диагностические release
   outputs не коммитятся.
6. Сборка и проверка не публикуют GitHub Release или Pages. Публикация остаётся
   отдельным явно разрешённым действием владельца.
7. Не ослабляются TCC migration, updater signature verification, path/symlink
   проверки, pasteboard preservation и PTT short-press/combination semantics.

## Архитектура

```text
Pull Request / main
        │
        ▼
┌─────────────────────────────────────────────────────────────┐
│ Gate A: Quality                                             │
│ fmt → Rust tests → AppKit pasteboard → Clippy → RustSec     │
│ → shell contract tests                                     │
└──────────────────────────┬──────────────────────────────────┘
                           │ accepted commit
                           ▼
Controlled Apple Silicon Mac
        │
        ▼
┌─────────────────────────────────────────────────────────────┐
│ Gate B: scripts/release-preflight.sh                        │
│ environment + Git + model + key + output + GUI readiness   │
└──────────────────────────┬──────────────────────────────────┘
                           │ complete immutable inputs
                           ▼
┌─────────────────────────────────────────────────────────────┐
│ Builder: scripts/build-release-artifacts.sh                 │
│ Full.app → Update.app → Full DMG → Update DMG → manifest   │
└──────────────────────────┬──────────────────────────────────┘
                           │ closed output set
                           ▼
┌─────────────────────────────────────────────────────────────┐
│ Gate C: scripts/verify-release-artifacts.sh                 │
│ bundle + model + Mach-O + codesign + DMG + SHA + Ed25519   │
│ + tag/source identity                                      │
└──────────────────────────┬──────────────────────────────────┘
                           │ verified artifacts
                           ▼
┌─────────────────────────────────────────────────────────────┐
│ Gate D: Manual P0                                          │
│ launch + TCC + hotkey + audio + ASR + insertion + updater  │
└──────────────────────────┬──────────────────────────────────┘
                           │ owner approval
                           ▼
              Separate publication workflow
```

## Gate A: Pull Request quality

GitHub Actions продолжает выполнять на `macos-15`:

```bash
cargo fmt --all -- --check
cargo test --all-targets --features test-support -- --test-threads=1
cargo clippy --all-targets --features test-support -- -D warnings
cargo audit --no-fetch --deny warnings
```

К этому gate добавляется отдельный запуск shell contract tests. Shell-fixtures,
которым нужна текущая версия, читают её из `Cargo.toml` тем же ограниченным
парсером, который применяют release scripts. Literal `1.1.0` не используется
как ожидаемая текущая версия.

Gate A не получает production model assets или private signing key и не строит
production DMG.

## Gate B: Release preflight

Новый `scripts/release-preflight.sh` принимает те же release identity и пути,
которые необходимы builder:

```text
--version X.Y.Z
--build YYYYMMDDHHMM
--source-commit COMMIT
--model-manifest PATH
--model-source PATH
--public-key PATH
--private-key PATH
--published-at YYYY-MM-DDTHH:MM:SSZ
--output-dir PATH
```

Preflight не создаёт `.app`, DMG или manifest. Он последовательно доказывает:

- `uname -m` возвращает `arm64`, macOS удовлетворяет minimum `13.0`;
- активен toolchain из `rust-toolchain.toml`, доступны Cargo, `hdiutil`,
  `codesign`, `lipo`, `otool`, `PlistBuddy`, `shasum` и signer/verifier;
- `SOURCE_COMMIT` равен exact clean `HEAD`;
- version совпадает с `Cargo.toml` и `Cargo.lock`;
- build является валидной 12-значной UTC calendar minute;
- output directory является реальным каталогом, не symlink, имеет достаточно
  места, а все будущие output names отсутствуют; минимальный reserve равен
  `2 * total_model_size + 1 GiB` на filesystem temporary/output workspace;
- public key совпадает с `updates/public-key.txt`;
- private key является обычным файлом, не имеет group/world permission bits
  (`mode & 0077 == 0`) и расположен вне Git repository;
- production model manifest совпадает с committed exact bytes;
- model source содержит ровно четыре разрешённых обычных файла без symlink,
  executable bits и дополнительных entries;
- размеры и SHA-256 всех model-файлов совпадают с manifest;
- доступна полноценная GUI/AppKit-сессия: отдельный запуск
  `cargo test --test pasteboard_main --features test-support` проходит.

Каждая ошибка содержит стабильную category и конкретный path/input, но не
выводит private key bytes, model contents или пользовательские данные.

## Builder

`scripts/build-release-artifacts.sh` сохраняет текущую ответственность:

1. повторно проверяет критичные preconditions, не полагаясь только на preflight;
2. строит Full `.app` из exact commit и проверенной модели;
3. создаёт Update `.app` только удалением `Contents/Resources/models` и сменой
   `PTT2meDistributionVariant`;
4. проверяет оба bundle через `scripts/check-bundle.sh`;
5. сравнивает unsigned Mach-O payload Full/Update;
6. создаёт оба DMG;
7. вычисляет SHA-256/size и подписывает release payload;
8. валидирует готовый manifest вместе с обоими DMG и model manifest;
9. публикует outputs в указанный каталог только hard-link операциями без
   перезаписи.

Builder использует private temporary directory и удаляет неполные outputs при
ошибке. Уже существующие release outputs никогда не перезаписываются.

## Gate C: Independent artifact verification

Новый `scripts/verify-release-artifacts.sh` принимает закрытый output-набор:

```text
Full DMG
Full DMG.sha256
Update DMG
Update DMG.sha256
signed update manifest
public key
production model manifest
expected source commit и необязательный expected tag для rehearsal
```

Verifier выполняет только read-only проверки готовых outputs:

1. проверяет имена, отсутствие symlink и отсутствие дополнительных release
   artifacts в выбранном наборе;
2. независимо пересчитывает SHA-256 обоих DMG и сравнивает `.sha256`;
3. проверяет Ed25519 envelope и signed sizes/hashes;
4. проверяет version/build/source commit/minimum macOS/architecture/model ID;
5. всегда сверяет expected source commit с signed payload; если передан expected
   tag, проверяет, что `vX.Y.Z` указывает на signed source commit;
6. выполняет `hdiutil verify`, затем монтирует каждый DMG read-only;
7. повторно запускает `scripts/check-bundle.sh` для `.app` из каждого DMG;
8. подтверждает, что Full содержит exact model, а Update не содержит
   `Contents/Resources/models`;
9. повторно сравнивает unsigned Mach-O payload извлечённых Full/Update bundle;
10. запускает bundled-model smoke из Full bundle с существующим bounded
    watchdog;
11. гарантированно отсоединяет mounted images при success, failure и signal.

Verifier не использует private signing key и не меняет release outputs.
Rehearsal до создания tag может выполняться только с expected source commit.
Перед публикацией повторный Gate C обязан получить expected tag и подтвердить
его соответствие signed source commit.

## Gate D: Manual P0

Manual gate выполняется на контролируемом Apple Silicon Mac из установленного
Full DMG. Результаты записываются в release checklist с build identity и именем
проверяющего. Канонический шаблон хранится в
`docs/release/MANUAL_P0_CHECKLIST.md`; заполненная копия остаётся рядом с
локальными release outputs и не коммитится.

### Запуск и TCC

- первый запуск подготавливает модель и сбрасывает только Accessibility,
  Input Monitoring и Microphone для нового build identity;
- выдача каждого разрешения приводит приложение к `Готово`;
- отзыв каждого разрешения блокирует диктовку и показывает правильную целевую
  recovery action;
- повторный запуск той же identity не выполняет повторный completed reset;
- новый build/source identity выполняет новый reset.

### PTT и клавиатура

- 20 коротких Fn/Globe presses выполняют системное действие и не запускают ASR;
- 20 длинных Fn/Globe holds запускают ровно один capture/recognition cycle и не
  воспроизводят Fn в macOS;
- назначенная обычная клавиша сохраняет short press;
- комбинация с назначенной клавишей проходит в исходном порядке;
- autorepeat, tap loss/restore и capture-start failure не оставляют stuck key
  или ложный следующий cycle.

### Audio, ASR и вставка

- microphone start/stop failure отображается и восстанавливается;
- пустой recognition output ничего не вставляет;
- punctuation модели сохраняется;
- `Пробел в конце` добавляет ровно один ASCII space только при включённой
  настройке;
- проверяются Accessibility selected-text, Unicode и pasteboard fallback;
- pasteboard fallback восстанавливает plain/rich/image/file representations;
- новый пользовательский clipboard во время вставки не перезаписывается;
- изменение focused field во время recognition направляет текст в текущий
  focused field согласно продуктовому контракту.

### Updater

- проверенная внешняя модель выбирает Update;
- missing/changed model выбирает Full и требует явного подтверждения;
- manifest/network/digest/quarantine failures не открывают DMG;
- verified DMG открывается только по действию пользователя;
- приложение завершает работу только после успешного workspace open;
- Finder replacement и последующий TCC cycle выполняются по README.

## Ошибки и восстановление

| Сбой | Результат |
|---|---|
| Нет model-файла или hash mismatch | Preflight останавливается до Cargo build |
| Dirty Git или identity mismatch | Release не начинается |
| Private key внутри repository | Release не начинается |
| Ошибка Full/Update parity | Весь набор отклоняется |
| Ошибка bundle/model smoke | DMG не считается verified |
| Ошибка `hdiutil verify`/mount | Gate C блокирует релиз |
| Ошибка SHA-256/Ed25519/source tag | Gate C блокирует релиз |
| Ошибка AppKit/TCC/PTT/manual flow | Gate D блокирует публикацию |
| Ошибка публикации | Immutable local verified outputs сохраняются для анализа; stable channel не меняется |

Ни один gate не преобразует `SKIPPED`, timeout или отсутствие GUI в success.

## Тестовая стратегия

### Unit и pure-state tests

Существующие Rust tests остаются источником проверки state machines, hotkey,
audio buffer, insertion orchestration, model store, permission migration и
updater. Новое production Rust behavior не добавляется.

### Shell contract tests

Для `release-preflight.sh` и `verify-release-artifacts.sh` используются fake
tool boundaries и temporary fixtures. Обязательные negative cases:

- invalid version/build/commit;
- dirty worktree;
- missing/extra/symlink/executable/model hash mismatch;
- key внутри Git или permissive key permissions;
- output already exists или output directory является symlink;
- checksum, signed size, signature, tag/source mismatch;
- Full без модели, Update с моделью;
- `hdiutil verify`, mount, detach и bundle recheck failure;
- cleanup после каждого failure point.

Положительный shell integration test использует маленькую synthetic модель и
temporary signing key, но не production model и не production private key.

### Real macOS integration

- GitHub Actions выполняет AppKit pasteboard test в GUI-capable `macos-15` job;
- controlled Mac выполняет реальный Full model smoke и DMG mount verification;
- Manual P0 остаётся обязательным, потому что физические Fn/TCC/microphone и
  Finder replacement не доказываются unit tests.

## Критерии приёмки

Стабилизация считается готовой, когда:

1. все shell-fixtures получают текущую package version без hardcoded release
   literal;
2. Gate A проходит в GitHub Actions;
3. preflight отклоняет каждый перечисленный invalid input до сборки;
4. один чистый tagged commit с полными release-входами создаёт оба DMG и signed
   manifest;
5. independent verifier принимает outputs без private key и без builder state;
   pre-publication повторный запуск также подтверждает expected tag;
6. повторная проверка извлечённых bundle подтверждает Full/Update layout,
   Mach-O parity, codesign и model smoke;
7. Manual P0 checklist полностью пройден и привязан к exact build identity;
8. публикация не входит в build/verify commands и требует отдельного разрешения;
9. repository не содержит model assets, keys, DMG, `.app`, `dist/`, `target/`
   или release logs.

## Порядок реализации

1. Завершить и оформить текущее fixture-исправление, заменив hardcoded `1.1.0`
   на чтение версии из `Cargo.toml`.
2. Добавить `release-preflight.sh` и его negative/positive shell tests.
3. Добавить `verify-release-artifacts.sh` и его shell tests.
4. Подключить shell contract tests к Gate A.
5. Добавить versioned manual P0 checklist без автоматизации TCC/Fn действий.
6. Выполнить полный rehearsal на контролируемом Mac с production model inputs,
   но без публикации.
7. Провести независимый review и повторить все blocking checks.

## Не входит в scope

- Apple Developer ID signing и notarization;
- автоматическая замена `/Applications/PTT2me.app`;
- автоматическая загрузка production model в checkout;
- хранение private signing key в CI или GitHub Secrets;
- telemetry, cloud ASR, история диктовок или отправка пользовательских данных;
- изменение Fn/custom-key, insertion, updater или TCC product semantics;
- публикация новой версии, GitHub Release или Pages stable channel;
- перенос приложения на Swift/AppKit host или архитектурная переработка Rust
  runtime.

## Риски

- GUI-capable поведение GitHub macOS runners может отличаться от локальной
  пользовательской сессии; Manual P0 остаётся отдельным доказательством.
- ad-hoc signing сохраняет ожидаемый повторный TCC cycle и возможный
  `Открыть всё равно`; стабилизация не устраняет этот продуктовый trade-off.
- production model и private key остаются внешними входами; preflight может
  доказать их корректность, но не может безопасно создавать или загружать их.
- полный release rehearsal требует свободного места, времени model smoke и
  возможности монтировать DMG; эти требования должны быть явными, а не
  интерпретироваться как дефект приложения.
