// The service worker: it keeps the app shell so the installed app opens offline, and nothing else.
// Data (/api), sign-in (/auth), anything but a GET and other origins always go to the network.
// A new release installs beside the old one and waits; the page offers it, and only the person's
// Reload sends "skip-waiting". The build replaces __ORQ_SHELL_VERSION__ with a hash of the shell.
const PREFIX = "orq-shell-";
const CACHE = PREFIX + __ORQ_SHELL_VERSION__;
const SHELL = ["/", "/app.css", "/app.react.js", "/manifest.webmanifest", "/icons/icon-192.png", "/icons/icon-512.png", "/icons/icon-maskable-512.png", "/icons/apple-touch-icon.png"];

// Install also opens PAGES, the record of where each open page came from (see below). The first
// install, with no worker active before it, marks that in PAGES for activation.
self.addEventListener("install", (event) => {
  const first = !self.registration.active;
  const shell = caches.open(CACHE).then((cache) => cache.addAll(SHELL));
  const pages = caches.open(PAGES).then((store) => first && quietly(() => store.put(FIRST, new Response(CACHE))));
  event.waitUntil(Promise.all([shell, pages]));
});

// The first worker's activation records every page already open as the server's before claiming
// them: no worker came before it, so they loaded from the server. A later release takes no open page
// for the server's, since one with no record may be a cached page whose record was lost.
self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((name) => name.startsWith(PREFIX) && name !== CACHE).map((name) => caches.delete(name))))
      .then(() => quietly(adopt))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("message", (event) => {
  if (event.data && event.data.type === "skip-waiting") self.skipWaiting();
});

const under = (path, root) => path === root || path.startsWith(root + "/");

self.addEventListener("fetch", (event) => {
  const request = event.request;
  if (request.method !== "GET") return;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (under(url.pathname, "/api") || under(url.pathname, "/auth")) return;
  if (request.mode === "navigate") {
    event.respondWith(navigate(request, event.resultingClientId));
    event.waitUntil(quietly(forget));
  } else if (SHELL.includes(url.pathname)) {
    event.respondWith(shell(url.pathname, event.clientId));
  }
});

// A page and its files always come from one release. A page opens this release's cached shell,
// online or offline, when the whole shell is there; when a shell file is missing (or cache storage
// fails) the page opens whole from the server instead. The cache is left in place, since another
// open page may have been built from it and still need its files.
//
// Where each open page came from is never read back from whether the cache is there now. Each page is
// recorded under its client id in PAGES, which outlives the worker (the browser stops and restarts it
// while pages stay open) and which activation leaves alone: the cache name it was built from, or ""
// for the server. A page opens from the cache only once its record is written; when it cannot be, the
// page opens from the server, whose record is written as soon as storage lets it. A page with no
// record is one whose source is unknown (its record was lost), and it gets no files rather than a
// guess. `opened` keeps what this worker has learned so far, and which of it is not yet written.
//
// Navigations forget the pages that have closed, here and in PAGES, so neither grows with every tab
// ever opened. A page joins the open clients only once its navigation finishes, so a record (or what
// `opened` holds) younger than GRACE is kept: it may be a page that is still opening. Navigations
// share one sweep while it runs and start another at most once every EVERY, so a burst of tabs or
// reloads lists PAGES once.
const PAGES = "orq-pages";
const ROOT = "/orq-page/";
const record = (client) => ROOT + encodeURIComponent(client);
const OPENED = "x-orq-opened";
const FIRST = "/orq-first-install";
const GRACE = 10 * 60 * 1000;
const EVERY = 60 * 1000;
let sweeping = null;
let swept = -Infinity;
const opened = new Map();
const unsaved = new Set();

// A navigation with no resulting client (one that opens no page) has no files to ask for.
async function navigate(request, client) {
  const page = await whole();
  if (!client) return page || fetch(request);
  const fromCache = Boolean(page) && (await save(client, CACHE));
  if (fromCache) opened.set(client, { source: CACHE, at: Date.now() });
  else await learn(client, "");
  return fromCache ? page : fetch(request);
}

// Learns where a page came from and writes it to PAGES, keeping it to write later when that fails.
async function learn(client, source) {
  opened.set(client, { source, at: Date.now() });
  if (!(await save(client, source))) unsaved.add(client);
}

async function save(client, source) {
  const written = await quietly(async () => {
    await (await caches.open(PAGES)).put(record(client), new Response(source, { headers: { [OPENED]: String(Date.now()) } }));
    return true;
  });
  if (written) unsaved.delete(client);
  return Boolean(written);
}

// On the first install, pages open when this worker takes over, with no record yet, loaded from the server.
async function adopt() {
  const pages = await caches.open(PAGES);
  if (!(await pages.match(FIRST))) return;
  const open = await self.clients.matchAll({ includeUncontrolled: true, type: "window" });
  for (const { id } of open) {
    if (!(await pages.match(record(id)))) await learn(id, "");
  }
  await pages.delete(FIRST);
}

function forget() {
  if (sweeping || Date.now() - swept < EVERY) return sweeping;
  swept = Date.now();
  sweeping = sweep().finally(() => {
    sweeping = null;
  });
  return sweeping;
}

async function sweep() {
  const open = new Set((await self.clients.matchAll({ includeUncontrolled: true, type: "window" })).map(({ id }) => id));
  const stale = (client, at) => !open.has(client) && Date.now() - at > GRACE;
  for (const [client, { at }] of opened) {
    if (stale(client, at)) {
      opened.delete(client);
      unsaved.delete(client);
    }
  }
  const pages = await caches.open(PAGES);
  for (const key of await pages.keys()) {
    const path = new URL(key.url).pathname;
    if (!path.startsWith(ROOT)) continue;
    const client = decodeURIComponent(path.slice(ROOT.length));
    if (open.has(client)) continue;
    const hit = await pages.match(key);
    if (hit && stale(client, Number(hit.headers.get(OPENED)) || 0)) await pages.delete(key);
  }
}

// A shell file comes from where its page came from. A page built from a release's cache, which is
// never rewritten after install, gets its files only from that cache: a file lost, evicted or behind
// failing storage is an error rather than the newer server's copy. A page from the server gets its
// files from the server. A page whose source is unknown, or whose record cannot be read, gets errors.
async function shell(path, client) {
  const source = client ? await sourceOf(client) : "";
  if (source === undefined) return Response.error();
  return source ? cachedOrError(path, source) : fetch(path);
}

// The cache the page was built from, "" for the server, or undefined when that is not known.
async function sourceOf(client) {
  if (opened.has(client)) {
    const { source } = opened.get(client);
    if (unsaved.has(client)) await save(client, source);
    return source;
  }
  try {
    const hit = await caches.match(record(client), { cacheName: PAGES });
    if (!hit) return undefined;
    const source = await hit.text();
    opened.set(client, { source, at: Number(hit.headers.get(OPENED)) || 0 });
    return source;
  } catch (error) {
    return undefined;
  }
}

async function cachedOrError(path, cacheName) {
  return (await quietly(() => caches.match(path, { cacheName }))) || Response.error();
}

// This release's cached page, only when every shell file is cached with it.
async function whole() {
  const hits = await quietly(() => Promise.all(SHELL.map((path) => caches.match(path, { cacheName: CACHE }))));
  return hits && hits.every(Boolean) ? hits[0] : undefined;
}

// Cache storage can fail; that counts as nothing cached.
async function quietly(task) {
  try {
    return await task();
  } catch (error) {
    return undefined;
  }
}
