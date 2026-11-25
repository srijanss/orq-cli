//! "Ask orq": a navigation entry (sidebar after Reviews, the phone bar's second row after Todos and Reviews)
//! that opens a chat panel beside the page (a full-screen sheet on a phone) without leaving it. The panel knows
//! the page it was opened on, lists the project's chats, shows a conversation (replies as safe Markdown, tool
//! calls as closed lines, refused calls with Allow buttons that ask the passkey), follows a running reply as
//! it streams, and stops it after asking. Run in headless Chromium against a fake API scripted in the page.

mod web_chromium;

use serde_json::{json, Value};

const PROJECT: &str = "alpha";
/// The ledger rule a refused ledger edit names.
const LEDGER_RULE: &str = "Edit(/srv/alpha/.tdd-features.json)";

/// Replaces the page's fetch with a scripted API before the app loads. `__API__` is `{routes, session}`:
/// each route `{method, path, replies}` (a path ending in `*` matches by prefix) answers its replies in turn
/// (the last one repeats); a reply is `{status, body, after}` (answered `after` ms late) or
/// `{status, stream: [{after, text}], hold}`, a body
/// of text chunks sent `after` ms apart and left open when `hold`. Every request goes into `window.__sent`
/// as `{method, path, body}`. `session` signs the page in with a passkey session (no token); the passkey
/// prompt is answered by a fake credential.
const FAKE: &str = r#"
(function () {
  var api = __API__;
  window.__sent = [];
  var counts = {};
  var real = window.fetch.bind(window);
  function json(status, body) {
    return new Response(JSON.stringify(body), { status: status, headers: { "Content-Type": "application/json" } });
  }
  function stream(reply) {
    var chunks = reply.stream || [];
    var encoder = new TextEncoder();
    var body = new ReadableStream({
      start: function (controller) {
        var at = 0;
        function next() {
          if (at >= chunks.length) {
            if (!reply.hold) controller.close();
            return;
          }
          var chunk = chunks[at++];
          setTimeout(function () { controller.enqueue(encoder.encode(chunk.text)); next(); }, chunk.after || 0);
        }
        next();
      }
    });
    return new Response(body, { status: reply.status || 200, headers: { "Content-Type": "text/event-stream" } });
  }
  window.fetch = function (input, init) {
    var url = new URL(typeof input === "string" ? input : input.url, location.href);
    var path = url.pathname + url.search;
    if (path.indexOf("/api") !== 0 && path.indexOf("/auth") !== 0) return real(input, init);
    var method = (init && init.method) || "GET";
    var body = null;
    try { body = init && typeof init.body === "string" ? JSON.parse(init.body) : null; } catch (e) { body = init.body; }
    window.__sent.push({ method: method, path: path, body: body });
    for (var i = 0; i < api.routes.length; i++) {
      var route = api.routes[i];
      var hit = route.path.slice(-1) === "*" ? path.indexOf(route.path.slice(0, -1)) === 0 : route.path === path;
      if (route.method !== method || !hit) continue;
      var n = counts[i] || 0;
      counts[i] = n + 1;
      var reply = route.replies[Math.min(n, route.replies.length - 1)];
      if (reply.stream) return Promise.resolve(stream(reply));
      if (reply.after) return new Promise(function (done) { setTimeout(function () { done(json(reply.status || 200, reply.body)); }, reply.after); });
      return Promise.resolve(json(reply.status || 200, reply.body));
    }
    if (path === "/auth/status") return Promise.resolve(json(200, { passkeys: true, signed_in: api.session, csrf: "c-1" }));
    if (path === "/auth/confirm/begin") return Promise.resolve(json(200, { challenge: "AAAA", rpId: location.hostname, userVerification: "required", timeout: 1000, allowCredentials: [] }));
    return Promise.resolve(json(404, { error: "not found" }));
  };
  if (api.session) {
    try { sessionStorage.removeItem("orq.token"); } catch (e) {}
  }
  if (typeof window.PublicKeyCredential !== "function") window.PublicKeyCredential = function () {};
  var bytes = function () { return new Uint8Array([1, 2, 3]).buffer; };
  var credential = { rawId: bytes(), response: { clientDataJSON: bytes(), authenticatorData: bytes(), signature: bytes() } };
  try { Object.defineProperty(navigator.credentials, "get", { configurable: true, value: function () { return Promise.resolve(credential); } }); } catch (e) {}
})();
"#;

fn reply(body: Value) -> Value {
    json!({"status": 200, "body": body})
}

fn route(method: &str, path: &str, replies: Vec<Value>) -> Value {
    json!({"method": method, "path": path, "replies": replies})
}

fn project() -> Value {
    json!({"id": PROJECT, "name": PROJECT, "available": true, "features": 1, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24})
}

fn settings(agent: bool, remote_actions: bool) -> Value {
    let agent = if agent { json!({"harness": "claude", "model": "claude-opus-5-5", "effort": null}) } else { Value::Null };
    json!({"settings": {"implementer": null, "reviewer": null, "ask_orq_agent": agent, "models": [], "remote_actions": remote_actions, "version": "v-1"}})
}

fn chat(id: &str, title: &str, context: Value, running: bool) -> Value {
    json!({"id": id, "title": title, "session_id": null, "model": "claude-opus-5-5", "effort": null, "context": context, "created_at": "2026-11-25T10:00:00Z", "last_message_at": "2026-11-25T10:05:00Z", "running": running})
}

/// The routes every page needs: the projects, the settings (an agent set, remote actions on) and no chats.
fn base() -> Vec<Value> {
    vec![
        route("GET", "/api/projects", vec![reply(json!({"projects": [project()]}))]),
        route("GET", "/api/settings", vec![reply(settings(true, true))]),
        route("GET", "/api/projects/alpha/chats", vec![reply(json!({"chats": []}))]),
    ]
}

/// The page at `path`, `width` px wide, with `routes` in front of the base ones, after `steps`.
fn open(width: u32, path: &str, routes: Vec<Value>, session: bool, steps: Vec<Value>) -> Option<Value> {
    open_after(width, path, routes, session, steps, "")
}

/// As `open`, with `before` run in the page before the app loads (to leave something in its storage).
fn open_after(width: u32, path: &str, routes: Vec<Value>, session: bool, steps: Vec<Value>, before: &str) -> Option<Value> {
    let chromium = web_chromium::binary()?;
    let mut all = routes;
    all.extend(base());
    // "</" is written "<\/" so a `</script>` in a reply never closes the page's inline script.
    let init = before.to_string() + &FAKE.replace("__API__", &json!({"routes": all, "session": session}).to_string().replace("</", "<\\/"));
    let mut steps = steps;
    steps.push(json!({"eval": "sent", "js": "return window.__sent;"}));
    steps.push(json!({"eval": "address", "js": "return location.pathname + location.search;"}));
    let result = web_chromium::measure_with(&chromium, width, path, &[], Value::Array(steps), &init).result;
    assert!(result.get("error").is_none(), "{result}");
    Some(result)
}

fn eval<'a>(result: &'a Value, name: &str) -> &'a Value {
    &result["evals"][name]
}

fn text(result: &Value) -> String {
    result["text"].as_str().unwrap_or("").to_string()
}

fn sent(result: &Value, method: &str, path: &str) -> Vec<Value> {
    eval(result, "sent").as_array().unwrap().iter().filter(|r| r["method"] == method && r["path"] == path).map(|r| r["body"].clone()).collect()
}

const ASK: &str = "[data-ask-orq]";
const PANEL: &str = "[data-ask-panel]";

