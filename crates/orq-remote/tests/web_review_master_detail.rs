//! A project's Reviews page: with a report open, the list stays beside it on a wide screen and a
//! phone shows the report alone with a Back button. One report component draws it either way.

mod web_cascade;
mod web_deps;

use web_cascade::effective_at;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn run(url: &str, responses: Value, steps: Value) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": steps});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [{"id": "alpha", "name": "alpha", "available": true, "features": 1, "active_runs": 0, "reviews_waiting": 0, "failed_runs": 0, "failed_window_hours": 24}]}})
}

fn list() -> Value {
    json!({"status": 200, "json": {"reviews": [
        {"run_id": "run-1", "feature": "first-feature", "status": "approved", "risk": "low", "stale": false},
        {"run_id": "run-2", "feature": "second-feature", "status": "needs changes", "stale": false}
    ]}})
}

fn detail(summary: &str) -> Value {
    json!({"status": 200, "json": {"stale": false, "link": null, "summary": [summary], "risks": [], "files": ["a.rs"], "tests": ["t"], "rounds": []}})
}

fn shown(frame: &Value) -> String {
    frame["settled"]["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).collect::<Vec<_>>().join(" | ")
}

fn css() -> String {
    web_cascade::css()
}

#[test]
fn an_open_report_is_drawn_next_to_the_list_which_stays_and_is_not_asked_for_again() {
    let Some(out) = run("/projects/alpha/reviews", json!([list(), projects(), detail("adds the thing"), detail("adds the other")]), json!([{"submit": TOKEN}, {"click": "[data-open-run=run-1]"}, {"click": "[data-open-run=run-2]"}])) else { return };
    let opened = shown(&out["frames"][2]);
    assert!(opened.contains("adds the thing") && opened.contains("first-feature") && opened.contains("second-feature"), "{opened}");
    assert_eq!(out["requests"].as_array().unwrap().iter().filter(|r| r["url"] == "/api/projects/alpha/reviews").count(), 1, "the list is not fetched again: {out}");
    let second = shown(&out["frames"][3]);
    assert!(second.contains("adds the other") && !second.contains("adds the thing") && second.contains("first-feature"), "{second}");
    assert_eq!(out["frames"][3]["settled"]["path"], "/projects/alpha/reviews/run-2", "{out}");
}

#[test]
fn the_report_is_marked_so_the_stylesheet_can_show_it_alone_on_a_phone() {
    let Some(out) = run("/projects/alpha/reviews/run-1", json!([list(), detail("adds the thing"), projects()]), json!([{"submit": TOKEN}])) else { return };
    let text = shown(&out["frames"][1]);
    assert!(text.contains("adds the thing") && text.contains("Back to the list"), "{text}");
    assert_eq!(out["frames"][1]["settled"]["selected"], json!(["run-1"]), "{out}");
}

#[test]
fn the_stylesheet_shows_the_list_only_on_wide_screens_while_a_report_is_open() {
    let css = css();
    for width in [320.0, 360.0, 430.0, 899.0] {
        assert_eq!(effective_at(&css, ".master-pane", "display", width), "none", "the list is hidden at {width}px");
        assert_ne!(effective_at(&css, ".detail-pane", "display", width), "none", "the report shows at {width}px");
    }
    for width in [900.0, 1280.0] {
        assert_eq!(effective_at(&css, ".master-pane", "display", width), "block", "the list shows at {width}px");
        assert_eq!(effective_at(&css, ".master-detail", "display", width), "grid", "two columns at {width}px");
        assert!(effective_at(&css, ".master-detail", "grid-template-columns", width).contains(" "), "two columns at {width}px");
    }
    assert_ne!(effective_at(&css, ".back-to-list", "display", 360.0), "none", "phones keep Back");
    assert_eq!(effective_at(&css, ".back-to-list", "display", 1280.0), "none", "Back is not needed beside the list");
}

#[test]
fn one_report_component_serves_every_report_page() {
    let src = std::fs::read_to_string(format!("{}/web-src/src/main.jsx", env!("CARGO_MANIFEST_DIR"))).unwrap();
    assert_eq!(src.matches("function Detail(").count(), 1);
    assert_eq!(src.matches("<Detail ").count(), 2, "the project page and the global inbox draw the same component");
}
