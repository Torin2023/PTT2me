# План реализации опционального пробела после реплики

> **Для агентного выполнения:** ОБЯЗАТЕЛЬНЫЙ ДОПОЛНИТЕЛЬНЫЙ НАВЫК:
> использовать `superpowers:subagent-driven-development` (рекомендуется) или
> `superpowers:executing-plans` для последовательного выполнения задач. Для
> отслеживания используются чекбоксы (`- [ ]`).

**Цель:** добавить сохраняемый чекбокс `Пробел в конце`, который по умолчанию
выключен и при включении добавляет ровно один пробел после вставляемой реплики,
не меняя пунктуацию ASR-модели.

**Архитектура:** новая чистая модель настройки отделяет значение и
`NSUserDefaults` от AppKit. Меню отправляет типизированную команду в runtime,
runtime обновляет и сохраняет настройку, а граница вставки применяет её после
существующей нормализации текста.

**Технологии:** Rust 2021, AppKit `NSMenu`/`NSMenuItem`, Foundation
`NSUserDefaults`, существующий тракт `NSPasteboard` + Command-V.

## Общие ограничения

- Точное название пункта меню: `Пробел в конце`.
- Значение по умолчанию: выключено (`false`).
- Выбор сохраняется между перезапусками приложения.
- Приложение не добавляет, не удаляет и не заменяет пунктуацию ASR-модели.
- При включённой настройке после нормализованной непустой реплики добавляется
  ровно один пробел ASCII.
- Полное сохранение и защищённое восстановление буфера обмена не меняются.
- Приложение остаётся локальным и поддерживает Apple Silicon на macOS 13+.

---

## Структура файлов

- Создать `src/output_preferences.rs`: модель `OutputPreferences`, тестируемая
  граница хранения, адаптер `NSUserDefaults` и контроллер текущего значения.
- Изменить `src/lib.rs`: экспортировать модуль настройки вывода.
- Изменить `Cargo.toml`: включить возможность `NSUserDefaults` у
  `objc2-foundation`.
- Изменить `src/inserter.rs`: передавать `append_space` в нормализацию и путь
  вставки.
- Изменить `src/menu.rs`: добавить пункт `Пробел в конце`, команду меню и
  отображение галочки.
- Изменить `src/runtime.rs`: загружать, применять и сохранять настройку,
  передавать её в `inserter`.
- Изменить `README.md`: описать новый пункт и единственную сохраняемую
  настройку.

---

### Задача 1: Модель настройки и постоянное хранение

**Файлы:**

- Создать: `src/output_preferences.rs`
- Изменить: `src/lib.rs`
- Изменить: `Cargo.toml`

**Интерфейсы:**

- Создаёт: `OutputPreferences { pub append_space: bool }`
- Создаёт: `RawOutputPreferenceStore`
- Создаёт: `OutputPreferenceRepository<R>`
- Создаёт: `OutputPreferenceController<R>`
- Создаёт: `SystemOutputPreferenceStore`

- [ ] **Шаг 1: написать падающие тесты модели и хранилища**

До производственной реализации добавить в `src/output_preferences.rs` тесты:

```rust
#[test]
fn output_preferences_default_to_no_trailing_space() {
    assert_eq!(
        OutputPreferences::default(),
        OutputPreferences {
            append_space: false,
        }
    );
}

#[test]
fn missing_stored_value_falls_back_to_disabled() {
    let repository = OutputPreferenceRepository::new(MemoryRawStore::default());
    assert_eq!(repository.load(), OutputPreferences::default());
}

#[test]
fn controller_updates_memory_before_persisting() {
    let raw = MemoryRawStore {
        value: Some(false),
        fail_writes: true,
    };
    let mut controller = OutputPreferenceController::load(
        OutputPreferenceRepository::new(raw),
    );

    assert_eq!(controller.set_append_space(true), Err(()));
    assert!(controller.current().append_space);
}

#[test]
fn enabled_value_round_trips_through_repository() {
    let mut repository =
        OutputPreferenceRepository::new(MemoryRawStore::default());
    assert_eq!(
        repository.save(OutputPreferences { append_space: true }),
        Ok(())
    );
    assert!(repository.load().append_space);
}
```

`MemoryRawStore` реализует реальный контракт сырого хранилища:

