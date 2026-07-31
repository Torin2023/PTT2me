const DOWNLOAD_URL =
  "https://github.com/Torin2023/PTT2me/releases/download/v1.0.5/PTT2me-1.0.5-macos-arm64.dmg";
const REPOSITORY_URL = "https://github.com/Torin2023/PTT2me";
const RELEASE_URL = `${REPOSITORY_URL}/releases/tag/v1.0.5`;

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
            <span aria-hidden="true" />
            Локальная диктовка для macOS
            <span className="preview-badge">Preview 1.0.5</span>
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
              Скачать PTT2me 1.0.5
              <span aria-hidden="true">↘</span>
            </a>
            <a className="text-link" href={REPOSITORY_URL}>
              Исходный код на GitHub
              <span aria-hidden="true">↗</span>
            </a>
          </div>
          <p className="compatibility">
            Apple Silicon <i /> macOS 13+ <i /> 182 МБ <i /> без облака
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
            PTT2me сначала вставляет текст напрямую через Accessibility или
            Unicode — эти способы не меняют буфер обмена. Если приложению нужен
            совместимый Command-V fallback, PTT2me восстанавливает все прежние
            элементы и форматы буфера и никогда не перезаписывает более новые
            изменения.
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

      <section className="section install" id="install">
        <div className="install-copy">
          <p className="eyebrow">PTT2me Preview 1.0.5</p>
          <h2>Готовы говорить?</h2>
          <p>Для Mac с Apple Silicon и macOS 13 Ventura или новее.</p>
          <a className="button" href={DOWNLOAD_URL}>
            Скачать Preview DMG · 182 МБ
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
              При первом запуске откройте приложение через контекстное меню
              «Открыть»: текущая сборка подписана ad-hoc и не нотарифицирована
              Apple.
            </p>
          </li>
          <li>
            <span>3</span>
            <p>
              Разрешите микрофон, мониторинг ввода и универсальный доступ.
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
            d89a1767edfb2c010ba98ffc59f6c35f8e346958c492b3ed33b4596f303a7c8c
          </code>
        </p>
        <div>
          <a href={RELEASE_URL}>Preview-релиз 1.0.5</a>
          <a href={REPOSITORY_URL}>GitHub</a>
        </div>
      </footer>
    </main>
  );
}