#[test]
fn the_sidebar_has_ask_orq_with_the_chat_icon_after_reviews_and_before_settings_and_only_the_icon_on_the_rail() {
    let names = "return Array.from(document.querySelectorAll('.sidebar nav > a, .sidebar nav > button')).map(function (e) { return e.textContent.trim(); });";
    let icon = "var b = document.querySelector('.sidebar nav [data-ask-orq]'); var svg = b && b.querySelector('svg'); return svg ? {d: svg.querySelector('path').getAttribute('d'), width: svg.getAttribute('stroke-width'), cap: svg.getAttribute('stroke-linecap'), box: svg.getAttribute('viewBox'), others: Array.from(document.querySelectorAll('.sidebar nav > a svg path')).map(function (p) { return p.getAttribute('d'); })} : null;";
    let tag = "var b = document.querySelector('.sidebar nav [data-ask-orq]'); return b ? [b.tagName, b.getAttribute('aria-expanded'), b.getAttribute('aria-label') || b.textContent.trim(), b.getAttribute('href')] : null;";
    let Some(result) = open(1440, "/projects/alpha/reviews", vec![], false, vec![json!({"wait": ASK}), json!({"eval": "names", "js": names}), json!({"eval": "icon", "js": icon}), json!({"eval": "tag", "js": tag})]) else { return };
    assert_eq!(eval(&result, "names"), &json!(["Dashboard", "Todos", "Features", "Orchestrations", "Queue", "Reviews", "Ask orq", "Settings"]), "{result}");
    let icon = eval(&result, "icon");
    assert!(icon["d"].as_str().is_some_and(|d| !d.is_empty()), "{icon}");
    assert!(!icon["others"].as_array().unwrap().contains(&icon["d"]), "its own icon: {icon}");
    assert_eq!((&icon["width"], &icon["cap"], &icon["box"]), (&json!("2"), &json!("round"), &json!("0 0 24 24")), "{icon}");
    assert_eq!(eval(&result, "tag"), &json!(["BUTTON", "false", "Ask orq", null]), "a toggle, not a link: {result}");

    let label = "var l = document.querySelector('.sidebar nav [data-ask-orq] .label'); return l ? getComputedStyle(l).display : null;";
    let Some(rail) = open(1440, "/projects/alpha/reviews", vec![], false, vec![json!({"click": "[data-rail-button]"}), json!({"wait": ".shell.collapsed"}), json!({"eval": "label", "js": label}), json!({"boxes": "[data-ask-orq] svg"})]) else { return };
    assert_eq!(eval(&rail, "label"), &json!("none"), "only the icon on the collapsed rail: {rail}");
    assert_eq!(rail["boxes"]["[data-ask-orq] svg"].as_array().map(Vec::len), Some(1), "{rail}");
}

#[test]
fn pressing_ask_orq_opens_the_panel_beside_the_page_without_leaving_it_and_marks_it() {
    let pill = "var a = document.querySelector('.sidebar nav [data-ask-orq]'); var c = document.querySelector('.sidebar nav a[aria-current=page]'); return {ask: getComputedStyle(a).backgroundColor, page: c ? getComputedStyle(c).backgroundColor : null, plain: getComputedStyle(document.querySelector('.sidebar nav a:not([aria-current])')).backgroundColor, expanded: a.getAttribute('aria-expanded')};";
    let steps = vec![json!({"click": ASK}), json!({"wait": PANEL}), json!({"eval": "pill", "js": pill}), json!({"boxes": PANEL}), json!({"boxes": "main.content"})];
    let Some(result) = open(1440, "/projects/alpha/reviews", vec![], false, steps) else { return };
    let pill = eval(&result, "pill");
    assert_eq!(pill["expanded"], "true", "{result}");
    assert_eq!(pill["ask"], pill["page"], "the open entry has the section pill, like the page's own: {pill}");
    assert_ne!(pill["ask"], pill["plain"], "{pill}");
    let address = eval(&result, "address").as_str().unwrap();
    assert!(address.starts_with("/projects/alpha/reviews?ask="), "the page stays and the address says the panel is open: {address}");
    let panel = &result["boxes"][PANEL][0];
    let main = &result["boxes"]["main.content"][0];
    assert!(panel["left"].as_i64() >= main["right"].as_i64(), "the panel sits beside the page: {result}");
    assert!(panel["right"].as_i64() <= Some(1440), "{result}");
}

#[test]
fn pressing_ask_orq_again_closes_the_panel() {
    let expanded = "return document.querySelector('.sidebar nav [data-ask-orq]').getAttribute('aria-expanded');";
    let steps = vec![json!({"click": ASK}), json!({"wait": PANEL}), json!({"click": ASK}), json!({"eval": "expanded", "js": expanded}), json!({"eval": "panel", "js": "return document.querySelectorAll('[data-ask-panel]').length;"})];
    let Some(result) = open(1440, "/projects/alpha/reviews", vec![], false, steps) else { return };
    assert_eq!(eval(&result, "expanded"), "false", "{result}");
    assert_eq!(eval(&result, "panel"), 0, "{result}");
    assert_eq!(eval(&result, "address"), "/projects/alpha/reviews", "{result}");
}

#[test]
fn on_a_phone_the_second_row_reads_todos_reviews_ask_orq_and_it_opens_a_full_screen_sheet_that_closes() {
    let row = "return Array.from(document.querySelectorAll('nav.tabbar > [data-tab-row=\"2\"] > *')).map(function (e) { return e.textContent.trim(); });";
    let Some(result) = open(390, "/projects/alpha/reviews", vec![], false, vec![json!({"wait": "nav.tabbar"}), json!({"eval": "row", "js": row}), json!({"click": "nav.tabbar [data-ask-orq]"}), json!({"wait": PANEL}), json!({"boxes": PANEL}), json!({"eval": "expanded", "js": "return document.querySelector('nav.tabbar [data-ask-orq]').getAttribute('aria-expanded');"})]) else { return };
    assert_eq!(eval(&result, "row"), &json!(["Todos", "Reviews", "Ask orq"]), "{result}");
    assert_eq!(eval(&result, "expanded"), "true", "{result}");
    let sheet = &result["boxes"][PANEL][0];
    assert_eq!((sheet["left"].as_i64(), sheet["top"].as_i64(), sheet["right"].as_i64(), sheet["bottom"].as_i64()), (Some(0), Some(0), Some(390), Some(800)), "a full-screen sheet: {result}");

    let Some(closed) = open(390, "/projects/alpha/reviews", vec![], false, vec![json!({"click": "nav.tabbar [data-ask-orq]"}), json!({"click": "[data-ask-close]"}), json!({"eval": "panel", "js": "return document.querySelectorAll('[data-ask-panel]').length;"})]) else { return };
    assert_eq!(eval(&closed, "panel"), 0, "the close button closes the sheet: {closed}");
    assert_eq!(eval(&closed, "address"), "/projects/alpha/reviews", "{closed}");
}

const CHAT: &str = "chat-aaaaaaaaaaaaaaaaaa";
const CHAT_PATH: &str = "/api/projects/alpha/chats/chat-aaaaaaaaaaaaaaaaaa";

/// A chat as `GET /chats/{chat}` reads it, with `items`, `rules` and whether a reply runs.
fn conversation(items: Vec<Value>, rules: Vec<Value>, running: bool) -> Value {
    json!({"chat": chat(CHAT, "Why did it fail", json!({"kind": "run", "target": "run-1795000000-1-1"}), running), "items": items, "rules": rules, "running": running})
}

fn finished_turn() -> Vec<Value> {
    vec![
        json!({"type": "user", "turn": 1, "text": "Why did it fail?", "state": "done", "reason": null}),
        json!({"type": "tool", "name": "Read", "input": "crates/orq-remote/web-src/src/main.jsx", "status": "ok", "result": "first line of the file\nsecond line"}),
        json!({"type": "assistant", "text": "It **failed** on `cargo test`.\n\n- one\n- two\n\n<script>window.__ran = 1</script>"}),
        json!({"type": "result", "cost": 0.01, "duration_ms": 12000, "error": null}),
    ]
}

