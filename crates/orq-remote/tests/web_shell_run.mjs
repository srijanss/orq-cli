// Mounts the app shell in jsdom and plays steps against it, printing a snapshot after each.
// Usage: node web_shell_run.mjs '<{projectId, hash, steps}>'
// steps: {click: selector} | {press: "Escape"|"Tab", shift?: bool} | {go: "/path"}
//      | {pointer: "down"|"move"|"up"|"cancel", at: selector, y: number, click?: bool} | {tap: selector} | {wait: ms}
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

const dom = new JSDOM('<!doctype html><html><body><div id="app"></div></body></html>', { url: "http://localhost" + (scenario.path || "/"), pretendToBeVisual: true });
const { window } = dom;
for (const name of ["window", "document", "navigator", "HTMLElement", "Node", "Event", "KeyboardEvent", "MouseEvent", "PopStateEvent"]) {
  Object.defineProperty(globalThis, name, { value: name === "window" ? window : window[name], configurable: true });
}
globalThis.IS_REACT_ACT_ENVIRONMENT = false;
// A phone-width query that a step can flip, since jsdom has no layout; `reducedMotion` answers the
// prefers-reduced-motion query.
let phone = scenario.phone === true;
const phoneListeners = new Set();
window.matchMedia = (query) => ({
  media: query,
  get matches() { return query.includes("prefers-reduced-motion") ? scenario.reducedMotion === true : phone; },
  addEventListener: (_, listener) => phoneListeners.add(listener),
  removeEventListener: (_, listener) => phoneListeners.delete(listener),
});
// jsdom has no layout: the tab bar's drawer of all sections measures `drawerHeight` (300) when open.
const drawerHeight = scenario.drawerHeight ?? 300;
Object.defineProperty(window.HTMLElement.prototype, "scrollHeight", {
  configurable: true,
  get() { return this.hasAttribute("data-tab-drawer") ? drawerHeight : 0; },
});
// Pointer capture as a browser does it: once an element captures the finger, its moves and its lift go to that
// element, and so does the click that follows the lift; the capture ends with the lift.
let captured = null;
window.Element.prototype.setPointerCapture = function () { captured = this; };
window.Element.prototype.releasePointerCapture = function () { if (captured === this) captured = null; };
window.Element.prototype.hasPointerCapture = function () { return captured === this; };

