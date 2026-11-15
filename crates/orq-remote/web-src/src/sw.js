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
    event.respondWith(navigate(request));
  } else if (SHELL.includes(url.pathname)) {
    event.respondWith(shell(url.pathname));
  }
});

// Every page opens this release's cached shell, online or offline, so the page, its script and its
// style always come from one release; the shell then fetches its data and says when it cannot.
async function navigate(request) {
  return (await cached("/")) || fetch(request);
}

// A shell file comes from this release's cache, which is never rewritten after install: a newer
// server's files arrive only with the next release's worker and cache.
async function shell(path) {
  return (await cached(path)) || fetch(path);
}

// A file missing from the cache, or cache storage failing, leaves the answer to the network.
async function cached(path) {
  try {
    return await caches.match(path, { cacheName: CACHE });
  } catch (error) {
    return undefined;
  }
}
