//! The installed app opens offline: a service worker keeps the app shell, never the data, and a
//! new release is offered with an "Update available" banner instead of reloading by itself.
//! Checked by running the built worker and the page's registration code under node.

mod web_deps;

use std::process::Command;

use serde_json::{json, Value};

const SHELL: [&str; 8] = ["/", "/app.css", "/app.react.js", "/icons/apple-touch-icon.png", "/icons/icon-192.png", "/icons/icon-512.png", "/icons/icon-maskable-512.png", "/manifest.webmanifest"];

fn node(args: &[String]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    match Command::new("node").args(args).output() {
        Ok(output) => {
            assert!(output.status.success(), "node failed: {}", String::from_utf8_lossy(&output.stderr));
            Some(serde_json::from_slice(&output.stdout).expect("node prints JSON"))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("cannot run node: {error}"),
    }
}

fn run() -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    node(&[format!("{dir}/tests/web_sw.mjs"), format!("{dir}/web/sw.js"), format!("{dir}/web-src/src/worker.js")])
}

fn page(scenario: Value) -> Option<Value> {
    let dir = env!("CARGO_MANIFEST_DIR");
    node(&[format!("{dir}/tests/web_react_harness.mjs"), format!("{dir}/web/app.react.js"), scenario.to_string()])
}

#[test]
fn on_install_the_worker_caches_the_shell_under_its_release_and_on_activate_drops_older_releases() {
    let Some(out) = run() else { return };
    let worker = &out["worker"];
    let names = worker["names"].as_array().unwrap();
    assert_eq!(names.len(), 1, "{worker}");
    let name = names[0].as_str().unwrap();
    let version = name.strip_prefix("orq-shell-").expect("the cache is named orq-shell-<version>");
    assert!(version.len() == 16 && version.chars().all(|c| c.is_ascii_hexdigit()), "the build stamps the shell version: {name}");
    assert_eq!(worker["shellPaths"], json!(SHELL), "the shell is the page, its style and script, the manifest and the icons");
    assert_eq!(worker["skippedOnInstall"], json!(false), "a new release waits; it is never forced onto an open page");
    assert_eq!(worker["claimedBeforeActivate"], json!(false));
    assert_eq!(worker["afterActivate"], json!([name, "someone-elses-cache"]), "only older orq shells are removed");
    assert_eq!(worker["claimed"], json!(true), "the active worker takes the open pages");
}

#[test]
fn data_sign_in_posts_and_other_origins_are_never_answered_by_the_worker() {
    let Some(out) = run() else { return };
    let online = &out["worker"]["online"];
    for case in ["api", "apiEvents", "apiNavigation", "auth", "authNavigation", "post", "postNavigation", "otherOrigin", "worker"] {
        assert_eq!(online[case]["handled"], json!(false), "{case} goes straight to the network: {online}");
    }
}

#[test]
fn a_page_opens_the_running_releases_cached_shell_online_and_offline() {
    // The page, its script and its style always come from one release, so a newer server never mixes them.
    let Some(out) = run() else { return };
    let worker = &out["worker"];
    assert_eq!(worker["online"]["navigation"], json!({"handled": true, "text": "install /"}), "online, the running release's shell opens");
    assert_eq!(worker["offline"]["navigation"], json!({"handled": true, "text": "install /"}), "offline, any page opens the shell, which then says it cannot reach the server");
    assert_eq!(worker["offline"]["root"], json!({"handled": true, "text": "install /"}));
}

#[test]
fn a_shell_file_comes_from_the_running_releases_cache_which_a_newer_server_never_overwrites() {
    let Some(out) = run() else { return };
    let worker = &out["worker"];
    assert_eq!(worker["online"]["asset"], json!({"handled": true, "text": "install /app.css"}));
    assert_eq!(worker["cachedAfter"], json!("install /app.css"), "a release's cache is never rewritten; the next release brings its own");
    assert_eq!(worker["fetchedOnline"], json!([]), "a cached shell never reaches for the newer server's files");
    assert_eq!(worker["offline"]["asset"], json!({"handled": true, "text": "install /app.react.js"}));
    assert_eq!(worker["offline"]["style"], json!({"handled": true, "text": "install /app.css"}));
    assert_eq!(worker["offline"]["icon"], json!({"handled": true, "text": "install /icons/icon-192.png"}));
}

