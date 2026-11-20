//! A push puts the run's own words on screen, fetched with the signed-in session, and never names anything
//! when they cannot be fetched; a click opens the page the push is about. Checked by running the built
//! service worker under node with a fake registration, fetch and window list.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

/// Runs the built worker in a vm context that is its own `self`. `scenario.pushes` are dispatched one after
/// another, each with the server's `reply` to /api/push/text (`{status, body}`, or "offline"); `scenario.click`
/// dispatches a notificationclick on the notification the pushes left, with `windows` the open pages.
const HARNESS: &str = r#"
import { readFileSync } from "node:fs";
import vm from "node:vm";
const code = readFileSync(process.env.ORQ_SW, "utf8");
const scenario = JSON.parse(process.env.ORQ_SCENARIO);
const ORIGIN = "https://box.example.ts.net";
const out = { shown: [], fetched: [], focused: [], navigated: [], opened: [] };
const visible = new Map();
let reply;
const fetch = async (request, init = {}) => {
  const url = new URL(typeof request === "string" ? request : request.url, ORIGIN);
  out.fetched.push({ path: url.pathname, query: Object.fromEntries(url.searchParams), credentials: init.credentials ?? "same-origin" });
  if (reply === "offline") throw new TypeError("Failed to fetch");
  return new Response(JSON.stringify(reply.body ?? {}), { status: reply.status, headers: { "content-type": "application/json" } });
};
const windows = (scenario.click?.windows ?? []).map((page, index) => ({
  id: "w" + index,
  type: "window",
  url: ORIGIN + page,
  focus: async function () { out.focused.push(this.id); return this; },
  navigate: async function (to) { out.navigated.push({ id: this.id, url: new URL(to, ORIGIN).pathname }); return this; },
}));
const listeners = {};
const context = {
  fetch, URL, URLSearchParams, Response, Request, Headers, Promise, console, encodeURIComponent,
  location: new URL("/sw.js", ORIGIN),
  caches: { open: async () => ({}), keys: async () => [], match: async () => undefined },
  registration: {
    active: {},
    showNotification: async (title, options = {}) => {
      out.shown.push({ title, options });
      visible.set(options.tag ?? Symbol(), { title, options });
    },
    getNotifications: async () => [...visible.values()],
  },
  clients: {
    matchAll: async () => windows,
    openWindow: async (to) => { out.opened.push(new URL(to, ORIGIN).pathname); return null; },
    claim: async () => {},
  },
  skipWaiting: async () => {},
  addEventListener: (type, listener) => { (listeners[type] ||= []).push(listener); },
};
context.self = context;
vm.createContext(context);
vm.runInContext(code, context);
const dispatch = async (type, fields) => {
  const waits = [];
  for (const listener of listeners[type] ?? []) listener({ ...fields, waitUntil: (p) => waits.push(p) });
  await Promise.all(waits);
};
for (const push of scenario.pushes ?? []) {
  reply = push.reply;
  const data = push.data;
  await dispatch("push", { data: data === null ? null : { json: () => data, text: () => JSON.stringify(data) } });
}
if (scenario.click) {
  const shown = [...visible.values()].at(-1);
  let closed = false;
  await dispatch("notificationclick", { notification: { ...shown.options, title: shown.title, close: () => { closed = true; } } });
  out.closed = closed;
}
out.visible = [...visible.values()];
console.log(JSON.stringify(out));
"#;

