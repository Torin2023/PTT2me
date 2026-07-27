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
    {
      ASSETS: {
        fetch: async () => new Response("Not found", { status: 404 }),
      },
    },
    {
      waitUntil() {},
      passThroughOnException() {},
    },
  );
}

test("renders the PTT2me product page", async () => {
  const response = await render();
  assert.equal(response.status, 200);

  const html = await response.text();
  assert.match(html, /<html[^>]*lang="ru"/i);
  assert.match(
    html,
    /<title>PTT2me — локальная диктовка для macOS<\/title>/i,
  );
  assert.match(html, /Говорите —[\s\S]*текст уже там/);
  assert.match(html, /Без облака/);
  assert.match(html, /Микрофон/);
  assert.match(
    html,
    /1119711c9fee89218d816fb9eb4a03c138c790a51b3a0792970f0c6c17016f53/,
  );
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/download\/v1\.0\.2\/PTT2me-1\.0\.2-macos-arm64\.dmg/,
  );
  assert.doesNotMatch(
    html,
    /codex-preview|react-loading-skeleton|Your site is taking shape/i,
  );
});

test("removes the disposable starter preview", async () => {
  await assert.rejects(access(new URL("app/_sites-preview", root)));
});
