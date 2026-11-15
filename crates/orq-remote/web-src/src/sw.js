// The service worker: it keeps the app shell so the installed app opens offline, and nothing else.
// Data (/api), sign-in (/auth), anything but a GET and other origins always go to the network.
// A new release installs beside the old one and waits; the page offers it, and only the person's
// Reload sends "skip-waiting". The build replaces __ORQ_SHELL_VERSION__ with a hash of the shell.
const PREFIX = "orq-shell-";
const CACHE = PREFIX + __ORQ_SHELL_VERSION__;
const SHELL = ["/", "/app.css", "/app.react.js", "/manifest.webmanifest", "/icons/icon-192.png", "/icons/icon-512.png", "/icons/icon-maskable-512.png", "/icons/apple-touch-icon.png"];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((names) => Promise.all(names.filter((name) => name.startsWith(PREFIX) && name !== CACHE).map((name) => caches.delete(name))))
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
// Where each open page came from is never read back from whether the cache is there now. A page
// opened from a release's cache is recorded under its client id in PAGES, which outlives the worker
// (the browser stops and restarts it while pages stay open) and which activation leaves alone. A page
// with no record came from the server: one this worker opened from the server, or one that loaded
// before any worker. So a page opens from the cache only once its record is written; when the record
// cannot be written the page opens from the server instead. `opened` keeps what this worker has
// learned so far: the cache name a page was built from, or "" for the server.
const PAGES = "orq-pages";
const record = (client) => "/orq-page/" + encodeURIComponent(client);
const opened = new Map();

// A navigation with no resulting client (one that opens no page) has no files to ask for.
async function navigate(request, client) {
  const page = await whole();
  if (!client) return page || fetch(request);
  const fromCache = Boolean(page) && Boolean(await quietly(() => remember(client)));
  opened.set(client, fromCache ? CACHE : "");
  return fromCache ? page : fetch(request);
}

async function remember(client) {
  await (await caches.open(PAGES)).put(record(client), new Response(CACHE));
  return true;
}

// A shell file comes from where its page came from. A page built from a release's cache, which is
// never rewritten after install, gets its files only from that cache: a file lost, evicted or behind
// failing storage is an error rather than the newer server's copy. A page from the server gets its
// files from the server. When the page's record cannot be read, its files are an error too.
async function shell(path, client) {
  const source = client ? await sourceOf(client) : "";
  if (source === undefined) return Response.error();
  return source ? cachedOrError(path, source) : fetch(path);
}

// The cache the page was built from, "" for the server, or undefined when that cannot be read.
async function sourceOf(client) {
  if (opened.has(client)) return opened.get(client);
  try {
    const hit = await caches.match(record(client), { cacheName: PAGES });
    const source = hit ? await hit.text() : "";
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
