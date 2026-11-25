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
    assert_eq!(names.len(), 2, "{worker}");
    let name = names[0].as_str().unwrap();
    assert_eq!(names[1], json!("orq-pages"), "install also opens the record of where each page came from: {worker}");
    let version = name.strip_prefix("orq-shell-").expect("the cache is named orq-shell-<version>");
    assert!(version.len() == 16 && version.chars().all(|c| c.is_ascii_hexdigit()), "the build stamps the shell version: {name}");
    assert_eq!(worker["shellPaths"], json!(SHELL), "the shell is the page, its style and script, the manifest and the icons");
    assert_eq!(worker["skippedOnInstall"], json!(false), "a new release waits; it is never forced onto an open page");
    assert_eq!(worker["claimedBeforeActivate"], json!(false));
    assert_eq!(worker["afterActivate"], json!(["orq-pages", name, "someone-elses-cache"]), "only older orq shells are removed");
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
fn a_damaged_release_cache_never_mixes_the_running_release_with_a_newer_servers_files() {
    let Some(out) = run() else { return };
    let damaged = &out["damaged"];
    // The server has moved on to a newer release; a page and its files must come from one release.
    let newer = json!({"page": "later /projects/p", "script": "later /app.react.js", "style": "later /app.css", "cacheKept": true});
    assert_eq!(damaged["missingAsset"], newer, "a shell missing a file is left in place and the page opens whole from the server");
    assert_eq!(damaged["missingPage"], newer, "a shell missing its page is left in place and the page opens whole from the server");
    assert_eq!(damaged["broken"]["page"], json!("later /projects/p"), "cache storage failing: the page opens whole from the server");
    assert_eq!(damaged["broken"]["script"], json!("later /app.react.js"));
    assert_eq!(damaged["broken"]["style"], json!("later /app.css"));
    let lost = &damaged["lostMidPage"];
    assert_eq!(lost["page"], json!("install /"));
    assert_eq!(lost["script"], json!("install /app.react.js"));
    assert_eq!(lost["style"], json!("<network error>"), "a file lost under an open cached page is never taken from the newer server");
}

#[test]
fn a_cache_that_fails_or_is_evicted_under_an_open_cached_page_never_hands_it_the_newer_servers_files() {
    let Some(out) = run() else { return };
    let damaged = &out["damaged"];
    // The page opened from the running release's cache; then cache storage fails, or the whole cache is evicted.
    let failed_closed = json!({"page": "install /", "script": "<network error>", "style": "<network error>"});
    for case in ["breaksMidPage", "evictedMidPage"] {
        let got = &damaged[case];
        assert_eq!(json!({"page": got["page"], "script": got["script"], "style": got["style"]}), failed_closed, "{case}: {got}");
    }
}

#[test]
fn each_open_page_gets_its_files_from_where_that_page_came_from_not_from_whether_the_cache_is_there_now() {
    let Some(out) = run() else { return };
    // Tab A opened from the running release's cache; storage failed while tab B opened, so B came from
    // the newer server; storage has recovered by the time either asks for its script.
    let tabs = &out["tabs"]["storageFails"];
    assert_eq!(tabs["a"], json!({"page": "install /", "script": "install /app.react.js"}), "{tabs}");
    assert_eq!(tabs["b"], json!({"page": "later /projects/q", "script": "later /app.react.js"}), "{tabs}");
}

#[test]
fn a_page_opening_from_the_server_leaves_the_cache_another_open_page_was_built_from() {
    let Some(out) = run() else { return };
    // Tab A opened from the running release's cache; an icon was lost, so tab B opened from the newer server.
    let tabs = &out["tabs"]["shellLost"];
    assert_eq!(tabs["a"], json!({"page": "install /", "script": "install /app.react.js"}), "tab B's navigation keeps tab A's release: {tabs}");
    assert_eq!(tabs["b"], json!({"page": "later /projects/q", "script": "later /app.react.js"}), "{tabs}");
}

#[test]
fn where_an_open_page_came_from_outlives_a_restart_of_the_worker() {
    let Some(out) = run() else { return };
    let restarts = &out["restarts"];
    let cached = json!({"page": "install /", "before": "install /app.react.js", "script": "install /app.react.js"});
    assert_eq!(restarts["cached"], cached, "a cached page keeps its release's files after a restart: {restarts}");
    let server = json!({"page": "later /projects/p", "before": "later /app.react.js", "script": "later /app.react.js"});
    assert_eq!(restarts["fromServer"], server, "a page from the server never gets the kept cache's older files after a restart: {restarts}");
    let gone = json!({"page": "install /", "before": "install /app.react.js", "script": "<network error>"});
    assert_eq!(restarts["evicted"], gone, "a cached page whose cache is evicted after a restart never gets the newer server's files: {restarts}");
    assert_eq!(restarts["allEvicted"], gone, "a cached page whose cache and records are evicted together never gets the newer server's files: {restarts}");
    assert_eq!(restarts["broken"], gone, "a cached page whose storage fails after a restart never gets the newer server's files: {restarts}");
    assert_eq!(restarts["unrecorded"], server, "a page the worker cannot record as cached opens whole from the server: {restarts}");
}

