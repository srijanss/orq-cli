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
  url: "http://localhost/",
  runScripts: "outside-only",
});
const { window } = dom;
const root = window.document.getElementById("app");

const requests = [];
const held = {};
const responses = [...scenario.responses];

window.fetch = (url, options) => {
  requests.push({ url, method: options.method, headers: options.headers });
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
    });
  }
  return Promise.resolve(reply());
};

function snapshot() {
  const texts = [];
  const inputs = [];
  const buttons = [];
  const links = [];
  for (const node of root.querySelectorAll("*")) {
    const own = [...node.childNodes].filter((child) => child.nodeType === 3).map((child) => child.data).join("");
    if (own) texts.push(own);
    if (node.tagName === "INPUT") inputs.push({ type: node.type, value: node.value, autocomplete: node.autocomplete });
    if (node.tagName === "BUTTON") buttons.push(node.textContent);
    if (node.tagName === "A") links.push({ href: node.getAttribute("href"), rel: node.getAttribute("rel"), text: node.textContent });
  }
  return { texts, inputs, buttons, links, hash: window.location.hash === "" ? "" : window.location.hash };
}

// Page errors are reported on stderr so a crashed render is not mistaken for an empty page.
window.addEventListener("error", (event) => process.stderr.write("page error: " + (event.error && event.error.stack ? event.error.stack : event.message) + "\n"));

// React and jsdom schedule work on timers, so wait a little for it to finish.
const settle = async () => {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setTimeout(resolve, 5));
};

const all = (tag) => [...root.querySelectorAll(tag)];

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
    } else if (step.back) {
      window.history.back();
    } else {
      all(step.click)[step.index || 0].click();
    }
    const sync = snapshot();
    await settle();
    frames.push({ step: JSON.stringify(step), fieldAfter, sync, settled: snapshot() });
  }
  process.stdout.write(JSON.stringify({ requests, frames }));
  window.close();
})().catch((error) => {
  process.stderr.write(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
