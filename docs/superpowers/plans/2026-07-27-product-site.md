# PTT2me Product Site Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and privately publish a Russian-language, single-page PTT2me product site whose primary action downloads the official 1.0.2 DMG.

**Architecture:** Initialize the bundled Sites vinext starter inside `site/` so the Rust application remains untouched. Implement the complete landing page as a server-rendered page plus one global stylesheet, use a rendered-HTML contract test for product copy and starter removal, then package and deploy the validated Cloudflare Worker output through Sites.

**Tech Stack:** React 19, vinext, TypeScript, CSS, Node test runner, Cloudflare Workers-compatible ESM, Sites hosting

## Global Constraints

- The product language is Russian.
- The primary headline is «Говорите — текст уже там».
- The primary download is PTT2me 1.0.2 for Apple Silicon from the official GitHub release asset.
- Compatibility is macOS 13 Ventura or newer; Intel support must not be implied.
- Recognition uses the bundled GigaAM v3 model and remains local.
- The page must not claim notarization; the release is ad-hoc signed and not notarized.
- The page has no analytics, account, form, microphone request, runtime GitHub API request, or durable state.
- The finished page must be keyboard accessible, responsive, and static under `prefers-reduced-motion: reduce`.
- Do not bundle the 182 MB DMG into the site.
- Preserve the starter's package manager, lockfile, vinext architecture, and `sites()` Vite plugin.

---

### Task 1: Initialize the Sites project and lock the rendered content contract

**Files:**
- Create: `site/` from the Sites vinext starter
- Modify: `site/tests/rendered-html.test.mjs`
- Delete: `site/app/_sites-preview/SkeletonPreview.tsx`
- Delete: `site/app/_sites-preview/preview.css`
- Modify: `site/package.json`
- Modify: `site/package-lock.json`

**Interfaces:**
- Consumes: the approved product-site design and the Sites initializer
- Produces: a working vinext project whose test requires product metadata, content, links, and no starter preview

- [ ] **Step 1: Initialize the site in its isolated project surface**

Run:

```bash
/Users/andrey/.codex/plugins/cache/openai-bundled/sites/0.1.31/scripts/init-site.sh /Users/andrey/dev/ptt2me/site
```

Expected: `site/package.json`, `site/app/page.tsx`, and
`site/.openai/hosting.json` exist, and dependency installation succeeds.

- [ ] **Step 2: Start the visible development preview**

Run from `site/`:

```bash
npm run dev
```

Keep the retained process running, use the exact printed Local URL, and open it
once in the Codex browser. Do not perform browser QA unless the user requests
it.

- [ ] **Step 3: Replace the starter test with the product contract**

Replace `site/tests/rendered-html.test.mjs` with:

```js
import assert from "node:assert/strict";
import { access } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

async function render(path = "/") {
  const workerUrl = new URL("../dist/server/index.js", import.meta.url);
  workerUrl.searchParams.set("test", `${process.pid}-${Date.now()}`);
  const { default: worker } = await import(workerUrl.href);
  return worker.fetch(
    new Request(`http://localhost${path}`, {
      headers: { accept: "text/html" },
    }),
    { ASSETS: { fetch: async () => new Response("Not found", { status: 404 }) } },
    { waitUntil() {}, passThroughOnException() {} },
  );
}

test("renders the PTT2me product page", async () => {
  const response = await render();
  assert.equal(response.status, 200);
  const html = await response.text();
  assert.match(html, /<html[^>]*lang="ru"/i);
  assert.match(html, /<title>PTT2me — локальная диктовка для macOS<\/title>/i);
  assert.match(html, /Говорите — текст уже там/);
  assert.match(html, /Без облака/);
  assert.match(html, /Microphone|Микрофон/);
  assert.match(html, /1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53/);
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/download\/v1\.0\.2\/PTT2me-1\.0\.2-macos-arm64\.dmg/,
  );
  assert.doesNotMatch(html, /codex-preview|react-loading-skeleton|Your site is taking shape/i);
});

test("removes the disposable starter preview", async () => {
  await assert.rejects(access(new URL("app/_sites-preview", root)));
});
```

- [ ] **Step 4: Run the contract and verify it fails against the starter**

Run:

```bash
npm test
```

Expected: FAIL because the starter title and loading skeleton do not satisfy
the PTT2me contract.

- [ ] **Step 5: Remove the disposable starter**

Delete `site/app/_sites-preview/`, remove its import from `site/app/page.tsx`,
then run:

```bash
npm uninstall react-loading-skeleton
```

Expected: both package files no longer contain `react-loading-skeleton`.

---

### Task 2: Implement the complete landing page and product metadata

**Files:**
- Modify: `site/app/page.tsx`
- Modify: `site/app/globals.css`
- Modify: `site/app/layout.tsx`
- Modify: `site/public/favicon.svg`

**Interfaces:**
- Consumes: the Task 1 rendered-HTML contract
- Produces: a single accessible route with stable section IDs `how`, `privacy`, `permissions`, and `install`

- [ ] **Step 1: Implement the semantic page structure**

Use this top-level structure in `site/app/page.tsx`, filling each array with the
exact approved Russian copy:

```tsx
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

