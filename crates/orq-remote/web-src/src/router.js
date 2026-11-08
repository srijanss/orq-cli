// The place lives in the address path and query, so reload, Back and Forward restore it.
// Moving forward adds a history entry; the page is told through one event, Back and Forward
// through popstate.
const NAVIGATED = "orq:navigate";

export function currentLocation() {
  return window.location.pathname + window.location.search;
}

export function currentPath() {
  return window.location.pathname;
}

export function subscribeToLocation(notify) {
  window.addEventListener("popstate", notify);
  window.addEventListener(NAVIGATED, notify);
  return () => {
    window.removeEventListener("popstate", notify);
    window.removeEventListener(NAVIGATED, notify);
  };
}

export function navigate(target) {
  if (currentLocation() !== target) window.history.pushState(null, "", target);
  window.dispatchEvent(new Event(NAVIGATED));
}

// A plain left click is ours to handle; a modified one belongs to the browser (new tab, window...).
export function isPlainLeftClick(event) {
  return event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey && !event.defaultPrevented;
}

// Addresses shared before the place moved into the path look like /#/reviews/<id>. Rewrite a
// recognised one to its path in place (no new history entry) so the old link still opens.
export function migrateLegacyHash() {
  const match = /^#(\/(?:dashboard|settings|projects(?:\/[^/]+){0,2}|reviews\/[^/]+))$/.exec(window.location.hash);
  if (match && window.location.pathname === "/") window.history.replaceState(null, "", match[1] + window.location.search);
}
