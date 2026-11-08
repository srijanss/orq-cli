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

// Intervals are faked so a test can fire them: {id, delay, active, run}. Only setInterval is faked;
// React schedules its own work with other timers.
const intervals = [];
window.setInterval = (run, delay) => { intervals.push({ delay, active: true, run }); return intervals.length; };
window.clearInterval = (id) => { if (intervals[id - 1]) intervals[id - 1].active = false; };
// Listeners for the tab being shown or hidden, so a test can see that leaving a page removes its own.
const visibilityListeners = new Set();
const addListener = window.document.addEventListener.bind(window.document);
const removeListener = window.document.removeEventListener.bind(window.document);
window.document.addEventListener = (type, listener, options) => { if (type === "visibilitychange") visibilityListeners.add(listener); return addListener(type, listener, options); };
window.document.removeEventListener = (type, listener, options) => { if (type === "visibilitychange") visibilityListeners.delete(listener); return removeListener(type, listener, options); };
let visibility = "visible";
Object.defineProperty(window.document, "visibilityState", { get() { return visibility; } });
Object.defineProperty(window.document, "hidden", { get() { return visibility === "hidden"; } });

// Retry timers of the live follower (3000 ms) are faked too, so a test fires them: {fire: 3000}.
const timeouts = [];
const realSetTimeout = window.setTimeout.bind(window);
const realClearTimeout = window.clearTimeout.bind(window);
window.setTimeout = (run, delay, ...rest) => { if (delay === 3000) { timeouts.push({ active: true, run }); return -timeouts.length; } return realSetTimeout(run, delay, ...rest); };
window.clearTimeout = (id) => { if (id < 0) { if (timeouts[-id - 1]) timeouts[-id - 1].active = false; } else realClearTimeout(id); };
window.TextDecoder = TextDecoder;

// A download is a Blob given to createObjectURL and an anchor with a `download` name that is clicked;
// both are recorded, and the blobs are read at the end as `downloads: [{name, type, text, revoked}]`.
const blobs = [];
const downloads = [];
window.URL.createObjectURL = (blob) => { blobs.push({ blob, revoked: false }); return "blob:harness-" + (blobs.length - 1); };
window.URL.revokeObjectURL = (url) => { const entry = blobs[Number(String(url).replace("blob:harness-", ""))]; if (entry) entry.revoked = true; };
window.HTMLAnchorElement.prototype.click = function () { downloads.push({ name: this.getAttribute("download"), href: this.getAttribute("href"), entry: blobs[Number(String(this.getAttribute("href")).replace("blob:harness-", ""))] }); };
const readBlob = (blob) => new Promise((resolve) => { const reader = new window.FileReader(); reader.onload = () => resolve(reader.result); reader.readAsText(blob); });

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
  // A streamed body: {stream: [text chunks], end: "close" (default) | "hold" | "error"}.
  const body = () => {
    const chunks = [...next.stream];
    const aborted = () => signal && signal.aborted;
    return {
      getReader: () => ({
        read: () => {
          if (aborted()) return Promise.reject(new window.DOMException("aborted", "AbortError"));
          if (chunks.length > 0) return Promise.resolve({ done: false, value: new TextEncoder().encode(chunks.shift()) });
          if (next.end === "error") return Promise.reject(new Error("dropped"));
          if (next.end === "hold") return new Promise((resolve, reject) => { if (signal) signal.addEventListener("abort", () => reject(new window.DOMException("aborted", "AbortError"))); });
          return Promise.resolve({ done: true });
        },
      }),
    };
  };
  const reply = () => ({
    status: next.status,
    ok: next.status >= 200 && next.status < 300,
    body: next.stream ? body() : null,
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
  const kept = (name) => { try { return Object.keys(window[name]).length === 0 ? null : Object.fromEntries(Object.entries(window[name]).map(([k, v]) => [k, v === scenario.token ? "<the token>" : "<something else>"])); } catch (blocked) { return "blocked"; } };
  const checked = [...root.querySelectorAll("[data-last-checked]")].map((el) => el.textContent);
  return { retries: timeouts.filter((t) => t.active).length, visibilityListeners: visibilityListeners.size, activeIntervals: intervals.filter((i) => i.active).map((i) => i.delay), checked, sessionKept: kept("sessionStorage"), localKept: kept("localStorage"), brandLogos: root.querySelectorAll(".signin .brand svg").length, scrim: root.querySelector(".scrim") !== null, switchers, sidebarLinks, skeletons: root.querySelectorAll(".skeleton").length, refreshButtons: root.querySelectorAll("[data-refresh]").length, theme: window.document.documentElement.dataset.theme || "", toggles: root.querySelectorAll("[data-theme-toggle]").length, cards: root.querySelectorAll(".card").length, shell: root.querySelector("[data-shell]") !== null, texts, inputs, buttons, links, hash: window.location.hash === "" ? "" : window.location.hash, path: window.location.pathname + window.location.search };
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
  // A refresh keeps what the tab stored; storage that throws is what a locked-down browser does.
  if (scenario.storage_blocked) {
    for (const name of ["sessionStorage", "localStorage"]) Object.defineProperty(window, name, { get() { throw new Error("blocked"); } });
  } else {
    for (const [key, value] of Object.entries(scenario.session || {})) window.sessionStorage.setItem(key, value);
  }
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
    } else if (step.tick !== undefined) {
      // The interval of that length fires once.
      for (const interval of intervals.filter((i) => i.active && i.delay === step.tick)) interval.run();
    } else if (step.fire !== undefined) {
      for (const timer of timeouts.filter((t) => t.active)) { timer.active = false; timer.run(); }
    } else if (step.visibility !== undefined) {
      visibility = step.visibility;
      window.document.dispatchEvent(new window.Event("visibilitychange"));
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
    } else if (step.set !== undefined) {
      // Types into a field or picks an option, the way React sees a real change.
      const el = root.querySelector(step.set);
      const proto = el.tagName === "SELECT" ? window.HTMLSelectElement.prototype : window.HTMLInputElement.prototype;
      Object.getOwnPropertyDescriptor(proto, "value").set.call(el, step.value);
      el.dispatchEvent(new window.Event(el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
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
  // Blobs are read while the window is still open; a closed window never finishes a read.
  const saved = [];
  for (const d of downloads) saved.push({ name: d.name, type: d.entry ? d.entry.blob.type : null, text: d.entry ? await readBlob(d.entry.blob) : null, revoked: d.entry ? d.entry.revoked : null });
  window.close();
  if (pageErrors.length > 0) {
    throw new Error("the page raised " + pageErrors.length + " error(s):\n" + pageErrors.join("\n"));
  }
  process.stdout.write(JSON.stringify({ requests, frames, downloads: saved }));
})().catch((error) => {
  process.stderr.write(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
