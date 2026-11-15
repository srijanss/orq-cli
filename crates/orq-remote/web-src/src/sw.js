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
// Where each open page came from is remembered by its client id, never read back from whether the
// cache is there now: true for this release's cache, false for the server.
const fromCache = new Map();

async function navigate(request, client) {
  const page = await whole();
  if (client) fromCache.set(client, Boolean(page));
  return page || fetch(request);
}

// A shell file comes from where its page came from. A page opened from this release's cache, which is
// never rewritten after install, gets its files only from that cache: a file lost, evicted or behind
// failing storage is an error rather than the newer server's copy. A page from the server gets its
// files from the server. A page this worker did not open (it was restarted since) falls back to the
// cache being there.
async function shell(path, client) {
  const cached = fromCache.get(client);
  if (cached === true) return cachedOrError(path);
  if (cached === false) return fetch(path);
  if (!(await quietly(() => caches.has(CACHE)))) return fetch(path);
  return cachedOrError(path);
}

async function cachedOrError(path) {
  return (await quietly(() => caches.match(path, { cacheName: CACHE }))) || Response.error();
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
