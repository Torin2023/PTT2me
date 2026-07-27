const DOWNLOAD_URL =
  "https://github.com/Torin2023/PTT2me/releases/download/v1.0.2/PTT2me-1.0.2-macos-arm64.dmg";
const REPOSITORY_URL = "https://github.com/Torin2023/PTT2me";
const RELEASE_URL = `${REPOSITORY_URL}/releases/tag/v1.0.2`;
const CHECKSUM_URL = `${DOWNLOAD_URL}.sha256`;

const steps = [
  ["01", "Удерживайте", "Нажмите и удерживайте Fn или Globe."],
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
          </p>
          <h1>
            Говорите —
            <br />
            текст уже там<span aria-hidden="true">.</span>
          </h1>
          <p className="lede">
            Удерживайте Fn, говорите по-русски и отпускайте. PTT2me распознает
            речь прямо на вашем Mac и вставит текст в активное приложение.
          </p>
          <div className="hero-actions">
            <a className="button" href={DOWNLOAD_URL}>
              Скачать PTT2me 1.0.2
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
          aria-label="Схема: удерживайте Fn, говорите и получите текст"
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
            <span>Fn удерживается</span>
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
          <h2>Одна клавиша</h2>
          <p>Никаких окон: удерживайте Fn или Globe.</p>
        </article>
        <article>
          <span>03</span>
          <h2>В любом приложении</h2>
          <p>Готовый текст вставляется туда, где находится курсор.</p>
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
          Минимальное удержание — 250 мс. После отпускания PTT2me оставляет
          180 мс, чтобы не обрезать окончание фразы. Максимальная запись —
          25 секунд.
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
            PTT2me не сохраняет аудио, расшифровки, историю, настройки или
            данные приложения. Буфер обмена восстанавливается после вставки,
            если вы не скопировали что-то новое.
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
              <p>Чтобы реагировать на Fn или Globe.</p>
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
      </section>

      <section className="section install" id="install">
        <div className="install-copy">
          <p className="eyebrow">PTT2me 1.0.2</p>
          <h2>Готовы говорить?</h2>
          <p>Для Mac с Apple Silicon и macOS 13 Ventura или новее.</p>
          <a className="button" href={DOWNLOAD_URL}>
            Скачать DMG · 182 МБ
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
            1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53
          </code>
        </p>
        <div>
          <a href={CHECKSUM_URL}>Checksum</a>
          <a href={RELEASE_URL}>Релиз 1.0.2</a>
          <a href={REPOSITORY_URL}>GitHub</a>
        </div>
      </footer>
    </main>
  );
}