#[test]
fn the_panel_lists_the_chats_and_shows_one_with_its_reply_as_markdown_and_its_tool_call_as_a_closed_line_that_opens() {
    let chats = json!({"chats": [chat(CHAT, "Why did it fail", json!({"kind": "run", "target": "run-1795000000-1-1"}), false), chat("chat-bbbbbbbbbbbbbbbbbb", "Queue question", json!({"kind": "queue"}), true)]});
    let routes = vec![route("GET", "/api/projects/alpha/chats", vec![reply(chats)]), route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))])];
    let list = "return Array.from(document.querySelectorAll('[data-ask-chat]')).map(function (e) { return {id: e.getAttribute('data-ask-chat'), text: e.textContent, running: e.querySelectorAll('[data-ask-running]').length}; });";
    let reply_js = "var a = document.querySelector('[data-ask-item=assistant]'); return {strong: a.querySelectorAll('strong').length, code: a.querySelectorAll('code').length, items: a.querySelectorAll('li').length, scripts: document.querySelectorAll('[data-ask-panel] script').length, shown: a.textContent.indexOf('<script>') >= 0, ran: window.__ran === 1, user: document.querySelector('[data-ask-item=user]').textContent};";
    let tool = "var t = document.querySelector('[data-step-ask-tool-toggle]'); return {label: t.textContent.trim(), expanded: t.getAttribute('aria-expanded'), body: document.querySelectorAll('[data-ask-tool-body]').length};";
    let opened = "var b = document.querySelector('[data-ask-tool-body]'); return b ? {text: b.textContent, overflow: getComputedStyle(b).overflowY} : null;";
    // Evals run once the steps are done, so each state is its own page load.
    let Some(listed) = open(1440, "/projects/alpha/reviews", routes.clone(), true, vec![json!({"click": ASK}), json!({"wait": "[data-ask-chat]"}), json!({"eval": "list", "js": list})]) else { return };
    let list = eval(&listed, "list").as_array().unwrap().clone();
    assert_eq!(list.len(), 2, "{listed}");
    let into = vec![json!({"click": ASK}), json!({"click": format!("[data-ask-chat={CHAT}]")}), json!({"wait": "[data-ask-item=assistant]"})];
    let mut steps = into.clone();
    steps.extend([json!({"eval": "reply", "js": reply_js}), json!({"eval": "tool", "js": tool})]);
    let Some(result) = open(1440, "/projects/alpha/reviews", routes.clone(), true, steps) else { return };
    let mut steps = into;
    steps.extend([json!({"click": "[data-step-ask-tool-toggle]"}), json!({"wait": "[data-ask-tool-body]"}), json!({"eval": "opened", "js": opened})]);
    let Some(opened) = open(1440, "/projects/alpha/reviews", routes, true, steps) else { return };
    let first = list[0]["text"].as_str().unwrap();
    assert!(first.contains("Why did it fail") && first.contains("run-1795000000-1-1"), "title and context: {first}");
    assert_eq!((list[0]["running"].as_i64(), list[1]["running"].as_i64()), (Some(0), Some(1)), "a running dot on the running chat: {listed}");
    assert!(list[1]["text"].as_str().unwrap().contains("Queue"), "{listed}");
    let shown = eval(&result, "reply");
    assert_eq!((shown["strong"].as_i64(), shown["items"].as_i64()), (Some(1), Some(2)), "the reply renders as Markdown: {shown}");
    assert!(shown["code"].as_i64().unwrap() >= 1, "{shown}");
    assert_eq!((shown["scripts"].as_i64(), shown["shown"].as_bool(), shown["ran"].as_bool()), (Some(0), Some(true), Some(false)), "a <script> shows as text: {shown}");
    assert_eq!(shown["user"], "Why did it fail?", "{shown}");
    let tool = eval(&result, "tool");
    assert_eq!((&tool["label"], &tool["expanded"], tool["body"].as_i64()), (&json!("Read main.jsx"), &json!("false"), Some(0)), "one closed line: {tool}");
    let opened = eval(&opened, "opened");
    let body = opened["text"].as_str().unwrap();
    assert!(body.contains("crates/orq-remote/web-src/src/main.jsx") && body.contains("second line"), "opens to its input and result: {opened}");
    assert_eq!(opened["overflow"], "auto", "{opened}");
    let address = eval(&result, "address").as_str().unwrap();
    assert!(address.ends_with(&format!("?ask={CHAT}")), "{address}");
    assert!(text(&result).contains("12s") || text(&result).contains("12 s"), "the turn's end shows its duration: {result}");
}

/// One server-sent event of the chat's stream.
fn event(text: &str, after: u64) -> Value {
    json!({"after": after, "text": text})
}

fn item_event(index: usize, item: Value, after: u64) -> Value {
    event(&format!("id: {index}\nevent: item\ndata: {}\n\n", json!({"index": index, "item": item})), after)
}

#[test]
fn a_running_reply_streams_in_without_a_reload_and_stop_asks_first_then_shows_the_turn_stopped() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let asked = json!({"type": "user", "turn": 1, "text": "What is it doing now?", "state": "running", "reason": null});
    let stopped = json!({"type": "user", "turn": 1, "text": "What is it doing now?", "state": "stopped", "reason": "stopped from the web"});
    let stream = json!({"status": 200, "hold": true, "stream": [event(": connected\n\n", 0), item_event(0, asked.clone(), 0), item_event(1, json!({"type": "assistant", "text": "Reading the run's log"}), 400)]});
    let routes = vec![
        route("GET", CHAT_PATH, vec![reply(conversation(vec![asked.clone()], vec![], true)), reply(conversation(vec![stopped, json!({"type": "assistant", "text": "Reading the run's log"})], vec![], false))]),
        route("GET", &format!("{CHAT_PATH}/stream"), vec![stream]),
        route("POST", &format!("{CHAT_PATH}/stop"), vec![reply(json!({"turn": 1, "state": "stopped"}))]),
    ];
    let state = "var i = document.querySelector('[data-ask-input]'); var w = document.querySelector('[data-ask-working]'); var r = document.querySelector('[data-ask-box-reason]'); var a = document.querySelector('[data-ask-item=assistant]'); return {disabled: i ? i.disabled : null, working: w ? w.textContent : null, reason: r ? r.textContent : null, reply: a ? a.textContent : null, confirm: document.querySelectorAll('[data-ask-panel] [data-confirm]').length};";
    let Some(streamed) = open(1440, &at, routes.clone(), true, vec![json!({"wait": "[data-ask-item=assistant]"}), json!({"eval": "state", "js": state})]) else { return };
    let seen = eval(&streamed, "state");
    assert_eq!(seen["reply"], "Reading the run's log", "the streamed item appears: {streamed}");
    assert_eq!(sent(&streamed, "GET", CHAT_PATH).len(), 1, "without reading the chat again: {streamed}");
    assert!(seen["working"].as_str().is_some_and(|w| w.contains("orq is working")), "{seen}");
    assert_eq!(seen["disabled"], true, "the box is disabled while orq works: {seen}");
    assert!(seen["reason"].as_str().is_some_and(|r| !r.is_empty()), "with the reason: {seen}");

    let Some(asking) = open(1440, &at, routes.clone(), true, vec![json!({"click": "[data-ask-stop]"}), json!({"wait": "[data-ask-panel] [data-confirm]"}), json!({"eval": "state", "js": state})]) else { return };
    assert_eq!(eval(&asking, "state")["confirm"], 1, "Stop asks first: {asking}");
    assert!(sent(&asking, "POST", &format!("{CHAT_PATH}/stop")).is_empty(), "nothing is stopped before the answer: {asking}");

    let steps = vec![json!({"click": "[data-ask-stop]"}), json!({"click": "[data-ask-panel] [data-confirm]"}), json!({"wait": "[data-ask-turn-end]"}), json!({"eval": "end", "js": "return document.querySelector('[data-ask-turn-end]').textContent;"})];
    let Some(ended) = open(1440, &at, routes, true, steps) else { return };
    assert_eq!(sent(&ended, "POST", &format!("{CHAT_PATH}/stop")), vec![json!({})], "{ended}");
    let end = eval(&ended, "end").as_str().unwrap();
    assert!(end.contains("Stopped") && end.contains("stopped from the web"), "the turn shows it was stopped and why: {end}");
}

const RUN: &str = "run-1795000000-1-1";
const NEW_CHAT: &str = "chat-cccccccccccccccccc";

/// The run's page reads it as failed (or `state`), for the feature home-v02-assets.
fn run_route(state: &str) -> Value {
    route("GET", &format!("/api/projects/alpha/runs/{RUN}"), vec![reply(json!({"run": {"run_id": RUN, "feature": "home-v02-assets", "state": state, "stage": "implement"}, "events": [], "next_cursor": null}))])
}

const DRAFT: &str = "var i = document.querySelector('[data-ask-input]'); var a = document.querySelector('[data-ask-about]'); return {message: i ? i.value : null, about: a ? a.textContent : null};";

