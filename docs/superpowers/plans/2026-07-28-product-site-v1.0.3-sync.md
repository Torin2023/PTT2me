# PTT2me Product Site v1.0.3 Sync Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Update and republish the existing public PTT2me landing page so its release data and product claims match GitHub prerelease v1.0.3 and `origin/main`.

**Architecture:** Preserve the existing single-route vinext site, visual system, social card, and Sites project. Merge current `origin/main` into the isolated product-site branch, update the rendered-HTML contract first, make one focused content patch across the page and metadata, then package and publish the exact validated commit as the next version of the existing public site.

**Tech Stack:** Git, React 19, vinext, TypeScript, CSS, Node test runner, Cloudflare Workers-compatible ESM, Sites hosting

## Global Constraints

- The primary headline remains «Говорите — текст уже там».
- The primary CTA downloads `PTT2me-1.0.3-macos-arm64.dmg`.
- The release is visibly labeled `Preview 1.0.3`.
- The v1.0.3 DMG URL is `https://github.com/Torin2023/PTT2me/releases/download/v1.0.3/PTT2me-1.0.3-macos-arm64.dmg`.
- The v1.0.3 release URL is `https://github.com/Torin2023/PTT2me/releases/tag/v1.0.3`.
- The displayed size remains approximately `182 МБ`.
- The SHA-256 is `513ddace2ca4b8d8bc9f9e5da099cc238ea6476f559d376605b80c76a267e2f4`.
- The GitHub release has no separate `.sha256` asset; do not link to one.
- Compatibility remains Apple Silicon and macOS 13 Ventura or newer.
- Keep the existing route, visual system, responsive behavior, accessibility, social card, package manager, lockfile, and Sites project ID.
- Do not add a changelog section, analytics, forms, accounts, persistence, or runtime GitHub requests.

---

### Task 1: Bring the product-site branch up to date with v1.0.3

**Files:**
- Merge from: `origin/main`
- Preserve: `site/**`
- Preserve: `docs/superpowers/specs/2026-07-27-product-site-design.md`
- Preserve: `docs/superpowers/plans/2026-07-27-product-site.md`

**Interfaces:**
- Consumes: fetched `origin/main` at tagged release `v1.0.3`
- Produces: branch `codex/product-site` containing current product code plus the existing site

- [ ] **Step 1: Confirm the branch and clean worktree**

Run:

```bash
git branch --show-current
git status --short
git rev-parse origin/main
git describe --exact-match --tags origin/main
```

Expected: branch `codex/product-site`, clean status, commit
`11df0f4...`, and tag `v1.0.3`.

- [ ] **Step 2: Merge current product history without rewriting the site branch**

Run:

```bash
git merge --no-edit origin/main
```

Expected: a merge commit, no unresolved conflicts, and `site/` remains present.

- [ ] **Step 3: Verify the existing site still builds before content changes**

Run from `site/`:

```bash
npm test
```

Expected: the production build succeeds and both existing rendered-HTML tests
pass against the unchanged v1.0.2 site.

---

### Task 2: Lock the v1.0.3 rendered-content contract

**Files:**
- Modify: `site/tests/rendered-html.test.mjs`

**Interfaces:**
- Consumes: the existing `render()` helper and server-rendered `/` route
- Produces: a failing test contract for v1.0.3 release data and behavior claims

- [ ] **Step 1: Replace the release assertions**

In `site/tests/rendered-html.test.mjs`, keep the existing language, title,
headline, privacy, permission, and starter-removal assertions. Replace the old
release assertions and add the new behavior assertions:

```js
assert.match(html, /Preview 1\.0\.3/);
assert.match(
  html,
  /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/download\/v1\.0\.3\/PTT2me-1\.0\.3-macos-arm64\.dmg/,
);
assert.match(
  html,
  /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/tag\/v1\.0\.3/,
);
assert.match(
  html,
  /513ddace2ca4b8d8bc9f9e5da099cc238ea6476f559d376605b80c76a267e2f4/,
);
assert.match(html, /поле, где находится курсор/);
assert.match(html, /короткое нажатие[^<]*системное действие/i);
assert.doesNotMatch(html, /PTT2me-1\.0\.2-macos-arm64\.dmg/);
assert.doesNotMatch(
  html,
  /1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53/,
);
assert.doesNotMatch(html, /releases\/download\/v1\.0\.3\/[^"']+\.sha256/);
```

- [ ] **Step 2: Run the contract and verify it fails against the old page**

Run:

```bash
npm test
```

Expected: FAIL because `Preview 1.0.3`, the new URLs, checksum, and new
behavior copy are absent.

---

### Task 3: Update page copy, release constants, and metadata

**Files:**
- Modify: `site/app/page.tsx`
- Modify: `site/app/globals.css`
- Modify: `site/app/layout.tsx`
- Test: `site/tests/rendered-html.test.mjs`

**Interfaces:**
- Consumes: the Task 2 rendered-content contract
- Produces: a server-rendered v1.0.3 landing page with unchanged route and visual system

- [ ] **Step 1: Update release constants**

At the top of `site/app/page.tsx`, use:

```tsx
const DOWNLOAD_URL =
  "https://github.com/Torin2023/PTT2me/releases/download/v1.0.3/PTT2me-1.0.3-macos-arm64.dmg";
const REPOSITORY_URL = "https://github.com/Torin2023/PTT2me";
const RELEASE_URL = `${REPOSITORY_URL}/releases/tag/v1.0.3`;
```

Remove `CHECKSUM_URL`; the v1.0.3 release has no checksum asset.

- [ ] **Step 2: Add the visible preview marker and revise hero copy**

Inside the hero eyebrow, add:

```tsx
<span className="preview-badge">Preview 1.0.3</span>
```

