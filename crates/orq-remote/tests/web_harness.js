"use strict";
// Runs the page script against a fake page and a scripted fetch, then prints
// what happened as JSON. Usage: node web_harness.js <app.js> '<scenario json>'
//
// scenario = { responses: [ {status, json} | {reject: true} ... ],
//              steps: [ {submit: "<token>"} | {click: "button"} ... ] }

const fs = require("fs");
const vm = require("vm");

const [, , scriptPath, scenarioText] = process.argv;
const scenario = JSON.parse(scenarioText);

class Node {
  constructor(tag) {
    this.tag = tag;
    this.children = [];
    this.text = "";
    this.value = "";
    this.attrs = {};
    this.listeners = {};
  }
  set textContent(value) {
    this.text = String(value);
  }
  get textContent() {
    return this.text;
  }
  setAttribute(name, value) {
    this.attrs[name] = String(value);
  }
  addEventListener(name, handler) {
    this.listeners[name] = handler;
  }
  appendChild(node) {
    this.children.push(node);
    return node;
  }
  replaceChildren(...nodes) {
    this.children = nodes;
  }
}

function walk(node, visit) {
  visit(node);
  node.children.forEach((child) => walk(child, visit));
}

const root = new Node("main");
const requests = [];
const held = {};
const responses = [...scenario.responses];

// A page address with a history, so Back can be tried: setting the hash adds an
// entry and tells the page asynchronously, as a browser does.
const pageListeners = {};
const entries = [""];
const location = {
  get hash() {
    return entries[entries.length - 1];
  },
  set hash(value) {
    entries.push(String(value));
    setImmediate(() => pageListeners.hashchange && pageListeners.hashchange());
  },
};

const sandbox = {
  document: { getElementById: () => root, createElement: (tag) => new Node(tag) },
  fetch: (url, options) => {
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
    // A held reply stays pending until a {release: n} step lets request n finish.
    if (next.hold === "fetch") {
      return new Promise((resolve, reject) => {
        held[index] = () => (next.reject ? reject(new Error("offline")) : resolve(reply()));
      });
    }
    return Promise.resolve(reply());
  },
  Array,
  URL,
  location,
  addEventListener: (name, handler) => {
    pageListeners[name] = handler;
  },
};

function snapshot() {
  const texts = [];
  const inputs = [];
  const buttons = [];
  const links = [];
  walk(root, (node) => {
    if (node.text) texts.push(node.text);
    if (node.tag === "input") inputs.push({ type: node.type, value: node.value, autocomplete: node.autocomplete });
    if (node.tag === "button") buttons.push(node.text);
    if (node.tag === "a") links.push({ href: node.attrs.href, rel: node.attrs.rel, text: node.text });
  });
  return { texts, inputs, buttons, links, hash: location.hash };
}

function find(tag, index = 0) {
  const all = [];
  walk(root, (node) => {
    if (node.tag === tag) all.push(node);
  });
  return all[index] || null;
}

const settle = async () => {
  for (let i = 0; i < 10; i++) await new Promise((resolve) => setImmediate(resolve));
};

(async () => {
  vm.runInNewContext(fs.readFileSync(scriptPath, "utf8"), sandbox);
  const frames = [{ step: "start", settled: snapshot() }];
  for (const step of scenario.steps) {
    let fieldAfter = null;
    if (step.submit !== undefined) {
      const field = find("input");
      field.value = step.submit;
      find("button").listeners.click();
      fieldAfter = field.value;
    } else if (step.release !== undefined) {
      held[step.release]();
    } else if (step.back) {
      entries.pop();
      pageListeners.hashchange && pageListeners.hashchange();
    } else {
      find(step.click, step.index || 0).listeners.click();
    }
    const sync = snapshot();
    await settle();
    frames.push({ step: JSON.stringify(step), fieldAfter, sync, settled: snapshot() });
  }
  process.stdout.write(JSON.stringify({ requests, frames }));
})().catch((error) => {
  process.stderr.write(String(error && error.stack ? error.stack : error));
  process.exit(1);
});
