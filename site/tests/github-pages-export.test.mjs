import assert from "node:assert/strict";
import { access, mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  exportGitHubPages,
  rewritePageHtml,
} from "../scripts/export-github-pages.mjs";

const projectRoot = new URL("../", import.meta.url);

test("rewrites local site assets for the GitHub Pages project path", () => {
  const html = [
    '<link rel="stylesheet" href="/assets/site.css">',
    "<style>@font-face{src:url(/assets/font.woff2)}</style>",
    '<script src="/_next/static/chunks/site.js"></script>',
    "<style>@font-face{src:url(/private/build/site/.vinext/fonts/geist/font.woff2)}</style>",
    '<link rel="preload" href="https://cdn.example/.vinext/fonts/external.woff2">',
    '<link rel="icon" href="http://localhost/favicon.svg">',
    '<meta property="og:image" content="http://localhost/og.png">',
    '<a href="#how">Как работает</a>',
    '<a href="https://github.com/Torin2023/PTT2me">GitHub</a>',
  ].join("");

  const rewritten = rewritePageHtml(html, {
    basePath: "/PTT2me",
    origin: "https://torin2023.github.io",
  });

  assert.match(rewritten, /href="\/PTT2me\/assets\/site\.css"/);
  assert.match(rewritten, /url\(\/PTT2me\/assets\/font\.woff2\)/);
  assert.doesNotMatch(rewritten, /<script\b/i);
  assert.match(
    rewritten,
    /url\(\/PTT2me\/_next\/static\/_vinext_fonts\/geist\/font\.woff2\)/,
  );
  assert.doesNotMatch(rewritten, /\/private\/build\/site\/\.vinext\/fonts/);
  assert.match(
    rewritten,
    /href="https:\/\/cdn\.example\/\.vinext\/fonts\/external\.woff2"/,
  );
  assert.match(
    rewritten,
    /href="https:\/\/torin2023\.github\.io\/PTT2me\/favicon\.svg"/,
  );
  assert.match(
    rewritten,
    /content="https:\/\/torin2023\.github\.io\/PTT2me\/og\.png"/,
  );
  assert.match(rewritten, /href="#how"/);
  assert.match(
    rewritten,
    /href="https:\/\/github\.com\/Torin2023\/PTT2me"/,
  );
});

test("removes client JavaScript from the static GitHub Pages HTML", () => {
  const html = [
    '<link rel="stylesheet" href="/_next/static/css/site.css">',
    '<link rel="modulepreload" href="/_next/static/chunks/site.js">',
    '<link rel="preload" as="script" href="/_next/static/chunks/runtime.js">',
    '<script src="/_next/static/chunks/runtime.js" type="module"></script>',
    '<script>self.__next_f.push([1,"payload"])</script>',
    '<main><h1>Говорите — текст уже там.</h1></main>',
  ].join("");

  const rewritten = rewritePageHtml(html, {
    basePath: "/PTT2me",
    origin: "https://torin2023.github.io",
  });

  assert.doesNotMatch(rewritten, /<script\b/i);
  assert.doesNotMatch(rewritten, /rel="modulepreload"/i);
  assert.doesNotMatch(rewritten, /rel="preload"[^>]*as="script"/i);
  assert.match(
    rewritten,
    /href="\/PTT2me\/_next\/static\/css\/site\.css"/,
  );
  assert.match(rewritten, /<main><h1>Говорите — текст уже там\.<\/h1><\/main>/);
});

