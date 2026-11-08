// Mounts the shell in jsdom and prints how it is put together (no steps, no layout).
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "../web-src/package.json"));
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");

const dom = new JSDOM('<!doctype html><html><body><div id="app"></div></body></html>', { url: "http://localhost/dashboard", pretendToBeVisual: true });
const { window } = dom;
for (const name of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "MouseEvent", "PopStateEvent"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? window : window[name], configurable: true });
}
globalThis.IS_REACT_ACT_ENVIRONMENT = false;

const dir = mkdtempSync(join(tmpdir(), "orq-structure-"));
try {
  const outfile = join(dir, "shell.cjs");
  const entry = `
    import { createElement } from "react";
    import { createRoot } from "react-dom/client";
    import { Shell } from "./src/shell.jsx";
    export const mount = (el) => createRoot(el).render(createElement(Shell, { projectId: "demo" }, createElement("p", null, "body")));
  `;
  await build({ stdin: { contents: entry, resolveDir: join(here, "../web-src"), loader: "jsx" }, outfile, bundle: true, format: "cjs", platform: "node", jsx: "automatic", logLevel: "silent" });
  require(outfile).mount(window.document.getElementById("app"));
  await new Promise((resolve) => setTimeout(resolve, 50));
  const doc = window.document;
  const side = doc.querySelector("#sidebar");
  const links = [...doc.querySelectorAll("nav a")];
  console.log(JSON.stringify({
    brand: side.querySelector("[data-brand]")?.textContent.trim() ?? null,
    railInSidebar: !!side.querySelector("[data-rail-button]"),
    themeInSidebar: !!side.querySelector("[data-theme-toggle]"),
    links: links.length,
    linksWithIcon: links.filter((a) => a.querySelector("svg")).length,
    iconsHidden: [...doc.querySelectorAll("svg")].every((s) => s.getAttribute("aria-hidden") === "true"),
  }));
  window.close();
} finally {
  rmSync(dir, { recursive: true, force: true });
}
