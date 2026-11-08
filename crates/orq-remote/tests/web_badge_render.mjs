// Renders every badge kind, plus an unknown and an omitted kind, to static markup and prints JSON.
// Usage: node web_badge_render.mjs <kinds as JSON array>
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "../web-src/package.json"));
const { build } = require("esbuild");

const dir = mkdtempSync(join(tmpdir(), "orq-badge-"));
try {
  const outfile = join(dir, "badge.cjs");
  const entry = `
    import { createElement } from "react";
    import { renderToStaticMarkup } from "react-dom/server";
    import { Badge, BADGES } from "./src/badge.jsx";
    export const kinds = Object.keys(BADGES);
    export const render = (kind) => renderToStaticMarkup(createElement(Badge, kind === null ? {} : { kind }));
  `;
  await build({ stdin: { contents: entry, resolveDir: join(here, "../web-src"), loader: "jsx" }, outfile, bundle: true, format: "cjs", platform: "node", jsx: "automatic", logLevel: "silent" });
  const { kinds, render } = require(outfile);
  const out = { kinds, html: {} };
  for (const kind of JSON.parse(process.argv[2])) {
    try {
      out.html[String(kind)] = { ok: true, html: render(kind) };
    } catch (error) {
      out.html[String(kind)] = { ok: false, error: String(error) };
    }
  }
  console.log(JSON.stringify(out));
} finally {
  rmSync(dir, { recursive: true, force: true });
}