/// What New chat offers on the page at `path`: the suggested message and the header.
fn new_chat_on(path: &str, routes: Vec<Value>) -> Option<Value> {
    let result = open(1440, path, routes, true, vec![json!({"click": ASK}), json!({"click": "[data-ask-new]"}), json!({"wait": "[data-ask-input]"}), json!({"eval": "draft", "js": DRAFT})])?;
    Some(eval(&result, "draft").clone())
}

#[test]
fn new_chat_follows_the_page_it_was_opened_on_with_an_editable_suggested_message() {
    let run_page = format!("/projects/alpha/orchestrations/{RUN}");
    let Some(failed) = new_chat_on(&run_page, vec![run_route("failed")]) else { return };
    assert_eq!(failed["message"], "What happened, and what should I do?", "{failed}");
    assert_eq!(failed["about"], format!("About {RUN} · home-v02-assets"), "{failed}");
    let Some(running) = new_chat_on(&run_page, vec![run_route("running")]) else { return };
    assert_eq!(running["message"], "What is it doing now?", "{running}");
    let Some(feature) = new_chat_on("/projects/alpha/features/home-v02-assets", vec![]) else { return };
    assert_eq!((&feature["message"], &feature["about"]), (&json!("What is the state of this feature?"), &json!("About home-v02-assets")), "{feature}");
    let Some(queue) = new_chat_on("/projects/alpha/queue", vec![]) else { return };
    assert_eq!((&queue["message"], &queue["about"]), (&json!("Why isn't the queue moving?"), &json!("About the queue")), "{queue}");
    let Some(other) = new_chat_on("/projects/alpha/reviews", vec![]) else { return };
    assert_eq!((&other["message"], &other["about"]), (&json!(""), &json!("About alpha")), "the project: {other}");

    // The message can be changed before it is sent; the chat is made about the run, then opened.
    let made = chat(NEW_CHAT, "Why did it stop", json!({"kind": "run", "target": RUN}), false);
    let routes = vec![
        run_route("failed"),
        route("POST", "/api/projects/alpha/chats", vec![reply(json!({"chat": made}))]),
        route("POST", &format!("/api/projects/alpha/chats/{NEW_CHAT}/messages"), vec![json!({"status": 202, "body": {"turn": 1}})]),
        route("GET", &format!("/api/projects/alpha/chats/{NEW_CHAT}"), vec![json!({"status": 200, "body": {"chat": made, "items": [], "rules": [], "running": false}})]),
    ];
    let steps = vec![json!({"click": ASK}), json!({"click": "[data-ask-new]"}), json!({"wait": "[data-ask-input]"}), json!({"type": "[data-ask-input]", "value": "Why did it stop?"}), json!({"click": "[data-ask-send]"}), json!({"wait": "[data-ask-conversation]"})];
    let Some(sent_chat) = open(1440, &run_page, routes, true, steps) else { return };
    let created = sent(&sent_chat, "POST", "/api/projects/alpha/chats");
    assert_eq!(created.len(), 1, "{sent_chat}");
    assert_eq!(created[0]["context"], json!({"kind": "run", "target": RUN}), "{sent_chat}");
    assert_eq!(sent(&sent_chat, "POST", &format!("/api/projects/alpha/chats/{NEW_CHAT}/messages")), vec![json!({"text": "Why did it stop?"})], "{sent_chat}");
    assert!(eval(&sent_chat, "address").as_str().unwrap().ends_with(&format!("?ask={NEW_CHAT}")), "{sent_chat}");
}

#[test]
fn a_new_chat_keeps_the_page_it_was_begun_on_and_the_persons_words_while_they_move_to_another_page() {
    let run_page = format!("/projects/alpha/orchestrations/{RUN}");
    let steps = vec![
        json!({"click": ASK}),
        json!({"click": "[data-ask-new]"}),
        json!({"wait": "[data-ask-about]", "text": format!("About {RUN} · ")}),
        json!({"type": "[data-ask-input]", "value": "My own question"}),
        json!({"click": ".sidebar nav a[href='/projects/alpha/queue']"}),
        json!({"wait": "main.content h1", "text": "Queue"}),
        json!({"wait": "[data-ask-input]"}),
        json!({"eval": "draft", "js": DRAFT}),
    ];
    let Some(result) = open(1440, &run_page, vec![run_route("failed")], true, steps) else { return };
    assert_eq!(eval(&result, "draft"), &json!({"message": "My own question", "about": format!("About {RUN} · home-v02-assets")}), "{result}");
}

#[test]
fn a_chat_made_whose_first_message_fails_keeps_the_words_and_the_retry_goes_to_that_chat_without_making_another() {
    let run_page = format!("/projects/alpha/orchestrations/{RUN}");
    let made = chat(NEW_CHAT, "Why did it stop", json!({"kind": "run", "target": RUN}), false);
    let messages = format!("/api/projects/alpha/chats/{NEW_CHAT}/messages");
    let routes = || {
        vec![
            two_projects(),
            run_route("failed"),
            route("GET", "/api/projects/beta/chats", vec![reply(json!({"chats": []}))]),
            route("POST", "/api/projects/alpha/chats", vec![reply(json!({"chat": made}))]),
            route("POST", &messages, vec![json!({"status": 500, "body": {"error": "The machine is busy."}}), json!({"status": 202, "body": {"turn": 1}})]),
            route("GET", &format!("/api/projects/alpha/chats/{NEW_CHAT}"), vec![reply(json!({"chat": made, "items": [], "rules": [], "running": false}))]),
        ]
    };
    let failed = "var n = document.querySelector('.ask-composer-wrap [role=alert]'); var i = document.querySelector('[data-ask-input]'); return {note: n ? n.textContent : null, message: i ? i.value : null};";
    let first = vec![
        json!({"click": ASK}),
        json!({"click": "[data-ask-new]"}),
        json!({"wait": "[data-ask-input]"}),
        json!({"type": "[data-ask-input]", "value": "Why did it stop?"}),
        json!({"click": "[data-ask-send]"}),
        json!({"wait": ".ask-composer-wrap [role=alert]", "text": "The machine is busy."}),
        json!({"eval": "failed", "js": failed}),
    ];
    // Evals run once the steps are done, so the failure is looked at in a run that stops there.
    let Some(failure) = open(1440, &run_page, routes(), true, first.clone()) else { return };
    assert_eq!(eval(&failure, "failed"), &json!({"note": "The machine is busy.", "message": "Why did it stop?"}), "the reason shows and the words stay: {failure}");
    let mut steps = first;
    steps.extend([json!({"click": "[data-ask-send]"}), json!({"wait": "[data-ask-conversation]"})]);
    let Some(retried) = open(1440, &run_page, routes(), true, steps) else { return };
    assert_eq!(sent(&retried, "POST", "/api/projects/alpha/chats").len(), 1, "one chat is made: {retried}");
    assert_eq!(sent(&retried, "POST", &messages), vec![json!({"text": "Why did it stop?"}), json!({"text": "Why did it stop?"})], "the retry goes to the chat made: {retried}");
    assert!(eval(&retried, "address").as_str().unwrap().ends_with(&format!("?ask={NEW_CHAT}")), "{retried}");

    // Moving to another project's page leaves the draft in alpha: nothing is made or sent in beta.
    let leave = move_when(".ask-composer-wrap [role=alert]", "/projects/beta/reviews");
    let steps = vec![
        json!({"click": ASK}),
        json!({"click": "[data-ask-new]"}),
        json!({"wait": "[data-ask-input]"}),
        json!({"type": "[data-ask-input]", "value": "Why did it stop?"}),
        json!({"click": "[data-ask-send]"}),
        json!({"wait": "main.content h1", "text": "Reviews"}),
        json!({"wait": "[data-ask-new]"}),
        json!({"eval": "left", "js": "return {inputs: document.querySelectorAll('[data-ask-input]').length, alerts: document.querySelectorAll('.ask-composer-wrap [role=alert]').length};"}),
    ];
    let Some(moved) = open_after(1440, &run_page, routes(), true, steps, &leave) else { return };
    assert_eq!(eval(&moved, "address"), "/projects/beta/reviews?ask=", "{moved}");
    assert_eq!(eval(&moved, "left"), &json!({"inputs": 0, "alerts": 0}), "beta's chat list, not alpha's draft: {moved}");
    let posts: Vec<Value> = eval(&moved, "sent").as_array().unwrap().iter().filter(|r| r["method"] == "POST" && r["path"].as_str().unwrap().starts_with("/api/projects/beta")).cloned().collect();
    assert!(posts.is_empty(), "nothing is made or sent in beta: {moved}");
}

