//! Web push settings: which kinds of notification are on and which projects are muted, read and changed
//! with a passkey session, and the Settings and project card controls that change them.

mod web_deps;
mod web_push_session;

use orq_core::storage::StorageRoots;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use serde_json::{json, Value};

const TOKEN: &str = "web-push-settings-token-0123456789abcdef";

fn roots(label: &str) -> StorageRoots {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    roots
}

/// A push service on loopback that takes every push with a 201 and counts them.
fn fake_push_service() -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; length];
            reader.read_exact(&mut body).unwrap();
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = write!(stream, "HTTP/1.1 201 X\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
    });
    (format!("http://127.0.0.1:{port}/push/one"), count)
}

fn subscribe_body(endpoint: &str) -> Value {
    let secret = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
    let public = secret.public_key().to_encoded_point(false);
    json!({"endpoint": endpoint, "keys": {"p256dh": orq_remote::web_push::base64url_encode(public.as_bytes()), "auth": orq_remote::web_push::base64url_encode(&[7u8; 16])}, "label": "Firefox on Linux"})
}

struct NoProjects;
impl orq_core::remote_inbox::ProjectsProvider for NoProjects {
    fn projects(&self) -> Result<Value, String> {
        Ok(json!([]))
    }
}

/// A server with passkey sign-in; the bearer token does not stand in for a session.
fn serve(roots: &StorageRoots) -> orq_remote::Server {
    let state = std::sync::Arc::new(orq_remote::ServerState { roots: roots.clone(), config: orq_core::remote_inbox::RemoteConfig { token: TOKEN.to_string() }, project_id: "p-0a1b2c3d".into(), project_name: "orq-cli".into() });
    let host = orq_core::passkey_register::PasskeyHost::from_setting("box.tail1234.ts.net").unwrap();
    let auth = std::sync::Arc::new(orq_core::passkey_routes::PasskeyAuth::new(host).with_loopback_token(false));
    orq_remote::Server::start_with_auth(std::net::TcpListener::bind("127.0.0.1:0").unwrap(), state, std::sync::Arc::new(NoProjects), auth).unwrap()
}

/// One request; the status and the JSON body. With a session it sends the cookie, origin and CSRF token a signed-in page does.
fn call(server: &orq_remote::Server, method: &str, path: &str, session: Option<&web_push_session::Session>, body: &Value) -> (u16, Value) {
    let body = if body.is_null() { Vec::new() } else { body.to_string().into_bytes() };
    let origin = format!("https://{}", web_push_session::RP);
    let headers: Vec<(&str, &str)> = match session {
        Some(s) => vec![("Cookie", &s.cookie), ("Origin", &origin), ("X-CSRF-Token", &s.csrf)],
        None => vec![],
    };
    let (status, _, body) = web_push_session::raw(server.addr(), method, path, &headers, &body);
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

fn event<'a>(kind: &'a str, project: &'a str, run: &'a str) -> orq_remote::web_push::PushEvent<'a> {
    orq_remote::web_push::PushEvent { kind, project_id: project, run_id: Some(run), event_id: None }
}

fn kind_on(kinds: &Value, kind: &str) -> Option<bool> {
    kinds["kinds"].as_array()?.iter().find(|k| k["kind"] == kind)?["enabled"].as_bool()
}