export default function Home() {
  return (
    <main>
      <header className="site-header" aria-label="Основная навигация">
        <a className="brand" href="#top" aria-label="PTT2me, к началу страницы">PTT2me</a>
        <nav>
          <a href="#how">Как работает</a>
          <a href={REPOSITORY_URL}>GitHub</a>
          <a className="button button-small" href={DOWNLOAD_URL}>Скачать</a>
        </nav>
      </header>

      <section className="hero" id="top">
        <div className="hero-copy">
          <p className="eyebrow">Локальная диктовка для macOS</p>
          <h1>Говорите —<br />текст уже там<span aria-hidden="true">.</span></h1>
          <p className="lede">
            Удерживайте Fn, говорите по-русски и отпускайте. PTT2me распознает
            речь прямо на вашем Mac и вставит текст в активное приложение.
          </p>
          <div className="hero-actions">
            <a className="button" href={DOWNLOAD_URL}>Скачать PTT2me 1.0.2</a>
            <a className="text-link" href={REPOSITORY_URL}>Исходный код на GitHub</a>
          </div>
          <p className="compatibility">Apple Silicon · macOS 13+ · 182 МБ · без облака</p>
        </div>
        <div className="dictation-demo" aria-label="Схема: удерживайте Fn, говорите и получите текст">
          <div className="fn-key"><span>fn</span><small>держите</small></div>
          <div className="waveform" aria-hidden="true">
            {Array.from({ length: 18 }, (_, index) => <i key={index} />)}
          </div>
          <div className="transcript">Встречаемся завтра в десять.</div>
        </div>
      </section>

      <section className="highlights" aria-label="Преимущества">
        <article><span>01</span><h2>Без облака</h2><p>Речь обрабатывается локально моделью GigaAM v3.</p></article>
        <article><span>02</span><h2>Одна клавиша</h2><p>Никаких окон: удерживайте Fn или Globe.</p></article>
        <article><span>03</span><h2>В любом приложении</h2><p>Готовый текст вставляется туда, где находится курсор.</p></article>
      </section>

      <section className="section" id="how">
        <p className="eyebrow">Один привычный жест</p>
        <h2>Нажали. Сказали. Продолжили.</h2>
        <div className="steps">
          {steps.map(([number, title, body]) => (
            <article key={number}><span>{number}</span><h3>{title}</h3><p>{body}</p></article>
          ))}
        </div>
        <p className="fine-print">Минимальное удержание — 250 мс. После отпускания PTT2me оставляет 180 мс, чтобы не обрезать окончание фразы. Максимальная запись — 25 секунд.</p>
      </section>

      <section className="section privacy" id="privacy">
        <p className="eyebrow">Ваши слова остаются вашими</p>
        <h2>Распознавание без отправки в интернет.</h2>
        <p>PTT2me не сохраняет аудио, расшифровки, историю, настройки или данные приложения. Буфер обмена восстанавливается после вставки, если вы не скопировали что-то новое.</p>
      </section>

      <section className="section" id="permissions">
        <p className="eyebrow">Только необходимые разрешения</p>
        <h2>Три системных доступа — и ни одного лишнего.</h2>
        <div className="permissions">
          <article><b>Микрофон</b><p>Чтобы слышать вашу речь.</p></article>
          <article><b>Мониторинг ввода</b><p>Чтобы реагировать на Fn или Globe.</p></article>
          <article><b>Универсальный доступ</b><p>Чтобы вставлять текст в активное приложение.</p></article>
        </div>
      </section>

      <section className="section install" id="install">
        <div>
          <p className="eyebrow">PTT2me 1.0.2</p>
          <h2>Готовы говорить?</h2>
          <p>Для Mac с Apple Silicon и macOS 13 Ventura или новее.</p>
          <a className="button" href={DOWNLOAD_URL}>Скачать DMG · 182 МБ</a>
        </div>
        <ol>
          <li>Откройте DMG и перетащите PTT2me в «Программы».</li>
          <li>При первом запуске откройте приложение через контекстное меню «Открыть»: текущая сборка подписана ad-hoc и не нотарифицирована Apple.</li>
          <li>Разрешите микрофон, мониторинг ввода и универсальный доступ.</li>
        </ol>
      </section>

      <footer>
        <a className="brand" href="#top">PTT2me</a>
        <p>SHA-256 <code>1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53</code></p>
        <div><a href={CHECKSUM_URL}>Checksum</a><a href={RELEASE_URL}>Релиз 1.0.2</a><a href={REPOSITORY_URL}>GitHub</a></div>
      </footer>
    </main>
  );
}
```

- [ ] **Step 2: Implement the complete responsive visual system**

In `site/app/globals.css`, define `--ink: #f3efe8`, `--paper: #0b0b0c`,
`--muted: #a7a29b`, `--line: rgba(255,255,255,.14)`, and
`--accent: #ff4d3d`. Style the header, two-column hero, CSS Fn key, animated
18-bar waveform, transcript surface, highlights, steps, privacy panel,
permissions, installation panel, and footer. Add breakpoints at 900 px and
640 px, minimum 44 px controls, visible `:focus-visible` outlines, and:

