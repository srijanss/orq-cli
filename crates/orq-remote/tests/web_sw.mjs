// Runs the built service worker and the page's registration code against a fake browser, then
// prints what happened as JSON. Usage: node web_sw.mjs <built sw.js> <web-src/src/worker.js>
//
// The worker runs in a vm context that is its own `self`, with fake `caches` and `fetch`. The network
// answers "<phase> <path>", so a reply shows whether it came from the install-time cache or later.
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import vm from "node:vm";

const [, , swPath, workerPath] = process.argv;
const code = readFileSync(swPath, "utf8");
const ORIGIN = "https://box.example.ts.net";
const pathOf = (request) => new URL(typeof request === "string" ? request : request.url, ORIGIN).pathname;

function boot() {
  const listeners = {};
  const stores = new Map();
  const net = { online: true, phase: "install", fetched: [] };
  const state = { claimed: false, skipped: false };
  const fetch = async (request) => {
    const path = pathOf(request);
    net.fetched.push(path);
    if (!net.online) throw new TypeError("Failed to fetch");
    return new Response(`${net.phase} ${path}`, { status: 200 });
  };
  const store = (name) => {
    if (!stores.has(name)) stores.set(name, new Map());
    return stores.get(name);
  };
  const cache = (name) => ({
    addAll: async (paths) => { for (const path of paths) store(name).set(pathOf(path), await fetch(path)); },
    add: async (path) => { store(name).set(pathOf(path), await fetch(path)); },
    put: async (request, response) => { store(name).set(pathOf(request), response); },
    match: async (request) => { const hit = store(name).get(pathOf(request)); return hit ? hit.clone() : undefined; },
  });
  const caches = {
    open: async (name) => { store(name); return cache(name); },
    keys: async () => [...stores.keys()],
    has: async (name) => stores.has(name),
    delete: async (name) => stores.delete(name),
    match: async (request, options = {}) => {
      const names = options.cacheName ? [options.cacheName] : [...stores.keys()];
      for (const name of names) {
        const hit = stores.has(name) ? stores.get(name).get(pathOf(request)) : undefined;
        if (hit) return hit.clone();
      }
      return undefined;
    },
  };
  const context = {
    caches, fetch, URL, Response, Request, Headers, Promise, console,
    location: new URL("/sw.js", ORIGIN),
    clients: { claim: async () => { state.claimed = true; } },
    skipWaiting: async () => { state.skipped = true; },
    addEventListener: (type, listener) => { (listeners[type] ||= []).push(listener); },
  };
  context.self = context;
  vm.createContext(context);
  vm.runInContext(code, context);
  // Dispatches an event the way the browser does and waits for everything it asked to wait for.
  const dispatch = async (type, fields = {}) => {
    const waits = [];
    let responded = null;
    const event = { ...fields, waitUntil: (promise) => { waits.push(promise); }, respondWith: (promise) => { responded = Promise.resolve(promise); } };
    for (const listener of listeners[type] || []) listener(event);
    const response = responded === null ? null : await responded;
    await Promise.all(waits);
    return { handled: responded !== null, response };
  };
  const request = (path, { method = "GET", mode = "cors", origin = ORIGIN } = {}) => ({ url: origin + path, method, mode, headers: new Headers() });
  const ask = async (path, options) => {
    const { handled, response } = await dispatch("fetch", { request: request(path, options) });
    return { handled, text: response ? (response.type === "error" ? "<network error>" : await response.text()) : null };
  };
  const cached = (name) => Object.fromEntries([...(stores.get(name) || new Map()).entries()].map(([path, response]) => [path, response]));
  const cachedText = async (name, path) => { const hit = stores.has(name) ? stores.get(name).get(path) : undefined; return hit ? await hit.clone().text() : null; };
  return { dispatch, ask, stores, net, state, cached, cachedText, caches };
}

async function worker() {
  const sw = boot();
  await sw.dispatch("install");
  const names = [...sw.stores.keys()];
  const shellCache = names[0];
  const shellPaths = Object.keys(sw.cached(shellCache)).sort();
  const claimedBeforeActivate = sw.state.claimed;
  const skippedOnInstall = sw.state.skipped;

  // An older release's shell and an unrelated cache are there when this one activates.
  (await sw.caches.open("orq-shell-0000000000000000")).put("/", new Response("old /"));
  await sw.caches.open("someone-elses-cache");
  await sw.dispatch("activate");
  const afterActivate = [...sw.stores.keys()].sort();

  // The server has moved on to a newer release while this one runs.
  sw.net.phase = "later";
  const installFetches = sw.net.fetched.length;
  const online = {
    api: await sw.ask("/api/projects"),
    apiEvents: await sw.ask("/api/events"),
    apiNavigation: await sw.ask("/api/reviews", { mode: "navigate" }),
    auth: await sw.ask("/auth/status"),
    authNavigation: await sw.ask("/auth/passkey/login", { mode: "navigate" }),
    post: await sw.ask("/app.css", { method: "POST" }),
    postNavigation: await sw.ask("/projects/p", { method: "POST", mode: "navigate" }),
    otherOrigin: await sw.ask("/app.css", { origin: "https://elsewhere.example" }),
    worker: await sw.ask("/sw.js"),
    navigation: await sw.ask("/projects/p/orchestrations/run-1", { mode: "navigate" }),
    asset: await sw.ask("/app.css"),
  };
  const cachedAfter = await sw.cachedText(shellCache, "/app.css");
  const fetchedOnline = sw.net.fetched.slice(installFetches);

  sw.net.online = false;
  sw.net.phase = "never";
  const offline = {
    navigation: await sw.ask("/projects/p/orchestrations/run-1", { mode: "navigate" }),
    root: await sw.ask("/", { mode: "navigate" }),
    asset: await sw.ask("/app.react.js"),
    style: await sw.ask("/app.css"),
    icon: await sw.ask("/icons/icon-192.png"),
  };

  const notSkip = boot();
  await notSkip.dispatch("message", { data: { type: "something-else" } });
  await sw.dispatch("message", { data: { type: "skip-waiting" } });

  return {
    names, shellPaths, claimedBeforeActivate, skippedOnInstall, afterActivate, claimed: sw.state.claimed,
    online, cachedAfter, fetchedOnline, offline, skipWaiting: sw.state.skipped, otherMessageSkips: notSkip.state.skipped,
  };
}

