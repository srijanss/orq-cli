// The projects pinned on the Dashboard, by id. They are kept in this browser's local storage under one
// key (nothing is sent anywhere); a browser that blocks storage still keeps them in memory for as long
// as the page is open.
const KEY = "orq.pinned";
let inMemory = [];

export function readPins() {
  try {
    const stored = JSON.parse(window.localStorage.getItem(KEY) || "[]");
    if (Array.isArray(stored)) inMemory = stored.filter((id) => typeof id === "string");
  } catch (blocked) {
    // Storage is blocked or holds something else: what this page pinned stays.
  }
  return inMemory;
}

export function writePins(ids) {
  inMemory = ids;
  try {
    window.localStorage.setItem(KEY, JSON.stringify(ids));
  } catch (blocked) {
    // Kept in memory only.
  }
}
