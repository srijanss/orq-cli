// Mounts the app shell in jsdom and plays steps against it, printing a snapshot after each.
// Usage: node web_shell_run.mjs '<{projectId, hash, steps}>'
// steps: {click: selector} | {press: "Escape"|"Tab", shift?: bool} | {hash: "#/..."}
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "../web-src/package.json"));
const { build } = require("esbuild");
const { JSDOM } = require("jsdom");
const scenario = JSON.parse(process.argv[2]);

const dom = new JSDOM('<!doctype html><html><body><div id="app"></div></body></html>', { url: "http://localhost/" + (scenario.hash || ""), pretendToBeVisual: true });
const { window } = dom;
for (const name of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "MouseEvent"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? window : window[name], configurable: true });
}
globalThis.IS_REACT_ACT_ENVIRONMENT = false;
// A phone-width query that a step can flip, since jsdom has no layout.
let phone = true;
const phoneListeners = new Set();
window.matchMedia = (query) => ({
  media: query,
  get matches() { return phone; },
  addEventListener: (_, listener) => phoneListeners.add(listener),
  removeEventListener: (_, listener) => phoneListeners.delete(listener),
});

const dir = mkdtempSync(join(tmpdir(), "orq-shell-"));
try {
  const outfile = join(dir, "shell.cjs");
  const entry = `
    import { createElement } from "react";
    import { createRoot } from "react-dom/client";
    import { Shell } from "./src/shell.jsx";
    export const mount = (el, projectId) => createRoot(el).render(createElement(Shell, { projectId }, createElement("p", { id: "content" }, "page body")));
  `;
  await build({ stdin: { contents: entry, resolveDir: join(here, "../web-src"), loader: "jsx" }, outfile, bundle: true, format: "cjs", platform: "node", jsx: "automatic", logLevel: "silent" });
  const { mount } = require(outfile);
  const tick = () => new Promise((resolve) => setTimeout(resolve, 20));
  const snapshot = () => {
    const doc = window.document;
    const active = doc.activeElement;
    return {
      links: [...doc.querySelectorAll("nav a")].map((a) => ({ text: a.querySelector(".label").textContent.trim(), href: a.getAttribute("href"), current: a.getAttribute("aria-current") })),
      menuExpanded: doc.querySelector("[data-menu-button]")?.getAttribute("aria-expanded") ?? null,
      railCollapsed: doc.querySelector("[data-shell]")?.classList.contains("collapsed") ?? null,
      drawerOpen: doc.querySelector("[data-shell]")?.classList.contains("drawer-open") ?? null,
      scrimPresent: !!doc.querySelector("[data-scrim]"),
      bodyOverflow: doc.body.style.overflow,
      focus: active ? (active.hasAttribute("data-menu-button") ? "menu" : active.hasAttribute("data-close-button") ? "close" : active.tagName === "A" ? "link:" + (active.querySelector(".label")?.textContent.trim() ?? "") : active.tagName.toLowerCase()) : null,
      content: doc.getElementById("content")?.textContent ?? null,
      hash: window.location.hash,
    };
  };
  mount(window.document.getElementById("app"), scenario.projectId ?? null);
  await tick();
  const frames = [snapshot()];
  for (const step of scenario.steps || []) {
    if (step.click) {
      window.document.querySelector(step.click).dispatchEvent(new window.MouseEvent("click", { bubbles: true, cancelable: true }));
    } else if (step.press) {
      const target = window.document.activeElement || window.document.body;
      const event = new window.KeyboardEvent("keydown", { key: step.press, shiftKey: !!step.shift, bubbles: true, cancelable: true });
      const proceed = target.dispatchEvent(event);
      if (step.press === "Tab" && proceed) {
        // The browser's own Tab move, for the case the page lets it happen.
        const items = [...window.document.querySelectorAll("a[href], button")];
        const at = items.indexOf(target);
        const next = items[(at + (step.shift ? -1 : 1) + items.length) % items.length];
        next.focus();
      }
    } else if (step.viewport) {
      phone = step.viewport === "phone";
      for (const listener of [...phoneListeners]) listener({ matches: phone });
    } else if (step.hash !== undefined) {
      window.location.hash = step.hash;
    }
    await tick();
    frames.push(snapshot());
  }
  console.log(JSON.stringify({ frames }));
} finally {
  rmSync(dir, { recursive: true, force: true });
}
