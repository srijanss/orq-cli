// The service worker: it keeps the app shell so the installed app opens offline, and nothing else.
// Data (/api), sign-in (/auth), anything but a GET and other origins always go to the network.
// A new release installs beside the old one and waits; the page offers it, and only the person's
// Reload sends "skip-waiting". The build replaces __ORQ_SHELL_VERSION__ with a hash of the shell.
const PREFIX = "orq-shell-";
const CACHE = PREFIX + __ORQ_SHELL_VERSION__;
const SHELL = ["/", "/app.css", "/app.react.js", "/manifest.webmanifest", "/icons/icon-192.png", "/icons/icon-512.png", "/icons/icon-maskable-512.png", "/icons/apple-touch-icon.png"];

// Install also opens PAGES, the record of where each open page came from (see below).
self.addEventListener("install", (event) => {
  event.waitUntil(Promise.all([caches.open(CACHE).then((cache) => cache.addAll(SHELL)), caches.open(PAGES)]));
});

// Activation records every page already open, which loaded before this worker took over and so came
// from the server, before claiming them.
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
const PAGES = "orq-pages";
const record = (client) => "/orq-page/" + encodeURIComponent(client);
const opened = new Map();
const unsaved = new Set();

// A navigation with no resulting client (one that opens no page) has no files to ask for.
async function navigate(request, client) {
  const page = await whole();
  if (!client) return page || fetch(request);
  const fromCache = Boolean(page) && (await save(client, CACHE));
  if (fromCache) opened.set(client, CACHE);
  else await learn(client, "");
  return fromCache ? page : fetch(request);
}

// Learns where a page came from and writes it to PAGES, keeping it to write later when that fails.
async function learn(client, source) {
  opened.set(client, source);
  if (!(await save(client, source))) unsaved.add(client);
}

async function save(client, source) {
  const written = await quietly(async () => {
    await (await caches.open(PAGES)).put(record(client), new Response(source));
    return true;
  });
  if (written) unsaved.delete(client);
  return Boolean(written);
}

// Pages open when this worker takes over, with no record yet, loaded from the server.
async function adopt() {
  const open = await self.clients.matchAll({ includeUncontrolled: true, type: "window" });
  for (const { id } of open) {
    if (!(await caches.match(record(id), { cacheName: PAGES }))) await learn(id, "");
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
    if (unsaved.has(client)) await save(client, opened.get(client));
    return opened.get(client);
  }
  try {
    const hit = await caches.match(record(client), { cacheName: PAGES });
    if (!hit) return undefined;
    const source = await hit.text();
    opened.set(client, source);
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