Use this hero description:

```tsx
<p className="lede">
  Удерживайте Fn, говорите по-русски и отпускайте. PTT2me распознает
  речь прямо на вашем Mac и вставит результат в поле, где находится курсор
  к моменту завершения распознавания.
</p>
```

Change the primary CTA to `Скачать PTT2me 1.0.3`.

- [ ] **Step 3: Update the three value highlights**

Keep `Без облака`. Change the other two cards to:

```tsx
<article>
  <span>02</span>
  <h2>Одна клавиша</h2>
  <p>Короткое нажатие сохраняет системное действие, удержание запускает диктовку.</p>
</article>
<article>
  <span>03</span>
  <h2>Точно в курсор</h2>
  <p>Результат получает поле, которое активно после распознавания.</p>
</article>
```

- [ ] **Step 4: Update workflow, privacy, and permission details**

Append this sentence to the workflow fine print:

```text
Короткое нажатие возвращается macOS и сохраняет настроенное системное действие.
```

Replace the main privacy paragraph with:

```tsx
<p>
  PTT2me сначала вставляет текст напрямую через Accessibility или Unicode —
  эти способы не меняют буфер обмена. Если приложению нужен совместимый
  Command-V fallback, PTT2me восстанавливает все прежние элементы и форматы
  буфера и никогда не перезаписывает более новые изменения.
</p>
```

After the permissions cards, add:

```tsx
<p className="permission-note">
  Если разрешения не хватает, «Открыть настройки…» повторно открывает
  соответствующий раздел «Конфиденциальность и безопасность».
</p>
```

- [ ] **Step 5: Update installation and footer release details**

Use `PTT2me Preview 1.0.3` in the installation eyebrow, keep the existing
ad-hoc/notarization disclosure, and update the button text to
`Скачать Preview DMG · 182 МБ`.

Replace the footer checksum link with plain release information:

```tsx
<div>
  <a href={RELEASE_URL}>Preview-релиз 1.0.3</a>
  <a href={REPOSITORY_URL}>GitHub</a>
</div>
```

Keep the new SHA-256 displayed in the existing `<code>` element.

- [ ] **Step 6: Style only the new factual elements**

Add these rules to `site/app/globals.css`:

```css
.preview-badge {
  display: inline-flex;
  align-items: center;
  min-height: 24px;
  margin-left: 4px;
  padding: 3px 8px;
  border: 1px solid rgba(255, 77, 61, 0.42);
  border-radius: 99px;
  background: rgba(255, 77, 61, 0.1);
  color: var(--accent);
  font-size: 9px;
  letter-spacing: 0.08em;
}

.permission-note {
  max-width: 760px;
  margin: 28px 0 0 auto;
  color: var(--dim);
  font-family: var(--font-geist-mono), monospace;
  font-size: 10px;
  line-height: 1.7;
}
```

Allow `.eyebrow` to wrap so the badge remains usable on narrow screens.

- [ ] **Step 7: Update metadata description**

In `site/app/layout.tsx`, replace both the main metadata description and the
Open Graph/X description with copy that says:

```text
Удерживайте Fn, говорите по-русски и вставляйте результат в поле, где находится курсор. Полностью локально на Apple Silicon.
```

Keep the existing title and `/og.png`.

- [ ] **Step 8: Run all site checks**

Run:

```bash
npm test
npm run lint
```

Expected: production build succeeds, both rendered-HTML tests pass, and ESLint
reports no errors.

- [ ] **Step 9: Commit the validated content update**

Run:

```bash
git add site/app/page.tsx site/app/globals.css site/app/layout.tsx site/tests/rendered-html.test.mjs
git commit -m "feat: sync product site with v1.0.3"
```

---

### Task 4: Save and publish the exact validated site version

**Files:**
- Reuse: `site/.openai/hosting.json`
- Reuse build output: `site/dist/**`
- Create temporary archive: `/private/tmp/ptt2me-site-v1.0.3.tar.gz`

**Interfaces:**
- Consumes: clean validated branch head and existing Sites project ID
- Produces: the next saved Sites version and successful public production deployment

- [ ] **Step 1: Confirm the exact source state**

Run:

```bash
git status --short
git rev-parse HEAD
sed -n '1,80p' site/.openai/hosting.json
```

Expected: clean status and the existing opaque `project_id`.

- [ ] **Step 2: Obtain a fresh Sites source credential**

Call `create_source_repository_write_credential` with the exact `project_id`
from `site/.openai/hosting.json`. Retain its branch, remote URL, token, and
expiration only for this publication flow.

- [ ] **Step 3: Push the exact validated branch head**

Push `HEAD` to the credential's source branch using a per-command HTTP
authorization header. Do not persist the token in a remote or Git
configuration.

- [ ] **Step 4: Package the validated build**

Run:

```bash
/Users/andrey/.codex/plugins/cache/openai-bundled/sites/0.1.31/scripts/package-site.sh \
  /Users/andrey/dev/ptt2me/.worktrees/product-site/site \
  /private/tmp/ptt2me-site-v1.0.3.tar.gz
```

Expected: the helper validates `dist/server/index.js`, copies current hosting
metadata, and creates the archive.

- [ ] **Step 5: Save one Sites version**

Call `save_site_version` with the exact pushed commit SHA and
`/private/tmp/ptt2me-site-v1.0.3.tar.gz`.

- [ ] **Step 6: Confirm public deployment and publish**

Read the site's access configuration. It should be `public`. Ask for explicit
approval to deploy the saved version to the existing public access, then call
`deploy_site_version`.

- [ ] **Step 7: Poll to terminal status**

Call `get_deployment_status` until it returns `succeeded` or a terminal
failure. On success, verify the returned URL is the existing PTT2me production
site and report the new user-facing version number.