#[test]
fn kinds_and_mutes_need_a_passkey_session_and_are_what_notify_reads() {
    let roots = roots("push-settings-routes");
    let (endpoint, pushed) = fake_push_service();
    let server = serve(&roots);
    let none = Value::Null;

    // Signed out: nothing is read or changed.
    assert_eq!(call(&server, "GET", "/api/push/kinds", None, &none).0, 401);
    assert_eq!(call(&server, "GET", "/api/push/mutes", None, &none).0, 401);
    assert_eq!(call(&server, "PUT", "/api/push/kinds", None, &json!({"failed": false})).0, 403);
    assert_eq!(call(&server, "PUT", "/api/push/mutes/p-alpha", None, &none).0, 403);
    assert_eq!(call(&server, "DELETE", "/api/push/mutes/p-alpha", None, &none).0, 403);

    let session = web_push_session::sign_in(server.addr(), TOKEN);
    let session = Some(&session);
    assert_eq!(call(&server, "POST", "/api/push/subscriptions", session, &subscribe_body(&endpoint)).0, 201);

    // Every kind is listed; "started" starts off, the rest on.
    let (status, kinds) = call(&server, "GET", "/api/push/kinds", session, &none);
    assert_eq!(status, 200, "{kinds}");
    assert_eq!(kinds["kinds"].as_array().unwrap().len(), orq_remote::web_push::KINDS.len(), "{kinds}");
    assert_eq!((kind_on(&kinds, "started"), kind_on(&kinds, "failed")), (Some(false), Some(true)), "{kinds}");

    // A kind switched off is not pushed; one switched on is.
    let (status, changed) = call(&server, "PUT", "/api/push/kinds", session, &json!({"failed": false, "started": true}));
    assert_eq!(status, 200, "{changed}");
    assert_eq!((kind_on(&changed, "started"), kind_on(&changed, "failed")), (Some(true), Some(false)), "{changed}");
    let kinds = call(&server, "GET", "/api/push/kinds", session, &none).1;
    assert_eq!((kind_on(&kinds, "started"), kind_on(&kinds, "failed")), (Some(true), Some(false)), "{kinds}");
    assert_eq!(orq_remote::web_push::notify(&roots, &event("failed", "p-alpha", "run-1")), 0);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("started", "p-alpha", "run-1")), 1);

    // Not a kind, or not on or off: refused, nothing changed.
    assert_eq!(call(&server, "PUT", "/api/push/kinds", session, &json!({"nonsense": true})).0, 400);
    assert_eq!(call(&server, "PUT", "/api/push/kinds", session, &json!({"failed": "yes"})).0, 400);
    assert_eq!(call(&server, "PUT", "/api/push/kinds", session, &json!({"failed": true, "nonsense": true})).0, 400);
    assert_eq!(kind_on(&call(&server, "GET", "/api/push/kinds", session, &none).1, "failed"), Some(false));

    // A muted project is not pushed; unmuted, it is again.
    assert_eq!(call(&server, "PUT", "/api/push/mutes/p-alpha", session, &none).0, 200);
    assert_eq!(call(&server, "GET", "/api/push/mutes", session, &none).1, json!({"muted": ["p-alpha"]}));
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", "p-alpha", "run-2")), 0);
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", "p-beta", "run-3")), 1);
    assert_eq!(call(&server, "DELETE", "/api/push/mutes/p-alpha", session, &none).0, 200);
    assert_eq!(call(&server, "GET", "/api/push/mutes", session, &none).1, json!({"muted": []}));
    assert_eq!(orq_remote::web_push::notify(&roots, &event("needs_input", "p-alpha", "run-2")), 1);

    // Not a project id: refused.
    assert_eq!(call(&server, "PUT", "/api/push/mutes/a%2Fb", session, &none).0, 400);
    assert_eq!(pushed.load(std::sync::atomic::Ordering::SeqCst), 3);
}

// The Settings section and the project card menu, in the page.

const FAILED_ON: &str = "input[data-push-kind=\"failed\"]:checked";
const BETA_MUTED: &str = "[data-push-muted=\"beta\"]";
const ENABLE_OFF: &str = "[data-push-enable]:disabled";
const SELECTORS: [&str; 9] = ["[data-push-device]", "[data-push-enable]", "input[data-push-kind]", "input[data-push-kind]:checked", "[data-push-mute]", "[data-push-muted]", FAILED_ON, BETA_MUTED, ENABLE_OFF];
const FIREFOX: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0";