#[test]
fn only_the_pages_skip_waiting_message_lets_a_new_release_take_over() {
    let Some(out) = run() else { return };
    assert_eq!(out["worker"]["skipWaiting"], json!(true));
    assert_eq!(out["worker"]["otherMessageSkips"], json!(false));
}

#[test]
fn the_page_registers_the_worker_after_load_only_in_a_secure_context_and_ignores_a_failure() {
    let Some(out) = run() else { return };
    let reg = &out["registration"];
    let once = json!([{"url": "/sw.js", "scope": "/"}]);
    assert_eq!(reg["insecure"]["registered"], json!([]), "plain http never registers");
    assert_eq!(reg["noApi"]["registered"], json!([]));
    assert_eq!(reg["noApi"]["threw"], Value::Null, "a browser without service workers is fine");
    assert_eq!(reg["loaded"]["registered"], once, "a page already loaded registers at once");
    assert_eq!(reg["loading"]["beforeLoad"], json!(0), "registration waits for load");
    assert_eq!(reg["loading"]["registered"], once);
    assert_eq!(reg["fails"]["registered"], once);
    assert_eq!(reg["fails"]["threw"], Value::Null);
    assert_eq!(reg["unhandled"], json!(0), "a refused registration is ignored, not left unhandled");
}

#[test]
fn a_waiting_release_is_offered_and_only_the_persons_reload_applies_it() {
    let Some(out) = run() else { return };
    let reg = &out["registration"];
    assert_eq!(reg["waiting"]["updates"], json!(1), "a release already waiting is offered");
    assert_eq!(reg["firstInstall"]["updates"], json!(0), "the first install is not an update");
    assert_eq!(reg["foundLater"]["updates"], json!(1), "a release found while the page is open is offered once it installs");
    for case in ["waiting", "foundLater"] {
        assert_eq!(reg[case]["reloads"], json!(0), "{case}: the page never reloads by itself");
        assert_eq!(reg[case]["messages"], json!([]), "{case}: nothing is applied before the person asks");
    }
    assert_eq!(reg["apply"]["messages"], json!([{"type": "skip-waiting"}]));
    assert_eq!(reg["apply"]["reloadsBeforeTakeover"], json!(0));
    assert_eq!(reg["apply"]["reloads"], json!(1), "the page reloads once the new worker has taken over, and only once");
}

#[test]
fn a_release_already_installing_when_the_page_registers_is_offered_once_it_installs() {
    let Some(out) = run() else { return };
    let reg = &out["registration"];
    assert_eq!(reg["installingAtRegister"]["updates"], json!(1), "a release mid-install when the page registers is still offered");
    assert_eq!(reg["installingAndFound"]["updates"], json!(1), "a release reported as found as well is offered only once");
    for case in ["installingAtRegister", "installingAndFound"] {
        assert_eq!(reg[case]["reloads"], json!(0), "{case}: the page never reloads by itself");
        assert_eq!(reg[case]["messages"], json!([]), "{case}: nothing is applied before the person asks");
    }
}

fn texts(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

#[test]
fn the_page_shows_an_update_banner_with_a_reload_button_when_a_release_is_waiting() {
    let waiting = json!({"responses": [], "steps": [{"click": "button[data-update-reload]"}], "service_worker": {"secure": true, "waiting": true}});
    let Some(out) = page(waiting) else { return };
    let start = &out["frames"][0];
    assert!(texts(start).contains("Update available"), "{}", texts(start));
    let live = start["settled"]["a11y"]["live"].as_array().unwrap();
    assert!(live.iter().any(|item| item["text"].as_str().unwrap_or("").contains("Update available")), "the banner is announced: {live:?}");
    assert_eq!(out["worker"]["registered"], json!([{"url": "/sw.js", "scope": "/"}]));
    assert_eq!(out["worker"]["messages"], json!([{"type": "skip-waiting"}]), "Reload tells the waiting worker to take over");

    let current = json!({"responses": [], "steps": [], "service_worker": {"secure": true, "waiting": false}});
    let Some(out) = page(current) else { return };
    assert!(!texts(&out["frames"][0]).contains("Update available"));
    assert_eq!(out["worker"]["messages"], json!([]));

    let insecure = json!({"responses": [], "steps": [], "service_worker": {"secure": false, "waiting": true}});
    let Some(out) = page(insecure) else { return };
    assert_eq!(out["worker"]["registered"], json!([]), "plain http never registers");
}