test("moves streamed metadata into head and removes streaming scaffolding", () => {
  const html = [
    "<!DOCTYPE html><html><head>",
    '<meta charSet="utf-8"/>',
    "</head><body><main>PTT2me</main>",
    '<div hidden=""><!--$?--><template id="B:0"></template><!--/$--></div>',
    '<div hidden id="S:0"><div hidden="">',
    "<title>PTT2me — локальная диктовка для macOS</title>",
    '<meta name="description" content="Локальная диктовка.">',
    '<meta property="og:title" content="PTT2me">',
    '<meta name="twitter:card" content="summary_large_image">',
    '<link data-vinext-streamed-icon="/:test:0" rel="icon" href="/favicon.svg">',
    "<script>document.head.append(document.querySelector('link[rel=icon]'))</script>",
    "</div></div></body></html>",
  ].join("");

  const rewritten = rewritePageHtml(html, {
    basePath: "/PTT2me",
    origin: "https://torin2023.github.io",
  });
  const head = rewritten.match(/<head>([\s\S]*?)<\/head>/i)?.[1] ?? "";

  assert.match(head, /<title>PTT2me — локальная диктовка для macOS<\/title>/);
  assert.match(head, /<meta name="description" content="Локальная диктовка\.">/);
  assert.match(head, /<meta property="og:title" content="PTT2me">/);
  assert.match(head, /<meta name="twitter:card" content="summary_large_image">/);
  assert.match(head, /<link rel="icon" href="\/PTT2me\/favicon\.svg">/);
  assert.doesNotMatch(rewritten, /\bid="S:\d+"/i);
  assert.doesNotMatch(rewritten, /\bid="B:\d+"/i);
  assert.doesNotMatch(rewritten, /data-vinext-streamed-icon/i);
  assert.doesNotMatch(rewritten, /<!--\$\??-->|<!--\/\$-->/);
});

test("exports a deployable static site without the server bundle", async () => {
  const outputDirectory = await mkdtemp(join(tmpdir(), "ptt2me-pages-test-"));

  try {
    await exportGitHubPages({
      projectRoot,
      outputDirectory,
      basePath: "/PTT2me",
      origin: "https://torin2023.github.io",
    });

    const html = await readFile(join(outputDirectory, "index.html"), "utf8");
    assert.match(html, /Preview 1\.2\.1/);
    assert.match(html, /\/PTT2me\/_next\/static\//);
    assert.doesNotMatch(html, /<script\b/i);
    assert.doesNotMatch(html, /rel="modulepreload"/i);
    assert.doesNotMatch(html, /rel="preload"[^>]*as="script"/i);
    assert.doesNotMatch(html, /http:\/\/localhost/);
    assert.doesNotMatch(html, /\/\.vinext\/fonts\//);
    assert.doesNotMatch(html, /\/(?:private|Users|home)\/[^"'()\s]+/);
    const head = html.match(/<head>([\s\S]*?)<\/head>/i)?.[1] ?? "";
    assert.match(head, /<title>PTT2me — локальная диктовка для macOS<\/title>/);
    assert.match(head, /<meta name="description"/);
    assert.match(head, /<meta property="og:title"/);
    assert.match(head, /<meta name="twitter:card"/);
    assert.match(head, /<link[^>]*rel="icon"/);
    assert.doesNotMatch(html, /\bid="S:\d+"/i);
    assert.doesNotMatch(html, /\bid="B:\d+"/i);
    assert.doesNotMatch(html, /data-vinext-streamed-icon/i);

    await access(join(outputDirectory, ".nojekyll"));
    await assert.rejects(
      access(join(outputDirectory, "_next", "static", "chunks")),
    );
    await access(
      join(outputDirectory, "_next", "static", "_vinext_fonts"),
    );
    await access(join(outputDirectory, "favicon.svg"));
    await assert.rejects(access(join(outputDirectory, "server")));
    await assert.rejects(access(join(outputDirectory, ".vite")));
    await assert.rejects(access(join(outputDirectory, "_headers")));
    await assert.rejects(access(join(outputDirectory, ".assetsignore")));
  } finally {
    await rm(outputDirectory, { recursive: true, force: true });
  }
});

test(
  "build publishes the browser entry under the Next static asset root",
  { skip: process.env.GITHUB_PAGES_BASE_PATH !== "/PTT2me" },
  async () => {
    const assetsDirectory = new URL(
      "../dist/client/_next/static/chunks/",
      import.meta.url,
    );
    const scripts = (await readdir(assetsDirectory)).filter((file) =>
      file.endsWith(".js"),
    );
    assert.ok(scripts.length >= 4);

    const entryManifest = JSON.parse(
      await readFile(
        new URL("../dist/client/vinext-client-entry-manifest.json", import.meta.url),
        "utf8",
      ),
    );

    assert.match(
      entryManifest.appBrowserEntry,
      /^_next\/static\/chunks\/[^/]+\.js$/,
    );
  },
);
