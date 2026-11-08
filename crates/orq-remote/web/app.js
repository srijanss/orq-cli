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
    load();
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

// One row per run, in the order the API gave. Every string is set as text; a
// stale report gets its own marker and class, so it never looks current.
function runRow(run) {
  const row = element("li");
  row.setAttribute("class", run.stale === true ? "run stale" : "run");
  row.appendChild(element("strong", String(run.feature)));
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

async function load() {
  show(element("p", "Loading the review inbox…"));
  try {
    const response = await fetch(API_REVIEWS, {
      method: "GET",
      headers: { Authorization: "Bearer " + token },
      cache: "no-store",
      referrerPolicy: "no-referrer",
    });
    if (response.status === 401) {
      token = null;
      askForToken(SIGN_IN_FAILED);
      return;
    }
    if (!response.ok) {
      // The token is still held, so trying again does not ask for it again.
      const retry = element("button", "Try again");
      retry.type = "button";
      retry.addEventListener("click", load);
      const notice = element("p", "The inbox could not be read just now.");
      notice.setAttribute("role", "alert");
      show(notice, retry);
      return;
    }
    const data = await response.json();
    showRuns(Array.isArray(data.reviews) ? data.reviews : []);
  } catch (failure) {
    token = null;
    askForToken(SIGN_IN_FAILED);
  }
}

askForToken("");