```rust
#[derive(Default)]
struct MemoryRawStore {
    value: Option<bool>,
    fail_writes: bool,
}

impl RawOutputPreferenceStore for MemoryRawStore {
    fn append_space(&self) -> Option<bool> {
        self.value
    }

    fn set_append_space(&mut self, value: bool) -> Result<(), ()> {
        if self.fail_writes {
            Err(())
        } else {
            self.value = Some(value);
            Ok(())
        }
    }
}
```

- [ ] **Шаг 2: запустить тесты и подтвердить RED**

Выполнить:

```bash
cargo test output_preferences::tests --lib
```

Ожидание: компиляция завершается ошибкой, потому что типы настройки и
репозитория ещё не определены.

- [ ] **Шаг 3: реализовать минимальную модель и тестируемое хранилище**

Добавить точные формы:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OutputPreferences {
    pub append_space: bool,
}

pub trait RawOutputPreferenceStore {
    fn append_space(&self) -> Option<bool>;
    fn set_append_space(&mut self, value: bool) -> Result<(), ()>;
}

pub struct OutputPreferenceRepository<R> {
    raw: R,
}

impl<R: RawOutputPreferenceStore> OutputPreferenceRepository<R> {
    pub fn new(raw: R) -> Self;
    pub fn load(&self) -> OutputPreferences;
    pub fn save(&mut self, value: OutputPreferences) -> Result<(), ()>;
}

pub struct OutputPreferenceController<R> {
    current: OutputPreferences,
    repository: OutputPreferenceRepository<R>,
}

impl<R: RawOutputPreferenceStore> OutputPreferenceController<R> {
    pub fn load(repository: OutputPreferenceRepository<R>) -> Self;
    pub const fn current(&self) -> OutputPreferences;
    pub fn set_append_space(&mut self, value: bool) -> Result<(), ()>;
}
```

`set_append_space` сначала меняет `current`, затем пытается сохранить полное
значение. Это обеспечивает действие настройки в текущем запуске даже при
ошибке записи.

- [ ] **Шаг 4: проверить GREEN чистой модели**

Выполнить:

```bash
cargo test output_preferences::tests --lib
```

Ожидание: все тесты модуля проходят.

- [ ] **Шаг 5: добавить системный адаптер**

Включить `NSUserDefaults` в списке возможностей `objc2-foundation`. Реализовать
`SystemOutputPreferenceStore` с ключом
`ptt2me.output.append-space`:

```rust
pub struct SystemOutputPreferenceStore {
    defaults: Retained<NSUserDefaults>,
}

impl SystemOutputPreferenceStore {
    pub fn standard() -> Self;
}
```

`append_space` сначала вызывает `objectForKey`, чтобы отличить отсутствующее
значение от сохранённого `false`, затем читает `boolForKey`.
`set_append_space` вызывает `setBool_forKey`; результат `synchronize() == false`
преобразуется в `Err(())`.

Экспортировать модуль из `src/lib.rs`.

- [ ] **Шаг 6: запустить тесты и закоммитить**

Выполнить:

```bash
cargo test output_preferences::tests --lib
```

После GREEN:

```bash
git add Cargo.toml src/lib.rs src/output_preferences.rs
git commit -m "feat: persist trailing space preference"
```

---

### Задача 2: Добавление пробела на границе вставки

**Файлы:**

- Изменить: `src/inserter.rs`

**Интерфейсы:**

- Потребляет: `append_space: bool`
- Изменяет: `normalize_text(text: &str, append_space: bool) -> Option<String>`
- Изменяет: `insert_text(text: &str, append_space: bool) -> Result<(), InsertError>`

- [ ] **Шаг 1: написать падающие тесты форматирования**

Заменить тесты `normalize_text` на поведенческие проверки:

```rust
#[test]
fn disabled_trailing_space_preserves_normalized_model_output() {
    assert_eq!(
        normalize_text("  Привет! \n", false),
        Some("Привет!".into())
    );
}

#[test]
fn enabled_trailing_space_follows_model_punctuation() {
    for (input, expected) in [
        ("Привет.", "Привет. "),
        ("Привет!", "Привет! "),
        ("Привет?", "Привет? "),
        ("Привет", "Привет "),
    ] {
        assert_eq!(normalize_text(input, true), Some(expected.into()));
    }
}

