//! A feature `awaiting_delivery` (built on a group run's branch, waiting for the run to deliver it): the features
//! API passes it through with the branch (`held_on`) and run (`held_by`) it waits on, and the web shows it as
//! "Built, awaiting delivery" on the Features page, the feature's page and a group run's member list, with no
//! Start or Queue.

mod web_chromium;
mod web_deps;

use std::path::{Path, PathBuf};
use std::process::Command;

use orq_core::remote_inbox::{handle_with, RemoteConfig, Request};
use orq_core::storage::StorageRoots;
use orq_remote::projects::StoreProjects;
use serde_json::{json, Value};

// The server.

fn git_repo(root: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    std::fs::create_dir_all(&path).unwrap();
    assert!(Command::new("git").args(["init", "-q"]).current_dir(&path).status().unwrap().success());
    path
}

struct Setup {
    scan: PathBuf,
    roots: StorageRoots,
}

fn setup(label: &str) -> Setup {
    let base = web_deps::private_temp_dir(label);
    let roots = StorageRoots { data: base.join("data"), config: base.join("config"), state: base.join("state"), cache: base.join("cache") };
    roots.create_all().unwrap();
    Setup { scan: base.join("code"), roots }
}

fn get(setup: &Setup, path: &str) -> (u16, Value) {
    let provider = StoreProjects::new(setup.roots.clone(), setup.scan.clone());
    let config = RemoteConfig { token: "tok".into() };
    let request = Request { method: "GET", path, peer: "127.0.0.1".parse().unwrap(), authorization: Some("Bearer tok") };
    let r = handle_with(&setup.roots, &config, "server-project", "server", &request, &provider);
    (r.status, r.body)
}

#[test]
fn the_features_api_returns_a_feature_awaiting_delivery_with_the_branch_and_run_it_waits_on() {
    let s = setup("features-awaiting-delivery");
    let alpha = git_repo(&s.scan, "alpha");
    std::fs::write(
        alpha.join(".tdd-features.json"),
        json!([
            {"featureName": "built", "dependsOn": [], "status": "awaiting_delivery", "heldOn": "fix--built", "heldBy": "run-7-1"},
            {"featureName": "next", "dependsOn": ["built"], "status": "pending"}
        ])
        .to_string(),
    )
    .unwrap();
    let (status, body) = get(&s, "/api/projects/alpha/features");
    assert_eq!(status, 200, "{body}");
    let built = &body["features"][0];
    assert_eq!((&built["name"], &built["status"]), (&json!("built"), &json!("awaiting_delivery")), "{body}");
    assert_eq!((&built["held_on"], &built["held_by"]), (&json!("fix--built"), &json!("run-7-1")), "{body}");
    let next = &body["features"][1];
    assert!(next.get("held_on").is_none() && next.get("held_by").is_none(), "a feature held by no run has neither: {body}");
}

/// A design group run `run` of shop-header (passed) and shop-cards (implementing), with shop-header
/// `awaiting_delivery` on the run's branch and shop-cards in progress.
fn stored_group_run(s: &Setup, run: &str) {
    use orq_orchestrator::{Event, RunRecord, RunStore};
    let path = git_repo(&s.scan, "alpha");
    let branch = format!("orq/{run}");
    std::fs::write(
        path.join(".tdd-features.json"),
        json!([
            {"featureName": "shop-header", "dependsOn": [], "status": "awaiting_delivery", "heldOn": branch, "heldBy": run},
            {"featureName": "shop-cards", "dependsOn": ["shop-header"], "status": "in_progress"}
        ])
        .to_string(),
    )
    .unwrap();
    let project = orq_git::project_id(&orq_git::resolve_project(&path).unwrap());
    let store = RunStore::new(&s.roots.state);
    let record = RunRecord {
        run_id: run.into(),
        project_id: project.clone(),
        feature_id: "shop-header".into(),
        remote: String::new(),
        provider: String::new(),
        target_branch: "main".into(),
        base_sha: "a".repeat(40),
        branch,
        worktree: s.scan.join(run),
        repository: String::new(),
        profiles: Default::default(),
        delivery: Default::default(),
    };
    store.create(&record).unwrap();
    let events = [
        ("run.grouped", json!({"features": ["shop-header", "shop-cards"]})),
        ("design_group.member_step", json!({"member": "shop-header", "index": 1, "of": 2, "step": "design"})),
        ("design_group.member_passed", json!({"member": "shop-header", "index": 1, "of": 2, "step": "design_group.member_passed"})),
        ("design_group.member_step", json!({"member": "shop-cards", "index": 2, "of": 2, "step": "implement"})),
    ];
    for (n, (kind, payload)) in events.into_iter().enumerate() {
        let event = Event { schema_version: 1, event_id: format!("{run}-{n}"), run_id: run.into(), project_id: project.clone(), feature_id: "shop-header".into(), kind: kind.into(), stage: None, attempt: 1, occurred_at: "2026-11-01T10:00:00Z".into(), payload };
        store.append_event(&event).unwrap();
    }
}