```css
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after {
    animation-duration: 0.01ms !important;
    animation-iteration-count: 1 !important;
    scroll-behavior: auto !important;
  }
}
```

- [ ] **Step 3: Replace starter metadata and locale**

Set `lang="ru"` in `site/app/layout.tsx`. Keep the bundled Geist fonts, import
`headers` from `next/headers`, and use:

```tsx
export async function generateMetadata(): Promise<Metadata> {
  const requestHeaders = await headers();
  const host = requestHeaders.get("host") ?? "localhost";
  const protocol = host.startsWith("localhost") ? "http" : "https";

  return {
    metadataBase: new URL(`${protocol}://${host}`),
    title: "PTT2me — локальная диктовка для macOS",
    description:
      "Удерживайте Fn, говорите по-русски и вставляйте распознанный текст в любое приложение. Полностью локально на Apple Silicon.",
    icons: { icon: "/favicon.svg", shortcut: "/favicon.svg" },
  };
}
```

- [ ] **Step 4: Replace the starter favicon**

Create a simple favicon with a near-black rounded square and a centered red
circle. This is a browser icon, not a page illustration.

- [ ] **Step 5: Run the contract and quality checks**

Run:

```bash
npm test
npm run lint
```

Expected: the production build succeeds, both Node tests pass, and ESLint exits
with status 0.

- [ ] **Step 6: Commit the complete local site**

Run:

```bash
git add site
git commit -m "feat: add PTT2me product site"
```

---

### Task 3: Create the social card, publish the validated source, and verify deployment

**Files:**
- Create: `site/public/og.png`
- Modify: `site/app/layout.tsx`
- Modify: `site/.openai/hosting.json`

**Interfaces:**
- Consumes: the final headline, accent palette, Fn key, waveform motif, and successful Task 2 build
- Produces: one saved Sites version and one successful private deployment URL

- [ ] **Step 1: Generate and inspect exactly one social card**

Generate a 1200×630 landscape card with the exact text:
`PTT2me` and `Говорите — текст уже там.` Use the finished site's near-black
background, warm red accent, oversized type, Fn key, and waveform motif.
Inspect the returned image; retry once only if either text string is missing,
incorrect, or invented. Save the accepted image as `site/public/og.png`.

- [ ] **Step 2: Add host-derived social metadata**

Extend the existing host-derived metadata in `site/app/layout.tsx` with the
social fields:

```tsx
export async function generateMetadata(): Promise<Metadata> {
  const requestHeaders = await headers();
  const host = requestHeaders.get("host") ?? "localhost";
  const protocol = host.startsWith("localhost") ? "http" : "https";
  const origin = `${protocol}://${host}`;

  return {
    metadataBase: new URL(origin),
    title: "PTT2me — локальная диктовка для macOS",
    description:
      "Удерживайте Fn, говорите по-русски и вставляйте распознанный текст в любое приложение. Полностью локально на Apple Silicon.",
    openGraph: {
      type: "website",
      locale: "ru_RU",
      title: "PTT2me — локальная диктовка для macOS",
      description: "Говорите — текст уже там. Полностью локальная диктовка на вашем Mac.",
      images: [{ url: "/og.png", width: 1200, height: 630, alt: "PTT2me — Говорите, текст уже там" }],
    },
    twitter: {
      card: "summary_large_image",
      title: "PTT2me — локальная диктовка для macOS",
      description: "Говорите — текст уже там. Полностью локальная диктовка на вашем Mac.",
      images: ["/og.png"],
    },
    icons: { icon: "/favicon.svg", shortcut: "/favicon.svg" },
  };
}
```

- [ ] **Step 3: Rebuild after the final source change**

Run:

```bash
npm test
```

Expected: the build succeeds and both rendered-HTML tests pass.

- [ ] **Step 4: Create or reuse the Sites project**

Read `site/.openai/hosting.json`. If it has no `project_id`, call Sites
`create_site` exactly once and persist only the returned `project_id` alongside
the existing null `d1` and `r2` bindings. If it already has a `project_id`,
reuse it and obtain a fresh source write credential only if needed.

- [ ] **Step 5: Push the exact validated source**

Commit the final social metadata and hosting file, push the exact branch head
with the Sites write credential passed only as an HTTP authorization header,
and record the pushed branch-head SHA. Do not store the credential in a remote
URL or Git configuration.

- [ ] **Step 6: Package and save one version**

Run:

```bash
/Users/andrey/.codex/plugins/cache/openai-bundled/sites/0.1.31/scripts/package-site.sh \
  /Users/andrey/dev/ptt2me/site \
  /private/tmp/ptt2me-site.tar.gz
```

Call Sites `save_site_version` with the pushed SHA and this archive.

- [ ] **Step 7: Deploy privately and wait for completion**

Call `deploy_private_site_version`, then poll `get_deployment_status` until it
returns `status: "succeeded"` or a terminal failure. On success, open the exact
deployed URL once in the Codex browser and return it as the primary deliverable.

- [ ] **Step 8: Stop the retained development server**

Terminate the `npm run dev` process only after publishing finishes.
