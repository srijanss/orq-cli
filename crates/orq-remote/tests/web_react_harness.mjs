// Runs the built React page in jsdom with a scripted fetch, then prints what happened as
// JSON in the same shape as web_harness.js. Usage: node web_react_harness.mjs <bundle> '<scenario json>'
//
// scenario = { responses: [ {status, json} | {reject: true} ... ],
//              steps: [ {submit: "<token>"} | {click: "button", index?} | {release: n} | {back: true} ] }
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(join(here, "../web-src/package.json"));
const { JSDOM } = require("jsdom");

const [, , bundlePath, scenarioText] = process.argv;
const scenario = JSON.parse(scenarioText);

const dom = new JSDOM('<!doctype html><html><body><main id="app"><p>Loading the review inbox…</p></main></body></html>', {
  url: "http://localhost" + (scenario.url || "/"),
  runScripts: "outside-only",
});
const { window } = dom;
const root = window.document.getElementById("app");

const requests = [];
const held = {};
const responses = [...scenario.responses];

window.fetch = (url, options) => {
  const signal = options.signal;
  requests.push({ url, method: options.method, headers: options.headers, get aborted() { return signal ? signal.aborted : null; } });
  const index = requests.length - 1;
  const next = responses.shift();
  if (!next || (next.reject && next.hold !== "fetch")) {
    return Promise.reject(new Error("offline"));
  }
  const reply = () => ({
    status: next.status,
    ok: next.status >= 200 && next.status < 300,
    json: () => {
      if (next.hold === "json") {
        return new Promise((resolve) => {
          held[index] = () => resolve(next.json);
        });
      }
      if (next.badjson) return Promise.reject(new SyntaxError("bad json"));
      return Promise.resolve(next.json);
    },
  });
  if (next.hold === "fetch") {
    return new Promise((resolve, reject) => {
      held[index] = () => (next.reject ? reject(new Error("offline")) : resolve(reply()));
      if (signal) signal.addEventListener("abort", () => reject(new window.DOMException("aborted", "AbortError")));
    });
  }
  return Promise.resolve(reply());
};

// The page proper: inside the app shell when there is one, so its menu does not count as page content.
const page = () => root.querySelector(".content") || root;

function snapshot() {
  const texts = [];
  const inputs = [];
  const buttons = [];
  const links = [];
  for (const node of page().querySelectorAll("*")) {
    const own = [...node.childNodes].filter((child) => child.nodeType === 3).map((child) => child.data).join("");
    if (own) texts.push(own);
    if (node.tagName === "INPUT") inputs.push({ type: node.type, value: node.value, autocomplete: node.autocomplete });
    if (node.tagName === "BUTTON" && !node.hasAttribute("data-refresh")) buttons.push(node.textContent);
    if (node.tagName === "A") links.push({ href: node.getAttribute("href"), rel: node.getAttribute("rel"), text: node.textContent });
  }
  const switchers = [...root.querySelectorAll("[data-project-switcher]")].map((el) => ({ value: el.value, options: [...el.options].map((o) => o.value) }));
  const sidebarLinks = [...root.querySelectorAll("nav a")].map((a) => a.getAttribute("href"));
  return { brandLogos: root.querySelectorAll(".signin .brand svg").length, scrim: root.querySelector(".scrim") !== null, switchers, sidebarLinks, skeletons: root.querySelectorAll(".skeleton").length, refreshButtons: root.querySelectorAll("[data-refresh]").length, theme: window.document.documentElement.dataset.theme || "", toggles: root.querySelectorAll("[data-theme-toggle]").length, cards: root.querySelectorAll(".card").length, shell: root.querySelector("[data-shell]") !== null, texts, inputs, buttons, links, hash: window.location.hash === "" ? "" : window.location.hash, path: window.location.pathname + window.location.search };
}

// Any error the page raises fails the run, so a crashed render is never mistaken for an empty page.
const pageErrors = [];
window.addEventListener("error", (event) => pageErrors.push(event.error && event.error.stack ? event.error.stack : event.message));

// React and jsdom schedule work on timers, so wait a little for it to finish.
const settle = async () => {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 5));
};

const all = (tag) => [...page().querySelectorAll(tag)];

(async () => {
  window.eval(readFileSync(bundlePath, "utf8"));
  await settle();
  const frames = [{ step: "start", settled: snapshot() }];
  for (const step of scenario.steps) {
    let fieldAfter = null;
    if (step.submit !== undefined) {
      const field = all("input")[0];
      field.value = step.submit;
      all("button")[0].click();
      fieldAfter = field.value;
    } else if (step.release !== undefined) {
      held[step.release]();
    } else if (step.go !== undefined) {
      // What Back and Forward do: the address changes and the page is told.
      window.history.pushState(null, "", step.go);
      window.dispatchEvent(new window.PopStateEvent("popstate"));
    } else if (step.nav_modified !== undefined) {
      root.querySelector(step.nav_modified).dispatchEvent(new window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0, ctrlKey: true }));
    } else if (step.nav !== undefined) {
      // A click on a link anywhere in the page, shell included.
      root.querySelector(step.nav).dispatchEvent(new window.MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
    } else if (step.select !== undefined) {
      const el = root.querySelector("[data-project-switcher]");
      el.value = step.select;
      el.dispatchEvent(new window.Event("change", { bubbles: true }));
    } else if (step.back) {
      window.history.back();
    } else {
      all(step.click).filter((el) => !el.hasAttribute("data-refresh"))[step.index || 0].click();
    }
    const sync = snapshot();
    await settle();
    frames.push({ step: JSON.stringify(step), fieldAfter, sync, settled: snapshot() });
  }
  window.close();
  if (pageErrors.length > 0) {
    throw new Error("the page raised " + pageErrors.length + " error(s):\n" + pageErrors.join("\n"));
  }
  process.stdout.write(JSON.stringify({ requests, frames }));
})().catch((error) => {
  process.stderr.write(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