#[test]
fn outside_a_project_the_panel_asks_which_project_and_remembers_the_last_one() {
    let two = route("GET", "/api/projects", vec![reply(json!({"projects": [project(), {"id": "beta", "name": "beta", "available": true, "features": 0, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}))]);
    let beta_chats = route("GET", "/api/projects/beta/chats", vec![reply(json!({"chats": []}))]);
    let choices = "return Array.from(document.querySelectorAll('[data-ask-project]')).map(function (b) { return b.getAttribute('data-ask-project'); });";
    let Some(asking) = open(1440, "/dashboard", vec![two.clone(), beta_chats.clone()], true, vec![json!({"click": ASK}), json!({"wait": "[data-ask-project]"}), json!({"eval": "choices", "js": choices})]) else { return };
    assert_eq!(eval(&asking, "choices"), &json!(["alpha", "beta"]), "{asking}");
    let steps = vec![json!({"click": ASK}), json!({"click": "[data-ask-project=beta]"}), json!({"wait": "[data-ask-new]"}), json!({"eval": "kept", "js": "return localStorage.getItem('orq.ask.project');"})];
    let Some(asked) = open(1440, "/dashboard", vec![two.clone(), beta_chats.clone()], true, steps) else { return };
    assert_eq!(eval(&asked, "kept"), "beta", "{asked}");
    assert_eq!(sent(&asked, "GET", "/api/projects/beta/chats").len(), 1, "{asked}");

    let steps = vec![json!({"click": ASK}), json!({"click": "[data-ask-new]"}), json!({"wait": "[data-ask-input]"}), json!({"eval": "draft", "js": DRAFT})];
    let Some(again) = open_after(1440, "/settings", vec![two, beta_chats], true, steps, "try { localStorage.setItem('orq.ask.project', 'beta'); } catch (e) {}\n") else { return };
    assert_eq!(sent(&again, "GET", "/api/projects/beta/chats").len(), 1, "the last project is remembered: {again}");
    assert_eq!(eval(&again, "draft")["about"], "About beta", "{again}");
}

#[test]
fn the_panel_stays_open_with_its_chat_across_pages_and_the_page_beside_it_still_scrolls() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let routes = vec![route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))])];
    // A long page, scrolled while the panel is open: the page moves, the panel stays in view.
    let scroll = "var tall = document.createElement('div'); tall.style.height = '3000px'; document.querySelector('main.content').appendChild(tall); window.scrollTo(0, 600); var p = document.querySelector('[data-ask-panel]').getBoundingClientRect(); return {y: Math.round(window.scrollY), top: Math.round(p.top), bottom: Math.round(p.bottom)};";
    let steps = vec![
        json!({"wait": "[data-ask-item=assistant]"}),
        json!({"click": ".sidebar nav a[href='/projects/alpha/queue']"}),
        json!({"wait": "main.content h1", "text": "Queue"}),
        json!({"eval": "after", "js": "var a = document.querySelector('[data-ask-item=assistant]'); return {reply: a ? a.textContent : null, title: document.querySelector('main.content h1').textContent};"}),
        json!({"eval": "scroll", "js": scroll}),
    ];
    let Some(result) = open(1440, &at, routes, true, steps) else { return };
    assert_eq!(eval(&result, "address"), &json!(format!("/projects/alpha/queue?ask={CHAT}")), "the address keeps the open chat: {result}");
    let after = eval(&result, "after");
    assert_eq!(after["title"], "Queue", "the page beside the panel works: {result}");
    assert!(after["reply"].as_str().is_some_and(|r| r.contains("failed")), "the chat stays open: {result}");
    assert_eq!(sent(&result, "GET", CHAT_PATH).len(), 1, "without reading the chat again: {result}");
    let scroll = eval(&result, "scroll");
    assert_eq!((scroll["y"].as_i64(), scroll["top"].as_i64(), scroll["bottom"].as_i64()), (Some(600), Some(0), Some(800)), "the page scrolls and the panel stays: {scroll}");
}

#[test]
fn with_no_ask_orq_agent_the_panel_points_to_settings_and_settings_saves_the_agent() {
    let none = route("GET", "/api/settings", vec![reply(settings(false, true))]);
    let reason = "var r = document.querySelector('[data-ask-no-agent]'); var a = document.querySelector('[data-ask-settings]'); return {reason: r ? r.textContent : null, href: a ? a.getAttribute('href') : null, new: document.querySelectorAll('[data-ask-new]').length, box: document.querySelectorAll('[data-ask-composer]').length};";
    let Some(listed) = open(1440, "/projects/alpha/reviews", vec![none.clone()], true, vec![json!({"click": ASK}), json!({"wait": "[data-ask-settings]"}), json!({"eval": "reason", "js": reason})]) else { return };
    let shown = eval(&listed, "reason");
    assert!(shown["reason"].as_str().is_some_and(|r| r.contains("Set the Ask orq agent in Settings")), "{listed}");
    assert_eq!((&shown["href"], shown["new"].as_i64()), (&json!("/settings"), Some(0)), "a link instead of New chat: {listed}");
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let chat_routes = vec![none.clone(), route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))])];
    let Some(opened) = open(1440, &at, chat_routes, true, vec![json!({"wait": "[data-ask-item=assistant]"}), json!({"wait": "[data-ask-settings]"}), json!({"eval": "reason", "js": reason})]) else { return };
    assert_eq!(eval(&opened, "reason")["box"], 0, "the reason instead of the box: {opened}");
    let Some(followed) = open(1440, "/projects/alpha/reviews", vec![none.clone()], true, vec![json!({"click": ASK}), json!({"click": "[data-ask-settings]"}), json!({"wait": "[data-agent-edit=ask_orq_agent]"})]) else { return };
    assert_eq!(eval(&followed, "address"), "/settings?ask=", "{followed}");

    let options = route("GET", "/api/settings/options", vec![reply(json!({"options": [{"harness": "claude", "models": ["claude-opus-5-5", "claude-sonnet-5-5"], "efforts": ["low", "high"]}, {"harness": "codex", "models": ["gpt-6.1"]}]}))]);
    let saved = route("POST", "/api/settings/agents", vec![reply(settings(true, true))]);
    let harnesses = "return Array.from(document.querySelectorAll('[data-agent-harness] option')).map(function (o) { return o.value; });";
    let steps = vec![json!({"click": "[data-agent-edit=ask_orq_agent]"}), json!({"wait": "[data-agent-harness]"}), json!({"click": "[data-agent-save]"}), json!({"click": "[data-confirm]"}), json!({"wait": "[data-agent-edit=ask_orq_agent]"})];
    let mut after = settings(true, true);
    after["settings"]["version"] = json!("v-2");
    let reread = route("GET", "/api/settings", vec![reply(settings(false, true)), reply(after)]);
    let Some(set) = open(1440, "/settings", vec![reread, options, saved], true, steps) else { return };
    assert!(text(&set).contains("Ask orq agent"), "{set}");
    assert_eq!(sent(&set, "POST", "/api/settings/agents"), vec![json!({"ask_orq_agent": {"harness": "claude", "model": "claude-opus-5-5", "effort": null}, "version": "v-1"})], "{set}");
    let steps = vec![json!({"click": "[data-agent-edit=ask_orq_agent]"}), json!({"wait": "[data-agent-harness]"}), json!({"eval": "harnesses", "js": harnesses})];
    let Some(picking) = open(1440, "/settings", vec![route("GET", "/api/settings", vec![reply(settings(false, true))]), route("GET", "/api/settings/options", vec![reply(json!({"options": [{"harness": "claude", "models": ["claude-opus-5-5"]}, {"harness": "codex", "models": ["gpt-6.1"]}]}))])], true, steps) else { return };
    assert_eq!(eval(&picking, "harnesses"), &json!(["claude"]), "only Claude models: {picking}");
}

