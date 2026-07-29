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

test("renders the current PTT2me v1.0.4 product contract", async () => {
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
  assert.match(html, /Preview 1\.0\.4/);
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/download\/v1\.0\.4\/PTT2me-1\.0\.4-macos-arm64\.dmg/,
  );
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/tag\/v1\.0\.4/,
  );
  assert.match(
    html,
    /64b54978627ba1f2f1c466222fd2a984b2fcd8623afb66d1714e12da13be83bf/,
  );
  assert.match(html, /поле, где находится курсор/);
  assert.match(html, /Назначьте удобную клавишу/);
  assert.match(html, /250, 500 и 750 мс/);
  assert.match(html, /по умолчанию — 500 мс/);
  assert.match(html, /Запись начинается сразу/);
  assert.match(html, /нажатие оказалось коротким/);
  assert.doesNotMatch(html, /PTT2me-1\.0\.[23]-macos-arm64\.dmg/);
  assert.doesNotMatch(
    html,
    /513ddace2ca4b8d8bc9f9e5da099cc238ea6476f559d376605b80c76a267e2f4/,
  );
  assert.doesNotMatch(
    html,
    /releases\/download\/v1\.0\.4\/[^"']+\.sha256/,
  );
  assert.doesNotMatch(
    html,
    /codex-preview|react-loading-skeleton|Your site is taking shape/i,
  );
});

test("removes the disposable starter preview", async () => {
  await assert.rejects(access(new URL("app/_sites-preview", root)));
});