#[test]
fn a_group_runs_member_awaiting_delivery_says_so_on_the_run_page() {
    let s = setup("group-member-awaiting-delivery");
    stored_group_run(&s, "r-1");
    let (status, body) = get(&s, "/api/projects/alpha/runs/r-1");
    assert_eq!(status, 200, "{body}");
    let members = body["run"]["design_group"]["members"].as_array().unwrap_or_else(|| panic!("{body}")).clone();
    assert_eq!((&members[0]["feature"], &members[0]["ledger_status"]), (&json!("shop-header"), &json!("awaiting_delivery")), "{body}");
    assert!(members[1].get("ledger_status").is_none(), "a member not awaiting delivery has none: {body}");
}

// The page in jsdom, fed what a fake server answers, as the features page tests are.

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const LABEL: &str = "Built, awaiting delivery";
const CHIP: &str = "[data-feature-state='awaiting_delivery']";
const SELECTORS: [&str; 8] = [
    CHIP,
    "[data-feature-row]",
    "[data-feature-row='built']",
    "[data-mark='built']",
    "[data-mark='next']",
    "[data-awaiting-delivery-count]",
    "[data-held-on]",
    "[data-held-by]",
];

fn harness(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 4, "active_runs": 1, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn feature(name: &str, status: &str) -> Value {
    json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null})
}

/// A ledger with actions on: `built` awaits delivery on run-7-1's branch, beside one feature in each other state.
fn ledger() -> Value {
    let mut built = feature("built", "awaiting_delivery");
    built["held_on"] = json!("fix--built");
    built["held_by"] = json!("run-7-1");
    json!({"status": 200, "json": {"features": [feature("done", "completed"), built, feature("busy", "in_progress"), feature("next", "pending")], "remote_actions": true}})
}

fn texts(frame: &Value) -> Vec<String> {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(String::from).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap_or_else(|| panic!("{selector} was not measured"))
}

#[test]
fn the_features_page_shows_a_feature_awaiting_delivery_with_its_chip_filters_to_it_and_counts_it_on_its_own() {
    let steps = json!([{"submit": TOKEN}, {"set": "[data-feature-status]", "value": "awaiting_delivery"}]);
    let Some(out) = harness("/projects/alpha/features", json!([ledger(), projects()]), steps) else { return };
    let all = &out["frames"][1];
    let t = texts(all);
    assert_eq!(count(all, CHIP), 1, "{t:?}");
    assert!(t.iter().any(|t| t == LABEL), "{t:?}");
    assert!(t.iter().any(|t| t == "Awaiting delivery"), "the state filter offers it: {t:?}");
    assert_eq!((count(all, "[data-mark='built']"), count(all, "[data-mark='next']")), (0, 1), "it cannot be marked to start: {t:?}");
    assert!(t.iter().any(|t| t == "1 in progress"), "{t:?}");
    assert_eq!(count(all, "[data-awaiting-delivery-count]"), 1, "{t:?}");
    assert!(t.iter().any(|t| t == "1 awaiting delivery"), "{t:?}");
    let filtered = &out["frames"][2];
    assert_eq!((count(filtered, "[data-feature-row]"), count(filtered, "[data-feature-row='built']")), (1, 1), "{:?}", texts(filtered));
}

fn no_runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