#[test]
fn setting_the_ask_orq_agent_on_the_settings_page_unlocks_the_open_panel() {
    let options = route("GET", "/api/settings/options", vec![reply(json!({"options": [{"harness": "claude", "models": ["claude-opus-5-5"]}]}))]);
    let saved = route("POST", "/api/settings/agents", vec![reply(settings(true, true))]);
    // The page and the panel each read the settings once with no agent; every read after the save has one.
    let reads = route("GET", "/api/settings", vec![reply(settings(false, true)), reply(settings(false, true)), reply(settings(true, true))]);
    let steps = vec![
        json!({"wait": "[data-ask-no-agent]"}),
        json!({"click": "[data-agent-edit=ask_orq_agent]"}),
        json!({"wait": "[data-agent-harness]"}),
        json!({"click": "[data-agent-save]"}),
        json!({"click": "[data-confirm]"}),
        json!({"wait": "[data-ask-new]"}),
        json!({"eval": "gate", "js": "return document.querySelectorAll('[data-ask-no-agent]').length;"}),
    ];
    let Some(result) = open_after(1440, "/settings?ask=", vec![reads, options, saved], true, steps, "try { localStorage.setItem('orq.ask.project', 'alpha'); } catch (e) {}\n") else { return };
    assert_eq!(eval(&result, "gate"), 0, "the panel no longer says to set the agent: {result}");
}

#[test]
fn a_token_only_session_sees_the_passkey_reason_and_with_remote_actions_off_the_box_is_the_reason() {
    let reason = "var p = document.querySelector('[data-ask-passkey]'); var r = document.querySelector('[data-ask-remote-off]'); return {passkey: p ? p.textContent : null, off: r ? r.textContent : null, box: document.querySelectorAll('[data-ask-composer]').length, new: document.querySelectorAll('[data-ask-new]').length};";
    let Some(token) = open(1440, "/projects/alpha/reviews", vec![], false, vec![json!({"click": ASK}), json!({"wait": "[data-ask-passkey]"}), json!({"eval": "reason", "js": reason})]) else { return };
    let shown = eval(&token, "reason");
    assert!(shown["passkey"].as_str().is_some_and(|p| p.contains("passkey")), "{token}");
    assert_eq!((shown["box"].as_i64(), shown["new"].as_i64()), (Some(0), Some(0)), "{token}");
    assert!(sent(&token, "GET", "/api/projects/alpha/chats").is_empty(), "the chats are not read without a passkey session: {token}");

    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let routes = vec![route("GET", "/api/settings", vec![reply(settings(true, false))]), route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))])];
    let Some(off) = open(1440, &at, routes, true, vec![json!({"wait": "[data-ask-item=assistant]"}), json!({"wait": "[data-ask-remote-off]"}), json!({"eval": "reason", "js": reason})]) else { return };
    let shown = eval(&off, "reason");
    assert!(shown["off"].as_str().is_some_and(|r| r.contains("Remote actions are off")), "{off}");
    assert_eq!(shown["box"], 0, "the reason instead of the box: {off}");
}

#[test]
fn at_390_and_1440_px_a_300_character_line_in_a_reply_and_a_code_block_never_scrolls_the_page_sideways() {
    let long = "x".repeat(300);
    let items = vec![
        json!({"type": "user", "turn": 1, "text": long.clone(), "state": "done", "reason": null}),
        json!({"type": "assistant", "text": format!("{long}\n\n```\n{long}\n```")}),
        json!({"type": "result", "cost": 0.01, "duration_ms": 1000, "error": null}),
    ];
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let routes = vec![route("GET", CHAT_PATH, vec![reply(conversation(items, vec![], false))])];
    let fits = "var pre = document.querySelector('[data-ask-item=assistant] pre'); var c = document.querySelector('[data-ask-composer]'); var p = document.querySelector('[data-ask-panel]').getBoundingClientRect(); return {page: document.documentElement.scrollWidth, pre: pre ? {inner: pre.scrollWidth > pre.clientWidth, overflow: getComputedStyle(pre).overflowX, right: Math.round(pre.getBoundingClientRect().right)} : null, composer: getComputedStyle(c).position, panelRight: Math.round(p.right)};";
    for width in [390, 1440] {
        let Some(result) = open(width, &at, routes.clone(), true, vec![json!({"wait": "[data-ask-item=assistant] pre"}), json!({"eval": "fits", "js": fits})]) else { return };
        let fits = eval(&result, "fits");
        assert_eq!(fits["page"].as_i64(), Some(width as i64), "no sideways scroll at {width}: {result}");
        assert_eq!((fits["pre"]["inner"].as_bool(), &fits["pre"]["overflow"]), (Some(true), &json!("auto")), "the code block scrolls inside at {width}: {fits}");
        assert!(fits["pre"]["right"].as_i64() <= fits["panelRight"].as_i64(), "{fits}");
        assert_eq!(fits["composer"], "static", "the box is in the normal flow at {width}: {fits}");
    }
}

/// Presses Enter (with Shift when `shift`) in the message box after the next `type` into it, recording in
/// `window.__pressed` whether the box took the key (its default was prevented).
fn press_after_typing(shift: bool) -> String {
    format!("window.__pressed = []; document.addEventListener('input', function (e) {{ if (!e.target.matches || !e.target.matches('[data-ask-input]') || window.__pressed.length > 0) return; var box = e.target; setTimeout(function () {{ window.__pressed.push(!box.dispatchEvent(new KeyboardEvent('keydown', {{key: 'Enter', shiftKey: {shift}, bubbles: true, cancelable: true}}))); }}, 50); }}, true);\n")
}

#[test]
fn enter_sends_on_desktop_shift_enter_and_a_phones_enter_are_new_lines_and_the_box_grows_to_8_lines() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let messages = format!("{CHAT_PATH}/messages");
    let routes = vec![route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))]), route("POST", &messages, vec![json!({"status": 202, "body": {"turn": 2}})])];
    let typed = |text: &str| vec![json!({"wait": "[data-ask-input]"}), json!({"type": "[data-ask-input]", "value": text}), json!({"wait": "[data-ask-composer]"}), json!({"eval": "pressed", "js": "return window.__pressed;"})];
    let Some(enter) = open_after(1440, &at, routes.clone(), true, typed("Hello"), &press_after_typing(false)) else { return };
    assert_eq!(eval(&enter, "pressed"), &json!([true]), "{enter}");
    assert_eq!(sent(&enter, "POST", &messages), vec![json!({"text": "Hello"})], "Enter sends on desktop: {enter}");
    let Some(shifted) = open_after(1440, &at, routes.clone(), true, typed("Hello"), &press_after_typing(true)) else { return };
    assert_eq!(eval(&shifted, "pressed"), &json!([false]), "Shift+Enter is a new line: {shifted}");
    assert!(sent(&shifted, "POST", &messages).is_empty(), "{shifted}");
    let Some(phone) = open_after(390, &at, routes.clone(), true, typed("Hello"), &press_after_typing(false)) else { return };
    assert_eq!(eval(&phone, "pressed"), &json!([false]), "Enter is a new line on a phone: {phone}");
    assert!(sent(&phone, "POST", &messages).is_empty(), "the button sends on a phone: {phone}");

    let size = "var b = document.querySelector('[data-ask-input]'); var line = parseFloat(getComputedStyle(b).lineHeight); return {height: b.clientHeight, scroll: b.scrollHeight, line: line};";
    let lines = |n: usize| (1..=n).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
    let grown = |n: usize| vec![json!({"wait": "[data-ask-input]"}), json!({"type": "[data-ask-input]", "value": lines(n)}), json!({"wait": "[data-ask-input]"}), json!({"eval": "size", "js": size})];
    let Some(one) = open(1440, &at, routes.clone(), true, vec![json!({"wait": "[data-ask-input]"}), json!({"eval": "size", "js": size})]) else { return };
    let Some(three) = open(1440, &at, routes.clone(), true, grown(3)) else { return };
    let Some(twenty) = open(1440, &at, routes, true, grown(20)) else { return };
    let (one, three, twenty) = (eval(&one, "size"), eval(&three, "size"), eval(&twenty, "size"));
    let line = three["line"].as_f64().unwrap();
    assert!(three["height"].as_f64().unwrap() >= one["height"].as_f64().unwrap() + 1.5 * line, "the box grows with its lines: {one} {three}");
    assert!(three["scroll"].as_i64() <= three["height"].as_i64(), "three lines show without scrolling: {three}");
    let most = twenty["height"].as_f64().unwrap();
    assert!(most >= 7.5 * line && most <= 9.5 * line + 24.0, "it stops at about 8 lines: {twenty}");
    assert!(twenty["scroll"].as_i64() > twenty["height"].as_i64(), "and scrolls past them: {twenty}");
}

