import { access, cp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { fileURLToPath, pathToFileURL } from "node:url";
import { resolve } from "node:path";

const PUBLIC_FILES = [
  "assets/",
  "favicon.svg",
  "file.svg",
  "globe.svg",
  "og.png",
  "window.svg",
];

function normalizeBasePath(basePath) {
  if (basePath === "") {
    return "";
  }
  if (!basePath.startsWith("/")) {
    throw new Error("GitHub Pages base path must start with /");
  }
  return basePath.replace(/\/+$/, "");
}

export function rewritePageHtml(html, { basePath, origin }) {
  const normalizedBasePath = normalizeBasePath(basePath);
  const normalizedOrigin = origin.replace(/\/+$/, "");
  const publicBase = `${normalizedOrigin}${normalizedBasePath}`;
  let rewritten = html;

  for (const publicFile of PUBLIC_FILES) {
    for (const localOrigin of ["http://localhost/", "https://localhost/"]) {
      rewritten = rewritten.replaceAll(
        `${localOrigin}${publicFile}`,
        `${publicBase}/${publicFile}`,
      );
    }

    const escapedFile = publicFile.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    const rootPath = new RegExp(
      `(["'(<>=:])/${escapedFile}`,
      "g",
    );
    rewritten = rewritten.replace(
      rootPath,
      `$1${normalizedBasePath}/${publicFile}`,
    );
  }

  return rewritten;
}

export async function exportGitHubPages({
  projectRoot,
  outputDirectory,
  basePath,
  origin,
}) {
  const root = fileURLToPath(projectRoot);
  const clientDirectory = resolve(root, "dist", "client");
  const workerPath = resolve(root, "dist", "server", "index.js");

  await access(clientDirectory);
  await access(workerPath);
  await rm(outputDirectory, { recursive: true, force: true });
  await mkdir(outputDirectory, { recursive: true });
  await cp(clientDirectory, outputDirectory, { recursive: true });

  await Promise.all([
    rm(resolve(outputDirectory, ".vite"), { recursive: true, force: true }),
    rm(resolve(outputDirectory, "_headers"), { force: true }),
    rm(resolve(outputDirectory, ".assetsignore"), { force: true }),
  ]);

  const workerUrl = pathToFileURL(workerPath);
  workerUrl.searchParams.set("github-pages-export", `${process.pid}-${Date.now()}`);
  const { default: worker } = await import(workerUrl.href);
  const response = await worker.fetch(
    new Request("http://localhost/", {
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

  if (!response.ok) {
    throw new Error(`Site render failed with HTTP ${response.status}`);
  }

  const html = rewritePageHtml(await response.text(), { basePath, origin });
  await writeFile(resolve(outputDirectory, "index.html"), html);
  await writeFile(resolve(outputDirectory, ".nojekyll"), "");

  return {
    html: await readFile(resolve(outputDirectory, "index.html"), "utf8"),
    outputDirectory,
  };
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : "";
if (invokedPath === import.meta.url) {
  await exportGitHubPages({
    projectRoot: new URL("../", import.meta.url),
    outputDirectory: resolve("pages-dist"),
    basePath: process.env.GITHUB_PAGES_BASE_PATH ?? "/PTT2me",
    origin: process.env.GITHUB_PAGES_ORIGIN ?? "https://torin2023.github.io",
  });
}