fn page(scenario: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let output = std::process::Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn kinds(off: &[&str]) -> Value {
    json!({"kinds": orq_remote::web_push::KINDS.iter().map(|kind| json!({"kind": kind, "enabled": !off.contains(kind)})).collect::<Vec<_>>()})
}

fn device(id: i64, label: &str) -> Value {
    json!({"id": id, "label": label, "created_at": 1_790_000_000})
}

/// Settings, signed in with a passkey, with the given /api/push replies.
fn settings_page(push: Value, replies: Value, steps: Value) -> Option<Value> {
    let mut push_replies = json!({"GET /api/push/key": {"public_key": "AQID"}, "GET /api/push/kinds": kinds(&["started"]), "GET /api/push/subscriptions": {"subscriptions": []}});
    for (route, reply) in replies.as_object().unwrap() {
        push_replies[route] = reply.clone();
    }
    let settings = json!({"status": 200, "json": {"settings": {"implementer": null, "reviewer": null, "models": [], "remote_actions": false, "version": "v-1"}}});
    page(json!({
        "url": "/settings", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"}, "auth_replies": {"/auth/passkeys": {"passkeys": []}},
        "responses": [settings.clone(), settings], "push": push, "push_replies": push_replies, "user_agent": FIREFOX, "steps": steps, "selectors": SELECTORS
    }))
}

fn shown(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

fn sent(out: &Value, method: &str) -> Vec<Value> {
    out["pushRequests"].as_array().unwrap().iter().filter(|r| r["method"] == method).cloned().collect()
}

#[test]
fn settings_enables_this_device_lists_devices_removes_one_and_sends_a_test() {
    let subscription = json!({"endpoint": "https://push.example/abc", "keys": {"p256dh": "BPk", "auth": "au"}});
    let replies = json!({
        "GET /api/push/subscriptions": [{"subscriptions": [device(1, "Safari on iPhone")]}, {"subscriptions": [device(1, "Safari on iPhone"), device(2, "Firefox on Linux")]}, {"subscriptions": [device(2, "Firefox on Linux")]}],
        "POST /api/push/subscriptions": {"status": 201, "json": {"id": 2}},
        "POST /api/push/subscriptions/1/test": {"sent": true},
        "DELETE /api/push/subscriptions/1": {"removed": true},
        "POST /api/push/subscriptions/2/test": {"sent": false, "error": "the push service answered 500"}
    });
    let steps = json!([{"click": "[data-push-enable]"}, {"click": "[data-push-test=\"1\"]"}, {"click": "[data-push-remove=\"1\"]"}, {"click": "[data-push-test=\"2\"]"}]);
    let Some(out) = settings_page(json!({"permission": "granted", "subscription": subscription}), replies, steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], "[data-push-device]"), 1, "{out}");
    assert_eq!(count(&frames[0], "input[data-push-kind]"), orq_remote::web_push::KINDS.len() as u64, "a switch per kind: {out}");
    assert_eq!(count(&frames[0], "input[data-push-kind]:checked"), orq_remote::web_push::KINDS.len() as u64 - 1, "{out}");

    // Enable: asks the browser, subscribes with the server's key, and sends the subscription with a label.
    assert_eq!(out["pushed"]["asked"], 1, "{out}");
    assert_eq!(out["pushed"]["subscribed"], json!([{"userVisibleOnly": true, "applicationServerKey": [1, 2, 3]}]), "{out}");
    let posts: Vec<Value> = sent(&out, "POST").into_iter().filter(|r| r["url"] == "/api/push/subscriptions").collect();
    assert_eq!(posts.len(), 1, "{out}");
    let mut expected = subscription.clone();
    expected["label"] = json!("Firefox on Linux");
    assert_eq!(posts[0]["body"], expected, "{out}");
    assert_eq!(posts[0]["headers"]["X-CSRF-Token"], "c-1", "{out}");
    assert_eq!(count(&frames[1], "[data-push-device]"), 2, "the list is read again: {out}");

    // Send test says "Sent" beside that device; a failure shows the push service's error there.
    assert!(shown(&frames[2]).iter().any(|t| t == "Sent"), "{out}");
    let deletes = sent(&out, "DELETE");
    assert_eq!(deletes.len(), 1, "{out}");
    assert_eq!((deletes[0]["url"].as_str(), deletes[0]["headers"]["X-CSRF-Token"].as_str()), (Some("/api/push/subscriptions/1"), Some("c-1")), "{out}");
    assert_eq!(count(&frames[3], "[data-push-device]"), 1, "{out}");
    assert!(shown(&frames[4]).iter().any(|t| t == "the push service answered 500"), "{out}");
}

#[test]
fn a_denied_permission_or_a_browser_without_push_says_so_and_sends_nothing() {
    let subscription = json!({"endpoint": "https://push.example/abc", "keys": {"p256dh": "BPk", "auth": "au"}});
    let steps = json!([{"click": "[data-push-enable]"}]);
    let Some(out) = settings_page(json!({"permission": "denied", "subscription": subscription}), json!({}), steps.clone()) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(out["pushed"]["asked"], 1, "{out}");
    assert!(shown(last).iter().any(|t| t == "Notifications are blocked for this site. Allow them in the browser's site settings, then try again."), "{out}");
    assert_eq!(out["pushed"]["subscribed"], json!([]), "nothing subscribed: {out}");
    assert!(sent(&out, "POST").is_empty(), "nothing sent: {out}");
    assert!(!out["pushRequests"].as_array().unwrap().iter().any(|r| r["url"] == "/api/push/key"), "{out}");

    let Some(out) = settings_page(json!({"supported": false}), json!({}), steps) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert!(shown(last).iter().any(|t| t == "This browser cannot show notifications from this page."), "{out}");
    assert!(sent(&out, "POST").is_empty(), "nothing sent: {out}");
}