#[test]
fn the_conversation_stays_pinned_to_its_newest_item_unless_the_person_scrolled_up() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let mut items = vec![json!({"type": "user", "turn": 1, "text": "Tell me everything", "state": "running", "reason": null})];
    items.extend((0..40).map(|i| json!({"type": "assistant", "text": format!("Paragraph {i} of the answer, long enough to take a line or two in the panel.")})));
    let late = json!({"type": "assistant", "text": "The late part"});
    let stream = json!({"status": 200, "hold": true, "stream": [event(": connected\n\n", 0), item_event(41, late, 1500)]});
    let routes = vec![route("GET", CHAT_PATH, vec![reply(conversation(items, vec![], true))]), route("GET", &format!("{CHAT_PATH}/stream"), vec![stream])];
    // The scroller is the conversation beside the page, and the whole sheet on a phone.
    let place = "var c = document.querySelector('[data-ask-conversation]'); var s = getComputedStyle(c).overflowY === 'visible' ? document.querySelector('[data-ask-panel]') : c; return {top: Math.round(s.scrollTop), bottom: Math.round(s.scrollHeight - s.scrollTop - s.clientHeight), tall: s.scrollHeight > s.clientHeight + 100};";
    let steps = vec![json!({"wait": "[data-ask-item=assistant]", "text": "The late part"}), json!({"eval": "place", "js": place})];
    for width in [1440, 390] {
        let Some(pinned) = open(width, &at, routes.clone(), true, steps.clone()) else { return };
        let place = eval(&pinned, "place");
        assert_eq!(place["tall"], true, "{pinned}");
        assert!(place["bottom"].as_i64().unwrap() <= 2, "the newest item is in view at {width}: {place}");
    }
    let scroll_up = "window.__up = setInterval(function () { var c = document.querySelector('[data-ask-conversation]'); if (!c || c.scrollHeight <= c.clientHeight + 100 || c.scrollTop === 0) return; c.scrollTop = 0; c.dispatchEvent(new Event('scroll')); clearInterval(window.__up); }, 100);\n";
    let Some(up) = open_after(1440, &at, routes, true, steps, scroll_up) else { return };
    assert_eq!(eval(&up, "place")["top"], 0, "a person who scrolled up stays where they are: {up}");
}

#[test]
fn a_chat_read_that_answers_after_the_stream_moved_on_does_not_take_back_what_the_stream_showed() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let asked = json!({"type": "user", "turn": 1, "text": "What is it doing now?", "state": "running", "reason": null});
    let done = json!({"type": "user", "turn": 1, "text": "What is it doing now?", "state": "done", "reason": null});
    // The stream says the turn is over before the (older) read of the chat answers that it is still running.
    let stream = json!({"status": 200, "hold": true, "stream": [event(": connected\n\n", 0), item_event(0, done, 0), item_event(1, json!({"type": "assistant", "text": "It finished"}), 0), event("event: state\ndata: {\"running\": false}\n\n", 100)]});
    let late = json!({"status": 200, "after": 800, "body": conversation(vec![asked], vec![], true)});
    let routes = vec![route("GET", CHAT_PATH, vec![late]), route("GET", &format!("{CHAT_PATH}/stream"), vec![stream])];
    // Looked at well after the read answered, then marked so the steps know.
    let look = "setTimeout(function () { var a = document.querySelector('[data-ask-item=assistant]'); window.__late = {reply: a ? a.textContent : null, working: document.querySelectorAll('[data-ask-working]').length}; document.body.setAttribute('data-late', ''); }, 1800);\n";
    let steps = vec![json!({"wait": "[data-ask-delete]"}), json!({"wait": "body[data-late]"}), json!({"eval": "late", "js": "return window.__late;"})];
    let Some(result) = open_after(1440, &at, routes, true, steps, look) else { return };
    assert_eq!(eval(&result, "late"), &json!({"reply": "It finished", "working": 0}), "{result}");
}

#[test]
fn a_refused_stream_signs_out_on_a_401_and_otherwise_says_the_live_updates_stopped_and_why() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let read = route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))]);
    let stream = |answer: Value| route("GET", &format!("{CHAT_PATH}/stream"), vec![answer]);
    // Marked a while after the chat shows, so the stream has answered.
    let settle = "setTimeout(function () { document.body.setAttribute('data-late', ''); }, 1200);\n";
    let note = "var n = document.querySelector('[data-ask-stream-note]'); return n ? n.textContent : null;";
    let steps = vec![json!({"wait": "[data-ask-item=assistant]"}), json!({"wait": "body[data-late]"}), json!({"eval": "note", "js": note})];
    let open_stream = json!({"status": 200, "hold": true, "stream": [event(": connected\n\n", 0)]});
    let Some(live) = open_after(1440, &at, vec![read.clone(), stream(open_stream)], true, steps.clone(), settle) else { return };
    let unauthorized = json!({"status": 401, "body": {"error": "sign in again"}});
    let Some(refused) = open_after(1440, &at, vec![read.clone(), stream(unauthorized)], true, steps.clone(), settle) else { return };
    assert_eq!(eval(&live, "note"), &Value::Null, "{live}");
    let asked = |result: &Value| sent(result, "GET", "/auth/status").len();
    assert_eq!(asked(&refused), asked(&live) + 1, "a 401 signs out, which asks who is signed in again: {refused}");

    let gone = json!({"status": 404, "body": {"error": "this chat is gone"}});
    let Some(stopped) = open_after(1440, &at, vec![read, stream(gone)], true, steps, settle) else { return };
    assert_eq!(eval(&stopped, "note"), "Live updates stopped: this chat is gone", "{stopped}");
}

/// Run in the page before the app loads: once `selector` shows, the page moves to `path` as a link would.
fn move_when(selector: &str, path: &str) -> String {
    format!("window.__moved = setInterval(function () {{ if (!document.querySelector({selector:?})) return; clearInterval(window.__moved); history.pushState(null, '', {path:?}); window.dispatchEvent(new Event('orq:navigate')); }}, 100);\n")
}

fn two_projects() -> Value {
    let beta = json!({"id": "beta", "name": "beta", "available": true, "features": 0, "active_runs": 0, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    route("GET", "/api/projects", vec![reply(json!({"projects": [project(), beta]}))])
}

#[test]
fn moving_to_another_projects_page_leaves_the_open_chat_and_lists_that_projects_chats() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let routes = vec![two_projects(), route("GET", "/api/projects/beta/chats", vec![reply(json!({"chats": []}))]), route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))])];
    let shown = "return {items: document.querySelectorAll('[data-ask-item]').length, about: document.querySelectorAll('[data-ask-about]').length};";
    let steps = vec![json!({"wait": "main.content h1", "text": "Reviews"}), json!({"wait": "[data-ask-new]"}), json!({"eval": "shown", "js": shown})];
    let Some(result) = open_after(1440, &at, routes, true, steps, &move_when("[data-ask-item=assistant]", "/projects/beta/reviews")) else { return };
    assert_eq!(eval(&result, "address"), "/projects/beta/reviews?ask=", "{result}");
    assert_eq!(eval(&result, "shown"), &json!({"items": 0, "about": 0}), "nothing of alpha's chat stays: {result}");
    assert!(sent(&result, "GET", "/api/projects/beta/chats/chat-aaaaaaaaaaaaaaaaaa").is_empty(), "alpha's chat is not asked of beta: {result}");
    assert_eq!(sent(&result, "GET", "/api/projects/beta/chats").len(), 1, "beta's chats are listed: {result}");
}

const PYTHON_RULE: &str = "Bash(python3 tools/check.py)";