#[test]
fn a_feature_awaiting_delivery_shows_the_run_and_branch_it_waits_on_and_offers_no_start_or_queue() {
    let steps = json!([{"submit": TOKEN}, {"nav": "[data-held-by]"}]);
    let Some(out) = harness("/projects/alpha/features/built", json!([ledger(), no_runs(), projects()]), steps) else { return };
    let page = &out["frames"][1];
    let t = texts(page);
    assert_eq!(count(page, CHIP), 1, "{t:?}");
    assert!(t.iter().any(|t| t == LABEL), "{t:?}");
    assert_eq!((count(page, "[data-held-by]"), count(page, "[data-held-on]")), (1, 1), "{t:?}");
    let links = page["settled"]["links"].as_array().unwrap();
    assert!(links.iter().any(|l| l["href"] == "/projects/alpha/orchestrations/run-7-1" && l["text"] == "run-7-1"), "{links:?}");
    assert!(t.iter().any(|t| t == "fix--built"), "the branch it waits on: {t:?}");
    let buttons: Vec<&str> = page["settled"]["buttons"].as_array().unwrap().iter().filter_map(|b| b.as_str()).collect();
    assert!(!buttons.iter().any(|b| b.contains("Start") || b.contains("Queue")), "{buttons:?}");
    assert_eq!(out["frames"][2]["settled"]["path"], "/projects/alpha/orchestrations/run-7-1", "the run link opens the run");
}

#[test]
fn a_group_runs_member_list_shows_a_member_awaiting_delivery_with_its_chip() {
    let run = json!({
        "run_id": "run-7-1", "feature": "shop-header", "target_branch": "main", "state": "running", "stage": "implement",
        "started_at": "2026-11-01T10:00:00Z", "updated_at": "2026-11-01T11:00:00Z", "delivery": "review",
        "design_group": {"at": 2, "of": 2, "stopped": null, "members": [
            {"feature": "shop-header", "state": "passed", "round": 1, "components": [], "design": null, "ledger_status": "awaiting_delivery"},
            {"feature": "shop-cards", "state": "implementing", "round": null, "components": [], "design": null}
        ]}
    });
    let body = json!({"run": run, "events": [], "next_cursor": null, "review_rounds": 0, "remote_actions": true});
    let responses = json!([{"status": 200, "json": body}, projects(), {"status": 200, "stream": [], "end": "hold"}]);
    let Some(out) = harness("/projects/alpha/orchestrations/run-7-1", responses, json!([{"submit": TOKEN}])) else { return };
    let page = &out["frames"][1];
    let t = texts(page);
    assert_eq!(count(page, CHIP), 1, "only the member awaiting delivery has the chip: {t:?}");
    assert!(t.iter().any(|t| t == LABEL), "{t:?}");
    assert!(t.iter().any(|t| t == "Passed") && t.iter().any(|t| t == "Implementing"), "{t:?}");
}

#[test]
fn on_a_390_px_phone_the_chip_and_what_it_waits_on_stay_on_screen() {
    use web_chromium::{get, measure};
    let Some(chromium) = web_chromium::binary() else { return };
    let mut built = feature("a-feature-built-in-a-group-run-with-a-long-name", "awaiting_delivery");
    built["held_on"] = json!("fix--a-feature-built-in-a-group-run-with-a-very-long-branch-name-that-must-wrap");
    built["held_by"] = json!("run-1795651298-3044572-1");
    let replies = vec![
        get("/api/projects", projects()["json"].clone()),
        get("/api/projects/alpha/features", json!({"features": [built, feature("next", "pending")], "remote_actions": true})),
        get("/api/projects/alpha/runs*", no_runs()["json"].clone()),
    ];
    for (path, shown) in [("/projects/alpha/features", CHIP), ("/projects/alpha/features/a-feature-built-in-a-group-run-with-a-long-name", "[data-feature-held]")] {
        let steps = json!([{"wait": shown}, {"boxes": CHIP}, {"boxes": shown}]);
        let out = measure(&chromium, 390, path, &replies, steps);
        let result = &out.result;
        assert!(result.get("error").is_none(), "{path}: {result}\nunanswered: {:?}", out.unanswered);
        for selector in [CHIP, shown] {
            let boxes = result["boxes"][selector].as_array().unwrap_or_else(|| panic!("{path}: {result}"));
            assert!(!boxes.is_empty(), "{path}: {selector} is shown: {result}");
            for b in boxes {
                assert!(b["left"].as_i64().unwrap() >= 0 && b["right"].as_i64().unwrap() <= 390, "{path}: {selector} stays on a 390 px screen: {result}");
            }
        }
    }
}
