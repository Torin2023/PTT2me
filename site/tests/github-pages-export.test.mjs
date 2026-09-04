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
    assert.match(html, /\/PTT2me\/assets\//);
    assert.doesNotMatch(html, /http:\/\/localhost/);

    await access(join(outputDirectory, ".nojekyll"));
    await access(join(outputDirectory, "assets"));
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
  "build scopes dynamic preload dependencies to the GitHub Pages project",
  { skip: process.env.GITHUB_PAGES_BASE_PATH !== "/PTT2me" },
  async () => {
    const assetsDirectory = new URL("../dist/client/assets/", import.meta.url);
    const scripts = (await readdir(assetsDirectory)).filter((file) =>
      file.endsWith(".js"),
    );
    const clientCode = (
      await Promise.all(
        scripts.map((file) => readFile(new URL(file, assetsDirectory), "utf8")),
      )
    ).join("\n");

    assert.match(clientCode, /return`\/PTT2me\/`\+e/);
    assert.doesNotMatch(clientCode, /return`\/`\+e/);
  },
);