fn refused_turn() -> Vec<Value> {
    vec![
        json!({"type": "user", "turn": 1, "text": "Add the draft and run the check", "state": "done", "reason": null}),
        json!({"type": "tool", "name": "Bash", "input": "python3 tools/check.py", "status": "refused", "result": "This command is not allowed in Ask orq", "rule": PYTHON_RULE}),
        json!({"type": "tool", "name": "Edit", "input": "/srv/alpha/.tdd-features.json", "status": "refused", "result": "This edit is not allowed in Ask orq", "rule": LEDGER_RULE}),
        json!({"type": "assistant", "text": "Both calls were refused."}),
        json!({"type": "result", "cost": 0.01, "duration_ms": 3000, "error": null}),
    ]
}

#[test]
fn a_refused_call_offers_allow_buttons_that_ask_for_the_passkey_and_then_offers_try_again() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let routes = vec![
        route("GET", CHAT_PATH, vec![reply(conversation(refused_turn(), vec![], false))]),
        route("POST", &format!("{CHAT_PATH}/allow"), vec![reply(json!({"rule": PYTHON_RULE, "scope": "chat"}))]),
        route("POST", &format!("{CHAT_PATH}/messages"), vec![json!({"status": 202, "body": {"turn": 2}})]),
    ];
    let buttons = "return Array.from(document.querySelectorAll('[data-ask-refused]')).map(function (r) { return {text: r.textContent, allow: Array.from(r.querySelectorAll('[data-ask-allow]')).map(function (b) { return b.getAttribute('data-ask-allow'); })}; });";
    let Some(shown) = open(1440, &at, routes.clone(), true, vec![json!({"wait": "[data-ask-refused]"}), json!({"eval": "refused", "js": buttons}), json!({"eval": "retry", "js": "return document.querySelectorAll('[data-ask-retry]').length;"})]) else { return };
    let refused = eval(&shown, "refused").as_array().unwrap().clone();
    assert_eq!(refused.len(), 2, "each refused call is a highlighted line: {shown}");
    assert!(refused[0]["text"].as_str().unwrap().contains("python3 tools/check.py"), "{shown}");
    assert_eq!(refused[0]["allow"], json!(["once", "chat"]), "{shown}");
    assert_eq!(refused[1]["allow"], json!(["once", "chat", "ledger"]), "the ledger's edits can be allowed too: {shown}");
    assert_eq!(eval(&shown, "retry"), 0, "Try again only after allowing: {shown}");

    let steps = vec![json!({"click": "[data-ask-refused] [data-ask-allow=chat]"}), json!({"wait": "[data-ask-retry]"}), json!({"click": "[data-ask-retry]"}), json!({"wait": "[data-ask-composer]"})];
    let Some(allowed) = open(1440, &at, routes, true, steps) else { return };
    let begun = sent(&allowed, "POST", "/auth/confirm/begin");
    assert_eq!(begun, vec![json!({"purpose": "chat.allow"})], "allowing asks for the passkey: {allowed}");
    let allow = sent(&allowed, "POST", &format!("{CHAT_PATH}/allow"));
    assert_eq!(allow.len(), 1, "{allowed}");
    assert_eq!((&allow[0]["rule"], &allow[0]["scope"]), (&json!(PYTHON_RULE), &json!("chat")), "{allowed}");
    assert!(allow[0]["assertion"]["signature"].is_string(), "with the passkey's answer: {allowed}");
    assert_eq!(sent(&allowed, "POST", &format!("{CHAT_PATH}/messages")), vec![json!({"text": format!("Allowed: {PYTHON_RULE}. Try again.")})], "{allowed}");
}

#[test]
fn deleting_a_chat_asks_first_says_why_when_refused_and_otherwise_goes_back_to_the_list() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let read = route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), vec![], false))]);
    let Some(asking) = open(1440, &at, vec![read.clone()], true, vec![json!({"click": "[data-ask-delete]"}), json!({"wait": "[data-ask-panel] [data-confirm]"})]) else { return };
    assert!(sent(&asking, "DELETE", CHAT_PATH).is_empty(), "nothing is deleted before the answer: {asking}");

    let refused = route("DELETE", CHAT_PATH, vec![json!({"status": 409, "body": {"error": "a reply is running in this chat"}})]);
    let steps = vec![json!({"click": "[data-ask-delete]"}), json!({"click": "[data-ask-panel] [data-confirm]"}), json!({"wait": "[data-ask-panel] [role=alert]"}), json!({"eval": "alert", "js": "return document.querySelector('[data-ask-panel] [role=alert]').textContent;"})];
    let Some(kept) = open(1440, &at, vec![read.clone(), refused], true, steps) else { return };
    assert_eq!(eval(&kept, "alert"), "a reply is running in this chat", "the server's reason: {kept}");
    assert!(eval(&kept, "address").as_str().unwrap().ends_with(&format!("?ask={CHAT}")), "the chat stays open: {kept}");

    let deleted = route("DELETE", CHAT_PATH, vec![reply(json!({"deleted": CHAT}))]);
    let steps = vec![json!({"click": "[data-ask-delete]"}), json!({"click": "[data-ask-panel] [data-confirm]"}), json!({"wait": "[data-ask-new]"})];
    let Some(gone) = open(1440, &at, vec![read, deleted], true, steps) else { return };
    assert_eq!(sent(&gone, "DELETE", CHAT_PATH), vec![json!({})], "{gone}");
    assert_eq!(eval(&gone, "address"), "/projects/alpha/reviews?ask=", "back to the list, the panel still open: {gone}");
}

#[test]
fn the_rules_allowed_in_a_chat_are_listed_and_removing_one_asks_first_then_reads_the_chat_again() {
    let at = format!("/projects/alpha/reviews?ask={CHAT}");
    let rules = vec![json!({"rule": PYTHON_RULE, "scope": "chat"}), json!({"rule": LEDGER_RULE, "scope": "ledger"})];
    let read = route("GET", CHAT_PATH, vec![reply(conversation(finished_turn(), rules.clone(), false)), reply(conversation(finished_turn(), vec![rules[1].clone()], false))]);
    let listed = "return Array.from(document.querySelectorAll('[data-ask-rule]')).map(function (r) { return r.getAttribute('data-ask-rule'); });";
    let steps = vec![json!({"wait": "[data-ask-rule]"}), json!({"eval": "rules", "js": listed}), json!({"click": "[data-ask-remove-rule]"}), json!({"wait": "[data-ask-panel] [data-confirm]"})];
    let Some(asking) = open(1440, &at, vec![read.clone()], true, steps) else { return };
    assert_eq!(eval(&asking, "rules"), &json!([PYTHON_RULE, LEDGER_RULE]), "{asking}");
    assert!(text(&asking).contains("Allowed in this chat"), "{asking}");
    assert!(sent(&asking, "DELETE", &format!("{CHAT_PATH}/allow")).is_empty(), "nothing is removed before the answer: {asking}");

    let refused = route("DELETE", &format!("{CHAT_PATH}/allow"), vec![json!({"status": 404, "body": {"error": "that rule is not allowed in this chat"}})]);
    let steps = vec![json!({"click": "[data-ask-remove-rule]"}), json!({"click": "[data-ask-panel] [data-confirm]"}), json!({"wait": "[data-ask-panel] [role=alert]"}), json!({"eval": "alert", "js": "return document.querySelector('[data-ask-panel] [role=alert]').textContent;"})];
    let Some(kept) = open(1440, &at, vec![read.clone(), refused], true, steps) else { return };
    assert_eq!(eval(&kept, "alert"), "that rule is not allowed in this chat", "{kept}");

    let removed = route("DELETE", &format!("{CHAT_PATH}/allow"), vec![reply(json!({"removed": PYTHON_RULE}))]);
    let steps = vec![json!({"click": "[data-ask-remove-rule]"}), json!({"click": "[data-ask-panel] [data-confirm]"}), json!({"wait": format!("[data-ask-rules]:not(:has([data-ask-rule='{PYTHON_RULE}']))")}), json!({"eval": "rules", "js": listed})];
    let Some(gone) = open(1440, &at, vec![read, removed], true, steps) else { return };
    assert_eq!(sent(&gone, "DELETE", &format!("{CHAT_PATH}/allow")), vec![json!({"rule": PYTHON_RULE})], "{gone}");
    assert_eq!(eval(&gone, "rules"), &json!([LEDGER_RULE]), "the chat is read again without the rule: {gone}");
}
