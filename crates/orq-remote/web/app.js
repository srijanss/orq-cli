"use strict";
// The review inbox page. The bearer token is typed in, held in this one
// variable and sent as a header on each request; it is never written anywhere
// else, so closing or reloading the page forgets it.

const API_REVIEWS = "/api/reviews";
// One message for every failed sign-in, so it never says which part failed.
const SIGN_IN_FAILED = "Could not sign in. Check the token and try again.";

let token = null;

const app = document.getElementById("app");

function show(...nodes) {
  app.replaceChildren(...nodes);
}

function element(tag, text) {
  const node = document.createElement(tag);
  if (text !== undefined) {
    node.textContent = text;
  }
  return node;
}

function askForToken(message) {
  const label = element("label", "Access token");
  const field = element("input");
  field.type = "password";
  field.autocomplete = "off";
  field.spellcheck = false;
  field.setAttribute("aria-label", "Access token");
  const button = element("button", "Open the inbox");
  button.type = "button";
  const notice = element("p", message || "");
  notice.setAttribute("role", "alert");

  const submit = () => {
    const typed = field.value.trim();
    field.value = "";
    if (typed === "") {
      return;
    }
    token = typed;
    route();
  };
  button.addEventListener("click", submit);
  field.addEventListener("keydown", (event) => {
    if (event.key === "Enter") {
      submit();
    }
  });
  show(label, field, button, notice);
}

const STALE_MARKER = "stale, may be out of date";
const DETAIL_PREFIX = "#/reviews/";
// The same plain identifiers the server accepts, checked before one is put in a
// path or an address.
const RUN_ID = /^[A-Za-z0-9_-]{1,128}$/;
const DETAIL_SECTIONS = [
  ["Summary", "summary"],
  ["Risks", "risks"],
  ["Files", "files"],
  ["Tests", "tests"],
  ["Review rounds", "rounds"],
];

// Only the page that is showing may draw; a slow answer for an earlier page is dropped.
let currentView = 0;

// One row per run, in the order the API gave. Every string is set as text; a
// stale report gets its own marker and class, so it never looks current.
function runRow(run) {
  const row = element("li");
  row.setAttribute("class", run.stale === true ? "run stale" : "run");
  if (typeof run.run_id === "string" && RUN_ID.test(run.run_id)) {
    const open = element("button", String(run.feature));
    open.type = "button";
    open.addEventListener("click", () => {
      location.hash = DETAIL_PREFIX + run.run_id;
    });
    row.appendChild(open);
  } else {
    row.appendChild(element("strong", String(run.feature)));
  }
  row.appendChild(element("span", String(run.status)));
  if (run.risk !== undefined && run.risk !== null) {
    row.appendChild(element("span", "risk: " + String(run.risk)));
  }
  if (run.stale === true) {
    row.appendChild(element("em", STALE_MARKER));
  }
  return row;
}

function showRuns(runs) {
  if (runs.length === 0) {
    show(element("p", "No reviews in the inbox."));
    return;
  }
  const list = element("ul");
  runs.forEach((run) => list.appendChild(runRow(run)));
  show(list);
}

function isRun(run) {
  return run !== null && typeof run === "object" && typeof run.feature === "string" && typeof run.status === "string";
}

function parseRuns(data) {
  return data && Array.isArray(data.reviews) && data.reviews.every(isRun) ? data.reviews : null;
}

function isTextList(value) {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function parseDetail(data) {
  const ok = data !== null && typeof data === "object" && typeof data.stale === "boolean" && DETAIL_SECTIONS.every(([, key]) => isTextList(data[key]));
  return ok ? data : null;
}

// A provider link is offered only when it is a plain https address without
// credentials, and never for a stale report.
function safeHttpsLink(value) {
  if (typeof value !== "string" || !value.startsWith("https://")) {
    return null;
  }
  try {
    const url = new URL(value);
    return url.protocol === "https:" && url.username === "" && url.password === "" && url.hostname !== "" ? url.href : null;
  } catch (invalid) {
    return null;
  }
}

function showDetail(report) {
  const back = element("button", "Back to the list");
  back.type = "button";
  back.addEventListener("click", () => {
    location.hash = "";
  });
  const nodes = [back];
  if (report.stale) {
    nodes.push(element("p", STALE_MARKER));
  } else {
    const href = safeHttpsLink(report.link);
    if (href !== null) {
      const link = element("a", "Open at the provider");
      link.setAttribute("href", href);
      link.setAttribute("rel", "noopener noreferrer");
      link.setAttribute("referrerpolicy", "no-referrer");
      nodes.push(link);
    }
  }
  DETAIL_SECTIONS.forEach(([title, key]) => {
    nodes.push(element("h2", title));
    const list = element("ul");
    const lines = report[key].length === 0 ? ["none recorded"] : report[key];
    lines.forEach((line) => list.appendChild(element("li", line)));
    nodes.push(list);
  });
  show(...nodes);
}

// The token is still held, so trying again does not ask for it again.
function showUnreadable(retryAction) {
  const retry = element("button", "Try again");
  retry.type = "button";
  retry.addEventListener("click", retryAction);
  const notice = element("p", "The inbox could not be read just now.");
  notice.setAttribute("role", "alert");
  show(notice, retry);
}

function request(path) {
  return fetch(path, {
    method: "GET",
    headers: { Authorization: "Bearer " + token },
    cache: "no-store",
    referrerPolicy: "no-referrer",
  });
}

// Reads one page of data and draws it. A refused token or a failed request asks
// for the token again; a reply that is not what was expected keeps it and
// offers a retry.
async function showFrom(path, loadingText, parse, draw, retryAction) {
  currentView += 1;
  const mine = currentView;
  show(element("p", loadingText));
  try {
    const response = await request(path);
    if (mine !== currentView) {
      return;
    }
    if (response.status === 401) {
      token = null;
      askForToken(SIGN_IN_FAILED);
      return;
    }
    if (!response.ok) {
      showUnreadable(retryAction);
      return;
    }
    let data = null;
    try {
      data = parse(await response.json());
    } catch (unreadable) {
      data = null;
    }
    if (mine !== currentView) {
      return;
    }
    if (data === null) {
      showUnreadable(retryAction);
      return;
    }
    draw(data);
  } catch (failure) {
    if (mine === currentView) {
      token = null;
      askForToken(SIGN_IN_FAILED);
    }
  }
}

function load() {
  return showFrom(API_REVIEWS, "Loading the review inbox…", parseRuns, showRuns, load);
}

function openRun(runId) {
  const path = API_REVIEWS + "/" + encodeURIComponent(runId);
  return showFrom(path, "Loading the report…", parseDetail, showDetail, () => openRun(runId));
}

// The address says which page to show, so the browser's Back button works.
function route() {
  if (token === null) {
    return;
  }
  const hash = location.hash;
  const runId = hash.startsWith(DETAIL_PREFIX) ? hash.slice(DETAIL_PREFIX.length) : "";
  if (RUN_ID.test(runId)) {
    openRun(runId);
  } else {
    load();
  }
}

addEventListener("hashchange", route);
askForToken("");
