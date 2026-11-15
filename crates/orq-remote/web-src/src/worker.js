// Registers the service worker that keeps the app shell, and offers a new release instead of
// applying it: `onUpdate(waiting)` is called with a worker waiting to take over, and only
// `applyUpdate` (the person's Reload) tells it to, then reloads once it has.

// Service workers need a secure context; on plain http, or a browser without them, nothing happens.
export function startWorker(win, onUpdate) {
  if (!win.isSecureContext || !win.navigator || !win.navigator.serviceWorker) return;
  const container = win.navigator.serviceWorker;
  // A worker waiting while no worker controls the page is the first install, not an update.
  // Each release is offered once, however the browser reports it (waiting, installing or found).
  const offered = new Set();
  const offer = (worker) => {
    if (!worker || !container.controller || offered.has(worker)) return;
    offered.add(worker);
    onUpdate(worker);
  };
  // A release still installing is offered once it has installed; one already installed at once.
  const watched = new Set();
  const watch = (worker) => {
    if (!worker || watched.has(worker)) return;
    watched.add(worker);
    if (worker.state === "installed") offer(worker);
    else worker.addEventListener("statechange", () => {
      if (worker.state === "installed") offer(worker);
    });
  };
  const register = () => {
    Promise.resolve()
      .then(() => container.register("/sw.js", { scope: "/" }))
      .then((registration) => {
        offer(registration.waiting);
        watch(registration.installing);
        registration.addEventListener("updatefound", () => watch(registration.installing));
      })
      .catch(() => {});
  };
  if (win.document.readyState === "complete") register();
  else win.addEventListener("load", register);
}

export function applyUpdate(win, waiting) {
  let reloaded = false;
  win.navigator.serviceWorker.addEventListener("controllerchange", () => {
    if (reloaded) return;
    reloaded = true;
    win.location.reload();
  });
  waiting.postMessage({ type: "skip-waiting" });
}