#[test]
fn trailing_space_option_does_not_make_empty_recognition_insertable() {
    assert_eq!(normalize_text(" \n\t ", true), None);
}
```

Производственная поломка, которую ловят тесты: потеря конечного пробела из-за
повторного `trim`, изменение пунктуации модели или превращение пустого
результата в строку из одного пробела.

- [ ] **Шаг 2: запустить тесты и подтвердить RED**

Выполнить:

```bash
cargo test inserter::tests --lib
```

Ожидание: компиляция падает из-за нового аргумента `append_space`.

- [ ] **Шаг 3: реализовать минимальное форматирование**

Изменить функцию:

```rust
pub fn normalize_text(text: &str, append_space: bool) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut normalized = trimmed.to_owned();
    if append_space {
        normalized.push(' ');
    }
    Some(normalized)
}
```

Передать `append_space` через `insert_with` и `insert_text`, не изменяя
последовательность снимка, временной записи, Command-V и восстановления.

- [ ] **Шаг 4: доказать, что временный буфер содержит пробел**

Добавить тест к `FakePasteboard`:

```rust
#[test]
fn temporary_text_keeps_requested_trailing_space() {
    let mut pasteboard =
        FakePasteboard::with_snapshot(PasteboardSnapshot::default());

    assert_eq!(
        insert_with(
            "Привет.",
            true,
            &mut pasteboard,
            &mut FakePasteCommand::succeed(),
            &mut FakeSleeper::default(),
        ),
        Ok(())
    );

    assert_eq!(
        pasteboard.temporary_texts,
        vec!["Привет. ".to_owned()]
    );
}
```

Расширить `FakePasteboard` полем `temporary_texts: Vec<String>` и записывать в
него фактический аргумент `write_temporary_text`. Это проверяет реальную
границу компонента, а не мок-вызов.

- [ ] **Шаг 5: запустить тесты вставки и закоммитить**

Выполнить:

```bash
cargo test inserter::tests --lib
```

Ожидание: все тесты вставки проходят.

После GREEN:

```bash
git add src/inserter.rs
git commit -m "feat: append optional trailing space on insertion"
```

---

### Задача 3: Чекбокс меню и подключение runtime

**Файлы:**

- Изменить: `src/menu.rs`
- Изменить: `src/runtime.rs`

**Интерфейсы:**

- Потребляет: `OutputPreferenceController<SystemOutputPreferenceStore>`
- Создаёт: `MenuCommand::SetAppendSpace(bool)`
- Изменяет:
  `MenuBar::new(append_space: bool, commands: Sender<MenuCommand>) -> Self`
- Создаёт: `MenuBar::render_append_space(append_space: bool)`

- [ ] **Шаг 1: написать падающие тесты порядка меню и переключения команды**

В `src/menu.rs` заменить инвариант четырёх строк и добавить проверку чистого
перехода:

```rust
#[test]
fn menu_descriptor_contains_trailing_space_before_separator() {
    assert_eq!(
        MENU_DESCRIPTOR,
        [
            MenuEntry::Status,
            MenuEntry::Version,
            MenuEntry::TrailingSpace,
            MenuEntry::Separator,
            MenuEntry::Quit,
        ]
    );
}

#[test]
fn toggling_trailing_space_emits_the_new_selected_value() {
    assert_eq!(
        toggled_append_space(false),
        (true, MenuCommand::SetAppendSpace(true))
    );
    assert_eq!(
        toggled_append_space(true),
        (false, MenuCommand::SetAppendSpace(false))
    );
}
```

Поломки, которые ловят тесты: отсутствие пункта, неверное место пункта или
повторная отправка старого значения вместо нового.

- [ ] **Шаг 2: запустить тесты и подтвердить RED**

Выполнить:

```bash
cargo test menu::tests --lib
```

Ожидание: компиляция падает, потому что новый пункт и команда отсутствуют.

- [ ] **Шаг 3: реализовать команду и чекбокс**

Добавить:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    SetAppendSpace(bool),
}

fn toggled_append_space(current: bool) -> (bool, MenuCommand) {
    let next = !current;
    (next, MenuCommand::SetAppendSpace(next))
}
```

Дать `MenuTarget` ivars с `Sender<MenuCommand>` и `Cell<bool>`. Objective-C
действие `toggleTrailingSpace:` вычисляет новое значение, обновляет `Cell` и
отправляет команду. Создать пункт с точным заголовком `Пробел в конце`,
действием `toggleTrailingSpace:`, включённым состоянием и target.

