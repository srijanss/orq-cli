// Renders the given kinds as badges and as status pills, plus every pill kind, to static markup and prints JSON.
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
    import { Badge, BADGES, PILLS, StatusPill } from "./src/badge.jsx";
    export const kinds = Object.keys(BADGES);
    export const pillKinds = Object.keys(PILLS);
    export const render = (kind) => renderToStaticMarkup(createElement(Badge, kind === null ? {} : { kind }));
    export const renderPill = (kind) => renderToStaticMarkup(createElement(StatusPill, kind === null ? {} : { kind }));
  `;
  await build({ stdin: { contents: entry, resolveDir: join(here, "../web-src"), loader: "jsx" }, outfile, bundle: true, format: "cjs", platform: "node", jsx: "automatic", logLevel: "silent" });
  const { kinds, pillKinds, render, renderPill } = require(outfile);
  // html: each kind as a Badge; pills: each kind as a StatusPill, plus every kind the pill table knows.
  const out = { kinds, pillKinds, html: {}, pills: {} };
  const attempt = (draw, kind) => {
    try {
      return { ok: true, html: draw(kind) };
    } catch (error) {
      return { ok: false, error: String(error) };
    }
  };
  const asked = JSON.parse(process.argv[2]);
  for (const kind of asked) out.html[String(kind)] = attempt(render, kind);
  for (const kind of [...asked, ...pillKinds]) out.pills[String(kind)] = attempt(renderPill, kind);
  console.log(JSON.stringify(out));
} finally {
  rmSync(dir, { recursive: true, force: true });
}
