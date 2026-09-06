const DOWNLOAD_URL =
  "https://github.com/Torin2023/PTT2me/releases/download/v1.2.0/PTT2me-1.2.0-full-macos-arm64.dmg";
const REPOSITORY_URL = "https://github.com/Torin2023/PTT2me";
const RELEASE_URL = `${REPOSITORY_URL}/releases/tag/v1.2.0`;

const steps = [
  ["01", "Удерживайте", "Нажмите и удерживайте выбранную клавишу."],
  ["02", "Говорите", "Продиктуйте текст по-русски в обычном темпе."],
  ["03", "Отпустите", "Текст появится в активном приложении."],
];

const waveform = [
  18, 28, 42, 30, 58, 76, 48, 86, 64, 92, 54, 72, 44, 66, 38, 52, 28, 18,
];

export default function Home() {
  return (
    <main>
      <header className="site-header" aria-label="Основная навигация">
        <a
          className="brand"
          href="#top"
          aria-label="PTT2me, к началу страницы"
        >
          <span className="brand-mark" aria-hidden="true" />
          PTT2me
        </a>
        <nav aria-label="Разделы сайта">
          <a href="#how">Как работает</a>
          <a href="#updates">Обновления 1.2.0</a>
          <a href={REPOSITORY_URL}>GitHub</a>
          <a className="button button-small" href={DOWNLOAD_URL}>
            Скачать
          </a>
        </nav>
      </header>

      <section className="hero" id="top">
        <div className="hero-glow" aria-hidden="true" />
        <div className="hero-copy">
          <p className="eyebrow">
            <span className="eyebrow-line" aria-hidden="true" />
            Локальная диктовка для macOS
            <span className="preview-badge">Preview 1.2.0</span>
          </p>
          <h1>
            Говорите —
            <br />
            текст уже там<span aria-hidden="true">.</span>
          </h1>
          <p className="lede">
            Выберите удобную клавишу, удерживайте её и говорите по-русски.
            PTT2me начинает запись сразу, распознает речь прямо на вашем Mac и
            вставляет результат в поле, где находится курсор.
          </p>
          <div className="hero-actions">
            <a className="button" href={DOWNLOAD_URL}>
              Скачать PTT2me 1.2.0
              <span aria-hidden="true">↘</span>
            </a>
            <a className="text-link" href={REPOSITORY_URL}>
              Исходный код на GitHub
              <span aria-hidden="true">↗</span>
            </a>
          </div>
          <p className="compatibility">
            Apple Silicon <i /> macOS 13+ <i /> 184,5 МиБ <i /> без облака
          </p>
        </div>

        <div
          className="dictation-demo"
          aria-label="Схема: удерживайте выбранную клавишу, говорите и получите текст"
        >
          <div className="demo-topline">
            <span>PTT2me</span>
            <span className="live-state">
              <i aria-hidden="true" />
              Слушаю
            </span>
          </div>
          <div className="demo-stage">
            <div className="fn-key" aria-hidden="true">
              <span>fn</span>
              <small>держите</small>
            </div>
            <div className="waveform" aria-hidden="true">
              {waveform.map((height, index) => (
                <i
                  key={`${height}-${index}`}
                  style={{ "--bar-height": `${height}%` } as React.CSSProperties}
                />
              ))}
            </div>
          </div>
          <div className="transcript">
            <span aria-hidden="true">Aa</span>
            <p>
              Встречаемся завтра
              <br />в десять.
            </p>
            <i aria-hidden="true" />
          </div>
          <div className="demo-caption">
            <span>Клавиша удерживается</span>
            <span>Локально на Mac</span>
          </div>
        </div>
      </section>

      <section className="highlights" aria-label="Преимущества">
        <article>
          <span>01</span>
          <h2>Без облака</h2>
          <p>Речь обрабатывается локально моделью GigaAM v3.</p>
        </article>
        <article>
          <span>02</span>
          <h2>Ваша клавиша</h2>
          <p>
            Назначьте удобную клавишу. Короткое нажатие останется обычным,
            удержание запустит диктовку.
          </p>
        </article>
        <article>
          <span>03</span>
          <h2>Точно в курсор</h2>
          <p>Результат получает поле, которое активно после распознавания.</p>
        </article>
      </section>

      <section className="section how-section" id="how">
        <div className="section-heading">
          <p className="eyebrow">Один привычный жест</p>
          <h2>Нажали. Сказали. Продолжили.</h2>
        </div>
        <div className="steps">
          {steps.map(([number, title, body]) => (
            <article key={number}>
              <span>{number}</span>
              <div className="step-icon" aria-hidden="true">
                {number === "01" ? "fn" : number === "02" ? "≈" : "Aa"}
              </div>
              <h3>{title}</h3>
              <p>{body}</p>
            </article>
          ))}
        </div>
        <p className="fine-print">
          В меню доступны пороги 250, 500 и 750 мс; по умолчанию — 500 мс.
          Запись начинается сразу при нажатии. Если нажатие оказалось коротким,
          запись отбрасывается, а клавиша передаётся macOS как обычно. После
          отпускания PTT2me оставляет 180 мс, чтобы не обрезать окончание фразы.
          Максимальная запись — 25 секунд. Настройка «Пробел в конце» позволяет
          разделять последовательные фразы, не изменяя пунктуацию модели.
        </p>
      </section>

      <section className="section privacy" id="privacy">
        <div className="privacy-orbit" aria-hidden="true">
          <span className="orbit-one" />
          <span className="orbit-two" />
          <span className="privacy-core">⌁</span>
        </div>
        <div className="privacy-copy">
          <p className="eyebrow">Ваши слова остаются вашими</p>
          <h2>Распознавание без отправки в интернет.</h2>
          <p>
            Accessibility проверяет активное поле и отклоняет защищённый ввод.
            В обычные поля PTT2me вставляет текст через системный Command-V:
            это работает в нативных приложениях, браузерных input, textarea и
            contenteditable, а также в строке ввода Codex. После вставки PTT2me
            восстанавливает все прежние элементы и форматы буфера и никогда не
            перезаписывает более новые изменения.
          </p>
          <div className="privacy-facts" aria-label="Факты о приватности">
            <span>Нет аккаунта</span>
            <span>Нет аналитики</span>
            <span>Нет истории</span>
          </div>
        </div>
      </section>

      <section className="section permissions-section" id="permissions">
        <div className="section-heading">
          <p className="eyebrow">Только необходимые разрешения</p>
          <h2>Три системных доступа — и ни одного лишнего.</h2>
        </div>
        <div className="permissions">
          <article>
            <span className="permission-icon" aria-hidden="true">
              ●
            </span>
            <div>
              <b>Микрофон</b>
              <p>Чтобы слышать вашу речь.</p>
            </div>
          </article>
          <article>
            <span className="permission-icon key-icon" aria-hidden="true">
              fn
            </span>
            <div>
              <b>Мониторинг ввода</b>
              <p>Чтобы реагировать на назначенную клавишу.</p>
            </div>
          </article>
          <article>
            <span className="permission-icon" aria-hidden="true">
              ↗
            </span>
            <div>
              <b>Универсальный доступ</b>
              <p>Чтобы вставлять текст в активное приложение.</p>
            </div>
          </article>
        </div>
        <p className="permission-note">
          Если разрешения не хватает, «Открыть настройки…» повторно открывает
          соответствующий раздел «Конфиденциальность и безопасность».
        </p>
      </section>

      <section className="section updates-section" id="updates">
        <div className="section-heading">
          <p className="eyebrow">PTT2me 1.2.0 опубликована</p>
          <div>
            <h2>Восстановление обновлений и вставка текста.</h2>
            <p className="release-boundary">
              Версиям 1.1.0 и 1.1.1 нужен один ручной переход на 1.2.0 через
              Full DMG: прежний updater не может скачать сборку с собственным
              исправлением. Версии начиная с 1.1.2 могут загрузить 1.2.0 через
              меню обновлений. В 1.2.0 добавлено восстановление после сбоя
              обработчика обновлений и исправлена проверка полей перед вставкой.
            </p>
          </div>
        </div>

        <div className="update-cards">
          <article>
            <span>01</span>
            <h3>Спокойный график</h3>
            <p>
              Первая автоматическая проверка начинается не раньше чем через 60
              секунд после запуска, следующие — не чаще одного раза в 24 часа.
              Пункт «Проверить обновления…» запускает ручную проверку сразу.
            </p>
          </article>
          <article>
            <span>02</span>
            <h3>Два пакета</h3>
            <p>
              Для новой установки всегда нужен Full DMG с моделью. Update DMG
              без модели выбирает только уже установленное приложение и только
              когда внешняя модель проверена по точному составу и контрольным
              суммам.
            </p>
          </article>
          <article>
            <span>03</span>
            <h3>Решение за вами</h3>
            <p>
              Проверка ничего не скачивает и не устанавливает. Только после
              выбора «Скачать обновление …» PTT2me загружает выбранный DMG из
              GitHub Release и проверяет его.
            </p>
          </article>
        </div>

        <div className="update-flow">
          <h3>Как пройдёт обновление</h3>
          <ol>
            <li>
              Выберите «Скачать обновление …» и дождитесь завершения проверки.
            </li>
            <li>
              Выберите «Открыть DMG и выйти…», затем замените PTT2me.app через
              Finder вручную.
            </li>
            <li>
              Если macOS блокирует эту неподписанную сборку, разрешите только
              PTT2me через «Открыть всё равно» в разделе «Конфиденциальность и
              безопасность». Gatekeeper не нужно отключать глобально.
            </li>
            <li>
              При запуске новая сборка сначала проверяет и подготавливает
              модель, затем автоматически сбрасывает решения для Универсального
              доступа, Мониторинга ввода и Микрофона. Если сброс не завершён,
              используйте «Повторить сброс разрешений».
            </li>
            <li>
              После сброса выдайте три разрешения заново в Настройках системы.
            </li>
          </ol>
        </div>

        <div className="update-notes">
          <article>
            <p className="eyebrow">Что сохраняется</p>
            <h3>Модель остаётся между версиями.</h3>
            <p>
              Проверенная модель хранится в{" "}
              <code>
                ~/Library/Application Support/PTT2me/models/gigaam-v3-rnnt-v1/
              </code>{" "}
              и не удаляется при замене приложения. Full DMG создаёт это
              хранилище при необходимости, а Update DMG использует его только
              после повторной проверки.
            </p>
            <p>
              Подписанная запись о релизе и проверенные DMG хранятся в{" "}
              <code>~/Library/Caches/com.ptt2me.app/</code>. В{" "}
              <code>~/Library/Preferences/com.ptt2me.app.plist</code> находятся
              настройки, время последней сетевой проверки и маркеры{" "}
              <code>PermissionsResetForBuild</code> и{" "}
              <code>PermissionsSetupCompletedForBuild</code>. Аудио, текст и
              история распознавания туда не записываются.
            </p>
          </article>
          <article>
            <p className="eyebrow">Полное удаление</p>
            <h3>Все данные удаляются вручную.</h3>
            <p>
              Закройте PTT2me и удалите{" "}
              <code>/Applications/PTT2me.app</code>,{" "}
              <code>~/Library/Application Support/PTT2me/</code>,{" "}
              <code>~/Library/Caches/com.ptt2me.app/</code> и{" "}
              <code>~/Library/Preferences/com.ptt2me.app.plist</code>. Затем
              удалите PTT2me из списков Универсального доступа, Мониторинга
              ввода и Микрофона в Настройках системы. Приложение не удаляет
              внешнюю модель автоматически.
            </p>
          </article>
        </div>

        <p className="update-privacy-note">
          Запрос подписанной записи идёт на GitHub Pages без идентификатора
          пользователя, устройства или телеметрии. Аудио, речь и распознанный
          текст по-прежнему обрабатываются только на Mac.
        </p>
      </section>

      <section className="section install" id="install">
        <div className="install-copy">
          <p className="eyebrow">PTT2me Preview 1.2.0</p>
          <h2>Готовы говорить?</h2>
          <p>Для Mac с Apple Silicon и macOS 13 Ventura или новее.</p>
          <p className="fine-print">
            Полная ручная проверка этой версии не завершена.
            {" "}<a href={RELEASE_URL}>Подробности и ограничения preview.</a>
          </p>
          <a className="button" href={DOWNLOAD_URL}>
            Скачать Full DMG · 184,5 МиБ
            <span aria-hidden="true">↘</span>
          </a>
        </div>
        <ol>
          <li>
            <span>1</span>
            <p>
              Откройте DMG и перетащите PTT2me в папку «Программы».
            </p>
          </li>
          <li>
            <span>2</span>
            <p>
              Запустите PTT2me из папки «Программы».
            </p>
          </li>
          <li>
            <span>3</span>
            <p>
              Если macOS блокирует запуск, откройте Настройки системы →
              Конфиденциальность и безопасность, нажмите «Открыть всё равно» и
              подтвердите запуск только PTT2me. Не отключайте Gatekeeper
              глобально.
            </p>
          </li>
          <li>
            <span>4</span>
            <p>
              Разрешите микрофон, мониторинг ввода и универсальный доступ в
              Настройках системы.
            </p>
          </li>
        </ol>
      </section>

      <footer>
        <a className="brand" href="#top">
          <span className="brand-mark" aria-hidden="true" />
          PTT2me
        </a>
        <p>
          SHA-256{" "}
          <code>
            575e55e957a0527f03f9bb21f070ae629d697635a24eb9b84c3d569931248372
          </code>
        </p>
        <div>
          <a href={RELEASE_URL}>Preview-релиз 1.2.0</a>
          <a href={REPOSITORY_URL}>GitHub</a>
        </div>
      </footer>
    </main>
  );
}