// A fake window for worker.js: `secure`, `api` (navigator.serviceWorker exists), `ready` (the document
// had loaded), `fails` (register rejects), `waiting`, `installing` and `controller` (what the registration holds).
function fakeWindow({ secure = true, api = true, ready = true, fails = false, waiting = false, installing = false, controller = true } = {}) {
  const log = { registered: [], messages: [], reloads: 0, listeners: {} };
  const on = (bag) => (type, listener) => { (bag[type] ||= []).push(listener); };
  const fire = (bag, type, event = {}) => { for (const listener of bag[type] || []) listener(event); };
  const workerListeners = {};
  const waitingWorker = { state: "installed", postMessage: (message) => log.messages.push(message) };
  const newWorker = () => {
    const next = { state: "installing", postMessage: (message) => log.messages.push(message), listeners: {} };
    next.addEventListener = on(next.listeners);
    return next;
  };
  const registration = { waiting: waiting ? waitingWorker : null, installing: installing ? newWorker() : null, addEventListener: on(workerListeners) };
  const container = {
    controller: controller ? {} : null,
    register: (url, options) => { log.registered.push({ url, scope: options && options.scope }); return fails ? Promise.reject(new Error("blocked")) : Promise.resolve(registration); },
    addEventListener: on(log.listeners),
  };
  const windowListeners = {};
  const win = {
    isSecureContext: secure,
    navigator: api ? { serviceWorker: container } : {},
    document: { readyState: ready ? "complete" : "loading" },
    location: { reload: () => { log.reloads += 1; } },
    addEventListener: on(windowListeners),
  };
  // A new worker found later: it installs while this page is open.
  // The worker already installing finishes; `found` says the browser also reported it as found.
  const finish = ({ found = false } = {}) => {
    const next = registration.installing;
    if (found) fire(workerListeners, "updatefound");
    next.state = "installed";
    registration.waiting = next;
    registration.installing = null;
    fire(next.listeners, "statechange");
  };
  const install = () => {
    registration.installing = newWorker();
    finish({ found: true });
  };
  return { win, log, load: () => fire(windowListeners, "load"), controllerChange: () => fire(log.listeners, "controllerchange"), install, finish, waitingWorker };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

async function registration() {
  const { startWorker, applyUpdate } = await import(pathToFileURL(workerPath).href);
  let unhandled = 0;
  process.on("unhandledRejection", () => { unhandled += 1; });
  const run = async (options, after) => {
    const fake = fakeWindow(options);
    const updates = [];
    let threw = null;
    try { startWorker(fake.win, (waiting) => updates.push(waiting)); } catch (error) { threw = String(error); }
    await tick();
    const beforeLoad = fake.log.registered.length;
    if (after) await after(fake);
    await tick();
    return { threw, beforeLoad, registered: fake.log.registered, updates: updates.length, reloads: fake.log.reloads, messages: fake.log.messages };
  };
  const result = {
    insecure: await run({ secure: false }),
    noApi: await run({ api: false }),
    loaded: await run({}),
    loading: await run({ ready: false }, (fake) => fake.load()),
    fails: await run({ fails: true }),
    waiting: await run({ waiting: true }),
    firstInstall: await run({ waiting: true, controller: false }),
    foundLater: await run({}, (fake) => fake.install()),
    installingAtRegister: await run({ installing: true }, (fake) => fake.finish()),
    installingAndFound: await run({ installing: true }, (fake) => fake.finish({ found: true })),
  };
  // Reload is the person's choice: the waiting worker is told to take over, and the page reloads once it has.
  const fake = fakeWindow({ waiting: true });
  applyUpdate(fake.win, fake.waitingWorker);
  const reloadsBeforeTakeover = fake.log.reloads;
  fake.controllerChange();
  fake.controllerChange();
  result.apply = { messages: fake.log.messages, reloadsBeforeTakeover, reloads: fake.log.reloads };
  await tick();
  result.unhandled = unhandled;
  return result;
}

process.stdout.write(JSON.stringify({ worker: await worker(), registration: await registration() }));
