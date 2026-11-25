//! Every state, risk and freshness label is one `.status-pill`: mixed-case words rather than enum names, an
//! icon before the words so colour is never the only signal, and a tone from the semantic tokens. A queue
//! entry's Running and a run row's Running wear the same pill; a review row's status, risk and freshness are
//! three pills of one height on one baseline, and Current freshness shows a Current pill. Rendered in jsdom
//! for the markup and measured in a real headless Chromium, in both themes, for the layout and contrast.

mod web_chromium;
mod web_deps;

use serde_json::{json, Value};
use std::process::Command;

const TOKEN: &str = "typed-token-0123456789abcdef0123456789";

fn jsdom(url: &str, responses: Vec<Value>, selectors: &[&str]) -> Option<Value> {
    if !web_deps::ready() {
        return None;
    }
    let dir = env!("CARGO_MANIFEST_DIR");
    let scenario = json!({"url": url, "responses": responses, "steps": [{"submit": TOKEN}], "selectors": selectors, "token": TOKEN});
    let output = Command::new("node").arg(format!("{dir}/tests/web_react_harness.mjs")).arg(format!("{dir}/web/app.react.js")).arg(scenario.to_string()).output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    Some(serde_json::from_slice(&output.stdout).unwrap())
}

fn settled(out: &Value) -> &Value {
    &out["frames"].as_array().unwrap().last().unwrap()["settled"]
}

fn count(out: &Value, selector: &str) -> u64 {
    settled(out)["matches"][selector].as_u64().unwrap_or_else(|| panic!("no count for {selector}: {out}"))
}

fn texts(out: &Value) -> Vec<String> {
    settled(out)["texts"].as_array().unwrap().iter().filter_map(|t| t.as_str()).map(|t| t.trim().to_string()).collect()
}

fn project() -> Value {
    json!({"id": "alpha", "name": "alpha", "available": true, "features": 2, "active_runs": 1, "max_active_runs": null, "start_blocked": null, "reviews_waiting": 1, "failed_runs": 0, "failed_window_hours": 24, "queue": null})
}

fn projects() -> Value {
    json!({"status": 200, "json": {"projects": [project()]}})
}

fn ledger() -> Value {
    let feature = |name: &str, status: &str| json!({"name": name, "description": null, "status": status, "depends_on": [], "recorded_at": null});
    json!({"status": 200, "json": {"features": [feature("busy", "in_progress"), feature("next-one", "pending")], "remote_actions": true}})
}

const BUSY: &str = "tr[data-feature-row=\"busy\"] .status-pill.tone-status-running > svg:first-child";

#[test]
fn an_in_progress_feature_says_in_progress_with_an_icon_not_its_enum_name() {
    let selectors = [BUSY, ".feature-state"];
    let Some(out) = jsdom("/projects/alpha/features", vec![ledger(), projects()], &selectors) else { return };
    assert_eq!(count(&out, BUSY), 1, "the running tone with its icon first: {out}");
    assert_eq!(count(&out, ".feature-state"), 0, "the old pill is gone: {out}");
    let shown = texts(&out);
    assert!(shown.iter().any(|t| t == "In progress"), "{shown:?}");
    assert!(!shown.iter().any(|t| t.eq_ignore_ascii_case("in_progress")), "no enum name: {shown:?}");
}

fn run_row(id: &str, state: &str) -> Value {
    json!({"run_id": id, "feature": "f-".to_string() + id, "state": state, "stage": null, "started_at": null, "updated_at": null, "target_branch": "main"})
}

#[test]
fn a_queue_entrys_running_and_a_run_rows_running_wear_the_same_pill() {
    let queued = json!({"feature": "COM-01", "state": "running", "run": null, "reason": null, "added_at": 1, "added_by": "web", "implementer": null, "reviewer": null});
    let queue = json!({"status": 200, "json": {"queue": {"entries": [queued], "count": 1, "paused": null}, "remote_actions": true}});
    let pill = "[data-queue-state=\"running\"].status-pill.tone-status-running > svg:first-child";
    let Some(out) = jsdom("/projects/alpha/queue", vec![queue, projects()], &[pill, ".badge"]) else { return };
    assert_eq!(count(&out, pill), 1, "{out}");
    assert_eq!(count(&out, ".badge"), 0, "the old badge is gone: {out}");
    assert!(texts(&out).iter().any(|t| t == "Running"), "{out}");

    let runs = json!({"status": 200, "json": {"runs": [run_row("r-run", "running")], "next_cursor": null, "total": 1, "running": 1, "remote_actions": true}});
    let row = ".run-table .status-pill.tone-status-running > svg:first-child";
    let Some(out) = jsdom("/projects/alpha/orchestrations", vec![runs, projects()], &[row, ".feature-state"]) else { return };
    assert_eq!(count(&out, row), 1, "{out}");
    assert_eq!(count(&out, ".feature-state"), 0, "{out}");
    assert!(texts(&out).iter().any(|t| t == "Running"), "{out}");
}

