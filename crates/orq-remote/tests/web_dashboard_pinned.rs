//! Projects used often can be pinned on the Dashboard: each card has a pin button, and pinned projects sit
//! in their own Pinned section above the rest. This browser remembers the pins; a browser that blocks
//! storage still pins for as long as the page is open.

mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";
const PINNED: &str = ".projects-pinned > .project [data-pin][aria-pressed=\"true\"]";
const UNPINNED: &str = ".projects-rest > .project [data-pin][aria-pressed=\"false\"]";

fn projects() -> Value {
    let project = |id: &str| json!({"id": id, "name": id, "available": true, "features": 3, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24});
    json!({"status": 200, "json": {"projects": [project("alpha"), project("beta"), project("gamma")]}})
}

fn run(extra: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let mut scenario = json!({"url": "/dashboard", "responses": [projects(), projects(), projects()], "steps": steps, "selectors": [PINNED, UNPINNED]});
    for (key, value) in extra.as_object().unwrap() {
        scenario[key] = value.clone();
    }
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn order(frame: &Value) -> Vec<String> {
    // The section titles and project names below the totals (whose first label is also "Projects").
    let wanted = ["Pinned", "Projects", "alpha", "beta", "gamma"];
    let texts: Vec<&str> = frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect();
    let below = texts.iter().position(|t| *t == "Last updated").map_or(0, |at| at + 1);
    texts[below..].iter().filter(|t| wanted.contains(t)).map(|t| t.to_string()).collect()
}

fn count(frame: &Value, selector: &str) -> u64 {
    frame["settled"]["matches"][selector].as_u64().unwrap()
}

#[test]
fn pinned_projects_sit_in_their_own_section_above_the_rest() {
    let Some(out) = run(json!({"local": {"orq.pinned": "[\"gamma\"]"}}), json!([{"submit": TOKEN}])) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(order(last), ["Pinned", "gamma", "Projects", "alpha", "beta"], "{out}");
    assert_eq!(count(last, PINNED), 1, "{out}");
    assert_eq!(count(last, UNPINNED), 2, "{out}");
}

#[test]
fn pinning_and_unpinning_moves_a_project_and_is_remembered() {
    let steps = json!([{"submit": TOKEN}, {"click": "[data-pin=\"beta\"]"}, {"go": "/settings"}, {"go": "/dashboard"}, {"click": "[data-pin=\"beta\"]"}]);
    let Some(out) = run(json!({}), steps) else { return };
    let frames = out["frames"].as_array().unwrap();
    assert_eq!(order(&frames[1]), ["Projects", "alpha", "beta", "gamma"], "nothing pinned, no Pinned section: {out}");
    assert_eq!(order(&frames[2]), ["Pinned", "beta", "Projects", "alpha", "gamma"], "{out}");
    assert_eq!(order(&frames[4]), ["Pinned", "beta", "Projects", "alpha", "gamma"], "the pin is remembered: {out}");
    assert_eq!(order(&frames[5]), ["Projects", "alpha", "beta", "gamma"], "{out}");
}

#[test]
fn a_browser_that_blocks_storage_still_pins_while_the_page_is_open() {
    let Some(out) = run(json!({"storage_blocked": true}), json!([{"submit": TOKEN}, {"click": "[data-pin=\"alpha\"]"}])) else { return };
    let last = out["frames"].as_array().unwrap().last().unwrap();
    assert_eq!(order(last), ["Pinned", "alpha", "Projects", "beta", "gamma"], "{out}");
}
