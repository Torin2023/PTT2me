# Manual P0 checklist PTT2me

Заполняйте отдельную копию этого шаблона для каждого release candidate. Копия
должна находиться рядом с локальным release-набором, но не внутри закрытого
каталога из пяти outputs Gate C и не в Git.

## Identity и проверяющий

- Версия:
- Build (YYYYMMDDHHMM):
- Source commit:
- Expected tag или `rehearsal without tag`:
- Full DMG SHA-256:
- Update DMG SHA-256:
- Signed manifest SHA-256:
- Результат `scripts/verify-release-artifacts.sh`:
- Проверяющий:
- Дата проверки:
- Controlled Mac / macOS version:

Правило: `SKIPPED`, timeout, отсутствие GUI/TCC/device access или неполное поле
считаются `FAIL` и блокируют публикацию.

## 1. Запуск, установка и TCC

- [ ] `PTT2me.app` показывает фирменную иконку в Finder и в окне DMG, а не
      универсальный значок приложения.
- [ ] Full DMG установлен через Finder; приложение запускается из
      `/Applications/PTT2me.app`.
- [ ] Первый запуск подготовил exact model и сбросил только Accessibility,
      Input Monitoring и Microphone для новой build identity.
- [ ] Выдача каждого из трёх разрешений последовательно приводит приложение к
      состоянию `Готово`.
- [ ] Отзыв Accessibility блокирует диктовку и показывает правильную recovery
      action для Accessibility.
- [ ] Отзыв Input Monitoring блокирует диктовку и показывает правильную
      recovery action для Input Monitoring.
- [ ] Отзыв Microphone блокирует диктовку и показывает правильную recovery
      action для Microphone.
- [ ] Повторный запуск той же identity не выполняет повторный completed reset.
- [ ] Новый build/source identity выполняет новый reset.

## 2. PTT и клавиатура

- [ ] 20 коротких Fn/Globe presses выполнили системное действие и не запустили
      ASR.
- [ ] 20 длинных Fn/Globe holds запустили ровно по одному
      capture/recognition cycle и не воспроизвели Fn в macOS.
- [ ] Назначенная обычная клавиша сохраняет short press.
- [ ] Комбинация с назначенной клавишей проходит в исходном порядке.
- [ ] Autorepeat не создаёт дополнительный capture/recognition cycle.
- [ ] Tap loss/restore не оставляет stuck key и после восстановления допускает
      следующий корректный cycle.
- [ ] Capture-start failure освобождает pending trigger и не создаёт ложный
      следующий cycle.

## 3. Audio, ASR и вставка

- [ ] Microphone start/stop failure отображается и после устранения причины
      восстанавливается.
- [ ] Пустой recognition output ничего не вставляет.
- [ ] Punctuation модели сохраняется без переписывания.
- [ ] `Пробел в конце` добавляет ровно один ASCII space только при включённой
      настройке.
- [ ] Accessibility-проверка отклоняет вставку в secure text field.
- [ ] Смена focus на secure text field до Command-V блокирует вставку и
      восстанавливает clipboard; ошибка или отсутствие AX focus завершается
      безопасным отказом.
- [ ] Command-V insertion работает в нативном текстовом поле, HTML `input`,
      HTML `textarea`, `contenteditable` и строке ввода Codex.
- [ ] Короткая проверка ChatGPT: в пустом черновике продиктовать «Проверка
      вставки.», сверить конечный текст и пунктуацию; повторить с выключенным
      `Пробел в конце`; сменить фокус на другой черновик во время recognition
      и убедиться, что текст появился только в текущем поле. После каждой
      вставки исходный clipboard сохранён. Сообщения оставлять черновиками.
      Записать вариант ChatGPT (web/desktop), браузер/версию приложения,
      macOS, ожидаемый и фактический текст.
- [ ] Pasteboard insertion восстанавливает plain/rich/image/file representations.
- [ ] Новый пользовательский clipboard, созданный во время вставки, не
      перезаписывается восстановлением старого snapshot.
- [ ] Изменение focused field во время recognition направляет текст в текущий
      focused field согласно продуктовому контракту.

Автоматический AppKit/WebKit fixture описан в
[`../testing/INSERTION_GUI.md`](../testing/INSERTION_GUI.md). Его отчёт дополняет
эту проверку: WKWebView не подтверждает совместимость с Chrome, Codex или
текущим редактором ChatGPT. Успешная сборка fixture не означает успешную вставку.

## 4. Updater

- [ ] Проверенная внешняя модель выбирает Update.
- [ ] Missing/changed model выбирает Full и требует явного подтверждения.
- [ ] Manifest/network/digest/quarantine failures не открывают DMG.
- [ ] Verified DMG открывается только по действию пользователя.
- [ ] Приложение завершает работу только после успешного workspace open.
- [ ] Finder replacement выполнен по README; последующий TCC cycle соответствует
      новой build identity.

## 5. Артефакты и неизменяемость

- [ ] Gate C повторно выполнен с exact source commit.
- [ ] Перед публикацией Gate C повторно выполнен с `--expected-tag vX.Y.Z`.
- [ ] Full содержит exact model; Update не содержит
      `Contents/Resources/models`.
- [ ] Full/Update unsigned Mach-O parity, codesign и Full model smoke прошли.
- [ ] В repository/diff нет model assets, private keys, `.app`, DMG, `dist/`,
      `target/` или release logs.
- [ ] Build/verify scripts не выполняли публикацию.

## Финальное решение владельца

- Итог: `PASS` / `FAIL`
- Blocking failures:
- Дополнительные наблюдения:
- Имя владельца, разрешившего публикацию:
- Дата и время решения (UTC):

Публикация разрешена только при `PASS`, заполненной identity, отсутствии
blocking failures и явном решении владельца.