fn run(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let sw = format!("{}/web/sw.js", env!("CARGO_MANIFEST_DIR"));
    let output = match Command::new("node").args(["--input-type=module", "-e", HARNESS]).env("ORQ_SW", sw).env("ORQ_SCENARIO", scenario.to_string()).output() {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => panic!("cannot run node: {error}"),
    };
    assert!(output.status.success(), "node failed: {}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).expect("node prints JSON"))
}

fn ok(title: &str, body: &str, path: &str) -> Value {
    json!({"status": 200, "body": {"title": title, "body": body, "path": path}})
}

#[test]
fn a_push_shows_the_words_the_server_gives_for_its_event_fetched_with_the_signed_in_session() {
    let data = json!({"kind": "failed", "project": "orq", "run": "run-7", "event": "e-3"});
    let Some(out) = run(json!({"pushes": [{"data": data, "reply": ok("login-page failed", "The gate went red.", "/projects/orq/orchestrations/run-7")}]})) else { return };
    assert_eq!(out["fetched"], json!([{"path": "/api/push/text", "query": {"project": "orq", "run": "run-7", "event": "e-3"}, "credentials": "same-origin"}]), "{out}");
    let shown = &out["shown"][0];
    assert_eq!(shown["title"], json!("login-page failed"), "{out}");
    assert_eq!(shown["options"]["body"], json!("The gate went red."), "{out}");
}

#[test]
fn a_push_whose_words_cannot_be_fetched_shows_the_generic_text_and_names_nothing() {
    let data = json!({"kind": "needs_input", "project": "secret-project", "run": "run-secret", "event": "e-1"});
    for reply in [json!("offline"), json!({"status": 401, "body": {"error": "unauthorized"}}), json!({"status": 404, "body": {"error": "not found"}}), json!({"status": 200, "body": "not words"})] {
        let Some(out) = run(json!({"pushes": [{"data": data, "reply": reply}]})) else { return };
        let shown = &out["shown"];
        assert_eq!(shown.as_array().map(Vec::len), Some(1), "{reply}: {out}");
        assert_eq!(shown[0]["title"], json!("orq: a run needs attention"), "{reply}: {out}");
        let text = shown[0]["title"].to_string() + &shown[0]["options"]["body"].to_string();
        assert!(!text.contains("secret") && !text.contains("needs_input"), "{reply}: nothing is named: {out}");
    }
}

#[test]
fn a_test_push_says_notifications_work_without_asking_the_server() {
    let Some(out) = run(json!({"pushes": [{"data": {"kind": "test"}, "reply": "offline"}]})) else { return };
    assert_eq!(out["shown"][0]["title"], json!("orq: notifications work on this device"), "{out}");
    assert_eq!(out["fetched"], json!([]), "a test push has no event to ask about: {out}");
}

#[test]
fn a_repeat_push_about_the_same_run_or_queue_and_kind_replaces_the_shown_one() {
    let failed = json!({"kind": "failed", "project": "orq", "run": "run-7", "event": "e-1"});
    let again = json!({"kind": "failed", "project": "orq", "run": "run-7", "event": "e-2"});
    let queue = json!({"kind": "queue_paused", "project": "orq", "event": "e-3"});
    let pushes = json!([
        {"data": failed, "reply": ok("first", "", "/x")},
        {"data": again, "reply": ok("second", "", "/x")},
        {"data": queue, "reply": "offline"},
        {"data": {"kind": "test"}, "reply": "offline"},
    ]);
    let Some(out) = run(json!({"pushes": pushes})) else { return };
    let tags: Vec<_> = out["shown"].as_array().unwrap().iter().map(|shown| shown["options"]["tag"].clone()).collect();
    assert_eq!(tags, vec![json!("run-7:failed"), json!("run-7:failed"), json!("orq:queue_paused"), json!(":test")], "{out}");
    let visible: Vec<_> = out["visible"].as_array().unwrap().iter().map(|shown| shown["title"].clone()).collect();
    assert_eq!(visible, vec![json!("second"), json!("orq: a run needs attention"), json!("orq: notifications work on this device")], "the second push replaced the first: {out}");
}

/// What a click on the notification one push left does, with `windows` the orq pages open.
fn click(data: Value, reply: Value, windows: Value) -> Option<Value> {
    run(json!({"pushes": [{"data": data, "reply": reply}], "click": {"windows": windows}}))
}

#[test]
fn a_click_focuses_an_open_orq_window_and_takes_it_to_the_page_the_words_gave() {
    let data = json!({"kind": "review", "project": "orq", "run": "run-7", "event": "e-1"});
    let Some(out) = click(data, ok("x's review is ready", "", "/projects/orq/reviews/run-7"), json!(["/dashboard", "/projects/orq/queue"])) else { return };
    assert_eq!(out["focused"], json!(["w0"]), "{out}");
    assert_eq!(out["navigated"], json!([{"id": "w0", "url": "/projects/orq/reviews/run-7"}]), "{out}");
    assert_eq!(out["opened"], json!([]), "an open window is reused: {out}");
    assert_eq!(out["closed"], json!(true), "{out}");
}

#[test]
fn a_click_with_no_orq_window_open_opens_one_at_the_right_page_for_each_kind() {
    let run_push = json!({"kind": "failed", "project": "orq", "run": "run-7", "event": "e-1"});
    let queue_push = json!({"kind": "queue_finished", "project": "orq", "event": "e-2"});
    let cases = [
        (run_push.clone(), ok("t", "", "/projects/orq/reviews/run-7"), "/projects/orq/reviews/run-7"),
        (run_push, json!("offline"), "/projects/orq/orchestrations/run-7"),
        (queue_push.clone(), json!({"status": 404, "body": {}}), "/projects/orq/queue"),
        (queue_push, json!("offline"), "/projects/orq/queue"),
        (json!({"kind": "test"}), json!("offline"), "/"),
    ];
    for (data, reply, path) in cases {
        let Some(out) = click(data.clone(), reply, json!([])) else { return };
        assert_eq!(out["opened"], json!([path]), "{data}: {out}");
        assert_eq!(out["navigated"], json!([]), "{data}: {out}");
    }
}

#[test]
fn a_click_never_opens_a_path_that_leaves_orq() {
    let data = json!({"kind": "failed", "project": "orq", "run": "run-7", "event": "e-1"});
    for path in ["https://evil.example/x", "//evil.example/x", "projects/orq"] {
        let Some(out) = click(data.clone(), ok("t", "", path), json!([])) else { return };
        assert_eq!(out["opened"], json!(["/projects/orq/orchestrations/run-7"]), "{path}: {out}");
    }
}
