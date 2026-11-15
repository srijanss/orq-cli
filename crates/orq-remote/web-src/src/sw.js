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
    event.respondWith(shell(event, url.pathname));
  }
});

// A page goes to the server first; offline, the cached shell opens and says it cannot reach the server.
async function navigate(request) {
  try {
    return await fetch(request);
  } catch (error) {
    const cached = await caches.match("/", { cacheName: CACHE });
    return cached || Response.error();
  }
}

// A shell file comes from the cache at once and is refreshed behind it for the next start.
async function shell(event, path) {
  const cached = await caches.match(path, { cacheName: CACHE });
  const refresh = fetch(path)
    .then(async (response) => {
      if (response.ok && (response.type === "basic" || response.type === "default")) {
        const cache = await caches.open(CACHE);
        await cache.put(path, response.clone());
      }
      return response;
    })
    .catch(() => null);
  event.waitUntil(refresh);
  if (cached) return cached;
  return (await refresh) || Response.error();
}
