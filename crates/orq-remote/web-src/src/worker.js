// Registers the service worker that keeps the app shell, and offers a new release instead of
// applying it: `onUpdate(waiting)` is called with a worker waiting to take over, and only
// `applyUpdate` (the person's Reload) tells it to, then reloads once it has.

// Service workers need a secure context; on plain http, or a browser without them, nothing happens.
export function startWorker(win, onUpdate) {
  if (!win.isSecureContext || !win.navigator || !win.navigator.serviceWorker) return;
  const container = win.navigator.serviceWorker;
  // A worker waiting while no worker controls the page is the first install, not an update.
  const offer = (worker) => {
    if (worker && container.controller) onUpdate(worker);
  };
  const register = () => {
    Promise.resolve()
      .then(() => container.register("/sw.js", { scope: "/" }))
      .then((registration) => {
        offer(registration.waiting);
        registration.addEventListener("updatefound", () => {
          const next = registration.installing;
          if (!next) return;
          next.addEventListener("statechange", () => {
            if (next.state === "installed") offer(next);
          });
        });
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
