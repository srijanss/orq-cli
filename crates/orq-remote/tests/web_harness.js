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
const responses = [...scenario.responses];

const sandbox = {
  document: { getElementById: () => root, createElement: (tag) => new Node(tag) },
  fetch: (url, options) => {
    requests.push({ url, method: options.method, headers: options.headers });
    const next = responses.shift();
    if (!next || next.reject) {
      return Promise.reject(new Error("offline"));
    }
    return Promise.resolve({ status: next.status, ok: next.status >= 200 && next.status < 300, json: async () => { if (next.badjson) throw new SyntaxError("bad json"); return next.json; } });
  },
  Array,
};

function snapshot() {
  const texts = [];
  const inputs = [];
  const buttons = [];
  walk(root, (node) => {
    if (node.text) texts.push(node.text);
    if (node.tag === "input") inputs.push({ type: node.type, value: node.value, autocomplete: node.autocomplete });
    if (node.tag === "button") buttons.push(node.text);
  });
  return { texts, inputs, buttons };
}

function find(tag) {
  let found = null;
  walk(root, (node) => {
    if (!found && node.tag === tag) found = node;
  });
  return found;
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
    } else {
      find(step.click).listeners.click();
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