#[test]
fn a_closed_page_is_forgotten_across_restarts_and_a_page_still_opening_keeps_its_record() {
    let Some(out) = run() else { return };
    let pruning = &out["pruning"];
    assert_eq!(pruning["afterClose"], json!(["b", "c"]), "a closed page's record is dropped: {pruning}");
    assert_eq!(pruning["closedAsks"], json!("<network error>"), "the worker forgets a closed page in memory too: {pruning}");
    assert_eq!(pruning["afterRestart"], json!(["c", "d"]), "a page closed while the worker was stopped is dropped after the restart: {pruning}");
    assert_eq!(pruning["pendingKept"], json!(true), "a page whose navigation is still in flight keeps its record: {pruning}");
    assert_eq!(pruning["pending"], json!("install /app.react.js"), "and gets its files from its release: {pruning}");
}

#[test]
fn a_newer_worker_never_takes_an_open_page_without_a_record_for_the_servers() {
    let Some(out) = run() else { return };
    let restarts = &out["restarts"];
    let gone = json!({"page": "install /", "before": "install /app.react.js", "script": "<network error>"});
    assert_eq!(restarts["replaced"], gone, "only the first worker takes the pages already open for the server's: {restarts}");
}

#[test]
fn a_burst_of_navigations_scans_the_records_once() {
    let Some(out) = run() else { return };
    let burst = &out["burst"];
    assert_eq!(burst["inBurst"], json!(1), "navigations at once or within a minute share one scan: {burst}");
    assert_eq!(burst["later"], json!(2), "a navigation a minute or more later scans again: {burst}");
}

#[test]
fn a_page_whose_source_is_unknown_gets_no_files_and_a_page_open_before_the_worker_took_over_gets_the_servers() {
    let Some(out) = run() else { return };
    let restarts = &out["restarts"];
    let unknown = json!({"page": null, "before": "<network error>", "script": "<network error>"});
    assert_eq!(restarts["neverOpened"], unknown, "no record is never taken to mean the server: {restarts}");
    let first = json!({"page": null, "before": "later /app.react.js", "script": "later /app.react.js"});
    assert_eq!(restarts["firstVisit"], first, "a page open when the worker took over came from the server: {restarts}");
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

const PROMPT: [&str; 4] = [".update-scrim", ".update-card[role=\"dialog\"][aria-modal=\"true\"]", "[data-update-reload]", "[data-update-fab]"];

fn prompt(steps: Vec<Value>, waiting: bool) -> Option<Value> {
    page(json!({"responses": [], "steps": steps, "selectors": PROMPT, "service_worker": {"secure": true, "waiting": waiting}}))
}

fn shown(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

// The prompt floats over the page instead of taking a place in its layout: a scrim covers the page and a
// modal card on it holds Reload, which has the focus.
#[test]
fn a_waiting_release_opens_a_modal_card_over_a_scrim_with_the_focus_on_reload() {
    let Some(out) = prompt(vec![], true) else { return };
    let start = &out["frames"][0];
    assert_eq!(shown(start, PROMPT[0]), 1, "the scrim: {out}");
    assert_eq!(shown(start, PROMPT[1]), 1, "the modal card: {out}");
    assert_eq!(start["settled"]["focusMatches"][PROMPT[2]], json!(true), "Reload has the focus: {out}");
    assert_eq!(shown(start, PROMPT[3]), 0, "no floating button while the card is open: {out}");
    assert!(texts(start).contains("Update available"), "{}", texts(start));
}

// Later, Escape or a tap on the scrim folds the prompt into a floating button, which opens it again; none of
// them applies the update.
#[test]
fn later_escape_or_the_scrim_fold_the_card_into_a_floating_button_that_opens_it_again() {
    for close in [json!({"click": "[data-update-later]"}), json!({"keydown": "Escape"}), json!({"click": ".update-scrim"})] {
        let Some(out) = prompt(vec![close.clone(), json!({"click": "[data-update-fab]"})], true) else { return };
        let folded = &out["frames"][1];
        assert_eq!(shown(folded, PROMPT[0]), 0, "{close}: the scrim is gone: {out}");
        assert_eq!(shown(folded, PROMPT[1]), 0, "{close}: the card is gone: {out}");
        assert_eq!(shown(folded, "[data-update-fab]"), 1, "{close}: the floating button: {out}");
        assert_eq!(folded["settled"]["focusMatches"][PROMPT[3]], json!(true), "{close}: focus goes to the floating button: {out}");
        let reopened = &out["frames"][2];
        assert_eq!(shown(reopened, PROMPT[1]), 1, "{close}: the button opens the card again: {out}");
        assert_eq!(shown(reopened, PROMPT[3]), 0, "{close}: {out}");
        assert_eq!(reopened["settled"]["focusMatches"][PROMPT[2]], json!(true), "{close}: Reload has the focus again: {out}");
        assert_eq!(out["worker"]["messages"], json!([]), "{close}: nothing is applied: {out}");
    }
}

// The card holds the focus: Tab from its last button wraps to its first, Shift+Tab from its first to its last.
#[test]
fn tab_and_shift_tab_keep_the_focus_inside_the_update_card() {
    let steps = vec![json!({"keydown": "Tab"}), json!({"keydown": "Tab", "shift": true})];
    let Some(out) = page(json!({"responses": [], "steps": steps, "selectors": ["[data-update-reload]", "[data-update-later]"],
        "service_worker": {"secure": true, "waiting": true}})) else { return };
    assert_eq!(out["frames"][1]["settled"]["focusMatches"]["[data-update-later]"], json!(true), "Tab wraps to Later: {out}");
    assert_eq!(out["frames"][2]["settled"]["focusMatches"]["[data-update-reload]"], json!(true), "Shift+Tab wraps to Reload: {out}");
}

fn css_rule<'a>(css: &'a str, start: &str) -> &'a str {
    css.lines().find(|line| line.starts_with(start)).unwrap_or_else(|| panic!("a rule starting {start:?}"))
}

