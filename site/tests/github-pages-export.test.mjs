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
  assert.match(
    rewritten,
    /src="\/PTT2me\/_next\/static\/chunks\/site\.js"/,
  );
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
    assert.match(html, /Preview 1\.1\.1/);
    assert.match(html, /\/PTT2me\/_next\/static\//);
    assert.doesNotMatch(html, /http:\/\/localhost/);
    assert.doesNotMatch(html, /\/\.vinext\/fonts\//);
    assert.doesNotMatch(html, /\/(?:private|Users|home)\/[^"'()\s]+/);

    await access(join(outputDirectory, ".nojekyll"));
    await access(join(outputDirectory, "_next", "static", "chunks"));
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
