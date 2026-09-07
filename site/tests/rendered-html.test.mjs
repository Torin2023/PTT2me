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

test("renders the current PTT2me v1.3.0 product contract", async () => {
  const response = await render();
  assert.equal(response.status, 200);

  const html = await response.text();
  assert.match(html, /<html[^>]*lang="ru"/i);
  assert.match(
    html,
    /<title>PTT2me — локальная диктовка для macOS<\/title>/i,
  );
  assert.match(
    html,
    /<meta name="description" content="PTT2me 1\.3\.0:[^"]*локальная диктовка на Apple Silicon/,
  );
  assert.match(html, /Говорите —[\s\S]*текст уже там/);
  assert.match(html, /Без облака/);
  assert.match(html, /Микрофон/);
  assert.match(html, /Preview 1\.3\.0/);
  assert.match(html, /class="preview-badge">Preview 1\.3\.0<\/span>/);
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/download\/v1\.3\.0\/PTT2me-1\.3\.0-full-macos-arm64\.dmg/,
  );
  assert.match(
    html,
    /https:\/\/github\.com\/Torin2023\/PTT2me\/releases\/tag\/v1\.3\.0/,
  );
  assert.match(
    html,
    /36c374264e924537faa0d3fbb74dd5abf98ab808d2d3aa7f27a970e79566b363/,
  );
  assert.match(html, /поле, где находится курсор/);
  assert.match(html, /Назначьте удобную клавишу/);
  assert.match(html, /250, 500 и 750 мс/);
  assert.match(html, /по умолчанию — 500 мс/);
  assert.match(html, /Запись начинается сразу/);
  assert.match(html, /нажатие оказалось коротким/);
  assert.match(html, /Пробел в конце/);
  assert.match(html, /Command-V/);
  assert.match(html, /браузер/i);
  assert.match(html, /contenteditable/);
  assert.match(html, /Вставка в ChatGPT в этой версии ещё[\s\S]*не подтверждена ручной проверкой/);
  assert.doesNotMatch(html, /PTT2me-1\.0\.[234]-macos-arm64\.dmg/);
  assert.doesNotMatch(
    html,
    /513ddace2ca4b8d8bc9f9e5da099cc238ea6476f559d376605b80c76a267e2f4/,
  );
  assert.doesNotMatch(
    html,
    /releases\/download\/v1\.3\.0\/[^"']+\.sha256/,
  );
  assert.doesNotMatch(
    html,
    /codex-preview|react-loading-skeleton|Your site is taking shape/i,
  );
});

test("documents the published 1.3.0 recovery update flow", async () => {
  const response = await render();
  assert.equal(response.status, 200);

  const html = await response.text();
  assert.match(html, /PTT2me 1\.3\.0 опубликована/);
  assert.match(html, /Версиям 1\.1\.0 и 1\.1\.1 нужен один ручной переход/);
  assert.match(html, /прежний updater не может скачать сборку с собственным исправлением/);
  assert.match(html, /через 60 секунд после запуска/);
  assert.match(html, /не чаще одного раза в 24 часа/);
  assert.match(html, /Проверить обновления…/);
  assert.match(html, /Full DMG/);
  assert.match(html, /Update DMG без модели/);
  assert.match(html, /модель проверена/);
  assert.match(
    html,
    /~\/Library\/Application Support\/PTT2me\/models\/gigaam-v3-rnnt-v1\//,
  );
  assert.match(html, /Скачать обновление/);
  assert.match(html, /GitHub Release/);
  assert.match(html, /Открыть DMG и выйти…/);
  assert.match(html, /замените PTT2me\.app через Finder/);
  assert.match(html, /Открыть всё равно/);
  assert.match(html, /сначала проверяет и подготавливает модель/);
  assert.match(
    html,
    /Универсального доступа, Мониторинга ввода и Микрофона/,
  );
  assert.match(html, /Повторить сброс разрешений/);
  assert.match(html, /выдайте три разрешения заново/);
  assert.match(html, /Полное удаление/);
  assert.match(html, /~\/Library\/Caches\/com\.ptt2me\.app\//);
  assert.match(html, /~\/Library\/Preferences\/com\.ptt2me\.app\.plist/);

  assert.doesNotMatch(html, /tccutil/i);
  assert.doesNotMatch(html, /application_update/);
  assert.match(html, /Скачать PTT2me 1\.3\.0/);
  assert.match(html, /Версии начиная с 1\.1\.2 могут загрузить 1\.3\.0/);
  assert.match(html, /Полный набор ручных проверок[\s\S]*не завершён/);
  assert.match(html, /без[\s\S]*нотариализации Apple/);
  assert.match(html, /175,1 МиБ/);
  assert.match(html, /меньше на 49%/);
  assert.doesNotMatch(html, /Скачать PTT2me 1\.1\.1/);
});

test("removes the disposable starter preview", async () => {
  await assert.rejects(access(new URL("app/_sites-preview", root)));
});