// The prompt floats: a fixed blurred scrim, a fixed card (near the bottom, at most 24rem wide; on a phone above the
// tab bar inside 16px gutters and the safe area) and a fixed 48px floating button; it fades in, which reduced motion
// turns off, and its colours are the theme's.
#[test]
fn the_update_prompt_floats_over_the_page_in_theme_colours() {
    let css = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/web/app.css")).unwrap();
    let scrim = css_rule(&css, ".update-scrim {");
    for part in ["position: fixed", "inset: 0", "background: var(--scrim)", "backdrop-filter: blur(2px)"] {
        assert!(scrim.contains(part), "scrim {part}: {scrim}");
    }
    let card = css_rule(&css, ".update-card {");
    for part in ["position: fixed", "background: var(--surface)", "border-radius: 0.75rem", "max-width: 24rem",
        "box-shadow: var(--shadow-dialog)", "animation: update-in 150ms"] {
        assert!(card.contains(part), "card {part}: {card}");
    }
    let phone = css.find("@media (max-width: 700px) {\n  .update-card").map(|at| &css[at..]).expect("the phone card query");
    let phone_card = css_rule(phone, "  .update-card {");
    for part in ["bottom: calc(3.75rem + env(safe-area-inset-bottom) + 0.75rem)", "left: calc(16px + env(safe-area-inset-left))",
        "right: calc(16px + env(safe-area-inset-right))"] {
        assert!(phone_card.contains(part), "phone card {part}: {phone_card}");
    }
    let phone_fab = css_rule(phone, "  .update-fab {");
    assert!(phone_fab.contains("bottom: calc(3.75rem + env(safe-area-inset-bottom) + 0.75rem)"), "{phone_fab}");
    let fab = css_rule(&css, ".update-fab {");
    for part in ["position: fixed", "width: 48px", "height: 48px", "border-radius: 50%"] {
        assert!(fab.contains(part), "fab {part}: {fab}");
    }
    assert!(css_rule(&css, ".update-dot {").contains("background: var(--accent)"));
    let later = css_rule(&css, ".update-later {");
    for part in ["min-height: 44px", "border: 1px solid var(--border)"] {
        assert!(later.contains(part), "Later {part}: {later}");
    }
    assert!(css.contains("@keyframes update-in"), "the fade-in");
    assert!(css.contains("@media (prefers-reduced-motion: reduce) {\n  *, *::before, *::after { animation: none !important;"),
        "no fade under reduced motion: the page-wide rule stops every animation");
}

// A release arriving while a page control has the focus: folding the card gives that control the focus back,
// or the floating button when the control has gone meanwhile.
#[test]
fn folding_the_card_gives_the_focus_back_to_the_control_that_had_it_or_the_floating_button_if_it_is_gone() {
    let later = |steps: Vec<Value>| page(json!({"responses": [], "steps": steps, "selectors": ["input", "[data-update-fab]", "[data-update-reload]"],
        "service_worker": {"secure": true, "waiting": "later"}}));
    let Some(out) = later(vec![json!({"focus": "input"}), json!({"update_found": true}), json!({"keydown": "Escape"})]) else { return };
    assert_eq!(out["frames"][2]["settled"]["focusMatches"]["[data-update-reload]"], json!(true), "the card takes the focus: {out}");
    assert_eq!(out["frames"][3]["settled"]["focusMatches"]["input"], json!(true), "the control has it back: {out}");
    let Some(out) = later(vec![json!({"focus": "input"}), json!({"update_found": true}), json!({"remove": "input"}), json!({"keydown": "Escape"})]) else { return };
    assert_eq!(out["frames"][4]["settled"]["focusMatches"]["[data-update-fab]"], json!(true), "the control is gone: {out}");
}