`MenuBar` удерживает `trailing_space_row`. Метод `render_append_space` ставит
`NSControlStateValueOn` или `NSControlStateValueOff`. Вызвать его при создании
меню с загруженным значением.

- [ ] **Шаг 4: запустить тесты меню и подтвердить GREEN**

Выполнить:

```bash
cargo test menu::tests --lib
```

Ожидание: все тесты меню проходят.

- [ ] **Шаг 5: написать падающий тест применения команды**

Добавить в `src/runtime.rs` чистую функцию:

```rust
fn apply_menu_command<R: RawOutputPreferenceStore>(
    command: MenuCommand,
    preferences: &mut OutputPreferenceController<R>,
) -> Result<OutputPreferences, ()>
```

и тест:

```rust
#[test]
fn menu_command_updates_current_preference_even_when_persistence_fails() {
    let repository = OutputPreferenceRepository::new(FailingRawStore);
    let mut preferences = OutputPreferenceController::load(repository);

    assert_eq!(
        apply_menu_command(
            MenuCommand::SetAppendSpace(true),
            &mut preferences,
        ),
        Err(())
    );
    assert!(preferences.current().append_space);
}
```

Тестовый `FailingRawStore` возвращает `Some(false)` при чтении и `Err(())` при
записи.

- [ ] **Шаг 6: запустить runtime-тест и подтвердить RED**

Выполнить:

```bash
cargo test runtime::tests::menu_command_updates_current_preference_even_when_persistence_fails --lib
```

Ожидание: компиляция падает, потому что `apply_menu_command` отсутствует.

- [ ] **Шаг 7: подключить канал, хранение и вставку**

При запуске runtime:

1. Создать `OutputPreferenceRepository::new(
   SystemOutputPreferenceStore::standard())`.
2. Загрузить `OutputPreferenceController`.
3. Создать канал `MenuCommand`.
4. Передать текущее `append_space` и sender в `MenuBar::new`.
5. Сохранить controller и receiver в `Runtime`.

В начале `drain_events` собрать команды меню. Для каждой команды вызвать
`apply_menu_command`, затем всегда обновить `menu.render_append_space` по
текущему значению. Ошибку сохранения записать как
`tracing::warn!(error_category = "output_preference_write")`.

При `Effect::InsertText(text)` вызвать:

```rust
inserter::insert_text(
    &text,
    self.output_preferences.current().append_space,
)
```

- [ ] **Шаг 8: запустить тесты меню и runtime, затем закоммитить**

Выполнить:

```bash
cargo test menu::tests --lib
cargo test runtime::tests --lib
```

После GREEN:

```bash
git add src/menu.rs src/runtime.rs
git commit -m "feat: add trailing space menu checkbox"
```

---

### Задача 4: Документация и полная проверка

**Файлы:**

- Изменить: `README.md`

**Интерфейсы:**

- Документирует точный пункт `Пробел в конце`, выключенное значение по
  умолчанию и сохранение выбора.

- [ ] **Шаг 1: обновить пользовательскую документацию**

Заменить старое описание меню точным:

```text
<status>
PTT2me <version>
Пробел в конце
────────────
Выйти
```

Добавить, что чекбокс по умолчанию выключен, добавляет один пробел после
реплики и сохраняется между запусками. В разделе приватности заменить
утверждение об отсутствии любых настроек: приложение хранит только этот
логический выбор и не хранит аудио, распознанный текст или историю.

- [ ] **Шаг 2: проверить форматирование и весь набор тестов**

Выполнить:

```bash
cargo fmt --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

Ожидание: каждая команда завершается с кодом `0`, без ошибок и предупреждений.

- [ ] **Шаг 3: проверить комплект приложения**

Выполнить:

```bash
scripts/build-app.sh
```

Ожидание: release-сборка завершается успешно, а встроенная проверка комплекта
подтверждает структуру, архитектуру, подпись и наличие ресурсов.

- [ ] **Шаг 4: проверить итоговый diff и закоммитить документацию**

Выполнить:

```bash
git diff --check
git status --short
git diff --stat HEAD~3..HEAD
```

Затем:

```bash
git add README.md docs/superpowers/specs/2026-07-31-optional-trailing-space-design.md docs/superpowers/plans/2026-07-31-optional-trailing-space.md
git commit -m "docs: document trailing space option"
```

Ручную проверку переключения, перезапуска и двух последовательных диктовок
оставить явным пунктом передачи пользователю, если GUI-приложение не запускалось
в текущем сеансе.