#[test]
fn enable_gives_up_when_no_service_worker_becomes_ready() {
    let subscription = json!({"endpoint": "https://push.example/abc", "keys": {"p256dh": "BPk", "auth": "au"}});
    let steps = json!([{"click": "[data-push-enable]"}, {"expire": 8000}]);
    let Some(out) = settings_page(json!({"permission": "granted", "subscription": subscription, "ready": "never"}), json!({}), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[1], ENABLE_OFF), 1, "busy while the worker is awaited: {out}");
    assert_eq!(count(&frames[2], ENABLE_OFF), 0, "Enable can be tried again: {out}");
    assert!(shown(&frames[2]).iter().any(|t| t == "This page's service worker did not start, so notifications cannot be set up. Reload the page and try again."), "{out}");
    assert_eq!(out["pushed"]["subscribed"], json!([]), "{out}");
    assert!(sent(&out, "POST").is_empty(), "nothing sent: {out}");
}

/// The Dashboard, signed in with a passkey, with the given /api/push replies.
fn dashboard_page(replies: Value, steps: Value) -> Option<Value> {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    let projects = json!({"status": 200, "json": {"projects": [project("alpha"), project("beta")]}});
    page(json!({
        "url": "/dashboard", "auth": {"passkeys": true, "signed_in": true, "csrf": "c-1"}, "auth_replies": {"/auth/passkeys": {"passkeys": []}},
        "responses": [projects.clone(), projects.clone(), projects.clone(), projects], "push_replies": replies, "steps": steps, "selectors": SELECTORS
    }))
}

fn buttons(frame: &Value) -> Vec<String> {
    frame["settled"]["buttons"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

#[test]
fn a_kind_switched_off_and_a_muted_project_show_as_such_after_reload() {
    // Settings: switching "Failed" off sends it, and the page read afresh shows it off.
    let replies = json!({"GET /api/push/kinds": [kinds(&["started"]), kinds(&["started", "failed"])], "PUT /api/push/kinds": kinds(&["started", "failed"])});
    let Some(out) = settings_page(json!({"permission": "granted", "subscription": {}}), replies, json!([{"click": "[data-push-kind=\"failed\"]"}, {"nav": "[data-refresh]"}])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], FAILED_ON), 1, "{out}");
    let puts = sent(&out, "PUT");
    assert_eq!(puts.len(), 1, "{out}");
    assert_eq!((puts[0]["url"].as_str(), &puts[0]["body"], puts[0]["headers"]["X-CSRF-Token"].as_str()), (Some("/api/push/kinds"), &json!({"failed": false}), Some("c-1")), "{out}");
    assert_eq!(count(&frames[1], FAILED_ON), 0, "{out}");
    assert_eq!(count(&frames[2], FAILED_ON), 0, "still off after a reload: {out}");
    assert_eq!(out["pushRequests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/push/kinds" && r["method"] == "GET").count(), 2, "{out}");

    // Dashboard: each project card's menu mutes it; once muted, a reload shows it so and offers Unmute.
    let replies = json!({"GET /api/push/mutes": [{"muted": []}, {"muted": ["beta"]}], "PUT /api/push/mutes/beta": {"project": "beta", "muted": true}});
    let Some(out) = dashboard_page(replies, json!([{"click": "[data-push-mute=\"beta\"]"}])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!((count(&frames[0], "[data-push-mute]"), count(&frames[0], "[data-push-muted]")), (2, 0), "{out}");
    assert_eq!(buttons(&frames[0]).iter().filter(|b| *b == "Mute notifications").count(), 2, "{out}");
    let puts = sent(&out, "PUT");
    assert_eq!(puts.len(), 1, "{out}");
    assert_eq!((puts[0]["url"].as_str(), puts[0]["headers"]["X-CSRF-Token"].as_str()), (Some("/api/push/mutes/beta"), Some("c-1")), "{out}");
    assert_eq!(count(&frames[1], BETA_MUTED), 1, "{out}");

    let replies = json!({"GET /api/push/mutes": [{"muted": ["beta"]}, {"muted": []}], "DELETE /api/push/mutes/beta": {"project": "beta", "muted": false}});
    let Some(out) = dashboard_page(replies, json!([{"click": "[data-push-mute=\"beta\"]"}])) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(count(&frames[0], BETA_MUTED), 1, "muted after a reload: {out}");
    assert!(shown(&frames[0]).iter().any(|t| t == "Notifications muted"), "{out}");
    assert_eq!(buttons(&frames[0]).iter().filter(|b| *b == "Unmute").count(), 1, "{out}");
    let deletes = sent(&out, "DELETE");
    assert_eq!((deletes.len(), deletes[0]["url"].as_str()), (1, Some("/api/push/mutes/beta")), "{out}");
    assert_eq!(count(&frames[1], "[data-push-muted]"), 0, "{out}");
}
