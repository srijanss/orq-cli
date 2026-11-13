//! Once a feature is started from the web, by Start, Start with… or Start N marked, the confirmation links
//! to the new run's orchestration page so it can be opened straight away.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const SELECTORS: [&str; 2] = ["[data-open-run]", "[data-marked-result]"];

fn run(url: &str, responses: Vec<Value>, steps: Vec<Value>) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let mut all = vec![json!({"submit": TOKEN})];
    all.extend(steps);
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": all, "selectors": SELECTORS});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn ledger() -> Value {
    let feature = |name: &str| json!({"name": name, "description": null, "status": "pending", "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("next-one"), feature("other")], "remote_actions": true}})
}

fn runs() -> Value {
    json!({"status": 200, "json": {"runs": [], "next_cursor": null, "total": 0, "running": 0}})
}

fn profiles() -> Value {
    json!({"status": 200, "json": {"profiles": ["opus"], "implementer": "opus", "reviewer": "opus"}})
}

fn started(run: &str) -> Value {
    json!({"status": 202, "json": {"run": run}})
}

fn last(out: &Value) -> &Value {
    out["frames"].as_array().unwrap().last().unwrap()
}

fn run_links(frame: &Value) -> Vec<(String, String)> {
    frame["settled"]["links"].as_array().unwrap().iter().filter(|l| l["href"].as_str().unwrap_or("").contains("/orchestrations/")).map(|l| (l["href"].as_str().unwrap().to_string(), l["text"].as_str().unwrap_or("").to_string())).collect()
}

#[test]
fn start_on_a_feature_page_links_to_the_new_runs_orchestration() {
    let steps = vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), started("run-7")], steps) else { return };
    assert_eq!(run_links(last(&out)), [("/projects/alpha/orchestrations/run-7".to_string(), "Open run".to_string())], "{out}");
    assert_eq!(last(&out)["settled"]["matches"]["[data-open-run]"], 1, "{out}");
}

#[test]
fn start_with_links_to_the_new_runs_orchestration() {
    let steps = vec![json!({"click": "[data-start-with]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), profiles(), started("run-8")], steps) else { return };
    assert_eq!(run_links(last(&out)), [("/projects/alpha/orchestrations/run-8".to_string(), "Open run".to_string())], "{out}");
}

#[test]
fn start_marked_links_each_started_run_and_none_for_a_refusal() {
    let refused = json!({"status": 409, "json": {"error": "no"}});
    let steps = vec![json!({"click": "[data-mark=\"next-one\"]"}), json!({"click": "[data-mark=\"other\"]"}), json!({"click": "[data-start-marked]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features", vec![ledger(), projects(), profiles(), started("run-9"), refused], steps) else { return };
    assert_eq!(last(&out)["settled"]["matches"]["[data-marked-result]"], 2, "{out}");
    assert_eq!(run_links(last(&out)), [("/projects/alpha/orchestrations/run-9".to_string(), "Open run".to_string())], "{out}");
}

/// A run id the page could not route to is reported but not linked.
#[test]
fn a_run_id_that_is_not_a_run_id_is_not_linked() {
    let steps = vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), started("../settings")], steps) else { return };
    assert!(run_links(last(&out)).is_empty(), "{out}");
    assert_eq!(last(&out)["settled"]["matches"]["[data-open-run]"], 0, "{out}");
}

/// A plain click on the link opens the run's orchestration page within the app.
#[test]
fn the_link_opens_the_orchestration_page() {
    let steps = vec![json!({"click": "[data-start]"}), json!({"click": "[data-confirm]"}), json!({"nav": "[data-open-run]"})];
    let Some(out) = run("/projects/alpha/features/next-one", vec![ledger(), runs(), projects(), started("run-7")], steps) else { return };
    assert_eq!(last(&out)["settled"]["path"], "/projects/alpha/orchestrations/run-7", "{out}");
}