const dir = mkdtempSync(join(tmpdir(), "orq-shell-"));
try {
  const outfile = join(dir, "shell.cjs");
  const entry = `
    import { createElement } from "react";
    import { createRoot } from "react-dom/client";
    import { Shell${scenario.tabbar ? ", TabBar" : ""} } from "./src/shell.jsx";
    export const mount = (el, projectId) => createRoot(el).render(${scenario.tabbar
      // Only the tab bar, given these entries in place of navigationEntries'.
      ? `createElement(TabBar, { entries: ${JSON.stringify(scenario.tabbar)}, path: ${JSON.stringify(scenario.path || "/")} })`
      : `createElement(Shell, { projectId, onSignOut: () => {} }, createElement("p", { id: "content" }, "page body"))`});
  `;
  await build({ stdin: { contents: entry, resolveDir: join(here, "../web-src"), loader: "jsx" }, outfile, bundle: true, format: "cjs", platform: "node", jsx: "automatic", logLevel: "silent" });
  const { mount } = require(outfile);
  const tick = () => new Promise((resolve) => setTimeout(resolve, 20));
  const snapshot = () => {
    const doc = window.document;
    const active = doc.activeElement;
    return {
      matches: Object.fromEntries((scenario.selectors || []).map((sel) => [sel, doc.querySelectorAll(sel).length])),
      topbarText: doc.querySelector(".topbar")?.textContent ?? null,
      links: [...doc.querySelectorAll(".sidebar nav a")].map((a) => ({ text: a.querySelector(".label").textContent.trim(), href: a.getAttribute("href"), current: a.getAttribute("aria-current") })),
      menuExpanded: doc.querySelector("[data-menu-button]")?.getAttribute("aria-expanded") ?? null,
      railCollapsed: doc.querySelector("[data-shell]")?.classList.contains("collapsed") ?? null,
      drawerOpen: doc.querySelector("[data-shell]")?.classList.contains("drawer-open") ?? null,
      scrimPresent: !!doc.querySelector("[data-scrim]"),
      bodyOverflow: doc.body.style.overflow,
      focus: active ? (active.hasAttribute("data-menu-button") ? "menu" : active.hasAttribute("data-close-button") ? "close" : active.tagName === "A" ? "link:" + (active.querySelector(".label")?.textContent.trim() ?? "") : active.tagName.toLowerCase()) : null,
      content: doc.getElementById("content")?.textContent ?? null,
      // Each `probe` selector's matches: their text and the attributes the tab bar and its sheet use.
      probes: Object.fromEntries((scenario.probe || []).map((sel) => [sel, [...doc.querySelectorAll(sel)].map((el) => ({
        tag: el.tagName.toLowerCase(),
        text: el.textContent.trim(),
        label: el.getAttribute("aria-label"),
        href: el.getAttribute("href"),
        current: el.getAttribute("aria-current"),
        expanded: el.getAttribute("aria-expanded"),
        popup: el.getAttribute("aria-haspopup"),
        icon: el.querySelector("svg path")?.getAttribute("d") ?? null,
        iconSize: el.querySelector("svg")?.getAttribute("width") ?? null,
        hidden: el.getAttribute("aria-hidden"),
        height: el.style.height || null,
        transition: el.style.transition || null,
      }))])),
      active: active && active !== doc.body ? { text: active.textContent.trim(), handle: active.hasAttribute("data-tab-handle"), inDrawer: !!active.closest("[data-tab-drawer]") } : null,
      path: window.location.pathname + window.location.search,
    };
  };
  mount(window.document.getElementById("app"), scenario.projectId ?? null);
  await tick();
  const frames = [snapshot()];
  for (const step of scenario.steps || []) {
    if (step.click) {
      window.document.querySelector(step.click).dispatchEvent(new window.MouseEvent("click", { bubbles: true, cancelable: true }));
    } else if (step.pointer || step.tap) {
      // A finger going down, moving, lifting or cancelled at height `y` over the element `at` (or the element
      // that captured it). `click: true` on a lift, and every `tap` (a down and a lift in place), also send the
      // click a browser sends after the lift, to the captured element if there is one.
      const Pointer = window.PointerEvent || window.MouseEvent;
      const send = (type, y, over) => {
        const target = type === "down" ? over : captured || over;
        target.dispatchEvent(new Pointer("pointer" + type, { bubbles: true, cancelable: true, clientX: 100, clientY: y, pointerId: 1 }));
        return target;
      };
      const over = window.document.querySelector(step.tap || step.at);
      const y = step.y ?? 600;
      const types = step.tap ? ["down", "up"] : [step.pointer];
      let lifted = null;
      for (const type of types) {
        const target = send(type, y, over);
        if (type === "up" || type === "cancel") {
          lifted = type === "up" ? target : null;
          captured = null;
        }
      }
      if (lifted && (step.tap || step.click)) lifted.dispatchEvent(new window.MouseEvent("click", { bubbles: true, cancelable: true }));
    } else if (step.wait !== undefined) {
      await new Promise((resolve) => setTimeout(resolve, step.wait));
    } else if (step.press) {
      const target = window.document.activeElement || window.document.body;
      const event = new window.KeyboardEvent("keydown", { key: step.press, shiftKey: !!step.shift, bubbles: true, cancelable: true });
      const proceed = target.dispatchEvent(event);
      if (step.press === "Tab" && proceed) {
        // The browser's own Tab move, for the case the page lets it happen.
        const items = [...window.document.querySelectorAll("a[href], button:not([data-rail-button])")];
        const at = items.indexOf(target);
        const next = items[(at + (step.shift ? -1 : 1) + items.length) % items.length];
        next.focus();
      }
    } else if (step.viewport) {
      phone = step.viewport === "phone";
      for (const listener of [...phoneListeners]) listener({ matches: phone });
    } else if (step.go !== undefined) {
      window.history.pushState(null, "", step.go);
      window.dispatchEvent(new window.PopStateEvent("popstate"));
    }
    await tick();
    frames.push(snapshot());
  }
  console.log(JSON.stringify({ frames }));
} finally {
  rmSync(dir, { recursive: true, force: true });
}