fn reviews() -> Value {
    json!({"reviews": [
        {"run_id": "run-1", "feature": "f-current", "status": "unreadable", "risk": "high", "stale": false, "freshness": "current", "generated_at": "2026-11-05T10:00:00Z", "project": "alpha"},
        {"run_id": "run-2", "feature": "f-stale", "status": "open", "risk": "low", "stale": true, "freshness": "stale", "generated_at": "2026-11-04T10:00:00Z", "project": "alpha"}
    ]})
}

const ROW_PILLS: &str = ".review-row .review-tags > *";
const ROW_STATUS_PILLS: &str = ".review-row .review-tags > .status-pill > svg:first-child";

#[test]
fn a_review_rows_status_risk_and_freshness_are_all_pills_and_current_shows_current() {
    let selectors = [ROW_PILLS, ROW_STATUS_PILLS, ".review-status", ".pill", ".badge"];
    let Some(out) = jsdom("/reviews", vec![json!({"status": 200, "json": reviews()})], &selectors) else { return };
    assert_eq!(count(&out, ROW_PILLS), 6, "three tags on each of two rows: {out}");
    assert_eq!(count(&out, ROW_STATUS_PILLS), 6, "every tag is a pill with its icon first: {out}");
    for old in [".review-status", ".pill", ".badge"] {
        assert_eq!(count(&out, old), 0, "{old} is gone: {out}");
    }
    let shown = texts(&out);
    for word in ["Current", "Stale", "Unreadable", "High risk", "Low risk", "Open"] {
        assert!(shown.iter().any(|t| t == word), "{word}: {shown:?}");
    }
    assert!(!shown.iter().any(|t| t == "unreadable" || t == "UNREADABLE"), "no raw status: {shown:?}");
}

fn rgb(colour: &str) -> [f64; 3] {
    let inner = colour.trim_start_matches("rgba(").trim_start_matches("rgb(").trim_end_matches(')');
    let parts: Vec<f64> = inner.split(',').map(|p| p.trim().parse().unwrap_or_else(|_| panic!("`{colour}` is not rgb()"))).collect();
    assert!(parts.len() == 3 || parts[3] == 1.0, "`{colour}` must be opaque");
    [parts[0], parts[1], parts[2]]
}

fn luminance(colour: &str) -> f64 {
    let channel = |v: f64| {
        let v = v / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let [r, g, b] = rgb(colour);
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

fn contrast(a: &str, b: &str) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn a_review_rows_pills_share_one_height_and_baseline_and_their_text_is_readable_in_both_themes() {
    let Some(chromium) = web_chromium::binary() else { return };
    let first = ".review-row:first-child .review-tags > .status-pill";
    let all = ".review-tags > .status-pill";
    let replies = vec![web_chromium::get("/api/reviews", reviews()), web_chromium::get("/api/projects", json!({"projects": [project()]}))];
    for theme in ["light", "dark"] {
        let init = format!("try {{ localStorage.setItem(\"orq.theme\", \"{theme}\"); }} catch (e) {{}}");
        let steps = json!([{"wait": first}, {"boxes": first}, {"boxes": format!("{first} > svg")}, {"styles": all, "props": ["color", "background-color", "font-size", "font-weight", "text-transform", "border-top-width"]}]);
        let measured = web_chromium::measure_with(&chromium, 1280, "/reviews", &replies, steps, &init);
        let result = measured.result;
        assert!(result.get("error").is_none(), "{theme}: {result} (unanswered {:?})", measured.unanswered);
        let boxes = result["boxes"][first].as_array().unwrap_or_else(|| panic!("{theme}: no boxes: {result}"));
        assert_eq!(boxes.len(), 3, "{theme}: three pills: {result}");
        let edge = |b: &Value, k: &str| b[k].as_f64().unwrap();
        for b in boxes {
            assert!((edge(b, "top") - edge(&boxes[0], "top")).abs() <= 0.5 && (edge(b, "bottom") - edge(&boxes[0], "bottom")).abs() <= 0.5, "{theme}: one height on one line: {result}");
        }
        for icon in result["boxes"][format!("{first} > svg")].as_array().unwrap() {
            assert_eq!((edge(icon, "right") - edge(icon, "left"), edge(icon, "bottom") - edge(icon, "top")), (16.0, 16.0), "{theme}: a 16px icon: {result}");
        }
        let styles = result["styles"][all].as_array().unwrap();
        assert_eq!(styles.len(), 6, "{theme}: {result}");
        for style in styles {
            let (fg, bg) = (style["color"].as_str().unwrap(), style["background-color"].as_str().unwrap());
            let ratio = contrast(fg, bg);
            assert!(ratio >= 4.5, "{theme}: {fg} on {bg} is {ratio:.2}:1");
            assert_eq!(style["font-size"], "12px", "{theme}: {style}");
            assert_eq!(style["font-weight"], "600", "{theme}: {style}");
            assert_eq!(style["text-transform"], "none", "{theme}: {style}");
            assert_eq!(style["border-top-width"], "0px", "{theme}: {style}");
        }
    }
}
