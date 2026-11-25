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
/// (the last one repeats); a reply is `{status, body}` or `{status, stream: [{after, text}], hold}`, a body
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
      return Promise.resolve(reply.stream ? stream(reply) : json(reply.status || 200, reply.body));
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
    std::fs::write(format!("/tmp/askdbg-{}.json", std::thread::current().name().unwrap_or("x").rsplit("::").next().unwrap_or("x")), result.to_string()).ok(); // DEBUG
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
