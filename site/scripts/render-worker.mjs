import { readFile, readdir } from "node:fs/promises";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { Miniflare } from "miniflare";

// Render the production worker in workerd, not Node: vinext uses native
// cloudflare: modules. No Cloudflare account, remote bindings or deployment.
export async function renderWorker(projectRoot, path = "/") {
  const root = projectRoot instanceof URL ? fileURLToPath(projectRoot) : projectRoot;
  const directory = resolve(root, "dist/server");
  const config = JSON.parse(await readFile(resolve(directory, "wrangler.json"), "utf8"));
  const paths = await readdir(directory, { recursive: true });
  const modules = Object.fromEntries(await Promise.all(paths
    .filter((path) => path.endsWith(".js"))
    .map(async (path) => [path.replaceAll("\\", "/"), {
      type: "esm",
      contents: await readFile(resolve(directory, path), "utf8"),
    }])));
  const runtime = new Miniflare({
    telemetry: { enabled: false },
    enableDevRegistry: false,
    workers: [{
      config: {
        name: "ptt2me-static-render",
        compatibilityDate: config.compatibility_date,
        compatibilityFlags: config.compatibility_flags,
        manifest: { mainModule: config.main, modulesRoot: directory, modules },
      },
    }],
  });
  try {
    const response = await runtime.dispatchFetch(`http://localhost${path}`, {
      headers: { accept: "text/html" },
    });
    // Consume the stream before disposing the worker process.
    return new Response(await response.arrayBuffer(), {
      status: response.status,
      statusText: response.statusText,
      headers: response.headers,
    });
  } finally {
    await runtime.dispose();
  }
}
